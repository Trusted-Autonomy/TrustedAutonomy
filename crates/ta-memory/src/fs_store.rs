// fs_store.rs — Filesystem-backed memory store.
//
// Stores each memory entry as a JSON file in `.ta/memory/`.
// File name is derived from the key (URL-encoded, truncated).
// Sufficient for small-to-medium projects. For large-scale semantic
// search, the ruvector backend can be enabled via cargo feature.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::debug;
use uuid::Uuid;

use crate::error::MemoryError;
use crate::store::{MemoryEntry, MemoryQuery, MemoryStore, StoreParams};

/// Filesystem-backed memory store.
pub struct FsMemoryStore {
    base_dir: PathBuf,
}

impl FsMemoryStore {
    /// Create a new store at the given directory.
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    /// Convert a key to a safe filename.
    fn key_to_filename(key: &str) -> String {
        let slug: String = key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let truncated = if slug.len() > 200 {
            &slug[..200]
        } else {
            &slug
        };
        format!("{}.json", truncated)
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(Self::key_to_filename(key))
    }

    /// Read all entries from disk.
    fn read_all(&self) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !self.base_dir.exists() {
            return Ok(vec![]);
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let content = fs::read_to_string(&path)?;
                match serde_json::from_str::<MemoryEntry>(&content) {
                    Ok(mem) => entries.push(mem),
                    Err(e) => {
                        debug!(?path, %e, "skipping malformed memory file");
                    }
                }
            }
        }

        // Sort by creation time (newest first).
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(entries)
    }
}

impl MemoryStore for FsMemoryStore {
    fn store(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
    ) -> Result<MemoryEntry, MemoryError> {
        self.store_with_params(key, value, tags, source, StoreParams::default())
    }

    fn store_with_params(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
        params: StoreParams,
    ) -> Result<MemoryEntry, MemoryError> {
        fs::create_dir_all(&self.base_dir)?;

        let now = Utc::now();
        let path = self.entry_path(key);

        // Check for existing entry to preserve entry_id and created_at.
        let (entry_id, created_at) = if path.exists() {
            let content = fs::read_to_string(&path)?;
            match serde_json::from_str::<MemoryEntry>(&content) {
                Ok(existing) => (existing.entry_id, existing.created_at),
                Err(_) => (Uuid::new_v4(), now),
            }
        } else {
            (Uuid::new_v4(), now)
        };

        let entry = MemoryEntry {
            entry_id,
            key: key.to_string(),
            value,
            tags,
            source: source.to_string(),
            goal_id: params.goal_id,
            category: params.category,
            expires_at: params.expires_at,
            confidence: params.confidence.unwrap_or(0.5),
            phase_id: params.phase_id,
            created_at,
            updated_at: now,
        };

        let content = serde_json::to_string_pretty(&entry)?;
        fs::write(&path, content)?;
        debug!(key, "memory entry stored");
        Ok(entry)
    }

    fn recall(&self, key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        let path = self.entry_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)?;
        let entry: MemoryEntry = serde_json::from_str(&content)?;
        Ok(Some(entry))
    }

    fn lookup(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError> {
        let all = self.read_all()?;
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|e| {
                if let Some(ref prefix) = query.key_prefix {
                    if !e.key.starts_with(prefix) {
                        return false;
                    }
                }
                if !query.tags.is_empty() && !query.tags.iter().all(|t| e.tags.contains(t)) {
                    return false;
                }
                if let Some(goal_id) = query.goal_id {
                    if e.goal_id != Some(goal_id) {
                        return false;
                    }
                }
                if let Some(ref cat) = query.category {
                    if e.category.as_ref() != Some(cat) {
                        return false;
                    }
                }
                // Phase filter: match entries for this phase OR global entries (v0.6.3).
                if let Some(ref phase) = query.phase_id {
                    match &e.phase_id {
                        Some(entry_phase) if entry_phase != phase => return false,
                        _ => {} // None (global) or matching phase — include
                    }
                }
                true
            })
            .collect();

        match query.limit {
            Some(n) => Ok(filtered.into_iter().take(n).collect()),
            None => Ok(filtered),
        }
    }

    fn list(&self, limit: Option<usize>) -> Result<Vec<MemoryEntry>, MemoryError> {
        let all = self.read_all()?;
        match limit {
            Some(n) => Ok(all.into_iter().take(n).collect()),
            None => Ok(all),
        }
    }

    fn forget(&mut self, key: &str) -> Result<bool, MemoryError> {
        let path = self.entry_path(key);
        if path.exists() {
            fs::remove_file(&path)?;
            debug!(key, "memory entry forgotten");
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store(dir: &TempDir) -> FsMemoryStore {
        FsMemoryStore::new(dir.path().join("memory"))
    }

    #[test]
    fn store_and_recall() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        let entry = store
            .store("test-key", serde_json::json!("hello"), vec![], "test")
            .unwrap();
        assert_eq!(entry.key, "test-key");

        let recalled = store.recall("test-key").unwrap().unwrap();
        assert_eq!(recalled.value, serde_json::json!("hello"));
    }

    #[test]
    fn recall_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);
        assert!(store.recall("nope").unwrap().is_none());
    }

    #[test]
    fn list_entries() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        store
            .store("a", serde_json::json!(1), vec![], "test")
            .unwrap();
        store
            .store("b", serde_json::json!(2), vec![], "test")
            .unwrap();

        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn list_with_limit() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        store
            .store("a", serde_json::json!(1), vec![], "test")
            .unwrap();
        store
            .store("b", serde_json::json!(2), vec![], "test")
            .unwrap();
        store
            .store("c", serde_json::json!(3), vec![], "test")
            .unwrap();

        let limited = store.list(Some(2)).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn forget_entry() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        store
            .store("to-forget", serde_json::json!("bye"), vec![], "test")
            .unwrap();
        assert!(store.forget("to-forget").unwrap());
        assert!(store.recall("to-forget").unwrap().is_none());
    }

    #[test]
    fn forget_nonexistent_returns_false() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);
        assert!(!store.forget("nope").unwrap());
    }

    #[test]
    fn lookup_by_tag() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        store
            .store(
                "tagged",
                serde_json::json!("yes"),
                vec!["important".into()],
                "test",
            )
            .unwrap();
        store
            .store("untagged", serde_json::json!("no"), vec![], "test")
            .unwrap();

        let results = store
            .lookup(MemoryQuery {
                tags: vec!["important".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "tagged");
    }

    #[test]
    fn lookup_by_prefix() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        store
            .store("project/foo", serde_json::json!(1), vec![], "test")
            .unwrap();
        store
            .store("project/bar", serde_json::json!(2), vec![], "test")
            .unwrap();
        store
            .store("other", serde_json::json!(3), vec![], "test")
            .unwrap();

        let results = store
            .lookup(MemoryQuery {
                key_prefix: Some("project/".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn overwrite_preserves_entry_id() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        let first = store
            .store("key", serde_json::json!("v1"), vec![], "test")
            .unwrap();
        let second = store
            .store("key", serde_json::json!("v2"), vec![], "test")
            .unwrap();

        assert_eq!(first.entry_id, second.entry_id);
        assert_eq!(second.value, serde_json::json!("v2"));
    }
}
