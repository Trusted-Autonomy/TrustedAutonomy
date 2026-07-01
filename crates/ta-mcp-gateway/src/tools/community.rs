// tools/community.rs — Community Knowledge Hub MCP tool handlers (v0.17.0.12.4).
//
// Exposes community_search, community_get, community_annotate, community_feedback,
// and community_suggest as native MCP tools on the `ta` gateway server. Each tool
// spawns the `ta-community-hub` plugin binary and speaks its JSON-over-stdio
// protocol (one JSON request line in, one JSON response line out, process exits).
// See plugins/ta-community-hub/src/main.rs for the protocol definition.
//
// If `ta-community-hub` is not installed, tools return a `not_configured` status
// rather than failing — the binary is an optional add-on (see install_local.sh).

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::server::GatewayState;

/// Parameters for `community_search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommunitySearchParams {
    /// Search query text.
    pub query: String,
    /// Filter to resources with this intent (e.g., "api-integration").
    #[serde(default)]
    pub intent: Option<String>,
    /// Filter to a specific resource by name.
    #[serde(default)]
    pub resource: Option<String>,
    /// Approximate token budget for returned content (default 4000).
    #[serde(default)]
    pub token_budget: Option<u64>,
}

/// Parameters for `community_get`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommunityGetParams {
    /// Document ID (`<resource-name>/<path>`).
    pub id: String,
    /// Approximate token budget for returned content (default 4000).
    #[serde(default)]
    pub token_budget: Option<u64>,
}

/// Parameters for `community_annotate`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommunityAnnotateParams {
    /// Document ID (`<resource-name>/<path>`) the annotation applies to.
    pub id: String,
    /// The gap or note text to stage for human review.
    pub note: String,
    /// Kind of gap being flagged (default "gap").
    #[serde(default)]
    pub gap_type: Option<String>,
    /// Goal run ID for audit tracking.
    #[serde(default)]
    pub goal_id: Option<String>,
}

/// Parameters for `community_feedback`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommunityFeedbackParams {
    /// Document ID (`<resource-name>/<path>`) being rated.
    pub id: String,
    /// "upvote" or "downvote".
    pub rating: String,
    /// Optional context explaining the rating.
    #[serde(default)]
    pub context: Option<String>,
    /// Goal run ID for audit tracking.
    #[serde(default)]
    pub goal_id: Option<String>,
}

/// Parameters for `community_suggest`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommunitySuggestParams {
    /// Title of the proposed document.
    pub title: String,
    /// Proposed document content.
    pub content: String,
    /// Intent this document serves (e.g., "api-integration").
    pub intent: String,
    /// Name of the resource to propose this document under.
    pub resource: String,
}

/// Resolve the `ta-community-hub` binary path.
///
/// Resolution order: `TA_COMMUNITY_HUB_BINARY` env var, then `which` on PATH
/// (covers `~/.local/bin`, where `install_local.sh` installs it).
fn resolve_community_hub_binary() -> Option<String> {
    if let Ok(val) = std::env::var("TA_COMMUNITY_HUB_BINARY") {
        let val = val.trim().to_string();
        if !val.is_empty() {
            return Some(val);
        }
    }
    which::which("ta-community-hub")
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

fn not_configured(method: &str) -> Result<CallToolResult, McpError> {
    let response = json!({
        "status": "not_configured",
        "message": "ta-community-hub binary is not installed or not on PATH.",
        "hint": "Run `./install_local.sh` to build and install ta-community-hub to ~/.local/bin, \
                 or set TA_COMMUNITY_HUB_BINARY to a custom binary path.",
        "tool": method,
    });
    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

/// Spawn `ta-community-hub`, send one JSON request line, read one JSON response line.
fn call_hub(
    workspace_root: &std::path::Path,
    method: &str,
    mut params: serde_json::Value,
) -> Result<CallToolResult, McpError> {
    let Some(binary) = resolve_community_hub_binary() else {
        return not_configured(method);
    };

    if let Some(obj) = params.as_object_mut() {
        obj.insert(
            "workspace_path".to_string(),
            json!(workspace_root.display().to_string()),
        );
    }

    let request = json!({ "method": method, "params": params });
    let request_line = serde_json::to_string(&request)
        .map_err(|e| McpError::internal_error(format!("failed to encode request: {}", e), None))?;

    let mut child = Command::new(&binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            McpError::internal_error(
                format!("failed to spawn ta-community-hub at '{}': {}", binary, e),
                None,
            )
        })?;

    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            McpError::internal_error("failed to open stdin for ta-community-hub", None)
        })?;
        writeln!(stdin, "{}", request_line).map_err(|e| {
            McpError::internal_error(
                format!("failed to write to ta-community-hub stdin: {}", e),
                None,
            )
        })?;
    }

    let output = child.wait_with_output().map_err(|e| {
        McpError::internal_error(format!("failed waiting on ta-community-hub: {}", e), None)
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(McpError::internal_error(
            format!(
                "ta-community-hub produced no output for method '{}'. stderr: {}",
                method, stderr
            ),
            None,
        ));
    }

    let parsed: serde_json::Value = serde_json::from_str(line).map_err(|e| {
        McpError::internal_error(
            format!("invalid JSON from ta-community-hub: {} (line: {})", e, line),
            None,
        )
    })?;

    Ok(CallToolResult::success(vec![Content::json(parsed)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_community_search(
    state: &Arc<Mutex<GatewayState>>,
    params: CommunitySearchParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    call_hub(
        &state.config.workspace_root,
        "community_search",
        json!({
            "query": params.query,
            "intent": params.intent,
            "resource": params.resource,
            "token_budget": params.token_budget,
        }),
    )
}

pub fn handle_community_get(
    state: &Arc<Mutex<GatewayState>>,
    params: CommunityGetParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    call_hub(
        &state.config.workspace_root,
        "community_get",
        json!({
            "id": params.id,
            "token_budget": params.token_budget,
        }),
    )
}

pub fn handle_community_annotate(
    state: &Arc<Mutex<GatewayState>>,
    params: CommunityAnnotateParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    call_hub(
        &state.config.workspace_root,
        "community_annotate",
        json!({
            "id": params.id,
            "note": params.note,
            "gap_type": params.gap_type,
            "goal_id": params.goal_id,
        }),
    )
}

pub fn handle_community_feedback(
    state: &Arc<Mutex<GatewayState>>,
    params: CommunityFeedbackParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    call_hub(
        &state.config.workspace_root,
        "community_feedback",
        json!({
            "id": params.id,
            "rating": params.rating,
            "context": params.context,
            "goal_id": params.goal_id,
        }),
    )
}

pub fn handle_community_suggest(
    state: &Arc<Mutex<GatewayState>>,
    params: CommunitySuggestParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    call_hub(
        &state.config.workspace_root,
        "community_suggest",
        json!({
            "title": params.title,
            "content": params.content,
            "intent": params.intent,
            "resource": params.resource,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both cases below touch TA_COMMUNITY_HUB_BINARY; kept in one test to avoid
    // the env-var races that parallel `set_var`/`remove_var` calls produce
    // (same hazard documented in server.rs's resolve_agent_id_priority_order).
    #[test]
    fn resolve_binary_env_override_and_not_configured_fallback() {
        std::env::set_var("TA_COMMUNITY_HUB_BINARY", "/custom/path/ta-community-hub");
        assert_eq!(
            resolve_community_hub_binary().as_deref(),
            Some("/custom/path/ta-community-hub")
        );
        std::env::remove_var("TA_COMMUNITY_HUB_BINARY");

        // With no binary on PATH and no env override, calling the hub should
        // degrade gracefully rather than erroring — unless a real
        // ta-community-hub happens to be installed on the test machine's PATH.
        if which::which("ta-community-hub").is_err() {
            let result = call_hub(
                std::path::Path::new("/nonexistent/workspace"),
                "community_search",
                json!({ "query": "test" }),
            )
            .unwrap();
            let text = match &result.content[0].raw {
                RawContent::Text(t) => t.text.clone(),
                _ => panic!("expected text content"),
            };
            assert!(
                text.contains("not_configured"),
                "expected not_configured status, got: {}",
                text
            );
        }
    }
}
