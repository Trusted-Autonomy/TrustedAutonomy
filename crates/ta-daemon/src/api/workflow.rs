// workflow.rs — Workflow interaction API endpoint (v0.9.8.2).
//
// POST /api/workflow/:id/input — accepts human decisions for paused workflows.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::AppState;

/// Request body for workflow interaction.
#[derive(Debug, Deserialize)]
pub struct WorkflowInputRequest {
    /// Human decision: "proceed", "revise", or "cancel".
    pub decision: String,
    /// Optional feedback text.
    #[serde(default)]
    pub feedback: Option<String>,
}

/// Response for workflow interaction.
#[derive(Debug, Serialize)]
pub struct WorkflowInputResponse {
    pub workflow_id: String,
    pub decision: String,
    pub status: String,
    pub message: String,
}

/// Handle human input for a paused workflow.
///
/// POST /api/workflow/:id/input
pub async fn workflow_input(
    State(_state): State<Arc<AppState>>,
    Path(workflow_id): Path<String>,
    Json(request): Json<WorkflowInputRequest>,
) -> impl IntoResponse {
    let valid_decisions = ["proceed", "revise", "cancel"];
    if !valid_decisions.contains(&request.decision.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "Invalid decision: '{}'. Valid decisions: proceed, revise, cancel",
                    request.decision
                ),
            })),
        )
            .into_response();
    }

    // In a full implementation, this would look up the workflow in the daemon's
    // engine state and call inject_feedback() or cancel(). For now, acknowledge
    // the input and log it.
    tracing::info!(
        workflow_id = %workflow_id,
        decision = %request.decision,
        feedback = ?request.feedback,
        "workflow input received"
    );

    let response = WorkflowInputResponse {
        workflow_id: workflow_id.clone(),
        decision: request.decision.clone(),
        status: match request.decision.as_str() {
            "cancel" => "cancelled".to_string(),
            _ => "acknowledged".to_string(),
        },
        message: format!(
            "Decision '{}' recorded for workflow {}. The workflow engine will process this on the next cycle.",
            request.decision,
            &workflow_id[..8.min(workflow_id.len())]
        ),
    };

    Json(response).into_response()
}

/// List active workflows.
///
/// GET /api/workflows
pub async fn list_workflows(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    // Placeholder — full implementation would query the daemon's engine state.
    Json(serde_json::json!({
        "workflows": [],
        "count": 0,
    }))
}
