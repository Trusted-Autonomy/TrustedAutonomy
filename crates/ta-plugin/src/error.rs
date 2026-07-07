//! Shared error type for all Plugin-category (§2.2) integrations.

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin '{name}' not found")]
    NotFound { name: String },
    #[error("plugin '{name}' method '{method}' failed: {reason}")]
    CallFailed {
        name: String,
        method: String,
        reason: String,
    },
    #[error("plugin '{name}' method '{method}' returned an invalid response: {reason}")]
    InvalidResponse {
        name: String,
        method: String,
        reason: String,
    },
    #[error("failed to spawn plugin command '{command}': {reason}")]
    SpawnFailed { command: String, reason: String },
    #[error("plugin '{name}' method '{method}' timed out after {timeout_secs}s")]
    Timeout {
        name: String,
        method: String,
        timeout_secs: u64,
    },
    #[error("plugin manifest not found at {path}")]
    ManifestNotFound { path: String },
    #[error("invalid plugin manifest at {path}: {reason}")]
    InvalidManifest { path: String, reason: String },
    #[error("plugin manifest at {path} is missing the required 'command' field")]
    MissingCommand { path: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
