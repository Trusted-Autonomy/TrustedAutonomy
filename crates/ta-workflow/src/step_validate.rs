// step_validate.rs — DAG validation for step-based workflows (v0.17.0.4).
//
// Validates a `StepWorkflowDef`, checking:
//   1. `initial_step` exists in the steps map
//   2. All `on_*` routing targets reference defined steps
//   3. No static cycles reachable from `initial_step`
//   4. Blocking steps (agent_review, pr_monitor, human_gate) have `on_timeout` configured
//   5. Unreachable steps (warnings only)

use std::collections::HashSet;

use crate::step_kind::StepWorkflowDef;
use crate::validate::{ValidationFinding, ValidationResult, ValidationSeverity};

/// Validate a step-based workflow definition.
///
/// Returns a `ValidationResult` with errors and warnings. A workflow with errors
/// should not be started — fix the errors before running.
///
/// # Checks
///
/// | Level   | Check |
/// |---------|-------|
/// | Error   | `initial_step` not in steps map |
/// | Error   | `on_*` target references undefined step |
/// | Error   | `on_*` target sequence contains undefined step |
/// | Error   | Static cycle reachable from `initial_step` |
/// | Warning | Blocking step missing `on_timeout` |
/// | Warning | Step not reachable from `initial_step` |
pub fn validate_step_workflow(def: &StepWorkflowDef) -> ValidationResult {
    let mut findings = Vec::new();
    let step_ids: HashSet<&str> = def.steps.keys().map(|s| s.as_str()).collect();

    // 1. initial_step must exist.
    if !step_ids.contains(def.initial_step.as_str()) {
        findings.push(ValidationFinding {
            severity: ValidationSeverity::Error,
            location: "initial_step".to_string(),
            message: format!(
                "initial_step '{}' is not defined in the steps map.",
                def.initial_step
            ),
            suggestion: Some(format!(
                "Add a step with id '{}' or change initial_step to one of: {}",
                def.initial_step,
                step_ids.iter().copied().collect::<Vec<_>>().join(", ")
            )),
        });
    }

    // 2. All on_* routing targets must reference defined steps.
    for (step_id, kind) in &def.steps {
        for target in kind.routing_targets() {
            for referenced_id in target.step_ids() {
                if !step_ids.contains(referenced_id) {
                    findings.push(ValidationFinding {
                        severity: ValidationSeverity::Error,
                        location: format!("steps.{}", step_id),
                        message: format!(
                            "Step '{}' routes to undefined step '{}'.",
                            step_id, referenced_id
                        ),
                        suggestion: Some(format!(
                            "Add a step with id '{}' or change the routing target.",
                            referenced_id
                        )),
                    });
                }
            }
        }
    }

    // 3. Detect static cycles reachable from initial_step (DFS with path tracking).
    if step_ids.contains(def.initial_step.as_str()) {
        if let Some(cycle) = detect_cycle(def) {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Error,
                location: "steps".to_string(),
                message: format!("Cycle detected in step routing: {}", cycle.join(" → ")),
                suggestion: Some(
                    "Break the cycle by introducing a human_gate or terminal step.".to_string(),
                ),
            });
        }
    }

    // 4. Blocking steps should configure on_timeout.
    for (step_id, kind) in &def.steps {
        if kind.is_blocking() && kind.route_for("timeout").is_none() {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Warning,
                location: format!("steps.{}", step_id),
                message: format!(
                    "Blocking step '{}' (type: {}) has no on_timeout configured. \
                     If the step hangs indefinitely, the workflow loop will stall.",
                    step_id,
                    kind.type_name()
                ),
                suggestion: Some(format!(
                    "Add on_timeout: <step_id> to 'steps.{}' to handle timeout escalation.",
                    step_id
                )),
            });
        }
    }

    // 5. Warn on unreachable steps.
    let reachable = def.reachable_from_initial();
    for step_id in def.steps.keys() {
        if !reachable.contains(step_id) {
            findings.push(ValidationFinding {
                severity: ValidationSeverity::Warning,
                location: format!("steps.{}", step_id),
                message: format!(
                    "Step '{}' is not reachable from initial_step '{}'.",
                    step_id, def.initial_step
                ),
                suggestion: Some(
                    "Remove the step or add a routing rule that leads to it.".to_string(),
                ),
            });
        }
    }

    ValidationResult { findings }
}

/// Detect a cycle in the step routing graph via DFS from initial_step.
///
/// Returns `Some(cycle_path)` if a cycle is found, `None` otherwise.
/// The path includes the repeated step at both ends (e.g., `["a", "b", "a"]`).
fn detect_cycle(def: &StepWorkflowDef) -> Option<Vec<String>> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut path: Vec<String> = Vec::new();

    fn dfs(
        current: &str,
        def: &StepWorkflowDef,
        visited: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if path.contains(&current.to_string()) {
            // Found a cycle: report from the repeated node.
            let cycle_start = path.iter().position(|s| s == current).unwrap_or(0);
            let mut cycle = path[cycle_start..].to_vec();
            cycle.push(current.to_string());
            return Some(cycle);
        }
        if visited.contains(current) {
            return None; // Already fully explored — no cycle through this node.
        }

        path.push(current.to_string());

        if let Some(kind) = def.steps.get(current) {
            for target in kind.routing_targets() {
                for next_id in target.step_ids() {
                    if let Some(cycle) = dfs(next_id, def, visited, path) {
                        return Some(cycle);
                    }
                }
            }
        }

        path.pop();
        visited.insert(current.to_string());
        None
    }

    dfs(&def.initial_step, def, &mut visited, &mut path)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_kind::{PrWaitCondition, StepTarget, StepWorkflowDef, WorkflowStepKind};
    use std::collections::HashMap;

    fn make_def(steps: HashMap<String, WorkflowStepKind>, initial: &str) -> StepWorkflowDef {
        StepWorkflowDef {
            name: "test".to_string(),
            initial_step: initial.to_string(),
            steps,
        }
    }

    fn linear_workflow() -> StepWorkflowDef {
        let mut steps = HashMap::new();
        steps.insert(
            "review".to_string(),
            WorkflowStepKind::AgentReview {
                role: "reviewer".to_string(),
                draft: "latest".to_string(),
                timeout: Some("30m".to_string()),
                on_apply: Some(StepTarget::Single("build".to_string())),
                on_deny: None,
                on_escalate: None,
                on_plan_mod: None,
                on_timeout: Some(StepTarget::Single("gate".to_string())),
            },
        );
        steps.insert(
            "build".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec!["cargo_build".to_string()],
                on_failure: None,
            },
        );
        steps.insert(
            "gate".to_string(),
            WorkflowStepKind::HumanGate {
                message: "Review needed".to_string(),
                options: vec!["apply".to_string(), "deny".to_string()],
            },
        );
        make_def(steps, "review")
    }

    // ── Valid workflow ───────────────────────────────────────────────────────

    #[test]
    fn valid_workflow_no_errors() {
        let def = linear_workflow();
        let result = validate_step_workflow(&def);
        assert!(
            !result.has_errors(),
            "expected no errors, got: {:?}",
            result.findings
        );
    }

    // human_gate is blocking but has no timeout field — warns but doesn't error.
    #[test]
    fn valid_workflow_warns_on_blocking_step_without_timeout() {
        let def = linear_workflow(); // "gate" step is HumanGate with no on_timeout
        let result = validate_step_workflow(&def);
        assert!(!result.has_errors());
        // gate step should warn
        let gate_warn = result
            .findings
            .iter()
            .any(|f| f.location.contains("gate") && f.message.contains("on_timeout"));
        assert!(gate_warn, "expected timeout warning for gate step");
    }

    // ── Missing initial_step ─────────────────────────────────────────────────

    #[test]
    fn missing_initial_step_is_error() {
        let mut steps = HashMap::new();
        steps.insert(
            "build".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: None,
            },
        );
        let def = make_def(steps, "nonexistent");
        let result = validate_step_workflow(&def);
        assert!(result.has_errors());
        assert!(result
            .findings
            .iter()
            .any(|f| f.location == "initial_step" && f.message.contains("nonexistent")));
    }

    // ── Undefined on_* targets ───────────────────────────────────────────────

    #[test]
    fn undefined_on_apply_target_is_error() {
        let mut steps = HashMap::new();
        steps.insert(
            "review".to_string(),
            WorkflowStepKind::AgentReview {
                role: "reviewer".to_string(),
                draft: "latest".to_string(),
                timeout: Some("30m".to_string()),
                on_apply: Some(StepTarget::Single("phantom_step".to_string())),
                on_deny: None,
                on_escalate: None,
                on_plan_mod: None,
                on_timeout: None,
            },
        );
        let def = make_def(steps, "review");
        let result = validate_step_workflow(&def);
        assert!(result.has_errors());
        assert!(result
            .findings
            .iter()
            .any(|f| f.message.contains("phantom_step")));
    }

    #[test]
    fn undefined_step_in_sequence_target_is_error() {
        let mut steps = HashMap::new();
        steps.insert(
            "monitor".to_string(),
            WorkflowStepKind::PrMonitor {
                wait_for: PrWaitCondition::CiPass,
                timeout: Some("60m".to_string()),
                on_pass: Some(StepTarget::Sequence(vec![
                    "merge".to_string(),      // defined below
                    "ghost_step".to_string(), // NOT defined
                ])),
                on_fail: None,
                on_timeout: None,
            },
        );
        steps.insert(
            "merge".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: None,
            },
        );
        let def = make_def(steps, "monitor");
        let result = validate_step_workflow(&def);
        assert!(result.has_errors());
        assert!(result
            .findings
            .iter()
            .any(|f| f.message.contains("ghost_step")));
    }

    // ── Cycle detection ──────────────────────────────────────────────────────

    #[test]
    fn dag_validator_rejects_simple_cycle() {
        let mut steps = HashMap::new();
        steps.insert(
            "a".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: Some(StepTarget::Single("b".to_string())),
            },
        );
        steps.insert(
            "b".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: Some(StepTarget::Single("a".to_string())),
            },
        );
        let def = make_def(steps, "a");
        let result = validate_step_workflow(&def);
        assert!(result.has_errors());
        assert!(result
            .findings
            .iter()
            .any(|f| f.message.to_lowercase().contains("cycle")));
    }

    #[test]
    fn dag_validator_rejects_self_loop() {
        let mut steps = HashMap::new();
        steps.insert(
            "loop_forever".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: Some(StepTarget::Single("loop_forever".to_string())),
            },
        );
        let def = make_def(steps, "loop_forever");
        let result = validate_step_workflow(&def);
        assert!(result.has_errors());
        assert!(result
            .findings
            .iter()
            .any(|f| f.message.contains("cycle") || f.message.contains("Cycle")));
    }

    #[test]
    fn dag_validator_allows_linear_chain() {
        let mut steps = HashMap::new();
        steps.insert(
            "step1".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: Some(StepTarget::Single("step2".to_string())),
            },
        );
        steps.insert(
            "step2".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: None,
            },
        );
        let def = make_def(steps, "step1");
        let result = validate_step_workflow(&def);
        let cycle_errors: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.severity == ValidationSeverity::Error && f.message.contains("ycle"))
            .collect();
        assert!(
            cycle_errors.is_empty(),
            "linear chain should not have cycle errors"
        );
    }

    // ── Blocking step timeout warnings ───────────────────────────────────────

    #[test]
    fn agent_review_with_on_timeout_no_warning() {
        let mut steps = HashMap::new();
        steps.insert(
            "review".to_string(),
            WorkflowStepKind::AgentReview {
                role: "reviewer".to_string(),
                draft: "latest".to_string(),
                timeout: Some("30m".to_string()),
                on_apply: None,
                on_deny: None,
                on_escalate: None,
                on_plan_mod: None,
                on_timeout: Some(StepTarget::Single("gate".to_string())),
            },
        );
        steps.insert(
            "gate".to_string(),
            WorkflowStepKind::HumanGate {
                message: String::new(),
                options: vec![],
            },
        );
        let def = make_def(steps, "review");
        let result = validate_step_workflow(&def);
        let timeout_warnings: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.location.contains("review") && f.message.contains("on_timeout"))
            .collect();
        assert!(
            timeout_warnings.is_empty(),
            "review step has on_timeout — no warning expected"
        );
    }

    #[test]
    fn pr_monitor_missing_on_timeout_warns() {
        let mut steps = HashMap::new();
        steps.insert(
            "monitor".to_string(),
            WorkflowStepKind::PrMonitor {
                wait_for: PrWaitCondition::CiPass,
                timeout: None,
                on_pass: None,
                on_fail: None,
                on_timeout: None,
            },
        );
        let def = make_def(steps, "monitor");
        let result = validate_step_workflow(&def);
        assert!(!result.has_errors()); // undefined refs are warnings, not errors in this case
        let warn = result
            .findings
            .iter()
            .any(|f| f.severity == ValidationSeverity::Warning && f.message.contains("on_timeout"));
        assert!(warn, "expected on_timeout warning for pr_monitor");
    }

    // ── Unreachable steps ────────────────────────────────────────────────────

    #[test]
    fn unreachable_step_warns() {
        let mut steps = HashMap::new();
        steps.insert(
            "start".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: None,
            },
        );
        steps.insert(
            "dead_end".to_string(),
            WorkflowStepKind::SyncBuild {
                steps: vec![],
                on_failure: None,
            },
        );
        let def = make_def(steps, "start");
        let result = validate_step_workflow(&def);
        assert!(!result.has_errors());
        let unreachable_warn = result
            .findings
            .iter()
            .any(|f| f.location.contains("dead_end") && f.message.contains("reachable"));
        assert!(
            unreachable_warn,
            "expected unreachable warning for dead_end"
        );
    }
}
