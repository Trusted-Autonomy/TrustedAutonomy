// rest.rs — ComfyUI REST API backend.
//
// Calls POST /prompt to submit workflows and GET /history/{id} to poll status.
// All HTTP calls are synchronous (reqwest::blocking).

use serde_json::{json, Value};

use crate::{
    backend::{ComfyUiBackend, JobState, JobStatus, ModelInfo},
    config::ComfyUiConnectorConfig,
    error::ComfyUiError,
};

pub struct RestBackend {
    url: String,
    client: reqwest::blocking::Client,
}

impl RestBackend {
    pub fn new(config: &ComfyUiConnectorConfig) -> Self {
        Self {
            url: config.url.trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/{}", self.url, path.trim_start_matches('/'))
    }
}

impl ComfyUiBackend for RestBackend {
    fn name(&self) -> &str {
        "rest"
    }

    fn base_url(&self) -> &str {
        &self.url
    }

    fn submit_workflow(
        &self,
        workflow_json: &str,
        inputs: Option<&Value>,
    ) -> Result<String, ComfyUiError> {
        let workflow: Value =
            serde_json::from_str(workflow_json).map_err(|e| ComfyUiError::Json(e.to_string()))?;

        // Merge caller-supplied input overrides into the workflow prompt.
        let mut prompt = workflow;
        if let Some(overrides) = inputs {
            if let (Some(obj), Some(ov_obj)) = (prompt.as_object_mut(), overrides.as_object()) {
                for (k, v) in ov_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }

        let body = json!({ "prompt": prompt });

        let resp = self
            .client
            .post(self.api_url("/prompt"))
            .json(&body)
            .send()
            .map_err(|e| ComfyUiError::NotReachable(self.url.clone(), e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ComfyUiError::Http(format!(
                "POST /prompt returned {}",
                resp.status()
            )));
        }

        let result: Value = resp.json().map_err(|e| ComfyUiError::Json(e.to_string()))?;

        let prompt_id = result["prompt_id"]
            .as_str()
            .ok_or_else(|| ComfyUiError::Json("missing prompt_id in response".into()))?
            .to_string();

        Ok(prompt_id)
    }

    fn poll_job(&self, job_id: &str) -> Result<JobStatus, ComfyUiError> {
        let resp = self
            .client
            .get(self.api_url(&format!("/history/{}", job_id)))
            .send()
            .map_err(|e| ComfyUiError::NotReachable(self.url.clone(), e.to_string()))?;

        if resp.status().as_u16() == 404 {
            return Err(ComfyUiError::JobNotFound(job_id.to_string()));
        }
        if !resp.status().is_success() {
            return Err(ComfyUiError::Http(format!(
                "GET /history/{} returned {}",
                job_id,
                resp.status()
            )));
        }

        let history: Value = resp.json().map_err(|e| ComfyUiError::Json(e.to_string()))?;

        // ComfyUI returns {} for unknown IDs, or { "<id>": { "outputs": {...}, "status": {...} } }
        if history.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            // Job is queued or not yet in history — treat as queued.
            return Ok(JobStatus {
                job_id: job_id.to_string(),
                state: JobState::Queued,
                progress: None,
                output_files: vec![],
            });
        }

        let job_data = &history[job_id];
        let status_obj = &job_data["status"];
        let completed = status_obj["completed"].as_bool().unwrap_or(false);
        let status_str = status_obj["status_str"].as_str().unwrap_or("");

        let state = if completed {
            JobState::Complete
        } else if status_str == "error" {
            JobState::Failed
        } else {
            JobState::Running
        };

        // Collect output file paths from outputs.
        let mut output_files = Vec::new();
        if let Some(outputs) = job_data["outputs"].as_object() {
            for node_outputs in outputs.values() {
                if let Some(images) = node_outputs["images"].as_array() {
                    for img in images {
                        if let Some(filename) = img["filename"].as_str() {
                            output_files.push(filename.to_string());
                        }
                    }
                }
                if let Some(videos) = node_outputs["videos"].as_array() {
                    for vid in videos {
                        if let Some(filename) = vid["filename"].as_str() {
                            output_files.push(filename.to_string());
                        }
                    }
                }
            }
        }

        Ok(JobStatus {
            job_id: job_id.to_string(),
            state,
            progress: if completed { Some(1.0) } else { None },
            output_files,
        })
    }

    fn cancel_job(&self, job_id: &str) -> Result<(), ComfyUiError> {
        let body = json!({ "delete": [job_id] });
        let resp = self
            .client
            .post(self.api_url("/queue"))
            .json(&body)
            .send()
            .map_err(|e| ComfyUiError::NotReachable(self.url.clone(), e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ComfyUiError::Http(format!(
                "POST /queue (cancel) returned {}",
                resp.status()
            )));
        }
        Ok(())
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ComfyUiError> {
        let resp = self
            .client
            .get(self.api_url("/object_info"))
            .send()
            .map_err(|e| ComfyUiError::NotReachable(self.url.clone(), e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ComfyUiError::Http(format!(
                "GET /object_info returned {}",
                resp.status()
            )));
        }

        let info: Value = resp.json().map_err(|e| ComfyUiError::Json(e.to_string()))?;

        // Extract model lists from CheckpointLoaderSimple, LoraLoader, VAELoader nodes.
        let mut models = Vec::new();
        let node_types = [
            ("CheckpointLoaderSimple", "checkpoints", "ckpt_name"),
            ("LoraLoader", "loras", "lora_name"),
            ("VAELoader", "vae", "vae_name"),
        ];

        for (node, model_type, input_key) in &node_types {
            if let Some(node_info) = info.get(*node) {
                if let Some(names) = node_info["input"]["required"][*input_key][0].as_array() {
                    for name in names {
                        if let Some(n) = name.as_str() {
                            models.push(ModelInfo {
                                name: n.to_string(),
                                model_type: model_type.to_string(),
                            });
                        }
                    }
                }
            }
        }

        Ok(models)
    }
}
