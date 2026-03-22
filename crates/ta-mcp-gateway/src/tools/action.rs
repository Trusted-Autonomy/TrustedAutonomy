// tools/action.rs — MCP handler for ta_external_action (v0.13.4).
//
// The `ta_external_action` tool is the agent-facing entry point for the
// External Action Governance Framework. When an agent wants to send an email,
// call an API, or execute any other external side effect, it calls this tool.
// TA then:
//
//   1. Validates the payload against the action type's schema.
//   2. Checks the rate limit for this goal + action type.
//   3. Applies policy (auto / review / block).
//   4. Captures the attempt to `.ta/action-log.jsonl` (every path).
//   5. Returns the outcome to the agent.
//
// Policy outcomes:
//   - Block  → error returned; agent knows the action is forbidden.
//   - Review → captured and added to pending_actions for human review in `ta draft view`.
//   - Auto   → executed via plugin (stubs return a clear "not implemented" message).
//
// Dry-run mode overrides all policies: action is logged but never executed
// or captured for review. Useful for testing workflow definitions.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;
use uuid::Uuid;

use ta_actions::{
    ActionCapture, ActionOutcome, ActionPolicies, ActionPolicy, ActionRegistry, CapturedAction,
    RateLimitResult,
};
use ta_changeset::draft_package::{ActionKind, ArtifactDisposition, PendingAction};

use crate::server::GatewayState;
use crate::validation::parse_uuid;

// ── Handler ──────────────────────────────────────────────────────────────────

/// Handle a `ta_external_action` call from an agent.
pub fn handle_external_action(
    state: &Arc<Mutex<GatewayState>>,
    params: ExternalActionParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let goal_run_id = params.goal_run_id.as_deref().map(parse_uuid).transpose()?;

    // Validate the action type against the registry.
    let registry = ActionRegistry::new();
    let action_impl = registry.get(&params.action_type).ok_or_else(|| {
        McpError::invalid_params(
            format!(
                "unknown action type '{}'. Registered types: {}",
                params.action_type,
                registry
                    .list()
                    .iter()
                    .map(|t| t.action_type.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
        )
    })?;

    // Validate the payload against the action type's schema.
    if let Err(e) = action_impl.validate(&params.payload) {
        return Err(McpError::invalid_params(
            format!(
                "payload validation failed for '{}': {}",
                params.action_type, e
            ),
            None,
        ));
    }

    // Load action policies from .ta/workflow.toml.
    let workflow_toml = state
        .config
        .workspace_root
        .join(".ta")
        .join("workflow.toml");
    let policies = ActionPolicies::load(&workflow_toml);
    let policy_config = policies.policy_for(&params.action_type);

    // Resolve effective policy: dry_run overrides everything.
    let dry_run = params.dry_run;

    // Rate limit check (only for review/auto — blocked actions don't consume budget).
    let rate_check = if policy_config.policy == ActionPolicy::Block {
        // Blocked actions skip the rate limiter entirely.
        RateLimitResult::Unlimited
    } else if let Some(goal_id) = goal_run_id {
        state
            .action_rate_limiter
            .check(goal_id, &params.action_type, policy_config.rate_limit)
    } else {
        RateLimitResult::Unlimited
    };

    // Determine the action outcome.
    let (outcome, pending_action) = if dry_run {
        // Dry run: log only, no execution, no review capture.
        (ActionOutcome::DryRun, None)
    } else if let RateLimitResult::Exceeded { limit, current } = rate_check {
        (ActionOutcome::RateLimited { limit, current }, None)
    } else {
        match &policy_config.policy {
            ActionPolicy::Block => (
                ActionOutcome::Blocked {
                    reason: format!(
                        "action type '{}' is blocked by policy (configure in .ta/workflow.toml)",
                        params.action_type
                    ),
                },
                None,
            ),

            ActionPolicy::Review => {
                // Add to pending_actions so it surfaces in `ta draft view`.
                let action_id = Uuid::new_v4();
                let description = build_description(&params);
                let pending = PendingAction {
                    action_id,
                    tool_name: format!("ta_external_action:{}", params.action_type),
                    parameters: params.payload.clone(),
                    kind: ActionKind::StateChanging,
                    intercepted_at: Utc::now(),
                    description,
                    target_uri: params.target_uri.clone(),
                    disposition: ArtifactDisposition::Pending,
                };
                (ActionOutcome::CapturedForReview, Some(pending))
            }

            ActionPolicy::Auto => {
                // Execute via plugin. Stubs return StubOnly error.
                match action_impl.execute(&params.payload) {
                    Ok(result) => (ActionOutcome::Executed { result }, None),
                    Err(ta_actions::ActionError::StubOnly(_)) => {
                        // Stub: log as executed with a clear placeholder result.
                        let result = serde_json::json!({
                            "status": "stub_executed",
                            "message": format!(
                                "Action type '{}' has no registered plugin executor. \
                                 Register a plugin via the ActionRegistry to provide \
                                 real execution. The action has been logged.",
                                params.action_type
                            )
                        });
                        (ActionOutcome::Executed { result }, None)
                    }
                    Err(e) => {
                        return Err(McpError::internal_error(
                            format!("action execution failed: {}", e),
                            None,
                        ));
                    }
                }
            }
        }
    };

    // Capture to the action log (every code path).
    let goal_title = goal_run_id
        .and_then(|id| state.goal_store.get(id).ok().flatten())
        .map(|g| g.title.clone());

    let ta_dir = state.config.workspace_root.join(".ta");
    let capture = ActionCapture::new(&ta_dir);
    let captured = CapturedAction::new(
        &params.action_type,
        params.payload.clone(),
        goal_run_id,
        goal_title,
        policy_config.policy.clone(),
        outcome.clone(),
        dry_run,
    );
    if let Err(e) = capture.append(&captured) {
        tracing::warn!(
            action_type = %params.action_type,
            error = %e,
            "failed to write to action log"
        );
    }

    // Wire review capture into state.pending_actions.
    if let Some(pending) = pending_action {
        if let Some(goal_id) = goal_run_id {
            state
                .pending_actions
                .entry(goal_id)
                .or_default()
                .push(pending);
        }
    }

    // Increment rate limiter (after all checks, for review and auto only).
    if !dry_run
        && !matches!(
            &outcome,
            ActionOutcome::Blocked { .. } | ActionOutcome::RateLimited { .. }
        )
    {
        if let Some(goal_id) = goal_run_id {
            state
                .action_rate_limiter
                .increment(goal_id, &params.action_type);
        }
    }

    // Build response.
    let response = build_response(
        &params.action_type,
        &outcome,
        dry_run,
        &policy_config,
        goal_run_id,
    );
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn build_description(params: &ExternalActionParams) -> String {
    match params.action_type.as_str() {
        "email" => {
            let to = params
                .payload
                .get("to")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let subject = params
                .payload
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("(no subject)");
            format!("Send email to {} -- \"{}\"", to, subject)
        }
        "social_post" => {
            let platform = params
                .payload
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let preview = params
                .payload
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| {
                    if s.len() > 60 {
                        format!("{}…", &s[..60])
                    } else {
                        s.to_owned()
                    }
                })
                .unwrap_or_else(|| "(no content)".into());
            format!("Post to {} -- \"{}\"", platform, preview)
        }
        "api_call" => {
            let method = params
                .payload
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let url = params
                .payload
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("{} {}", method, url)
        }
        "db_query" => {
            let query = params
                .payload
                .get("query")
                .and_then(|v| v.as_str())
                .map(|s| {
                    if s.len() > 80 {
                        format!("{}…", &s[..80])
                    } else {
                        s.to_owned()
                    }
                })
                .unwrap_or_else(|| "(no query)".into());
            format!("DB query: {}", query)
        }
        _ => format!("External action: {}", params.action_type),
    }
}

fn build_response(
    action_type: &str,
    outcome: &ActionOutcome,
    dry_run: bool,
    policy_config: &ta_actions::ActionPolicyConfig,
    goal_run_id: Option<Uuid>,
) -> serde_json::Value {
    let base = serde_json::json!({
        "action_type": action_type,
        "dry_run": dry_run,
        "policy": policy_config.policy.to_string(),
        "goal_run_id": goal_run_id.map(|id| id.to_string()),
    });

    let mut obj = base.as_object().unwrap().clone();

    match outcome {
        ActionOutcome::DryRun => {
            obj.insert("outcome".into(), "dry_run".into());
            obj.insert(
                "message".into(),
                format!(
                    "Dry-run: action '{}' would be {}d (policy: {}). \
                     No capture or execution occurred.",
                    action_type, policy_config.policy, policy_config.policy
                )
                .into(),
            );
        }
        ActionOutcome::RateLimited { limit, current } => {
            obj.insert("outcome".into(), "rate_limited".into());
            obj.insert(
                "message".into(),
                format!(
                    "Rate limit exceeded for '{}': {} of {} allowed per goal. \
                     Configure in .ta/workflow.toml under [actions.{}].rate_limit.",
                    action_type, current, limit, action_type
                )
                .into(),
            );
        }
        ActionOutcome::Blocked { reason } => {
            obj.insert("outcome".into(), "blocked".into());
            obj.insert("message".into(), reason.clone().into());
        }
        ActionOutcome::CapturedForReview => {
            obj.insert("outcome".into(), "captured_for_review".into());
            obj.insert(
                "message".into(),
                format!(
                    "Action '{}' captured for human review. It will appear under \
                     'Pending Actions' in `ta draft view`. The action will only be \
                     executed after human approval.",
                    action_type
                )
                .into(),
            );
        }
        ActionOutcome::Executed { result } => {
            obj.insert("outcome".into(), "executed".into());
            obj.insert("result".into(), result.clone());
        }
    }

    serde_json::Value::Object(obj)
}

// ── Params struct (defined here, referenced in server.rs) ────────────────────

// Note: ExternalActionParams is defined in server.rs and imported by the tool
// method. The handler is called with the deserialized params.

pub use crate::server::ExternalActionParams;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    use crate::config::GatewayConfig;
    use crate::server::GatewayState;

    fn make_state(root: &std::path::Path) -> Arc<Mutex<GatewayState>> {
        let config = GatewayConfig::for_project(root);
        let state = GatewayState::new(config).expect("state init failed");
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn unknown_action_type_returns_error() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "not_a_real_action".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
        };

        let result = handle_external_action(&state, params);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_payload_returns_error() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "email".into(),
            payload: json!({"to": "alice@example.com"}), // missing subject and body
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
        };

        let result = handle_external_action(&state, params);
        assert!(result.is_err());
    }

    #[test]
    fn dry_run_succeeds_with_dry_run_outcome() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "email".into(),
            payload: json!({"to": "a@b.com", "subject": "hi", "body": "hello"}),
            goal_run_id: None,
            target_uri: None,
            dry_run: true,
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        // Dry run action log entry should exist.
        let log_path = dir.path().join(".ta").join("action-log.jsonl");
        assert!(log_path.exists(), "action log should be created");
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("dry_run"));
    }

    #[test]
    fn review_policy_adds_to_pending_actions() {
        let dir = tempdir().unwrap();

        // Write a workflow.toml with email policy=review.
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.email]\npolicy = \"review\"\n",
        )
        .unwrap();

        let state = make_state(dir.path());

        let goal_id = Uuid::new_v4();
        let params = ExternalActionParams {
            action_type: "email".into(),
            payload: json!({"to": "alice@example.com", "subject": "Test", "body": "Body text"}),
            goal_run_id: Some(goal_id.to_string()),
            target_uri: None,
            dry_run: false,
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        // Verify the pending action was added to state.
        let state_guard = state.lock().unwrap();
        let pending = state_guard.pending_actions.get(&goal_id);
        assert!(
            pending.is_some(),
            "pending action should be stored in state"
        );
        assert_eq!(pending.unwrap().len(), 1);
        let action = &pending.unwrap()[0];
        assert_eq!(action.tool_name, "ta_external_action:email");
    }

    #[test]
    fn block_policy_returns_blocked_outcome() {
        let dir = tempdir().unwrap();

        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.social_post]\npolicy = \"block\"\n",
        )
        .unwrap();

        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "social_post".into(),
            payload: json!({"platform": "twitter", "content": "Hello world"}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
        };

        let result = handle_external_action(&state, params).unwrap();
        // Blocked returns a success response with outcome=blocked (not an MCP error).
        assert!(!result.is_error.unwrap_or(false));

        // The action should still be in the log.
        let log = std::fs::read_to_string(ta_dir.join("action-log.jsonl")).unwrap();
        assert!(log.contains("blocked"));
    }

    #[test]
    fn rate_limit_enforced_after_threshold() {
        let dir = tempdir().unwrap();

        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.email]\npolicy = \"review\"\nrate_limit = 2\n",
        )
        .unwrap();

        let state = make_state(dir.path());
        let goal_id = Uuid::new_v4();

        let make_params = || ExternalActionParams {
            action_type: "email".into(),
            payload: json!({"to": "a@b.com", "subject": "s", "body": "b"}),
            goal_run_id: Some(goal_id.to_string()),
            target_uri: None,
            dry_run: false,
        };

        // First two should succeed (review).
        handle_external_action(&state, make_params()).unwrap();
        handle_external_action(&state, make_params()).unwrap();

        // Third should be rate-limited.
        let result = handle_external_action(&state, make_params()).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        // Check outcome in first content item.
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(
            text.contains("rate_limited"),
            "expected rate_limited outcome: {}",
            text
        );
    }

    #[test]
    fn auto_policy_stub_returns_stub_executed() {
        let dir = tempdir().unwrap();

        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.api_call]\npolicy = \"auto\"\n",
        )
        .unwrap();

        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "api_call".into(),
            payload: json!({"method": "GET", "url": "https://api.example.com/status"}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(
            text.contains("executed"),
            "expected executed outcome: {}",
            text
        );
        assert!(
            text.contains("stub_executed"),
            "expected stub_executed status: {}",
            text
        );
    }
}
