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

pub mod conversation;
pub mod error;
pub mod events;
pub mod goal_run;
pub mod history;
pub mod operations;
pub mod persona;
pub mod store;
pub mod velocity;

pub use conversation::{ConversationStore, ConversationTurn, TurnRole};
pub use error::GoalError;
pub use events::{EventDispatcher, LogSink, NotificationSink, TaEvent};
pub use goal_run::{slugify_title, GoalRun, GoalRunState};
pub use history::{GoalHistoryEntry, GoalHistoryLedger, HistoryFilter};
pub use operations::{ActionSeverity, ActionStatus, CorrectiveAction, OperationsLog};
pub use persona::{PersonaCapabilities, PersonaConfig, PersonaInner, PersonaStyle, PersonaSummary};
pub use store::GoalRunStore;
pub use velocity::{
    aggregate_by_contributor, detect_phase_conflicts, merge_velocity_entries,
    migrate_local_to_history, ContributorAggregate, GoalOutcome, PhaseConflict, VelocityAggregate,
    VelocityEntry, VelocityHistoryStore, VelocityStore,
};
