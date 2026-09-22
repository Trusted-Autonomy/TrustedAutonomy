// error.rs — Error types for the goal lifecycle subsystem.

use thiserror::Error;
use uuid::Uuid;

/// Errors that can occur during goal lifecycle operations.
#[derive(Debug, Error)]
pub enum GoalError {
    /// A file I/O operation failed.
    #[error("I/O error at {path}: {source}")]
    IoError {
        path: String,
        source: std::io::Error,
    },

    /// Failed to serialize/deserialize goal data.
    #[error("serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// The requested GoalRun was not found.
    #[error("goal run not found: {0}")]
    NotFound(Uuid),

    /// Invalid state transition.
    #[error("invalid transition from {from} to {to} for goal {goal_run_id}")]
    InvalidTransition {
        goal_run_id: Uuid,
        from: String,
        to: String,
    },

    /// A notification dispatch failed (non-fatal).
    #[error("notification error: {0}")]
    NotificationError(String),

    /// Failed to parse a config file (e.g. TOML) into its expected structure.
    #[error("parse error: {0}")]
    ParseError(String),
}
