//! WorkspaceBackend — plugin trait for staging workspace storage (v0.14.4).
//!
//! Staging workspaces are temporary copies of a project that the agent works
//! in. The default [`LocalWorkspaceBackend`] stores them in `.ta/staging/`
//! on the local filesystem.
//!
//! Enterprise deployments may want shared workspaces (so multiple reviewers
//! can inspect the same staging copy) or remote storage (S3, GCS, NFS).
//! Implement this trait and register it via `[plugins].workspace` to swap
//! the storage layer without changing TA's core logic.
//!
//! ## Plugin registration
//!
//! ```toml
//! [plugins]
//! workspace = "ta-workspace-s3"
//! ```

use crate::ExtensionError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A resolved workspace location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePath {
    /// Unique identifier matching the goal ID.
    pub goal_id: String,
    /// URI used to identify and access the workspace.
    ///
    /// For local storage: `file:///home/user/.ta/staging/<goal_id>`.
    /// For remote: `s3://bucket/staging/<goal_id>` or similar.
    pub uri: String,
    /// Local filesystem path, if the workspace is locally accessible.
    /// `None` for fully remote backends.
    pub local_path: Option<PathBuf>,
}

/// Plugin trait for staging workspace storage.
///
/// The daemon calls these methods when creating, checking, and cleaning up
/// staging workspaces for goal runs.
///
/// # Stability contract (v0.14.4)
///
/// This interface is **stable**. SA plugins implement this trait against the
/// v0.14.4 release.
#[async_trait]
pub trait WorkspaceBackend: Send + Sync {
    /// Name for logging and diagnostics (e.g., `"local"`, `"s3"`, `"gcs"`).
    fn name(&self) -> &str;

    /// Create a new workspace for a goal.
    ///
    /// Implementations should be idempotent — if the workspace already exists,
    /// return its path rather than failing.
    async fn create_workspace(&self, goal_id: &str) -> Result<WorkspacePath, ExtensionError>;

    /// Check whether a workspace exists.
    async fn workspace_exists(&self, goal_id: &str) -> Result<bool, ExtensionError>;

    /// Remove a workspace and all its contents.
    ///
    /// Used during cleanup after apply, deny, or GC. Should not fail if the
    /// workspace doesn't exist (idempotent).
    async fn remove_workspace(&self, goal_id: &str) -> Result<(), ExtensionError>;

    /// List all workspace IDs currently stored.
    async fn list_workspaces(&self) -> Result<Vec<String>, ExtensionError>;
}

/// Default workspace backend — stores staging copies in `.ta/staging/`.
pub struct LocalWorkspaceBackend {
    staging_root: PathBuf,
}

impl LocalWorkspaceBackend {
    /// Create a backend rooted at `<project_root>/.ta/staging/`.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            staging_root: project_root.into().join(".ta").join("staging"),
        }
    }
}

#[async_trait]
impl WorkspaceBackend for LocalWorkspaceBackend {
    fn name(&self) -> &str {
        "local"
    }

    async fn create_workspace(&self, goal_id: &str) -> Result<WorkspacePath, ExtensionError> {
        let path = self.staging_root.join(goal_id);
        std::fs::create_dir_all(&path)?;
        Ok(WorkspacePath {
            goal_id: goal_id.to_string(),
            uri: format!("file://{}", path.display()),
            local_path: Some(path),
        })
    }

    async fn workspace_exists(&self, goal_id: &str) -> Result<bool, ExtensionError> {
        Ok(self.staging_root.join(goal_id).exists())
    }

    async fn remove_workspace(&self, goal_id: &str) -> Result<(), ExtensionError> {
        let path = self.staging_root.join(goal_id);
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        Ok(())
    }

    async fn list_workspaces(&self) -> Result<Vec<String>, ExtensionError> {
        if !self.staging_root.exists() {
            return Ok(vec![]);
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.staging_root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_backend_name() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalWorkspaceBackend::new(dir.path());
        assert_eq!(b.name(), "local");
    }

    #[tokio::test]
    async fn create_and_exists() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalWorkspaceBackend::new(dir.path());

        assert!(!b.workspace_exists("abc123").await.unwrap());
        let ws = b.create_workspace("abc123").await.unwrap();
        assert_eq!(ws.goal_id, "abc123");
        assert!(ws.local_path.unwrap().exists());
        assert!(b.workspace_exists("abc123").await.unwrap());
    }

    #[tokio::test]
    async fn create_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalWorkspaceBackend::new(dir.path());
        b.create_workspace("dup").await.unwrap();
        // Second call should not fail.
        b.create_workspace("dup").await.unwrap();
        assert!(b.workspace_exists("dup").await.unwrap());
    }

    #[tokio::test]
    async fn remove_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalWorkspaceBackend::new(dir.path());
        b.create_workspace("rm_me").await.unwrap();
        b.remove_workspace("rm_me").await.unwrap();
        assert!(!b.workspace_exists("rm_me").await.unwrap());
    }

    #[tokio::test]
    async fn remove_nonexistent_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalWorkspaceBackend::new(dir.path());
        assert!(b.remove_workspace("ghost").await.is_ok());
    }

    #[tokio::test]
    async fn list_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalWorkspaceBackend::new(dir.path());
        b.create_workspace("a").await.unwrap();
        b.create_workspace("b").await.unwrap();
        let mut ids = b.list_workspaces().await.unwrap();
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn list_empty_when_root_missing() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalWorkspaceBackend::new(dir.path().join("nonexistent"));
        let ids = b.list_workspaces().await.unwrap();
        assert!(ids.is_empty());
    }
}
