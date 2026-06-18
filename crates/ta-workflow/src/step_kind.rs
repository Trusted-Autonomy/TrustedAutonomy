// step_kind.rs — WorkflowStepKind and step-based workflow types (v0.17.0.4).
//
// Five step types for the autonomous phase loop:
//   agent_review  — invoke advisor, route on AgentAction outcome
//   pr_monitor    — poll PR CI status, route on pass/fail/timeout
//   plan_check    — invoke architect to validate upcoming phases
//   sync_build    — run a sequence of local build commands
//   human_gate    — non-blocking escalation point in headless contexts
//
// Step-based workflows are distinct from stage-based WorkflowDefinition.
// They use a map of step IDs to WorkflowStepKind values, with `on_*` fields
// pointing to the next step ID on each outcome.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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

// ── WorkflowStepKind ──────────────────────────────────────────────────────────

/// Step type variants for step-based workflows (v0.17.0.4).
///
/// Parsed from YAML with `type: <variant>` as the discriminant.
/// Each `on_*` field routes to a step ID (or sequence of IDs) on a given outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowStepKind {
    /// Invoke an advisor agent to review the latest draft.
    ///
    /// Routes based on the advisor's `AgentAction` outcome.
    ///
    /// ```yaml
    /// type: agent_review
    /// role: reviewer
    /// draft: latest
    /// timeout: 30m
    /// on_apply: sync_build
    /// on_deny: rework
    /// on_escalate: human_gate
    /// on_plan_mod: plan_check
    /// ```
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
    },

    /// Poll a pull request's CI status and route on pass/fail/timeout.
    ///
    /// ```yaml
    /// type: pr_monitor
    /// wait_for: ci_pass
    /// timeout: 60m
    /// on_pass: [merge, sync_build_local]
    /// on_fail: [check_logs, escalate]
    /// on_timeout: escalate
    /// ```
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
    },

    /// Invoke an architect agent to review upcoming plan phases.
    ///
    /// ```yaml
    /// type: plan_check
    /// agent_role: architect
    /// check_phases: next_3
    /// on_modification: replan
    /// on_no_change: continue
    /// ```
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
    },

    /// Run a sequence of local build commands.
    ///
    /// Built-in step names: `git_pull`, `cargo_build`, `install_local`.
    /// Custom steps are passed through to the shell executor.
    ///
    /// ```yaml
    /// type: sync_build
    /// steps: [git_pull, cargo_build, install_local]
    /// on_failure: escalate
    /// ```
    SyncBuild {
        /// Ordered list of build step names to execute.
        #[serde(default)]
        steps: Vec<String>,
        /// Route when any step fails.
        #[serde(default)]
        on_failure: Option<StepTarget>,
    },

    /// Non-blocking escalation gate for headless/autonomous contexts.
    ///
    /// Interactive mode: prompts the human and routes on their choice.
    /// Headless mode: produces an `Escalate` action without blocking; the caller
    /// dispatches a notification and the loop suspends at this step.
    ///
    /// ```yaml
    /// type: human_gate
    /// message: "{{escalation.question}}"
    /// options: [apply, deny, modify]
    /// ```
    HumanGate {
        /// Question or message. Supports `{{escalation.question}}` interpolation.
        #[serde(default)]
        message: String,
        /// Available response options in interactive mode.
        #[serde(default)]
        options: Vec<String>,
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
    /// Return all routing targets defined on this step (for validation and reachability).
    pub fn routing_targets(&self) -> Vec<&StepTarget> {
        let mut targets = Vec::new();
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

    /// Route this step's output based on an action kind string.
    ///
    /// Returns the `StepTarget` for the given outcome, or `None` if no route is configured.
    ///
    /// Valid action kinds per step type:
    /// - `agent_review`:  "apply", "deny", "escalate", "plan_mod", "timeout"
    /// - `pr_monitor`:    "pass", "fail", "timeout"
    /// - `plan_check`:    "modification", "no_change", "timeout"
    /// - `sync_build`:    "failure"
    /// - `human_gate`:    (routing handled externally via options)
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
///
/// ```yaml
/// name: autonomous-phase-loop
/// initial_step: agent_review
/// steps:
///   agent_review:
///     type: agent_review
///     role: reviewer
///     timeout: 30m
///     on_apply: sync_build
///     on_deny: rework
///     on_escalate: human_gate
///   sync_build:
///     type: sync_build
///     steps: [git_pull, cargo_build]
///     on_failure: human_gate
///   human_gate:
///     type: human_gate
///     message: "{{escalation.question}}"
///     options: [apply, deny, modify]
/// ```
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
        };
        assert!(review.is_blocking());

        let pr = WorkflowStepKind::PrMonitor {
            wait_for: PrWaitCondition::CiPass,
            timeout: None,
            on_pass: None,
            on_fail: None,
            on_timeout: None,
        };
        assert!(pr.is_blocking());

        let gate = WorkflowStepKind::HumanGate {
            message: "continue?".to_string(),
            options: vec![],
        };
        assert!(gate.is_blocking());

        let build = WorkflowStepKind::SyncBuild {
            steps: vec![],
            on_failure: None,
        };
        assert!(!build.is_blocking());
    }

    #[test]
    fn type_name_variants() {
        let build = WorkflowStepKind::SyncBuild {
            steps: vec![],
            on_failure: None,
        };
        assert_eq!(build.type_name(), "sync_build");

        let gate = WorkflowStepKind::HumanGate {
            message: String::new(),
            options: vec![],
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
}
