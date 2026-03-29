// yaml_engine.rs — Built-in YAML workflow engine.
//
// Deliberately simple (~400 lines). Power users use LangGraph or CrewAI
// via the process engine. This engine handles:
//   - Topological sort of stage dependencies
//   - Sequential/parallel role execution within stages
//   - Verdict collection and scoring
//   - Retry routing with loop detection
//   - AwaitHuman interaction points

use std::collections::HashMap;

use chrono::Utc;
use uuid::Uuid;

use crate::definition::{VerdictConfig, WorkflowDefinition};
use crate::error::WorkflowError;
use crate::interaction::{AwaitHumanConfig, InteractionRequest};
use crate::scorer::score_verdicts;
use crate::verdict::Verdict;
use crate::{
    FeedbackContext, GoalContext, StageAction, WorkflowEngine, WorkflowId, WorkflowState,
    WorkflowStatus,
};

/// State for a single running workflow instance.
struct WorkflowInstance {
    id: WorkflowId,
    definition: WorkflowDefinition,
    state: WorkflowState,
    /// Topologically sorted stage execution order.
    stage_order: Vec<String>,
    /// Index into stage_order for the current stage.
    current_index: usize,
    /// Stages that have completed successfully.
    completed_stages: Vec<String>,
    /// Retry counts per stage.
    retry_counts: HashMap<String, u32>,
    started_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

/// Built-in YAML workflow engine.
pub struct YamlWorkflowEngine {
    workflows: HashMap<WorkflowId, WorkflowInstance>,
}

impl YamlWorkflowEngine {
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
        }
    }

    fn get_workflow(&self, id: &str) -> Result<&WorkflowInstance, WorkflowError> {
        self.workflows
            .get(id)
            .ok_or_else(|| WorkflowError::NotFound { id: id.to_string() })
    }

    fn get_workflow_mut(&mut self, id: &str) -> Result<&mut WorkflowInstance, WorkflowError> {
        self.workflows
            .get_mut(id)
            .ok_or_else(|| WorkflowError::NotFound { id: id.to_string() })
    }

    /// Build a WorkflowStatus from an instance.
    fn build_status(instance: &WorkflowInstance) -> WorkflowStatus {
        let current_stage = if instance.current_index < instance.stage_order.len() {
            Some(instance.stage_order[instance.current_index].clone())
        } else {
            None
        };
        let stages_remaining: Vec<String> = instance.stage_order
            [instance.current_index..instance.stage_order.len().min(instance.current_index + 10)]
            .to_vec();

        WorkflowStatus {
            workflow_id: instance.id.clone(),
            name: instance.definition.name.clone(),
            current_stage,
            state: instance.state.clone(),
            stages_completed: instance.completed_stages.clone(),
            stages_remaining,
            retry_counts: instance.retry_counts.clone(),
            started_at: instance.started_at,
            updated_at: instance.updated_at,
        }
    }

    /// Determine the StageAction based on verdicts, scoring, and stage config.
    fn evaluate_stage(
        instance: &mut WorkflowInstance,
        stage_name: &str,
        verdicts: &[Verdict],
    ) -> Result<StageAction, WorkflowError> {
        let stage_def = instance
            .definition
            .stages
            .iter()
            .find(|s| s.name == stage_name)
            .ok_or_else(|| WorkflowError::StageNotFound {
                workflow_id: instance.id.clone(),
                stage: stage_name.to_string(),
            })?
            .clone();

        // Score verdicts against the workflow's verdict config.
        let default_config = VerdictConfig {
            scorer: None,
            pass_threshold: 0.7,
            required_pass: vec![],
        };
        let verdict_config = instance
            .definition
            .verdict
            .as_ref()
            .unwrap_or(&default_config);
        let failure_route = stage_def.on_fail.as_ref().map(|f| f.route_to.as_str());
        let scoring = score_verdicts(verdicts, verdict_config, failure_route);

        instance.updated_at = Utc::now();

        if scoring.passes {
            // Check await_human config.
            if stage_def.await_human == AwaitHumanConfig::Always {
                instance.state = WorkflowState::AwaitingHuman;
                return Ok(StageAction::AwaitHuman {
                    request: InteractionRequest {
                        prompt: format!(
                            "Stage '{}' completed (score: {:.2}). Review before proceeding.",
                            stage_name, scoring.score
                        ),
                        context: serde_json::json!({
                            "stage": stage_name,
                            "score": scoring.score,
                            "findings_count": scoring.findings.len(),
                        }),
                        options: vec![
                            "proceed".to_string(),
                            "revise".to_string(),
                            "cancel".to_string(),
                        ],
                        timeout_secs: None,
                    },
                });
            }

            // Mark stage complete and advance.
            instance.completed_stages.push(stage_name.to_string());
            instance.current_index += 1;

            if instance.current_index >= instance.stage_order.len() {
                instance.state = WorkflowState::Completed;
                return Ok(StageAction::Complete);
            }

            let next = instance.stage_order[instance.current_index].clone();
            Ok(StageAction::Proceed {
                next_stage: next,
                context: GoalContext {
                    previous_summary: Some(format!(
                        "Stage '{}' passed with score {:.2}",
                        stage_name, scoring.score
                    )),
                    feedback_findings: vec![],
                    context_from: vec![],
                },
            })
        } else {
            // Check await_human on failure.
            if stage_def.await_human == AwaitHumanConfig::OnFail {
                instance.state = WorkflowState::AwaitingHuman;
                return Ok(StageAction::AwaitHuman {
                    request: InteractionRequest {
                        prompt: format!(
                            "Stage '{}' failed review (score: {:.2}). {} findings need attention.",
                            stage_name,
                            scoring.score,
                            scoring.findings.len()
                        ),
                        context: serde_json::json!({
                            "stage": stage_name,
                            "score": scoring.score,
                            "severity": scoring.severity.to_string(),
                            "findings": scoring.findings.iter().map(|f| {
                                serde_json::json!({
                                    "title": f.title,
                                    "severity": f.severity.to_string(),
                                    "category": f.category,
                                })
                            }).collect::<Vec<_>>(),
                        }),
                        options: vec![
                            "proceed".to_string(),
                            "revise".to_string(),
                            "cancel".to_string(),
                        ],
                        timeout_secs: None,
                    },
                });
            }

            // Route back if configured.
            if let Some(routing) = &stage_def.on_fail {
                let count = instance
                    .retry_counts
                    .entry(stage_name.to_string())
                    .or_insert(0);
                *count += 1;

                if *count > routing.max_retries {
                    instance.state = WorkflowState::Failed;
                    return Err(WorkflowError::MaxRetriesExceeded {
                        workflow_id: instance.id.clone(),
                        stage: stage_name.to_string(),
                        max: routing.max_retries,
                    });
                }

                // Find the target stage index.
                let target_idx = instance
                    .stage_order
                    .iter()
                    .position(|s| s == &routing.route_to);
                if let Some(idx) = target_idx {
                    instance.current_index = idx;
                }

                return Ok(StageAction::RouteBack {
                    target_stage: routing.route_to.clone(),
                    feedback: FeedbackContext {
                        feedback: scoring.feedback,
                        score: Some(scoring.score),
                        findings: scoring.findings,
                    },
                    severity: scoring.severity,
                });
            }

            // No routing configured — fail the workflow.
            instance.state = WorkflowState::Failed;
            Err(WorkflowError::Other(format!(
                "Stage '{}' failed with score {:.2} and no failure routing configured",
                stage_name, scoring.score
            )))
        }
    }
}

impl Default for YamlWorkflowEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowEngine for YamlWorkflowEngine {
    fn start(&mut self, def: &WorkflowDefinition) -> Result<WorkflowId, WorkflowError> {
        let stage_order = def.stage_order()?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();

        let instance = WorkflowInstance {
            id: id.clone(),
            definition: def.clone(),
            state: WorkflowState::Running,
            stage_order,
            current_index: 0,
            completed_stages: Vec::new(),
            retry_counts: HashMap::new(),
            started_at: now,
            updated_at: now,
        };
        self.workflows.insert(id.clone(), instance);

        tracing::info!(workflow_id = %id, name = %def.name, stages = def.stages.len(), "workflow started");
        Ok(id)
    }

    fn stage_completed(
        &mut self,
        id: &str,
        stage: &str,
        verdicts: &[Verdict],
    ) -> Result<StageAction, WorkflowError> {
        let instance = self.get_workflow_mut(id)?;
        if instance.state != WorkflowState::Running
            && instance.state != WorkflowState::AwaitingHuman
        {
            return Err(WorkflowError::InvalidState {
                id: id.to_string(),
                state: instance.state.to_string(),
            });
        }
        instance.state = WorkflowState::Running;
        Self::evaluate_stage(instance, stage, verdicts)
    }

    fn status(&self, id: &str) -> Result<WorkflowStatus, WorkflowError> {
        let instance = self.get_workflow(id)?;
        Ok(Self::build_status(instance))
    }

    fn inject_feedback(
        &mut self,
        id: &str,
        _stage: &str,
        _feedback: FeedbackContext,
    ) -> Result<(), WorkflowError> {
        let instance = self.get_workflow_mut(id)?;
        if instance.state != WorkflowState::AwaitingHuman {
            return Err(WorkflowError::InvalidState {
                id: id.to_string(),
                state: instance.state.to_string(),
            });
        }
        instance.state = WorkflowState::Running;
        instance.updated_at = Utc::now();
        tracing::info!(workflow_id = %id, "human feedback injected, resuming workflow");
        Ok(())
    }

    fn cancel(&mut self, id: &str) -> Result<(), WorkflowError> {
        let instance = self.get_workflow_mut(id)?;
        instance.state = WorkflowState::Cancelled;
        instance.updated_at = Utc::now();
        tracing::info!(workflow_id = %id, "workflow cancelled");
        Ok(())
    }

    fn list(&self) -> Vec<WorkflowStatus> {
        self.workflows.values().map(Self::build_status).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{FailureRouting, StageDefinition};
    use crate::interaction::AwaitHumanConfig;
    use crate::verdict::{Finding, Severity, VerdictDecision};

    fn simple_workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            name: "test-workflow".to_string(),
            stages: vec![
                StageDefinition {
                    name: "build".to_string(),
                    depends_on: vec![],
                    roles: vec!["engineer".to_string()],
                    then: vec![],
                    review: None,
                    on_fail: None,
                    await_human: AwaitHumanConfig::Never,
                    inputs: vec![],
                    outputs: vec![],
                },
                StageDefinition {
                    name: "review".to_string(),
                    depends_on: vec!["build".to_string()],
                    roles: vec!["reviewer".to_string()],
                    then: vec![],
                    review: None,
                    on_fail: Some(FailureRouting {
                        route_to: "build".to_string(),
                        max_retries: 2,
                    }),
                    await_human: AwaitHumanConfig::Never,
                    inputs: vec![],
                    outputs: vec![],
                },
            ],
            roles: HashMap::new(),
            verdict: None,
            agent_framework: None,
        }
    }

    #[test]
    fn start_workflow() {
        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&simple_workflow()).unwrap();
        let status = engine.status(&id).unwrap();
        assert_eq!(status.state, WorkflowState::Running);
        assert_eq!(status.current_stage, Some("build".to_string()));
        assert_eq!(status.stages_remaining, vec!["build", "review"]);
    }

    #[test]
    fn stage_proceed_on_pass() {
        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&simple_workflow()).unwrap();

        let verdicts = vec![Verdict {
            role: "engineer".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        let action = engine.stage_completed(&id, "build", &verdicts).unwrap();
        match action {
            StageAction::Proceed { next_stage, .. } => assert_eq!(next_stage, "review"),
            _ => panic!("expected Proceed, got {:?}", action),
        }
    }

    #[test]
    fn workflow_completes() {
        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&simple_workflow()).unwrap();

        let pass = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        engine.stage_completed(&id, "build", &pass).unwrap();
        let action = engine.stage_completed(&id, "review", &pass).unwrap();
        assert!(matches!(action, StageAction::Complete));

        let status = engine.status(&id).unwrap();
        assert_eq!(status.state, WorkflowState::Completed);
    }

    #[test]
    fn route_back_on_failure() {
        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&simple_workflow()).unwrap();

        let pass = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        engine.stage_completed(&id, "build", &pass).unwrap();

        let fail = vec![Verdict {
            role: "reviewer".to_string(),
            decision: VerdictDecision::Fail,
            severity: Some(Severity::Major),
            findings: vec![Finding {
                title: "Bug".to_string(),
                description: "There's a bug".to_string(),
                severity: Severity::Major,
                category: None,
            }],
        }];
        let action = engine.stage_completed(&id, "review", &fail).unwrap();
        match action {
            StageAction::RouteBack {
                target_stage,
                severity,
                ..
            } => {
                assert_eq!(target_stage, "build");
                assert_eq!(severity, Severity::Major);
            }
            _ => panic!("expected RouteBack"),
        }
    }

    #[test]
    fn max_retries_exceeded() {
        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&simple_workflow()).unwrap();

        let pass = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        let fail = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Fail,
            severity: Some(Severity::Minor),
            findings: vec![],
        }];

        // Build passes, review fails, route back to build.
        engine.stage_completed(&id, "build", &pass).unwrap();
        engine.stage_completed(&id, "review", &fail).unwrap();

        // Retry 1: build passes, review fails again.
        engine.stage_completed(&id, "build", &pass).unwrap();
        engine.stage_completed(&id, "review", &fail).unwrap();

        // Retry 2: build passes, review fails — should exceed max_retries=2.
        engine.stage_completed(&id, "build", &pass).unwrap();
        let result = engine.stage_completed(&id, "review", &fail);
        assert!(matches!(
            result,
            Err(WorkflowError::MaxRetriesExceeded { .. })
        ));
    }

    #[test]
    fn cancel_workflow() {
        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&simple_workflow()).unwrap();
        engine.cancel(&id).unwrap();
        let status = engine.status(&id).unwrap();
        assert_eq!(status.state, WorkflowState::Cancelled);
    }

    #[test]
    fn list_workflows() {
        let mut engine = YamlWorkflowEngine::new();
        engine.start(&simple_workflow()).unwrap();
        engine.start(&simple_workflow()).unwrap();
        let list = engine.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn not_found_error() {
        let engine = YamlWorkflowEngine::new();
        let result = engine.status("nonexistent");
        assert!(matches!(result, Err(WorkflowError::NotFound { .. })));
    }

    #[test]
    fn await_human_always() {
        let mut def = simple_workflow();
        def.stages[0].await_human = AwaitHumanConfig::Always;

        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&def).unwrap();

        let pass = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        let action = engine.stage_completed(&id, "build", &pass).unwrap();
        assert!(matches!(action, StageAction::AwaitHuman { .. }));

        let status = engine.status(&id).unwrap();
        assert_eq!(status.state, WorkflowState::AwaitingHuman);
    }

    #[test]
    fn await_human_on_fail_only_fires_on_failure() {
        let mut def = simple_workflow();
        def.stages[0].await_human = AwaitHumanConfig::OnFail;
        def.stages[0].on_fail = Some(FailureRouting {
            route_to: "build".to_string(),
            max_retries: 3,
        });

        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&def).unwrap();

        // Pass: should NOT await human.
        let pass = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        let action = engine.stage_completed(&id, "build", &pass).unwrap();
        assert!(matches!(action, StageAction::Proceed { .. }));
    }

    #[test]
    fn await_human_on_fail_fires_when_failing() {
        let mut def = simple_workflow();
        def.stages[1].await_human = AwaitHumanConfig::OnFail;

        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&def).unwrap();

        let pass = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        engine.stage_completed(&id, "build", &pass).unwrap();

        // Fail: should await human.
        let fail = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Fail,
            severity: Some(Severity::Major),
            findings: vec![],
        }];
        let action = engine.stage_completed(&id, "review", &fail).unwrap();
        assert!(matches!(action, StageAction::AwaitHuman { .. }));
    }

    #[test]
    fn inject_feedback_resumes_workflow() {
        let mut def = simple_workflow();
        def.stages[0].await_human = AwaitHumanConfig::Always;

        let mut engine = YamlWorkflowEngine::new();
        let id = engine.start(&def).unwrap();

        let pass = vec![Verdict {
            role: "x".to_string(),
            decision: VerdictDecision::Pass,
            severity: None,
            findings: vec![],
        }];
        engine.stage_completed(&id, "build", &pass).unwrap();

        // Inject feedback to resume.
        engine
            .inject_feedback(
                &id,
                "build",
                FeedbackContext {
                    feedback: "Looks good".to_string(),
                    score: Some(0.9),
                    findings: vec![],
                },
            )
            .unwrap();

        let status = engine.status(&id).unwrap();
        assert_eq!(status.state, WorkflowState::Running);
    }
}
