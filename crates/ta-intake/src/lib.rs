//! `ta-intake` — tier 1 of the 3-tier request model
//! (`docs/design/ta-concepts-and-architecture.md` §13/§13.1): a first-class
//! trigger abstraction so goal creation can be fed by more than an explicit
//! `ta run`/MCP call.
//!
//! This crate is a library only — no CLI or daemon-specific glue. It owns
//! exactly one thing: "an event of type X arrived, here's its normalized
//! payload" (`TriggerEvent`, produced by a `TriggerSource`). What happens
//! next — routing, privilege derivation, goal creation — is explicitly out
//! of scope here (`ta-brain`, v0.17.0.12.20).
//!
//! Per-type trigger configs are data (`.ta/triggers/<type>.toml`), the same
//! pattern established for plugins (v0.17.0.12.14) and personas
//! (v0.17.0.12.12) — see `manifest::TriggerManifest`.

pub mod discovery;
pub mod email;
pub mod event;
pub mod manifest;
pub mod schedule;

pub use discovery::{discover_triggers, find_trigger, DiscoveredTrigger};
pub use email::{EmailTriggerSource, MessageFetcher};
pub use event::{TriggerError, TriggerEvent, TriggerSource};
pub use manifest::{Dispatch, TriggerManifest};
pub use schedule::ScheduleTriggerSource;
