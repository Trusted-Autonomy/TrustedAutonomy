//! Error type for release adapter operations.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error("adapter '{adapter}' does not support operation '{operation}'")]
    Unsupported { adapter: String, operation: String },

    #[error("no release adapter resolved for publish_url '{0}' — check `.release.toml` [release] publish_url or pass --adapter")]
    NoAdapterResolved(String),

    #[error("adapter '{adapter}' preflight failed: {reason}")]
    PrepareFailed { adapter: String, reason: String },

    #[error("adapter '{adapter}' publish failed: {reason}")]
    PublishFailed { adapter: String, reason: String },

    #[error("adapter '{adapter}' requires a semver version, got non-semver label '{label}'")]
    SemverRequired { adapter: String, label: String },

    #[error("channel '{0}' is not recognized by this adapter (see `ta release adapters` for its custom_channel_names)")]
    UnknownChannel(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error("subprocess '{command}' failed: {reason}")]
    SubprocessFailed { command: String, reason: String },
}

pub type Result<T> = std::result::Result<T, ReleaseError>;
