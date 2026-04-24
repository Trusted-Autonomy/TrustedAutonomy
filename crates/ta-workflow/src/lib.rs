// ta-workflow — Pluggable workflow engine for Trusted Autonomy (v0.9.8.2 / v0.13.7).
//
// Provides the `WorkflowEngine` trait that all engines implement, plus:
//   - Built-in YAML engine for simple workflows
//   - Process-based plugin bridge for external engines (LangGraph, CrewAI)
//   - Verdict scoring and feedback routing
//   - Interactive human-in-the-loop interaction requests
//   - Serial phase chains with gate evaluation (v0.13.7)
//   - Parallel agent swarm coordination (v0.13.7)
//   - Multi-agent consensus review workflow (v0.15.15)
//   - Natural language → workflow intent resolver (v0.15.24)

pub mod artifact_dag;
pub mod artifact_store;
pub mod consensus;
pub mod definition;
pub mod error;
pub mod intent;
pub mod interaction;
pub mod params;
pub mod process_engine;
pub mod registry;
pub mod scorer;
pub mod serial_phases;
pub mod swarm;
pub mod trigger;
pub mod validate;
pub mod verdict;
pub mod work_plan;
pub mod yaml_engine;

pub use artifact_dag::{render_ascii, render_dot, MissingInput, ResolvedDag};
pub use artifact_store::{artifact_key, run_prefix, stage_prefix, ArtifactStore, StoredArtifact};
pub use consensus::{
    run_consensus, ConsensusAlgorithm, ConsensusInput, ConsensusResult, ReviewerVote,
};
pub use definition::{
    FailureRouting, RoleDefinition, StageDefinition, StageReview, WorkflowCatalog,
    WorkflowDefinition,
};
pub use intent::{
    extract_intent, format_confirmation_card, resolve_intent, ExtractedIntent, IntentVerb,
    ResolutionResult, ScopeModifier, TemplateCandidate, CONFIDENCE_THRESHOLD,
};
pub use params::{
    interpolate, version_prefix_from_id, ParamDecl, ParamValues, PlanContext, TemplateEntry,
    TemplateLibrary, TemplateSource,
};
pub use registry::{cached_index_path, RegistryEntry, RegistryIndex};
// Re-export ArtifactType from ta-changeset for callers that only depend on ta-workflow.
pub use error::WorkflowError;
pub use interaction::{AwaitHumanConfig, InteractionRequest, InteractionResponse};
pub use serial_phases::{
    evaluate_gates, run_gate, GateFailure, GateResult, SerialPhasesState, StepState, WorkflowGate,
};
pub use swarm::{IntegrationConfig, SubGoalSpec, SubGoalStatus, SwarmState};
pub use ta_changeset::ArtifactType;
pub use trigger::{TriggerCondition, TriggerConfig, TriggerWaitRecord};
pub use verdict::{Finding, Severity, Verdict, VerdictDecision};
pub use work_plan::{ImplementationStep, WorkPlan, WorkPlanDecision};
pub use yaml_engine::YamlWorkflowEngine;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a running workflow instance.
pub type WorkflowId = String;

/// Context passed to the next stage when a workflow proceeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalContext {
    /// Summary of the previous stage's output.
    pub previous_summary: Option<String>,
    /// Verdict findings from the previous stage (on route-back).
    pub feedback_findings: Vec<String>,
    /// IDs of goals whose output feeds into this stage.
    pub context_from: Vec<Uuid>,
}

/// Feedback context passed back to a stage when routing back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackContext {
    /// Synthesized feedback text for the next iteration.
    pub feedback: String,
    /// Aggregate score from verdict scoring (0.0-1.0).
    pub score: Option<f64>,
    /// Individual findings that led to the route-back.
    pub findings: Vec<Finding>,
}

/// Action decided by the workflow engine after a stage completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum StageAction {
    /// Proceed to the next stage.
    Proceed {
        next_stage: String,
        context: GoalContext,
    },
    /// Route back to a previous stage with feedback.
    RouteBack {
        target_stage: String,
        feedback: FeedbackContext,
        severity: Severity,
    },
    /// Workflow is complete.
    Complete,
    /// Await human input before proceeding.
    AwaitHuman { request: InteractionRequest },
}

/// Status of a running workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStatus {
    pub workflow_id: WorkflowId,
    pub name: String,
    pub current_stage: Option<String>,
    pub state: WorkflowState,
    pub stages_completed: Vec<String>,
    pub stages_remaining: Vec<String>,
    pub retry_counts: std::collections::HashMap<String, u32>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle state of a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Running,
    AwaitingHuman,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for WorkflowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowState::Running => write!(f, "running"),
            WorkflowState::AwaitingHuman => write!(f, "awaiting_human"),
            WorkflowState::Completed => write!(f, "completed"),
            WorkflowState::Failed => write!(f, "failed"),
            WorkflowState::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Core trait that all workflow engines implement.
///
/// TA mediates, doesn't mandate: the engine decides *how* to route
/// between stages. TA provides the governance wrapper (verdicts,
/// feedback scoring, human interaction).
pub trait WorkflowEngine: Send + Sync {
    /// Start a workflow from a definition. Returns a workflow ID.
    fn start(&mut self, def: &WorkflowDefinition) -> Result<WorkflowId, WorkflowError>;

    /// Notify the engine that a stage completed with verdicts.
    /// Returns the next action (proceed, route back, complete, or await human).
    fn stage_completed(
        &mut self,
        id: &str,
        stage: &str,
        verdicts: &[Verdict],
    ) -> Result<StageAction, WorkflowError>;

    /// Get the current status of a workflow.
    fn status(&self, id: &str) -> Result<WorkflowStatus, WorkflowError>;

    /// Inject human feedback into a paused workflow.
    fn inject_feedback(
        &mut self,
        id: &str,
        stage: &str,
        feedback: FeedbackContext,
    ) -> Result<(), WorkflowError>;

    /// Cancel a running workflow.
    fn cancel(&mut self, id: &str) -> Result<(), WorkflowError>;

    /// List all workflows (active and completed).
    fn list(&self) -> Vec<WorkflowStatus>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_action_serialization() {
        let action = StageAction::Proceed {
            next_stage: "review".to_string(),
            context: GoalContext {
                previous_summary: Some("Built the feature".to_string()),
                feedback_findings: vec![],
                context_from: vec![],
            },
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("\"action\":\"proceed\""));
        let restored: StageAction = serde_json::from_str(&json).unwrap();
        match restored {
            StageAction::Proceed { next_stage, .. } => assert_eq!(next_stage, "review"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn workflow_state_display() {
        assert_eq!(WorkflowState::Running.to_string(), "running");
        assert_eq!(WorkflowState::AwaitingHuman.to_string(), "awaiting_human");
        assert_eq!(WorkflowState::Completed.to_string(), "completed");
    }

    #[test]
    fn stage_action_route_back_serialization() {
        let action = StageAction::RouteBack {
            target_stage: "build".to_string(),
            feedback: FeedbackContext {
                feedback: "Fix the SQL injection".to_string(),
                score: Some(0.3),
                findings: vec![],
            },
            severity: Severity::Critical,
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("\"action\":\"route_back\""));
        assert!(json.contains("Fix the SQL injection"));
    }

    #[test]
    fn stage_action_await_human_serialization() {
        let action = StageAction::AwaitHuman {
            request: InteractionRequest {
                prompt: "Review needed".to_string(),
                context: serde_json::json!({"score": 0.5}),
                options: vec!["proceed".to_string(), "revise".to_string()],
                timeout_secs: Some(300),
            },
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("\"action\":\"await_human\""));
        assert!(json.contains("Review needed"));
    }
}
