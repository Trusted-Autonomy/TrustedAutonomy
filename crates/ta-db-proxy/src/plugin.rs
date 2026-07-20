use std::net::SocketAddr;
use std::path::Path;

use crate::capture::{CaptureAction, CaptureHandle, CaptureParams};
use crate::classification::QueryClass;
use crate::error::Result;
use ta_db_overlay::OverlayEntry;
use ta_decision::Decision;

/// Configuration for starting a database proxy.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// The local address the proxy will listen on (e.g., `127.0.0.1:15432`).
    pub listen_addr: SocketAddr,
    /// The real database connection string (forwarded after policy checks).
    pub upstream_dsn: String,
    /// Path to the staging directory (for DraftOverlay storage).
    pub staging_dir: std::path::PathBuf,
}

/// Handle to a running proxy instance. Dropping stops the proxy.
pub trait ProxyHandle: Send {
    /// The address the proxy is listening on.
    fn listen_addr(&self) -> SocketAddr;
    /// Stop the proxy gracefully.
    fn stop(&mut self);
}

/// Database proxy plugin trait.
///
/// Each database type (SQLite, Postgres, MongoDB, etc.) provides its own
/// implementation. TA calls `start()` before the agent runs and `stop()` after.
///
/// The plugin intercepts all DB operations:
/// - READs: checked against DraftOverlay first (read-your-writes); if not in
///   overlay, forwarded to real DB.
/// - WRITEs: captured in DraftOverlay, not forwarded to real DB during the draft.
/// - DDL: captured in DraftOverlay as DDLMutation, flagged for reviewer approval.
pub trait DbProxyPlugin: Send + Sync {
    /// Human-readable name of this plugin (e.g., "sqlite", "postgres", "mongodb").
    fn name(&self) -> &str;

    /// Wire protocol identifier (e.g., "sqlite-vfs", "postgres", "mongodb").
    fn wire_protocol(&self) -> &str;

    /// Start the proxy and return a handle. The proxy listens on `config.listen_addr`
    /// and connects to `config.upstream_dsn`.
    fn start(&self, config: ProxyConfig) -> Result<Box<dyn ProxyHandle>>;

    /// Classify a raw query string into READ/WRITE/DDL/ADMIN/UNKNOWN.
    /// Used for policy enforcement before forwarding.
    fn classify_query(&self, query: &str) -> QueryClass;

    /// Review/Decision gate for a staged mutation (v0.17.0.12.15).
    ///
    /// Uses the same shared `ta-decision::decide()` function every other
    /// Commit-contract endpoint (VCS apply, social publish) uses — DDL and
    /// deletions never auto-approve; a missing pre-image on an update/delete
    /// downgrades confidence. Callers MUST call this and check
    /// `.is_auto_approvable()` before calling `apply_mutation` — this
    /// replaces the previously entirely-absent gate on this trait.
    fn review_mutation(&self, entry: &OverlayEntry) -> Decision {
        crate::review::review_mutation(entry, &ta_decision::DecisionThresholds::default())
    }

    /// Replay staged mutations against the real DB on `ta draft apply`.
    /// Called once per mutation in `DraftOverlay::list_mutations()` order.
    ///
    /// Callers must call `review_mutation()` first and only invoke this when
    /// the result is `is_auto_approvable()` — otherwise the mutation must go
    /// through a human (`Rework`/`Escalate`) or be discarded (`Reject`).
    fn apply_mutation(
        &self,
        upstream_dsn: &str,
        uri: &str,
        before: Option<&serde_json::Value>,
        after: &serde_json::Value,
        staging_dir: &Path,
    ) -> Result<()>;

    /// Begin change-capture for a goal (v0.17.1): a Postgres logical
    /// replication slot, a recorded MySQL binlog position, a SQLite
    /// shadow-copy snapshot — engine-specific. Called once at goal start,
    /// before the agent can reach the database at all.
    fn start_capture(&self, params: &CaptureParams) -> Result<CaptureHandle>;

    /// End change-capture for a goal, called once at `ta draft apply`
    /// (`CaptureAction::Apply`) or `ta draft deny` (`CaptureAction::Discard`).
    /// Must release the underlying capture resource (drop the replication
    /// slot, delete the shadow copy, ...) in both cases — a capture that
    /// only cleans up on `Apply` leaks the resource on every denied draft.
    ///
    /// `upstream_dsn` is resolved fresh from the credential vault by the
    /// caller for this call, exactly like `apply_mutation`'s — `handle.cursor`
    /// must never be relied on to carry it. `CaptureHandle` is written to
    /// disk by TA core between `start_capture` and `stop_capture` (see
    /// `capture.rs`'s doc comment), so a DSN embedded in `cursor` would leak
    /// plaintext DB credentials into that on-disk state.
    fn stop_capture(
        &self,
        upstream_dsn: &str,
        handle: &CaptureHandle,
        action: CaptureAction,
    ) -> Result<()>;
}
