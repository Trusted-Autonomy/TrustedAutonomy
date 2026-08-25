// cache.rs — the local half of the design doc's §7.2 "worktree model": a
// small on-disk mirror of every item this integration has pushed to
// Wayfinder, with per-item dirty-tracking so a failed push is retried on
// the next sync cycle instead of being silently lost (§8's "local outbox").
//
// One JSON file (`<project_root>/.ta/wayfinder_cache.json`), matching
// `ta_credentials::FileVault`'s whole-blob read/write shape. `0600`
// permissions on Unix, defense in depth: this file holds task titles/
// descriptions (project content), not credentials, but there's no reason
// to leave it group/world-readable when the vault right next to it isn't.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// One tracked item — either a phase gate task or a goal-run task, keyed by
/// its `external_id` (see `mapping.rs`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedItem {
    pub external_id: String,
    /// Wayfinder's own task id, once known (absent until the first
    /// successful push — a brand-new local item with no id yet).
    pub wayfinder_id: Option<String>,
    /// True when this item has a local write not yet confirmed pushed.
    /// Set before any network call is attempted (durability comes from
    /// writing this to disk first, matching §8's "durable, appended local
    /// record before it's ever sent" — even though this is a flag on an
    /// upsert-in-place record rather than a literal append log, the
    /// durability property is the same: the intent to push survives a
    /// crash between "decided to push" and "confirmed pushed").
    pub dirty: bool,
    /// Status/hold_reason last successfully pushed, used to detect a human
    /// override on the next pull (§7.2) without needing a second round
    /// trip.
    pub last_pushed_status: Option<String>,
    pub last_pushed_hold_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CacheData {
    items: HashMap<String, CachedItem>,
    /// `updated_since` watermark for the next `poll_changes` pull — the
    /// "behind remote" half of §7.3's staleness check.
    watermark: Option<String>,
}

pub struct LocalCache {
    path: PathBuf,
    data: CacheData,
}

impl LocalCache {
    /// Opens (or creates) the cache file at
    /// `<project_root>/.ta/wayfinder_cache.json`.
    pub fn open(project_root: &Path) -> anyhow::Result<Self> {
        let path = project_root.join(".ta").join("wayfinder_cache.json");
        let data = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse {} as JSON", path.display()))?
        } else {
            CacheData::default()
        };
        Ok(Self { path, data })
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.path, json)
            .with_context(|| format!("failed to write {}", self.path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to set permissions on {}", self.path.display()))?;
        }
        Ok(())
    }

    pub fn get(&self, external_id: &str) -> Option<&CachedItem> {
        self.data.items.get(external_id)
    }

    /// Marks `external_id` dirty (a local write pending push), creating the
    /// record if this is the first time this item has been touched.
    /// Written to disk immediately — see `CachedItem::dirty`'s doc comment
    /// on why this must happen before any network attempt.
    pub fn mark_dirty(&mut self, external_id: &str) -> anyhow::Result<()> {
        self.data
            .items
            .entry(external_id.to_string())
            .or_insert_with(|| CachedItem {
                external_id: external_id.to_string(),
                wayfinder_id: None,
                dirty: false,
                last_pushed_status: None,
                last_pushed_hold_reason: None,
            })
            .dirty = true;
        self.save()
    }

    /// Records a successful push: clears `dirty`, remembers the Wayfinder
    /// id and what was pushed (for override detection on the next pull).
    pub fn mark_pushed(
        &mut self,
        external_id: &str,
        wayfinder_id: &str,
        status: &str,
        hold_reason: Option<&str>,
    ) -> anyhow::Result<()> {
        let entry = self
            .data
            .items
            .entry(external_id.to_string())
            .or_insert_with(|| CachedItem {
                external_id: external_id.to_string(),
                wayfinder_id: None,
                dirty: false,
                last_pushed_status: None,
                last_pushed_hold_reason: None,
            });
        entry.wayfinder_id = Some(wayfinder_id.to_string());
        entry.dirty = false;
        entry.last_pushed_status = Some(status.to_string());
        entry.last_pushed_hold_reason = hold_reason.map(str::to_string);
        self.save()
    }

    /// Every item with a push still outstanding — the "ahead of remote"
    /// half of §7.3's staleness check, and what a sync cycle retries.
    pub fn dirty_items(&self) -> Vec<&CachedItem> {
        self.data.items.values().filter(|i| i.dirty).collect()
    }

    pub fn watermark(&self) -> Option<&str> {
        self.data.watermark.as_deref()
    }

    pub fn set_watermark(&mut self, watermark: String) -> anyhow::Result<()> {
        self.data.watermark = Some(watermark);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_fresh_cache_has_no_items_and_no_watermark() {
        let dir = TempDir::new().unwrap();
        let cache = LocalCache::open(dir.path()).unwrap();
        assert!(cache.dirty_items().is_empty());
        assert_eq!(cache.watermark(), None);
    }

    #[test]
    fn mark_dirty_then_mark_pushed_clears_dirty_and_records_the_id() {
        let dir = TempDir::new().unwrap();
        let mut cache = LocalCache::open(dir.path()).unwrap();

        cache.mark_dirty("ta-phase-gate:v0.1.0").unwrap();
        assert_eq!(cache.dirty_items().len(), 1);

        cache
            .mark_pushed("ta-phase-gate:v0.1.0", "task-1", "open", None)
            .unwrap();
        assert!(cache.dirty_items().is_empty());
        assert_eq!(
            cache
                .get("ta-phase-gate:v0.1.0")
                .unwrap()
                .wayfinder_id
                .as_deref(),
            Some("task-1")
        );
    }

    #[test]
    fn state_persists_across_opens() {
        let dir = TempDir::new().unwrap();
        {
            let mut cache = LocalCache::open(dir.path()).unwrap();
            cache.mark_dirty("ta-goal:abc").unwrap();
            cache.set_watermark("1700000000".to_string()).unwrap();
        }
        let cache = LocalCache::open(dir.path()).unwrap();
        assert_eq!(cache.dirty_items().len(), 1);
        assert_eq!(cache.watermark(), Some("1700000000"));
    }

    #[test]
    fn a_failed_push_stays_dirty_for_the_next_cycle() {
        // Simulates: mark_dirty (before the network attempt) succeeds, the
        // network call fails, mark_pushed is never called -- the item must
        // still show up as dirty on a fresh open, proving nothing was lost.
        let dir = TempDir::new().unwrap();
        {
            let mut cache = LocalCache::open(dir.path()).unwrap();
            cache.mark_dirty("ta-goal:abc").unwrap();
        }
        let cache = LocalCache::open(dir.path()).unwrap();
        assert_eq!(cache.dirty_items().len(), 1);
        assert_eq!(cache.dirty_items()[0].external_id, "ta-goal:abc");
    }

    #[cfg(unix)]
    #[test]
    fn cache_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let mut cache = LocalCache::open(dir.path()).unwrap();
        cache.mark_dirty("ta-goal:abc").unwrap();

        let path = dir.path().join(".ta").join("wayfinder_cache.json");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
