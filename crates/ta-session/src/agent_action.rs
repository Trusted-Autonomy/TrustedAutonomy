// agent_action.rs — AgentAction: typed structured output and routing (v0.17.0.2).
//
// Replaces the binary `AdvisorOutcome` with a typed enum that lets the workflow
// engine distinguish *why* an advisor acted and route accordingly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::advisor_agent::AdvisorOutcome;

// ── TeamRole ──────────────────────────────────────────────────────────────────

/// Role of the agent/team member producing or consuming an action.
///
/// Data-defined (per `TA-CONSTITUTION.md` §1.6): this is an open role name, not a
/// closed enum, so a `team.toml` can declare any role (e.g. `"security-team"`)
/// without a TA core change. The five well-known names below are recognized as
/// constants/constructors for defaults and documentation but are not exhaustive.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct TeamRole(String);

impl TeamRole {
    pub const IMPLEMENTER: &'static str = "implementer";
    pub const REVIEWER: &'static str = "reviewer";
    pub const QA: &'static str = "qa";
    pub const ARCHITECT: &'static str = "architect";
    pub const RELEASE_MANAGER: &'static str = "release_manager";
    const HUMAN_PREFIX: &'static str = "human:";

    /// Construct a role from an arbitrary name — the data-defined escape hatch
    /// for custom/community roles.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn implementer() -> Self {
        Self(Self::IMPLEMENTER.to_string())
    }

    pub fn reviewer() -> Self {
        Self(Self::REVIEWER.to_string())
    }

    pub fn qa() -> Self {
        Self(Self::QA.to_string())
    }

    pub fn architect() -> Self {
        Self(Self::ARCHITECT.to_string())
    }

    pub fn release_manager() -> Self {
        Self(Self::RELEASE_MANAGER.to_string())
    }

    /// A human member identified by a label (serialized as `human:<id>`).
    pub fn human(id: impl Into<String>) -> Self {
        Self(format!("{}{}", Self::HUMAN_PREFIX, id.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The human label, if this role is a `human:<id>` reference.
    pub fn human_id(&self) -> Option<&str> {
        self.0.strip_prefix(Self::HUMAN_PREFIX)
    }
}

impl std::fmt::Display for TeamRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── TeamMember ────────────────────────────────────────────────────────────────

/// A configured team member with role, agent identity, security level, and optional persona (v0.17.0.3).
///
/// Persisted in `.ta/team.toml` and used by the advisor subsystem to select
/// the right agent, security posture, and persona for each role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMember {
    pub role: TeamRole,
    pub agent_id: String,
    pub security: crate::workflow_session::AdvisorSecurity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
}

// ── RoleRef ───────────────────────────────────────────────────────────────────

/// Reference to a recipient for an escalation — by role or by specific agent ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleRef {
    Role(TeamRole),
    Agent(String),
}

impl std::fmt::Display for RoleRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoleRef::Role(role) => write!(f, "role:{}", role),
            RoleRef::Agent(id) => write!(f, "agent:{}", id),
        }
    }
}

// ── PlanEdit ──────────────────────────────────────────────────────────────────

/// A structured, reviewable edit to a PLAN.md phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanEdit {
    AddItem { item: String },
    RemoveItem { item: String },
    ModifyItem { from: String, to: String },
    ModifyDescription { text: String },
    AddDependency { phase_id: String },
    RemoveDependency { phase_id: String },
}

// ── AgentAction ───────────────────────────────────────────────────────────────

/// Typed action produced by an advisor or orchestration agent.
///
/// Confidence (where applicable) is expressed as a percentage 0–100.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentAction {
    /// Apply the specified draft.
    Apply {
        draft_id: Uuid,
        /// Confidence percentage 0–100. 100 = fully confident.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
    },

    /// Deny the specified draft, with reason and optional rework hint.
    Deny {
        draft_id: Uuid,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rework_hint: Option<String>,
    },

    /// Propose a structured modification to a plan phase.
    PlanMod {
        phase_id: String,
        edit: PlanEdit,
        justification: String,
    },

    /// Start a new goal (sub-task).
    StartGoal {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },

    /// Escalate a question or decision to a specific role or agent.
    Escalate {
        question: String,
        escalate_to: RoleRef,
    },

    /// Wait for CI checks to pass on a PR before proceeding.
    WaitCI {
        pr_number: u64,
        #[serde(default)]
        checks: Vec<String>,
    },

    /// Merge a pull request.
    Merge { pr_number: u64 },

    /// Continue to the next workflow step (no-op routing action).
    Continue,
}

impl std::fmt::Display for AgentAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentAction::Apply {
                draft_id,
                confidence,
                ..
            } => {
                if let Some(pct) = confidence {
                    write!(f, "Apply({}, {}%)", draft_id, pct)
                } else {
                    write!(f, "Apply({})", draft_id)
                }
            }
            AgentAction::Deny {
                draft_id, reason, ..
            } => {
                write!(f, "Deny({}, \"{}\")", draft_id, reason)
            }
            AgentAction::PlanMod { phase_id, .. } => write!(f, "PlanMod({})", phase_id),
            AgentAction::StartGoal { title, .. } => write!(f, "StartGoal(\"{}\")", title),
            AgentAction::Escalate { escalate_to, .. } => {
                write!(f, "Escalate(→ {})", escalate_to)
            }
            AgentAction::WaitCI { pr_number, .. } => write!(f, "WaitCI(#{})", pr_number),
            AgentAction::Merge { pr_number } => write!(f, "Merge(#{})", pr_number),
            AgentAction::Continue => write!(f, "Continue"),
        }
    }
}

// ── ActionEnvelope ────────────────────────────────────────────────────────────

/// Envelope wrapping an `AgentAction` with identity, timing, and extensible metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEnvelope {
    pub action_id: Uuid,
    pub agent_id: String,
    pub role: TeamRole,
    pub timestamp: DateTime<Utc>,
    pub action: AgentAction,
    pub metadata: serde_json::Value,
}

impl ActionEnvelope {
    pub fn new(agent_id: impl Into<String>, role: TeamRole, action: AgentAction) -> Self {
        Self {
            action_id: Uuid::new_v4(),
            agent_id: agent_id.into(),
            role,
            timestamp: Utc::now(),
            action,
            metadata: serde_json::Value::Object(Default::default()),
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

// ── AdvisorOutcome → ActionEnvelope conversion ────────────────────────────────

/// Backward-compatible bridge from `AdvisorOutcome` to `ActionEnvelope`.
///
/// Existing `spawn_advisor_agent` callers can pass the outcome through
/// the new routing layer without changing their call sites.
pub fn advisor_outcome_to_envelope(
    outcome: &AdvisorOutcome,
    agent_id: impl Into<String>,
    role: TeamRole,
    draft_id: Uuid,
) -> ActionEnvelope {
    let action = match outcome {
        AdvisorOutcome::Applied => AgentAction::Apply {
            draft_id,
            confidence: Some(100),
            notes: None,
        },
        AdvisorOutcome::Denied => AgentAction::Deny {
            draft_id,
            reason: "Advisor denied the draft".to_string(),
            rework_hint: None,
        },
        AdvisorOutcome::TimedOut => AgentAction::Escalate {
            question: "Advisor timed out waiting for human response. Manual review required."
                .to_string(),
            escalate_to: RoleRef::Role(TeamRole::human("primary")),
        },
        AdvisorOutcome::SpawnFailed { reason } => AgentAction::Escalate {
            question: format!(
                "Advisor subprocess failed to start: {}. Manual review required.",
                reason
            ),
            escalate_to: RoleRef::Role(TeamRole::human("primary")),
        },
        AdvisorOutcome::ReviewerBusy {
            active_advisor_goal_id,
        } => AgentAction::Escalate {
            question: format!(
                "Another advisor (goal {}) is already reviewing this draft. \
                 Wait for it to complete or manually review the draft.",
                active_advisor_goal_id
            ),
            escalate_to: RoleRef::Role(TeamRole::human("primary")),
        },
    };
    ActionEnvelope::new(agent_id, role, action)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_action_apply_round_trip() {
        let draft_id = Uuid::new_v4();
        let action = AgentAction::Apply {
            draft_id,
            confidence: Some(90),
            notes: Some("looks good".to_string()),
        };
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn agent_action_deny_round_trip() {
        let action = AgentAction::Deny {
            draft_id: Uuid::new_v4(),
            reason: "missing tests".to_string(),
            rework_hint: Some("add coverage".to_string()),
        };
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn agent_action_plan_mod_round_trip() {
        let action = AgentAction::PlanMod {
            phase_id: "v0.17.0.3".to_string(),
            edit: PlanEdit::AddItem {
                item: "new item".to_string(),
            },
            justification: "scope expansion".to_string(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn agent_action_start_goal_round_trip() {
        let action = AgentAction::StartGoal {
            title: "Fix flaky tests".to_string(),
            phase_id: Some("v0.17.0.3".to_string()),
            context: Some("tests fail on CI only".to_string()),
        };
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn agent_action_escalate_round_trip() {
        let action = AgentAction::Escalate {
            question: "Is this change safe?".to_string(),
            escalate_to: RoleRef::Role(TeamRole::architect()),
        };
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn agent_action_wait_ci_round_trip() {
        let action = AgentAction::WaitCI {
            pr_number: 42,
            checks: vec!["build".to_string(), "test".to_string()],
        };
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn agent_action_merge_round_trip() {
        let action = AgentAction::Merge { pr_number: 99 };
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn agent_action_continue_round_trip() {
        let action = AgentAction::Continue;
        let json = serde_json::to_string(&action).unwrap();
        let restored: AgentAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, restored);
    }

    #[test]
    fn action_envelope_round_trip() {
        let env = ActionEnvelope::new(
            "claude-sonnet-4-6",
            TeamRole::reviewer(),
            AgentAction::Continue,
        );
        let json = serde_json::to_string(&env).unwrap();
        let restored: ActionEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.agent_id, "claude-sonnet-4-6");
        assert_eq!(restored.role, TeamRole::reviewer());
        assert_eq!(restored.action, AgentAction::Continue);
        assert_eq!(restored.action_id, env.action_id);
    }

    #[test]
    fn team_role_display() {
        assert_eq!(TeamRole::implementer().to_string(), "implementer");
        assert_eq!(TeamRole::reviewer().to_string(), "reviewer");
        assert_eq!(TeamRole::qa().to_string(), "qa");
        assert_eq!(TeamRole::architect().to_string(), "architect");
        assert_eq!(TeamRole::release_manager().to_string(), "release_manager");
        assert_eq!(TeamRole::human("alice").to_string(), "human:alice");
    }

    #[test]
    fn team_role_serialization() {
        let role = TeamRole::human("bob");
        let json = serde_json::to_string(&role).unwrap();
        let restored: TeamRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, restored);
    }

    #[test]
    fn role_ref_display() {
        assert_eq!(
            RoleRef::Role(TeamRole::reviewer()).to_string(),
            "role:reviewer"
        );
        assert_eq!(
            RoleRef::Agent("claude-opus".to_string()).to_string(),
            "agent:claude-opus"
        );
    }

    #[test]
    fn agent_action_display() {
        let draft_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let apply = AgentAction::Apply {
            draft_id,
            confidence: Some(80),
            notes: None,
        };
        assert!(apply.to_string().contains("Apply"));
        assert!(apply.to_string().contains("80%"));
        assert!(AgentAction::Continue.to_string().contains("Continue"));
        assert!(AgentAction::Merge { pr_number: 5 }
            .to_string()
            .contains("Merge(#5)"));
    }

    #[test]
    fn advisor_outcome_to_envelope_applied() {
        let draft_id = Uuid::new_v4();
        let env = advisor_outcome_to_envelope(
            &AdvisorOutcome::Applied,
            "claude",
            TeamRole::reviewer(),
            draft_id,
        );
        assert!(matches!(
            env.action,
            AgentAction::Apply {
                confidence: Some(100),
                ..
            }
        ));
        assert_eq!(env.agent_id, "claude");
        assert_eq!(env.role, TeamRole::reviewer());
    }

    #[test]
    fn advisor_outcome_to_envelope_denied() {
        let draft_id = Uuid::new_v4();
        let env = advisor_outcome_to_envelope(
            &AdvisorOutcome::Denied,
            "claude",
            TeamRole::reviewer(),
            draft_id,
        );
        assert!(matches!(env.action, AgentAction::Deny { .. }));
    }

    #[test]
    fn advisor_outcome_to_envelope_timed_out() {
        let draft_id = Uuid::new_v4();
        let env = advisor_outcome_to_envelope(
            &AdvisorOutcome::TimedOut,
            "claude",
            TeamRole::reviewer(),
            draft_id,
        );
        assert!(matches!(env.action, AgentAction::Escalate { .. }));
    }

    #[test]
    fn advisor_outcome_to_envelope_spawn_failed() {
        let draft_id = Uuid::new_v4();
        let env = advisor_outcome_to_envelope(
            &AdvisorOutcome::SpawnFailed {
                reason: "binary not found".to_string(),
            },
            "claude",
            TeamRole::implementer(),
            draft_id,
        );
        if let AgentAction::Escalate { question, .. } = &env.action {
            assert!(question.contains("binary not found"));
        } else {
            panic!("expected Escalate action");
        }
    }

    #[test]
    fn action_envelope_with_metadata() {
        use serde_json::json;
        let env = ActionEnvelope::new("agent", TeamRole::qa(), AgentAction::Continue)
            .with_metadata(json!({"session_id": "abc"}));
        assert_eq!(env.metadata["session_id"], "abc");
    }
}
