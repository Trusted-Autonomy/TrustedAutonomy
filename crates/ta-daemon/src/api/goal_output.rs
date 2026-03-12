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
    }

    /// List goal IDs that have active output channels.
    pub async fn active_goals(&self) -> Vec<String> {
        let channels = self.channels.lock().await;
        channels.keys().cloned().collect()
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
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("No active output stream for goal '{}'", goal_id),
                "hint": "The goal may have already completed or not yet started."
            })),
        )
            .into_response();
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
async fn resolve_goal_id(state: &AppState, prefix: &str) -> Option<String> {
    let active = state.goal_output.active_goals().await;
    // Exact match.
    if active.contains(&prefix.to_string()) {
        return Some(prefix.to_string());
    }
    // Prefix match.
    let matches: Vec<_> = active.iter().filter(|id| id.starts_with(prefix)).collect();
    if matches.len() == 1 {
        return Some(matches[0].clone());
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
}
