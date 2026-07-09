//! The normalized event shape every trigger type produces (§13/§13.1,
//! `docs/design/ta-concepts-and-architecture.md`).
//!
//! `ta-intake` owns only "an event of type X arrived, here's its normalized
//! payload" — nothing about what to do with it. Turning a `TriggerEvent`
//! into a goal (directly or via a queue) is the consumer's job: the `ta
//! intake fire` CLI glue added alongside this crate for end-to-end
//! demonstration today, and `ta-brain` (v0.17.0.12.20) going forward.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single normalized event fired by a `TriggerSource`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct TriggerEvent {
    /// Unique identifier for this event instance.
    pub id: Uuid,

    /// The trigger type that produced this event (matches the `type` field
    /// in `.ta/triggers/<type>.toml` and the config's discovery filename).
    pub trigger_type: String,

    /// Human-readable origin of this specific event (e.g. a sender address
    /// for `inbound-email`, or the trigger's config name for `schedule`).
    pub source: String,

    /// When the underlying event actually happened (not when it was polled).
    pub occurred_at: DateTime<Utc>,

    /// Trigger-type-specific normalized data. Deliberately untyped at this
    /// layer — `ta-intake` doesn't know or care what a consumer will do with
    /// it, only that it round-trips as JSON.
    pub payload: serde_json::Value,

    /// A ready-to-use goal title, so a direct-dispatch consumer doesn't need
    /// to know how to turn a given trigger type's payload into a title.
    pub suggested_goal_title: String,

    /// Stable key for idempotency (e.g. a provider message ID, or a fired
    /// timestamp) — lets a consumer skip an event it has already acted on.
    pub dedupe_key: Option<String>,
}

/// Errors a `TriggerSource` implementation can report.
#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error("trigger config not found at {path}")]
    ConfigNotFound { path: String },

    #[error("invalid trigger config at {path}: {reason}")]
    InvalidConfig { path: String, reason: String },

    #[error("trigger '{trigger_type}' poll failed: {reason}")]
    PollFailed {
        trigger_type: String,
        reason: String,
    },
}

/// Produces normalized `TriggerEvent`s for a single trigger type.
///
/// Implementations own polling/connecting to whatever produces the raw
/// event (a clock, a mailbox, a webhook queue) and normalizing it — nothing
/// about what happens after an event is produced.
pub trait TriggerSource {
    /// The trigger type this source implements (e.g. `"schedule"`,
    /// `"inbound-email"`). Matches `TriggerManifest::trigger_type`.
    fn trigger_type(&self) -> &str;

    /// Poll for events that occurred since `since` (an RFC 3339 timestamp
    /// watermark), or all available events if `since` is `None` (first
    /// poll). Returns an empty `Vec` when nothing new fired — that is not
    /// an error.
    fn poll(&self, since: Option<&str>) -> Result<Vec<TriggerEvent>, TriggerError>;
}
