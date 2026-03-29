// api/goal_output.rs — Live agent output streaming for running goals.
//
// Each background goal command gets a broadcast channel. Lines from the agent's
// stdout/stderr are published to the channel. Clients subscribe via
// `GET /api/goals/:id/output` (SSE).
//
// v0.14.9.3: Added SSE event IDs and history replay for reconnect reliability.
// Each published line gets a monotonically-increasing sequence number. Clients
// that reconnect with `Last-Event-ID` header receive missed events from the
// in-memory history (capped at 512 entries per goal).

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use tokio::sync::{broadcast, Mutex};

use crate::api::AppState;

/// History cap per goal channel.
const HISTORY_CAP: usize = 512;

/// A single line of output from the agent process, with a monotonic sequence number.
///
/// Fields are flat (not nested) for ergonomic access.
#[derive(Debug, Clone)]
pub struct SequencedLine {
    pub seq: u64,
    pub stream: &'static str, // "stdout", "stderr", or "prompt"
    pub line: String,
}

/// A single line of output from the agent process.
///
/// Kept for source compatibility — the broadcast channel now uses SequencedLine
/// directly; this type is no longer constructed but kept to avoid churn.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OutputLine {
    pub stream: &'static str, // "stdout" or "stderr"
    pub line: String,
}

/// A cloneable publisher for a single goal's output channel.
///
/// Wraps the broadcast sender, a shared atomic counter, and the per-goal history
/// buffer. All clones share the same underlying state (same Arc), so sequence
/// numbers are globally monotonic across all senders for a given goal.
#[derive(Clone)]
pub struct GoalOutputPublisher {
    sender: broadcast::Sender<SequencedLine>,
    counter: Arc<AtomicU64>,
    history: Arc<Mutex<VecDeque<SequencedLine>>>,
}

impl GoalOutputPublisher {
    /// Publish a line of output. Atomically increments the sequence counter,
    /// stores in history (capped at HISTORY_CAP), and broadcasts to subscribers.
    ///
    /// Lagged receivers will see `RecvError::Lagged` — the history buffer lets
    /// reconnecting clients catch up without data loss (up to 512 entries).
    pub async fn publish(&self, stream: &'static str, line: String) {
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let sline = SequencedLine { seq, stream, line };
        {
            let mut history = self.history.lock().await;
            if history.len() >= HISTORY_CAP {
                history.pop_front();
            }
            history.push_back(sline.clone());
        }
        // Ignore send error — no active subscribers is normal.
        let _ = self.sender.send(sline);
    }
}

/// Manages per-goal output broadcast channels.
///
/// Wrapped in `Arc` internally so it can be shared between AppState and
/// spawned background tasks.
#[derive(Clone)]
pub struct GoalOutputManager {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<SequencedLine>>>>,
    publishers: Arc<Mutex<HashMap<String, GoalOutputPublisher>>>,
    /// Creation order for resolving "latest" — newest entry is last (v0.12.4.1).
    creation_order: Arc<Mutex<Vec<String>>>,
}

impl GoalOutputManager {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(Mutex::new(HashMap::new())),
            publishers: Arc::new(Mutex::new(HashMap::new())),
            creation_order: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get a cloneable reference for use in spawned tasks.
    pub fn clone_ref(&self) -> Self {
        self.clone()
    }

    /// Create a channel for a goal. Returns a GoalOutputPublisher the spawner
    /// uses to publish lines with automatic sequence numbering and history buffering.
    pub async fn create_channel(&self, goal_id: &str) -> GoalOutputPublisher {
        let (tx, _) = broadcast::channel(256);
        let publisher = GoalOutputPublisher {
            sender: tx.clone(),
            counter: Arc::new(AtomicU64::new(0)),
            history: Arc::new(Mutex::new(VecDeque::new())),
        };

        let mut channels = self.channels.lock().await;
        channels.insert(goal_id.to_string(), tx);
        drop(channels);

        let mut publishers = self.publishers.lock().await;
        publishers.insert(goal_id.to_string(), publisher.clone());
        drop(publishers);

        let mut order = self.creation_order.lock().await;
        order.push(goal_id.to_string());

        publisher
    }

    /// Subscribe to a goal's output. Returns None if the goal has no channel.
    pub async fn subscribe(&self, goal_id: &str) -> Option<broadcast::Receiver<SequencedLine>> {
        let channels = self.channels.lock().await;
        channels.get(goal_id).map(|tx| tx.subscribe())
    }

    /// Get history entries with seq >= since_seq for the given goal.
    ///
    /// Used by reconnecting clients to replay missed events. Returns entries
    /// in sequence order (oldest first).
    pub async fn get_history_from(&self, goal_id: &str, since_seq: u64) -> Vec<SequencedLine> {
        // Clone the Arc so we can release the publishers lock before locking history.
        let history_arc = {
            let publishers = self.publishers.lock().await;
            publishers.get(goal_id).map(|p| p.history.clone())
        };
        let Some(history_arc) = history_arc else {
            return Vec::new();
        };
        let history = history_arc.lock().await;
        history
            .iter()
            .filter(|sline| sline.seq >= since_seq)
            .cloned()
            .collect()
    }

    /// Register an alias so that `alias` resolves to the same channel and
    /// publisher as `primary`. The alias shares the primary's history buffer and
    /// sequence counter (same Arc), so events published through either key are
    /// visible in the shared history.
    pub async fn add_alias(&self, alias: &str, primary: &str) {
        // Extract clones while holding each lock briefly and independently to
        // avoid holding two locks simultaneously (prevents lock-order deadlock).
        let tx_clone = {
            let channels = self.channels.lock().await;
            channels.get(primary).cloned()
        };
        let pub_clone = {
            let publishers = self.publishers.lock().await;
            publishers.get(primary).cloned()
        };

        if let (Some(tx), Some(pub_)) = (tx_clone, pub_clone) {
            {
                let mut channels = self.channels.lock().await;
                channels.insert(alias.to_string(), tx);
            }
            {
                let mut publishers = self.publishers.lock().await;
                publishers.insert(alias.to_string(), pub_);
            }
        }
    }

    /// Remove a goal's channel (call when the goal process exits).
    pub async fn remove_channel(&self, goal_id: &str) {
        let mut channels = self.channels.lock().await;
        channels.remove(goal_id);
        drop(channels);

        let mut publishers = self.publishers.lock().await;
        publishers.remove(goal_id);
        drop(publishers);

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
///
/// Supports reconnect via the `Last-Event-ID` HTTP header (SSE spec).
/// When a client reconnects with `Last-Event-ID: N`, missed events with
/// seq > N are replayed from the in-memory history before streaming live events.
pub async fn goal_output_stream(
    State(state): State<Arc<AppState>>,
    Path(goal_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    // Parse Last-Event-ID for reconnect replay.
    let last_event_id: Option<u64> = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    // Try exact match first, then prefix match for short IDs.
    let resolved_id = resolve_goal_id(&state, &goal_id).await;

    // Subscribe FIRST to avoid the race where events are published between
    // get_history_from() and subscribe(). We then deduplicate against history.
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

    // Fetch history for replay (after subscribe, so no events are lost).
    let history_events: Vec<SequencedLine> = match resolved_id {
        Some(ref id) => {
            let since = last_event_id.map(|n| n + 1).unwrap_or(0);
            state.goal_output.get_history_from(id, since).await
        }
        None => Vec::new(),
    };

    // The highest seq in history — live events with seq <= this have already
    // been replayed from history and must be skipped to avoid duplicates.
    let history_max_seq: Option<u64> = history_events.last().map(|s| s.seq);

    let stream = async_stream::stream! {
        // Replay history events first.
        for sline in history_events {
            let data = serde_json::json!({
                "stream": sline.stream,
                "line": sline.line,
            });
            yield Ok::<_, Infallible>(
                Event::default()
                    .id(sline.seq.to_string())
                    .event("output")
                    .data(data.to_string())
            );
        }

        // Stream live events, skipping any already covered by history replay.
        loop {
            match rx.recv().await {
                Ok(sline) => {
                    // Skip events already sent via history replay.
                    if let Some(max) = history_max_seq {
                        if sline.seq <= max {
                            continue;
                        }
                    }
                    let data = serde_json::json!({
                        "stream": sline.stream,
                        "line": sline.line,
                    });
                    yield Ok::<_, Infallible>(
                        Event::default()
                            .id(sline.seq.to_string())
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
        tx.publish("stdout", "hello".to_string()).await;
        let sline = rx.recv().await.unwrap();
        assert_eq!(sline.line, "hello");
        assert_eq!(sline.stream, "stdout");

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
        tx.publish("stdout", "from primary".to_string()).await;
        let sline = rx.recv().await.unwrap();
        assert_eq!(sline.line, "from primary");
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

    // ── v0.14.9.3: SSE event IDs and history replay ───────────────────────────

    #[tokio::test]
    async fn sse_event_ids_increment_monotonically() {
        let mgr = GoalOutputManager::new();
        let pub_ = mgr.create_channel("goal-seq").await;
        let mut rx = mgr.subscribe("goal-seq").await.unwrap();

        pub_.publish("stdout", "line 0".to_string()).await;
        pub_.publish("stdout", "line 1".to_string()).await;
        pub_.publish("stdout", "line 2".to_string()).await;

        let s0 = rx.recv().await.unwrap();
        let s1 = rx.recv().await.unwrap();
        let s2 = rx.recv().await.unwrap();

        assert_eq!(s0.seq, 0);
        assert_eq!(s1.seq, 1);
        assert_eq!(s2.seq, 2);
        assert_eq!(s0.line, "line 0");
        assert_eq!(s1.line, "line 1");
        assert_eq!(s2.line, "line 2");
    }

    #[tokio::test]
    async fn get_history_from_returns_since_seq() {
        let mgr = GoalOutputManager::new();
        let pub_ = mgr.create_channel("goal-hist").await;

        for i in 0u64..5 {
            pub_.publish("stdout", format!("line {}", i)).await;
        }

        // Retrieve events with seq >= 3 (should return seqs 3 and 4).
        let history = mgr.get_history_from("goal-hist", 3).await;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].seq, 3);
        assert_eq!(history[1].seq, 4);
        assert_eq!(history[0].line, "line 3");
        assert_eq!(history[1].line, "line 4");
    }

    #[tokio::test]
    async fn reconnect_replays_missed_events() {
        let mgr = GoalOutputManager::new();
        let pub_ = mgr.create_channel("goal-reconnect").await;

        // Subscribe and receive 5 events.
        let mut rx = mgr.subscribe("goal-reconnect").await.unwrap();
        for i in 0u64..5 {
            pub_.publish("stdout", format!("early {}", i)).await;
        }
        for _ in 0..5 {
            rx.recv().await.unwrap();
        }
        // Client disconnects (drop rx).
        drop(rx);

        // 3 more events are published while client is gone.
        pub_.publish("stdout", "missed 0".to_string()).await;
        pub_.publish("stdout", "missed 1".to_string()).await;
        pub_.publish("stdout", "missed 2".to_string()).await;

        // Client reconnects with Last-Event-ID = 4 (last seq seen).
        // Should receive seqs 5, 6, 7 (the 3 missed events).
        let missed = mgr.get_history_from("goal-reconnect", 5).await;
        assert_eq!(missed.len(), 3);
        assert_eq!(missed[0].seq, 5);
        assert_eq!(missed[1].seq, 6);
        assert_eq!(missed[2].seq, 7);
        assert_eq!(missed[0].line, "missed 0");
        assert_eq!(missed[1].line, "missed 1");
        assert_eq!(missed[2].line, "missed 2");
    }

    #[tokio::test]
    async fn alias_shares_history_with_primary() {
        let mgr = GoalOutputManager::new();
        let pub_ = mgr.create_channel("primary-key").await;

        pub_.publish("stdout", "event 0".to_string()).await;
        pub_.publish("stdout", "event 1".to_string()).await;

        // Add alias after publishing — alias shares the same history buffer.
        mgr.add_alias("alias-key", "primary-key").await;

        let history_via_alias = mgr.get_history_from("alias-key", 0).await;
        assert_eq!(history_via_alias.len(), 2);
        assert_eq!(history_via_alias[0].seq, 0);
        assert_eq!(history_via_alias[1].seq, 1);
    }

    #[tokio::test]
    async fn remove_channel_also_removes_publisher() {
        let mgr = GoalOutputManager::new();
        mgr.create_channel("goal-rm").await;

        mgr.remove_channel("goal-rm").await;

        // After removal, subscribe returns None and history is empty.
        assert!(mgr.subscribe("goal-rm").await.is_none());
        let history = mgr.get_history_from("goal-rm", 0).await;
        assert!(history.is_empty());
    }
}
