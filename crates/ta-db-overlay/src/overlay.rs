use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::entry::{BlobRef, OverlayEntry, OverlayEntryKind};
use crate::error::Result;

/// Read-your-writes overlay for database mutations within a draft.
///
/// Persisted to `.ta/staging/<goal_id>/db-overlay.jsonl`.
/// Each line is a JSON-serialized `OverlayEntry`.
/// Multiple writes to the same URI accumulate: `before` stays fixed (original),
/// `after` is updated to the latest value.
pub struct DraftOverlay {
    /// Path to the JSONL file.
    overlay_path: PathBuf,
    /// Path to the blobs subdirectory.
    blobs_dir: PathBuf,
    /// In-memory cache: URI → latest entry.
    cache: HashMap<String, OverlayEntry>,
    /// True after the JSONL has been loaded into cache.
    loaded: bool,
}

impl DraftOverlay {
    /// Create (or open) an overlay rooted at `staging_dir`.
    pub fn new(staging_dir: &Path) -> Result<Self> {
        let overlay_path = staging_dir.join("db-overlay.jsonl");
        let blobs_dir = staging_dir.join("db-blobs");
        fs::create_dir_all(staging_dir)?;
        fs::create_dir_all(&blobs_dir)?;
        Ok(Self {
            overlay_path,
            blobs_dir,
            cache: HashMap::new(),
            loaded: false,
        })
    }

    /// Load existing entries from JSONL into the in-memory cache.
    fn ensure_loaded(&mut self) -> Result<()> {
        if self.loaded {
            return Ok(());
        }
        self.loaded = true;
        if !self.overlay_path.exists() {
            return Ok(());
        }
        let file = File::open(&self.overlay_path)?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<OverlayEntry>(line) {
                Ok(entry) => {
                    self.cache.insert(entry.uri.clone(), entry);
                }
                Err(e) => {
                    tracing::warn!("db-overlay: skipping malformed entry: {} — {}", line, e);
                }
            }
        }
        Ok(())
    }

    /// Rewrite the entire JSONL file with the current cache contents.
    fn flush_entry(&self, entry: &OverlayEntry) -> Result<()> {
        // Rewrite entire JSONL with the updated cache.
        // For small overlays this is fine; production could use WAL-style appends.
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.overlay_path)?;
        for e in self.cache.values() {
            let line = serde_json::to_string(e)?;
            writeln!(file, "{}", line)?;
        }
        let _ = entry; // already in cache, flushed above
        Ok(())
    }

    /// Stage a write: `uri` is the resource URI, `after` is the new value.
    /// If `before` is `None`, the original value must be supplied by the caller
    /// (fetched from the real DB before the first write).
    pub fn put(
        &mut self,
        uri: &str,
        before: Option<serde_json::Value>,
        after: serde_json::Value,
    ) -> Result<()> {
        self.ensure_loaded()?;
        let existing = self.cache.get(uri);
        let entry = if let Some(e) = existing {
            // Accumulate: keep the original `before`, update `after`.
            OverlayEntry {
                uri: uri.to_string(),
                before: e.before.clone(),
                after,
                ts: Utc::now(),
                kind: OverlayEntryKind::Update,
            }
        } else {
            let kind = if before.is_none() {
                OverlayEntryKind::Insert
            } else {
                OverlayEntryKind::Update
            };
            OverlayEntry {
                uri: uri.to_string(),
                before,
                after,
                ts: Utc::now(),
                kind,
            }
        };
        self.cache.insert(uri.to_string(), entry.clone());
        debug!(uri = uri, "db-overlay: staged write");
        self.flush_entry(&entry)
    }

    /// Stage a delete: `before` is the record being deleted.
    pub fn delete(&mut self, uri: &str, before: Option<serde_json::Value>) -> Result<()> {
        self.ensure_loaded()?;
        let prior_before = self.cache.get(uri).and_then(|e| e.before.clone());
        let entry = OverlayEntry {
            uri: uri.to_string(),
            before: before.or(prior_before),
            after: serde_json::Value::Null,
            ts: Utc::now(),
            kind: OverlayEntryKind::Delete,
        };
        self.cache.insert(uri.to_string(), entry.clone());
        debug!(uri = uri, "db-overlay: staged delete");
        self.flush_entry(&entry)
    }

    /// Stage a DDL change (schema modification).
    pub fn put_ddl(&mut self, uri: &str, ddl_json: serde_json::Value) -> Result<()> {
        self.ensure_loaded()?;
        let entry = OverlayEntry {
            uri: uri.to_string(),
            before: None,
            after: ddl_json,
            ts: Utc::now(),
            kind: OverlayEntryKind::Ddl,
        };
        self.cache.insert(uri.to_string(), entry.clone());
        self.flush_entry(&entry)
    }

    /// Stage a binary blob field. The bytes are stored by SHA-256 in `db-blobs/`.
    pub fn put_blob(&mut self, uri: &str, field: &str, bytes: &[u8]) -> Result<BlobRef> {
        self.ensure_loaded()?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let sha256 = format!("{:x}", hasher.finalize());
        let blob_path = self.blobs_dir.join(&sha256);
        if !blob_path.exists() {
            fs::write(&blob_path, bytes)?;
        }
        let blob_ref = BlobRef {
            sha256: sha256.clone(),
            size_bytes: bytes.len() as u64,
            field: field.to_string(),
        };
        let after = serde_json::to_value(&blob_ref)?;
        let entry = OverlayEntry {
            uri: uri.to_string(),
            before: None,
            after,
            ts: Utc::now(),
            kind: OverlayEntryKind::Blob,
        };
        self.cache.insert(uri.to_string(), entry.clone());
        self.flush_entry(&entry)?;
        Ok(blob_ref)
    }

    /// Read a staged value (returns `None` if not in overlay — caller hits real DB).
    pub fn get(&mut self, uri: &str) -> Result<Option<&OverlayEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.get(uri))
    }

    /// List all staged mutations.
    pub fn list_mutations(&mut self) -> Result<Vec<&OverlayEntry>> {
        self.ensure_loaded()?;
        let mut entries: Vec<&OverlayEntry> = self.cache.values().collect();
        entries.sort_by_key(|e| e.ts);
        Ok(entries)
    }

    /// Returns true if there are no staged mutations.
    pub fn is_empty(&mut self) -> Result<bool> {
        self.ensure_loaded()?;
        Ok(self.cache.is_empty())
    }

    /// Count of staged mutations by kind.
    pub fn mutation_count(&mut self) -> Result<MutationCount> {
        self.ensure_loaded()?;
        let mut count = MutationCount::default();
        for e in self.cache.values() {
            match e.kind {
                OverlayEntryKind::Update => count.updates += 1,
                OverlayEntryKind::Insert => count.inserts += 1,
                OverlayEntryKind::Delete => count.deletes += 1,
                OverlayEntryKind::Ddl => count.ddl += 1,
                OverlayEntryKind::Blob => count.blobs += 1,
            }
        }
        Ok(count)
    }
}

/// Summary count of mutations by kind.
#[derive(Debug, Default)]
pub struct MutationCount {
    pub updates: usize,
    pub inserts: usize,
    pub deletes: usize,
    pub ddl: usize,
    pub blobs: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_overlay() -> (DraftOverlay, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let overlay = DraftOverlay::new(dir.path()).unwrap();
        (overlay, dir)
    }

    #[test]
    fn put_and_get_returns_staged_value() {
        let (mut overlay, _dir) = make_overlay();
        let before = serde_json::json!({"active_issues": 4});
        let after = serde_json::json!({"active_issues": 7});
        overlay
            .put("sqlite://db/issues/1", Some(before.clone()), after.clone())
            .unwrap();
        let entry = overlay.get("sqlite://db/issues/1").unwrap().unwrap();
        assert_eq!(entry.after, after);
        assert_eq!(entry.before, Some(before));
        assert_eq!(entry.kind, OverlayEntryKind::Update);
    }

    #[test]
    fn put_accumulates_before_stays_fixed() {
        let (mut overlay, _dir) = make_overlay();
        let before = serde_json::json!({"v": 1});
        let after1 = serde_json::json!({"v": 2});
        let after2 = serde_json::json!({"v": 3});
        overlay
            .put("sqlite://db/t/1", Some(before.clone()), after1)
            .unwrap();
        overlay
            .put("sqlite://db/t/1", None, after2.clone())
            .unwrap();
        let entry = overlay.get("sqlite://db/t/1").unwrap().unwrap();
        assert_eq!(entry.after, after2, "after should be latest value");
        assert_eq!(
            entry.before,
            Some(before),
            "before should be original value"
        );
    }

    #[test]
    fn insert_has_null_before() {
        let (mut overlay, _dir) = make_overlay();
        overlay
            .put("sqlite://db/t/99", None, serde_json::json!({"name": "new"}))
            .unwrap();
        let entry = overlay.get("sqlite://db/t/99").unwrap().unwrap();
        assert_eq!(entry.kind, OverlayEntryKind::Insert);
        assert!(entry.before.is_none());
    }

    #[test]
    fn delete_staged_correctly() {
        let (mut overlay, _dir) = make_overlay();
        let before = serde_json::json!({"name": "old"});
        overlay
            .delete("sqlite://db/t/5", Some(before.clone()))
            .unwrap();
        let entry = overlay.get("sqlite://db/t/5").unwrap().unwrap();
        assert_eq!(entry.kind, OverlayEntryKind::Delete);
        assert_eq!(entry.after, serde_json::Value::Null);
        assert_eq!(entry.before, Some(before));
    }

    #[test]
    fn put_blob_stores_by_sha256() {
        let (mut overlay, _dir) = make_overlay();
        let data = b"hello world binary data";
        let blob_ref = overlay
            .put_blob("sqlite://db/files/1", "content", data)
            .unwrap();
        assert_eq!(blob_ref.size_bytes, data.len() as u64);
        assert!(!blob_ref.sha256.is_empty());
    }

    #[test]
    fn list_mutations_returns_all() {
        let (mut overlay, _dir) = make_overlay();
        overlay
            .put("sqlite://db/t/1", None, serde_json::json!({"v": 1}))
            .unwrap();
        overlay
            .put("sqlite://db/t/2", None, serde_json::json!({"v": 2}))
            .unwrap();
        let mutations = overlay.list_mutations().unwrap();
        assert_eq!(mutations.len(), 2);
    }

    #[test]
    fn overlay_persists_to_jsonl() {
        let dir = tempdir().unwrap();
        {
            let mut overlay = DraftOverlay::new(dir.path()).unwrap();
            overlay
                .put(
                    "sqlite://db/t/1",
                    Some(serde_json::json!({"v": 0})),
                    serde_json::json!({"v": 1}),
                )
                .unwrap();
        }
        // Reopen and verify persistence.
        let mut overlay2 = DraftOverlay::new(dir.path()).unwrap();
        let entry = overlay2.get("sqlite://db/t/1").unwrap().unwrap();
        assert_eq!(entry.after, serde_json::json!({"v": 1}));
    }

    #[test]
    fn mutation_count_correct() {
        let (mut overlay, _dir) = make_overlay();
        overlay
            .put("sqlite://db/t/1", None, serde_json::json!({}))
            .unwrap();
        overlay
            .put(
                "sqlite://db/t/2",
                Some(serde_json::json!({"x": 1})),
                serde_json::json!({"x": 2}),
            )
            .unwrap();
        overlay.delete("sqlite://db/t/3", None).unwrap();
        let count = overlay.mutation_count().unwrap();
        assert_eq!(count.inserts, 1);
        assert_eq!(count.updates, 1);
        assert_eq!(count.deletes, 1);
    }
}
