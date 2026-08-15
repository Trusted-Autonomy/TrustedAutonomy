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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;
use uuid::Uuid;

use ta_actions::{
    discover_adapter_actions, ActionCapture, ActionOutcome, ActionPolicies, ActionPolicy,
    ActionRegistry, CapturedAction, DispatchResult, EmailDispatchGuard, RateLimitResult,
    RiskAssessment, SessionRateLimiter,
};
use ta_changeset::draft_package::{ActionKind, ArtifactDisposition, PendingAction};
use ta_decision::{decide, Decision, DecisionInput, Verdict};
use ta_policy::business_budget::{self, BudgetCheckResult};
use ta_session::workflow_session::AdvisorSecurity;

use crate::server::GatewayState;
use crate::tools::human_verify::{
    handle_credential_scope_elevation, load_thresholds, resolve_workload_context,
    validate_ledger_path, HeadlessSyntheticPipeline, ScopeElevationRequest, SyntheticPipeline,
    CREDENTIAL_SCOPE_ELEVATION_WORKLOAD_TYPE,
};
use crate::validation::parse_uuid;

// ── Handler ──────────────────────────────────────────────────────────────────

/// Handle a `ta_external_action` call from an agent.
pub fn handle_external_action(
    state_arc: &Arc<Mutex<GatewayState>>,
    params: ExternalActionParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state_arc
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let goal_run_id = params.goal_run_id.as_deref().map(parse_uuid).transpose()?;

    // Validate the action type against the registry: the four built-in
    // stubs plus every verb declared by a discovered adapter plugin under
    // `.ta/plugins/adapter/<name>/plugin.toml` (v0.17.5.3) — this is the
    // registry's first real production caller of plugin-backed actions.
    // Discovery only reads manifests, it never spawns a subprocess, so this
    // stays cheap even for calls targeting an unrelated built-in type.
    let mut registry = ActionRegistry::new();
    for plugin_action in discover_adapter_actions(&state.config.workspace_root) {
        registry.register(plugin_action);
    }
    let action_impl = registry.get(&params.action_type).ok_or_else(|| {
        McpError::invalid_params(
            format!(
                "unknown action type '{}' — no adapter registered for this verb. \
                 Registered types: {}. Author a plugin under \
                 .ta/plugins/adapter/<name>/plugin.toml declaring \
                 `capabilities = [\"verb:{}\"]` to handle it (see \
                 docs/community-adapter-plugin.md).",
                params.action_type,
                registry
                    .list()
                    .iter()
                    .map(|t| t.action_type.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                params.action_type,
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

    // Apply email dispatch guard — forces email to Review regardless of policy config.
    let dispatch_guard = EmailDispatchGuard::new();
    let dispatch_result = dispatch_guard.enforce(&params.action_type, &policy_config.policy);
    let effective_policy = match &dispatch_result {
        DispatchResult::ForcedReview { reason } => {
            tracing::info!(
                action_type = %params.action_type,
                reason = %reason,
                "email dispatch guard: forced to review"
            );
            ActionPolicy::Review
        }
        DispatchResult::Blocked { message } => {
            // Return a blocked outcome immediately.
            let ta_dir = state.config.workspace_root.join(".ta");
            let capture = ActionCapture::new(&ta_dir);
            let goal_title = goal_run_id
                .and_then(|id| state.goal_store.get(id).ok().flatten())
                .map(|g| g.title.clone());
            let blocked_outcome = ActionOutcome::Blocked {
                reason: message.clone(),
            };
            let captured = CapturedAction::new(
                &params.action_type,
                params.payload.clone(),
                goal_run_id,
                goal_title,
                policy_config.policy.clone(),
                blocked_outcome.clone(),
                dry_run,
            );
            if let Err(e) = capture.append(&captured) {
                tracing::warn!(
                    action_type = %params.action_type,
                    error = %e,
                    "failed to write blocked dispatch to action log"
                );
            }
            let response = build_response(
                &params.action_type,
                &blocked_outcome,
                dry_run,
                &policy_config,
                goal_run_id,
            );
            return Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?]));
        }
        DispatchResult::Allowed => policy_config.policy.clone(),
    };

    // Cross-session rate limit check for email (max_per_hour / max_per_day).
    if !dry_run
        && params.action_type == "email"
        && (policy_config.max_per_hour.is_some() || policy_config.max_per_day.is_some())
    {
        let ta_dir = state.config.workspace_root.join(".ta");
        let mut session_limiter = SessionRateLimiter::new(&ta_dir);
        let session_check = session_limiter.check_and_record(
            &params.action_type,
            policy_config.max_per_hour,
            policy_config.max_per_day,
        );
        match session_check {
            ta_actions::SessionRateLimitResult::HourlyExceeded { limit, count } => {
                let outcome = ActionOutcome::RateLimited {
                    limit,
                    current: count,
                };
                let goal_title = goal_run_id
                    .and_then(|id| state.goal_store.get(id).ok().flatten())
                    .map(|g| g.title.clone());
                let capture = ActionCapture::new(&ta_dir);
                let captured = CapturedAction::new(
                    &params.action_type,
                    params.payload.clone(),
                    goal_run_id,
                    goal_title,
                    effective_policy.clone(),
                    outcome.clone(),
                    dry_run,
                );
                if let Err(e) = capture.append(&captured) {
                    tracing::warn!(error = %e, "failed to write rate-limited action to log");
                }
                let response = build_response(
                    &params.action_type,
                    &outcome,
                    dry_run,
                    &policy_config,
                    goal_run_id,
                );
                return Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?]));
            }
            ta_actions::SessionRateLimitResult::DailyExceeded { limit, count } => {
                let outcome = ActionOutcome::RateLimited {
                    limit,
                    current: count,
                };
                let goal_title = goal_run_id
                    .and_then(|id| state.goal_store.get(id).ok().flatten())
                    .map(|g| g.title.clone());
                let capture = ActionCapture::new(&ta_dir);
                let captured = CapturedAction::new(
                    &params.action_type,
                    params.payload.clone(),
                    goal_run_id,
                    goal_title,
                    effective_policy.clone(),
                    outcome.clone(),
                    dry_run,
                );
                if let Err(e) = capture.append(&captured) {
                    tracing::warn!(error = %e, "failed to write rate-limited action to log");
                }
                let response = build_response(
                    &params.action_type,
                    &outcome,
                    dry_run,
                    &policy_config,
                    goal_run_id,
                );
                return Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?]));
            }
            ta_actions::SessionRateLimitResult::Allowed => {}
        }
    }

    // Per-goal rate limit check (only for review/auto — blocked actions don't consume budget).
    let rate_check = if effective_policy == ActionPolicy::Block {
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
        match &effective_policy {
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
                // Check allowed_recipients for email actions.
                let recipient_flag = if params.action_type == "email"
                    && !policy_config.allowed_recipients.is_empty()
                {
                    let to = params
                        .payload
                        .get("to")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !policy_config.allowed_recipients.iter().any(|r| r == to) {
                        Some(format!(
                            "Recipient '{}' not in allowed_recipients (configure in \
                             .ta/workflow.toml under [actions.email].allowed_recipients)",
                            to
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Add to pending_actions so it surfaces in `ta draft view`.
                let action_id = Uuid::new_v4();
                let base_description = build_description(&params);
                let description = if let Some(ref flag) = recipient_flag {
                    format!("{} [FLAG: {}]", base_description, flag)
                } else {
                    base_description
                };
                if let Some(ref flag) = recipient_flag {
                    tracing::warn!(
                        action_type = %params.action_type,
                        flag = %flag,
                        "email action flagged: recipient not in allowed_recipients"
                    );
                }
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
                let requesting_agent_id = goal_run_id.and_then(|id| state.agent_for_goal(id).ok());
                let workspace_root = state.config.workspace_root.clone();
                let use_keychain = state.config.credential_vault_use_keychain;
                // Release the gateway-wide lock before dispatching: a scope
                // deficit now escalates through ta_human_verify's synthetic
                // pipeline and, on anything but an auto-confirmed Commit,
                // blocks on a real human response (up to its configured
                // timeout) -- holding this mutex for that long would stall
                // every other in-flight gateway call (v0.17.6.6).
                drop(state);
                let dispatch_result = dispatch_auto_action(
                    state_arc,
                    &workspace_root,
                    &params,
                    action_impl,
                    requesting_agent_id.as_deref(),
                    use_keychain,
                );
                state = state_arc
                    .lock()
                    .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
                dispatch_result?
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
        effective_policy.clone(),
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

/// Dispatch an `ActionPolicy::Auto` action through the risk/budget/security
/// gate (v0.17.5.3 item 3): a plugin-computed risk score is scored via the
/// same `ta_decision::gate::decide()` used by code drafts and
/// `ta_human_verify` — one uniform `Commit`/`Reject`/`Rework`/`Escalate`
/// contract regardless of domain — a business-metric budget guardrail
/// (v0.17.5.2, item 4) is consulted when the caller supplies one, and
/// `security_tier` gates whether the gate's own verdict is even allowed to
/// auto-commit (item 3's "consults `security_tier` directly" requirement):
/// this mirrors `check_advisor_auto_approve`'s Auto-only rule as this
/// path's own live caller, rather than leaving that logic unwired like the
/// real draft-apply path does today.
fn dispatch_auto_action(
    state: &Arc<Mutex<GatewayState>>,
    workspace_root: &Path,
    params: &ExternalActionParams,
    action_impl: &dyn ta_actions::ExternalAction,
    requesting_agent_id: Option<&str>,
    credential_vault_use_keychain: bool,
) -> Result<(ActionOutcome, Option<PendingAction>), McpError> {
    // The real, subprocess-backed synthetic pipeline (v0.17.6.6) -- only
    // constructed lazily inside the ScopeDeficit branch below, since most
    // dispatches never hit a scope deficit at all.
    let ta_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ta"));
    let recent_misses = crate::verify_audit::load_recent_misses(
        workspace_root,
        CREDENTIAL_SCOPE_ELEVATION_WORKLOAD_TYPE,
        5,
    );
    let pipeline =
        HeadlessSyntheticPipeline::new(workspace_root.to_path_buf(), ta_bin, recent_misses);

    dispatch_auto_action_with_pipeline(
        state,
        workspace_root,
        params,
        action_impl,
        requesting_agent_id,
        credential_vault_use_keychain,
        &pipeline,
    )
}

/// Same as `dispatch_auto_action`, but takes an explicit `pipeline` for the
/// credential-scope-elevation escalation (mirrors
/// `human_verify::handle_human_verify_with_pipeline`'s test seam) so tests
/// can exercise the elevation flow deterministically without spawning a
/// real subprocess.
fn dispatch_auto_action_with_pipeline(
    state: &Arc<Mutex<GatewayState>>,
    workspace_root: &Path,
    params: &ExternalActionParams,
    action_impl: &dyn ta_actions::ExternalAction,
    requesting_agent_id: Option<&str>,
    credential_vault_use_keychain: bool,
    pipeline: &dyn SyntheticPipeline,
) -> Result<(ActionOutcome, Option<PendingAction>), McpError> {
    // Business-metric budget hard-limit pre-gate — deterministic, runs
    // before any risk scoring (mirrors `ta_human_verify`'s ordering: a
    // hard limit is a workflow-declared rule with no confidence override).
    let mut budget_soft_limit_reason: Option<String> = None;
    if let Some(budget_params) = &params.budget {
        validate_ledger_path(&budget_params.ledger_path)?;
        let guardrails = budget_params.guardrails.clone().into();
        let ledger_path = workspace_root.join(&budget_params.ledger_path);
        let ledger_total_before = business_budget::ledger_running_total(&ledger_path);
        match business_budget::check_budget(
            &guardrails,
            ledger_total_before,
            budget_params.action_amount,
        ) {
            BudgetCheckResult::HardLimitExceeded(reason) => {
                tracing::warn!(
                    action_type = %params.action_type,
                    amount = budget_params.action_amount,
                    reason = %reason,
                    "ta_external_action: business-metric budget hard limit exceeded, \
                     blocking before risk scoring"
                );
                return Ok((ActionOutcome::Blocked { reason }, None));
            }
            BudgetCheckResult::SoftLimitCrossed(reason) => {
                budget_soft_limit_reason = Some(reason);
            }
            BudgetCheckResult::Ok => {}
        }
    }

    // Gateway live interception / secret-substitution broker (v0.17.6.3):
    // deterministic pre-gate, same "runs before any risk scoring" ordering
    // as the budget hard limit above — a connector authorization decision
    // is a mechanical scope comparison, not something an LLM judgment call
    // (the risk gate below) should be able to override.
    let secret_for_execute = match resolve_connector_authorization(
        workspace_root,
        params,
        requesting_agent_id,
        credential_vault_use_keychain,
    ) {
        ConnectorAuthorization::Error(e) => return Err(e),
        ConnectorAuthorization::ScopeDeficit {
            connector_id,
            credential_id,
            agent_id,
            requested_scope,
            current_caveats,
            parent_goal_scope,
            secret,
            description,
        } => {
            tracing::info!(
                connector = %connector_id,
                requested_scope = %requested_scope,
                "ta_external_action: scope deficit detected, escalating via ta_human_verify \
                 credential-scope-elevation pipeline (v0.17.6.6) instead of a hard failure"
            );

            let request = ScopeElevationRequest {
                connector_id,
                credential_id,
                agent_id,
                requested_scope,
                current_caveats,
                target_uri: params.target_uri.clone(),
                goal_id: requesting_agent_id.map(|s| s.to_string()),
                parent_goal_scope,
            };

            let elevation_result = handle_credential_scope_elevation(
                state,
                request,
                credential_vault_use_keychain,
                pipeline,
            )?;

            let auto_confirmed = elevation_result
                .content
                .first()
                .and_then(|c| match &c.raw {
                    RawContent::Text(t) => serde_json::from_str::<serde_json::Value>(&t.text).ok(),
                    _ => None,
                })
                .and_then(|v| v.get("auto_confirmed").and_then(|b| b.as_bool()))
                .unwrap_or(false);

            if auto_confirmed {
                Some(secret)
            } else {
                let action_id = Uuid::new_v4();
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
                return Ok((ActionOutcome::CapturedForReview, Some(pending)));
            }
        }
        ConnectorAuthorization::NotBrokered => None,
        ConnectorAuthorization::Authorized { secret } => Some(secret),
    };

    let RiskAssessment {
        risk_score,
        confidence,
    } = action_impl.risk_score(&params.payload).map_err(|e| {
        McpError::internal_error(
            format!(
                "risk scoring failed for action '{}': {e}",
                params.action_type
            ),
            None,
        )
    })?;

    let thresholds = load_thresholds(workspace_root, &params.action_type);
    let input = DecisionInput {
        verdict: Verdict::Pass,
        risk_score,
        confidence,
    };
    let mut decision = decide(&input, &thresholds);

    if let Some(reason) = &budget_soft_limit_reason {
        if decision != Decision::Escalate {
            tracing::info!(
                action_type = %params.action_type,
                reason = %reason,
                original_decision = ?decision,
                "ta_external_action: forcing Escalate — business-metric budget soft \
                 threshold crossed"
            );
        }
        decision = Decision::Escalate;
    }

    // security_tier only ever downgrades autonomy, never upgrades it — same
    // rule `ta-brain::route()`'s confidence-downgrade uses. An
    // inferred/non-autonomous workload never gets to auto-commit an
    // external side effect, no matter how low-risk the gate scored it.
    let (_, security_tier) = resolve_workload_context(workspace_root);
    if security_tier != AdvisorSecurity::Auto && decision == Decision::Commit {
        tracing::info!(
            action_type = %params.action_type,
            security_tier = %security_tier,
            "ta_external_action: security_tier != auto, downgrading Commit to Escalate"
        );
        decision = Decision::Escalate;
    }

    tracing::info!(
        action_type = %params.action_type,
        decision = ?decision,
        risk_score,
        confidence,
        "ta_external_action: gate decision for auto-policy dispatch"
    );

    if !decision.is_auto_approvable() {
        let action_id = Uuid::new_v4();
        let description = format!(
            "{} [gate: {decision:?}, risk_score={risk_score}, confidence={:.0}%]",
            build_description(params),
            confidence * 100.0,
        );
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
        return Ok((ActionOutcome::CapturedForReview, Some(pending)));
    }

    match action_impl.execute_with_secret(&params.payload, secret_for_execute.as_deref()) {
        Ok(result) => {
            if let Some(budget_params) = &params.budget {
                let ledger_path = workspace_root.join(&budget_params.ledger_path);
                if let Err(e) = business_budget::record_ledger_spend(
                    &ledger_path,
                    &budget_params.action_label,
                    budget_params.action_amount,
                ) {
                    tracing::warn!(
                        path = %ledger_path.display(),
                        error = %e,
                        "ta_external_action: failed to record business-metric budget \
                         ledger entry"
                    );
                }
            }
            Ok((ActionOutcome::Executed { result }, None))
        }
        // Built-in stubs have no risk-scoring gate to fail (default is
        // zero-risk/full-confidence, always `Commit`) so this is only
        // reachable for the four built-ins, never a plugin-backed verb.
        Err(ta_actions::ActionError::StubOnly(_)) => {
            let result = serde_json::json!({
                "status": "stub_executed",
                "message": format!(
                    "Action type '{}' has no registered plugin executor. \
                     Register a plugin via the ActionRegistry to provide \
                     real execution. The action has been logged.",
                    params.action_type
                )
            });
            Ok((ActionOutcome::Executed { result }, None))
        }
        Err(e) => Err(McpError::internal_error(
            format!("action execution failed: {}", e),
            None,
        )),
    }
}

// ── Gateway live interception / secret-substitution broker (v0.17.6.3) ───────

/// Outcome of authorizing `params.connector` against the credential vault
/// and `ConnectorRegistry` before an auto-policy action is dispatched.
enum ConnectorAuthorization {
    /// No `connector` was declared, or the declared connector exists but is
    /// not `broker_mediated` — dispatch proceeds via the existing
    /// `bare_process.rs` env-injection fallback (v0.17.6.3 item 5), not the
    /// broker.
    NotBrokered,
    /// The connector is `broker_mediated`, the presented `session_token`
    /// validated, and its scope covers the connector's `required_scope`.
    /// `secret` is the resolved `Credential.secret` — attach it only to the
    /// gateway's own outbound call, never return it to the agent.
    Authorized { secret: String },
    /// The session token is valid but its `allowed_scopes` don't cover the
    /// connector's `required_scope`. Per PLAN item 4 ("hand off to
    /// v0.17.6.6's escalation path instead of a hard failure"): wired into
    /// `ta_human_verify`'s confidence-gated credential-scope-elevation
    /// pipeline (v0.17.6.6) rather than denied outright or deferred to an
    /// async review queue.
    ScopeDeficit {
        connector_id: String,
        credential_id: Uuid,
        agent_id: String,
        requested_scope: String,
        /// Scopes already present on the presented (deficient) token.
        current_caveats: Vec<String>,
        /// The full scope set declared on the underlying vault credential —
        /// the ceiling the elevation may not exceed (v0.17.6.6 item 3).
        parent_goal_scope: Vec<String>,
        /// The resolved secret, ready to attach to the gateway's own
        /// outbound call if the elevation is approved — never returned to
        /// the agent.
        secret: String,
        /// Human-readable fallback description, used only if the elevation
        /// doesn't auto-confirm and the action is captured for review.
        description: String,
    },
    /// A hard, actionable failure: unknown connector, missing/invalid/
    /// expired/mismatched session token, or an unreadable vault. Unlike a
    /// scope deficit, none of these represent "the agent knows what it's
    /// doing but needs more privilege" — they're malformed requests.
    Error(McpError),
}

/// Authorize `params.connector` for an auto-policy dispatch and, on
/// success, resolve the real secret to attach to the gateway's own
/// outbound call (v0.17.6.3 items 1/2/4).
///
/// A request that doesn't declare `connector` at all is unaffected —
/// `ConnectorAuthorization::NotBrokered` — so this is purely additive for
/// callers that don't opt into broker mediation yet.
fn resolve_connector_authorization(
    workspace_root: &Path,
    params: &ExternalActionParams,
    requesting_agent_id: Option<&str>,
    credential_vault_use_keychain: bool,
) -> ConnectorAuthorization {
    use ta_credentials::CredentialVault;

    let Some(connector_id) = params.connector.as_deref() else {
        return ConnectorAuthorization::NotBrokered;
    };

    let registry = ta_credentials::ConnectorRegistry::load(&workspace_root.join(".ta"));
    let Some(entry) = registry.get(connector_id) else {
        return ConnectorAuthorization::Error(McpError::invalid_params(
            format!(
                "unknown connector '{connector_id}' — declare it under \
                 [connectors.{connector_id}] in .ta/connectors.toml before referencing it \
                 from ta_external_action (see docs/USAGE.md 'Broker-Mediated Connectors')"
            ),
            None,
        ));
    };

    if !entry.broker_mediated {
        tracing::debug!(
            connector = connector_id,
            "connector is not broker_mediated; dispatch falls through to the \
             non-gateway-mediated reduced-security fallback (pending v0.17.6.7)"
        );
        return ConnectorAuthorization::NotBrokered;
    }

    let Some(session_token) = params.session_token.as_deref() else {
        return ConnectorAuthorization::Error(McpError::invalid_params(
            format!(
                "connector '{connector_id}' is broker_mediated — a session_token is \
                 required (the agent receives one via TA_SESSION_TOKEN_<credential> in its \
                 own environment; see docs/USAGE.md 'Broker-Mediated Connectors')"
            ),
            None,
        ));
    };
    let mut cred_config = ta_credentials::CredentialsConfig::for_project(workspace_root);
    cred_config.use_keychain = credential_vault_use_keychain;
    let Ok(vault) = ta_credentials::FileVault::open(&cred_config) else {
        return ConnectorAuthorization::Error(McpError::internal_error(
            format!(
                "connector '{connector_id}' is broker_mediated but the credential vault at \
                 {} could not be opened",
                cred_config.vault_path.display()
            ),
            None,
        ));
    };

    // v0.17.6.4: the presented `session_token` is now a biscuit grant minted
    // by `ta_credential_broker::CredentialBroker` (see `ta credentials
    // grant` / `load_vault_credentials`), not a UUID that resolves against
    // `FileVault`'s in-process token list. `CredentialBroker::verify`
    // checks the signature, the grant's embedded expiry, and the broker's
    // revocation denylist entirely offline — no shared state with whichever
    // process minted it is required.
    let broker_dir = cred_config
        .vault_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| workspace_root.join(".ta"));
    let Ok(broker) = ta_credential_broker::CredentialBroker::open(&broker_dir) else {
        return ConnectorAuthorization::Error(McpError::internal_error(
            format!(
                "connector '{connector_id}' is broker_mediated but the credential broker at \
                 {} could not be opened",
                broker_dir.display()
            ),
            None,
        ));
    };

    let grant = match broker.verify(session_token) {
        Ok(g) => g,
        Err(e) => {
            return ConnectorAuthorization::Error(McpError::invalid_params(
                format!(
                    "session_token for connector '{connector_id}' failed verification: {e} \
                     (grants expire — mint a fresh one via `ta credentials grant`)"
                ),
                None,
            ));
        }
    };

    if let Some(agent_id) = requesting_agent_id {
        if grant.agent_id != agent_id {
            return ConnectorAuthorization::Error(McpError::invalid_params(
                format!(
                    "session_token for connector '{connector_id}' was issued to a different \
                     agent ('{}') than the requesting goal's agent ('{agent_id}')",
                    grant.agent_id
                ),
                None,
            ));
        }
    }

    let credential = match vault.get(grant.credential_id) {
        Ok(c) => c,
        Err(e) => {
            return ConnectorAuthorization::Error(McpError::internal_error(
                format!("failed to resolve credential for connector '{connector_id}': {e}"),
                None,
            ));
        }
    };
    if credential.name != entry.credential_name {
        return ConnectorAuthorization::Error(McpError::invalid_params(
            format!(
                "session_token for connector '{connector_id}' does not back the credential \
                 that connector declares ('{}' expected, token backs '{}') — this connector's \
                 session_token cannot be used for a different connector's credential",
                entry.credential_name, credential.name
            ),
            None,
        ));
    }

    if let Some(required_scope) = &entry.required_scope {
        if !grant.allowed_scopes.iter().any(|s| s == required_scope) {
            return ConnectorAuthorization::ScopeDeficit {
                connector_id: connector_id.to_string(),
                credential_id: credential.id,
                agent_id: grant.agent_id.clone(),
                requested_scope: required_scope.clone(),
                current_caveats: grant.allowed_scopes.clone(),
                parent_goal_scope: credential.scopes.clone(),
                description: format!(
                    "ta_external_action:{} via connector '{connector_id}' requires scope \
                     '{required_scope}', but the presented session token only allows {:?} — \
                     escalated for credential scope elevation via ta_human_verify (v0.17.6.6); \
                     a reviewer can also re-grant a wider-scoped token via \
                     `ta credentials grant`.",
                    params.action_type, grant.allowed_scopes
                ),
                secret: credential.secret,
            };
        }
    }

    ConnectorAuthorization::Authorized {
        secret: credential.secret,
    }
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
    // Show the effective policy (may differ from policy_config.policy for email).
    let effective_policy_str = match outcome {
        ActionOutcome::CapturedForReview => "review".to_string(),
        _ => policy_config.policy.to_string(),
    };
    let base = serde_json::json!({
        "action_type": action_type,
        "dry_run": dry_run,
        "policy": effective_policy_str,
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
    use crate::tools::human_verify::{OpinionResult, ValidatorResult};

    fn make_state(root: &std::path::Path) -> Arc<Mutex<GatewayState>> {
        let mut config = GatewayConfig::for_project(root);
        // Force file-based key custody for the credential vault: the OS
        // keychain is a process/OS-global resource (see
        // `ta_credentials::CredentialsConfig::use_keychain`'s own doc
        // comment) that must not leak between tests or interfere with the
        // developer's real keychain.
        config.credential_vault_use_keychain = false;
        let state = GatewayState::new(config).expect("state init failed");
        Arc::new(Mutex::new(state))
    }

    /// Logs a `RoutingDecision` with the given `security_tier`, the same
    /// `.ta/routing-decisions.jsonl` fixture `human_verify.rs`'s tests use —
    /// `dispatch_auto_action` resolves `security_tier` from this same log.
    fn write_routing_decision(workspace_root: &std::path::Path, security_tier: &str) {
        let log_path = workspace_root.join(".ta").join("routing-decisions.jsonl");
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        let line = serde_json::json!({
            "timestamp": "2026-07-10T00:00:00Z",
            "goal_title": "test goal",
            "decision": {
                "team": "implementer",
                "agent": "claude-code",
                "security_tier": security_tier,
                "priority": "normal",
                "workload_type": "general",
                "workload_confidence": 0.9,
                "rationale": ["test"],
            }
        });
        std::fs::write(&log_path, format!("{}\n", line)).unwrap();
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
            budget: None,
            connector: None,
            session_token: None,
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
            budget: None,
            connector: None,
            session_token: None,
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
            budget: None,
            connector: None,
            session_token: None,
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
            budget: None,
            connector: None,
            session_token: None,
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
            budget: None,
            connector: None,
            session_token: None,
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
            budget: None,
            connector: None,
            session_token: None,
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

    /// With `security_tier: auto` logged, the gate's default zero-risk
    /// score commits and the built-in stub's placeholder executes — same
    /// observable behavior as before v0.17.5.3's gate existed.
    #[test]
    fn auto_policy_stub_returns_stub_executed_when_security_tier_is_auto() {
        let dir = tempdir().unwrap();

        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.api_call]\npolicy = \"auto\"\n",
        )
        .unwrap();
        write_routing_decision(dir.path(), "auto");

        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "api_call".into(),
            payload: json!({"method": "GET", "url": "https://api.example.com/status"}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: None,
            session_token: None,
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

    /// Without a logged `security_tier: auto` routing decision (the
    /// unclassified-workload default, `Suggest`), `dispatch_auto_action`
    /// never lets the gate's `Commit` verdict auto-execute — even a
    /// zero-risk built-in stub is captured for review instead (v0.17.5.3
    /// item 3: security_tier only ever downgrades autonomy).
    #[test]
    fn auto_policy_without_auto_security_tier_is_captured_for_review() {
        let dir = tempdir().unwrap();

        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.api_call]\npolicy = \"auto\"\n",
        )
        .unwrap();
        // No routing decision logged -- resolve_workload_context defaults
        // to Suggest, not Auto.

        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "api_call".into(),
            payload: json!({"method": "GET", "url": "https://api.example.com/status"}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: None,
            session_token: None,
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(
            text.contains("captured_for_review"),
            "expected captured_for_review outcome: {}",
            text
        );
    }

    // ── v0.17.5.3: adapter plugin end-to-end gate round-trip ──────────────

    /// Writes a mock `trade.execute` adapter plugin under
    /// `.ta/plugins/adapter/paper-trading/` whose risk score scales with the
    /// trade's dollar `amount` -- a small Python script, no live brokerage
    /// dependency (item 5/7's "no live external API dependency in TA's own
    /// test suite" requirement).
    fn write_paper_trading_plugin(workspace_root: &std::path::Path) {
        let plugin_dir = workspace_root
            .join(".ta")
            .join("plugins")
            .join("adapter")
            .join("paper-trading");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let script_path = plugin_dir.join("mock_plugin.py");
        std::fs::write(
            &script_path,
            r#"
import json
import sys

req = json.loads(sys.stdin.readline())
method = req["method"]
payload = req["params"]["payload"]
amount = payload.get("amount", 0)

if method == "risk_score":
    # Real, non-hardcoded score: scales with trade notional size.
    result = {"risk_score": min(100, int(amount / 10)), "confidence": 0.95}
    print(json.dumps({"ok": True, "result": result}))
elif method == "execute":
    result = {"status": "filled", "amount": amount}
    print(json.dumps({"ok": True, "result": result}))
else:
    print(json.dumps({"ok": False, "error": f"unknown method {method}"}))
"#,
        )
        .unwrap();
        // Serialize via toml::Table rather than hand-formatting the string:
        // `script_path.display()` on Windows renders backslashes
        // (`C:\Users\...`), and TOML string literals treat `\` as an
        // escape-sequence introducer — `format!`-ing it in raw produced
        // "invalid unicode 8-digit hex code" parse errors in Windows CI.
        // toml::Table's Serialize impl escapes the path correctly for
        // every platform.
        let mut manifest = toml::Table::new();
        manifest.insert("name".into(), "paper-trading".into());
        manifest.insert("type".into(), "adapter".into());
        manifest.insert("command".into(), "python3".into());
        manifest.insert(
            "args".into(),
            toml::Value::Array(vec![script_path.to_string_lossy().to_string().into()]),
        );
        manifest.insert(
            "capabilities".into(),
            toml::Value::Array(vec!["verb:trade.execute".into()]),
        );
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn adapter_plugin_low_risk_trade_auto_executes() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            b"[actions.\"trade.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        write_paper_trading_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "trade.execute".into(),
            // amount=100 -> risk_score=10, well under the default
            // max_risk_score=40 threshold -> Commit.
            payload: json!({"symbol": "ACME", "amount": 100}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: None,
            session_token: None,
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(
            text.contains("executed") && !text.contains("captured_for_review"),
            "expected executed outcome: {}",
            text
        );
        assert!(
            text.contains("filled"),
            "expected the plugin's real execute() result: {}",
            text
        );
    }

    #[test]
    fn adapter_plugin_high_risk_trade_is_captured_for_review_not_executed() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            b"[actions.\"trade.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        write_paper_trading_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "trade.execute".into(),
            // amount=1000 -> risk_score=100, well over max_risk_score=40
            // and escalate_risk_score=75 -> Escalate, never executed.
            payload: json!({"symbol": "ACME", "amount": 1000}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: None,
            session_token: None,
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(
            text.contains("captured_for_review"),
            "expected captured_for_review outcome, not an executed trade: {}",
            text
        );
        assert!(
            !text.contains("filled"),
            "the risky trade must not have actually executed: {}",
            text
        );
    }

    #[test]
    fn unregistered_verb_returns_clear_no_adapter_error() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path());

        let params = ExternalActionParams {
            action_type: "trade.execute".into(),
            payload: json!({"symbol": "ACME", "amount": 100}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: None,
            session_token: None,
        };

        // No plugin registered under .ta/plugins/adapter/ -- must be a
        // clear, actionable error, not a silent no-op.
        let err = handle_external_action(&state, params).unwrap_err();
        let message = err.message.to_string();
        assert!(
            message.contains("no adapter registered for this verb"),
            "expected a clear 'no adapter registered' error, got: {}",
            message
        );
    }

    // ── v0.17.6.3: gateway live interception / secret-substitution broker ──

    /// A `connector.execute` adapter plugin whose `execute` method reports
    /// whether it saw a secret via `TA_CONNECTOR_SECRET` -- never echoing
    /// the secret itself back in its result, matching how a real connector
    /// plugin (e.g. calling GitHub with the resolved token) would behave.
    fn write_broker_test_plugin(workspace_root: &std::path::Path) {
        let plugin_dir = workspace_root
            .join(".ta")
            .join("plugins")
            .join("adapter")
            .join("broker-test");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let script_path = plugin_dir.join("mock_plugin.py");
        std::fs::write(
            &script_path,
            r#"
import json
import os
import sys

req = json.loads(sys.stdin.readline())
method = req["method"]

if method == "risk_score":
    print(json.dumps({"ok": True, "result": {"risk_score": 0, "confidence": 1.0}}))
elif method == "execute":
    secret = os.environ.get("TA_CONNECTOR_SECRET", "")
    result = {"saw_secret": len(secret) > 0, "secret_len": len(secret)}
    print(json.dumps({"ok": True, "result": result}))
else:
    print(json.dumps({"ok": False, "error": f"unknown method {method}"}))
"#,
        )
        .unwrap();
        let mut manifest = toml::Table::new();
        manifest.insert("name".into(), "broker-test".into());
        manifest.insert("type".into(), "adapter".into());
        manifest.insert("command".into(), "python3".into());
        manifest.insert(
            "args".into(),
            toml::Value::Array(vec![script_path.to_string_lossy().to_string().into()]),
        );
        manifest.insert(
            "capabilities".into(),
            toml::Value::Array(vec!["verb:connector.execute".into()]),
        );
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    /// Opens (creating if needed) a file-key-custody vault for a test
    /// workspace -- `use_keychain: false` keeps tests off the real, process-
    /// global OS keychain.
    fn open_test_vault(workspace_root: &std::path::Path) -> ta_credentials::FileVault {
        let mut cred_config = ta_credentials::CredentialsConfig::for_project(workspace_root);
        cred_config.use_keychain = false;
        ta_credentials::FileVault::open(&cred_config).unwrap()
    }

    #[test]
    fn broker_mediated_connector_never_returns_raw_secret_to_agent() {
        use ta_credentials::CredentialVault;

        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.\"connector.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        std::fs::write(
            ta_dir.join("connectors.toml"),
            b"[connectors.paper-trading]\n\
              credential_name = \"PAPER_API_KEY\"\n\
              broker_mediated = true\n\
              required_scope = \"trade.write\"\n",
        )
        .unwrap();
        write_broker_test_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let mut vault = open_test_vault(dir.path());
        let cred = vault
            .add(
                "PAPER_API_KEY",
                "paper-trading",
                "sk-live-supersecret-do-not-leak",
                vec!["trade.write".into()],
            )
            .unwrap();
        let broker = ta_credential_broker::CredentialBroker::open(&ta_dir).unwrap();
        let granted = broker
            .grant(cred.id, "test-agent", vec!["trade.write".into()], 3600)
            .unwrap();

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "connector.execute".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: Some("paper-trading".into()),
            session_token: Some(granted.token),
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false));
        let text = serde_json::to_string(&result.content[0]).unwrap();

        assert!(
            !text.contains("sk-live-supersecret-do-not-leak"),
            "the raw secret must never appear in the agent-visible response: {}",
            text
        );
        assert!(
            text.contains("executed") && text.contains(r#"\"saw_secret\":true"#),
            "expected the plugin to have received the secret server-side and executed: {}",
            text
        );

        // The action log (which the agent can also read back via other
        // tools) must not carry the raw secret either -- only the request
        // payload TA actually captured, which never contained it.
        let log = std::fs::read_to_string(ta_dir.join("action-log.jsonl")).unwrap();
        assert!(!log.contains("sk-live-supersecret-do-not-leak"));
    }

    #[test]
    fn broker_mediated_connector_with_missing_token_is_a_clear_actionable_error() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.\"connector.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        std::fs::write(
            ta_dir.join("connectors.toml"),
            b"[connectors.paper-trading]\n\
              credential_name = \"PAPER_API_KEY\"\n\
              broker_mediated = true\n",
        )
        .unwrap();
        write_broker_test_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "connector.execute".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: Some("paper-trading".into()),
            session_token: None,
        };

        let err = handle_external_action(&state, params).unwrap_err();
        assert!(
            err.message
                .to_string()
                .contains("session_token is required"),
            "expected an actionable 'session_token is required' error: {}",
            err.message
        );
    }

    #[test]
    fn broker_mediated_connector_with_malformed_token_is_a_clear_actionable_error() {
        // v0.17.6.4: `session_token` is now a biscuit grant, not a UUID —
        // a garbage string must fail broker verification with an
        // actionable message, not panic on `Uuid::parse_str`.
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.\"connector.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        std::fs::write(
            ta_dir.join("connectors.toml"),
            b"[connectors.paper-trading]\n\
              credential_name = \"PAPER_API_KEY\"\n\
              broker_mediated = true\n",
        )
        .unwrap();
        write_broker_test_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "connector.execute".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: Some("paper-trading".into()),
            session_token: Some("not-a-real-uuid-or-biscuit".into()),
        };

        let err = handle_external_action(&state, params).unwrap_err();
        assert!(
            err.message.to_string().contains("failed verification"),
            "expected an actionable verification-failure error: {}",
            err.message
        );
    }

    #[test]
    fn broker_mediated_connector_with_revoked_grant_is_a_clear_actionable_error() {
        use ta_credentials::CredentialVault;

        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.\"connector.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        std::fs::write(
            ta_dir.join("connectors.toml"),
            b"[connectors.paper-trading]\n\
              credential_name = \"PAPER_API_KEY\"\n\
              broker_mediated = true\n",
        )
        .unwrap();
        write_broker_test_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let mut vault = open_test_vault(dir.path());
        let cred = vault
            .add(
                "PAPER_API_KEY",
                "paper-trading",
                "sk-live-supersecret-do-not-leak",
                vec![],
            )
            .unwrap();
        let mut broker = ta_credential_broker::CredentialBroker::open(&ta_dir).unwrap();
        let granted = broker.grant(cred.id, "test-agent", vec![], 3600).unwrap();
        broker.revoke(&granted.token_id).unwrap();

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "connector.execute".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: Some("paper-trading".into()),
            session_token: Some(granted.token),
        };

        let err = handle_external_action(&state, params).unwrap_err();
        assert!(
            err.message.to_string().contains("failed verification"),
            "expected an actionable verification-failure error for a revoked grant: {}",
            err.message
        );
    }

    /// A deterministic `SyntheticPipeline` double for scope-elevation tests
    /// (v0.17.6.6) -- mirrors `human_verify.rs`'s own `FakePipeline`, kept
    /// as a local copy since that one lives in a private test module and
    /// isn't reachable from here.
    struct FakeElevationPipeline {
        opinion: OpinionResult,
        validator: ValidatorResult,
    }

    impl SyntheticPipeline for FakeElevationPipeline {
        fn opinion(&self, _q: &str, _c: Option<&str>) -> Result<OpinionResult, String> {
            Ok(self.opinion.clone())
        }
        fn validate(
            &self,
            _q: &str,
            _c: Option<&str>,
            _o: &OpinionResult,
        ) -> Result<ValidatorResult, String> {
            Ok(self.validator.clone())
        }
    }

    /// Sets up a credential whose grant only covers `trade.read`, while the
    /// connector requires `trade.write` -- a scope deficit that the
    /// elevation gate (v0.17.6.6) must handle, not a hard denial. Returns
    /// the granted biscuit token (v0.17.6.4: grants are minted by the
    /// broker, not `vault.issue_token()`).
    fn setup_scope_deficit_fixture(dir: &std::path::Path) -> String {
        use ta_credentials::CredentialVault;

        let ta_dir = dir.join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.\"connector.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        std::fs::write(
            ta_dir.join("connectors.toml"),
            b"[connectors.paper-trading]\n\
              credential_name = \"PAPER_API_KEY\"\n\
              broker_mediated = true\n\
              required_scope = \"trade.write\"\n",
        )
        .unwrap();
        write_broker_test_plugin(dir);
        write_routing_decision(dir, "auto");

        let mut vault = open_test_vault(dir);
        // Credential/grant exist, but the grant was only issued a narrower
        // scope than the connector requires.
        let cred = vault
            .add(
                "PAPER_API_KEY",
                "paper-trading",
                "sk-live-supersecret-do-not-leak",
                vec!["trade.read".into(), "trade.write".into()],
            )
            .unwrap();
        let broker = ta_credential_broker::CredentialBroker::open(&ta_dir).unwrap();
        let granted = broker
            .grant(cred.id, "test-agent", vec!["trade.read".into()], 3600)
            .unwrap();
        granted.token
    }

    fn scope_deficit_params(token: String) -> ExternalActionParams {
        ExternalActionParams {
            action_type: "connector.execute".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: Some("paper-trading".into()),
            session_token: Some(token),
        }
    }

    /// Registry with the broker-test plugin (written by
    /// `write_broker_test_plugin`) discovered and registered.
    fn test_registry(workspace_root: &std::path::Path) -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        for action in discover_adapter_actions(workspace_root) {
            registry.register(action);
        }
        registry
    }

    /// Answers the next pending `ta_ask_human` interaction with `answer_text`
    /// (mirrors `human_verify.rs`'s own private `spawn_answer_thread`, kept
    /// as a local copy since that one isn't reachable from here).
    fn spawn_answer_thread(dir: &std::path::Path, answer_text: &str) {
        let pending_dir = dir.join(".ta").join("interactions").join("pending");
        let answers_dir = dir.join(".ta").join("interactions").join("answers");
        let answer_text = answer_text.to_string();
        std::thread::spawn(move || {
            for _ in 0..50 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if let Ok(entries) = std::fs::read_dir(&pending_dir) {
                    let files: Vec<_> = entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                        .collect();
                    if let Some(entry) = files.first() {
                        let stem = entry
                            .path()
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .to_string();
                        std::fs::create_dir_all(&answers_dir).unwrap();
                        let answer_path = answers_dir.join(format!("{}.json", stem));
                        let answer = json!({
                            "text": answer_text,
                            "responder_id": "test-human",
                            "answered_at": chrono::Utc::now().to_rfc3339(),
                        });
                        std::fs::write(&answer_path, serde_json::to_string(&answer).unwrap())
                            .unwrap();
                        return;
                    }
                }
            }
        });
    }

    #[test]
    fn broker_mediated_connector_scope_deficit_auto_commits_mints_token_and_executes() {
        let dir = tempdir().unwrap();
        let token = setup_scope_deficit_fixture(dir.path());
        let state = make_state(dir.path());
        let registry = test_registry(dir.path());
        let action_impl = registry.get("connector.execute").unwrap();

        let pipeline = FakeElevationPipeline {
            opinion: OpinionResult {
                answer: "Approve".to_string(),
                reasoning: "Within parent scope, low risk trade action.".to_string(),
                confidence: 0.99,
            },
            validator: ValidatorResult {
                verdict: Verdict::Pass,
                risk_score: 1,
                confidence: 0.99,
                reasoning: "Contained within parent scope.".to_string(),
            },
        };

        let (outcome, pending) = dispatch_auto_action_with_pipeline(
            &state,
            dir.path(),
            &scope_deficit_params(token),
            action_impl,
            Some("test-agent"),
            false,
            &pipeline,
        )
        .unwrap();

        assert!(
            pending.is_none(),
            "an auto-confirmed elevation must execute, not be captured"
        );
        let ActionOutcome::Executed { result } = outcome else {
            panic!(
                "expected Executed after an auto-confirmed elevation, got: {:?}",
                outcome
            );
        };
        assert!(
            result["saw_secret"] == json!(true),
            "the plugin should have received the resolved secret server-side: {:?}",
            result
        );

        let audit_path = dir.path().join(".ta").join("human-verify-audit.jsonl");
        let audit = std::fs::read_to_string(&audit_path).unwrap();
        assert!(
            audit.contains("\"granted_scope\":[\"trade.write\"]"),
            "got: {}",
            audit
        );
    }

    #[test]
    fn broker_mediated_connector_scope_deficit_non_commit_is_captured_for_review_not_denied() {
        let dir = tempdir().unwrap();
        let token = setup_scope_deficit_fixture(dir.path());
        let state = make_state(dir.path());
        let registry = test_registry(dir.path());
        let action_impl = registry.get("connector.execute").unwrap();

        // Low confidence -> the gate escalates rather than commits.
        let pipeline = FakeElevationPipeline {
            opinion: OpinionResult {
                answer: "Unsure".to_string(),
                reasoning: "Ambiguous signal.".to_string(),
                confidence: 0.3,
            },
            validator: ValidatorResult {
                verdict: Verdict::Warn,
                risk_score: 20,
                confidence: 0.3,
                reasoning: "Not confident in this elevation.".to_string(),
            },
        };

        spawn_answer_thread(dir.path(), "deny");

        let (outcome, pending) = dispatch_auto_action_with_pipeline(
            &state,
            dir.path(),
            &scope_deficit_params(token),
            action_impl,
            Some("test-agent"),
            false,
            &pipeline,
        )
        .unwrap();

        assert!(
            matches!(outcome, ActionOutcome::CapturedForReview),
            "a non-committed elevation must be captured for review, not denied: {:?}",
            outcome
        );
        assert!(pending.is_some());

        let audit_path = dir.path().join(".ta").join("human-verify-audit.jsonl");
        assert!(
            !audit_path.exists(),
            "a non-committed elevation must never mint or audit a grant"
        );
    }

    #[test]
    fn broker_mediated_connector_scope_deficit_never_leaks_secret_when_not_committed() {
        let dir = tempdir().unwrap();
        let token = setup_scope_deficit_fixture(dir.path());
        let state = make_state(dir.path());
        let registry = test_registry(dir.path());
        let action_impl = registry.get("connector.execute").unwrap();

        spawn_answer_thread(dir.path(), "deny");
        let pipeline = FakeElevationPipeline {
            opinion: OpinionResult {
                answer: "Unsure".to_string(),
                reasoning: "Ambiguous.".to_string(),
                confidence: 0.3,
            },
            validator: ValidatorResult {
                verdict: Verdict::Warn,
                risk_score: 20,
                confidence: 0.3,
                reasoning: "Not confident.".to_string(),
            },
        };

        let (outcome, pending) = dispatch_auto_action_with_pipeline(
            &state,
            dir.path(),
            &scope_deficit_params(token),
            action_impl,
            Some("test-agent"),
            false,
            &pipeline,
        )
        .unwrap();

        assert!(matches!(outcome, ActionOutcome::CapturedForReview));
        let pending = pending.unwrap();
        let serialized = serde_json::to_string(&pending).unwrap();
        assert!(!serialized.contains("sk-live-supersecret-do-not-leak"));
    }

    #[test]
    fn broker_mediated_connector_scope_deficit_forces_block_when_scope_exceeds_parent() {
        // item 6: a scope-exceeding request that violates parent-containment
        // is force-blocked even when the opinion/validator LLM signals a
        // favorable verdict.
        let dir = tempdir().unwrap();
        let token = setup_scope_deficit_fixture(dir.path());
        let state = make_state(dir.path());
        let registry = test_registry(dir.path());
        let action_impl = registry.get("connector.execute").unwrap();

        // The fixture's credential only declares [trade.read, trade.write]
        // (parent_goal_scope). Point the connector's required_scope at a
        // scope the credential never declared at all, so the elevation
        // request itself is unsatisfiable regardless of what the
        // opinion/validator pipeline concludes -- exercising the
        // non-bypassable pre-check (item 3).
        let mut params = scope_deficit_params(token);
        params.connector = Some("paper-trading".into());
        std::fs::write(
            dir.path().join(".ta").join("connectors.toml"),
            b"[connectors.paper-trading]\n\
              credential_name = \"PAPER_API_KEY\"\n\
              broker_mediated = true\n\
              required_scope = \"trade.admin\"\n",
        )
        .unwrap();

        struct FavorableOpinionOnlyPipeline;
        impl SyntheticPipeline for FavorableOpinionOnlyPipeline {
            fn opinion(&self, _q: &str, _c: Option<&str>) -> Result<OpinionResult, String> {
                Ok(OpinionResult {
                    answer: "Approve".to_string(),
                    reasoning: "Looks fine to me.".to_string(),
                    confidence: 0.99,
                })
            }
            fn validate(
                &self,
                _q: &str,
                _c: Option<&str>,
                _o: &OpinionResult,
            ) -> Result<ValidatorResult, String> {
                panic!(
                    "the validator's LLM critique must never run once the parent-containment \
                     pre-check has already failed"
                );
            }
        }

        spawn_answer_thread(dir.path(), "deny");
        let (outcome, pending) = dispatch_auto_action_with_pipeline(
            &state,
            dir.path(),
            &params,
            action_impl,
            Some("test-agent"),
            false,
            &FavorableOpinionOnlyPipeline,
        )
        .unwrap();

        assert!(
            matches!(outcome, ActionOutcome::CapturedForReview),
            "a force-blocked (out-of-parent-scope) elevation must never execute: {:?}",
            outcome
        );
        assert!(pending.is_some());

        let audit_path = dir.path().join(".ta").join("human-verify-audit.jsonl");
        assert!(
            !audit_path.exists(),
            "a force-blocked elevation must never mint a token"
        );
    }

    #[test]
    fn undeclared_connector_returns_clear_actionable_error() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.\"connector.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        write_broker_test_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "connector.execute".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: Some("never-declared".into()),
            session_token: None,
        };

        let err = handle_external_action(&state, params).unwrap_err();
        assert!(
            err.message.to_string().contains("unknown connector"),
            "expected an actionable 'unknown connector' error: {}",
            err.message
        );
    }

    #[test]
    fn non_broker_mediated_connector_falls_through_to_existing_dispatch_unchanged() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            b"[actions.\"connector.execute\"]\npolicy = \"auto\"\n",
        )
        .unwrap();
        // Declared, but not broker_mediated -- the reduced-security
        // fallback (item 5): no session_token required, dispatch proceeds
        // exactly as it did before this connector existed.
        std::fs::write(
            ta_dir.join("connectors.toml"),
            b"[connectors.slack-ops]\ncredential_name = \"SLACK_BOT_TOKEN\"\n",
        )
        .unwrap();
        write_broker_test_plugin(dir.path());
        write_routing_decision(dir.path(), "auto");

        let state = make_state(dir.path());
        let params = ExternalActionParams {
            action_type: "connector.execute".into(),
            payload: json!({}),
            goal_run_id: None,
            target_uri: None,
            dry_run: false,
            budget: None,
            connector: Some("slack-ops".into()),
            session_token: None,
        };

        let result = handle_external_action(&state, params).unwrap();
        assert!(!result.is_error.unwrap_or(false), "must not silently fail");
        let text = serde_json::to_string(&result.content[0]).unwrap();
        assert!(
            text.contains("executed") && text.contains(r#"\"saw_secret\":false"#),
            "expected a normal, secret-less execution: {}",
            text
        );
    }
}
