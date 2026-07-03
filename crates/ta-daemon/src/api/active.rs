// api/active.rs — "Active" tab backend (v0.17.0.12.6 item 5).
//
// Lists Running/Configured goals with elapsed time and the last emitted
// output line, and lets Studio post a free-text note to a specific running
// agent ("Send info / ask this agent").

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::api::AppState;
use ta_goal::{GoalRunState, GoalRunStore};

#[derive(Debug, Serialize)]
pub struct ActiveGoalSummary {
    pub goal_id: String,
    pub title: String,
    /// "running" | "configured"
    pub state: String,
    pub elapsed_secs: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
}

/// `GET /api/active/goals` — Running/Configured goals for the Studio "Active" tab.
pub async fn list_active_goals(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let now = Utc::now();
    let goals = GoalRunStore::new(&state.goals_dir)
        .ok()
        .and_then(|store| store.list().ok())
        .unwrap_or_default();

    let mut summaries = Vec::new();
    for g in goals
        .into_iter()
        .filter(|g| matches!(g.state, GoalRunState::Running | GoalRunState::Configured))
    {
        let goal_id = g.goal_run_id.to_string();
        let last_event = state
            .goal_output
            .get_history_from(&goal_id, 0)
            .await
            .last()
            .map(|l| l.line.clone());
        summaries.push(ActiveGoalSummary {
            goal_id,
            title: g.title.clone(),
            state: format!("{:?}", g.state).to_lowercase(),
            elapsed_secs: (now - g.updated_at).num_seconds(),
            phase: g.plan_phase.clone(),
            last_event,
        });
    }

    Json(summaries).into_response()
}

/// Request body for `POST /api/goals/:id/message` — "Send info / ask this agent".
#[derive(Debug, Deserialize)]
pub struct GoalMessageRequest {
    pub message: String,
}

/// `POST /api/goals/:id/message` — deliver a free-text note to a specific
/// running goal's agent. Reuses the same mid-run note channel as
/// `POST /api/advisor/inject`, but always targets the explicit goal id from
/// the path rather than falling back to "most recently running".
pub async fn handle_goal_message(
    State(state): State<Arc<AppState>>,
    Path(goal_id): Path<String>,
    Json(body): Json<GoalMessageRequest>,
) -> impl IntoResponse {
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "message is required"})),
        )
            .into_response();
    }

    match crate::api::advisor::inject_note_for_goal(&state, Some(&goal_id), &message) {
        Ok(resp) => Json(resp).into_response(),
        Err((status, body)) => (status, Json(body)).into_response(),
    }
}
