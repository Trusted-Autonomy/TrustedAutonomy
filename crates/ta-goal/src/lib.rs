//! # ta-goal
//!
//! GoalRun lifecycle management and event dispatch for Trusted Autonomy.
//!
//! A [`GoalRun`] is the top-level execution unit that ties together an agent,
//! a capability manifest, a staging workspace, and eventual PR packages.
//! The state machine enforces a valid lifecycle from creation through
//! review and application.
//!
//! ## Key components
//!
//! - [`GoalRun`] — the lifecycle state machine (Created → Configured → Running
//!   → PrReady → UnderReview → Approved → Applied → Completed)
//! - [`GoalRunStore`] — JSON file-based persistence for GoalRun records
//! - [`TaEvent`] — events emitted at key lifecycle points
//! - [`EventDispatcher`] — dispatches events to notification sinks
//! - [`NotificationSink`] — trait for receiving events (log, webhook, etc.)

pub mod analysis;
pub mod conversation;
pub mod error;
pub mod events;
pub mod goal_run;
pub mod history;
pub mod human_review;
pub mod messaging_audit;
pub mod operations;
pub mod persona;
pub mod phase_selector;
pub mod security;
pub mod social_audit;
pub mod store;
pub mod token_cost;
pub mod velocity;

pub use analysis::{
    detect_language, parse_output, run_analyzer, AnalysisConfig, AnalysisFinding, FindingSeverity,
    Language, OnFailure, OnMaxIterations,
};
pub use conversation::{ConversationStore, ConversationTurn, TurnRole};
pub use error::GoalError;
pub use events::{EventDispatcher, LogSink, NotificationSink, TaEvent};
pub use goal_run::{slugify_title, GoalRun, GoalRunState};
pub use history::{GoalHistoryEntry, GoalHistoryLedger, HistoryFilter};
pub use human_review::{
    extract_human_review_items, HumanReviewRecord, HumanReviewStatus, HumanReviewStore,
};
pub use messaging_audit::{DraftEmailRecord, DraftEmailState, MessagingAuditLog};
pub use operations::{ActionSeverity, ActionStatus, CorrectiveAction, OperationsLog};
pub use persona::{PersonaCapabilities, PersonaConfig, PersonaInner, PersonaStyle, PersonaSummary};
pub use phase_selector::{PhaseSelector, PhaseSelectorConfig, SelectedPhase};
pub use security::{
    AuditMode, ConstitutionBlockMode, SecretScanMode, SecurityLevel, SecurityOverrides,
    SecurityProfile, DEFAULT_MID_FORBIDDEN_TOOLS,
};
pub use social_audit::{DraftSocialRecord, SocialAuditLog, SocialPostRecordState};
pub use store::GoalRunStore;
pub use token_cost::{compute_cost, rate_for_model, ModelRate};
pub use velocity::{
    aggregate_by_contributor, detect_phase_conflicts, filter_by_phase_prefix,
    merge_velocity_entries, migrate_local_to_history, update_parent_rework, ContributorAggregate,
    GoalOutcome, PhaseConflict, VelocityAggregate, VelocityEntry, VelocityHistoryStore,
    VelocityStore,
};
