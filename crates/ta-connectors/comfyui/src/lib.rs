pub mod backend;
pub mod config;
pub mod error;
pub mod frame_watcher;
pub mod rest;
pub mod stub;

pub use backend::{ComfyUiBackend, JobState, JobStatus, ModelInfo};
pub use config::ComfyUiConnectorConfig;
pub use error::ComfyUiError;
pub use frame_watcher::{ComfyUiArtifact, ComfyUiOutputWatcher};
pub use stub::StubBackend;

/// Instantiate the default (REST) backend from config.
pub fn make_backend(config: &ComfyUiConnectorConfig) -> Box<dyn ComfyUiBackend> {
    Box::new(rest::RestBackend::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_parse_correctly() {
        let cfg = ComfyUiConnectorConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.url, "http://localhost:8188");
        assert!(cfg.output_dir.is_empty());
    }

    #[test]
    fn config_from_toml_full() {
        let toml = r#"
enabled = true
url = "http://192.168.1.100:8188"
output_dir = "/home/user/ComfyUI/output"
"#;
        let cfg = ComfyUiConnectorConfig::from_toml(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.url, "http://192.168.1.100:8188");
        assert_eq!(cfg.output_dir, "/home/user/ComfyUI/output");
    }

    #[test]
    fn config_from_toml_minimal() {
        let toml = r#"
enabled = true
"#;
        let cfg = ComfyUiConnectorConfig::from_toml(toml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.url, "http://localhost:8188");
    }

    #[test]
    fn stub_backend_submit_returns_job_id() {
        let stub = StubBackend::new("http://localhost:8188");
        let job_id = stub.submit_workflow("{}", None).unwrap();
        assert!(job_id.starts_with("stub-job-"));
    }

    #[test]
    fn stub_backend_poll_queued_after_submit() {
        let stub = StubBackend::new("http://localhost:8188");
        let job_id = stub.submit_workflow("{}", None).unwrap();
        let status = stub.poll_job(&job_id).unwrap();
        assert_eq!(status.state, JobState::Queued);
        assert_eq!(status.job_id, job_id);
    }

    #[test]
    fn stub_backend_poll_unknown_job_returns_error() {
        let stub = StubBackend::new("http://localhost:8188");
        let result = stub.poll_job("nonexistent-id");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent-id"),
            "error should mention job id: {}",
            err
        );
    }

    #[test]
    fn stub_backend_cancel_sets_cancelled_state() {
        let stub = StubBackend::new("http://localhost:8188");
        let job_id = stub.submit_workflow("{}", None).unwrap();
        stub.cancel_job(&job_id).unwrap();
        let status = stub.poll_job(&job_id).unwrap();
        assert_eq!(status.state, JobState::Cancelled);
    }

    #[test]
    fn stub_backend_list_models_returns_wan_models() {
        let stub = StubBackend::new("http://localhost:8188");
        let models = stub.list_models().unwrap();
        assert!(!models.is_empty(), "stub should return at least one model");
        assert!(
            models.iter().any(|m| m.name.contains("wan")),
            "stub should include a Wan2.1 model"
        );
    }

    #[test]
    fn stub_backend_name_is_stub() {
        let stub = StubBackend::new("http://localhost:8188");
        assert_eq!(stub.name(), "stub");
    }

    #[test]
    fn stub_backend_base_url() {
        let stub = StubBackend::new("http://localhost:8188");
        assert_eq!(stub.base_url(), "http://localhost:8188");
    }

    #[test]
    fn make_backend_returns_rest() {
        let cfg = ComfyUiConnectorConfig::default();
        let backend = make_backend(&cfg);
        assert_eq!(backend.name(), "rest");
        assert_eq!(backend.base_url(), "http://localhost:8188");
    }
}
