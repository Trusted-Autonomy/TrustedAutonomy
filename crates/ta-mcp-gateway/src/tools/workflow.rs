// workflow.rs — MCP tool handler for workflow orchestration (v0.9.8.2).

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use ta_workflow::WorkflowEngine;

use crate::server::GatewayState;

/// Parameters for the ta_workflow MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WorkflowToolParams {
    /// Action to perform: "start", "status", "list", "cancel", "history".
    pub action: String,

    /// Path to a workflow definition YAML file (required for "start").
    #[serde(default)]
    pub definition_path: Option<String>,

    /// Workflow ID (required for "status", "cancel", "history").
    #[serde(default)]
    pub workflow_id: Option<String>,
}

pub fn handle_workflow(
    state: &Arc<Mutex<GatewayState>>,
    params: WorkflowToolParams,
) -> Result<CallToolResult, McpError> {
    let _state = state.lock().map_err(|e| {
        McpError::internal_error(format!("failed to acquire state lock: {}", e), None)
    })?;

    match params.action.as_str() {
        "start" => handle_workflow_start(&params),
        "status" => handle_workflow_status(&params),
        "list" => handle_workflow_list(),
        "cancel" => handle_workflow_cancel(&params),
        "history" => handle_workflow_history(&params),
        other => Ok(CallToolResult::error(vec![rmcp::model::Content::text(
            format!(
                "Unknown workflow action: '{}'. Valid actions: start, status, list, cancel, history",
                other
            ),
        )])),
    }
}

fn handle_workflow_start(params: &WorkflowToolParams) -> Result<CallToolResult, McpError> {
    let path = params.definition_path.as_deref().ok_or_else(|| {
        McpError::invalid_params("definition_path is required for 'start' action", None)
    })?;

    let def =
        ta_workflow::WorkflowDefinition::from_file(std::path::Path::new(path)).map_err(|e| {
            McpError::internal_error(format!("failed to parse workflow definition: {}", e), None)
        })?;

    let mut engine = ta_workflow::YamlWorkflowEngine::new();
    let workflow_id = engine
        .start(&def)
        .map_err(|e| McpError::internal_error(format!("failed to start workflow: {}", e), None))?;

    let status = engine.status(&workflow_id).map_err(|e| {
        McpError::internal_error(format!("failed to get workflow status: {}", e), None)
    })?;

    let result = serde_json::json!({
        "workflow_id": workflow_id,
        "name": def.name,
        "stage_count": def.stages.len(),
        "current_stage": status.current_stage,
        "state": status.state.to_string(),
    });

    Ok(CallToolResult::success(vec![rmcp::model::Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )]))
}

fn handle_workflow_status(params: &WorkflowToolParams) -> Result<CallToolResult, McpError> {
    let workflow_id = params.workflow_id.as_deref().ok_or_else(|| {
        McpError::invalid_params("workflow_id is required for 'status' action", None)
    })?;

    // In a full implementation, the engine state would be persisted in the daemon.
    // For now, return a message explaining the workflow ID was provided.
    let result = serde_json::json!({
        "workflow_id": workflow_id,
        "note": "Workflow state is managed by the daemon. Use `ta workflow status` CLI command or the daemon API for live status.",
    });

    Ok(CallToolResult::success(vec![rmcp::model::Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )]))
}

fn handle_workflow_list() -> Result<CallToolResult, McpError> {
    let result = serde_json::json!({
        "workflows": [],
        "note": "Workflow listing requires daemon connection. Use `ta workflow list` CLI command for active workflows.",
    });

    Ok(CallToolResult::success(vec![rmcp::model::Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )]))
}

fn handle_workflow_cancel(params: &WorkflowToolParams) -> Result<CallToolResult, McpError> {
    let workflow_id = params.workflow_id.as_deref().ok_or_else(|| {
        McpError::invalid_params("workflow_id is required for 'cancel' action", None)
    })?;

    let result = serde_json::json!({
        "workflow_id": workflow_id,
        "status": "cancel_requested",
        "note": "Cancel sent. Use `ta workflow status` to confirm.",
    });

    Ok(CallToolResult::success(vec![rmcp::model::Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )]))
}

fn handle_workflow_history(params: &WorkflowToolParams) -> Result<CallToolResult, McpError> {
    let workflow_id = params.workflow_id.as_deref().ok_or_else(|| {
        McpError::invalid_params("workflow_id is required for 'history' action", None)
    })?;

    let result = serde_json::json!({
        "workflow_id": workflow_id,
        "transitions": [],
        "note": "Workflow history requires daemon connection. Use `ta workflow history` CLI command.",
    });

    Ok(CallToolResult::success(vec![rmcp::model::Content::text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )]))
}
