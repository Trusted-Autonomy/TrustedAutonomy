//! Script build adapter — runs user-defined commands from config.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::adapter::{BuildAdapter, BuildError, BuildResult, Result};

/// Build adapter that runs arbitrary user-defined commands.
///
/// Configured via `[build] command` and `[build] test_command` in
/// `.ta/workflow.toml`. Falls back to `make` / `make test` if the
/// project has a Makefile.
pub struct ScriptAdapter {
    project_root: PathBuf,
    build_command: String,
    test_command: String,
}

impl ScriptAdapter {
    /// Create a new ScriptAdapter with explicit commands.
    pub fn new(project_root: &Path, build_command: String, test_command: String) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            build_command,
            test_command,
        }
    }

    /// Create a ScriptAdapter that uses `make` / `make test`.
    pub fn make(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            build_command: "make".to_string(),
            test_command: "make test".to_string(),
        }
    }

    fn run_shell_command(&self, cmd: &str) -> Result<BuildResult> {
        let start = Instant::now();

        let output = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| {
                BuildError::CommandFailed(format!(
                    "Failed to execute '{}': {}. Ensure 'sh' is available.",
                    cmd, e
                ))
            })?;

        let duration = start.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        if output.status.success() {
            Ok(BuildResult::success(stdout, stderr, duration))
        } else {
            Ok(BuildResult::failure(exit_code, stdout, stderr, duration))
        }
    }
}

impl BuildAdapter for ScriptAdapter {
    fn build(&self) -> Result<BuildResult> {
        tracing::info!(
            adapter = "script",
            command = %self.build_command,
            "Running build"
        );
        self.run_shell_command(&self.build_command)
    }

    fn test(&self) -> Result<BuildResult> {
        tracing::info!(
            adapter = "script",
            command = %self.test_command,
            "Running tests"
        );
        self.run_shell_command(&self.test_command)
    }

    fn name(&self) -> &str {
        "script"
    }

    fn detect(project_root: &Path) -> bool {
        // Script adapter applies when a Makefile exists and no higher-priority
        // adapter was detected.
        project_root.join("Makefile").exists() || project_root.join("makefile").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detect_makefile_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), "all:\n\techo ok").unwrap();
        assert!(ScriptAdapter::detect(dir.path()));
    }

    #[test]
    fn detect_no_makefile() {
        let dir = tempdir().unwrap();
        assert!(!ScriptAdapter::detect(dir.path()));
    }

    #[test]
    fn script_adapter_name() {
        let dir = tempdir().unwrap();
        let adapter = ScriptAdapter::new(
            dir.path(),
            "echo build".to_string(),
            "echo test".to_string(),
        );
        assert_eq!(adapter.name(), "script");
    }

    #[test]
    fn script_adapter_runs_custom_command() {
        let dir = tempdir().unwrap();
        let adapter = ScriptAdapter::new(
            dir.path(),
            "echo hello-script".to_string(),
            "echo test-script".to_string(),
        );
        let result = adapter.build().unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("hello-script"));
    }

    #[test]
    fn script_adapter_captures_failure() {
        let dir = tempdir().unwrap();
        let adapter = ScriptAdapter::new(dir.path(), "exit 42".to_string(), "exit 1".to_string());
        let result = adapter.build().unwrap();
        assert!(!result.success);
        assert_eq!(result.exit_code, 42);
    }

    #[test]
    fn make_adapter() {
        let dir = tempdir().unwrap();
        let adapter = ScriptAdapter::make(dir.path());
        assert_eq!(adapter.name(), "script");
    }
}
