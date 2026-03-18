//! # ta-events
//!
//! Event system and subscription API for Trusted Autonomy.
//!
//! Provides a stable `SessionEvent` schema, in-process event bus,
//! file-based event persistence, hook execution, and token-based
//! non-interactive approval.

pub mod bus;
pub mod channel;
pub mod error;
pub mod hooks;
pub mod router;
pub mod schema;
pub mod store;
pub mod strategies;
pub mod tokens;

/// Sentinel printed to stderr by `ta run --headless` when a goal is created.
/// The daemon's background command runner scans subprocess stderr for this prefix
/// to register a UUID→output_key alias, enabling SSE auto-tail and `:tail <uuid>`.
///
/// Format: `GOAL_STARTED_SENTINEL "title" (uuid)`
///
/// Both the emitter (`ta-cli run.rs`) and the scanner (`ta-daemon cmd.rs`) MUST
/// use this constant. The companion test `goal_started_sentinel_round_trip` in
/// `ta-daemon` validates that emit→scan is consistent.
pub const GOAL_STARTED_SENTINEL: &str = "[goal started]";

pub use bus::{EventBus, EventFilter};
pub use channel::{ChannelDelivery, ChannelQuestion, ChannelRouting, DeliveryResult};
pub use error::EventError;
pub use hooks::{HookConfig, HookRunner};
pub use router::{
    EventRouter, EventRoutingFilter, Responder, ResponseStrategy, RoutingConfig, RoutingDecision,
    RoutingDefaults,
};
pub use schema::{EventAction, EventEnvelope, SessionEvent};
pub use store::{EventStore, FsEventStore};
pub use strategies::agent::AgentResponseContext;
pub use strategies::workflow::WorkflowResponseContext;
pub use tokens::{ApprovalToken, TokenStore};
