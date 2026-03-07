use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("workflow not found: {id}")]
    NotFound { id: String },

    #[error("workflow '{id}' is not in a state that allows this operation (current: {state})")]
    InvalidState { id: String, state: String },

    #[error("stage '{stage}' not found in workflow '{workflow_id}'")]
    StageNotFound { workflow_id: String, stage: String },

    #[error("workflow '{id}' has a dependency cycle involving stage '{stage}'")]
    CycleDetected { id: String, stage: String },

    #[error("stage '{stage}' in workflow '{workflow_id}' exceeded max retries ({max})")]
    MaxRetriesExceeded {
        workflow_id: String,
        stage: String,
        max: u32,
    },

    #[error("failed to parse workflow definition: {reason}")]
    ParseError { reason: String },

    #[error("process engine error: {reason}")]
    ProcessError { reason: String },

    #[error("I/O error at {path}: {source}")]
    IoError {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Other(String),
}
