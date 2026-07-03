// shared_files.rs — Identifies files commonly edited both by an in-flight
// goal's staging copy and directly on the source project (e.g. by the
// Advisor dashboard or a human) while the goal runs: PLAN.md, CLAUDE.md,
// Cargo.toml, docs/USAGE.md, and memory/*.md.
//
// `ta draft apply` diffs staging vs source at goal-start time, then copies
// changed files. If one of these files was ALSO changed on main between
// goal-start and apply, the main changes were previously lost — victims
// included memory/work_queue.md being deleted by a concurrent apply.
//
// `SharedFileBase` captures the real byte content of these files at
// `ta goal start` time (not just a hash) so it can serve as the merge base
// for an apply-time 3-way merge, independent of git history — this still
// works when a file was created/edited on main without a commit, which is
// exactly the scenario git-HEAD reconstruction cannot handle.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Exact relative paths always treated as shared files subject to
/// apply-time 3-way merge.
const SHARED_FILE_EXACT: &[&str] = &["PLAN.md", "CLAUDE.md", "Cargo.toml", "docs/USAGE.md"];

/// Returns true if `rel_path` is a shared file subject to apply-time 3-way merge.
pub fn is_shared_file(rel_path: &str) -> bool {
    SHARED_FILE_EXACT.contains(&rel_path)
        || (rel_path.starts_with("memory/") && rel_path.ends_with(".md"))
}

/// Base content of shared files captured at `ta goal start` time.
///
/// Stored as `.ta/staging/<goal-id>/apply-base.json`. Used as the merge base
/// for apply-time 3-way merges of shared files — real byte content, not a
/// hash, since a 3-way merge needs the actual base text.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SharedFileBase {
    /// When the snapshot was taken (seconds since UNIX epoch).
    pub created_at: u64,
    /// Relative path -> base64-encoded content at goal-start time.
    /// A path absent from this map means the file did not exist at goal start.
    pub files: HashMap<String, String>,
}

impl SharedFileBase {
    /// Capture the current content of every shared file that exists under `root`.
    pub fn capture(root: &Path) -> Self {
        let mut files = HashMap::new();
        for path in shared_file_candidates(root) {
            if let Ok(content) = fs::read(root.join(&path)) {
                files.insert(
                    path,
                    base64::engine::general_purpose::STANDARD.encode(content),
                );
            }
        }
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self { created_at, files }
    }

    /// Serialize to JSON and write to `path`, creating parent dirs as needed.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(path, json)
    }

    /// Load a previously-saved snapshot from `path`.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Base content bytes for `rel_path`, or `None` if the file did not exist
    /// at goal-start time (or isn't tracked in this snapshot).
    pub fn get(&self, rel_path: &str) -> Option<Vec<u8>> {
        self.files
            .get(rel_path)
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
    }
}

/// Enumerate candidate shared-file relative paths that actually exist under `root`.
fn shared_file_candidates(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for exact in SHARED_FILE_EXACT {
        if root.join(exact).is_file() {
            found.push((*exact).to_string());
        }
    }
    let memory_dir = root.join("memory");
    if let Ok(entries) = fs::read_dir(&memory_dir) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".md") {
                        found.push(format!("memory/{}", name));
                    }
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_file(dir: &Path, rel_path: &str, content: &str) {
        let abs_path = dir.join(rel_path);
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&abs_path, content).unwrap();
    }

    #[test]
    fn is_shared_file_matches_exact_names() {
        assert!(is_shared_file("PLAN.md"));
        assert!(is_shared_file("CLAUDE.md"));
        assert!(is_shared_file("Cargo.toml"));
        assert!(is_shared_file("docs/USAGE.md"));
    }

    #[test]
    fn is_shared_file_matches_memory_md_glob() {
        assert!(is_shared_file("memory/work_queue.md"));
        assert!(is_shared_file("memory/notes.md"));
    }

    #[test]
    fn is_shared_file_rejects_unrelated_paths() {
        assert!(!is_shared_file("src/main.rs"));
        assert!(!is_shared_file("memory/notes.txt"));
        assert!(!is_shared_file("docs/OTHER.md"));
        assert!(!is_shared_file("nested/PLAN.md"));
    }

    #[test]
    fn shared_file_base_captures_existing_files() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "PLAN.md", "# Plan");
        write_file(dir.path(), "memory/work_queue.md", "queued items");
        write_file(dir.path(), "src/main.rs", "fn main() {}");

        let base = SharedFileBase::capture(dir.path());

        assert_eq!(base.get("PLAN.md").unwrap(), b"# Plan");
        assert_eq!(base.get("memory/work_queue.md").unwrap(), b"queued items");
        assert!(base.get("src/main.rs").is_none());
    }

    #[test]
    fn shared_file_base_skips_missing_files() {
        let dir = TempDir::new().unwrap();
        // No PLAN.md, CLAUDE.md, etc. at all.
        let base = SharedFileBase::capture(dir.path());
        assert!(base.files.is_empty());
    }

    #[test]
    fn shared_file_base_round_trips_through_save_load() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "PLAN.md", "# Plan v1");
        write_file(dir.path(), "CLAUDE.md", "instructions");

        let base = SharedFileBase::capture(dir.path());
        let snapshot_path = dir.path().join("apply-base.json");
        base.save(&snapshot_path).unwrap();

        let loaded = SharedFileBase::load(&snapshot_path).unwrap();
        assert_eq!(loaded.get("PLAN.md").unwrap(), b"# Plan v1");
        assert_eq!(loaded.get("CLAUDE.md").unwrap(), b"instructions");
        assert_eq!(loaded.created_at, base.created_at);
    }

    #[test]
    fn shared_file_base_get_returns_none_for_untracked_path() {
        let base = SharedFileBase::default();
        assert!(base.get("PLAN.md").is_none());
    }
}
