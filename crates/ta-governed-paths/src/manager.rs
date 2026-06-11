//! `GovernedPathManager` — high-level orchestration of SHA store + URI journal.
//!
//! This is the primary API surface callers use; it wires together the store
//! and journal and provides the four key operations:
//!
//! - `snapshot_path` — record the pre-goal baseline SHA for a governed path
//! - `capture_write` — store a write's content and journal a `write` entry
//! - `apply_path` — write a journaled SHA blob back to the real filesystem
//! - `deny_path` — append a `DENIED` journal entry for a write
//! - `rollback_path` — restore the pre-goal snapshot SHA to the real path
//! - `run_gc` — remove unreferenced blobs and return bytes freed

use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::config::GovernedPathConfig;
use crate::error::GovernedPathError;
use crate::journal::{path_to_uri, JournalAction, JournalEntry, UriJournal};
use crate::sha_store::ShaStore;

/// Orchestrates managed-path operations for a single workspace.
pub struct GovernedPathManager {
    store: ShaStore,
    journal: UriJournal,
    workspace_root: PathBuf,
}

impl GovernedPathManager {
    /// Open (or create) both the SHA store and journal for `workspace_root`.
    pub fn open(workspace_root: &Path) -> Result<Self, GovernedPathError> {
        let store = ShaStore::open(workspace_root)?;
        let journal = UriJournal::open(workspace_root)?;
        Ok(Self {
            store,
            journal,
            workspace_root: workspace_root.to_path_buf(),
        })
    }

    // ── snapshot ─────────────────────────────────────────────────────────────

    /// Record the current content of `abs_path` as a pre-goal snapshot.
    ///
    /// If the file does not exist yet, the empty string is stored as the SHA so
    /// rollback can recreate an empty file (or the caller can check before applying).
    ///
    /// Returns the SHA-256 of the snapshotted content.
    pub fn snapshot_path(
        &self,
        abs_path: &Path,
        goal_id: Option<Uuid>,
    ) -> Result<String, GovernedPathError> {
        let sha = if abs_path.exists() {
            self.store.write_file(abs_path)?
        } else {
            // File doesn't exist — snapshot the empty blob.
            self.store.write_bytes(b"")?
        };
        let uri = self.rel_uri(abs_path);
        let entry = JournalEntry::new(&uri, &sha, JournalAction::Snapshot, goal_id);
        self.journal.append(&entry)?;
        Ok(sha)
    }

    // ── write capture ─────────────────────────────────────────────────────────

    /// Capture a write to `abs_path`: store the new content and journal a `write` entry.
    ///
    /// Enforces `max_sha_store_mb` if the config is provided.
    ///
    /// Returns the SHA-256 of the written content.
    pub fn capture_write(
        &self,
        abs_path: &Path,
        config: Option<&GovernedPathConfig>,
        goal_id: Option<Uuid>,
    ) -> Result<String, GovernedPathError> {
        // Check read-only enforcement.
        if let Some(cfg) = config {
            if cfg.mode == crate::config::PathMode::ReadOnly {
                return Err(GovernedPathError::ReadOnly {
                    path: abs_path.display().to_string(),
                });
            }
            // Check size cap.
            if let Some(max_mb) = cfg.max_sha_store_mb {
                let current_bytes = self.store.total_bytes();
                let max_bytes = max_mb * 1024 * 1024;
                if current_bytes >= max_bytes {
                    return Err(GovernedPathError::StoreFull {
                        path: abs_path.display().to_string(),
                        max_mb,
                    });
                }
            }
        }

        let sha = self.store.write_file(abs_path)?;
        let uri = self.rel_uri(abs_path);
        let entry = JournalEntry::new(&uri, &sha, JournalAction::Write, goal_id);
        self.journal.append(&entry)?;
        Ok(sha)
    }

    // ── apply ─────────────────────────────────────────────────────────────────

    /// Write the blob identified by `sha` back to `abs_path`.
    ///
    /// Called from `ta draft apply` for each journaled write. Appends an
    /// `Applied` entry to the journal.
    pub fn apply_path(
        &self,
        abs_path: &Path,
        sha: &str,
        goal_id: Option<Uuid>,
        draft_id: Option<Uuid>,
    ) -> Result<(), GovernedPathError> {
        self.store.restore_to(sha, abs_path)?;
        let uri = self.rel_uri(abs_path);
        let mut entry = JournalEntry::new(&uri, sha, JournalAction::Applied, goal_id);
        if let Some(did) = draft_id {
            entry = entry.with_draft_id(did);
        }
        self.journal.append(&entry)?;
        Ok(())
    }

    // ── deny ──────────────────────────────────────────────────────────────────

    /// Record a `Denied` journal entry for a given URI+SHA pair.
    ///
    /// The blob stays in the store — GC will eventually clean it once the
    /// retention window expires. No filesystem changes are made.
    pub fn deny_write(
        &self,
        uri: &str,
        sha: &str,
        goal_id: Option<Uuid>,
        draft_id: Option<Uuid>,
    ) -> Result<(), GovernedPathError> {
        let mut entry = JournalEntry::new(uri, sha, JournalAction::Denied, goal_id);
        if let Some(did) = draft_id {
            entry = entry.with_draft_id(did);
        }
        self.journal.append(&entry)?;
        Ok(())
    }

    // ── rollback ──────────────────────────────────────────────────────────────

    /// Restore the most recent `snapshot` for `abs_path` to the real filesystem.
    ///
    /// If no snapshot exists, returns `Ok(false)` (nothing to restore).
    pub fn rollback_path(
        &self,
        abs_path: &Path,
        goal_id: Option<Uuid>,
    ) -> Result<bool, GovernedPathError> {
        let uri = self.rel_uri(abs_path);
        let Some(snap) = self.journal.last_snapshot(&uri)? else {
            return Ok(false);
        };
        if snap.sha256.is_empty() {
            // Snapshot was of a non-existent file — delete the current file.
            if abs_path.exists() {
                std::fs::remove_file(abs_path)
                    .map_err(|e| GovernedPathError::io(abs_path.display().to_string(), e))?;
            }
        } else {
            self.store.restore_to(&snap.sha256, abs_path)?;
        }
        let entry = JournalEntry::new(&uri, &snap.sha256, JournalAction::RolledBack, goal_id);
        self.journal.append(&entry)?;
        Ok(true)
    }

    // ── GC ───────────────────────────────────────────────────────────────────

    /// Remove SHA blobs not referenced by any live journal entry.
    ///
    /// "Live" means the entry's timestamp is within `retain_days` of today, OR
    /// the entry is not a terminal action (`Denied` / `RolledBack`).
    ///
    /// Returns the total number of bytes freed.
    pub fn gc(&self, retain_days: u32, dry_run: bool) -> Result<GcStats, GovernedPathError> {
        let live = self.journal.live_shas(retain_days)?;
        let all = self.store.list_shas();
        let mut freed_bytes = 0u64;
        let mut removed = 0u32;
        let mut kept = 0u32;

        for sha in &all {
            if live.contains(sha) {
                kept += 1;
                continue;
            }
            // Blob is not referenced by any live entry — safe to remove.
            let blob_path = self.workspace_root.join(".ta").join("sha-fs").join(sha);
            let size = std::fs::metadata(&blob_path).map(|m| m.len()).unwrap_or(0);
            if !dry_run {
                self.store.remove_blob(sha)?;
            }
            freed_bytes += size;
            removed += 1;
        }

        Ok(GcStats {
            blobs_removed: removed,
            blobs_kept: kept,
            bytes_freed: freed_bytes,
            dry_run,
        })
    }

    // ── accessors ─────────────────────────────────────────────────────────────

    /// Borrow the underlying SHA store.
    pub fn store(&self) -> &ShaStore {
        &self.store
    }

    /// Borrow the underlying URI journal.
    pub fn journal(&self) -> &UriJournal {
        &self.journal
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Build the canonical URI for an absolute path under the workspace root.
    ///
    /// Paths outside the workspace root are represented as-is.
    fn rel_uri(&self, abs_path: &Path) -> String {
        let rel = abs_path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(abs_path);
        path_to_uri(rel)
    }
}

/// Statistics from a GC run.
#[derive(Debug)]
pub struct GcStats {
    pub blobs_removed: u32,
    pub blobs_kept: u32,
    pub bytes_freed: u64,
    pub dry_run: bool,
}

impl GcStats {
    pub fn bytes_freed_display(&self) -> String {
        format_bytes(self.bytes_freed)
    }
}

fn format_bytes(b: u64) -> String {
    if b >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024 * 1024 {
        format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
    } else if b >= 1024 {
        format!("{:.1} KiB", b as f64 / 1024.0)
    } else {
        format!("{} B", b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn write_file(path: &Path, content: &[u8]) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content).unwrap();
    }

    #[test]
    fn snapshot_and_capture_write_round_trip() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();

        let file = dir.path().join("data").join("out.txt");
        write_file(&file, b"original");

        // Snapshot before goal runs.
        let snap_sha = mgr.snapshot_path(&file, None).unwrap();

        // Agent writes new content.
        write_file(&file, b"modified");
        let write_sha = mgr.capture_write(&file, None, None).unwrap();
        assert_ne!(snap_sha, write_sha);

        // Journal should have two entries.
        let entries = mgr.journal().entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].action, JournalAction::Snapshot));
        assert!(matches!(entries[1].action, JournalAction::Write));
    }

    #[test]
    fn apply_writes_blob_to_real_path() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();

        // Store a blob manually.
        let sha = mgr.store().write_bytes(b"applied content").unwrap();
        let dest = dir.path().join("applied.txt");
        mgr.apply_path(&dest, &sha, None, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"applied content");

        // Applied entry in journal.
        let entries = mgr.journal().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].action, JournalAction::Applied));
    }

    #[test]
    fn deny_records_entry_no_file_change() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();

        let file = dir.path().join("file.txt");
        write_file(&file, b"content");

        mgr.deny_write("fs://workspace/file.txt", "aabbcc", None, None)
            .unwrap();

        let entries = mgr.journal().entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].action, JournalAction::Denied));
        // File unchanged.
        assert_eq!(std::fs::read(&file).unwrap(), b"content");
    }

    #[test]
    fn rollback_restores_snapshot() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();

        let file = dir.path().join("r.txt");
        write_file(&file, b"original");
        mgr.snapshot_path(&file, None).unwrap();

        // Overwrite the file.
        write_file(&file, b"overwritten");

        // Rollback should restore original.
        let restored = mgr.rollback_path(&file, None).unwrap();
        assert!(restored);
        assert_eq!(std::fs::read(&file).unwrap(), b"original");
    }

    #[test]
    fn gc_removes_unreferenced_blobs() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();

        // Write a blob with no journal reference.
        mgr.store().write_bytes(b"orphan").unwrap();
        assert_eq!(mgr.store().list_shas().len(), 1);

        let stats = mgr.gc(30, false).unwrap();
        assert_eq!(stats.blobs_removed, 1);
        assert_eq!(mgr.store().list_shas().len(), 0);
    }

    #[test]
    fn gc_keeps_live_blobs() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();

        let file = dir.path().join("keep.txt");
        write_file(&file, b"keep me");
        mgr.snapshot_path(&file, None).unwrap();

        // GC should keep the blob since the snapshot is recent.
        let stats = mgr.gc(30, false).unwrap();
        assert_eq!(stats.blobs_removed, 0);
        assert_eq!(stats.blobs_kept, 1);
    }

    #[test]
    fn gc_dry_run_does_not_delete() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();
        mgr.store().write_bytes(b"orphan2").unwrap();

        let stats = mgr.gc(30, true).unwrap();
        assert_eq!(stats.blobs_removed, 1);
        assert!(stats.dry_run);
        // Blob still there.
        assert_eq!(mgr.store().list_shas().len(), 1);
    }

    #[test]
    fn read_only_capture_returns_error() {
        use crate::config::{GovernedPathConfig, PathMode};
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();

        let file = dir.path().join("ro.txt");
        write_file(&file, b"ro");

        let cfg = GovernedPathConfig {
            path: std::path::PathBuf::from("ro.txt"),
            mode: PathMode::ReadOnly,
            purpose: "test".to_string(),
            max_sha_store_mb: None,
        };

        let err = mgr.capture_write(&file, Some(&cfg), None).unwrap_err();
        assert!(matches!(
            err,
            crate::error::GovernedPathError::ReadOnly { .. }
        ));
    }

    #[test]
    fn snapshot_nonexistent_file_uses_empty_sha() {
        let dir = tempdir().unwrap();
        let mgr = GovernedPathManager::open(dir.path()).unwrap();
        let file = dir.path().join("does_not_exist.txt");
        let sha = mgr.snapshot_path(&file, None).unwrap();
        // Should not error, and the SHA should be for empty bytes.
        let empty_sha = crate::sha_store::ShaStore::sha256_hex(b"");
        assert_eq!(sha, empty_sha);
    }
}
