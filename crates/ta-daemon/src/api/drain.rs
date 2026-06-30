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

use crate::api::agent::SessionStatus;
use crate::api::AppState;

/// `GET /api/drain/status` — Report current active work to support graceful drain.
///
/// Returns:
/// - `status`: `"clean"` when no active goals or agent sessions remain, `"draining"` otherwise.
/// - `active_goals`: number of goal runs in Running/Configured state (PrReady excluded — agent is done).
/// - `active_sessions`: number of agent sessions in Starting/Running/Idle state.
///
/// The `ta daemon restart` CLI polls this endpoint every 2 seconds and restarts
/// only once `status == "clean"` or the daemon stops responding.
pub async fn drain_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Count active goal runs from the goal store.
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

    // Count active agent sessions (shell connections, MCP connections).
    let active_sessions = state
        .agent_sessions
        .list_sessions()
        .await
        .into_iter()
        .filter(|s| {
            matches!(
                s.status,
                SessionStatus::Starting | SessionStatus::Running | SessionStatus::Idle
            )
        })
        .count();

    let is_clean = active_goals == 0 && active_sessions == 0;

    Json(serde_json::json!({
        "status": if is_clean { "clean" } else { "draining" },
        "active_goals": active_goals,
        "active_sessions": active_sessions,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    // Unit tests for drain logic are integration-tested via the CLI drain_and_poll path.
    // The endpoint itself just reads from AppState which is hard to mock in isolation.
    // Coverage is provided by the cmd_restart integration path in daemon.rs tests.
}
