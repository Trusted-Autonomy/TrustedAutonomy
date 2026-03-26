//! # ta-extension
//!
//! Plugin trait definitions for the Trusted Autonomy daemon extension surface.
//!
//! These traits define the boundary where SA (Secure Autonomy) and other
//! enterprise plugins connect to extend TA with remote access, authentication,
//! shared workspaces, and external review queues.
//!
//! TA ships local-first default implementations for every trait. Enterprise
//! capabilities (OIDC auth, shared workspace storage, external review queues,
//! SIEM audit sinks) are implemented by external plugins that register against
//! these traits via `[plugins]` in `daemon.toml`.
//!
//! ## Extension Points (v0.14.4)
//!
//! | Trait | Purpose | Default |
//! |---|---|---|
//! | [`TransportBackend`] | Network-exposed MCP transport | Local Unix socket |
//! | [`AuthMiddleware`] | Request authentication & identity | No-op (local single-user) |
//! | [`WorkspaceBackend`] | Staging workspace storage | Local filesystem |
//! | [`ReviewQueueBackend`] | Draft routing & review queues | Local queue |
//! | [`AuditStorageBackend`] | Audit log storage | Local JSONL file |
//!
//! ## Plugin Registration
//!
//! Plugins register by name in `daemon.toml`:
//!
//! ```toml
//! [plugins]
//! # transport = "ta-transport-websocket"
//! # auth = "ta-auth-oidc"
//! # workspace = "ta-workspace-s3"
//! # review_queue = "ta-review-jira"
//! # audit_storage = "ta-audit-splunk"
//! ```
//!
//! Each value is either a binary name (resolved from `$PATH` and
//! `.ta/plugins/`) or an absolute path to a plugin binary.
//! The daemon loads registered plugins at startup and wires them in place
//! of the local defaults.

pub mod audit;
pub mod auth;
pub mod plugin_config;
pub mod review_queue;
pub mod transport;
pub mod workspace;

pub use audit::{AuditStorageBackend, LocalAuditStorage, RawAuditEntry};
pub use auth::{AuthError, AuthMiddleware, AuthRequest, Identity, NoopAuthMiddleware, SessionInfo};
pub use plugin_config::PluginsConfig;
pub use review_queue::{LocalReviewQueue, ReviewDecision, ReviewQueueBackend, ReviewQueueEntry};
pub use transport::{LocalTransportBackend, TransportBackend, TransportPluginConfig};
pub use workspace::{LocalWorkspaceBackend, WorkspaceBackend, WorkspacePath};

/// Common error type for extension operations.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Plugin error: {0}")]
    Plugin(String),

    #[error("Not supported: {0}")]
    NotSupported(String),
}
