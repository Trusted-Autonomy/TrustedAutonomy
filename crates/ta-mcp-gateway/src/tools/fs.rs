// tools/fs.rs — Filesystem MCP tool handlers.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;

use crate::server::{FsDiffParams, FsListParams, FsReadParams, FsWriteParams, GatewayState};
use crate::validation::{enforce_policy, parse_uuid};

pub fn handle_fs_read(
    state: &Arc<Mutex<GatewayState>>,
    params: FsReadParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;
    let agent_id = state
        .agent_for_goal(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let decision = state
        .check_policy(&agent_id, "read", &params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    enforce_policy(&decision)?;

    let workspace_root = state.config.workspace_root.clone();
    let connector = state.connectors.get_mut(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let content = connector
        .read_source(&workspace_root, &params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let text = String::from_utf8_lossy(&content).to_string();
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

pub fn handle_fs_write(
    state: &Arc<Mutex<GatewayState>>,
    params: FsWriteParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    // v0.9.3: Enforce caller mode — orchestrators cannot use ta_fs_write
    // unless the target path is a whitelisted release artifact (v0.10.6).
    if state.caller_mode.is_tool_forbidden("ta_fs_write")
        && !state.caller_mode.is_write_whitelisted(&params.path)
    {
        return Err(McpError::invalid_request(
            format!(
                "ta_fs_write is forbidden in orchestrator mode for '{}'. \
                 Only release artifacts (.release-draft.md, CHANGELOG.md, version.json, \
                 .press-release-draft.md) are whitelisted. Use ta_goal to launch an \
                 implementation agent for other writes.",
                params.path
            ),
            None,
        ));
    }

    let goal_run_id = parse_uuid(&params.goal_run_id)?;
    let agent_id = state
        .agent_for_goal(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let decision = state
        .check_policy(&agent_id, "write_patch", &params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    enforce_policy(&decision)?;

    let connector = state.connectors.get_mut(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let changeset = connector
        .write_patch(&params.path, params.content.as_bytes())
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({
        "changeset_id": changeset.changeset_id.to_string(),
        "target_uri": changeset.target_uri,
        "status": "staged",
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_fs_list(
    state: &Arc<Mutex<GatewayState>>,
    params: FsListParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;

    let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let files = connector
        .list_staged()
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({ "files": files, "count": files.len() });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_fs_diff(
    state: &Arc<Mutex<GatewayState>>,
    params: FsDiffParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;

    let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let diff = connector
        .diff_file(&params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    match diff {
        Some(diff_text) => Ok(CallToolResult::success(vec![Content::text(diff_text)])),
        None => Ok(CallToolResult::success(vec![Content::text(
            "No changes (file is identical to source).",
        )])),
    }
}
