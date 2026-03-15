//! Core BuildAdapter trait and result types.
//!
//! The `BuildAdapter` trait abstracts over project build systems (cargo, npm,
//! make, etc.). Each adapter knows how to build and test a project, returning
//! structured results that flow through TA's event system.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during build operations.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("Build adapter not configured: {0}")]
    NotConfigured(String),

    #[error("Build command failed: {0}")]
    CommandFailed(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Build timed out after {0}s")]
    Timeout(u64),

    #[error("Webhook error: {0}")]
    WebhookError(String),
}

pub type Result<T> = std::result::Result<T, BuildError>;

/// Result of a build or test operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    /// Whether the build/test succeeded (exit code 0).
    pub success: bool,

    /// Process exit code.
    pub exit_code: i32,

    /// Captured stdout.
    pub stdout: String,

    /// Captured stderr.
    pub stderr: String,

    /// Wall-clock duration of the build/test.
    pub duration_secs: f64,
}

impl BuildResult {
    /// Create a successful result.
    pub fn success(stdout: String, stderr: String, duration: Duration) -> Self {
        Self {
            success: true,
            exit_code: 0,
            stdout,
            stderr,
            duration_secs: duration.as_secs_f64(),
        }
    }

    /// Create a failed result.
    pub fn failure(exit_code: i32, stdout: String, stderr: String, duration: Duration) -> Self {
        Self {
            success: false,
            exit_code,
            stdout,
            stderr,
            duration_secs: duration.as_secs_f64(),
        }
    }
}

/// Pluggable adapter for project build/test operations.
///
/// Implementations wrap specific build tools (cargo, npm, make, etc.)
/// and provide a uniform interface for building and testing projects.
pub trait BuildAdapter: Send + Sync {
    /// Build the project.
    ///
    /// For Cargo: `cargo build --workspace`
    /// For npm: `npm run build`
    /// For script: user-defined command
    fn build(&self) -> Result<BuildResult>;

    /// Run the project's test suite.
    ///
    /// For Cargo: `cargo test --workspace`
    /// For npm: `npm test`
    /// For script: user-defined test command
    fn test(&self) -> Result<BuildResult>;

    /// Adapter display name (for CLI output and events).
    fn name(&self) -> &str;

    /// Auto-detect whether this adapter applies to the given project root.
    ///
    /// Cargo: checks for Cargo.toml
    /// npm: checks for package.json
    fn detect(project_root: &Path) -> bool
    where
        Self: Sized,
    {
        let _ = project_root;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_result_success_constructor() {
        let result = BuildResult::success(
            "compiled".to_string(),
            "".to_string(),
            Duration::from_secs(5),
        );
        assert!(result.success);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "compiled");
        assert!(result.duration_secs >= 5.0);
    }

    #[test]
    fn build_result_failure_constructor() {
        let result = BuildResult::failure(
            1,
            "".to_string(),
            "error[E0308]".to_string(),
            Duration::from_secs(3),
        );
        assert!(!result.success);
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.stderr, "error[E0308]");
    }

    #[test]
    fn build_result_serialization_roundtrip() {
        let result = BuildResult::success("ok".to_string(), "".to_string(), Duration::from_secs(1));
        let json = serde_json::to_string(&result).unwrap();
        let restored: BuildResult = serde_json::from_str(&json).unwrap();
        assert!(restored.success);
        assert_eq!(restored.exit_code, 0);
    }
}
