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

    #[error(
        "vault encryption key at {path} is missing or corrupt: {reason}. \
         Delete the file to generate a fresh key (existing encrypted credentials \
         will become unreadable and must be re-added), or restore the original \
         key file from backup. Run `ta doctor` for key custody details."
    )]
    KeyUnreadable {
        path: std::path::PathBuf,
        reason: String,
    },

    #[error(
        "OS keychain unavailable for vault key custody ({reason}); falling back \
         to a chmod-0600 key file"
    )]
    KeychainUnavailable { reason: String },

    #[error(
        "failed to decrypt vault at {path}: {reason}. This usually means the \
         encryption key was lost, rotated, or replaced with a mismatched one — \
         existing credentials cannot be recovered without the original key. \
         Run `ta doctor` to check key custody status."
    )]
    DecryptionFailed {
        path: std::path::PathBuf,
        reason: String,
    },

    #[error("failed to encrypt vault: {0}")]
    EncryptionFailed(String),

    #[error("vault error: {0}")]
    Other(String),
}
