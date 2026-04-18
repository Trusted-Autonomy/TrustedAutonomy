// ta-session — Session & Human Control Plane (Layer 3).

pub mod advisor_agent;
pub mod error;
pub mod intent;
pub mod manager;
pub mod phase_summary;
pub mod plan;
pub mod session;
pub mod workflow_manager;
pub mod workflow_session;

pub use advisor_agent::{
    build_advisor_context, check_advisor_auto_approve, poll_draft_outcome, spawn_advisor_agent,
    write_advisor_context, AdvisorConfig, AdvisorOutcome,
};
pub use error::SessionError;
pub use intent::{classify_intent, Intent, IntentResult};
pub use manager::SessionManager;
pub use phase_summary::{build_phase_summary, PhaseRecord, PhaseSummary};
pub use plan::{PlanDocument, PlanItem};
pub use session::{ConversationTurn, SessionState, TaSession};
pub use workflow_manager::WorkflowSessionManager;
pub use workflow_session::{
    AdvisorSecurity, GateMode, WorkflowItemState, WorkflowSession, WorkflowSessionItem,
    WorkflowSessionState,
};
