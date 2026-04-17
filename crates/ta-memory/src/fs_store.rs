// fs_store.rs — Filesystem-backed memory store.
//
// Stores each memory entry as a JSON file in `.ta/memory/`.
// File name is derived from the key (URL-encoded, truncated).
// Sufficient for small-to-medium projects. For large-scale semantic
// search, the ruvector backend can be enabled via cargo feature.

use std::cmp::Reverse;
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

    /// Convert a key to a safe filename (pub for conflict helpers).
    pub fn key_to_filename_pub(key: &str) -> String {
        Self::key_to_filename(key)
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
        entries.sort_by_key(|e| Reverse(e.created_at));
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
            scope: params.scope,
            file_paths: params.file_paths,
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

// ===========================================================================
// ProjectMemoryStore — scope-routing dual-directory memory store (v0.15.13.3)
// ===========================================================================
//
// Entries with scope = "project" or scope = "team" are stored in
// `project_dir` (.ta/project-memory/) which is VCS-committed and shared.
// All other entries (scope = "local" or None) go to `local_dir` (.ta/memory/).
//
// Read operations (recall, lookup, list) merge both directories.
// Project entries take precedence over local entries when keys collide —
// i.e., a project-scope entry wins over a local-scope entry for the same key.
//
// Conflict detection: on project-dir reads, if two JSON files produce the same
// entry key (different entry_ids), a ConflictPair is written to
// `.ta/project-memory/.conflicts/` and the newer entry is returned.

/// A scope-routing memory store that splits entries across local and project dirs.
///
/// | Scope          | Written to              | Committed? |
/// |----------------|-------------------------|------------|
/// | `None` / local | `.ta/memory/`           | No         |
/// | `project`      | `.ta/project-memory/`   | Yes        |
/// | `team`         | `.ta/project-memory/`   | Yes        |
pub struct ProjectMemoryStore {
    /// Local-scoped entries: `.ta/memory/`.
    local_store: FsMemoryStore,
    /// Project/team-scoped entries: `.ta/project-memory/`.
    project_store: FsMemoryStore,
    /// Path to `.ta/project-memory/` for conflict helpers.
    project_dir: std::path::PathBuf,
}

impl ProjectMemoryStore {
    /// Create a new `ProjectMemoryStore` for the given project root.
    ///
    /// - `local_dir`:   `.ta/memory/`
    /// - `project_dir`: `.ta/project-memory/`
    pub fn new(
        local_dir: impl AsRef<std::path::Path>,
        project_dir: impl AsRef<std::path::Path>,
    ) -> Self {
        let project_dir = project_dir.as_ref().to_path_buf();
        Self {
            local_store: FsMemoryStore::new(local_dir),
            project_store: FsMemoryStore::new(&project_dir),
            project_dir,
        }
    }

    /// Create from a project root (`.ta/memory/` and `.ta/project-memory/`).
    pub fn for_project(project_root: impl AsRef<std::path::Path>) -> Self {
        let root = project_root.as_ref();
        Self::new(
            root.join(".ta").join("memory"),
            root.join(".ta").join("project-memory"),
        )
    }

    /// Return a read-only reference to the project store (for list/lookup).
    pub fn project_store(&self) -> &FsMemoryStore {
        &self.project_store
    }

    /// Return a read-only reference to the local store (for list/lookup).
    pub fn local_store(&self) -> &FsMemoryStore {
        &self.local_store
    }

    /// Path to the project-memory directory.
    pub fn project_dir(&self) -> &std::path::Path {
        &self.project_dir
    }

    /// Return whether the given scope routes to the project store.
    fn is_project_scope(scope: Option<&str>) -> bool {
        matches!(scope, Some("project") | Some("team"))
    }

    /// Merge local and project entries, with project entries taking precedence.
    /// Also detects and records conflicts within the project store itself.
    fn read_merged(&self) -> Result<Vec<crate::store::MemoryEntry>, crate::error::MemoryError> {
        let local_entries = self.local_store.read_all()?;
        let project_entries = self.project_store.read_all()?;

        // Detect conflicts in project entries: same key, different entry_ids.
        let mut project_by_key: std::collections::HashMap<String, Vec<crate::store::MemoryEntry>> =
            std::collections::HashMap::new();
        for e in &project_entries {
            project_by_key
                .entry(e.key.clone())
                .or_default()
                .push(e.clone());
        }
        let mut deduplicated_project: Vec<crate::store::MemoryEntry> = Vec::new();
        for (key, mut versions) in project_by_key {
            if versions.len() == 1 {
                deduplicated_project.push(versions.remove(0));
            } else {
                // Conflict detected: write to .conflicts/ and take the newest entry.
                versions.sort_by_key(|v| Reverse(v.updated_at));
                let winner = versions.remove(0);
                let loser = versions.remove(0);
                let conflict = crate::store::ConflictPair {
                    key: key.clone(),
                    ours: winner.clone(),
                    theirs: loser,
                    base: None,
                    detected_at: chrono::Utc::now(),
                };
                // Best-effort conflict write — don't fail the read operation.
                let _ = crate::conflict::write_conflict(&self.project_dir, &conflict);
                tracing::warn!(
                    key = %key,
                    "project-memory conflict detected — auto-resolved by timestamp. \
                     Run `ta memory conflicts` to review."
                );
                deduplicated_project.push(winner);
            }
        }

        // Merge: build a map keyed by entry key, project entries win.
        let mut merged: std::collections::HashMap<String, crate::store::MemoryEntry> =
            std::collections::HashMap::new();
        for e in local_entries {
            merged.insert(e.key.clone(), e);
        }
        // Project entries overwrite local entries for same key.
        for e in deduplicated_project {
            merged.insert(e.key.clone(), e);
        }

        let mut result: Vec<_> = merged.into_values().collect();
        result.sort_by_key(|e| Reverse(e.created_at));
        Ok(result)
    }
}

impl crate::store::MemoryStore for ProjectMemoryStore {
    fn store(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
    ) -> Result<crate::store::MemoryEntry, crate::error::MemoryError> {
        self.store_with_params(
            key,
            value,
            tags,
            source,
            crate::store::StoreParams::default(),
        )
    }

    fn store_with_params(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
        params: crate::store::StoreParams,
    ) -> Result<crate::store::MemoryEntry, crate::error::MemoryError> {
        if Self::is_project_scope(params.scope.as_deref()) {
            self.project_store
                .store_with_params(key, value, tags, source, params)
        } else {
            self.local_store
                .store_with_params(key, value, tags, source, params)
        }
    }

    fn recall(
        &self,
        key: &str,
    ) -> Result<Option<crate::store::MemoryEntry>, crate::error::MemoryError> {
        // Project store takes precedence.
        if let Some(e) = self.project_store.recall(key)? {
            return Ok(Some(e));
        }
        self.local_store.recall(key)
    }

    fn lookup(
        &self,
        query: crate::store::MemoryQuery,
    ) -> Result<Vec<crate::store::MemoryEntry>, crate::error::MemoryError> {
        let all = self.read_merged()?;
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
                if let Some(ref phase) = query.phase_id {
                    match &e.phase_id {
                        Some(ep) if ep != phase => return false,
                        _ => {}
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

    fn list(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<crate::store::MemoryEntry>, crate::error::MemoryError> {
        let all = self.read_merged()?;
        match limit {
            Some(n) => Ok(all.into_iter().take(n).collect()),
            None => Ok(all),
        }
    }

    fn forget(&mut self, key: &str) -> Result<bool, crate::error::MemoryError> {
        let p = self.project_store.forget(key)?;
        let l = self.local_store.forget(key)?;
        Ok(p || l)
    }
}

// ---------------------------------------------------------------------------
// ProjectMemoryStore tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod project_store_tests {
    use super::*;
    use crate::store::{MemoryStore, StoreParams};
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> ProjectMemoryStore {
        ProjectMemoryStore::new(dir.path().join("memory"), dir.path().join("project-memory"))
    }

    #[test]
    fn project_scope_goes_to_project_dir() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        store
            .store_with_params(
                "arch:decision",
                serde_json::json!("use JWT"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("project".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Should appear in project-memory dir.
        let proj_dir = dir.path().join("project-memory");
        assert!(proj_dir.exists(), "project-memory dir should be created");
        let files: Vec<_> = std::fs::read_dir(&proj_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .collect();
        assert_eq!(files.len(), 1, "one entry in project-memory");

        // Should NOT appear in local memory dir.
        let local_dir = dir.path().join("memory");
        if local_dir.exists() {
            let local_files: Vec<_> = std::fs::read_dir(&local_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .collect();
            assert_eq!(local_files.len(), 0, "no entries in local memory");
        }
    }

    #[test]
    fn local_scope_goes_to_local_dir() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        store
            .store_with_params(
                "scratch:note",
                serde_json::json!("temp"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("local".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        let local_dir = dir.path().join("memory");
        assert!(local_dir.exists());
        let files: Vec<_> = std::fs::read_dir(&local_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .collect();
        assert_eq!(files.len(), 1, "one entry in local memory");

        // Should NOT appear in project-memory dir.
        let proj_dir = dir.path().join("project-memory");
        if proj_dir.exists() {
            let proj_files: Vec<_> = std::fs::read_dir(&proj_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .collect();
            assert_eq!(proj_files.len(), 0, "no entries in project-memory");
        }
    }

    #[test]
    fn none_scope_goes_to_local_dir() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        store
            .store("implicit:local", serde_json::json!("value"), vec![], "test")
            .unwrap();

        let local_dir = dir.path().join("memory");
        assert!(local_dir.exists());
    }

    #[test]
    fn team_scope_goes_to_project_dir() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        store
            .store_with_params(
                "team:convention",
                serde_json::json!("use tabs"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("team".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        let proj_dir = dir.path().join("project-memory");
        assert!(
            proj_dir.exists(),
            "project-memory dir should be created for team scope"
        );
    }

    #[test]
    fn list_merges_both_stores() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        store
            .store_with_params(
                "local:key",
                serde_json::json!("local"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("local".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .store_with_params(
                "project:key",
                serde_json::json!("project"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("project".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 2, "both entries visible in merged list");
    }

    #[test]
    fn project_entry_wins_over_local_for_same_key() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        // Write local entry first.
        store
            .store_with_params(
                "shared:key",
                serde_json::json!("local value"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("local".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Write project entry with same key.
        store
            .store_with_params(
                "shared:key",
                serde_json::json!("project value"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("project".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();

        let recalled = store.recall("shared:key").unwrap().unwrap();
        assert_eq!(
            recalled.value,
            serde_json::json!("project value"),
            "project entry should win"
        );

        let all = store.list(None).unwrap();
        assert_eq!(
            all.len(),
            1,
            "merged list should de-duplicate by key (project wins)"
        );
    }

    #[test]
    fn forget_removes_from_both_stores() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        store
            .store_with_params(
                "key",
                serde_json::json!("v"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("project".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.forget("key").unwrap());
        assert!(store.recall("key").unwrap().is_none());
    }

    #[test]
    fn file_paths_stored_in_entry() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);

        store
            .store_with_params(
                "arch:api",
                serde_json::json!("use REST not gRPC"),
                vec![],
                "test",
                StoreParams {
                    scope: Some("project".to_string()),
                    file_paths: vec!["apps/ta-cli/src/commands/api.rs".to_string()],
                    ..Default::default()
                },
            )
            .unwrap();

        let recalled = store.recall("arch:api").unwrap().unwrap();
        assert_eq!(recalled.file_paths, vec!["apps/ta-cli/src/commands/api.rs"]);
    }
}
