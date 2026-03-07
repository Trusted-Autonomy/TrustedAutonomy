// goal_run.rs — GoalRun: the top-level execution lifecycle unit.
//
// A GoalRun ties together everything needed for one unit of agent work:
// - An agent identity and capability manifest
// - A staging workspace for file changes
// - A set of ChangeSets produced by the agent
// - An eventual PR package for human review
//
// The state machine enforces a valid lifecycle:
//   Created → Configured → Running → PrReady → UnderReview
//     → Approved → Applied → Completed
//   (or Failed from any state)

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::GoalError;

/// The lifecycle state of a GoalRun.
///
/// In Rust, enums can carry data per variant (like `Failed { reason }`).
/// The `#[serde(tag = "state")]` attribute makes this serialize as
/// `{"state": "running"}` in JSON — clean and readable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GoalRunState {
    /// Just created, not yet configured with manifest/workspace.
    Created,

    /// Manifest issued and workspace allocated — ready for the agent.
    Configured,

    /// Agent is actively working (making tool calls).
    Running,

    /// Agent has built a PR package — awaiting review.
    PrReady,

    /// A reviewer is looking at the PR package.
    UnderReview,

    /// The PR package has been approved.
    Approved { approved_by: String },

    /// Approved changes have been applied to the target.
    Applied,

    /// Goal completed successfully.
    Completed,

    /// Goal failed at some point.
    Failed { reason: String },
}

impl fmt::Display for GoalRunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GoalRunState::Created => write!(f, "created"),
            GoalRunState::Configured => write!(f, "configured"),
            GoalRunState::Running => write!(f, "running"),
            GoalRunState::PrReady => write!(f, "pr_ready"),
            GoalRunState::UnderReview => write!(f, "under_review"),
            GoalRunState::Approved { .. } => write!(f, "approved"),
            GoalRunState::Applied => write!(f, "applied"),
            GoalRunState::Completed => write!(f, "completed"),
            GoalRunState::Failed { .. } => write!(f, "failed"),
        }
    }
}

impl GoalRunState {
    /// Check whether transitioning from this state to `next` is valid.
    ///
    /// The valid transitions form a directed graph:
    ///   Created → Configured → Running → PrReady → UnderReview
    ///     → Approved → Applied → Completed
    ///   Any state → Failed (always valid — things can break anywhere)
    pub fn can_transition_to(&self, next: &GoalRunState) -> bool {
        // Transition to Failed is always allowed.
        if matches!(next, GoalRunState::Failed { .. }) {
            return true;
        }

        matches!(
            (self, next),
            (GoalRunState::Created, GoalRunState::Configured)
                | (GoalRunState::Configured, GoalRunState::Running)
                | (GoalRunState::Running, GoalRunState::PrReady)
                | (GoalRunState::PrReady, GoalRunState::UnderReview)
                | (GoalRunState::UnderReview, GoalRunState::Approved { .. })
                | (GoalRunState::Approved { .. }, GoalRunState::Applied)
                | (GoalRunState::Applied, GoalRunState::Completed)
                // Allow going back from UnderReview to Running (denied PR, try again)
                | (GoalRunState::UnderReview, GoalRunState::Running)
                // Macro goals: allow PrReady → Running for inner-loop iteration.
                // Agent submits a sub-goal draft, then continues working on the next one.
                | (GoalRunState::PrReady, GoalRunState::Running)
        )
    }
}

/// A GoalRun — one unit of agent work from start to completion.
///
/// This is the top-level execution unit introduced by the Plan Revision doc.
/// It replaces ad-hoc goal tracking with a formal lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalRun {
    /// Unique identifier for this goal run.
    pub goal_run_id: Uuid,

    /// Human-readable title (e.g., "Fix authentication bug").
    pub title: String,

    /// Detailed objective describing what needs to be accomplished.
    pub objective: String,

    /// The agent working on this goal.
    pub agent_id: String,

    /// Current lifecycle state.
    pub state: GoalRunState,

    /// The capability manifest issued for this goal run.
    pub manifest_id: Uuid,

    /// Path to the staging workspace directory.
    pub workspace_path: PathBuf,

    /// Path to the change store directory.
    pub store_path: PathBuf,

    /// Path to the original source project (for overlay-based goals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<PathBuf>,

    /// Optional plan phase this goal is working on (e.g., "4b").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_phase: Option<String>,

    /// Parent goal ID for follow-up goals (enables iterative refinement).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_goal_id: Option<Uuid>,

    /// Source file snapshot taken at goal start (for conflict detection).
    /// Serialized as embedded JSON — allows concurrent session conflict detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<serde_json::Value>,

    /// Whether this is a macro goal (supports inner-loop iteration with sub-goals).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_macro: bool,

    /// Parent macro goal ID for sub-goals created during a macro session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_macro_id: Option<Uuid>,

    /// IDs of sub-goals created within this macro goal session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_goal_ids: Vec<Uuid>,

    /// Workflow ID this goal belongs to (v0.9.8.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,

    /// Workflow stage this goal belongs to (v0.9.8.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,

    /// Workflow role this goal fulfills (v0.9.8.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// Goals whose output feeds into this one's context (v0.9.8.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_from: Vec<Uuid>,

    /// The PR package ID, if one has been built.
    pub pr_package_id: Option<Uuid>,

    /// When this goal run was created.
    pub created_at: DateTime<Utc>,

    /// When this goal run was last updated.
    pub updated_at: DateTime<Utc>,
}

impl GoalRun {
    /// Create a new GoalRun in the Created state.
    pub fn new(
        title: impl Into<String>,
        objective: impl Into<String>,
        agent_id: impl Into<String>,
        workspace_path: PathBuf,
        store_path: PathBuf,
    ) -> Self {
        let now = Utc::now();
        Self {
            goal_run_id: Uuid::new_v4(),
            title: title.into(),
            objective: objective.into(),
            agent_id: agent_id.into(),
            state: GoalRunState::Created,
            manifest_id: Uuid::new_v4(),
            workspace_path,
            store_path,
            source_dir: None,
            plan_phase: None,
            parent_goal_id: None,
            source_snapshot: None,
            is_macro: false,
            parent_macro_id: None,
            sub_goal_ids: Vec::new(),
            workflow_id: None,
            stage: None,
            role: None,
            context_from: Vec::new(),
            pr_package_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Transition to a new state. Returns an error if the transition is invalid.
    pub fn transition(&mut self, new_state: GoalRunState) -> Result<(), GoalError> {
        if !self.state.can_transition_to(&new_state) {
            return Err(GoalError::InvalidTransition {
                goal_run_id: self.goal_run_id,
                from: self.state.to_string(),
                to: new_state.to_string(),
            });
        }
        self.state = new_state;
        self.updated_at = Utc::now();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_goal_run() -> GoalRun {
        GoalRun::new(
            "Test Goal",
            "Testing the system",
            "test-agent",
            PathBuf::from("/tmp/staging"),
            PathBuf::from("/tmp/store"),
        )
    }

    #[test]
    fn new_goal_run_starts_in_created_state() {
        let gr = test_goal_run();
        assert_eq!(gr.state, GoalRunState::Created);
        assert_eq!(gr.title, "Test Goal");
        assert_eq!(gr.agent_id, "test-agent");
        assert!(gr.pr_package_id.is_none());
    }

    #[test]
    fn valid_forward_transitions() {
        let mut gr = test_goal_run();
        gr.transition(GoalRunState::Configured).unwrap();
        gr.transition(GoalRunState::Running).unwrap();
        gr.transition(GoalRunState::PrReady).unwrap();
        gr.transition(GoalRunState::UnderReview).unwrap();
        gr.transition(GoalRunState::Approved {
            approved_by: "reviewer".to_string(),
        })
        .unwrap();
        gr.transition(GoalRunState::Applied).unwrap();
        gr.transition(GoalRunState::Completed).unwrap();
    }

    #[test]
    fn invalid_transition_returns_error() {
        let mut gr = test_goal_run();
        // Can't go from Created directly to Running (must configure first).
        let result = gr.transition(GoalRunState::Running);
        assert!(matches!(result, Err(GoalError::InvalidTransition { .. })));
    }

    #[test]
    fn failed_transition_always_valid() {
        let mut gr = test_goal_run();
        gr.transition(GoalRunState::Failed {
            reason: "test failure".to_string(),
        })
        .unwrap();
        assert!(matches!(gr.state, GoalRunState::Failed { .. }));
    }

    #[test]
    fn under_review_can_go_back_to_running() {
        let mut gr = test_goal_run();
        gr.transition(GoalRunState::Configured).unwrap();
        gr.transition(GoalRunState::Running).unwrap();
        gr.transition(GoalRunState::PrReady).unwrap();
        gr.transition(GoalRunState::UnderReview).unwrap();
        // PR denied — agent can try again.
        gr.transition(GoalRunState::Running).unwrap();
        assert_eq!(gr.state, GoalRunState::Running);
    }

    #[test]
    fn serialization_round_trip() {
        let gr = test_goal_run();
        let json = serde_json::to_string_pretty(&gr).unwrap();
        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert_eq!(gr.goal_run_id, restored.goal_run_id);
        assert_eq!(gr.title, restored.title);
        assert_eq!(gr.state, restored.state);
    }

    #[test]
    fn plan_phase_serialization_round_trip() {
        let mut gr = test_goal_run();
        gr.plan_phase = Some("4b".to_string());
        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(json.contains("\"plan_phase\""));
        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.plan_phase, Some("4b".to_string()));
    }

    #[test]
    fn plan_phase_none_omitted_from_json() {
        let gr = test_goal_run();
        assert!(gr.plan_phase.is_none());
        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(!json.contains("plan_phase"));
        // Deserializing JSON without plan_phase should produce None (backward compat).
        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert!(restored.plan_phase.is_none());
    }

    #[test]
    fn state_display_format() {
        assert_eq!(GoalRunState::Created.to_string(), "created");
        assert_eq!(GoalRunState::Running.to_string(), "running");
        assert_eq!(GoalRunState::PrReady.to_string(), "pr_ready");
        assert_eq!(
            GoalRunState::Approved {
                approved_by: "x".to_string()
            }
            .to_string(),
            "approved"
        );
    }

    #[test]
    fn parent_goal_id_serialization_round_trip() {
        let mut gr = test_goal_run();
        let parent_id = Uuid::new_v4();
        gr.parent_goal_id = Some(parent_id);
        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(json.contains("\"parent_goal_id\""));
        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.parent_goal_id, Some(parent_id));
    }

    #[test]
    fn macro_goal_pr_ready_to_running() {
        let mut gr = test_goal_run();
        gr.is_macro = true;
        gr.transition(GoalRunState::Configured).unwrap();
        gr.transition(GoalRunState::Running).unwrap();
        gr.transition(GoalRunState::PrReady).unwrap();
        // Macro goals can go back to Running for inner-loop iteration.
        gr.transition(GoalRunState::Running).unwrap();
        assert_eq!(gr.state, GoalRunState::Running);
    }

    #[test]
    fn macro_goal_fields_serialization_round_trip() {
        let mut gr = test_goal_run();
        gr.is_macro = true;
        let sub_id = Uuid::new_v4();
        gr.sub_goal_ids = vec![sub_id];
        let parent_macro = Uuid::new_v4();
        gr.parent_macro_id = Some(parent_macro);

        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(json.contains("\"is_macro\""));
        assert!(json.contains("\"parent_macro_id\""));
        assert!(json.contains("\"sub_goal_ids\""));

        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert!(restored.is_macro);
        assert_eq!(restored.parent_macro_id, Some(parent_macro));
        assert_eq!(restored.sub_goal_ids, vec![sub_id]);
    }

    #[test]
    fn macro_fields_default_omitted_from_json() {
        let gr = test_goal_run();
        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(!json.contains("is_macro"));
        assert!(!json.contains("parent_macro_id"));
        assert!(!json.contains("sub_goal_ids"));
        // Backward compat: JSON without these fields deserializes fine.
        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert!(!restored.is_macro);
        assert!(restored.parent_macro_id.is_none());
        assert!(restored.sub_goal_ids.is_empty());
    }

    #[test]
    fn workflow_fields_serialization_round_trip() {
        let mut gr = test_goal_run();
        gr.workflow_id = Some("wf-123".to_string());
        gr.stage = Some("build".to_string());
        gr.role = Some("engineer".to_string());
        let ctx_id = Uuid::new_v4();
        gr.context_from = vec![ctx_id];

        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(json.contains("\"workflow_id\""));
        assert!(json.contains("\"stage\""));
        assert!(json.contains("\"role\""));
        assert!(json.contains("\"context_from\""));

        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.workflow_id, Some("wf-123".to_string()));
        assert_eq!(restored.stage, Some("build".to_string()));
        assert_eq!(restored.role, Some("engineer".to_string()));
        assert_eq!(restored.context_from, vec![ctx_id]);
    }

    #[test]
    fn workflow_fields_default_omitted_from_json() {
        let gr = test_goal_run();
        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(!json.contains("workflow_id"));
        assert!(!json.contains("\"stage\""));
        assert!(!json.contains("\"role\""));
        assert!(!json.contains("context_from"));
        // Backward compat: JSON without these fields deserializes fine.
        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert!(restored.workflow_id.is_none());
        assert!(restored.context_from.is_empty());
    }

    #[test]
    fn parent_goal_id_none_omitted_from_json() {
        let gr = test_goal_run();
        assert!(gr.parent_goal_id.is_none());
        let json = serde_json::to_string_pretty(&gr).unwrap();
        assert!(!json.contains("parent_goal_id"));
        // Deserializing JSON without parent_goal_id should produce None (backward compat).
        let restored: GoalRun = serde_json::from_str(&json).unwrap();
        assert!(restored.parent_goal_id.is_none());
    }
}
