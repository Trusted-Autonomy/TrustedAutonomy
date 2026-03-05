// conflict.rs — Concurrent session conflict detection for overlay workspaces.
//
// When a source directory is copied to staging at goal start time, we snapshot
// the source file state (mtime + content hash). Before applying changes back,
// we check if the source has diverged — another process may have edited files
// while the agent was working in staging.
//
// Phase v0.2.1: Basic mtime/hash comparison with configurable resolution.
// Future: VCS adapter integration for smart 3-way merge (git merge, p4 resolve).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::WorkspaceError;

/// Strategy for resolving conflicts when source has changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    /// Abort the apply operation — safest default.
    #[default]
    Abort,
    /// Overwrite source with staging changes regardless of conflicts.
    /// WARNING: May lose uncommitted work in source.
    ForceOverwrite,
    /// Attempt automatic merge using VCS adapter (git merge, etc.).
    /// Falls back to Abort if no VCS adapter is available.
    Merge,
}

/// Snapshot of a single file's state at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Relative path from workspace root.
    pub path: String,
    /// File modification time (seconds since UNIX epoch).
    pub mtime_secs: u64,
    /// SHA-256 hash of file content (hex string).
    pub content_hash: String,
    /// File size in bytes (for quick sanity check).
    pub size_bytes: u64,
}

impl FileSnapshot {
    /// Create a snapshot of a file.
    pub fn capture(root: &Path, rel_path: &str) -> Result<Self, WorkspaceError> {
        let abs_path = root.join(rel_path);
        let metadata = fs::metadata(&abs_path).map_err(|source| WorkspaceError::IoError {
            path: abs_path.clone(),
            source,
        })?;

        let mtime_secs = metadata
            .modified()
            .map_err(|source| WorkspaceError::IoError {
                path: abs_path.clone(),
                source,
            })?
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let content = fs::read(&abs_path).map_err(|source| WorkspaceError::IoError {
            path: abs_path.clone(),
            source,
        })?;

        let size_bytes = content.len() as u64;
        let content_hash = format!("{:x}", Sha256::digest(&content));

        Ok(Self {
            path: rel_path.to_string(),
            mtime_secs,
            content_hash,
            size_bytes,
        })
    }

    /// Check if the current file state differs from this snapshot.
    /// Returns true if the file has changed (mtime OR content hash differs).
    pub fn has_changed(&self, root: &Path) -> Result<bool, WorkspaceError> {
        let abs_path = root.join(&self.path);

        if !abs_path.exists() {
            // File was deleted since snapshot.
            return Ok(true);
        }

        let metadata = fs::metadata(&abs_path).map_err(|source| WorkspaceError::IoError {
            path: abs_path.clone(),
            source,
        })?;

        let current_mtime = metadata
            .modified()
            .map_err(|source| WorkspaceError::IoError {
                path: abs_path.clone(),
                source,
            })?
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Quick mtime check first (fast path).
        if current_mtime != self.mtime_secs {
            // Mtime differs — check content hash to confirm (mtime can be unreliable).
            let content = fs::read(&abs_path).map_err(|source| WorkspaceError::IoError {
                path: abs_path.clone(),
                source,
            })?;
            let current_hash = format!("{:x}", Sha256::digest(&content));
            return Ok(current_hash != self.content_hash);
        }

        Ok(false)
    }
}

/// A conflict detected between source and staging.
#[derive(Debug, Clone)]
pub struct Conflict {
    /// File path relative to workspace root.
    pub path: String,
    /// Snapshot state at goal start.
    pub snapshot: FileSnapshot,
    /// Whether the file currently exists in source.
    pub source_exists: bool,
    /// Human-readable description of the conflict.
    pub description: String,
}

/// Snapshot of all source files at goal start time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnapshot {
    /// Map of relative path -> file snapshot.
    pub files: HashMap<String, FileSnapshot>,
    /// When the snapshot was taken.
    pub created_at: u64, // seconds since UNIX epoch
}

impl SourceSnapshot {
    /// Capture a snapshot of all files in a directory tree.
    /// Excludes directories matching `should_skip` predicate.
    pub fn capture<F>(root: &Path, should_skip: F) -> Result<Self, WorkspaceError>
    where
        F: Fn(&str) -> bool,
    {
        let mut files = HashMap::new();
        let mut pending = vec![PathBuf::new()]; // Start with root (empty relative path).

        while let Some(rel_dir) = pending.pop() {
            let abs_dir = root.join(&rel_dir);
            if !abs_dir.exists() {
                continue;
            }

            let entries = fs::read_dir(&abs_dir).map_err(|source| WorkspaceError::IoError {
                path: abs_dir.clone(),
                source,
            })?;

            for entry in entries {
                let entry = entry.map_err(|source| WorkspaceError::IoError {
                    path: abs_dir.clone(),
                    source,
                })?;
                let file_name = entry.file_name();
                let rel_path = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&file_name)
                } else {
                    rel_dir.join(&file_name)
                };
                let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");

                // Skip infrastructure directories (always) and user-excluded paths.
                if is_infra_path(&rel_path_str) || should_skip(&rel_path_str) {
                    continue;
                }

                let abs_path = entry.path();
                if abs_path.is_dir() {
                    pending.push(rel_path);
                } else {
                    let snapshot = FileSnapshot::capture(root, &rel_path_str)?;
                    files.insert(rel_path_str, snapshot);
                }
            }
        }

        let created_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Self { files, created_at })
    }

    /// Detect conflicts between this snapshot and the current source state.
    /// Returns a list of files that have changed since the snapshot.
    /// The `should_skip` predicate filters paths from the "new file" scan,
    /// matching the same patterns used during snapshot capture (e.g., target/, node_modules/).
    pub fn detect_conflicts<F>(
        &self,
        source_root: &Path,
        should_skip: F,
    ) -> Result<Vec<Conflict>, WorkspaceError>
    where
        F: Fn(&str) -> bool,
    {
        let mut conflicts = Vec::new();

        for (path, snapshot) in &self.files {
            let abs_path = source_root.join(path);
            let source_exists = abs_path.exists();

            if snapshot.has_changed(source_root)? {
                let description = if !source_exists {
                    format!("File '{}' was deleted from source", path)
                } else {
                    format!(
                        "File '{}' was modified in source (mtime/hash changed)",
                        path
                    )
                };

                conflicts.push(Conflict {
                    path: path.clone(),
                    snapshot: snapshot.clone(),
                    source_exists,
                    description,
                });
            }
        }

        // Also check for new files in source (not in snapshot).
        // These aren't conflicts per se, but inform the user that source has diverged.
        // We'll walk the current source tree to find new files.
        let mut current_files = HashMap::new();
        let mut pending = vec![PathBuf::new()];

        while let Some(rel_dir) = pending.pop() {
            let abs_dir = source_root.join(&rel_dir);
            if !abs_dir.exists() {
                continue;
            }

            let entries = match fs::read_dir(&abs_dir) {
                Ok(e) => e,
                Err(_) => continue, // Skip inaccessible dirs.
            };

            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let rel_path = if rel_dir.as_os_str().is_empty() {
                    PathBuf::from(&file_name)
                } else {
                    rel_dir.join(&file_name)
                };
                let rel_path_str = rel_path.to_string_lossy().replace('\\', "/");

                // Skip infra dirs and user-excluded paths (target/, node_modules/, etc.).
                if is_infra_path(&rel_path_str) || should_skip(&rel_path_str) {
                    continue;
                }

                let abs_path = entry.path();
                if abs_path.is_dir() {
                    pending.push(rel_path);
                } else {
                    current_files.insert(rel_path_str, ());
                }
            }
        }

        for path in current_files.keys() {
            if !self.files.contains_key(path) {
                // New file in source — not a conflict, but worth noting.
                conflicts.push(Conflict {
                    path: path.clone(),
                    snapshot: FileSnapshot {
                        path: path.clone(),
                        mtime_secs: 0,
                        content_hash: String::new(),
                        size_bytes: 0,
                    },
                    source_exists: true,
                    description: format!(
                        "File '{}' was created in source (new since snapshot)",
                        path
                    ),
                });
            }
        }

        conflicts.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(conflicts)
    }

    /// Count the number of files in the snapshot.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// Check if a path is an infrastructure directory (agent runtime, VCS, etc.).
/// These are always excluded from snapshots and conflict detection.
fn is_infra_path(path: &str) -> bool {
    const INFRA_DIRS: &[&str] = &[".ta", ".git", ".claude-flow", ".hive-mind", ".swarm"];

    for dir in INFRA_DIRS {
        if path == *dir
            || path.starts_with(&format!("{}/", dir))
            || path.starts_with(&format!("{}\\", dir))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, rel_path: &str, content: &str) {
        let abs_path = dir.join(rel_path);
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&abs_path, content).unwrap();
    }

    #[test]
    fn file_snapshot_captures_state() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "file.txt", "hello world");

        let snapshot = FileSnapshot::capture(dir.path(), "file.txt").unwrap();
        assert_eq!(snapshot.path, "file.txt");
        assert_eq!(snapshot.size_bytes, 11);
        assert!(!snapshot.content_hash.is_empty());
    }

    #[test]
    fn file_snapshot_detects_content_change() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "file.txt", "original");

        let snapshot = FileSnapshot::capture(dir.path(), "file.txt").unwrap();
        assert!(!snapshot.has_changed(dir.path()).unwrap());

        // Modify content (and wait to ensure mtime changes).
        // Note: on some file systems, mtime granularity is 1-2 seconds.
        thread::sleep(Duration::from_secs(2));
        create_test_file(dir.path(), "file.txt", "modified");

        assert!(snapshot.has_changed(dir.path()).unwrap());
    }

    #[test]
    fn file_snapshot_detects_deletion() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "file.txt", "content");

        let snapshot = FileSnapshot::capture(dir.path(), "file.txt").unwrap();
        fs::remove_file(dir.path().join("file.txt")).unwrap();

        assert!(snapshot.has_changed(dir.path()).unwrap());
    }

    #[test]
    fn source_snapshot_captures_tree() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "README.md", "# Test");
        create_test_file(dir.path(), "src/main.rs", "fn main() {}");
        create_test_file(dir.path(), "src/lib.rs", "pub fn hello() {}");
        // Create infra dir that should be excluded.
        create_test_file(dir.path(), ".ta/state.json", "{}");

        let snapshot = SourceSnapshot::capture(dir.path(), |_| false).unwrap();
        // .ta/ should be auto-excluded, so we expect 3 files.
        let expected = 3;
        let actual = snapshot.file_count();
        assert_eq!(
            actual,
            expected,
            "Expected {} files but got {}. Files: {:?}",
            expected,
            actual,
            snapshot.files.keys().collect::<Vec<_>>()
        );
        assert!(snapshot.files.contains_key("README.md"));
        assert!(snapshot.files.contains_key("src/main.rs"));
        assert!(snapshot.files.contains_key("src/lib.rs"));
        // .ta/ should be auto-excluded.
        assert!(!snapshot.files.contains_key(".ta/state.json"));
    }

    #[test]
    fn detect_conflicts_finds_modified_files() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "a.txt", "original A");
        create_test_file(dir.path(), "b.txt", "original B");

        let snapshot = SourceSnapshot::capture(dir.path(), |_| false).unwrap();

        // Modify one file (wait for mtime granularity).
        thread::sleep(Duration::from_secs(2));
        create_test_file(dir.path(), "a.txt", "modified A");

        let conflicts = snapshot.detect_conflicts(dir.path(), |_| false).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "a.txt");
        assert!(conflicts[0].description.contains("modified"));
    }

    #[test]
    fn detect_conflicts_finds_deleted_files() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "file.txt", "content");

        let snapshot = SourceSnapshot::capture(dir.path(), |_| false).unwrap();
        fs::remove_file(dir.path().join("file.txt")).unwrap();

        let conflicts = snapshot.detect_conflicts(dir.path(), |_| false).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "file.txt");
        assert!(conflicts[0].description.contains("deleted"));
    }

    #[test]
    fn detect_conflicts_finds_new_files() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "existing.txt", "content");

        let snapshot = SourceSnapshot::capture(dir.path(), |_| false).unwrap();

        // Create a new file in source.
        create_test_file(dir.path(), "new.txt", "new content");

        let conflicts = snapshot.detect_conflicts(dir.path(), |_| false).unwrap();
        // Should find the new file as a divergence (not a strict conflict, but noteworthy).
        let new_file_conflict = conflicts.iter().find(|c| c.path == "new.txt");
        assert!(new_file_conflict.is_some());
        assert!(new_file_conflict.unwrap().description.contains("created"));
    }

    #[test]
    fn no_conflicts_when_unchanged() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "file.txt", "content");

        let snapshot = SourceSnapshot::capture(dir.path(), |_| false).unwrap();
        let conflicts = snapshot.detect_conflicts(dir.path(), |_| false).unwrap();

        assert!(conflicts.is_empty());
    }

    #[test]
    fn infra_dirs_excluded_from_snapshot() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "src/main.rs", "code");
        create_test_file(dir.path(), ".ta/state.json", "internal");
        create_test_file(dir.path(), ".git/config", "gitconfig");
        create_test_file(dir.path(), ".hive-mind/session.json", "hive");

        let snapshot = SourceSnapshot::capture(dir.path(), |_| false).unwrap();
        let expected = 1;
        let actual = snapshot.file_count();
        assert_eq!(
            actual,
            expected,
            "Expected {} files but got {}. Files: {:?}",
            expected,
            actual,
            snapshot.files.keys().collect::<Vec<_>>()
        );
        assert!(snapshot.files.contains_key("src/main.rs"));
        assert!(!snapshot.files.contains_key(".ta/state.json"));
        assert!(!snapshot.files.contains_key(".git/config"));
        assert!(!snapshot.files.contains_key(".hive-mind/session.json"));
    }

    #[test]
    fn detect_conflicts_skips_excluded_paths() {
        let dir = TempDir::new().unwrap();
        create_test_file(dir.path(), "src/main.rs", "fn main() {}");

        // Snapshot with no target/ directory.
        let snapshot = SourceSnapshot::capture(dir.path(), |_| false).unwrap();
        assert_eq!(snapshot.file_count(), 1);

        // Simulate `cargo build` creating target/ after snapshot.
        create_test_file(dir.path(), "target/debug/binary", "fake binary");
        create_test_file(dir.path(), "target/release/deps/foo.d", "dep file");

        // Without skip predicate: target/ files appear as "new" conflicts.
        let conflicts = snapshot.detect_conflicts(dir.path(), |_| false).unwrap();
        assert!(
            conflicts.len() >= 2,
            "Expected target/ files as conflicts, got {}",
            conflicts.len()
        );

        // With skip predicate filtering target/: no false conflicts.
        let conflicts = snapshot
            .detect_conflicts(dir.path(), |path| path.starts_with("target"))
            .unwrap();
        assert!(
            conflicts.is_empty(),
            "Expected no conflicts but got: {:?}",
            conflicts.iter().map(|c| &c.path).collect::<Vec<_>>()
        );
    }
}
