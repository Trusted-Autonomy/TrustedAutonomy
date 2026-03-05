// tools/plan.rs — Plan management MCP tool handler.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;

use ta_changeset::interaction::InteractionRequest;
use ta_goal::TaEvent;

use crate::server::{GatewayState, PlanToolParams};
use crate::validation::{parse_uuid, validate_goal_exists};

pub fn handle_plan(
    state: &Arc<Mutex<GatewayState>>,
    params: PlanToolParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    match params.action.as_str() {
        "read" => {
            let goal_run_id =
                parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                    McpError::invalid_params("goal_run_id required for read", None)
                })?)?;

            let goal = state
                .goal_store
                .get(goal_run_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
                .ok_or_else(|| {
                    McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
                })?;

            let plan_path = goal.workspace_path.join("PLAN.md");
            if plan_path.exists() {
                let content = std::fs::read_to_string(&plan_path)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![Content::text(content)]))
            } else {
                let response = serde_json::json!({
                    "message": "No PLAN.md found in workspace.",
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
        }
        "update" => {
            let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                McpError::invalid_params("goal_run_id required for update", None)
            })?)?;
            validate_goal_exists(&state.goal_store, goal_run_id)?;
            let phase = params.phase.as_deref().unwrap_or("unknown");
            let status_note = params
                .status_note
                .as_deref()
                .unwrap_or("Agent proposes phase update");

            state
                .event_dispatcher
                .dispatch(&TaEvent::PlanUpdateProposed {
                    goal_run_id,
                    phase: phase.to_string(),
                    status_note: status_note.to_string(),
                    timestamp: Utc::now(),
                });

            let interaction_req =
                InteractionRequest::plan_negotiation(phase, status_note).with_goal_id(goal_run_id);

            let review_result = state.request_review(&interaction_req);

            let (plan_status, plan_decision) = match &review_result {
                Ok(resp) => {
                    let decision_str = format!("{}", resp.decision);
                    (
                        if decision_str == "approved" {
                            "approved"
                        } else {
                            "proposed"
                        },
                        decision_str,
                    )
                }
                Err(_) => ("proposed", "pending".to_string()),
            };

            let response = serde_json::json!({
                "goal_run_id": goal_run_id.to_string(),
                "phase": phase,
                "status": plan_status,
                "decision": plan_decision,
                "message": if plan_decision == "pending" {
                    "Plan update proposed. Human must approve via `ta draft approve` before it takes effect."
                } else {
                    "Plan update reviewed through ReviewChannel."
                },
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        _ => Err(McpError::invalid_params(
            format!("unknown action '{}'. Expected: read, update", params.action),
            None,
        )),
    }
}
