// step_kind.rs — WorkflowStepKind and step-based workflow types (v0.17.0.4.5).
//
// Five step types for the autonomous phase loop:
//   agent_review  — invoke advisor, route on AgentAction outcome
//   pr_monitor    — poll PR CI status, route on pass/fail/timeout
//   plan_check    — invoke architect to validate upcoming phases
//   sync_build    — (deprecated) run a sequence of local build commands
//   human_gate    — non-blocking escalation point in headless contexts
//
// Step-based workflows are distinct from stage-based WorkflowDefinition.
// They use a map of step IDs to WorkflowStepKind values. Routing uses either
// the legacy `on_*` fields (backward compat) or the new `transitions:` list
// (v0.17.0.4.5 — supports guards, actions, and context patches).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::step_action::StepAction;
use crate::step_context::WorkflowContext;
use crate::step_guard::GuardExpr;

// ── StepTarget ────────────────────────────────────────────────────────────────

/// Routing target: a single next step ID or a sequence to execute in order.
///
/// YAML forms:
///   on_apply: sync_build               # Single step
///   on_pass: [merge, sync_build_local] # Sequence
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StepTarget {
    Single(String),
    Sequence(Vec<String>),
}

impl StepTarget {
    /// Return the first step ID in this target.
    pub fn first(&self) -> &str {
        match self {
            StepTarget::Single(s) => s.as_str(),
            StepTarget::Sequence(v) => v.first().map(|s| s.as_str()).unwrap_or(""),
        }
    }

    /// Return all step IDs in this target, in execution order.
    pub fn step_ids(&self) -> Vec<&str> {
        match self {
            StepTarget::Single(s) => vec![s.as_str()],
            StepTarget::Sequence(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

// ── PrWaitCondition ───────────────────────────────────────────────────────────

/// Condition the `pr_monitor` step waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrWaitCondition {
    /// All required CI checks pass.
    #[default]
    CiPass,
    /// PR is merged.
    Merged,
    /// PR is closed (merged or declined).
    Closed,
}

impl std::fmt::Display for PrWaitCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrWaitCondition::CiPass => write!(f, "ci_pass"),
            PrWaitCondition::Merged => write!(f, "merged"),
            PrWaitCondition::Closed => write!(f, "closed"),
        }
    }
}

// ── Timeout parsing ───────────────────────────────────────────────────────────

/// Parse a timeout string like "30m", "2h", "90s" into seconds.
///
/// Returns `None` if the string is empty or unparseable.
pub fn parse_timeout_secs(timeout: &str) -> Option<u64> {
    let s = timeout.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_suffix('m') {
        rest.parse::<u64>().ok().map(|m| m * 60)
    } else if let Some(rest) = s.strip_suffix('h') {
        rest.parse::<u64>().ok().map(|h| h * 3600)
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.parse::<u64>().ok()
    } else {
        s.parse::<u64>().ok()
    }
}

// ── Transition ────────────────────────────────────────────────────────────────

/// A single state-machine transition on a step.
///
/// Transitions replace flat `on_*` keys (v0.17.0.4.5). Multiple transitions
/// on the same event with different guards allow conditional routing — the
/// first matching transition wins.
///
/// YAML example:
/// ```yaml
/// transitions:
///   - on: apply
///     guard: "context.rework_count < 3"
///     actions:
///       - action_type: ta.apply_draft
///         params:
///           draft_id: "{{trigger.data.draft_id}}"
///     context_patch:
///       last_applied_draft: "{{trigger.data.draft_id}}"
///     payload:
///       draft_id: "{{trigger.data.draft_id}}"
///     to: pr_monitor
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    /// Trigger event name (e.g., "apply", "deny", "timeout").
    pub on: String,
    /// Optional guard expression — evaluated before actions. First matching guard wins.
    #[serde(default)]
    pub guard: Option<GuardExpr>,
    /// Actions to dispatch when this transition fires.
    #[serde(default)]
    pub actions: Vec<StepAction>,
    /// Fields to merge into WorkflowContext.fields after dispatch.
    #[serde(default)]
    pub context_patch: serde_json::Map<String, serde_json::Value>,
    /// Payload to carry to the next step's trigger.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Routing target (next step ID or sequence). `None` means terminal.
    #[serde(default)]
    pub to: Option<StepTarget>,
}

// ── WorkflowStepKind ──────────────────────────────────────────────────────────

/// Step type variants for step-based workflows (v0.17.0.4 / v0.17.0.4.5).
///
/// Each variant may define routing via:
/// - Legacy: `on_*` fields (backward compat, auto-promoted to transitions)
/// - New: `transitions:` list with optional guards, actions, and context patches
///
/// `entry_actions` fire on step entry before transition evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowStepKind {
    /// Invoke an advisor agent to review the latest draft.
    ///
    /// Routes based on the advisor's `AgentAction` outcome.
    AgentReview {
        /// Team role of the advisor to invoke.
        role: String,
        /// Draft target: "latest" or a specific draft ID.
        #[serde(default = "default_latest")]
        draft: String,
        /// Timeout string (e.g., "30m", "2h"). Fires `on_timeout` when elapsed.
        #[serde(default)]
        timeout: Option<String>,
        /// Route when advisor returns Apply.
        #[serde(default)]
        on_apply: Option<StepTarget>,
        /// Route when advisor returns Deny.
        #[serde(default)]
        on_deny: Option<StepTarget>,
        /// Route when advisor returns Escalate.
        #[serde(default)]
        on_escalate: Option<StepTarget>,
        /// Route when advisor returns PlanMod.
        #[serde(default)]
        on_plan_mod: Option<StepTarget>,
        /// Route when the timeout elapses.
        #[serde(default)]
        on_timeout: Option<StepTarget>,
        /// New-style transition list (v0.17.0.4.5). Takes precedence over on_* fields.
        #[serde(default)]
        transitions: Vec<Transition>,
        /// Actions fired on step entry before transition evaluation.
        #[serde(default)]
        entry_actions: Vec<StepAction>,
    },

    /// Poll a pull request's CI status and route on pass/fail/timeout.
    PrMonitor {
        /// Condition to wait for (default: ci_pass).
        #[serde(default)]
        wait_for: PrWaitCondition,
        /// Timeout string. Fires `on_timeout` when elapsed.
        #[serde(default)]
        timeout: Option<String>,
        /// Route when the wait condition is satisfied.
        #[serde(default)]
        on_pass: Option<StepTarget>,
        /// Route when CI checks fail.
        #[serde(default)]
        on_fail: Option<StepTarget>,
        /// Route when the timeout elapses.
        #[serde(default)]
        on_timeout: Option<StepTarget>,
        /// New-style transition list (v0.17.0.4.5).
        #[serde(default)]
        transitions: Vec<Transition>,
        /// Actions fired on step entry.
        #[serde(default)]
        entry_actions: Vec<StepAction>,
    },

    /// Invoke an architect agent to review upcoming plan phases.
    PlanCheck {
        /// Team role of the agent to invoke for plan review.
        #[serde(default = "default_architect")]
        agent_role: String,
        /// Which phases to review: "next_1", "next_3", "all_pending", or a phase ID.
        #[serde(default = "default_next_3")]
        check_phases: String,
        /// Route when the agent proposes plan modifications.
        #[serde(default)]
        on_modification: Option<StepTarget>,
        /// Route when the agent finds no changes needed.
        #[serde(default)]
        on_no_change: Option<StepTarget>,
        /// Route when the timeout elapses.
        #[serde(default)]
        on_timeout: Option<StepTarget>,
        /// New-style transition list (v0.17.0.4.5).
        #[serde(default)]
        transitions: Vec<Transition>,
        /// Actions fired on step entry.
        #[serde(default)]
        entry_actions: Vec<StepAction>,
    },

    /// Run a sequence of local build commands.
    ///
    /// **Deprecated** (v0.17.0.4.5): use `ta.run_build` actions on transitions
    /// or in `entry_actions` instead. `sync_build` is a synchronous action
    /// sequence, not a state waiting for an external event.
    SyncBuild {
        /// Ordered list of build step names to execute.
        #[serde(default)]
        steps: Vec<String>,
        /// Route when any step fails.
        #[serde(default)]
        on_failure: Option<StepTarget>,
        /// New-style transition list (v0.17.0.4.5).
        #[serde(default)]
        transitions: Vec<Transition>,
        /// Actions fired on step entry.
        #[serde(default)]
        entry_actions: Vec<StepAction>,
    },

    /// Non-blocking escalation gate for headless/autonomous contexts.
    ///
    /// Interactive mode: prompts the human and routes on their choice.
    /// Headless mode: produces an `Escalate` action without blocking.
    /// `transitions:` list now provides typed exit routes.
    HumanGate {
        /// Question or message.
        #[serde(default)]
        message: String,
        /// Available response options in interactive mode.
        #[serde(default)]
        options: Vec<String>,
        /// New-style transition list (v0.17.0.4.5) — exit routes for each option.
        #[serde(default)]
        transitions: Vec<Transition>,
        /// Actions fired on step entry.
        #[serde(default)]
        entry_actions: Vec<StepAction>,
    },
}

fn default_latest() -> String {
    "latest".to_string()
}

fn default_architect() -> String {
    "architect".to_string()
}

fn default_next_3() -> String {
    "next_3".to_string()
}

impl WorkflowStepKind {
    /// Return the transitions list for this step.
    pub fn transitions(&self) -> &[Transition] {
        match self {
            WorkflowStepKind::AgentReview { transitions, .. } => transitions.as_slice(),
            WorkflowStepKind::PrMonitor { transitions, .. } => transitions.as_slice(),
            WorkflowStepKind::PlanCheck { transitions, .. } => transitions.as_slice(),
            WorkflowStepKind::SyncBuild { transitions, .. } => transitions.as_slice(),
            WorkflowStepKind::HumanGate { transitions, .. } => transitions.as_slice(),
        }
    }

    /// Return the entry_actions for this step.
    pub fn entry_actions(&self) -> &[StepAction] {
        match self {
            WorkflowStepKind::AgentReview { entry_actions, .. } => entry_actions.as_slice(),
            WorkflowStepKind::PrMonitor { entry_actions, .. } => entry_actions.as_slice(),
            WorkflowStepKind::PlanCheck { entry_actions, .. } => entry_actions.as_slice(),
            WorkflowStepKind::SyncBuild { entry_actions, .. } => entry_actions.as_slice(),
            WorkflowStepKind::HumanGate { entry_actions, .. } => entry_actions.as_slice(),
        }
    }

    /// Select the first matching transition for `event`, evaluating guards against `ctx`.
    ///
    /// If the step has `transitions:` configured, iterates them and returns the first
    /// where `t.on == event` and the guard passes (or there is no guard).
    ///
    /// Falls back to synthesizing a guard-free Transition from `on_*` fields when
    /// `transitions` is empty (backward compatibility).
    pub fn select_transition(&self, event: &str, ctx: &WorkflowContext) -> Option<Transition> {
        let trs = self.transitions();
        if !trs.is_empty() {
            // New-style: guard-evaluated selection.
            for t in trs {
                if t.on == event {
                    let guard_passes = match &t.guard {
                        Some(g) => g.evaluate(ctx),
                        None => true,
                    };
                    if guard_passes {
                        return Some(t.clone());
                    }
                }
            }
            return None;
        }

        // Backward compat: synthesize from on_* fields.
        let target = self.route_for(event)?;
        Some(Transition {
            on: event.to_string(),
            guard: None,
            actions: vec![],
            context_patch: serde_json::Map::new(),
            payload: serde_json::Value::Null,
            to: Some(target.clone()),
        })
    }

    /// Return all routing targets defined on this step (for validation and reachability).
    pub fn routing_targets(&self) -> Vec<&StepTarget> {
        let mut targets = Vec::new();

        // Collect from legacy on_* fields.
        match self {
            WorkflowStepKind::AgentReview {
                on_apply,
                on_deny,
                on_escalate,
                on_plan_mod,
                on_timeout,
                ..
            } => {
                for t in [on_apply, on_deny, on_escalate, on_plan_mod, on_timeout]
                    .into_iter()
                    .flatten()
                {
                    targets.push(t);
                }
            }
            WorkflowStepKind::PrMonitor {
                on_pass,
                on_fail,
                on_timeout,
                ..
            } => {
                for t in [on_pass, on_fail, on_timeout].into_iter().flatten() {
                    targets.push(t);
                }
            }
            WorkflowStepKind::PlanCheck {
                on_modification,
                on_no_change,
                on_timeout,
                ..
            } => {
                for t in [on_modification, on_no_change, on_timeout]
                    .into_iter()
                    .flatten()
                {
                    targets.push(t);
                }
            }
            WorkflowStepKind::SyncBuild { on_failure, .. } => {
                if let Some(t) = on_failure {
                    targets.push(t);
                }
            }
            WorkflowStepKind::HumanGate { .. } => {}
        }

        // Collect from new-style transitions.
        for t in self.transitions() {
            if let Some(target) = &t.to {
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }

        targets
    }

    /// Return the timeout string for this step, if configured.
    pub fn timeout_str(&self) -> Option<&str> {
        match self {
            WorkflowStepKind::AgentReview { timeout, .. } => timeout.as_deref(),
            WorkflowStepKind::PrMonitor { timeout, .. } => timeout.as_deref(),
            _ => None,
        }
    }

    /// Returns true for step types that can block indefinitely waiting for an external event.
    ///
    /// Blocking steps should always configure `on_timeout` to avoid deadlocking the loop.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            WorkflowStepKind::AgentReview { .. }
                | WorkflowStepKind::PrMonitor { .. }
                | WorkflowStepKind::HumanGate { .. }
        )
    }

    /// Route this step's output based on an action kind string (legacy on_* fields).
    ///
    /// Returns the `StepTarget` for the given outcome, or `None` if no route is configured.
    pub fn route_for(&self, action_kind: &str) -> Option<&StepTarget> {
        match (self, action_kind) {
            (WorkflowStepKind::AgentReview { on_apply, .. }, "apply") => on_apply.as_ref(),
            (WorkflowStepKind::AgentReview { on_deny, .. }, "deny") => on_deny.as_ref(),
            (WorkflowStepKind::AgentReview { on_escalate, .. }, "escalate") => on_escalate.as_ref(),
            (WorkflowStepKind::AgentReview { on_plan_mod, .. }, "plan_mod") => on_plan_mod.as_ref(),
            (WorkflowStepKind::AgentReview { on_timeout, .. }, "timeout")
            | (WorkflowStepKind::PrMonitor { on_timeout, .. }, "timeout")
            | (WorkflowStepKind::PlanCheck { on_timeout, .. }, "timeout") => on_timeout.as_ref(),
            (WorkflowStepKind::PrMonitor { on_pass, .. }, "pass") => on_pass.as_ref(),
            (WorkflowStepKind::PrMonitor { on_fail, .. }, "fail") => on_fail.as_ref(),
            (
                WorkflowStepKind::PlanCheck {
                    on_modification, ..
                },
                "modification",
            ) => on_modification.as_ref(),
            (WorkflowStepKind::PlanCheck { on_no_change, .. }, "no_change") => {
                on_no_change.as_ref()
            }
            (WorkflowStepKind::SyncBuild { on_failure, .. }, "failure") => on_failure.as_ref(),
            _ => None,
        }
    }

    /// Return the YAML `type:` name for this step kind.
    pub fn type_name(&self) -> &str {
        match self {
            WorkflowStepKind::AgentReview { .. } => "agent_review",
            WorkflowStepKind::PrMonitor { .. } => "pr_monitor",
            WorkflowStepKind::PlanCheck { .. } => "plan_check",
            WorkflowStepKind::SyncBuild { .. } => "sync_build",
            WorkflowStepKind::HumanGate { .. } => "human_gate",
        }
    }
}

// ── StepWorkflowDef ───────────────────────────────────────────────────────────

/// A complete step-based workflow definition.
///
/// Distinct from `WorkflowDefinition` (stage-based). Step workflows model
/// autonomous loops where outcomes route directly to named steps rather than
/// advancing through ordered stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepWorkflowDef {
    /// Human-readable workflow name.
    pub name: String,
    /// ID of the first step to execute when the workflow starts.
    pub initial_step: String,
    /// Map from step ID to step definition.
    pub steps: HashMap<String, WorkflowStepKind>,
}

impl StepWorkflowDef {
    /// Parse a step workflow definition from YAML.
    pub fn from_yaml(yaml: &str) -> Result<Self, crate::WorkflowError> {
        serde_yaml::from_str(yaml).map_err(|e| crate::WorkflowError::ParseError {
            reason: e.to_string(),
        })
    }

    /// Parse a step workflow definition from a file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, crate::WorkflowError> {
        let content = std::fs::read_to_string(path).map_err(|e| crate::WorkflowError::IoError {
            path: path.display().to_string(),
            source: e,
        })?;
        Self::from_yaml(&content)
    }

    /// Return all step IDs reachable from `initial_step` (DFS order).
    ///
    /// Useful for reachability analysis and cycle detection.
    pub fn reachable_from_initial(&self) -> Vec<String> {
        let mut visited = Vec::new();
        let mut stack = vec![self.initial_step.clone()];
        while let Some(id) = stack.pop() {
            if visited.contains(&id) {
                continue;
            }
            visited.push(id.clone());
            if let Some(kind) = self.steps.get(&id) {
                for target in kind.routing_targets() {
                    for step_id in target.step_ids() {
                        if !visited.contains(&step_id.to_string()) {
                            stack.push(step_id.to_string());
                        }
                    }
                }
            }
        }
        visited
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_target_single_first() {
        let t = StepTarget::Single("sync_build".to_string());
        assert_eq!(t.first(), "sync_build");
        assert_eq!(t.step_ids(), vec!["sync_build"]);
    }

    #[test]
    fn step_target_sequence_ids() {
        let t = StepTarget::Sequence(vec!["merge".to_string(), "sync_build".to_string()]);
        assert_eq!(t.first(), "merge");
        assert_eq!(t.step_ids(), vec!["merge", "sync_build"]);
    }

    #[test]
    fn step_target_serde_single() {
        let t = StepTarget::Single("foo".to_string());
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, r#""foo""#);
        let back: StepTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn step_target_serde_sequence() {
        let t = StepTarget::Sequence(vec!["a".to_string(), "b".to_string()]);
        let json = serde_json::to_string(&t).unwrap();
        let back: StepTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn parse_timeout_minutes() {
        assert_eq!(parse_timeout_secs("30m"), Some(1800));
        assert_eq!(parse_timeout_secs("60m"), Some(3600));
        assert_eq!(parse_timeout_secs("1m"), Some(60));
    }

    #[test]
    fn parse_timeout_hours() {
        assert_eq!(parse_timeout_secs("2h"), Some(7200));
        assert_eq!(parse_timeout_secs("1h"), Some(3600));
    }

    #[test]
    fn parse_timeout_seconds() {
        assert_eq!(parse_timeout_secs("90s"), Some(90));
        assert_eq!(parse_timeout_secs("300"), Some(300));
    }

    #[test]
    fn parse_timeout_empty() {
        assert_eq!(parse_timeout_secs(""), None);
        assert_eq!(parse_timeout_secs("  "), None);
        assert_eq!(parse_timeout_secs("invalid"), None);
    }

    #[test]
    fn agent_review_routing_targets() {
        let kind = WorkflowStepKind::AgentReview {
            role: "reviewer".to_string(),
            draft: "latest".to_string(),
            timeout: Some("30m".to_string()),
            on_apply: Some(StepTarget::Single("sync_build".to_string())),
            on_deny: Some(StepTarget::Single("rework".to_string())),
            on_escalate: Some(StepTarget::Single("human_gate".to_string())),
            on_plan_mod: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        let targets = kind.routing_targets();
        assert_eq!(targets.len(), 3);
    }

    #[test]
    fn agent_review_route_for_apply() {
        let kind = WorkflowStepKind::AgentReview {
            role: "reviewer".to_string(),
            draft: "latest".to_string(),
            timeout: None,
            on_apply: Some(StepTarget::Single("sync_build".to_string())),
            on_deny: None,
            on_escalate: None,
            on_plan_mod: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        assert_eq!(
            kind.route_for("apply"),
            Some(&StepTarget::Single("sync_build".to_string()))
        );
        assert_eq!(kind.route_for("deny"), None);
        assert_eq!(kind.route_for("unknown"), None);
    }

    #[test]
    fn pr_monitor_route_for_pass_fail_timeout() {
        let kind = WorkflowStepKind::PrMonitor {
            wait_for: PrWaitCondition::CiPass,
            timeout: Some("60m".to_string()),
            on_pass: Some(StepTarget::Sequence(vec![
                "merge".to_string(),
                "sync_build".to_string(),
            ])),
            on_fail: Some(StepTarget::Single("escalate".to_string())),
            on_timeout: Some(StepTarget::Single("escalate".to_string())),
            transitions: vec![],
            entry_actions: vec![],
        };
        let pass = kind.route_for("pass").unwrap();
        assert_eq!(pass.step_ids(), vec!["merge", "sync_build"]);
        assert!(kind.route_for("fail").is_some());
        assert!(kind.route_for("timeout").is_some());
    }

    #[test]
    fn plan_check_routing() {
        let kind = WorkflowStepKind::PlanCheck {
            agent_role: "architect".to_string(),
            check_phases: "next_3".to_string(),
            on_modification: Some(StepTarget::Single("replan".to_string())),
            on_no_change: Some(StepTarget::Single("continue".to_string())),
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        assert!(kind.route_for("modification").is_some());
        assert!(kind.route_for("no_change").is_some());
        assert!(kind.route_for("timeout").is_none());
    }

    #[test]
    fn sync_build_routing() {
        let kind = WorkflowStepKind::SyncBuild {
            steps: vec!["git_pull".to_string(), "cargo_build".to_string()],
            on_failure: Some(StepTarget::Single("escalate".to_string())),
            transitions: vec![],
            entry_actions: vec![],
        };
        assert!(kind.route_for("failure").is_some());
        assert!(kind.route_for("pass").is_none());
    }

    #[test]
    fn is_blocking_variants() {
        let review = WorkflowStepKind::AgentReview {
            role: "r".to_string(),
            draft: "latest".to_string(),
            timeout: None,
            on_apply: None,
            on_deny: None,
            on_escalate: None,
            on_plan_mod: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        assert!(review.is_blocking());

        let pr = WorkflowStepKind::PrMonitor {
            wait_for: PrWaitCondition::CiPass,
            timeout: None,
            on_pass: None,
            on_fail: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        assert!(pr.is_blocking());

        let gate = WorkflowStepKind::HumanGate {
            message: "continue?".to_string(),
            options: vec![],
            transitions: vec![],
            entry_actions: vec![],
        };
        assert!(gate.is_blocking());

        let build = WorkflowStepKind::SyncBuild {
            steps: vec![],
            on_failure: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        assert!(!build.is_blocking());
    }

    #[test]
    fn type_name_variants() {
        let build = WorkflowStepKind::SyncBuild {
            steps: vec![],
            on_failure: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        assert_eq!(build.type_name(), "sync_build");

        let gate = WorkflowStepKind::HumanGate {
            message: String::new(),
            options: vec![],
            transitions: vec![],
            entry_actions: vec![],
        };
        assert_eq!(gate.type_name(), "human_gate");
    }

    #[test]
    fn step_workflow_def_from_yaml() {
        let yaml = r#"
name: test-loop
initial_step: review
steps:
  review:
    type: agent_review
    role: reviewer
    timeout: 30m
    on_apply: build
    on_deny: review
    on_timeout: gate
  build:
    type: sync_build
    steps: [cargo_build]
    on_failure: gate
  gate:
    type: human_gate
    message: "Need human input"
    options: [apply, deny]
"#;
        let def = StepWorkflowDef::from_yaml(yaml).unwrap();
        assert_eq!(def.name, "test-loop");
        assert_eq!(def.initial_step, "review");
        assert_eq!(def.steps.len(), 3);
        assert!(def.steps.contains_key("review"));
        assert!(def.steps.contains_key("build"));
        assert!(def.steps.contains_key("gate"));
    }

    #[test]
    fn step_workflow_def_reachable_from_initial() {
        let yaml = r#"
name: test
initial_step: a
steps:
  a:
    type: sync_build
    steps: []
    on_failure: b
  b:
    type: human_gate
    message: "?"
    options: []
  unreachable:
    type: sync_build
    steps: []
"#;
        let def = StepWorkflowDef::from_yaml(yaml).unwrap();
        let reachable = def.reachable_from_initial();
        assert!(reachable.contains(&"a".to_string()));
        assert!(reachable.contains(&"b".to_string()));
        assert!(!reachable.contains(&"unreachable".to_string()));
    }

    #[test]
    fn workflow_step_kind_serde_round_trip() {
        let kind = WorkflowStepKind::PrMonitor {
            wait_for: PrWaitCondition::CiPass,
            timeout: Some("60m".to_string()),
            on_pass: Some(StepTarget::Single("merge".to_string())),
            on_fail: Some(StepTarget::Single("escalate".to_string())),
            on_timeout: Some(StepTarget::Single("escalate".to_string())),
            transitions: vec![],
            entry_actions: vec![],
        };
        let json = serde_json::to_string(&kind).unwrap();
        let restored: WorkflowStepKind = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.type_name(), "pr_monitor");
    }

    #[test]
    fn pr_wait_condition_display() {
        assert_eq!(PrWaitCondition::CiPass.to_string(), "ci_pass");
        assert_eq!(PrWaitCondition::Merged.to_string(), "merged");
        assert_eq!(PrWaitCondition::Closed.to_string(), "closed");
    }

    // ── New tests for v0.17.0.4.5 ────────────────────────────────────────────

    #[test]
    fn select_transition_legacy_on_apply() {
        let kind = WorkflowStepKind::AgentReview {
            role: "reviewer".to_string(),
            draft: "latest".to_string(),
            timeout: None,
            on_apply: Some(StepTarget::Single("build".to_string())),
            on_deny: None,
            on_escalate: None,
            on_plan_mod: None,
            on_timeout: None,
            transitions: vec![],
            entry_actions: vec![],
        };
        let ctx = WorkflowContext::new();
        let t = kind.select_transition("apply", &ctx).unwrap();
        assert_eq!(t.on, "apply");
        assert_eq!(t.to, Some(StepTarget::Single("build".to_string())));
        assert!(t.guard.is_none());
    }

    #[test]
    fn select_transition_new_style_guard_passes() {
        let guard = GuardExpr::parse("context.rework_count < 3").unwrap();
        let kind = WorkflowStepKind::HumanGate {
            message: String::new(),
            options: vec![],
            transitions: vec![Transition {
                on: "approve".to_string(),
                guard: Some(guard),
                actions: vec![],
                context_patch: serde_json::Map::new(),
                payload: serde_json::Value::Null,
                to: Some(StepTarget::Single("done".to_string())),
            }],
            entry_actions: vec![],
        };
        let mut ctx = WorkflowContext::new();
        ctx.fields
            .insert("rework_count".to_string(), serde_json::json!(2));
        let t = kind.select_transition("approve", &ctx).unwrap();
        assert_eq!(t.to, Some(StepTarget::Single("done".to_string())));
    }

    #[test]
    fn select_transition_guard_fails_returns_none() {
        let guard = GuardExpr::parse("context.rework_count < 3").unwrap();
        let kind = WorkflowStepKind::HumanGate {
            message: String::new(),
            options: vec![],
            transitions: vec![Transition {
                on: "approve".to_string(),
                guard: Some(guard),
                actions: vec![],
                context_patch: serde_json::Map::new(),
                payload: serde_json::Value::Null,
                to: Some(StepTarget::Single("done".to_string())),
            }],
            entry_actions: vec![],
        };
        let mut ctx = WorkflowContext::new();
        ctx.fields
            .insert("rework_count".to_string(), serde_json::json!(5));
        // Guard fails (5 is not < 3) → None
        assert!(kind.select_transition("approve", &ctx).is_none());
    }

    #[test]
    fn select_transition_first_matching_guard_wins() {
        let kind = WorkflowStepKind::HumanGate {
            message: String::new(),
            options: vec![],
            transitions: vec![
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
            entry_actions: vec![],
        };

        let mut ctx_low = WorkflowContext::new();
        ctx_low
            .fields
            .insert("rework_count".to_string(), serde_json::json!(2));
        let t_low = kind.select_transition("deny", &ctx_low).unwrap();
        assert_eq!(t_low.to, Some(StepTarget::Single("rework".to_string())));

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
    fn transitions_field_parsed_from_yaml() {
        let yaml = r#"
name: test
initial_step: review
steps:
  review:
    type: agent_review
    role: reviewer
    timeout: 30m
    transitions:
      - on: apply
        guard: "context.rework_count < 3"
        actions:
          - action_type: ta.apply_draft
            params:
              draft_id: "{{trigger.data.draft_id}}"
        to: build
      - on: deny
        to: rework
  build:
    type: sync_build
    steps: []
  rework:
    type: human_gate
    message: "Rework needed"
"#;
        let def = StepWorkflowDef::from_yaml(yaml).unwrap();
        let review = def.steps.get("review").unwrap();
        let trs = review.transitions();
        assert_eq!(trs.len(), 2);
        assert_eq!(trs[0].on, "apply");
        assert!(trs[0].guard.is_some());
        assert_eq!(trs[0].actions.len(), 1);
        assert_eq!(trs[0].actions[0].action_type, "ta.apply_draft");
        assert_eq!(trs[1].on, "deny");
        assert!(trs[1].guard.is_none());
    }

    #[test]
    fn routing_targets_includes_transition_targets() {
        let kind = WorkflowStepKind::HumanGate {
            message: String::new(),
            options: vec![],
            transitions: vec![
                Transition {
                    on: "apply".to_string(),
                    guard: None,
                    actions: vec![],
                    context_patch: serde_json::Map::new(),
                    payload: serde_json::Value::Null,
                    to: Some(StepTarget::Single("build".to_string())),
                },
                Transition {
                    on: "deny".to_string(),
                    guard: None,
                    actions: vec![],
                    context_patch: serde_json::Map::new(),
                    payload: serde_json::Value::Null,
                    to: Some(StepTarget::Single("rework".to_string())),
                },
            ],
            entry_actions: vec![],
        };
        let targets = kind.routing_targets();
        let ids: Vec<&str> = targets.iter().flat_map(|t| t.step_ids()).collect();
        assert!(ids.contains(&"build"));
        assert!(ids.contains(&"rework"));
    }
}
