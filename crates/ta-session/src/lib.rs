// ta-session — Session & Human Control Plane (Layer 3).

pub mod action_audit;
pub mod action_router;
pub mod advisor_agent;
pub mod advisor_session;
pub mod agent_action;
pub mod error;
pub mod intent;
pub mod manager;
pub mod phase_summary;
pub mod plan;
pub mod session;
pub mod team;
pub mod workflow_manager;
pub mod workflow_session;

pub use action_audit::{ActionAuditLog, AuditLogError};
pub use action_router::{
    check_plan_mod_constitution, ActionRouter, ApplyPrimitive, DenyPrimitive, EscalatePrimitive,
    PrimitiveError, StartGoalPrimitive, WorkflowContext, WorkflowPrimitive,
};
pub use advisor_agent::{
    acquire_reviewer_lock, build_advisor_context, check_advisor_auto_approve, poll_draft_outcome,
    spawn_advisor_agent, write_advisor_context, AdvisorConfig, AdvisorOutcome, ReviewerLock,
};
pub use advisor_session::{
    build_response_and_options, AdvisorContext, AdvisorOption, AdvisorSession,
};
pub use agent_action::{
    advisor_outcome_to_envelope, ActionEnvelope, AgentAction, PlanEdit, RoleRef, TeamMember,
    TeamRole,
};
pub use error::SessionError;
pub use intent::{classify_intent, Intent, IntentResult};
pub use manager::SessionManager;
pub use phase_summary::{build_phase_summary, PhaseRecord, PhaseSummary};
pub use plan::{PlanDocument, PlanItem};
pub use session::{ConversationTurn, SessionState, TaSession};
pub use team::{
    default_personas_governed_path_toml, TeamConfig, TeamConfigError, PERSONAS_GOVERNED_PATH,
};
pub use workflow_manager::WorkflowSessionManager;
pub use workflow_session::{
    AdvisorSecurity, GateMode, WorkflowItemState, WorkflowSession, WorkflowSessionItem,
    WorkflowSessionState,
};
