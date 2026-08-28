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
//! - [`staged_conflicts`] — query actual staged (not-yet-applied) drafts
//!   for resource overlap, via the [`staged_conflicts::DraftLookup`] trait
//!   (v0.17.11.7) — strictly higher-signal than presence-declared intent,
//!   but not transport-based like the layers above (no NATS/whiteboard
//!   involvement — it reads staging state, supplied by the caller).
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
mod resource_match;
pub mod staged_conflicts;
pub mod tasks;
pub mod transport;

pub use config::{select_transport, WhiteboardConfig};
pub use error::{Result, WhiteboardError};
pub use memory_transport::InMemoryTransport;
pub use nats_transport::NatsTransport;
pub use transport::{StreamEnvelope, WhiteboardTransport};
