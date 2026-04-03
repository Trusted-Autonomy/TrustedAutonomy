use thiserror::Error;

#[derive(Debug, Error)]
pub enum ComfyUiError {
    #[error("ComfyUI server not reachable at '{0}': {1}")]
    NotReachable(String, String),
    #[error("job '{0}' not found")]
    JobNotFound(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("JSON error: {0}")]
    Json(String),
}
