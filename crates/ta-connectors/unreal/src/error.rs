use thiserror::Error;

#[derive(Debug, Error)]
pub enum UnrealConnectorError {
    #[error("backend '{0}' is not installed; run `ta connector install unreal --backend {0}`")]
    NotInstalled(String),
    #[error("backend process failed to start: {0}")]
    SpawnFailed(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported backend: '{0}' (valid: kvick, flopperam, special-agent)")]
    UnsupportedBackend(String),
}
