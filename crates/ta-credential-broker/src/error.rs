// error.rs — Credential broker error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error(
        "broker root key at {path} is missing or corrupt: {reason}. Deleting it \
         invalidates every outstanding grant (agents must request fresh ones); \
         restore the original key file from backup if grants must survive."
    )]
    KeyUnreadable {
        path: std::path::PathBuf,
        reason: String,
    },

    #[error("failed to mint grant: {0}")]
    MintFailed(String),

    #[error(
        "session grant failed verification: {0} (grants expire and cannot be \
         reused after tampering — mint a fresh one via `ta credentials grant`)"
    )]
    InvalidGrant(String),

    #[error("session grant was revoked")]
    Revoked,
}
