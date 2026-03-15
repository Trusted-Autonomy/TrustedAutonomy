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
pub mod store;

pub use conversation::{ConversationStore, ConversationTurn, TurnRole};
pub use error::GoalError;
pub use events::{EventDispatcher, LogSink, NotificationSink, TaEvent};
pub use goal_run::{slugify_title, GoalRun, GoalRunState};
pub use history::{GoalHistoryEntry, GoalHistoryLedger, HistoryFilter};
pub use store::GoalRunStore;
