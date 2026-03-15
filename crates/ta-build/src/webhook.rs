//! Webhook build adapter — POST to external CI, poll for result.
//!
//! This is a stub implementation. Full webhook support (endpoint configuration,
//! result polling, authentication) will be fleshed out in a future phase.

use std::path::Path;

use crate::adapter::{BuildAdapter, BuildError, BuildResult, Result};

/// Build adapter that triggers builds via HTTP webhooks.
///
/// Sends a POST to an external CI system and polls for the result.
/// Useful for projects that use cloud CI/CD pipelines (GitHub Actions,
/// GitLab CI, Jenkins, etc.).
pub struct WebhookAdapter {
    /// The webhook URL to POST to for build triggers.
    pub build_url: String,
    /// The webhook URL to POST to for test triggers (optional, defaults to build_url).
    pub test_url: Option<String>,
}

impl WebhookAdapter {
    /// Create a new WebhookAdapter with the given webhook URL.
    pub fn new(build_url: String) -> Self {
        Self {
            build_url,
            test_url: None,
        }
    }

    /// Create with separate build and test URLs.
    pub fn with_test_url(build_url: String, test_url: String) -> Self {
        Self {
            build_url,
            test_url: Some(test_url),
        }
    }
}

impl BuildAdapter for WebhookAdapter {
    fn build(&self) -> Result<BuildResult> {
        tracing::info!(adapter = "webhook", url = %self.build_url, "Webhook build not yet implemented");
        Err(BuildError::WebhookError(
            "Webhook build adapter is not yet implemented. \
             Configure a local build adapter (cargo, npm, script) instead, \
             or wait for webhook support in a future release."
                .to_string(),
        ))
    }

    fn test(&self) -> Result<BuildResult> {
        let url = self.test_url.as_deref().unwrap_or(&self.build_url);
        tracing::info!(adapter = "webhook", url = %url, "Webhook test not yet implemented");
        Err(BuildError::WebhookError(
            "Webhook test adapter is not yet implemented. \
             Configure a local build adapter (cargo, npm, script) instead, \
             or wait for webhook support in a future release."
                .to_string(),
        ))
    }

    fn name(&self) -> &str {
        "webhook"
    }

    fn detect(_project_root: &Path) -> bool {
        // Webhook adapter is never auto-detected; it must be explicitly configured.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_adapter_name() {
        let adapter = WebhookAdapter::new("https://ci.example.com/build".to_string());
        assert_eq!(adapter.name(), "webhook");
    }

    #[test]
    fn webhook_build_returns_not_implemented() {
        let adapter = WebhookAdapter::new("https://ci.example.com/build".to_string());
        let result = adapter.build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }

    #[test]
    fn webhook_test_returns_not_implemented() {
        let adapter = WebhookAdapter::new("https://ci.example.com/build".to_string());
        let result = adapter.test();
        assert!(result.is_err());
    }

    #[test]
    fn webhook_never_auto_detected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!WebhookAdapter::detect(dir.path()));
    }
}
