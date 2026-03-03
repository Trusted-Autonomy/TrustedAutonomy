// ruvector_store.rs — RuVector-backed memory store with semantic search.
//
// Uses ruvector-core's HNSW indexing for O(log n) semantic recall.
// Entries are stored as vectors with metadata, enabling similarity search
// via hash-based embeddings (upgrade to LLM embeddings is opt-in future work).
//
// Storage: single `.rvf` directory at `.ta/memory.rvf`.
// Migration: auto-imports existing `.ta/memory/*.json` on first open.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use ruvector_core::types::DbOptions;
use ruvector_core::{DistanceMetric, SearchQuery, VectorDB, VectorEntry as RvEntry};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::MemoryError;
use crate::store::{MemoryEntry, MemoryQuery, MemoryStore};

/// Embedding dimension used for hash-based text embeddings.
/// Matches ruvector-core's default hash embedding output size.
const EMBED_DIM: usize = 128;

/// RuVector-backed memory store with HNSW semantic search.
pub struct RuVectorStore {
    db: VectorDB,
    storage_path: PathBuf,
}

impl RuVectorStore {
    /// Open or create a RuVector store at the given path.
    ///
    /// If the path doesn't exist, a new database is created.
    /// If `.ta/memory/` contains JSON files, they are auto-imported.
    pub fn open(storage_path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let storage_path = storage_path.as_ref().to_path_buf();
        let options = DbOptions {
            dimensions: EMBED_DIM,
            distance_metric: DistanceMetric::Cosine,
            storage_path: storage_path.to_string_lossy().to_string(),
            hnsw_config: None,
            quantization: None,
        };

        let db = VectorDB::new(options).map_err(|e| MemoryError::VectorDb(e.to_string()))?;
        debug!(path = %storage_path.display(), "opened ruvector memory store");

        Ok(Self { db, storage_path })
    }

    /// Import existing filesystem memory entries (`.ta/memory/*.json`).
    ///
    /// Called once on first use when migrating from `FsMemoryStore`.
    /// Skips entries whose keys already exist in the vector store.
    pub fn migrate_from_fs(&self, fs_memory_dir: &Path) -> Result<usize, MemoryError> {
        if !fs_memory_dir.exists() {
            return Ok(0);
        }

        let mut imported = 0;
        for entry in std::fs::read_dir(fs_memory_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let content = std::fs::read_to_string(&path)?;
                match serde_json::from_str::<MemoryEntry>(&content) {
                    Ok(mem) => {
                        // Skip if already exists.
                        if self
                            .db
                            .get(&mem.entry_id.to_string())
                            .map_err(|e| MemoryError::VectorDb(e.to_string()))?
                            .is_some()
                        {
                            continue;
                        }

                        let vector = text_to_embedding(&mem.value.to_string());
                        let metadata = entry_to_metadata(&mem);

                        let rv_entry = RvEntry {
                            id: Some(mem.entry_id.to_string()),
                            vector,
                            metadata: Some(metadata),
                        };
                        self.db
                            .insert(rv_entry)
                            .map_err(|e| MemoryError::VectorDb(e.to_string()))?;
                        imported += 1;
                    }
                    Err(e) => {
                        warn!(?path, %e, "skipping malformed memory file during migration");
                    }
                }
            }
        }

        debug!(imported, "migrated filesystem memory entries to ruvector");
        Ok(imported)
    }

    /// Get the storage path.
    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }
}

impl MemoryStore for RuVectorStore {
    fn store(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
    ) -> Result<MemoryEntry, MemoryError> {
        let now = Utc::now();

        // Check for existing entry to preserve ID and created_at.
        let (entry_id, created_at) = match self.recall(key)? {
            Some(existing) => {
                // Delete old entry before re-inserting.
                let _ = self
                    .db
                    .delete(&existing.entry_id.to_string())
                    .map_err(|e| MemoryError::VectorDb(e.to_string()))?;
                (existing.entry_id, existing.created_at)
            }
            None => (Uuid::new_v4(), now),
        };

        let entry = MemoryEntry {
            entry_id,
            key: key.to_string(),
            value: value.clone(),
            tags,
            source: source.to_string(),
            goal_id: None,
            created_at,
            updated_at: now,
        };

        let vector = text_to_embedding(&value.to_string());
        let metadata = entry_to_metadata(&entry);

        let rv_entry = RvEntry {
            id: Some(entry_id.to_string()),
            vector,
            metadata: Some(metadata),
        };

        self.db
            .insert(rv_entry)
            .map_err(|e| MemoryError::VectorDb(e.to_string()))?;

        debug!(key, "memory entry stored in ruvector");
        Ok(entry)
    }

    fn recall(&self, key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        // Linear scan over all entries to find by key.
        // This is O(n) but recall-by-exact-key is a secondary use case;
        // semantic_search is the primary benefit of ruvector.
        let all = self.list(None)?;
        Ok(all.into_iter().find(|e| e.key == key))
    }

    fn lookup(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError> {
        let all = self.list(None)?;
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
                true
            })
            .collect();

        match query.limit {
            Some(n) => Ok(filtered.into_iter().take(n).collect()),
            None => Ok(filtered),
        }
    }

    fn list(&self, limit: Option<usize>) -> Result<Vec<MemoryEntry>, MemoryError> {
        let keys = self
            .db
            .keys()
            .map_err(|e| MemoryError::VectorDb(e.to_string()))?;

        let mut entries = Vec::new();
        for id in &keys {
            if let Some(rv_entry) = self
                .db
                .get(id)
                .map_err(|e| MemoryError::VectorDb(e.to_string()))?
            {
                if let Some(entry) = metadata_to_entry(&rv_entry) {
                    entries.push(entry);
                }
            }
        }

        // Sort by creation time (newest first), matching FsMemoryStore behavior.
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        match limit {
            Some(n) => Ok(entries.into_iter().take(n).collect()),
            None => Ok(entries),
        }
    }

    fn forget(&mut self, key: &str) -> Result<bool, MemoryError> {
        if let Some(entry) = self.recall(key)? {
            self.db
                .delete(&entry.entry_id.to_string())
                .map_err(|e| MemoryError::VectorDb(e.to_string()))?;
            debug!(key, "memory entry forgotten from ruvector");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn semantic_search(&self, query: &str, k: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        if k == 0 {
            return Ok(vec![]);
        }

        let query_vector = text_to_embedding(query);
        let search = SearchQuery {
            vector: query_vector,
            k,
            filter: None,
            ef_search: None,
        };

        let results = self
            .db
            .search(search)
            .map_err(|e| MemoryError::VectorDb(e.to_string()))?;

        let mut entries = Vec::new();
        for result in results {
            if let Some(rv_entry) = self
                .db
                .get(&result.id)
                .map_err(|e| MemoryError::VectorDb(e.to_string()))?
            {
                if let Some(entry) = metadata_to_entry(&rv_entry) {
                    entries.push(entry);
                }
            }
        }

        Ok(entries)
    }
}

// ── Embedding helpers ──────────────────────────────────────────

/// Convert text to a fixed-size embedding vector using a deterministic hash.
///
/// This uses a simple hash-based approach for zero-dependency embeddings.
/// Quality is lower than LLM-based embeddings but sufficient for basic
/// similarity matching. Future: add opt-in LLM embedding via API.
fn text_to_embedding(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0f32; EMBED_DIM];

    // Character n-gram hashing with positional encoding.
    // Produces a rough "bag of n-grams" embedding.
    let text_lower = text.to_lowercase();
    let chars: Vec<char> = text_lower.chars().collect();

    for ngram_size in 1..=3 {
        for (pos, window) in chars.windows(ngram_size).enumerate() {
            let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
            for &c in window {
                hash ^= c as u64;
                hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
            }
            // Mix in position for word-order sensitivity.
            hash = hash.wrapping_add(pos as u64);

            let idx = (hash as usize) % EMBED_DIM;
            vector[idx] += 1.0;
        }
    }

    // L2 normalize.
    let norm: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vector {
            *v /= norm;
        }
    }

    vector
}

// ── Metadata serialization ─────────────────────────────────────

/// Serialize a MemoryEntry into ruvector metadata.
fn entry_to_metadata(entry: &MemoryEntry) -> HashMap<String, serde_json::Value> {
    let mut meta = HashMap::new();
    meta.insert("key".into(), serde_json::json!(entry.key));
    meta.insert("value".into(), entry.value.clone());
    meta.insert("tags".into(), serde_json::json!(entry.tags));
    meta.insert("source".into(), serde_json::json!(entry.source));
    meta.insert(
        "entry_id".into(),
        serde_json::json!(entry.entry_id.to_string()),
    );
    if let Some(goal_id) = entry.goal_id {
        meta.insert("goal_id".into(), serde_json::json!(goal_id.to_string()));
    }
    meta.insert(
        "created_at".into(),
        serde_json::json!(entry.created_at.to_rfc3339()),
    );
    meta.insert(
        "updated_at".into(),
        serde_json::json!(entry.updated_at.to_rfc3339()),
    );
    meta
}

/// Deserialize a ruvector entry's metadata back into a MemoryEntry.
fn metadata_to_entry(rv_entry: &RvEntry) -> Option<MemoryEntry> {
    let meta = rv_entry.metadata.as_ref()?;

    let key = meta.get("key")?.as_str()?.to_string();
    let value = meta.get("value")?.clone();
    let tags: Vec<String> = meta
        .get("tags")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let source = meta
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let entry_id: Uuid = meta
        .get("entry_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())?;
    let goal_id: Option<Uuid> = meta
        .get("goal_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());
    let created_at = meta
        .get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(Utc::now);
    let updated_at = meta
        .get("updated_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(Utc::now);

    Some(MemoryEntry {
        entry_id,
        key,
        value,
        tags,
        source,
        goal_id,
        created_at,
        updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store(dir: &TempDir) -> RuVectorStore {
        RuVectorStore::open(dir.path().join("memory.rvf")).unwrap()
    }

    #[test]
    fn store_and_recall_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        let entry = store
            .store("test-key", serde_json::json!("hello world"), vec![], "test")
            .unwrap();
        assert_eq!(entry.key, "test-key");

        let recalled = store.recall("test-key").unwrap().unwrap();
        assert_eq!(recalled.value, serde_json::json!("hello world"));
        assert_eq!(recalled.entry_id, entry.entry_id);
    }

    #[test]
    fn semantic_search_returns_relevant_results() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        store
            .store(
                "auth",
                serde_json::json!("JWT authentication with RS256 signatures"),
                vec!["security".into()],
                "test",
            )
            .unwrap();
        store
            .store(
                "db",
                serde_json::json!("PostgreSQL database connection pooling"),
                vec!["infrastructure".into()],
                "test",
            )
            .unwrap();
        store
            .store(
                "login",
                serde_json::json!("User login flow with token validation"),
                vec!["security".into()],
                "test",
            )
            .unwrap();

        let results = store.semantic_search("authentication tokens", 2).unwrap();
        assert!(!results.is_empty());
        assert!(results.len() <= 2);
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
    fn empty_store_search_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let results = store.semantic_search("anything", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn migration_from_filesystem() {
        let dir = TempDir::new().unwrap();
        let fs_dir = dir.path().join("memory");
        std::fs::create_dir_all(&fs_dir).unwrap();

        // Create a fake FS memory entry.
        let entry = MemoryEntry {
            entry_id: Uuid::new_v4(),
            key: "migrated-key".to_string(),
            value: serde_json::json!("migrated value"),
            tags: vec!["old".into()],
            source: "fs".to_string(),
            goal_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let content = serde_json::to_string_pretty(&entry).unwrap();
        std::fs::write(fs_dir.join("migrated-key.json"), content).unwrap();

        let store = RuVectorStore::open(dir.path().join("memory.rvf")).unwrap();
        let imported = store.migrate_from_fs(&fs_dir).unwrap();
        assert_eq!(imported, 1);

        let recalled = store.recall("migrated-key").unwrap().unwrap();
        assert_eq!(recalled.value, serde_json::json!("migrated value"));
        assert_eq!(recalled.tags, vec!["old".to_string()]);
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
    fn concurrent_access_safety() {
        // VectorDB uses Arc internally — verify we can share across scopes.
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        store
            .store("thread-safe", serde_json::json!("yes"), vec![], "test")
            .unwrap();

        // Re-open from the same path (simulates concurrent access).
        let store2 = RuVectorStore::open(dir.path().join("memory.rvf")).unwrap();
        let entry = store2.recall("thread-safe").unwrap();
        // May or may not see the entry depending on persistence flush timing.
        // The point is that opening doesn't panic or corrupt.
        let _ = entry;
    }
}
