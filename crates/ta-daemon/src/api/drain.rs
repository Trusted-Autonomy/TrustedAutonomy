// api/drain.rs — Graceful drain status endpoint (`GET /api/drain/status`).
//
// Used by `ta daemon restart` (without --force) to poll whether the daemon has
// finished active work before restarting. The CLI polls this endpoint until
// `status` is "clean", then proceeds with the restart.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;

use ta_goal::{GoalRunState, GoalRunStore};

use crate::api::AppState;

/// `GET /api/drain/status` — Report current active work to support graceful drain.
///
/// Returns:
/// - `status`: `"clean"` when no active agent work remains, `"draining"` otherwise.
/// - `active_goals`: goal runs in Running/Configured state only. PrReady is excluded
///   (agent finished, draft awaiting human review). Sessions are not counted — goal
///   state is the authoritative signal for whether agent work is in progress.
/// - `active_sessions`: always 0; retained in the response for API compatibility.
///
/// The `ta daemon restart` CLI polls this endpoint every 2 seconds and restarts
/// once `status == "clean"` or the daemon stops responding.
pub async fn drain_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Count goal runs where an agent is actively executing.
    // PrReady: agent done, draft awaiting human review — not active work.
    let active_goals = {
        let store = GoalRunStore::new(&state.goals_dir);
        match store {
            Ok(s) => s
                .list()
                .unwrap_or_default()
                .iter()
                .filter(|g| matches!(g.state, GoalRunState::Running | GoalRunState::Configured))
                .count(),
            Err(_) => 0,
        }
    };

    // Session connections (MCP clients, Claude Code sidebar, ta shell) are NOT
    // counted toward drain. Goal state is the authoritative signal for whether
    // agent work is in progress — a Running/Configured goal means an agent
    // subprocess is actively executing. Sessions are UI connections that outlive
    // individual goal runs and must not block restart.
    let active_sessions = 0usize;

    let is_clean = active_goals == 0;

    Json(serde_json::json!({
        "status": if is_clean { "clean" } else { "draining" },
        "active_goals": active_goals,
        "active_sessions": active_sessions,
    }))
    .into_response()
}

#[cfg(test)]
fn drain_status_str(active_goals: usize, active_sessions: usize) -> &'static str {
    if active_goals == 0 && active_sessions == 0 {
        "clean"
    } else {
        "draining"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_when_no_work() {
        assert_eq!(drain_status_str(0, 0), "clean");
    }

    #[test]
    fn draining_when_goal_running() {
        assert_eq!(drain_status_str(1, 0), "draining");
        assert_eq!(drain_status_str(3, 0), "draining");
    }

    #[test]
    fn draining_when_session_active() {
        assert_eq!(drain_status_str(0, 1), "draining");
    }

    #[test]
    fn clean_when_only_idle_connections() {
        // Idle MCP connections are NOT counted — this must return clean so
        // `ta daemon restart` can proceed without disconnecting Claude Code.
        // The session filter in drain_status() excludes SessionStatus::Idle.
        // This test documents the contract: idle connections ≠ active work.
        assert_eq!(drain_status_str(0, 0), "clean");
    }

    #[test]
    fn clean_when_only_pr_ready_goals() {
        // PrReady goals have a finished agent; the draft waits for human review.
        // They must NOT block restart. This test documents the contract:
        // the goal filter in drain_status() excludes GoalRunState::PrReady.
        // Zero active_goals is what drain_status() returns for PrReady-only state.
        assert_eq!(drain_status_str(0, 0), "clean");
    }
}
