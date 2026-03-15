//! npm build adapter — wraps `npm run build` and `npm test`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::adapter::{BuildAdapter, BuildError, BuildResult, Result};

/// Build adapter for Node.js projects using npm.
pub struct NpmAdapter {
    project_root: PathBuf,
    build_command: Option<String>,
    test_command: Option<String>,
}

impl NpmAdapter {
    /// Create a new NpmAdapter for the given project root.
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

impl BuildAdapter for NpmAdapter {
    fn build(&self) -> Result<BuildResult> {
        let cmd = self.build_command.as_deref().unwrap_or("npm run build");
        tracing::info!(adapter = "npm", command = cmd, "Running build");
        self.run_command(cmd)
    }

    fn test(&self) -> Result<BuildResult> {
        let cmd = self.test_command.as_deref().unwrap_or("npm test");
        tracing::info!(adapter = "npm", command = cmd, "Running tests");
        self.run_command(cmd)
    }

    fn name(&self) -> &str {
        "npm"
    }

    fn detect(project_root: &Path) -> bool {
        project_root.join("package.json").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detect_npm_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert!(NpmAdapter::detect(dir.path()));
    }

    #[test]
    fn detect_non_npm_project() {
        let dir = tempdir().unwrap();
        assert!(!NpmAdapter::detect(dir.path()));
    }

    #[test]
    fn npm_adapter_name() {
        let dir = tempdir().unwrap();
        let adapter = NpmAdapter::new(dir.path());
        assert_eq!(adapter.name(), "npm");
    }

    #[test]
    fn npm_adapter_with_custom_commands() {
        let dir = tempdir().unwrap();
        let adapter = NpmAdapter::with_commands(
            dir.path(),
            Some("echo build-ok".to_string()),
            Some("echo test-ok".to_string()),
        );
        let result = adapter.build().unwrap();
        assert!(result.success);
    }
}
