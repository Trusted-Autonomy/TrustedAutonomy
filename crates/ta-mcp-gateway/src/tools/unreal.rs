// tools/unreal.rs — UE5 MCP tool handlers.
//
// These tools route to the active Unreal backend (kvick/flopperam/special-agent)
// through the policy engine. MRQ submissions are gated behind `unreal://render/**`
// and Python execution is gated behind `unreal://script/**`.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use ta_policy::{PolicyEngine, PolicyRequest};

use crate::server::GatewayState;
use crate::validation::enforce_policy;

/// Parameters for `ue5_python_exec`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Ue5PythonExecParams {
    /// Python script to execute in the UE5 Editor context.
    pub script: String,
    /// Goal run ID (for audit tracking).
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `ue5_scene_query`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Ue5SceneQueryParams {
    /// Content-browser path to the level (e.g., "/Game/Maps/TestLevel").
    pub level_path: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `ue5_asset_list`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Ue5AssetListParams {
    /// Content-browser path to list (e.g., "/Game/Characters").
    pub path: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `ue5_mrq_submit`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Ue5MrqSubmitParams {
    /// Content-browser path to the Level Sequence to render.
    pub sequence_path: String,
    /// Filesystem output directory for rendered frames.
    pub output_dir: String,
    /// Render passes to produce per frame: `"png"`, `"depth_exr"`, `"normal_exr"`.
    /// Defaults to `["png"]` if omitted.
    #[serde(default)]
    pub passes: Vec<String>,
    /// Time-of-day lighting preset name to apply before rendering (optional).
    #[serde(default)]
    pub tod_preset: Option<String>,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `ue5_mrq_status`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Ue5MrqStatusParams {
    /// MRQ job ID returned by `ue5_mrq_submit`.
    pub job_id: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `ue5_sequencer_query`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Ue5SequencerQueryParams {
    /// Content-browser path to the level to inspect (e.g., "/Game/Maps/ProductionLevel").
    pub level_path: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `ue5_lighting_preset_list`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Ue5LightingPresetListParams {
    /// Content-browser path to the level whose lighting presets to enumerate.
    pub level_path: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Resolve agent_id for UE5 tool calls.
///
/// Priority: goal_run_id lookup → TA_AGENT_ID env → dev_session_id → "unknown".
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

/// Evaluate a policy for a UE5-specific resource URI.
fn check_unreal_policy(
    engine: &PolicyEngine,
    agent_id: &str,
    verb: &str,
    resource: &str,
) -> Result<ta_policy::PolicyDecision, McpError> {
    let request = PolicyRequest {
        agent_id: agent_id.to_string(),
        tool: "unreal".to_string(),
        verb: verb.to_string(),
        target_uri: resource.to_string(),
    };
    Ok(engine.evaluate(&request))
}

pub fn handle_ue5_python_exec(
    state: &Arc<Mutex<GatewayState>>,
    params: Ue5PythonExecParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = "unreal://script/python_exec";
    let decision = check_unreal_policy(&state.policy_engine, &agent_id, "execute", resource)?;
    enforce_policy(&decision)?;

    // Backend is not started — return a structured stub response indicating the
    // connector is installed but the Editor is not running.
    let response = json!({
        "status": "connector_not_running",
        "message": "Unreal Editor is not running or the MCP plugin is not loaded.",
        "hint": "Start the Unreal Editor with the plugin enabled, then retry.",
        "script_length": params.script.len()
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_ue5_scene_query(
    state: &Arc<Mutex<GatewayState>>,
    params: Ue5SceneQueryParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!(
        "unreal://scene/{}",
        params.level_path.trim_start_matches('/')
    );
    let decision = check_unreal_policy(&state.policy_engine, &agent_id, "read", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "level_path": params.level_path,
        "message": "Unreal Editor is not running or the MCP plugin is not loaded.",
        "hint": "Start the Unreal Editor with the plugin enabled, then retry."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_ue5_asset_list(
    state: &Arc<Mutex<GatewayState>>,
    params: Ue5AssetListParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!("unreal://assets/{}", params.path.trim_start_matches('/'));
    let decision = check_unreal_policy(&state.policy_engine, &agent_id, "read", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "path": params.path,
        "message": "Unreal Editor is not running or the MCP plugin is not loaded.",
        "hint": "Start the Unreal Editor with the plugin enabled, then retry."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_ue5_mrq_submit(
    state: &Arc<Mutex<GatewayState>>,
    params: Ue5MrqSubmitParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    // Policy gate: `unreal://render/**` — MRQ submissions require human approval.
    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!(
        "unreal://render/{}",
        params.sequence_path.trim_start_matches('/')
    );
    let decision = check_unreal_policy(&state.policy_engine, &agent_id, "submit", &resource)?;
    enforce_policy(&decision)?;

    // Default passes to ["png"] if caller omitted the field.
    let passes = if params.passes.is_empty() {
        vec!["png".to_string()]
    } else {
        params.passes.clone()
    };

    let response = json!({
        "status": "connector_not_running",
        "sequence_path": params.sequence_path,
        "output_dir": params.output_dir,
        "passes": passes,
        "tod_preset": params.tod_preset,
        "message": "Unreal Editor is not running or the MCP plugin is not loaded.",
        "hint": "Start the Unreal Editor with the plugin enabled, then retry."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_ue5_mrq_status(
    state: &Arc<Mutex<GatewayState>>,
    params: Ue5MrqStatusParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!("unreal://render/status/{}", params.job_id);
    let decision = check_unreal_policy(&state.policy_engine, &agent_id, "read", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "job_id": params.job_id,
        "message": "Unreal Editor is not running or the MCP plugin is not loaded.",
        "hint": "Start the Unreal Editor with the plugin enabled, then retry."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_ue5_sequencer_query(
    state: &Arc<Mutex<GatewayState>>,
    params: Ue5SequencerQueryParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!(
        "unreal://scene/{}",
        params.level_path.trim_start_matches('/')
    );
    let decision = check_unreal_policy(&state.policy_engine, &agent_id, "read", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "level_path": params.level_path,
        "message": "Unreal Editor is not running or the MCP plugin is not loaded.",
        "hint": "Start the Unreal Editor with the plugin enabled, then retry."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_ue5_lighting_preset_list(
    state: &Arc<Mutex<GatewayState>>,
    params: Ue5LightingPresetListParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!(
        "unreal://scene/{}",
        params.level_path.trim_start_matches('/')
    );
    let decision = check_unreal_policy(&state.policy_engine, &agent_id, "read", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "level_path": params.level_path,
        "message": "Unreal Editor is not running or the MCP plugin is not loaded.",
        "hint": "Start the Unreal Editor with the plugin enabled, then retry."
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}
