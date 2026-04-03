// stub.rs — In-memory stub backend for tests.

use std::sync::{Arc, Mutex};

use crate::{
    backend::{ComfyUiBackend, JobState, JobStatus, ModelInfo},
    error::ComfyUiError,
};

/// In-memory stub that tracks submitted jobs and returns deterministic responses.
pub struct StubBackend {
    base_url: String,
    /// Submitted jobs: job_id → JobStatus
    jobs: Arc<Mutex<std::collections::HashMap<String, JobStatus>>>,
    /// Counter for generating unique job IDs.
    counter: Arc<Mutex<u32>>,
}

impl StubBackend {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            counter: Arc::new(Mutex::new(0)),
        }
    }

    pub fn set_job_state(&self, job_id: &str, state: JobState) {
        if let Ok(mut jobs) = self.jobs.lock() {
            if let Some(job) = jobs.get_mut(job_id) {
                job.state = state;
            }
        }
    }
}

impl ComfyUiBackend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn submit_workflow(
        &self,
        _workflow_json: &str,
        _inputs: Option<&serde_json::Value>,
    ) -> Result<String, ComfyUiError> {
        let mut counter = self
            .counter
            .lock()
            .map_err(|_| ComfyUiError::Http("lock poisoned".into()))?;
        *counter += 1;
        let job_id = format!("stub-job-{:04}", *counter);

        let status = JobStatus {
            job_id: job_id.clone(),
            state: JobState::Queued,
            progress: None,
            output_files: vec![],
        };
        self.jobs
            .lock()
            .map_err(|_| ComfyUiError::Http("lock poisoned".into()))?
            .insert(job_id.clone(), status);

        Ok(job_id)
    }

    fn poll_job(&self, job_id: &str) -> Result<JobStatus, ComfyUiError> {
        let jobs = self
            .jobs
            .lock()
            .map_err(|_| ComfyUiError::Http("lock poisoned".into()))?;
        jobs.get(job_id)
            .cloned()
            .ok_or_else(|| ComfyUiError::JobNotFound(job_id.to_string()))
    }

    fn cancel_job(&self, job_id: &str) -> Result<(), ComfyUiError> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| ComfyUiError::Http("lock poisoned".into()))?;
        if let Some(job) = jobs.get_mut(job_id) {
            job.state = JobState::Cancelled;
            Ok(())
        } else {
            Err(ComfyUiError::JobNotFound(job_id.to_string()))
        }
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ComfyUiError> {
        Ok(vec![
            ModelInfo {
                name: "wan2.1_t2v_14B.safetensors".to_string(),
                model_type: "checkpoints".to_string(),
            },
            ModelInfo {
                name: "wan2.1_vace_14B.safetensors".to_string(),
                model_type: "checkpoints".to_string(),
            },
            ModelInfo {
                name: "wan_video_vae.safetensors".to_string(),
                model_type: "vae".to_string(),
            },
        ])
    }
}
