// validation.rs — Shared helpers for MCP tool handlers.

use rmcp::ErrorData as McpError;
use uuid::Uuid;

use ta_goal::{GoalRun, GoalRunStore};
use ta_policy::PolicyDecision;

/// Parse a UUID string, returning an MCP error on failure.
pub fn parse_uuid(s: &str) -> Result<Uuid, McpError> {
    Uuid::parse_str(s)
        .map_err(|e| McpError::invalid_params(format!("invalid UUID '{}': {}", s, e), None))
}

/// Validate that a goal_run_id corresponds to an existing goal in the store.
/// Returns the goal on success, or an MCP invalid_params error if not found.
pub fn validate_goal_exists(
    goal_store: &GoalRunStore,
    goal_run_id: Uuid,
) -> Result<GoalRun, McpError> {
    goal_store
        .get(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(format!("goal_run_id not found: {}", goal_run_id), None)
        })
}

/// Enforce a policy decision, returning an MCP error if denied.
pub fn enforce_policy(decision: &PolicyDecision) -> Result<(), McpError> {
    match decision {
        PolicyDecision::Allow => Ok(()),
        PolicyDecision::Deny { reason } => Err(McpError::invalid_request(
            format!("Policy denied: {}", reason),
            None,
        )),
        PolicyDecision::RequireApproval { reason } => {
            // For now, RequireApproval is treated as allowed since the
            // CLI approval flow will handle the actual gating.
            tracing::info!("action requires approval: {}", reason);
            Ok(())
        }
    }
}
