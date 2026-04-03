// tools/unity.rs — Unity Engine tool handlers.
//
// These tools route to a Unity MCP server through the policy engine.
// Build triggers are gated behind `unity://build/**`.
// Test runs are gated behind `unity://test/**`.
// Scene queries are gated behind `unity://scene/**`.
// Render captures are gated behind `unity://render/**`.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use ta_policy::{PolicyEngine, PolicyRequest};

use crate::server::GatewayState;
use crate::validation::enforce_policy;

// ── Parameter types ───────────────────────────────────────────────────────────

/// Parameters for `unity_build_trigger`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnityBuildTriggerParams {
    /// Build target: "StandaloneOSX", "StandaloneWindows64", "WebGL", "AssetBundle", etc.
    pub target: String,
    /// Optional build configuration: "Debug" or "Release" (default: "Release").
    #[serde(default)]
    pub config: Option<String>,
    /// Goal run ID (for audit tracking).
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `unity_scene_query`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnitySceneQueryParams {
    /// Asset path of the scene to query (e.g. "Assets/Scenes/Main.unity").
    /// Pass an empty string to query the currently-open scene.
    #[serde(default)]
    pub scene_path: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `unity_test_run`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnityTestRunParams {
    /// Optional test name filter (substring match). Omit to run all tests.
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `unity_addressables_build`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnityAddressablesBuildParams {
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

/// Parameters for `unity_render_capture`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnityRenderCaptureParams {
    /// GameObject path to the camera (e.g. "/Main Camera").
    pub camera_path: String,
    /// Destination file path for the PNG (relative to Unity project root).
    pub output_path: String,
    #[serde(default)]
    pub goal_run_id: Option<String>,
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Validate a Unity build target or simple identifier.
/// Rejects values containing `/`, `\`, or `..` to prevent policy-engine URI
/// manipulation before the connector is wired to a real backend.
/// Returns a structured `invalid_parameter` error on failure.
fn validate_unity_identifier(value: &str, field: &str) -> Result<(), McpError> {
    if value.contains("..") || value.contains('/') || value.contains('\\') {
        return Err(McpError::invalid_params(
            format!(
                "invalid_parameter: '{}' must not contain path separators or traversal \
                 sequences ('/', '\\\\', '..'): got {:?}",
                field, value
            ),
            None,
        ));
    }
    Ok(())
}

/// Validate a Unity scene/camera path.
/// The '/' separator is legitimate in Unity hierarchy paths (e.g. "/Main Camera"),
/// but `..` traversal and backslashes must be rejected.
fn validate_unity_path(value: &str, field: &str) -> Result<(), McpError> {
    if value.contains("..") || value.contains('\\') {
        return Err(McpError::invalid_params(
            format!(
                "invalid_parameter: '{}' must not contain path traversal sequences ('..') \
                 or backslashes: got {:?}",
                field, value
            ),
            None,
        ));
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

fn check_unity_policy(
    engine: &PolicyEngine,
    agent_id: &str,
    verb: &str,
    resource: &str,
) -> Result<ta_policy::PolicyDecision, McpError> {
    let request = PolicyRequest {
        agent_id: agent_id.to_string(),
        tool: "unity".to_string(),
        verb: verb.to_string(),
        target_uri: resource.to_string(),
    };
    Ok(engine.evaluate(&request))
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub fn handle_unity_build_trigger(
    state: &Arc<Mutex<GatewayState>>,
    params: UnityBuildTriggerParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    validate_unity_identifier(&params.target, "target")?;
    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!("unity://build/{}", params.target);
    let decision = check_unity_policy(&state.policy_engine, &agent_id, "trigger", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "message": "Unity MCP server is not reachable. Ensure the Unity Editor is open with com.unity.mcp-server installed.",
        "hint": "Run `ta connector install unity` for setup instructions, or check [connectors.unity] in your config.",
        "target": params.target,
        "config": params.config.as_deref().unwrap_or("Release"),
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_unity_scene_query(
    state: &Arc<Mutex<GatewayState>>,
    params: UnitySceneQueryParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let scene = if params.scene_path.is_empty() {
        "active".to_string()
    } else {
        params.scene_path.clone()
    };
    let resource = format!("unity://scene/{}", scene);
    let decision = check_unity_policy(&state.policy_engine, &agent_id, "read", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "message": "Unity MCP server is not reachable.",
        "hint": "Ensure the Unity Editor is open and com.unity.mcp-server is installed. Check [connectors.unity] socket in config.",
        "scene_path": params.scene_path,
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_unity_test_run(
    state: &Arc<Mutex<GatewayState>>,
    params: UnityTestRunParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = "unity://test/run";
    let decision = check_unity_policy(&state.policy_engine, &agent_id, "run", resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "message": "Unity MCP server is not reachable.",
        "hint": "Ensure the Unity Editor is open with com.unity.mcp-server installed.",
        "filter": params.filter,
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_unity_addressables_build(
    state: &Arc<Mutex<GatewayState>>,
    params: UnityAddressablesBuildParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = "unity://build/addressables";
    let decision = check_unity_policy(&state.policy_engine, &agent_id, "trigger", resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "message": "Unity MCP server is not reachable.",
        "hint": "Ensure the Unity Editor is open with com.unity.mcp-server installed and Addressables package configured.",
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

pub fn handle_unity_render_capture(
    state: &Arc<Mutex<GatewayState>>,
    params: UnityRenderCaptureParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    validate_unity_path(&params.camera_path, "camera_path")?;
    let agent_id = resolve_agent_id(&state, params.goal_run_id.as_deref());
    let resource = format!("unity://render/capture/{}", params.camera_path);
    let decision = check_unity_policy(&state.policy_engine, &agent_id, "capture", &resource)?;
    enforce_policy(&decision)?;

    let response = json!({
        "status": "connector_not_running",
        "message": "Unity MCP server is not reachable.",
        "hint": "Ensure the Unity Editor is open with com.unity.mcp-server installed.",
        "camera_path": params.camera_path,
        "output_path": params.output_path,
    });

    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&response).unwrap_or_default(),
    )]))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GatewayConfig;
    use crate::server::GatewayState;
    use ta_policy::{AlignmentProfile, AutonomyEnvelope, CoordinationConfig};
    use tempfile::tempdir;

    /// Create a GatewayState with a running goal scoped to unity://** and return
    /// the state + goal_run_id. The profile grants all four Unity verbs so that
    /// stub-response tests can reach the connector_not_running path.
    fn make_state_with_goal(root: &std::path::Path) -> (Arc<Mutex<GatewayState>>, String) {
        let config = GatewayConfig::for_project(root);
        let mut raw = GatewayState::new(config).expect("state init failed");

        let profile = AlignmentProfile {
            principal: "test".to_string(),
            autonomy_envelope: AutonomyEnvelope {
                bounded_actions: vec![
                    "unity_trigger".to_string(),
                    "unity_read".to_string(),
                    "unity_run".to_string(),
                    "unity_capture".to_string(),
                ],
                escalation_triggers: vec![],
                forbidden_actions: vec![],
            },
            constitution: "default-v1".to_string(),
            coordination: CoordinationConfig::default(),
        };

        let goal = raw
            .start_goal_with_profile(
                "test",
                "unity handler stub tests",
                "test-agent",
                &profile,
                Some(vec!["unity://**".to_string()]),
            )
            .expect("start_goal_with_profile failed");
        let goal_id = goal.goal_run_id.to_string();
        (Arc::new(Mutex::new(raw)), goal_id)
    }

    fn extract_text(result: CallToolResult) -> String {
        result
            .content
            .into_iter()
            .find_map(|c| {
                if let rmcp::model::RawContent::Text(t) = c.raw {
                    Some(t.text)
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }

    // ── unity_build_trigger ──────────────────────────────────────────────────

    #[test]
    fn build_trigger_returns_connector_not_running() {
        // (a) stub response structure; (b) valid target is accepted
        let dir = tempdir().unwrap();
        let (state, gid) = make_state_with_goal(dir.path());
        let params = UnityBuildTriggerParams {
            target: "StandaloneOSX".into(),
            config: None,
            goal_run_id: Some(gid),
        };
        let result = handle_unity_build_trigger(&state, params).unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(result)).unwrap();
        assert_eq!(v["status"], "connector_not_running");
        assert_eq!(v["target"], "StandaloneOSX");
    }

    #[test]
    fn build_trigger_rejects_path_traversal_in_target() {
        // (b) path traversal in target must be rejected before policy evaluation
        let dir = tempdir().unwrap();
        let (state, _) = make_state_with_goal(dir.path());
        let params = UnityBuildTriggerParams {
            target: "StandaloneOSX/../render/capture/foo".into(),
            config: None,
            goal_run_id: None,
        };
        let err = handle_unity_build_trigger(&state, params).unwrap_err();
        assert!(
            err.message.contains("invalid_parameter"),
            "expected invalid_parameter error, got: {}",
            err.message
        );
    }

    // ── unity_scene_query ────────────────────────────────────────────────────

    #[test]
    fn scene_query_returns_connector_not_running() {
        // (a) stub response structure; (b) policy URI for active scene is well-formed
        let dir = tempdir().unwrap();
        let (state, gid) = make_state_with_goal(dir.path());
        let params = UnitySceneQueryParams {
            scene_path: "".into(),
            goal_run_id: Some(gid),
        };
        let result = handle_unity_scene_query(&state, params).unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(result)).unwrap();
        assert_eq!(v["status"], "connector_not_running");
    }

    // ── unity_test_run ───────────────────────────────────────────────────────

    #[test]
    fn test_run_returns_connector_not_running() {
        // (a) stub response structure; (b) policy URI unity://test/run is fixed/well-formed
        let dir = tempdir().unwrap();
        let (state, gid) = make_state_with_goal(dir.path());
        let params = UnityTestRunParams {
            filter: None,
            goal_run_id: Some(gid),
        };
        let result = handle_unity_test_run(&state, params).unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(result)).unwrap();
        assert_eq!(v["status"], "connector_not_running");
    }

    // ── unity_addressables_build ─────────────────────────────────────────────

    #[test]
    fn addressables_build_returns_connector_not_running() {
        // (a) stub response structure; (b) policy URI unity://build/addressables is fixed
        let dir = tempdir().unwrap();
        let (state, gid) = make_state_with_goal(dir.path());
        let params = UnityAddressablesBuildParams {
            goal_run_id: Some(gid),
        };
        let result = handle_unity_addressables_build(&state, params).unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(result)).unwrap();
        assert_eq!(v["status"], "connector_not_running");
    }

    // ── unity_render_capture ─────────────────────────────────────────────────

    #[test]
    fn render_capture_returns_connector_not_running() {
        // (a) stub response structure; (b) valid camera_path (with '/') is accepted
        let dir = tempdir().unwrap();
        let (state, gid) = make_state_with_goal(dir.path());
        let params = UnityRenderCaptureParams {
            camera_path: "/Main Camera".into(),
            output_path: "screenshots/out.png".into(),
            goal_run_id: Some(gid),
        };
        let result = handle_unity_render_capture(&state, params).unwrap();
        let v: serde_json::Value = serde_json::from_str(&extract_text(result)).unwrap();
        assert_eq!(v["status"], "connector_not_running");
        assert_eq!(v["camera_path"], "/Main Camera");
    }

    #[test]
    fn render_capture_rejects_traversal_in_camera_path() {
        // (b) '..' traversal in camera_path must be rejected before policy evaluation
        let dir = tempdir().unwrap();
        let (state, _) = make_state_with_goal(dir.path());
        let params = UnityRenderCaptureParams {
            camera_path: "/Camera/../../../etc/passwd".into(),
            output_path: "out.png".into(),
            goal_run_id: None,
        };
        let err = handle_unity_render_capture(&state, params).unwrap_err();
        assert!(
            err.message.contains("invalid_parameter"),
            "expected invalid_parameter error, got: {}",
            err.message
        );
    }
}
