// error.rs — Credential vault error types.

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("credential not found: {0}")]
    NotFound(Uuid),

    #[error("credential with name '{0}' already exists")]
    DuplicateName(String),

    #[error("session token not found: {0}")]
    TokenNotFound(Uuid),

    #[error("session token expired: {0}")]
    TokenExpired(Uuid),

    #[error("vault error: {0}")]
    Other(String),
}
