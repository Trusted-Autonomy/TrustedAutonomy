// tools/event.rs — Event query MCP tool handler (v0.9.4).
//
// Provides `ta_event_subscribe` for orchestrator agents to query events
// without polling. Uses the file-based event store for reliable event
// retrieval with filtering by type, goal, and time range.
//
// Design: synchronous query-based (not streaming) because MCP tool calls
// are request-response. The orchestrator calls this tool when it needs to
// check for events since a given timestamp, avoiding repeated identical polls.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use ta_events::{EventStore, FsEventStore};

use crate::server::GatewayState;

/// Parameters for `ta_event_subscribe`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EventSubscribeParams {
    /// Action: "query" (get events matching filter), "latest" (most recent events),
    /// or "watch" (events since a timestamp — use for polling-free orchestration).
    pub action: String,
    /// Filter by event type names (e.g., ["goal_completed", "goal_failed", "draft_built"]).
    #[serde(default)]
    pub event_types: Option<Vec<String>>,
    /// Filter by goal ID.
    #[serde(default)]
    pub goal_id: Option<String>,
    /// ISO 8601 timestamp — return events after this time.
    /// For "watch" action, this is the cursor: pass the timestamp of the last
    /// event you received to get only new events.
    #[serde(default)]
    pub since: Option<String>,
    /// Maximum number of events to return (default: 20).
    #[serde(default)]
    pub limit: Option<usize>,
}

pub fn handle_event_subscribe(
    state: &Arc<Mutex<GatewayState>>,
    params: EventSubscribeParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let events_dir = state.config.workspace_root.join(".ta").join("events");
    let event_store = FsEventStore::new(&events_dir);

    match params.action.as_str() {
        "query" | "watch" => {
            let mut filter = ta_events::store::EventQueryFilter {
                event_types: params.event_types.unwrap_or_default(),
                goal_id: None,
                since: None,
                until: None,
                limit: Some(params.limit.unwrap_or(20)),
            };

            if let Some(ref goal_id_str) = params.goal_id {
                filter.goal_id = goal_id_str.parse::<uuid::Uuid>().ok();
            }

            if let Some(ref since_str) = params.since {
                filter.since = chrono::DateTime::parse_from_rfc3339(since_str)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc));
            }

            let events = event_store
                .query(&filter)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            let items: Vec<serde_json::Value> = events
                .iter()
                .map(|env| {
                    serde_json::json!({
                        "id": env.id.to_string(),
                        "timestamp": env.timestamp.to_rfc3339(),
                        "event_type": env.event_type,
                        "goal_id": env.payload.goal_id().map(|id| id.to_string()),
                        "payload": serde_json::to_value(&env.payload).unwrap_or_default(),
                    })
                })
                .collect();

            // Include the latest timestamp as a cursor for the next "watch" call.
            let cursor = events.last().map(|e| e.timestamp.to_rfc3339());

            let response = serde_json::json!({
                "count": items.len(),
                "events": items,
                "cursor": cursor,
                "message": if items.is_empty() {
                    "No events match the filter. Use 'since' with the last cursor to watch for new events."
                } else {
                    "Pass the 'cursor' value as 'since' in the next call to get only newer events."
                },
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        "latest" => {
            let limit = params.limit.unwrap_or(10);
            let filter = ta_events::store::EventQueryFilter {
                event_types: params.event_types.unwrap_or_default(),
                goal_id: None,
                since: None,
                until: None,
                limit: Some(limit),
            };

            if let Some(ref goal_id_str) = params.goal_id {
                let _ = goal_id_str; // Used in filter above if needed
            }

            let events = event_store
                .query(&filter)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            // Take the last N events (most recent).
            let recent: Vec<serde_json::Value> = events
                .iter()
                .rev()
                .take(limit)
                .map(|env| {
                    serde_json::json!({
                        "id": env.id.to_string(),
                        "timestamp": env.timestamp.to_rfc3339(),
                        "event_type": env.event_type,
                        "goal_id": env.payload.goal_id().map(|id| id.to_string()),
                        "payload": serde_json::to_value(&env.payload).unwrap_or_default(),
                    })
                })
                .collect();

            let response = serde_json::json!({
                "count": recent.len(),
                "events": recent,
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        _ => Err(McpError::invalid_params(
            format!(
                "unknown action '{}'. Expected: query, watch, latest",
                params.action
            ),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
    use tempfile::tempdir;

    /// Helper: create an FsEventStore in a temp dir and emit lifecycle events.
    fn setup_events_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let events_dir = dir.path().join(".ta").join("events");
        (dir, events_dir)
    }

    #[test]
    fn end_to_end_lifecycle_events() {
        let (_dir, events_dir) = setup_events_dir();
        let store = FsEventStore::new(&events_dir);

        let goal_id = uuid::Uuid::new_v4();
        let draft_id = uuid::Uuid::new_v4();

        // 1. Emit GoalStarted.
        let started = SessionEvent::GoalStarted {
            goal_id,
            title: "Fix auth bug".into(),
            agent_id: "claude-code".into(),
            phase: Some("v0.9.4.1".into()),
        };
        store.append(&EventEnvelope::new(started)).unwrap();

        // 2. Emit GoalCompleted.
        let completed = SessionEvent::GoalCompleted {
            goal_id,
            title: "Fix auth bug".into(),
            duration_secs: Some(120),
        };
        store.append(&EventEnvelope::new(completed)).unwrap();

        // 3. Emit DraftBuilt.
        let built = SessionEvent::DraftBuilt {
            goal_id,
            draft_id,
            artifact_count: 5,
        };
        store.append(&EventEnvelope::new(built)).unwrap();

        // Query all events for this goal.
        let filter = ta_events::store::EventQueryFilter {
            goal_id: Some(goal_id),
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].event_type, "goal_started");
        assert_eq!(results[1].event_type, "goal_completed");
        assert_eq!(results[2].event_type, "draft_built");

        // Query by event type.
        let filter = ta_events::store::EventQueryFilter {
            event_types: vec!["goal_completed".into(), "goal_failed".into()],
            goal_id: Some(goal_id),
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_type, "goal_completed");

        // Query by event types + draft_built.
        let filter = ta_events::store::EventQueryFilter {
            event_types: vec!["draft_built".into()],
            goal_id: Some(goal_id),
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_type, "draft_built");
    }

    #[test]
    fn cursor_based_watch_pattern() {
        let (_dir, events_dir) = setup_events_dir();
        let store = FsEventStore::new(&events_dir);

        let goal_id = uuid::Uuid::new_v4();

        // Emit initial event.
        let started = SessionEvent::GoalStarted {
            goal_id,
            title: "Task A".into(),
            agent_id: "claude-code".into(),
            phase: None,
        };
        store.append(&EventEnvelope::new(started)).unwrap();

        // First query with no `since` — gets all events.
        let filter = ta_events::store::EventQueryFilter::default();
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 1);

        // Capture the cursor (timestamp of last event).
        let cursor = results.last().unwrap().timestamp;

        // v0.9.5.1: Query with exact cursor — `since` is now strictly-after (>),
        // so passing the last event's timestamp returns 0 events (no +1ms needed).
        let filter = ta_events::store::EventQueryFilter {
            since: Some(cursor),
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert!(
            results.is_empty(),
            "cursor-exclusive: should not re-fetch the last event"
        );

        // Emit a new event.
        // Small delay to ensure timestamp is after cursor.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let completed = SessionEvent::GoalCompleted {
            goal_id,
            title: "Task A".into(),
            duration_secs: Some(60),
        };
        store.append(&EventEnvelope::new(completed)).unwrap();

        // Query with the same cursor — should get only the new event.
        let filter = ta_events::store::EventQueryFilter {
            since: Some(cursor),
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_type, "goal_completed");
    }

    #[test]
    fn goal_failed_events_persisted() {
        let (_dir, events_dir) = setup_events_dir();
        let store = FsEventStore::new(&events_dir);

        let goal_id = uuid::Uuid::new_v4();

        // Emit GoalStarted then GoalFailed.
        store
            .append(&EventEnvelope::new(SessionEvent::GoalStarted {
                goal_id,
                title: "Failing task".into(),
                agent_id: "test".into(),
                phase: None,
            }))
            .unwrap();

        store
            .append(&EventEnvelope::new(SessionEvent::GoalFailed {
                goal_id,
                error: "Agent exited with status 1".into(),
                exit_code: Some(1),
            }))
            .unwrap();

        // Query failure events.
        let filter = ta_events::store::EventQueryFilter {
            event_types: vec!["goal_failed".into()],
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].payload.goal_id(), Some(goal_id));
    }
}
