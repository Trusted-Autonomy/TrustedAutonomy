//! `ta-agent-whiteboard` — live coordination substrate for concurrently
//! running TA agents (v0.17.11.2). Complements `task-graph`'s static,
//! plan-time wave scheduling with a runtime presence/discovery/handoff
//! layer for conflicts that aren't knowable in advance — see
//! `docs/design/agent-coordination-whiteboard.md` for the full design.
//!
//! Layers, each transport-agnostic (built purely on [`WhiteboardTransport`],
//! never on a concrete backend directly):
//! - [`presence`] — TTL'd "what am I doing right now" records.
//! - [`discovery`] — query presence: who's active, who's touching what.
//! - [`tasks`] — a shared, dependency-aware, race-free-claim task list.
//! - [`handoff`] — durable, peer-to-peer messages.
//!
//! TA-core infrastructure, not a downstream-product concern — see the
//! design doc §1 and PLAN.md's v0.17.11.2 entry.

pub mod config;
pub mod discovery;
pub mod error;
pub mod handoff;
pub mod memory_transport;
pub mod nats_transport;
pub mod presence;
pub mod tasks;
pub mod transport;

pub use config::{select_transport, WhiteboardConfig};
pub use error::{Result, WhiteboardError};
pub use memory_transport::InMemoryTransport;
pub use nats_transport::NatsTransport;
pub use transport::{StreamEnvelope, WhiteboardTransport};
