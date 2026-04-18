// error.rs -- Event system error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EventError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("event not found: {0}")]
    NotFound(String),

    #[error("invalid token: {0}")]
    InvalidToken(String),

    #[error("token expired")]
    TokenExpired,

    #[error("hook execution failed: {0}")]
    HookFailed(String),

    #[error("bus error: {0}")]
    BusError(String),

    #[error("routing config error: {0}")]
    RoutingConfig(String),

    #[error("routing config parse error at {path}: {detail}")]
    RoutingParse { path: String, detail: String },

    #[error("protected event '{event_type}' cannot use strategy '{strategy}': {reason}")]
    ProtectedEvent {
        event_type: String,
        strategy: String,
        reason: String,
    },

    #[error("subscription not found: {0}")]
    SubscriptionNotFound(uuid::Uuid),

    #[error("subscription with name '{0}' already exists")]
    SubscriptionAlreadyExists(String),
}
