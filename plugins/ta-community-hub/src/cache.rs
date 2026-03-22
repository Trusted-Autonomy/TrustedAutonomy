//! Local cache management for community resources.
//!
//! Remote resources (github:) are synced to `.ta/community-cache/<name>/`.
//! Local resources (local:) are read directly — no caching needed.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::registry::Resource;

/// Metadata stored alongside the cached content for each resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
    pub resource_name: String,
    pub synced_at: DateTime<Utc>,
    pub source: String,
    pub document_count: usize,
}

/// A single cached document.
#[derive(Debug, Clone)]
pub struct CachedDoc {
    /// Document ID: "<resource-name>/<relative-path-without-ext>"
    pub id: String,
    /// Raw markdown content.
    pub content: String,
    /// Last-modified timestamp (from sync metadata).
    pub synced_at: DateTime<Utc>,
}

impl CachedDoc {
    /// Is this document stale (older than `days` days)?
    pub fn is_stale(&self, days: i64) -> bool {
        let age = Utc::now() - self.synced_at;
        age.num_days() > days
    }
}

/// Cache directory accessor for a workspace.
pub struct ResourceCache {
    cache_root: PathBuf,
}

impl ResourceCache {
    pub fn new(workspace: &Path) -> Self {
        Self {
            cache_root: workspace.join(".ta").join("community-cache"),
        }
    }

    /// Path to the cache directory for a specific resource.
    pub fn resource_dir(&self, name: &str) -> PathBuf {
        self.cache_root.join(name)
    }

    /// Path to the metadata file for a resource's cache.
    pub fn metadata_path(&self, name: &str) -> PathBuf {
        self.resource_dir(name).join("_meta.json")
    }

    /// Load metadata for a cached resource, if it exists.
    pub fn load_metadata(&self, name: &str) -> Option<CacheMetadata> {
        let path = self.metadata_path(name);
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Write metadata after a successful sync.
    pub fn write_metadata(&self, meta: &CacheMetadata) -> Result<(), String> {
        let dir = self.resource_dir(&meta.resource_name);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create cache dir: {}", e))?;
        let json = serde_json::to_string_pretty(meta)
            .map_err(|e| format!("failed to serialize metadata: {}", e))?;
        std::fs::write(self.metadata_path(&meta.resource_name), json)
            .map_err(|e| format!("failed to write cache metadata: {}", e))
    }

    /// Write a document file into the cache.
    pub fn write_doc(&self, resource_name: &str, rel_path: &str, content: &str) -> Result<(), String> {
        let path = self.resource_dir(resource_name).join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create doc parent dir: {}", e))?;
        }
        std::fs::write(&path, content)
            .map_err(|e| format!("failed to write cached doc {}: {}", rel_path, e))
    }

    /// Load all documents from the cache for a resource.
    pub fn load_docs(&self, resource_name: &str) -> Vec<CachedDoc> {
        let dir = self.resource_dir(resource_name);
        let meta = self.load_metadata(resource_name);
        let synced_at = meta.map(|m| m.synced_at).unwrap_or_else(Utc::now);

        let mut docs = Vec::new();
        collect_docs(&dir, &dir, resource_name, synced_at, &mut docs);
        docs
    }

    /// Search across all enabled resources for documents matching a query.
    ///
    /// Returns documents that contain all query words (case-insensitive).
    pub fn search(
        &self,
        query: &str,
        resource_filter: Option<&str>,
        intent_filter: Option<&str>,
        resources: &[&Resource],
        token_budget: usize,
    ) -> Vec<SearchResult> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();

        let mut results = Vec::new();

        for resource in resources {
            if let Some(name) = resource_filter {
                if resource.name != name {
                    continue;
                }
            }
            if let Some(intent) = intent_filter {
                if resource.intent != intent {
                    continue;
                }
            }

            let docs = self.load_docs(&resource.name);
            for doc in docs {
                let lower_content = doc.content.to_lowercase();
                if words.iter().all(|w| lower_content.contains(w.as_str())) {
                    let score = words
                        .iter()
                        .map(|w| lower_content.matches(w.as_str()).count())
                        .sum::<usize>();

                    let excerpt = make_excerpt(&doc.content, &words, 300);
                    let stale = doc.is_stale(90);
                    results.push(SearchResult {
                        id: doc.id.clone(),
                        resource_name: resource.name.clone(),
                        intent: resource.intent.clone(),
                        score,
                        excerpt,
                        synced_at: doc.synced_at,
                        is_stale: stale,
                        content_bytes: doc.content.len(),
                    });
                }
            }
        }

        // Sort by relevance score descending.
        results.sort_by(|a, b| b.score.cmp(&a.score));

        // Enforce token budget (rough: 4 chars ≈ 1 token).
        let mut total_chars = 0usize;
        let char_budget = token_budget * 4;
        results.retain(|r| {
            total_chars += r.excerpt.len();
            total_chars <= char_budget
        });

        results
    }

    /// Fetch a specific document by ID ("<resource>/<relative-path>").
    pub fn get_doc(&self, id: &str, token_budget: usize) -> Option<DocumentResult> {
        let (resource_name, rel_path) = id.split_once('/')?;
        let meta = self.load_metadata(resource_name);
        let synced_at = meta.as_ref().map(|m| m.synced_at).unwrap_or_else(Utc::now);
        let is_stale = {
            let age = Utc::now() - synced_at;
            age.num_days() > 90
        };

        let cache_dir = self.resource_dir(resource_name);
        // Try exact path, then with .md extension.
        let candidates = [
            cache_dir.join(rel_path),
            cache_dir.join(format!("{}.md", rel_path)),
        ];
        for path in &candidates {
            if path.exists() {
                let content = std::fs::read_to_string(path).ok()?;
                let (truncated, summary) = enforce_budget(&content, token_budget);
                return Some(DocumentResult {
                    id: id.to_string(),
                    resource_name: resource_name.to_string(),
                    content: truncated,
                    synced_at,
                    is_stale,
                    truncated: summary.is_some(),
                    summary,
                });
            }
        }
        None
    }
}

/// A search result entry.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub id: String,
    pub resource_name: String,
    pub intent: String,
    pub score: usize,
    pub excerpt: String,
    pub synced_at: DateTime<Utc>,
    pub is_stale: bool,
    pub content_bytes: usize,
}

/// A fetched document.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentResult {
    pub id: String,
    pub resource_name: String,
    pub content: String,
    pub synced_at: DateTime<Utc>,
    pub is_stale: bool,
    pub truncated: bool,
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_docs(
    root: &Path,
    dir: &Path,
    resource_name: &str,
    synced_at: DateTime<Utc>,
    out: &mut Vec<CachedDoc>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_docs(root, &path, resource_name, synced_at, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if path.file_name().and_then(|n| n.to_str()) == Some("_meta.json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().to_string();
            // Strip .md extension from ID.
            let id_rel = rel_str.trim_end_matches(".md").to_string();
            let id = format!("{}/{}", resource_name, id_rel);
            out.push(CachedDoc { id, content, synced_at });
        }
    }
}

fn make_excerpt(content: &str, keywords: &[String], max_chars: usize) -> String {
    // Find the first line containing any keyword, return a window around it.
    let lower = content.to_lowercase();
    let start = keywords
        .iter()
        .filter_map(|w| lower.find(w.as_str()))
        .min()
        .unwrap_or(0);
    let begin = start.saturating_sub(50);
    let chars: Vec<char> = content.chars().collect();
    let end = (begin + max_chars).min(chars.len());
    chars[begin..end].iter().collect()
}

fn enforce_budget(content: &str, token_budget: usize) -> (String, Option<String>) {
    let char_budget = token_budget * 4;
    if content.len() <= char_budget {
        return (content.to_string(), None);
    }
    let truncated: String = content.chars().take(char_budget).collect();
    let summary = format!(
        "[Document truncated to {} tokens. Full document has {} characters. \
         Use community_get with a larger budget to retrieve the complete content.]",
        token_budget,
        content.len()
    );
    (truncated, Some(summary))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn setup_cache(dir: &Path, resource: &str, docs: &[(&str, &str)]) -> ResourceCache {
        let cache = ResourceCache::new(dir);
        let now = Utc::now();
        let meta = CacheMetadata {
            resource_name: resource.to_string(),
            synced_at: now,
            source: "local:test".into(),
            document_count: docs.len(),
        };
        cache.write_metadata(&meta).unwrap();
        for (name, content) in docs {
            cache.write_doc(resource, name, content).unwrap();
        }
        cache
    }

    #[test]
    fn search_finds_matching_docs() {
        let dir = tempfile::tempdir().unwrap();
        let cache = setup_cache(
            dir.path(),
            "api-docs",
            &[
                ("stripe.md", "Stripe PaymentIntents API for charging cards"),
                ("twilio.md", "Twilio SMS messaging service"),
            ],
        );
        let res = Resource {
            name: "api-docs".into(),
            intent: "api-integration".into(),
            description: "d".into(),
            source: "local:x".into(),
            content_path: "".into(),
            access: crate::registry::Access::ReadOnly,
            auto_query: false,
            languages: vec![],
            update_frequency: "on-demand".into(),
        };
        let results = cache.search("stripe payment", None, None, &[&res], 4000);
        assert!(!results.is_empty());
        assert!(results[0].id.contains("stripe"));
    }

    #[test]
    fn get_doc_returns_content() {
        let dir = tempfile::tempdir().unwrap();
        let cache = setup_cache(
            dir.path(),
            "api-docs",
            &[("stripe.md", "# Stripe API\n\nPaymentIntents reference.")],
        );
        let result = cache.get_doc("api-docs/stripe", 4000);
        assert!(result.is_some());
        let doc = result.unwrap();
        assert!(doc.content.contains("Stripe API"));
        assert!(!doc.truncated);
    }

    #[test]
    fn token_budget_truncates_large_doc() {
        let long = "x".repeat(10_000);
        let (truncated, summary) = enforce_budget(&long, 100);
        assert_eq!(truncated.len(), 400); // 100 tokens * 4 chars
        assert!(summary.is_some());
    }

    #[test]
    fn search_respects_resource_filter() {
        let dir = tempfile::tempdir().unwrap();
        let cache = setup_cache(
            dir.path(),
            "api-docs",
            &[("stripe.md", "Stripe API docs")],
        );
        setup_cache(dir.path(), "security", &[("cves.md", "Stripe CVE list")]);

        let api_res = Resource {
            name: "api-docs".into(),
            intent: "api-integration".into(),
            description: "d".into(),
            source: "local:x".into(),
            content_path: "".into(),
            access: crate::registry::Access::ReadOnly,
            auto_query: false,
            languages: vec![],
            update_frequency: "on-demand".into(),
        };
        let sec_res = Resource {
            name: "security".into(),
            intent: "security-intelligence".into(),
            description: "d".into(),
            source: "local:y".into(),
            content_path: "".into(),
            access: crate::registry::Access::ReadOnly,
            auto_query: false,
            languages: vec![],
            update_frequency: "on-demand".into(),
        };

        let results = cache.search("stripe", Some("api-docs"), None, &[&api_res, &sec_res], 4000);
        assert!(results.iter().all(|r| r.resource_name == "api-docs"));
    }
}
