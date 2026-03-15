//! Cargo build adapter — wraps `cargo build` and `cargo test`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::adapter::{BuildAdapter, BuildError, BuildResult, Result};

/// Build adapter for Rust projects using Cargo.
pub struct CargoAdapter {
    project_root: PathBuf,
    build_command: Option<String>,
    test_command: Option<String>,
}

impl CargoAdapter {
    /// Create a new CargoAdapter for the given project root.
    pub fn new(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            build_command: None,
            test_command: None,
        }
    }

    /// Create with custom build/test commands.
    pub fn with_commands(
        project_root: &Path,
        build_command: Option<String>,
        test_command: Option<String>,
    ) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            build_command,
            test_command,
        }
    }

    fn run_command(&self, cmd: &str) -> Result<BuildResult> {
        let start = Instant::now();

        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return Err(BuildError::CommandFailed("Empty command".to_string()));
        }

        let output = Command::new(parts[0])
            .args(&parts[1..])
            .current_dir(&self.project_root)
            .output()
            .map_err(|e| {
                BuildError::CommandFailed(format!(
                    "Failed to execute '{}': {}. Ensure '{}' is installed and in PATH.",
                    cmd, e, parts[0]
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

impl BuildAdapter for CargoAdapter {
    fn build(&self) -> Result<BuildResult> {
        let cmd = self
            .build_command
            .as_deref()
            .unwrap_or("cargo build --workspace");
        tracing::info!(adapter = "cargo", command = cmd, "Running build");
        self.run_command(cmd)
    }

    fn test(&self) -> Result<BuildResult> {
        let cmd = self
            .test_command
            .as_deref()
            .unwrap_or("cargo test --workspace");
        tracing::info!(adapter = "cargo", command = cmd, "Running tests");
        self.run_command(cmd)
    }

    fn name(&self) -> &str {
        "cargo"
    }

    fn detect(project_root: &Path) -> bool {
        project_root.join("Cargo.toml").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detect_cargo_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        assert!(CargoAdapter::detect(dir.path()));
    }

    #[test]
    fn detect_non_cargo_project() {
        let dir = tempdir().unwrap();
        assert!(!CargoAdapter::detect(dir.path()));
    }

    #[test]
    fn cargo_adapter_name() {
        let dir = tempdir().unwrap();
        let adapter = CargoAdapter::new(dir.path());
        assert_eq!(adapter.name(), "cargo");
    }

    #[test]
    fn cargo_adapter_with_custom_commands() {
        let dir = tempdir().unwrap();
        let adapter = CargoAdapter::with_commands(
            dir.path(),
            Some("echo build".to_string()),
            Some("echo test".to_string()),
        );
        let result = adapter.build().unwrap();
        assert!(result.success);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn cargo_adapter_build_captures_output() {
        let dir = tempdir().unwrap();
        let adapter =
            CargoAdapter::with_commands(dir.path(), Some("echo hello-build".to_string()), None);
        let result = adapter.build().unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("hello-build"));
    }

    #[test]
    fn cargo_adapter_test_captures_failure() {
        let dir = tempdir().unwrap();
        // Create a script that exits with failure
        let script_path = dir.path().join("fail.sh");
        std::fs::write(&script_path, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let adapter = CargoAdapter::with_commands(
            dir.path(),
            None,
            Some(script_path.to_string_lossy().to_string()),
        );
        let result = adapter.test().unwrap();
        assert!(!result.success);
        assert_eq!(result.exit_code, 1);
    }
}
