//! `WhiteboardTransport` — the pluggable coordination-substrate abstraction
//! (v0.17.11.2 item 1). Selected at runtime via config exactly the way
//! `SourceAdapter`/`select_adapter` already selects `GitAdapter`/`SvnAdapter`
//! off `.ta/workflow.toml`'s `[submit] adapter = "git"` — this is the third
//! instance of that same TA convention (`SourceAdapter` for VCS, `PlanStore`
//! for plan storage as of v0.17.11.1, now `WhiteboardTransport` for
//! coordination).
//!
//! Primitives are shaped around what NATS JetStream natively provides (KV
//! with TTL for presence/liveness, durable streams+consumers for handoff
//! delivery) but are not NATS-specific in signature — `InMemoryTransport`
//! implements the identical trait with a plain in-process store, proving the
//! boundary is real (item 9's "a second, minimal implementation").
//!
//! **Deliberate v1 simplification**: no push-based `kv_watch` primitive.
//! NATS JetStream KV does support server-side watch, but a faithful
//! in-memory equivalent (a broadcast channel per bucket, correctly ordered
//! against concurrent writes) is meaningfully more complex than the current
//! consumer (`discovery.rs`) needs — every discovery query today is a
//! point-in-time snapshot (`kv_list` + filter), not a live subscription.
//! `kv_list` is polled instead. Revisit if a future caller genuinely needs
//! push notifications rather than periodic/on-demand snapshots.

use std::time::Duration;

use async_trait::async_trait;

use crate::error::Result;

/// A single durable message read off a handoff stream, with enough identity
/// to acknowledge it later via [`WhiteboardTransport::stream_ack`].
#[derive(Debug, Clone)]
pub struct StreamEnvelope {
    /// Opaque, transport-assigned identifier for this delivery. Stable
    /// across `stream_read_next` calls for the same underlying message
    /// (redelivered-but-unacked reads return the same `msg_id`).
    pub msg_id: String,
    pub payload: Vec<u8>,
}

/// The coordination-substrate abstraction. `NatsTransport` is the default,
/// recommended implementation; `InMemoryTransport` is a fully-functional
/// single-process implementation (useful standalone for single-agent/
/// no-coordination-needed setups per the design doc, and as the trait's
/// test double).
#[async_trait]
pub trait WhiteboardTransport: Send + Sync {
    /// Short, stable identifier for this backend (`"nats"`, `"memory"`),
    /// for logging/diagnostics.
    fn backend_name(&self) -> &str;

    /// Establish the underlying connection (idempotent — safe to call more
    /// than once). Must be called before any other method.
    async fn connect(&self) -> Result<()>;

    // ── KV (presence, discovery, task claiming) ─────────────────────────

    /// Upsert `key` in `bucket`. If `ttl` is set, the *bucket* is configured
    /// with that max-age on first use (see module doc — v1 uses bucket-wide
    /// TTL, not true per-key TTL) — every `kv_put` call, including this one,
    /// refreshes the entry's age, which is exactly the heartbeat-refresh
    /// semantics presence records need.
    async fn kv_put(
        &self,
        bucket: &str,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<()>;

    /// Create `key` in `bucket` **only if it does not already exist**.
    /// Returns `Ok(true)` if this call created the key, `Ok(false)` if the
    /// key was already present (someone else claimed it first) — the
    /// primitive `tasks.rs` builds self-claim on top of, avoiding the race
    /// a plain get-then-put would have.
    async fn kv_create(&self, bucket: &str, key: &str, value: Vec<u8>) -> Result<bool>;

    async fn kv_get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>>;

    async fn kv_delete(&self, bucket: &str, key: &str) -> Result<()>;

    /// All live (non-expired) entries in `bucket`, as `(key, value)` pairs.
    async fn kv_list(&self, bucket: &str) -> Result<Vec<(String, Vec<u8>)>>;

    // ── Durable streams (handoff messages) ──────────────────────────────

    /// Append `payload` to `stream` (created on first use). At-least-once,
    /// durable — a message survives no consumer being currently attached.
    async fn stream_append(&self, stream: &str, payload: Vec<u8>) -> Result<()>;

    /// Read the next unacknowledged message for `consumer` on `stream`
    /// (a durable, named cursor — created on first use). Returns `None` if
    /// there is nothing new to deliver. A message already delivered but not
    /// yet acked is redelivered on the next call.
    async fn stream_read_next(
        &self,
        stream: &str,
        consumer: &str,
    ) -> Result<Option<StreamEnvelope>>;

    /// Acknowledge `msg_id`, advancing `consumer`'s cursor past it.
    async fn stream_ack(&self, stream: &str, consumer: &str, msg_id: &str) -> Result<()>;
}
