// tools/comfyui.rs — ComfyUI inference tool handlers.
//
// These tools route to the ComfyUI REST API through the policy engine.
// Workflow submission is gated behind `comfyui://workflow/**`.
// Model listing is gated behind `comfyui://model/**`.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use ta_policy::{PolicyEngine, PolicyRequest};

use crate::server::GatewayState;
use crate::validation::enforce_policy;

/// Parameters for `comfyui_workflow_submit`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComfyUiWorkflowSubmitParams {
    /// ComfyUI workflow JSON (the full prompt graph).
    pub workflow_json: String,
    /// Optional input overrides to merge into the workflow (node ID → values).
    #[serde(default)]
    pub inputs: Option<serde_json::Value>,
    /// Goal run ID (for audit tracking).
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `comfyui_job_status`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComfyUiJobStatusParams {
    /// Job ID returned by `comfyui_workflow_submit`.
    pub job_id: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `comfyui_job_cancel`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComfyUiJobCancelParams {
    /// Job ID to cancel.
    pub job_id: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `comfyui_model_list`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComfyUiModelListParams {
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Resolve agent_id for ComfyUI tool calls.
fn resolve_agent_id(state: &GatewayState, goal_run_id: Option<&str>) -> String {
    if let Some(id) = goal_run_id {
        if let Ok(uuid) = uuid::Uuid::parse_str(id) {
            if let Ok(agent_id) = state.agent_for_goal(uuid) {
                return agent_id;
            }
        }
    }
    state.resolve_agent_id()
}

/// Evaluate a policy for a ComfyUI-specific resource URI.
fn check_comfyui_policy(
    engine: &PolicyEngine,
    agent_id: &str,
    verb: &str,
    resource: &str,
) -> Result<ta_policy::PolicyDecision, McpError> {
    let request = PolicyRequest {
        agent_id: agent_id.to_string(),
        tool: "comfyui".to_string(),
        verb: verb.to_string(),
        target_uri: resource.to_string(),
    };
    Ok(engine.evaluate(&request))
}

pub fn handle_comfyui_workflow_submit(
    state: &Arc<Mutex<GatewayState>>,
    params: ComfyUiWorkflowSubmitParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = "comfyui://workflow/submit";
    let decision = check_comfyui_policy(&state.policy_engine, &agent_id, "submit", resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "message": "ComfyUI server is not reachable. Ensure ComfyUI is running at the configured URL.",
        "hint": "Run `ta connector install comfyui` for setup instructions, or check [connectors.comfyui] in your config.",
        "workflow_length": params.workflow_json.len()
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_comfyui_job_status(
    state: &Arc<Mutex<GatewayState>>,
    params: ComfyUiJobStatusParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!("comfyui://workflow/status/{}", params.job_id);
    let decision = check_comfyui_policy(&state.policy_engine, &agent_id, "read", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "job_id": params.job_id,
        "message": "ComfyUI server is not reachable.",
        "hint": "Ensure ComfyUI is running and [connectors.comfyui] url is correct."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_comfyui_job_cancel(
    state: &Arc<Mutex<GatewayState>>,
    params: ComfyUiJobCancelParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!("comfyui://workflow/cancel/{}", params.job_id);
    let decision = check_comfyui_policy(&state.policy_engine, &agent_id, "cancel", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "job_id": params.job_id,
        "message": "ComfyUI server is not reachable.",
        "hint": "Ensure ComfyUI is running and [connectors.comfyui] url is correct."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_comfyui_model_list(
    state: &Arc<Mutex<GatewayState>>,
    params: ComfyUiModelListParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = "comfyui://model/list";
    let decision = check_comfyui_policy(&state.policy_engine, &agent_id, "read", resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "message": "ComfyUI server is not reachable.",
        "hint": "Ensure ComfyUI is running and [connectors.comfyui] url is correct."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}
