//! Error type shared across all whiteboard transports and domain layers.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, WhiteboardError>;

#[derive(Debug, Error)]
pub enum WhiteboardError {
    #[error("whiteboard transport not connected: {0}")]
    NotConnected(String),

    #[error("whiteboard connection failed: {0}")]
    ConnectFailed(String),

    #[error("whiteboard KV operation failed on bucket {bucket:?} key {key:?}: {detail}")]
    Kv {
        bucket: String,
        key: String,
        detail: String,
    },

    #[error("whiteboard stream operation failed on stream {stream:?}: {detail}")]
    Stream { stream: String, detail: String },

    #[error("serialization failed: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error("unknown whiteboard transport {0:?} in [whiteboard] config")]
    UnknownTransport(String),
}
