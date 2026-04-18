// workflow_session.rs — WorkflowSession: project-level plan execution session (v0.14.11).
//
// A WorkflowSession tracks the execution of a PlanDocument across multiple governed
// workflow runs. Created by `ta session start <plan-id>`, reviewed interactively via
// `ta session review`, and executed via `ta session run`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::SessionError;
use crate::plan::{PlanDocument, PlanItem};

/// Security level for the advisor agent: controls what actions it may take autonomously.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorSecurity {
    /// Advisor can only answer questions and present diffs — never starts a goal or
    /// applies a draft autonomously.
    #[default]
    ReadOnly,
    /// Advisor presents the exact `ta run "..."` command for the human to copy-paste.
    Suggest,
    /// At ≥80% intent confidence, advisor fires `ta run` directly without prompting.
    Auto,
}

impl std::fmt::Display for AdvisorSecurity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdvisorSecurity::ReadOnly => write!(f, "read_only"),
            AdvisorSecurity::Suggest => write!(f, "suggest"),
            AdvisorSecurity::Auto => write!(f, "auto"),
        }
    }
}

impl std::str::FromStr for AdvisorSecurity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().replace('-', "_").as_str() {
            "read_only" | "readonly" => Ok(AdvisorSecurity::ReadOnly),
            "suggest" => Ok(AdvisorSecurity::Suggest),
            "auto" => Ok(AdvisorSecurity::Auto),
            other => Err(format!(
                "Unknown advisor security level '{}'. Valid values: read_only, suggest, auto.",
                other
            )),
        }
    }
}

/// Gate mode controlling how often the human is asked to approve before applying.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// Automatically proceed on success; pause only on failure or explicit flag.
    #[default]
    Auto,
    /// Always prompt the human for approval before applying each plan item.
    Prompt,
    /// Require explicit human approval for every gate (synonym for Prompt).
    Always,
    /// Spawn an advisor agent to present changes and converse with the human.
    Agent {
        /// Optional persona name (references `.ta/personas/<name>.toml`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persona: Option<String>,
        /// Controls what the advisor is allowed to do autonomously.
        #[serde(default)]
        security: AdvisorSecurity,
    },
}

impl std::fmt::Display for GateMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateMode::Auto => write!(f, "auto"),
            GateMode::Prompt => write!(f, "prompt"),
            GateMode::Always => write!(f, "always"),
            GateMode::Agent { persona, security } => match persona {
                Some(p) => write!(f, "agent(persona={}, security={})", p, security),
                None => write!(f, "agent(security={})", security),
            },
        }
    }
}

impl std::str::FromStr for GateMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(GateMode::Auto),
            "prompt" => Ok(GateMode::Prompt),
            "always" => Ok(GateMode::Always),
            "agent" => Ok(GateMode::Agent {
                persona: None,
                security: AdvisorSecurity::ReadOnly,
            }),
            other => Err(format!(
                "Unknown gate mode '{}'. Valid values: auto, prompt, always, agent.",
                other
            )),
        }
    }
}

/// Lifecycle state of a workflow session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSessionState {
    /// Plan items are being reviewed interactively (`ta session review`).
    Reviewing,
    /// Session is actively executing accepted items (`ta session run`).
    Running,
    /// Session is paused (human interrupted, or AwaitHuman gate requires action).
    Paused,
    /// All accepted items reached a terminal state; session is complete.
    Complete,
}

impl std::fmt::Display for WorkflowSessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowSessionState::Reviewing => write!(f, "reviewing"),
            WorkflowSessionState::Running => write!(f, "running"),
            WorkflowSessionState::Paused => write!(f, "paused"),
            WorkflowSessionState::Complete => write!(f, "complete"),
        }
    }
}

/// Lifecycle state of a single plan item within a workflow session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowItemState {
    /// Awaiting user review decision during `ta session review`.
    Pending,
    /// Accepted by user; queued for execution.
    Accepted,
    /// Agent is currently executing this item.
    Running,
    /// Agent completed; paused at AwaitHuman gate awaiting human verdict.
    AtGate,
    /// Advisor agent is active, conversing with the human about this item.
    AdvisorActive {
        /// Goal run ID of the advisor agent process.
        advisor_goal_id: Uuid,
    },
    /// Draft applied successfully — item is done.
    Complete,
    /// User chose to skip this item during review.
    Skipped,
    /// User chose to defer this item to a future session.
    Deferred,
    /// Execution failed (see `failure_reason`).
    Failed,
}

impl std::fmt::Display for WorkflowItemState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowItemState::Pending => write!(f, "pending"),
            WorkflowItemState::Accepted => write!(f, "accepted"),
            WorkflowItemState::Running => write!(f, "running"),
            WorkflowItemState::AtGate => write!(f, "at_gate"),
            WorkflowItemState::AdvisorActive { .. } => write!(f, "advisor_active"),
            WorkflowItemState::Complete => write!(f, "complete"),
            WorkflowItemState::Skipped => write!(f, "skipped"),
            WorkflowItemState::Deferred => write!(f, "deferred"),
            WorkflowItemState::Failed => write!(f, "failed"),
        }
    }
}

impl WorkflowItemState {
    /// Returns true if this state is terminal (no further transitions possible).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkflowItemState::Complete
                | WorkflowItemState::Skipped
                | WorkflowItemState::Deferred
                | WorkflowItemState::Failed
        )
    }
}

/// A plan item tracked within a workflow session, extended with execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSessionItem {
    pub item_id: Uuid,
    pub title: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_effort: Option<String>,
    pub state: WorkflowItemState,
    /// Goal run ID spawned to execute this item (set when state → Running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<Uuid>,
    /// Draft package ID built from the goal (set when agent exits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<Uuid>,
    /// Timestamp when this item reached a terminal state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<DateTime<Utc>>,
    /// Human-readable failure reason when state is Failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

impl WorkflowSessionItem {
    /// Create from a PlanItem, starting in Pending state.
    pub fn from_plan_item(item: &PlanItem) -> Self {
        Self {
            item_id: item.item_id,
            title: item.title.clone(),
            acceptance_criteria: item.acceptance_criteria.clone(),
            estimated_effort: item.estimated_effort.clone(),
            state: WorkflowItemState::Pending,
            goal_id: None,
            draft_id: None,
            applied_at: None,
            failure_reason: None,
        }
    }
}

/// A project-level session that tracks the execution of a complete PlanDocument.
///
/// Persisted to `.ta/sessions/workflow-<session-id>.json`.
/// Multiple WorkflowSessions can exist (one per plan).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSession {
    pub session_id: Uuid,
    pub plan_id: Uuid,
    pub plan_title: String,
    pub items: Vec<WorkflowSessionItem>,
    pub state: WorkflowSessionState,
    pub gate_mode: GateMode,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkflowSession {
    /// Create a new WorkflowSession from a PlanDocument.
    ///
    /// All items start in `Pending` state; session starts in `Reviewing`.
    pub fn from_plan(plan: &PlanDocument, gate_mode: GateMode) -> Self {
        let now = Utc::now();
        Self {
            session_id: Uuid::new_v4(),
            plan_id: plan.plan_id,
            plan_title: plan.title.clone(),
            items: plan
                .items
                .iter()
                .map(WorkflowSessionItem::from_plan_item)
                .collect(),
            state: WorkflowSessionState::Reviewing,
            gate_mode,
            created_at: now,
            updated_at: now,
        }
    }

    /// Transition session to a new state, validating the transition.
    pub fn transition(&mut self, new_state: WorkflowSessionState) -> Result<(), SessionError> {
        let valid = matches!(
            (&self.state, &new_state),
            (
                WorkflowSessionState::Reviewing,
                WorkflowSessionState::Running
            ) | (
                WorkflowSessionState::Reviewing,
                WorkflowSessionState::Paused
            ) | (WorkflowSessionState::Running, WorkflowSessionState::Paused)
                | (
                    WorkflowSessionState::Running,
                    WorkflowSessionState::Complete
                )
                | (WorkflowSessionState::Paused, WorkflowSessionState::Running)
                | (WorkflowSessionState::Paused, WorkflowSessionState::Complete)
        );
        if !valid {
            return Err(SessionError::InvalidTransition {
                from: self.state.to_string(),
                to: new_state.to_string(),
            });
        }
        self.state = new_state;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Update an item's state. Returns true if the item was found.
    pub fn update_item_state(&mut self, item_id: Uuid, state: WorkflowItemState) -> bool {
        if let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) {
            item.state = state;
            self.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    /// Associate a goal run ID with a running item.
    pub fn set_item_goal(&mut self, item_id: Uuid, goal_id: Uuid) -> bool {
        if let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) {
            item.goal_id = Some(goal_id);
            self.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    /// Mark an item complete with its applied draft ID.
    pub fn complete_item(&mut self, item_id: Uuid, draft_id: Option<Uuid>) {
        if let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) {
            item.state = WorkflowItemState::Complete;
            item.draft_id = draft_id;
            item.applied_at = Some(Utc::now());
            self.updated_at = Utc::now();
        }
    }

    /// Mark an item as failed with a reason.
    pub fn fail_item(&mut self, item_id: Uuid, reason: &str) {
        if let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) {
            item.state = WorkflowItemState::Failed;
            item.failure_reason = Some(reason.to_string());
            self.updated_at = Utc::now();
        }
    }

    /// Return the next item in Accepted state (ready to execute), if any.
    pub fn next_runnable(&self) -> Option<&WorkflowSessionItem> {
        self.items
            .iter()
            .find(|i| i.state == WorkflowItemState::Accepted)
    }

    /// Return the currently running item, if any.
    pub fn current_running(&self) -> Option<&WorkflowSessionItem> {
        self.items
            .iter()
            .find(|i| i.state == WorkflowItemState::Running)
    }

    /// Return the item currently at the AwaitHuman gate, if any.
    pub fn at_gate(&self) -> Option<&WorkflowSessionItem> {
        self.items
            .iter()
            .find(|i| i.state == WorkflowItemState::AtGate)
    }

    /// Return the item with an active advisor agent, if any.
    pub fn advisor_active(&self) -> Option<&WorkflowSessionItem> {
        self.items
            .iter()
            .find(|i| matches!(i.state, WorkflowItemState::AdvisorActive { .. }))
    }

    /// Set item state to AdvisorActive with the given advisor goal run ID.
    pub fn set_item_advisor(&mut self, item_id: Uuid, advisor_goal_id: Uuid) -> bool {
        if let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) {
            item.state = WorkflowItemState::AdvisorActive { advisor_goal_id };
            self.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    /// Returns true when all items are in a terminal state.
    pub fn all_items_terminal(&self) -> bool {
        self.items.iter().all(|i| i.state.is_terminal())
    }

    /// Count items currently in the given state.
    pub fn count_by_state(&self, state: &WorkflowItemState) -> usize {
        self.items.iter().filter(|i| &i.state == state).count()
    }

    /// One-line status summary suitable for `ta session list` output.
    pub fn status_summary(&self) -> String {
        let total = self.items.len();
        let complete = self.count_by_state(&WorkflowItemState::Complete);
        let running = self.count_by_state(&WorkflowItemState::Running);
        let accepted = self.count_by_state(&WorkflowItemState::Accepted);
        let skipped = self.count_by_state(&WorkflowItemState::Skipped);
        let deferred = self.count_by_state(&WorkflowItemState::Deferred);
        let failed = self.count_by_state(&WorkflowItemState::Failed);
        format!(
            "{total} items: {complete} done, {running} running, {accepted} queued, \
             {skipped} skipped, {deferred} deferred, {failed} failed"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{PlanDocument, PlanItem};

    fn two_item_plan() -> PlanDocument {
        let mut plan = PlanDocument::new("Test Plan");
        plan.add_item(PlanItem::new("Item Alpha"));
        plan.add_item(PlanItem::new("Item Beta"));
        plan
    }

    #[test]
    fn from_plan_creates_session_in_reviewing_state() {
        let plan = two_item_plan();
        let session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        assert_eq!(session.plan_id, plan.plan_id);
        assert_eq!(session.plan_title, "Test Plan");
        assert_eq!(session.items.len(), 2);
        assert_eq!(session.state, WorkflowSessionState::Reviewing);
        assert_eq!(session.gate_mode, GateMode::Auto);
        for item in &session.items {
            assert_eq!(item.state, WorkflowItemState::Pending);
            assert!(item.goal_id.is_none());
            assert!(item.draft_id.is_none());
        }
    }

    #[test]
    fn transition_reviewing_to_running() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        session.transition(WorkflowSessionState::Running).unwrap();
        assert_eq!(session.state, WorkflowSessionState::Running);
    }

    #[test]
    fn transition_running_to_paused_and_back() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        session.transition(WorkflowSessionState::Running).unwrap();
        session.transition(WorkflowSessionState::Paused).unwrap();
        assert_eq!(session.state, WorkflowSessionState::Paused);
        session.transition(WorkflowSessionState::Running).unwrap();
        assert_eq!(session.state, WorkflowSessionState::Running);
    }

    #[test]
    fn invalid_transition_rejected() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        // Cannot jump from Reviewing → Complete without going through Running.
        assert!(session.transition(WorkflowSessionState::Complete).is_err());
    }

    #[test]
    fn update_item_state_returns_true_on_success() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Prompt);
        let id = session.items[0].item_id;
        assert!(session.update_item_state(id, WorkflowItemState::Accepted));
        assert_eq!(session.items[0].state, WorkflowItemState::Accepted);
    }

    #[test]
    fn update_item_state_returns_false_for_unknown_id() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        assert!(!session.update_item_state(Uuid::new_v4(), WorkflowItemState::Accepted));
    }

    #[test]
    fn complete_item_sets_state_and_draft_id() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        let item_id = session.items[0].item_id;
        let draft_id = Uuid::new_v4();
        session.complete_item(item_id, Some(draft_id));
        assert_eq!(session.items[0].state, WorkflowItemState::Complete);
        assert_eq!(session.items[0].draft_id, Some(draft_id));
        assert!(session.items[0].applied_at.is_some());
    }

    #[test]
    fn fail_item_stores_reason() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        let item_id = session.items[1].item_id;
        session.fail_item(item_id, "compilation error");
        assert_eq!(session.items[1].state, WorkflowItemState::Failed);
        assert_eq!(
            session.items[1].failure_reason.as_deref(),
            Some("compilation error")
        );
    }

    #[test]
    fn all_items_terminal_when_all_done() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        for item in session.items.iter_mut() {
            item.state = WorkflowItemState::Complete;
        }
        assert!(session.all_items_terminal());
    }

    #[test]
    fn not_all_terminal_with_pending() {
        let plan = two_item_plan();
        let session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        assert!(!session.all_items_terminal());
    }

    #[test]
    fn all_terminal_with_mixed_states() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        session.items[0].state = WorkflowItemState::Complete;
        session.items[1].state = WorkflowItemState::Skipped;
        assert!(session.all_items_terminal());
    }

    #[test]
    fn next_runnable_returns_first_accepted() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        let id = session.items[1].item_id;
        session.update_item_state(id, WorkflowItemState::Accepted);
        let runnable = session.next_runnable().unwrap();
        assert_eq!(runnable.item_id, id);
    }

    #[test]
    fn next_runnable_none_when_no_accepted() {
        let plan = two_item_plan();
        let session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        assert!(session.next_runnable().is_none());
    }

    #[test]
    fn count_by_state() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        session.items[0].state = WorkflowItemState::Complete;
        session.items[1].state = WorkflowItemState::Skipped;
        assert_eq!(session.count_by_state(&WorkflowItemState::Complete), 1);
        assert_eq!(session.count_by_state(&WorkflowItemState::Skipped), 1);
        assert_eq!(session.count_by_state(&WorkflowItemState::Pending), 0);
    }

    #[test]
    fn gate_mode_display() {
        assert_eq!(GateMode::Auto.to_string(), "auto");
        assert_eq!(GateMode::Prompt.to_string(), "prompt");
        assert_eq!(GateMode::Always.to_string(), "always");
        assert_eq!(
            GateMode::Agent {
                persona: None,
                security: AdvisorSecurity::ReadOnly
            }
            .to_string(),
            "agent(security=read_only)"
        );
        assert_eq!(
            GateMode::Agent {
                persona: Some("my-advisor".to_string()),
                security: AdvisorSecurity::Suggest
            }
            .to_string(),
            "agent(persona=my-advisor, security=suggest)"
        );
    }

    #[test]
    fn gate_mode_from_str() {
        assert_eq!("auto".parse::<GateMode>().unwrap(), GateMode::Auto);
        assert_eq!("prompt".parse::<GateMode>().unwrap(), GateMode::Prompt);
        assert_eq!("always".parse::<GateMode>().unwrap(), GateMode::Always);
        assert_eq!(
            "agent".parse::<GateMode>().unwrap(),
            GateMode::Agent {
                persona: None,
                security: AdvisorSecurity::ReadOnly
            }
        );
        let err = "bad".parse::<GateMode>();
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("Unknown gate mode"));
    }

    #[test]
    fn gate_mode_case_insensitive_parse() {
        assert_eq!("AUTO".parse::<GateMode>().unwrap(), GateMode::Auto);
        assert_eq!("Prompt".parse::<GateMode>().unwrap(), GateMode::Prompt);
        assert_eq!(
            "AGENT".parse::<GateMode>().unwrap(),
            GateMode::Agent {
                persona: None,
                security: AdvisorSecurity::ReadOnly,
            }
        );
    }

    #[test]
    fn advisor_security_round_trip() {
        assert_eq!(
            "read_only".parse::<AdvisorSecurity>().unwrap(),
            AdvisorSecurity::ReadOnly
        );
        assert_eq!(
            "suggest".parse::<AdvisorSecurity>().unwrap(),
            AdvisorSecurity::Suggest
        );
        assert_eq!(
            "auto".parse::<AdvisorSecurity>().unwrap(),
            AdvisorSecurity::Auto
        );
        assert_eq!(AdvisorSecurity::ReadOnly.to_string(), "read_only");
        assert_eq!(AdvisorSecurity::Suggest.to_string(), "suggest");
        assert_eq!(AdvisorSecurity::Auto.to_string(), "auto");
        assert!("bogus".parse::<AdvisorSecurity>().is_err());
    }

    #[test]
    fn workflow_item_state_is_terminal() {
        assert!(WorkflowItemState::Complete.is_terminal());
        assert!(WorkflowItemState::Skipped.is_terminal());
        assert!(WorkflowItemState::Deferred.is_terminal());
        assert!(WorkflowItemState::Failed.is_terminal());
        assert!(!WorkflowItemState::Pending.is_terminal());
        assert!(!WorkflowItemState::Accepted.is_terminal());
        assert!(!WorkflowItemState::Running.is_terminal());
        assert!(!WorkflowItemState::AtGate.is_terminal());
        assert!(!WorkflowItemState::AdvisorActive {
            advisor_goal_id: Uuid::new_v4()
        }
        .is_terminal());
    }

    #[test]
    fn advisor_active_state() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(
            &plan,
            GateMode::Agent {
                persona: None,
                security: AdvisorSecurity::ReadOnly,
            },
        );
        let item_id = session.items[0].item_id;
        let advisor_id = Uuid::new_v4();
        assert!(session.set_item_advisor(item_id, advisor_id));
        let active = session.advisor_active().unwrap();
        assert_eq!(active.item_id, item_id);
        assert_eq!(
            active.state,
            WorkflowItemState::AdvisorActive {
                advisor_goal_id: advisor_id
            }
        );
        assert_eq!(active.state.to_string(), "advisor_active");
    }

    #[test]
    fn session_serialization_round_trip() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Prompt);
        let id = session.items[0].item_id;
        session.update_item_state(id, WorkflowItemState::Accepted);

        let json = serde_json::to_string_pretty(&session).unwrap();
        let restored: WorkflowSession = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.session_id, session.session_id);
        assert_eq!(restored.plan_id, plan.plan_id);
        assert_eq!(restored.items.len(), 2);
        assert_eq!(restored.items[0].state, WorkflowItemState::Accepted);
        assert_eq!(restored.gate_mode, GateMode::Prompt);
    }

    #[test]
    fn set_item_goal() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        let item_id = session.items[0].item_id;
        let goal_id = Uuid::new_v4();
        assert!(session.set_item_goal(item_id, goal_id));
        assert_eq!(session.items[0].goal_id, Some(goal_id));
    }

    #[test]
    fn current_running_item() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        let id = session.items[0].item_id;
        session.update_item_state(id, WorkflowItemState::Running);
        let running = session.current_running().unwrap();
        assert_eq!(running.item_id, id);
    }

    #[test]
    fn at_gate_item() {
        let plan = two_item_plan();
        let mut session = WorkflowSession::from_plan(&plan, GateMode::Auto);
        let id = session.items[1].item_id;
        session.update_item_state(id, WorkflowItemState::AtGate);
        let gated = session.at_gate().unwrap();
        assert_eq!(gated.item_id, id);
    }
}
