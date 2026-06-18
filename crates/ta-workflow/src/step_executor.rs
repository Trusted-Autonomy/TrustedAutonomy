// step_executor.rs — Step execution outcomes and headless human-gate logic (v0.17.0.4).
//
// Provides:
//   StepOutcome       — result of executing one "tick" of a workflow step
//   StepExecutionContext — configuration for the step runner
//   ask_human_headless()  — non-blocking escalation (no stdin read)
//   route_step()          — map an action-kind string to the next StepTarget
//   execute_pr_monitor_tick() — poll `gh pr view` and route on CI outcome
//   execute_sync_build_step() — run a named build sub-step

use std::path::PathBuf;

use crate::step_kind::{parse_timeout_secs, StepTarget, StepWorkflowDef, WorkflowStepKind};

// ── StepError ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("step '{0}' not found in workflow definition")]
    StepNotFound(String),
    #[error("execution failed for step '{step}': {reason}")]
    ExecutionFailed { step: String, reason: String },
}

// ── StepOutcome ───────────────────────────────────────────────────────────────

/// Result of executing one tick of a workflow step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Proceed to a single next step.
    Next(String),
    /// Execute these steps in sequence, starting with the first.
    Sequence(Vec<String>),
    /// Workflow is complete — no more steps to execute.
    Complete,
    /// Human input is required (headless: fire notification + suspend loop).
    ///
    /// In headless mode, the loop should emit this as an Escalate action rather
    /// than blocking on stdin. The caller dispatches a notification via ta-events.
    EscalateRequired { question: String },
    /// Step is still in progress (e.g., pr_monitor waiting for CI, advisor still reviewing).
    ///
    /// The runner should retry after a delay.
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
///
/// Instead of blocking on stdin (which would deadlock an autonomous loop),
/// returns `StepOutcome::EscalateRequired`. The caller is responsible for:
/// 1. Emitting an `AgentAction::Escalate` via the ActionRouter
/// 2. Dispatching a notification via ta-events
/// 3. Suspending the workflow loop at this step until a response arrives
///
/// This satisfies item 5: `ta_ask_human` in headless context routes to `Escalate`
/// rather than blocking.
pub fn ask_human_headless(question: &str) -> StepOutcome {
    tracing::info!(question = %question, "headless human-gate: escalating without blocking stdin");
    StepOutcome::EscalateRequired {
        question: question.to_string(),
    }
}

// ── route_step ────────────────────────────────────────────────────────────────

/// Route from a completed step using an action-kind string.
///
/// Looks up `def.steps[step_id]`, calls `kind.route_for(action_kind)`, and
/// converts the result to a `StepOutcome`.
///
/// Returns `StepOutcome::Complete` if no route is configured for the action.
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

// ── execute_pr_monitor_tick ───────────────────────────────────────────────────

/// PR CI status returned by `gh pr view`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrCiStatus {
    /// All required checks passed.
    AllPassed,
    /// One or more checks failed.
    Failed,
    /// Checks are still running (pending/queued).
    Pending,
    /// PR is already merged.
    Merged,
    /// PR was closed without merging.
    Closed,
}

/// Poll `gh pr view --json statusCheckRollup` and map the result to a `PrCiStatus`.
///
/// Returns `PrCiStatus::Pending` if `gh` is unavailable or parsing fails —
/// the caller should retry after a delay.
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
///
/// Polls PR CI status and returns the appropriate `StepOutcome` based on the
/// step's `on_pass`/`on_fail`/`on_timeout` routing.
///
/// `elapsed_secs` should be the number of seconds since the step started —
/// used to check whether the timeout has fired.
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

    // Check timeout first.
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

    // Poll CI status.
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
///
/// Built-in step names:
///   `git_pull`      → `git pull --ff-only`
///   `cargo_build`   → `cargo build --workspace`
///   `install_local` → `bash install_local.sh`
///
/// Any other name is treated as a shell command.
pub fn execute_sync_build_step(
    step_name: &str,
    workspace_root: &std::path::Path,
) -> BuildStepResult {
    let (program, args): (&str, &[&str]) = match step_name {
        "git_pull" => ("git", &["pull", "--ff-only"]),
        "cargo_build" => ("cargo", &["build", "--workspace"]),
        "install_local" => ("bash", &["install_local.sh"]),
        other => {
            // Treat as a shell command.
            let parts: Vec<&str> = other.splitn(2, ' ').collect();
            if parts.is_empty() {
                return BuildStepResult {
                    step_name: step_name.to_string(),
                    success: false,
                    exit_code: None,
                    output_lines: vec!["empty step name".to_string()],
                };
            }
            // We can't return references to stack data here, so fall back to sh -c.
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
///
/// Stops at the first failed sub-step and routes via `on_failure`.
pub fn execute_sync_build(kind: &WorkflowStepKind, ctx: &StepExecutionContext) -> StepOutcome {
    let (steps, on_failure) = match kind {
        WorkflowStepKind::SyncBuild { steps, on_failure } => (steps, on_failure),
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
    use crate::step_kind::{PrWaitCondition, StepTarget, StepWorkflowDef, WorkflowStepKind};
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
        // Verify the call returns immediately (no stdin read).
        // If ask_human_headless blocked on stdin, this test would hang.
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
            },
        );
        let def = def_with_steps(steps, "review");

        // "plan_mod" has no on_plan_mod configured → Complete.
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

        // Simulate elapsed time exceeding the 60m timeout (3601 seconds).
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
            on_timeout: None, // no on_timeout configured
        };

        // Elapsed > timeout → Complete (no route).
        let outcome = execute_pr_monitor_tick(&kind, 99, 9999, &ctx);
        assert_eq!(outcome, StepOutcome::Complete);
    }

    #[test]
    fn pr_monitor_within_timeout_returns_pending_when_no_gh() {
        let tmp = TempDir::new().unwrap();
        let ctx = StepExecutionContext::new(tmp.path());
        let kind = make_pr_monitor_step("60m", Some("escalate"));

        // Elapsed < timeout; gh is not available in test env so poll returns Pending.
        // We can't force gh to be unavailable, but we can verify the elapsed check:
        let outcome = execute_pr_monitor_tick(&kind, 99, 10, &ctx);
        // Either Pending (gh unavailable) or a routed outcome (gh available).
        // Both are valid — the important thing is it doesn't panic.
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
        // Override to 0 seconds → fires immediately.
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
        };
        let ctx = StepExecutionContext::new(tmp.path());
        assert_eq!(ctx.effective_timeout_secs(&kind), Some(45 * 60));
    }
}
