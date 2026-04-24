// registry.rs — Workflow template registry protocol (v0.15.27).
//
// Registry index format (JSON array):
//   [{ name, description, version, tags, url, min_ta_version }, ...]
//
// The index is cached locally at ~/.config/ta/workflow-registry-index.json.
// Update with: ta workflow update-index [--url <url>]
// Default registry: built-in templates only (no external network call required).

use serde::{Deserialize, Serialize};

/// A single entry in the workflow template registry index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Template name (matches the YAML `name:` field).
    pub name: String,
    /// Short description of the template.
    pub description: String,
    /// Semver of this template release (e.g. "0.1.0").
    pub version: String,
    /// Tags used for discovery (e.g. ["security", "review"]).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Download URL for the template YAML. None for built-in templates.
    #[serde(default)]
    pub url: Option<String>,
    /// Minimum TA binary version required to run this template.
    #[serde(default)]
    pub min_ta_version: Option<String>,
}

/// A workflow template registry index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryIndex {
    pub entries: Vec<RegistryEntry>,
}

impl RegistryIndex {
    /// Load the index: try the cached file first, fall back to built-in entries.
    pub fn load() -> Self {
        if let Some(path) = cached_index_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(entries) = serde_json::from_str::<Vec<RegistryEntry>>(&content) {
                    return Self { entries };
                }
            }
        }
        Self::builtin()
    }

    /// The default built-in index — no network call required.
    pub fn builtin() -> Self {
        Self {
            entries: vec![
                RegistryEntry {
                    name: "plan-build-phases".to_string(),
                    description: "Iterate pending PLAN.md phases through the governed build workflow.".to_string(),
                    version: "0.15.27".to_string(),
                    tags: vec![
                        "plan".to_string(),
                        "phases".to_string(),
                        "automation".to_string(),
                    ],
                    url: None,
                    min_ta_version: Some("0.15.23".to_string()),
                },
                RegistryEntry {
                    name: "governed-goal".to_string(),
                    description: "Safe autonomous coding loop: run_goal → review → human_gate → apply → pr_sync.".to_string(),
                    version: "0.15.27".to_string(),
                    tags: vec![
                        "governance".to_string(),
                        "safe".to_string(),
                        "coding".to_string(),
                    ],
                    url: None,
                    min_ta_version: Some("0.14.8".to_string()),
                },
            ],
        }
    }

    /// Merge external entries into this index, deduplicating by name (external wins).
    pub fn merge(&mut self, external: Self) {
        for entry in external.entries {
            if let Some(existing) = self.entries.iter_mut().find(|e| e.name == entry.name) {
                *existing = entry;
            } else {
                self.entries.push(entry);
            }
        }
    }

    /// Save this index to the local cache file.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = cached_index_path()
            .ok_or_else(|| anyhow::anyhow!("cannot determine registry cache path: HOME not set"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "failed to create cache directory {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }
        let json = serde_json::to_string_pretty(&self.entries)?;
        std::fs::write(&path, &json).map_err(|e| {
            anyhow::anyhow!("failed to write registry cache {}: {}", path.display(), e)
        })?;
        Ok(())
    }

    /// Search entries by keyword (name, description, or tags).
    pub fn search(&self, query: &str) -> Vec<&RegistryEntry> {
        let q = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                e.name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// Filter entries that have an exact tag match (case-insensitive).
    pub fn by_tag(&self, tag: &str) -> Vec<&RegistryEntry> {
        let tag_lower = tag.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.tags.iter().any(|t| t.to_lowercase() == tag_lower))
            .collect()
    }

    /// Find an entry by exact name.
    pub fn get(&self, name: &str) -> Option<&RegistryEntry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

/// Returns the path to the cached registry index file, or None if HOME is not set.
pub fn cached_index_path() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("ta")
            .join("workflow-registry-index.json"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_index_has_expected_entries() {
        let idx = RegistryIndex::builtin();
        let names: Vec<&str> = idx.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"plan-build-phases"), "got: {:?}", names);
        assert!(names.contains(&"governed-goal"), "got: {:?}", names);
    }

    #[test]
    fn search_by_keyword_matches_name() {
        let idx = RegistryIndex::builtin();
        let results = idx.search("plan");
        assert!(!results.is_empty());
        assert!(results.iter().any(|e| e.name == "plan-build-phases"));
    }

    #[test]
    fn search_by_keyword_matches_description() {
        let idx = RegistryIndex::builtin();
        let results = idx.search("autonomous");
        assert!(!results.is_empty());
        assert!(results.iter().any(|e| e.name == "governed-goal"));
    }

    #[test]
    fn search_by_keyword_matches_tag() {
        let idx = RegistryIndex::builtin();
        let results = idx.search("governance");
        assert!(!results.is_empty());
        assert!(results.iter().any(|e| e.name == "governed-goal"));
    }

    #[test]
    fn search_no_match_returns_empty() {
        let idx = RegistryIndex::builtin();
        let results = idx.search("zzz-nonexistent-zzz");
        assert!(results.is_empty());
    }

    #[test]
    fn by_tag_exact_match() {
        let idx = RegistryIndex::builtin();
        let results = idx.by_tag("plan");
        assert!(!results.is_empty());
        assert!(results.iter().any(|e| e.name == "plan-build-phases"));
    }

    #[test]
    fn by_tag_case_insensitive() {
        let idx = RegistryIndex::builtin();
        let results = idx.by_tag("PLAN");
        assert!(!results.is_empty());
    }

    #[test]
    fn by_tag_no_match() {
        let idx = RegistryIndex::builtin();
        let results = idx.by_tag("zzz-nonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn get_by_name_found() {
        let idx = RegistryIndex::builtin();
        let entry = idx.get("governed-goal");
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().name, "governed-goal");
    }

    #[test]
    fn get_by_name_not_found() {
        let idx = RegistryIndex::builtin();
        assert!(idx.get("nonexistent").is_none());
    }

    #[test]
    fn merge_external_overrides_builtin() {
        let mut idx = RegistryIndex::builtin();
        let external = RegistryIndex {
            entries: vec![RegistryEntry {
                name: "governed-goal".to_string(),
                description: "Custom override".to_string(),
                version: "1.0.0".to_string(),
                tags: vec!["custom".to_string()],
                url: Some("https://example.com/template.yaml".to_string()),
                min_ta_version: None,
            }],
        };
        idx.merge(external);
        let entry = idx.get("governed-goal").unwrap();
        assert_eq!(entry.description, "Custom override");
        assert_eq!(entry.version, "1.0.0");
    }

    #[test]
    fn merge_adds_new_entries() {
        let mut idx = RegistryIndex::builtin();
        let initial_count = idx.entries.len();
        let external = RegistryIndex {
            entries: vec![RegistryEntry {
                name: "new-template".to_string(),
                description: "A new template".to_string(),
                version: "0.1.0".to_string(),
                tags: vec![],
                url: None,
                min_ta_version: None,
            }],
        };
        idx.merge(external);
        assert_eq!(idx.entries.len(), initial_count + 1);
        assert!(idx.get("new-template").is_some());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");
        let idx = RegistryIndex::builtin();
        let json = serde_json::to_string_pretty(&idx.entries).unwrap();
        std::fs::write(&path, &json).unwrap();
        let loaded: Vec<RegistryEntry> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.len(), idx.entries.len());
        assert_eq!(loaded[0].name, idx.entries[0].name);
    }

    #[test]
    fn registry_entry_serialization_roundtrip() {
        let entry = RegistryEntry {
            name: "my-template".to_string(),
            description: "A test template".to_string(),
            version: "0.2.0".to_string(),
            tags: vec!["test".to_string(), "ci".to_string()],
            url: Some("https://example.com/template.yaml".to_string()),
            min_ta_version: Some("0.15.0".to_string()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let restored: RegistryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.name, entry.name);
        assert_eq!(restored.tags, entry.tags);
        assert_eq!(restored.url, entry.url);
    }
}
