// staging.rs — Ephemeral staging workspace for filesystem changes.
//
// A StagingWorkspace is a temporary directory where agents stage file
// changes before they are reviewed and applied to the real filesystem.
//
// Key design:
// - Each goal/iteration gets its own staging directory
// - Original file content is snapshotted when first read (for diff generation)
// - Modified files are written to the staging directory
// - Diffs are computed by comparing originals to current staged content
// - No git dependency — pure Rust diff computation

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::WorkspaceError;

/// An ephemeral staging workspace for filesystem changes.
///
/// Files are staged in a temporary directory. The workspace tracks
/// original content (snapshots) to generate diffs showing what changed.
pub struct StagingWorkspace {
    /// The goal this workspace is for.
    goal_id: String,

    /// Root of the staging directory (e.g., /tmp/ta-staging-xxxxx/).
    staging_dir: PathBuf,

    /// Snapshots of original file content, keyed by relative path.
    /// Used to compute diffs. If a file has no snapshot, it's a new file.
    originals: HashMap<String, Vec<u8>>,
}

impl StagingWorkspace {
    /// Create a new staging workspace in the given directory.
    ///
    /// `staging_root` is the parent dir; the workspace creates a subdirectory
    /// for this goal (e.g., `staging_root/goal-1/`).
    pub fn new(
        goal_id: impl Into<String>,
        staging_root: impl AsRef<Path>,
    ) -> Result<Self, WorkspaceError> {
        let goal_id = goal_id.into();
        let staging_dir = staging_root.as_ref().join(&goal_id);
        fs::create_dir_all(&staging_dir).map_err(|source| WorkspaceError::IoError {
            path: staging_dir.clone(),
            source,
        })?;

        Ok(Self {
            goal_id,
            staging_dir,
            originals: HashMap::new(),
        })
    }

    /// Get the goal ID.
    pub fn goal_id(&self) -> &str {
        &self.goal_id
    }

    /// Get the staging directory path.
    pub fn staging_path(&self) -> &Path {
        &self.staging_dir
    }

    /// Snapshot the original content of a file (before modification).
    ///
    /// Call this before staging a write so diffs can show what changed.
    /// If the file doesn't exist in the original source, don't snapshot it
    /// — it will be treated as a new file.
    pub fn snapshot_original(&mut self, relative_path: &str, content: Vec<u8>) {
        self.originals.insert(relative_path.to_string(), content);
    }

    /// Write a file to the staging directory.
    ///
    /// Returns an error if the path tries to escape the staging directory.
    pub fn write_file(&self, relative_path: &str, content: &[u8]) -> Result<(), WorkspaceError> {
        let full_path = self.resolve_path(relative_path)?;

        // Ensure parent directories exist.
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).map_err(|source| WorkspaceError::IoError {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        fs::write(&full_path, content).map_err(|source| WorkspaceError::IoError {
            path: full_path,
            source,
        })?;

        Ok(())
    }

    /// Read a file from the staging directory.
    pub fn read_file(&self, relative_path: &str) -> Result<Vec<u8>, WorkspaceError> {
        let full_path = self.resolve_path(relative_path)?;

        if !full_path.exists() {
            return Err(WorkspaceError::FileNotFound {
                path: relative_path.to_string(),
            });
        }

        fs::read(&full_path).map_err(|source| WorkspaceError::IoError {
            path: full_path,
            source,
        })
    }

    /// Generate a unified diff for a specific file.
    ///
    /// Compares the original snapshot (if any) against the current staged content.
    /// Returns None if the file hasn't been modified.
    pub fn diff_file(&self, relative_path: &str) -> Result<Option<String>, WorkspaceError> {
        let full_path = self.resolve_path(relative_path)?;

        if !full_path.exists() {
            return Ok(None);
        }

        let current = fs::read(&full_path).map_err(|source| WorkspaceError::IoError {
            path: full_path,
            source,
        })?;

        let original = self.originals.get(relative_path);

        match original {
            Some(orig) => {
                // Both exist — compute a diff.
                let orig_str = String::from_utf8_lossy(orig);
                let curr_str = String::from_utf8_lossy(&current);

                if orig_str == curr_str {
                    return Ok(None); // No change
                }

                Ok(Some(simple_unified_diff(
                    relative_path,
                    &orig_str,
                    &curr_str,
                )))
            }
            None => {
                // No original — this is a new file.
                let curr_str = String::from_utf8_lossy(&current);
                Ok(Some(new_file_diff(relative_path, &curr_str)))
            }
        }
    }

    /// List all files currently in the staging directory (relative paths).
    pub fn list_files(&self) -> Result<Vec<String>, WorkspaceError> {
        let mut files = Vec::new();
        self.walk_dir(&self.staging_dir, &self.staging_dir, &mut files)?;
        files.sort();
        Ok(files)
    }

    /// Clean up the staging directory.
    pub fn cleanup(self) -> Result<(), WorkspaceError> {
        if self.staging_dir.exists() {
            fs::remove_dir_all(&self.staging_dir).map_err(|source| WorkspaceError::IoError {
                path: self.staging_dir,
                source,
            })?;
        }
        Ok(())
    }

    /// Resolve a relative path to an absolute path within the staging dir.
    /// Rejects path traversal attempts.
    fn resolve_path(&self, relative_path: &str) -> Result<PathBuf, WorkspaceError> {
        // Reject obvious path traversal.
        if relative_path.contains("..") {
            return Err(WorkspaceError::PathTraversal {
                path: relative_path.to_string(),
            });
        }

        let full_path = self.staging_dir.join(relative_path);

        // Canonicalize isn't reliable on non-existent paths, so we check
        // the resolved path starts with the staging directory.
        // For new files, we verify the parent exists after creation.
        if !full_path.starts_with(&self.staging_dir) {
            return Err(WorkspaceError::PathTraversal {
                path: relative_path.to_string(),
            });
        }

        Ok(full_path)
    }

    /// Recursively walk a directory and collect relative file paths.
    fn walk_dir(
        &self,
        dir: &Path,
        root: &Path,
        files: &mut Vec<String>,
    ) -> Result<(), WorkspaceError> {
        if !dir.exists() {
            return Ok(());
        }

        let entries = fs::read_dir(dir).map_err(|source| WorkspaceError::IoError {
            path: dir.to_path_buf(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| WorkspaceError::IoError {
                path: dir.to_path_buf(),
                source,
            })?;
            let path = entry.path();

            if path.is_dir() {
                self.walk_dir(&path, root, files)?;
            } else {
                // Convert to relative path string with forward slashes for cross-platform consistency.
                if let Ok(rel) = path.strip_prefix(root) {
                    files.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }

        Ok(())
    }
}

/// Generate a simple unified diff between two strings.
///
/// This is a minimal line-by-line diff — not as sophisticated as a real
/// unified diff algorithm, but sufficient for the MVP. Shows removed lines
/// with "-" prefix and added lines with "+" prefix.
fn simple_unified_diff(path: &str, original: &str, modified: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!("--- a/{}\n", path));
    output.push_str(&format!("+++ b/{}\n", path));

    let orig_lines: Vec<&str> = original.lines().collect();
    let mod_lines: Vec<&str> = modified.lines().collect();

    // Simple diff: show all original lines as removed, all modified as added.
    // A proper diff algorithm (LCS) would be better, but this works for MVP.
    if orig_lines != mod_lines {
        output.push_str(&format!(
            "@@ -1,{} +1,{} @@\n",
            orig_lines.len(),
            mod_lines.len()
        ));
        for line in &orig_lines {
            output.push_str(&format!("-{}\n", line));
        }
        for line in &mod_lines {
            output.push_str(&format!("+{}\n", line));
        }
    }

    output
}

/// Generate a diff for a newly created file.
fn new_file_diff(path: &str, content: &str) -> String {
    let mut output = String::new();
    output.push_str("--- /dev/null\n");
    output.push_str(&format!("+++ b/{}\n", path));

    let lines: Vec<&str> = content.lines().collect();
    output.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for line in &lines {
        output.push_str(&format!("+{}\n", line));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_staging_workspace() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        assert!(ws.staging_path().exists());
        assert_eq!(ws.goal_id(), "goal-1");
    }

    #[test]
    fn write_and_read_file() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        ws.write_file("test.txt", b"hello world").unwrap();
        let content = ws.read_file("test.txt").unwrap();
        assert_eq!(content, b"hello world");
    }

    #[test]
    fn write_file_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        ws.write_file("src/deep/nested/file.txt", b"content")
            .unwrap();
        let content = ws.read_file("src/deep/nested/file.txt").unwrap();
        assert_eq!(content, b"content");
    }

    #[test]
    fn read_nonexistent_file_errors() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        let result = ws.read_file("nonexistent.txt");
        assert!(result.is_err());
    }

    #[test]
    fn path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        let result = ws.write_file("../escape.txt", b"malicious");
        assert!(matches!(result, Err(WorkspaceError::PathTraversal { .. })));
    }

    #[test]
    fn diff_new_file() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        ws.write_file("new.txt", b"line one\nline two").unwrap();
        let diff = ws.diff_file("new.txt").unwrap();

        assert!(diff.is_some());
        let diff = diff.unwrap();
        assert!(diff.contains("+++ b/new.txt"));
        assert!(diff.contains("+line one"));
        assert!(diff.contains("+line two"));
    }

    #[test]
    fn diff_modified_file() {
        let dir = tempdir().unwrap();
        let mut ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        // Snapshot original
        ws.snapshot_original("file.txt", b"original content".to_vec());

        // Write modified version
        ws.write_file("file.txt", b"modified content").unwrap();
        let diff = ws.diff_file("file.txt").unwrap();

        assert!(diff.is_some());
        let diff = diff.unwrap();
        assert!(diff.contains("-original content"));
        assert!(diff.contains("+modified content"));
    }

    #[test]
    fn diff_unchanged_file_returns_none() {
        let dir = tempdir().unwrap();
        let mut ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        ws.snapshot_original("file.txt", b"same content".to_vec());
        ws.write_file("file.txt", b"same content").unwrap();

        let diff = ws.diff_file("file.txt").unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn list_files() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();

        ws.write_file("a.txt", b"a").unwrap();
        ws.write_file("b.txt", b"b").unwrap();
        ws.write_file("sub/c.txt", b"c").unwrap();

        let files = ws.list_files().unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&"a.txt".to_string()));
        assert!(files.contains(&"b.txt".to_string()));
        assert!(files.contains(&"sub/c.txt".to_string()));
    }

    #[test]
    fn cleanup_removes_staging_dir() {
        let dir = tempdir().unwrap();
        let ws = StagingWorkspace::new("goal-1", dir.path()).unwrap();
        let staging_path = ws.staging_path().to_path_buf();

        ws.write_file("test.txt", b"data").unwrap();
        assert!(staging_path.exists());

        ws.cleanup().unwrap();
        assert!(!staging_path.exists());
    }
}
