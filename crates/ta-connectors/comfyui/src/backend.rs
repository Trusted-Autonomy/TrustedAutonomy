use serde::{Deserialize, Serialize};

use crate::error::ComfyUiError;

/// State of a ComfyUI inference job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Complete,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Status response for a ComfyUI job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatus {
    pub job_id: String,
    pub state: JobState,
    /// 0.0–1.0 progress (None if not yet started).
    pub progress: Option<f32>,
    /// Paths to output files (relative to ComfyUI output dir).
    pub output_files: Vec<String>,
}

/// A model advertised by ComfyUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    /// Model type, e.g. "checkpoints", "loras", "vae".
    pub model_type: String,
}

/// Trait implemented by each ComfyUI backend (rest, stub).
pub trait ComfyUiBackend: Send + Sync {
    /// Human-readable backend name ("rest" or "stub").
    fn name(&self) -> &str;

    /// Submit a workflow JSON and optional input overrides.
    /// Returns the job ID (prompt_id in ComfyUI terms).
    fn submit_workflow(
        &self,
        workflow_json: &str,
        inputs: Option<&serde_json::Value>,
    ) -> Result<String, ComfyUiError>;

    /// Poll the status of a job by ID.
    fn poll_job(&self, job_id: &str) -> Result<JobStatus, ComfyUiError>;

    /// Cancel a queued or running job.
    fn cancel_job(&self, job_id: &str) -> Result<(), ComfyUiError>;

    /// List models available in ComfyUI.
    fn list_models(&self) -> Result<Vec<ModelInfo>, ComfyUiError>;

    /// Return the base URL this backend targets.
    fn base_url(&self) -> &str;
}
