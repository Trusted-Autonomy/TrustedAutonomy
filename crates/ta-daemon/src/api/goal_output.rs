// api/goal_output.rs — Live agent output streaming for running goals.
//
// Each background goal command gets a broadcast channel. Lines from the agent's
// stdout/stderr are published to the channel. Clients subscribe via
// `GET /api/goals/:id/output` (SSE).

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use tokio::sync::{broadcast, Mutex};

use crate::api::AppState;

/// Manages per-goal output broadcast channels.
///
/// Wrapped in `Arc` internally so it can be shared between AppState and
/// spawned background tasks.
#[derive(Clone)]
pub struct GoalOutputManager {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<OutputLine>>>>,
    /// Creation order for resolving "latest" — newest entry is last (v0.12.4.1).
    creation_order: Arc<Mutex<Vec<String>>>,
}

/// A single line of output from the agent process.
#[derive(Debug, Clone)]
pub struct OutputLine {
    pub stream: &'static str, // "stdout" or "stderr"
    pub line: String,
}

impl GoalOutputManager {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(Mutex::new(HashMap::new())),
            creation_order: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get a cloneable reference for use in spawned tasks.
    pub fn clone_ref(&self) -> Self {
        self.clone()
    }

    /// Create a channel for a goal. Returns a Sender the spawner uses to publish lines.
    pub async fn create_channel(&self, goal_id: &str) -> broadcast::Sender<OutputLine> {
        let mut channels = self.channels.lock().await;
        // Buffer 256 lines — if a subscriber falls behind, it skips.
        let (tx, _) = broadcast::channel(256);
        channels.insert(goal_id.to_string(), tx.clone());
        drop(channels);
        let mut order = self.creation_order.lock().await;
        order.push(goal_id.to_string());
        tx
    }

    /// Subscribe to a goal's output. Returns None if the goal has no channel.
    pub async fn subscribe(&self, goal_id: &str) -> Option<broadcast::Receiver<OutputLine>> {
        let channels = self.channels.lock().await;
        channels.get(goal_id).map(|tx| tx.subscribe())
    }

    /// Register an alias so that `alias` resolves to the same channel as `primary`.
    /// Used to map goal UUIDs to the human-friendly output key (e.g., "v0.10.13").
    pub async fn add_alias(&self, alias: &str, primary: &str) {
        let channels = self.channels.lock().await;
        if let Some(tx) = channels.get(primary) {
            let tx = tx.clone();
            drop(channels);
            let mut channels = self.channels.lock().await;
            channels.insert(alias.to_string(), tx);
        }
    }

    /// Remove a goal's channel (call when the goal process exits).
    pub async fn remove_channel(&self, goal_id: &str) {
        let mut channels = self.channels.lock().await;
        channels.remove(goal_id);
        drop(channels);
        let mut order = self.creation_order.lock().await;
        order.retain(|id| id != goal_id);
    }

    /// Return the most recently created goal ID that still has an active channel (v0.12.4.1).
    pub async fn latest_goal(&self) -> Option<String> {
        let order = self.creation_order.lock().await;
        let channels = self.channels.lock().await;
        // Iterate newest-first (reverse order) to find first still-active goal.
        order
            .iter()
            .rev()
            .find(|id| channels.contains_key(*id))
            .cloned()
    }

    /// List goal IDs that have active output channels.
    pub async fn active_goals(&self) -> Vec<String> {
        let channels = self.channels.lock().await;
        channels.keys().cloned().collect()
    }
}

// ── Stdin relay for background agent processes (v0.10.18.5) ──────

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;

/// Manages per-goal stdin handles for interactive prompt relay.
///
/// When a background command is spawned with piped stdin, the handle is
/// stored here so that `POST /api/goals/:id/input` can write to it.
#[derive(Clone)]
pub struct GoalInputManager {
    handles: Arc<Mutex<HashMap<String, Arc<Mutex<ChildStdin>>>>>,
}

impl GoalInputManager {
    pub fn new() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Store a stdin handle for a goal output key.
    pub async fn register(&self, goal_id: &str, stdin: ChildStdin) {
        let mut handles = self.handles.lock().await;
        handles.insert(goal_id.to_string(), Arc::new(Mutex::new(stdin)));
    }

    /// Write a line to the agent's stdin pipe. Returns an error if the goal
    /// has no registered stdin handle or the write fails.
    pub async fn send_input(&self, goal_id: &str, input: &str) -> Result<(), String> {
        let handle = {
            let handles = self.handles.lock().await;
            handles.get(goal_id).cloned()
        };

        let Some(handle) = handle else {
            // Try prefix match (same resolution logic as output).
            let handles = self.handles.lock().await;
            let forward: Vec<_> = handles
                .keys()
                .filter(|id| id.starts_with(goal_id))
                .cloned()
                .collect();
            if forward.len() == 1 {
                let h = handles.get(&forward[0]).cloned().unwrap();
                drop(handles);
                let mut stdin = h.lock().await;
                stdin
                    .write_all(format!("{}\n", input).as_bytes())
                    .await
                    .map_err(|e| {
                        format!("Failed to write to agent stdin for '{}': {}", goal_id, e)
                    })?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| format!("Failed to flush agent stdin for '{}': {}", goal_id, e))?;
                return Ok(());
            }
            return Err(format!(
                "No stdin handle for goal '{}'. The goal may have already exited or was not \
                 started with stdin relay enabled.",
                goal_id,
            ));
        };

        let mut stdin = handle.lock().await;
        stdin
            .write_all(format!("{}\n", input).as_bytes())
            .await
            .map_err(|e| format!("Failed to write to agent stdin for '{}': {}", goal_id, e))?;
        stdin
            .flush()
            .await
            .map_err(|e| format!("Failed to flush agent stdin for '{}': {}", goal_id, e))?;
        Ok(())
    }

    /// Remove a goal's stdin handle (call when the goal process exits).
    pub async fn remove(&self, goal_id: &str) {
        let mut handles = self.handles.lock().await;
        handles.remove(goal_id);
    }

    /// Register an alias for stdin (mirrors GoalOutputManager::add_alias).
    pub async fn add_alias(&self, alias: &str, primary: &str) {
        let handles = self.handles.lock().await;
        if let Some(handle) = handles.get(primary) {
            let handle = handle.clone();
            drop(handles);
            let mut handles = self.handles.lock().await;
            handles.insert(alias.to_string(), handle);
        }
    }
}

/// Request body for `POST /api/goals/:id/input`.
#[derive(Deserialize)]
pub struct GoalInputRequest {
    /// The text to send to the agent's stdin.
    pub input: String,
}

/// `POST /api/goals/:id/input` — Write a line to the agent's stdin pipe (v0.10.18.5).
pub async fn goal_input_handler(
    State(state): State<Arc<AppState>>,
    Path(goal_id): Path<String>,
    Json(body): Json<GoalInputRequest>,
) -> impl IntoResponse {
    let resolved_id = resolve_goal_id(&state, &goal_id).await;
    let target_id = resolved_id.as_deref().unwrap_or(&goal_id);

    match state.goal_input.send_input(target_id, &body.input).await {
        Ok(()) => {
            tracing::info!(
                goal_id = target_id,
                input_len = body.input.len(),
                "Stdin input delivered to agent"
            );
            Json(serde_json::json!({
                "status": "delivered",
                "goal_id": target_id,
                "input_length": body.input.len(),
            }))
            .into_response()
        }
        Err(e) => {
            tracing::warn!(goal_id = target_id, error = %e, "Failed to deliver stdin input");
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": e,
                    "hint": "Check that the goal is still running with: ta goal list"
                })),
            )
                .into_response()
        }
    }
}

/// `GET /api/goals/:id/output` — SSE stream of live agent output.
pub async fn goal_output_stream(
    State(state): State<Arc<AppState>>,
    Path(goal_id): Path<String>,
) -> impl IntoResponse {
    // Try exact match first, then prefix match for short IDs.
    let resolved_id = resolve_goal_id(&state, &goal_id).await;

    let rx = match resolved_id {
        Some(ref id) => state.goal_output.subscribe(id).await,
        None => None,
    };

    let Some(mut rx) = rx else {
        // Channel already closed: process finished before client subscribed.
        // Send a done event immediately so the client cleans up its tail state
        // instead of accumulating ghost subscriptions (v0.11.7 item 4).
        let stream = async_stream::stream! {
            yield Ok::<_, std::convert::Infallible>(
                axum::response::sse::Event::default()
                    .event("done")
                    .data("{\"status\": \"goal process already exited\"}")
            );
        };
        return Sse::new(stream).into_response();
    };

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(line) => {
                    let data = serde_json::json!({
                        "stream": line.stream,
                        "line": line.line,
                    });
                    yield Ok::<_, Infallible>(
                        Event::default()
                            .event("output")
                            .data(data.to_string())
                    );
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    yield Ok::<_, Infallible>(
                        Event::default()
                            .event("lagged")
                            .data(format!("{{\"skipped\": {}}}", n))
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    yield Ok::<_, Infallible>(
                        Event::default()
                            .event("done")
                            .data("{\"status\": \"goal process exited\"}")
                    );
                    break;
                }
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// `GET /api/goals/active-output` — List goals with active output streams.
pub async fn list_active_output(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let goals = state.goal_output.active_goals().await;
    Json(serde_json::json!({ "goals": goals }))
}

/// Resolve a short goal ID prefix to a full ID.
///
/// Special values:
/// - `"latest"` — resolves to the most recently started still-running goal (v0.12.4.1).
async fn resolve_goal_id(state: &AppState, query: &str) -> Option<String> {
    // Special alias: "latest" resolves to the newest running goal.
    if query == "latest" {
        return state.goal_output.latest_goal().await;
    }

    let active = state.goal_output.active_goals().await;
    // Exact match.
    if active.contains(&query.to_string()) {
        return Some(query.to_string());
    }
    // Forward prefix: query is a prefix of an active key (e.g. short ID → full UUID).
    let forward: Vec<_> = active.iter().filter(|id| id.starts_with(query)).collect();
    if forward.len() == 1 {
        return Some(forward[0].clone());
    }
    // Reverse prefix: an active key is a prefix of the query (e.g. alias "d01f0930"
    // matches query "d01f0930-bc2f-432f-bbf3-40c75b991e15").
    let reverse: Vec<_> = active
        .iter()
        .filter(|id| query.starts_with(id.as_str()))
        .collect();
    if reverse.len() == 1 {
        return Some(reverse[0].clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn output_manager_lifecycle() {
        let mgr = GoalOutputManager::new();

        // Create channel.
        let tx = mgr.create_channel("goal-1").await;
        assert_eq!(mgr.active_goals().await.len(), 1);

        // Subscribe and receive.
        let mut rx = mgr.subscribe("goal-1").await.unwrap();
        tx.send(OutputLine {
            stream: "stdout",
            line: "hello".to_string(),
        })
        .unwrap();
        let line = rx.recv().await.unwrap();
        assert_eq!(line.line, "hello");
        assert_eq!(line.stream, "stdout");

        // Remove.
        mgr.remove_channel("goal-1").await;
        assert_eq!(mgr.active_goals().await.len(), 0);
        assert!(mgr.subscribe("goal-1").await.is_none());
    }

    #[tokio::test]
    async fn subscribe_nonexistent_returns_none() {
        let mgr = GoalOutputManager::new();
        assert!(mgr.subscribe("nope").await.is_none());
    }

    #[tokio::test]
    async fn alias_resolves_to_same_channel() {
        let mgr = GoalOutputManager::new();
        let tx = mgr.create_channel("v0.10.13").await;

        // Add alias (goal UUID → output key).
        mgr.add_alias("492fac59-eda4-4e87-bf65-9e2edd2e70ce", "v0.10.13")
            .await;

        // Subscribe via alias.
        let mut rx = mgr
            .subscribe("492fac59-eda4-4e87-bf65-9e2edd2e70ce")
            .await
            .unwrap();

        // Send via primary key's sender — alias subscriber receives it.
        tx.send(OutputLine {
            stream: "stdout",
            line: "from primary".to_string(),
        })
        .unwrap();
        let line = rx.recv().await.unwrap();
        assert_eq!(line.line, "from primary");
    }

    #[tokio::test]
    async fn alias_nonexistent_primary_is_noop() {
        let mgr = GoalOutputManager::new();
        // Should not panic or create a channel.
        mgr.add_alias("alias", "nonexistent").await;
        assert!(mgr.subscribe("alias").await.is_none());
    }

    // ── v0.12.4.1: latest_goal and resolve_goal_id("latest") ─────────────────

    #[tokio::test]
    async fn latest_goal_returns_newest_active() {
        let mgr = GoalOutputManager::new();
        assert!(mgr.latest_goal().await.is_none());

        mgr.create_channel("goal-first").await;
        assert_eq!(mgr.latest_goal().await.as_deref(), Some("goal-first"));

        mgr.create_channel("goal-second").await;
        // Newest is "goal-second".
        assert_eq!(mgr.latest_goal().await.as_deref(), Some("goal-second"));

        mgr.create_channel("goal-third").await;
        assert_eq!(mgr.latest_goal().await.as_deref(), Some("goal-third"));
    }

    #[tokio::test]
    async fn latest_goal_skips_removed_channels() {
        let mgr = GoalOutputManager::new();
        mgr.create_channel("goal-a").await;
        mgr.create_channel("goal-b").await;

        // Remove the newest — "latest" should fall back to goal-a.
        mgr.remove_channel("goal-b").await;
        assert_eq!(mgr.latest_goal().await.as_deref(), Some("goal-a"));

        // Remove everything — no latest.
        mgr.remove_channel("goal-a").await;
        assert!(mgr.latest_goal().await.is_none());
    }
}
