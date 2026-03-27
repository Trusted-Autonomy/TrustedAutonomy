// error.rs — Memory store error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("memory entry not found: {0}")]
    NotFound(String),

    #[error("vector database error: {0}")]
    VectorDb(String),

    /// External plugin error (spawn failure, protocol mismatch, backend error).
    #[error("memory plugin error: {0}")]
    Plugin(String),
}
