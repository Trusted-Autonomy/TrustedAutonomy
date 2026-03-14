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
