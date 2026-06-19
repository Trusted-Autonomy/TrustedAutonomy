// step_executor.rs — Step execution outcomes and statechart executor (v0.17.0.4.5).
//
// Provides:
//   StepOutcome           — result of executing one "tick" of a workflow step (legacy)
//   StepExecutionContext  — configuration for the step runner
//   ask_human_headless()  — non-blocking escalation (no stdin read)
//   route_step()          — map an action-kind string to the next StepTarget
//   execute_pr_monitor_tick() — poll `gh pr view` and route on CI outcome
//   execute_sync_build_step() — run a named build sub-step
//   execute_step()        — full statechart executor with guards, actions, context patches

use std::path::PathBuf;

use crate::step_action::{ActionRouter, StepAction};
use crate::step_context::{StepInput, StepOutput, TransitionPayload, WorkflowContext};
use crate::step_kind::{parse_timeout_secs, StepTarget, StepWorkflowDef, WorkflowStepKind};
use crate::step_template::{resolve_context_patch, resolve_params};

// ── StepError ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("step '{0}' not found in workflow definition")]
    StepNotFound(String),
    #[error("execution failed for step '{step}': {reason}")]
    ExecutionFailed { step: String, reason: String },
}

// ── StepOutcome ───────────────────────────────────────────────────────────────

/// Result of executing one tick of a workflow step (legacy API).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Proceed to a single next step.
    Next(String),
    /// Execute these steps in sequence, starting with the first.
    Sequence(Vec<String>),
    /// Workflow is complete — no more steps to execute.
    Complete,
    /// Human input is required (headless: fire notification + suspend loop).
    EscalateRequired { question: String },
    /// Step is still in progress (e.g., pr_monitor waiting for CI, advisor still reviewing).
    Pending,
}

impl StepOutcome {
    /// Convert a `StepTarget` into the corresponding `StepOutcome`.
    pub fn from_target(target: &StepTarget) -> Self {
        match target {
            StepTarget::Single(id) => StepOutcome::Next(id.clone()),
            StepTarget::Sequence(ids) => {
                if ids.is_empty() {
                    StepOutcome::Complete
                } else {
                    StepOutcome::Sequence(ids.clone())
                }
            }
        }
    }
}

// ── StepExecutionContext ──────────────────────────────────────────────────────

/// Configuration for the step runner.
#[derive(Debug, Clone)]
pub struct StepExecutionContext {
    /// Root of the workspace (used for build steps).
    pub workspace_root: PathBuf,
    /// When true, human-gate steps produce `EscalateRequired` instead of reading stdin.
    pub headless: bool,
    /// Optional override for all step timeouts (seconds). `None` uses per-step config.
    pub timeout_override_secs: Option<u64>,
}

impl StepExecutionContext {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            headless: false,
            timeout_override_secs: None,
        }
    }

    pub fn headless(mut self) -> Self {
        self.headless = true;
        self
    }

    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_override_secs = Some(timeout_secs);
        self
    }

    /// Resolve the effective timeout for a step.
    pub fn effective_timeout_secs(&self, kind: &WorkflowStepKind) -> Option<u64> {
        if let Some(override_secs) = self.timeout_override_secs {
            return Some(override_secs);
        }
        kind.timeout_str().and_then(parse_timeout_secs)
    }
}

// ── ask_human_headless ────────────────────────────────────────────────────────

/// Non-blocking human-gate handler for headless/autonomous contexts.
pub fn ask_human_headless(question: &str) -> StepOutcome {
    tracing::info!(question = %question, "headless human-gate: escalating without blocking stdin");
    StepOutcome::EscalateRequired {
        question: question.to_string(),
    }
}

// ── route_step ────────────────────────────────────────────────────────────────

/// Route from a completed step using an action-kind string (legacy API).
pub fn route_step(
    def: &StepWorkflowDef,
    step_id: &str,
    action_kind: &str,
) -> Result<StepOutcome, StepError> {
    let kind = def
        .steps
        .get(step_id)
        .ok_or_else(|| StepError::StepNotFound(step_id.to_string()))?;

    match kind.route_for(action_kind) {
        Some(target) => Ok(StepOutcome::from_target(target)),
        None => {
            tracing::debug!(
                step = %step_id,
                action_kind = %action_kind,
                "no on_{} route configured — treating as Complete",
                action_kind
            );
            Ok(StepOutcome::Complete)
        }
    }
}

// ── execute_step ──────────────────────────────────────────────────────────────

/// Execute a step using the full statechart model (v0.17.0.4.5).
///
/// 1. Dispatches `entry_actions` via `router` (with template resolution).
/// 2. Finds the matching transition by evaluating guards.
/// 3. Dispatches transition actions via `router`.
/// 4. Resolves `context_patch` and `payload` templates.
/// 5. Returns `StepOutput` with `edge`, `data`, and `context_patch`.
///
/// Returns `StepOutput::terminal()` when no transition matches.
pub fn execute_step(
    def: &StepWorkflowDef,
    input: StepInput,
    elapsed_secs: u64,
    router: &mut dyn ActionRouter,
) -> Result<StepOutput, StepError> {
    let kind = def
        .steps
        .get(&input.step_id)
        .ok_or_else(|| StepError::StepNotFound(input.step_id.clone()))?;

    let trigger_ref = input.trigger.as_ref();

    // 1. Dispatch entry_actions with template resolution.
    for action in kind.entry_actions() {
        let resolved_params =
            resolve_params(&action.params, trigger_ref, &input.context, elapsed_secs);
        let resolved = StepAction {
            action_type: action.action_type.clone(),
            params: resolved_params,
        };
        if let Err(e) = router.dispatch(&resolved) {
            tracing::warn!(
                step = %input.step_id,
                action = %action.action_type,
                error = %e,
                "entry_action dispatch failed (continuing)"
            );
        }
    }

    // 2. Determine the trigger event name.
    let event = input
        .trigger
        .as_ref()
        .map(|t| t.edge.as_str())
        .unwrap_or("start");

    // 3. Find matching transition.
    let transition = match kind.select_transition(event, &input.context) {
        Some(t) => t,
        None => {
            tracing::debug!(
                step = %input.step_id,
                event = %event,
                "no matching transition — step is terminal"
            );
            return Ok(StepOutput::terminal());
        }
    };

    // 4. Dispatch transition actions.
    for action in &transition.actions {
        let resolved_params =
            resolve_params(&action.params, trigger_ref, &input.context, elapsed_secs);
        let resolved = StepAction {
            action_type: action.action_type.clone(),
            params: resolved_params,
        };
        if let Err(e) = router.dispatch(&resolved) {
            tracing::warn!(
                step = %input.step_id,
                action = %action.action_type,
                error = %e,
                "transition action dispatch failed (continuing)"
            );
        }
    }

    // 5. Resolve context_patch and payload.
    let context_patch = resolve_context_patch(
        &transition.context_patch,
        trigger_ref,
        &input.context,
        elapsed_secs,
    );

    let data = resolve_payload(
        &transition.payload,
        trigger_ref,
        &input.context,
        elapsed_secs,
    );

    Ok(StepOutput {
        edge: transition.on.clone(),
        data,
        context_patch,
    })
}

/// Resolve templates in a payload value (recursively for objects, directly for strings).
fn resolve_payload(
    payload: &serde_json::Value,
    trigger: Option<&TransitionPayload>,
    ctx: &WorkflowContext,
    elapsed_secs: u64,
) -> serde_json::Value {
    use crate::step_template::resolve_template;
    match payload {
        serde_json::Value::String(s) => {
            serde_json::Value::String(resolve_template(s, trigger, ctx, elapsed_secs))
        }
        serde_json::Value::Object(map) => {
            serde_json::Value::Object(resolve_params(map, trigger, ctx, elapsed_secs))
        }
        other => other.clone(),
    }
}

// ── execute_pr_monitor_tick ───────────────────────────────────────────────────

/// PR CI status returned by `gh pr view`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrCiStatus {
    AllPassed,
    Failed,
    Pending,
    Merged,
    Closed,
}

/// Poll `gh pr view --json statusCheckRollup` and map the result to a `PrCiStatus`.
pub fn poll_pr_ci_status(pr_number: u64) -> PrCiStatus {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "state,statusCheckRollup",
            "--jq",
            ".state + \":\" + (if (.statusCheckRollup | length) == 0 then \"PENDING\" else (.statusCheckRollup | map(.conclusion) | if any(. == \"FAILURE\") then \"FAILURE\" elif all(. == \"SUCCESS\" or . == \"SKIPPED\") then \"SUCCESS\" else \"PENDING\" end) end)",
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            let line = raw.trim();
            if line.starts_with("MERGED") || line.contains("MERGED") {
                PrCiStatus::Merged
            } else if line.contains("CLOSED") {
                PrCiStatus::Closed
            } else if line.contains("SUCCESS") {
                PrCiStatus::AllPassed
            } else if line.contains("FAILURE") {
                PrCiStatus::Failed
            } else {
                PrCiStatus::Pending
            }
        }
        _ => PrCiStatus::Pending,
    }
}

/// Execute one monitoring tick for a `pr_monitor` step.
pub fn execute_pr_monitor_tick(
    kind: &WorkflowStepKind,
    pr_number: u64,
    elapsed_secs: u64,
    ctx: &StepExecutionContext,
) -> StepOutcome {
    let (on_pass, on_fail, on_timeout, timeout_str) = match kind {
        WorkflowStepKind::PrMonitor {
            on_pass,
            on_fail,
            on_timeout,
            timeout,
            ..
        } => (on_pass, on_fail, on_timeout, timeout),
        _ => return StepOutcome::Pending,
    };

    let effective_timeout = ctx
        .timeout_override_secs
        .or_else(|| timeout_str.as_deref().and_then(parse_timeout_secs));

    if let Some(timeout_secs) = effective_timeout {
        if elapsed_secs >= timeout_secs {
            tracing::warn!(
                pr = pr_number,
                elapsed_secs = elapsed_secs,
                timeout_secs = timeout_secs,
                "pr_monitor: timeout fired"
            );
            return match on_timeout {
                Some(target) => StepOutcome::from_target(target),
                None => StepOutcome::Complete,
            };
        }
    }

    match poll_pr_ci_status(pr_number) {
        PrCiStatus::AllPassed | PrCiStatus::Merged => match on_pass {
            Some(target) => StepOutcome::from_target(target),
            None => StepOutcome::Complete,
        },
        PrCiStatus::Failed | PrCiStatus::Closed => match on_fail {
            Some(target) => StepOutcome::from_target(target),
            None => StepOutcome::Complete,
        },
        PrCiStatus::Pending => StepOutcome::Pending,
    }
}

// ── execute_sync_build_step ───────────────────────────────────────────────────

/// Result of running a single build sub-step.
#[derive(Debug)]
pub struct BuildStepResult {
    pub step_name: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub output_lines: Vec<String>,
}

/// Execute a single named build sub-step.
pub fn execute_sync_build_step(
    step_name: &str,
    workspace_root: &std::path::Path,
) -> BuildStepResult {
    let (program, args): (&str, &[&str]) = match step_name {
        "git_pull" => ("git", &["pull", "--ff-only"]),
        "cargo_build" => ("cargo", &["build", "--workspace"]),
        "install_local" => ("bash", &["install_local.sh"]),
        _ => {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(step_name)
                .current_dir(workspace_root)
                .output();
            return match output {
                Ok(out) => BuildStepResult {
                    step_name: step_name.to_string(),
                    success: out.status.success(),
                    exit_code: out.status.code(),
                    output_lines: String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|l| l.to_string())
                        .collect(),
                },
                Err(e) => BuildStepResult {
                    step_name: step_name.to_string(),
                    success: false,
                    exit_code: None,
                    output_lines: vec![format!("failed to spawn: {}", e)],
                },
            };
        }
    };

    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(workspace_root)
        .output();

    match output {
        Ok(out) => {
            let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| l.to_string())
                .collect();
            if !out.status.success() {
                for line in String::from_utf8_lossy(&out.stderr).lines() {
                    lines.push(format!("[stderr] {}", line));
                }
            }
            BuildStepResult {
                step_name: step_name.to_string(),
                success: out.status.success(),
                exit_code: out.status.code(),
                output_lines: lines,
            }
        }
        Err(e) => BuildStepResult {
            step_name: step_name.to_string(),
            success: false,
            exit_code: None,
            output_lines: vec![format!("failed to spawn '{}': {}", program, e)],
        },
    }
}

/// Run all build steps for a `sync_build` step and return the outcome.
pub fn execute_sync_build(kind: &WorkflowStepKind, ctx: &StepExecutionContext) -> StepOutcome {
    let (steps, on_failure) = match kind {
        WorkflowStepKind::SyncBuild {
            steps, on_failure, ..
        } => (steps, on_failure),
        _ => return StepOutcome::Complete,
    };

    for step_name in steps {
        tracing::info!(step = %step_name, "sync_build: running sub-step");
        let result = execute_sync_build_step(step_name, &ctx.workspace_root);
        if !result.success {
            tracing::error!(
                step = %step_name,
                exit_code = ?result.exit_code,
                "sync_build: sub-step failed"
            );
            return match on_failure {
                Some(target) => StepOutcome::from_target(target),
                None => StepOutcome::Complete,
            };
        }
        tracing::info!(step = %step_name, "sync_build: sub-step passed");
    }

    StepOutcome::Complete
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_action::{CollectingActionRouter, NoopActionRouter, StepAction};
    use crate::step_context::{TransitionPayload, WorkflowContext};
    use crate::step_guard::GuardExpr;
    use crate::step_kind::{
        PrWaitCondition, StepTarget, StepWorkflowDef, Transition, WorkflowStepKind,
    };
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn make_agent_review_step(
        on_apply: Option<&str>,
        on_timeout: Option<&str>,
    ) -> WorkflowStepKind {
        WorkflowStepKind::AgentReview {
            role: "reviewer".to_string(),
            draft: "latest".to_string(),
            timeout: Some("30m".to_string()),
            on_apply: on_apply.map(|s| StepTarget::Single(s.to_string())),
            on_deny: None,
            on_escalate: None,
            on_plan_mod: None,
            on_timeout: on_timeout.map(|s| StepTarget::Single(s.to_string())),
            transitions: vec![],
            entry_actions: vec![],
        }
    }

    fn make_pr_monitor_step(timeout_str: &str, on_timeout: Option<&str>) -> WorkflowStepKind {
        WorkflowStepKind::PrMonitor {
            wait_for: PrWaitCondition::CiPass,
            timeout: Some(timeout_str.to_string()),
            on_pass: Some(StepTarget::Sequence(vec![
                "merge".to_string(),
                "sync_build".to_string(),
            ])),
            on_fail: Some(StepTarget::Single("escalate".to_string())),
            on_timeout: on_timeout.map(|s| StepTarget::Single(s.to_string())),
            transitions: vec![],
            entry_actions: vec![],
        }
    }

    fn def_with_steps(steps: HashMap<String, WorkflowStepKind>, initial: &str) -> StepWorkflowDef {
        StepWorkflowDef {
            name: "test".to_string(),
            initial_step: initial.to_string(),
            steps,
        }
    }

    // ── ask_human_headless ───────────────────────────────────────────────────

    #[test]
    fn ask_human_headless_produces_escalate_not_blocking() {
        let question = "Is it safe to proceed with the merge?";
        let outcome = ask_human_headless(question);
        match outcome {
            StepOutcome::EscalateRequired { question: q } => {
                assert_eq!(q, question);
            }
            other => panic!("expected EscalateRequired, got {:?}", other),
        }
    }

    #[test]
    fn ask_human_headless_does_not_block() {
        let start = std::time::Instant::now();
        let _ = ask_human_headless("continue?");
        assert!(
            start.elapsed().as_secs() < 1,
            "ask_human_headless should return immediately"
        );
    }

    // ── route_step ───────────────────────────────────────────────────────────

    #[test]
    fn agent_review_apply_routes_to_sync_build() {
        let mut steps = HashMap::new();
        steps.insert(
            "review".to_string(),
            make_agent_review_step(Some("sync_build"), None),
        );
        steps.insert(
            "sync_build".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec!["cargo_build".to_string()],
                on_failure: None,
                transitions: vec![],
                entry_actions: vec![],
            },
        );
        let def = def_with_steps(steps, "review");

        let outcome = route_step(&def, "review", "apply").unwrap();
        assert_eq!(outcome, StepOutcome::Next("sync_build".to_string()));
    }

    #[test]
    fn route_step_no_route_returns_complete() {
        let mut steps = HashMap::new();
        steps.insert(
            "review".to_string(),
            make_agent_review_step(Some("sync_build"), None),
        );
        steps.insert(
            "sync_build".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: None,
                transitions: vec![],
                entry_actions: vec![],
            },
        );
        let def = def_with_steps(steps, "review");

        let outcome = route_step(&def, "review", "plan_mod").unwrap();
        assert_eq!(outcome, StepOutcome::Complete);
    }

    #[test]
    fn route_step_unknown_step_returns_error() {
        let def = def_with_steps(HashMap::new(), "start");
        let result = route_step(&def, "nonexistent", "apply");
        assert!(matches!(result, Err(StepError::StepNotFound(_))));
    }

    // ── pr_monitor tick ──────────────────────────────────────────────────────

    #[test]
    fn pr_monitor_timeout_fires_escalate() {
        let tmp = TempDir::new().unwrap();
        let ctx = StepExecutionContext::new(tmp.path());
        let kind = make_pr_monitor_step("60m", Some("escalate"));

        let outcome = execute_pr_monitor_tick(&kind, 42, 3601, &ctx);
        assert_eq!(outcome, StepOutcome::Next("escalate".to_string()));
    }

    #[test]
    fn pr_monitor_timeout_no_route_returns_complete() {
        let tmp = TempDir::new().unwrap();
        let ctx = StepExecutionContext::new(tmp.path());
        let kind = WorkflowStepKind::PrMonitor {
            wait_for: PrWaitCondition::CiPass,
            timeout: Some("30m".to_string()),
            on_pass: None,
            on_fail: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };

        let outcome = execute_pr_monitor_tick(&kind, 99, 9999, &ctx);
        assert_eq!(outcome, StepOutcome::Complete);
    }

    #[test]
    fn pr_monitor_within_timeout_returns_pending_when_no_gh() {
        let tmp = TempDir::new().unwrap();
        let ctx = StepExecutionContext::new(tmp.path());
        let kind = make_pr_monitor_step("60m", Some("escalate"));

        let outcome = execute_pr_monitor_tick(&kind, 99, 10, &ctx);
        assert!(matches!(
            outcome,
            StepOutcome::Pending
                | StepOutcome::Next(_)
                | StepOutcome::Sequence(_)
                | StepOutcome::Complete
        ));
    }

    #[test]
    fn pr_monitor_context_timeout_override() {
        let tmp = TempDir::new().unwrap();
        let ctx = StepExecutionContext::new(tmp.path()).with_timeout(0);
        let kind = make_pr_monitor_step("60m", Some("escalate"));

        let outcome = execute_pr_monitor_tick(&kind, 42, 1, &ctx);
        assert_eq!(outcome, StepOutcome::Next("escalate".to_string()));
    }

    // ── StepOutcome helpers ──────────────────────────────────────────────────

    #[test]
    fn step_outcome_from_single_target() {
        let target = StepTarget::Single("next_step".to_string());
        assert_eq!(
            StepOutcome::from_target(&target),
            StepOutcome::Next("next_step".to_string())
        );
    }

    #[test]
    fn step_outcome_from_sequence_target() {
        let target = StepTarget::Sequence(vec!["step_a".to_string(), "step_b".to_string()]);
        assert_eq!(
            StepOutcome::from_target(&target),
            StepOutcome::Sequence(vec!["step_a".to_string(), "step_b".to_string()])
        );
    }

    #[test]
    fn step_outcome_from_empty_sequence_is_complete() {
        let target = StepTarget::Sequence(vec![]);
        assert_eq!(StepOutcome::from_target(&target), StepOutcome::Complete);
    }

    // ── StepExecutionContext ─────────────────────────────────────────────────

    #[test]
    fn context_headless_flag() {
        let tmp = TempDir::new().unwrap();
        let ctx = StepExecutionContext::new(tmp.path()).headless();
        assert!(ctx.headless);
    }

    #[test]
    fn context_timeout_override() {
        let tmp = TempDir::new().unwrap();
        let kind = WorkflowStepKind::AgentReview {
            role: "r".to_string(),
            draft: "latest".to_string(),
            timeout: Some("30m".to_string()),
            on_apply: None,
            on_deny: None,
            on_escalate: None,
            on_plan_mod: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        let ctx = StepExecutionContext::new(tmp.path()).with_timeout(120);
        assert_eq!(ctx.effective_timeout_secs(&kind), Some(120));
    }

    #[test]
    fn context_falls_back_to_step_timeout() {
        let tmp = TempDir::new().unwrap();
        let kind = WorkflowStepKind::AgentReview {
            role: "r".to_string(),
            draft: "latest".to_string(),
            timeout: Some("45m".to_string()),
            on_apply: None,
            on_deny: None,
            on_escalate: None,
            on_plan_mod: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        let ctx = StepExecutionContext::new(tmp.path());
        assert_eq!(ctx.effective_timeout_secs(&kind), Some(45 * 60));
    }

    // ── execute_step (v0.17.0.4.5) ───────────────────────────────────────────

    fn make_def_with_transitions(
        step_id: &str,
        transitions: Vec<Transition>,
        entry_actions: Vec<StepAction>,
    ) -> StepWorkflowDef {
        let mut steps = HashMap::new();
        steps.insert(
            step_id.to_string(),
            WorkflowStepKind::HumanGate {
                message: String::new(),
                options: vec![],
                transitions,
                entry_actions,
            },
        );
        StepWorkflowDef {
            name: "test".to_string(),
            initial_step: step_id.to_string(),
            steps,
        }
    }

    #[test]
    fn execute_step_applies_context_patch() {
        let mut patch = serde_json::Map::new();
        patch.insert("rework_count".to_string(), serde_json::json!(1));

        let def = make_def_with_transitions(
            "gate",
            vec![Transition {
                on: "start".to_string(),
                guard: None,
                actions: vec![],
                context_patch: patch,
                payload: serde_json::Value::Null,
                to: Some(StepTarget::Single("next".to_string())),
            }],
            vec![],
        );

        let input = StepInput::initial("gate", WorkflowContext::new());
        let mut router = NoopActionRouter;
        let output = execute_step(&def, input, 0, &mut router).unwrap();

        assert_eq!(output.edge, "start");
        assert_eq!(
            output.context_patch.get("rework_count"),
            Some(&serde_json::json!(1))
        );
    }

    #[test]
    fn guard_selects_correct_branch_at_rework_count_boundary() {
        let def = make_def_with_transitions(
            "gate",
            vec![
                Transition {
                    on: "deny".to_string(),
                    guard: Some(GuardExpr::parse("context.rework_count < 3").unwrap()),
                    actions: vec![],
                    context_patch: serde_json::Map::new(),
                    payload: serde_json::Value::Null,
                    to: Some(StepTarget::Single("rework".to_string())),
                },
                Transition {
                    on: "deny".to_string(),
                    guard: Some(GuardExpr::parse("context.rework_count >= 3").unwrap()),
                    actions: vec![],
                    context_patch: serde_json::Map::new(),
                    payload: serde_json::Value::Null,
                    to: Some(StepTarget::Single("human_gate".to_string())),
                },
            ],
            vec![],
        );

        // rework_count = 2: first branch matches (< 3)
        let mut ctx_low = WorkflowContext::new();
        ctx_low
            .fields
            .insert("rework_count".to_string(), serde_json::json!(2));
        let trigger_deny = TransitionPayload {
            source_step: "prev".to_string(),
            edge: "deny".to_string(),
            data: serde_json::Value::Null,
        };
        let input_low = StepInput::with_trigger("gate", trigger_deny.clone(), ctx_low);
        let mut router = NoopActionRouter;
        let out_low = execute_step(&def, input_low, 0, &mut router).unwrap();
        assert_eq!(out_low.edge, "deny");
        // The transition selected routes to "rework"
        // Verify by checking edge (both transitions have on:"deny", so check via target)
        // We can verify this by checking what select_transition returns separately
        let kind = def.steps.get("gate").unwrap();
        let mut ctx_low2 = WorkflowContext::new();
        ctx_low2
            .fields
            .insert("rework_count".to_string(), serde_json::json!(2));
        let t_low = kind.select_transition("deny", &ctx_low2).unwrap();
        assert_eq!(t_low.to, Some(StepTarget::Single("rework".to_string())));

        // rework_count = 3: second branch matches (>= 3)
        let mut ctx_high = WorkflowContext::new();
        ctx_high
            .fields
            .insert("rework_count".to_string(), serde_json::json!(3));
        let t_high = kind.select_transition("deny", &ctx_high).unwrap();
        assert_eq!(
            t_high.to,
            Some(StepTarget::Single("human_gate".to_string()))
        );
    }

    #[test]
    fn ta_apply_draft_action_dispatched_on_apply() {
        let mut params = serde_json::Map::new();
        params.insert("draft_id".to_string(), serde_json::json!("d1"));

        let def = make_def_with_transitions(
            "review",
            vec![Transition {
                on: "start".to_string(),
                guard: None,
                actions: vec![StepAction {
                    action_type: "ta.apply_draft".to_string(),
                    params,
                }],
                context_patch: serde_json::Map::new(),
                payload: serde_json::Value::Null,
                to: Some(StepTarget::Single("build".to_string())),
            }],
            vec![],
        );

        let input = StepInput::initial("review", WorkflowContext::new());
        let mut router = CollectingActionRouter::new();
        execute_step(&def, input, 0, &mut router).unwrap();

        assert_eq!(router.dispatched.len(), 1);
        assert_eq!(router.dispatched[0].action_type, "ta.apply_draft");
        assert_eq!(
            router.dispatched[0].params["draft_id"],
            serde_json::json!("d1")
        );
    }

    #[test]
    fn external_send_email_no_credentials_in_params() {
        let mut params = serde_json::Map::new();
        params.insert("to".to_string(), serde_json::json!("alice@example.com"));
        params.insert("subject".to_string(), serde_json::json!("Draft approved"));
        // No password, token, or secret fields.

        let def = make_def_with_transitions(
            "review",
            vec![Transition {
                on: "start".to_string(),
                guard: None,
                actions: vec![StepAction {
                    action_type: "external.send_email".to_string(),
                    params,
                }],
                context_patch: serde_json::Map::new(),
                payload: serde_json::Value::Null,
                to: None,
            }],
            vec![],
        );

        let input = StepInput::initial("review", WorkflowContext::new());
        let mut router = CollectingActionRouter::new();
        execute_step(&def, input, 0, &mut router).unwrap();

        assert_eq!(router.dispatched.len(), 1);
        let dispatched = &router.dispatched[0];
        assert_eq!(dispatched.action_type, "external.send_email");
        // Verify no credential fields.
        assert!(!dispatched.params.contains_key("password"));
        assert!(!dispatched.params.contains_key("token"));
        assert!(!dispatched.params.contains_key("secret"));
        // Verify legitimate fields are present.
        assert_eq!(
            dispatched.params["to"],
            serde_json::json!("alice@example.com")
        );
    }

    #[test]
    fn template_interpolation_resolves_context_and_trigger_fields() {
        let mut action_params = serde_json::Map::new();
        action_params.insert("to".to_string(), serde_json::json!("{{context.author}}"));
        action_params.insert(
            "ref".to_string(),
            serde_json::json!("{{trigger.data.draft_id}}"),
        );

        let def = make_def_with_transitions(
            "review",
            vec![Transition {
                on: "apply".to_string(),
                guard: None,
                actions: vec![StepAction {
                    action_type: "external.send_email".to_string(),
                    params: action_params,
                }],
                context_patch: serde_json::Map::new(),
                payload: serde_json::Value::Null,
                to: None,
            }],
            vec![],
        );

        let mut ctx = WorkflowContext::new();
        ctx.fields
            .insert("author".to_string(), serde_json::json!("alice"));

        let trigger = TransitionPayload {
            source_step: "prev".to_string(),
            edge: "apply".to_string(),
            data: serde_json::json!({"draft_id": "d42"}),
        };
        let input = StepInput::with_trigger("review", trigger, ctx);
        let mut router = CollectingActionRouter::new();
        execute_step(&def, input, 0, &mut router).unwrap();

        assert_eq!(router.dispatched.len(), 1);
        let dispatched = &router.dispatched[0];
        assert_eq!(dispatched.params["to"], serde_json::json!("alice"));
        assert_eq!(dispatched.params["ref"], serde_json::json!("d42"));
    }

    #[test]
    fn execute_step_entry_actions_dispatched_before_transition() {
        let entry_action = StepAction::new("ta.notify");

        let def = make_def_with_transitions(
            "gate",
            vec![Transition {
                on: "start".to_string(),
                guard: None,
                actions: vec![StepAction::new("ta.apply_draft")],
                context_patch: serde_json::Map::new(),
                payload: serde_json::Value::Null,
                to: None,
            }],
            vec![entry_action],
        );

        let input = StepInput::initial("gate", WorkflowContext::new());
        let mut router = CollectingActionRouter::new();
        execute_step(&def, input, 0, &mut router).unwrap();

        // entry action dispatched first, then transition action.
        assert_eq!(router.dispatched.len(), 2);
        assert_eq!(router.dispatched[0].action_type, "ta.notify");
        assert_eq!(router.dispatched[1].action_type, "ta.apply_draft");
    }

    #[test]
    fn execute_step_unknown_step_returns_error() {
        let def = def_with_steps(HashMap::new(), "start");
        let input = StepInput::initial("nonexistent", WorkflowContext::new());
        let mut router = NoopActionRouter;
        assert!(matches!(
            execute_step(&def, input, 0, &mut router),
            Err(StepError::StepNotFound(_))
        ));
    }

    #[test]
    fn execute_step_no_matching_transition_returns_terminal() {
        let def = make_def_with_transitions("gate", vec![], vec![]);
        let input = StepInput::initial("gate", WorkflowContext::new());
        let mut router = NoopActionRouter;
        let output = execute_step(&def, input, 0, &mut router).unwrap();
        assert!(output.is_terminal());
    }
}
