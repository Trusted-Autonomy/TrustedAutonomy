// tools/agent.rs — Agent status MCP tool handler (v0.9.6).

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;

use crate::server::{AgentStatusParams, GatewayState};

pub fn handle_agent_status(
    state: &Arc<Mutex<GatewayState>>,
    params: AgentStatusParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    match params.action.as_str() {
        "list" => {
            let agents: Vec<serde_json::Value> = state
                .active_agents
                .values()
                .map(|s| {
                    serde_json::json!({
                        "agent_id": s.agent_id,
                        "agent_type": s.agent_type,
                        "goal_run_id": s.goal_run_id.map(|id| id.to_string()),
                        "caller_mode": s.caller_mode,
                        "started_at": s.started_at.to_rfc3339(),
                        "last_heartbeat": s.last_heartbeat.to_rfc3339(),
                    })
                })
                .collect();

            let response = serde_json::json!({
                "agents": agents,
                "count": agents.len(),
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        "status" => {
            let agent_id = params.agent_id.as_deref().ok_or_else(|| {
                McpError::invalid_params("agent_id required for status action", None)
            })?;

            match state.active_agents.get(agent_id) {
                Some(session) => {
                    let goal_title = session.goal_run_id.and_then(|gid| {
                        state
                            .goal_store
                            .get(gid)
                            .ok()
                            .flatten()
                            .map(|g| g.title.clone())
                    });

                    let elapsed = chrono::Utc::now()
                        .signed_duration_since(session.started_at)
                        .num_seconds();

                    let response = serde_json::json!({
                        "agent_id": session.agent_id,
                        "agent_type": session.agent_type,
                        "goal_run_id": session.goal_run_id.map(|id| id.to_string()),
                        "goal_title": goal_title,
                        "caller_mode": session.caller_mode,
                        "started_at": session.started_at.to_rfc3339(),
                        "last_heartbeat": session.last_heartbeat.to_rfc3339(),
                        "running_secs": elapsed,
                    });
                    Ok(CallToolResult::success(vec![Content::json(response)
                        .map_err(|e| {
                            McpError::internal_error(e.to_string(), None)
                        })?]))
                }
                None => {
                    let response = serde_json::json!({
                        "status": "not_found",
                        "agent_id": agent_id,
                        "message": "No active session for this agent.",
                    });
                    Ok(CallToolResult::success(vec![Content::json(response)
                        .map_err(|e| {
                            McpError::internal_error(e.to_string(), None)
                        })?]))
                }
            }
        }
        _ => Err(McpError::invalid_params(
            format!("unknown action '{}'. Expected: list, status", params.action),
            None,
        )),
    }
}
