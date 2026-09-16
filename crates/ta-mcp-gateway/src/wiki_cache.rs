//! Local cache for Wayfinder wiki pages, keyed by `wiki_manifest`'s `sha`
//! diff -- see `docs/superpowers/specs/2026-09-15-virtual-team-wiki-retrieval-design.md`
//! in the `ta-virtual-team` repo ("Local cache" section) for the design
//! this implements.
//!
//! Layout: `.ta/wiki-cache/{scope}/{id}/{page_id}.md` (OKF-shaped:
//! frontmatter + body, mirroring exactly what Wayfinder stores) plus a
//! sibling `.ta/wiki-cache/manifest.json` tracking `{page_id: {sha,
//! cached_at}}` per `{scope}/{id}`, the local half of the diff.
//!
//! Cache hits are deliberately stale-tolerant (see the design doc's own
//! explicit acknowledgment of this tradeoff): `get()` never checks
//! Wayfinder before returning a cached page. Freshness depends entirely on
//! something else (the daemon-owned periodic sync task) having refreshed
//! the manifest recently.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::wiki_client::WikiPage;

/// One cache entry's bookkeeping (not the page content itself, which
/// lives in the sibling `.md` file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub sha: String,
    pub cached_at: chrono::DateTime<chrono::Utc>,
}

/// `.ta/wiki-cache/manifest.json`'s on-disk shape: `{scope}/{id}` ->
/// `{page_id}` -> `CacheEntry`. Two levels of map rather than a flat
/// `(scope, id, page_id)` tuple key because `serde_json` can't use a
/// tuple as a map key, and `{scope}/{id}` composed into one string key
/// (e.g. `"project/proj-1"`) is simpler than a nested three-level map for
/// the same information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LocalManifest {
    #[serde(flatten)]
    scopes: HashMap<String, HashMap<String, CacheEntry>>,
}

pub struct WikiCache {
    /// `.ta/wiki-cache`
    root: PathBuf,
}

/// Composes `{scope}/{id}` into the single string key `LocalManifest`
/// uses -- `scope` is always `"project"`/`"org"` (no `/` in practice), so
/// this can't collide across different `(scope, id)` pairs.
fn scope_key(scope: &str, id: &str) -> String {
    format!("{scope}/{id}")
}

impl WikiCache {
    /// `project_root` is the real project root (`.ta/wiki-cache` under
    /// it), not a goal's staging directory -- same reasoning
    /// `tools/whiteboard.rs` already documents for its own daemon client:
    /// staging's `.ta/` never contains a prior cache, so every read would
    /// silently miss.
    pub fn new(project_root: &Path) -> Self {
        Self {
            root: project_root.join(".ta").join("wiki-cache"),
        }
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    fn page_path(&self, scope: &str, id: &str, page_id: &str) -> PathBuf {
        self.root.join(scope).join(id).join(format!("{page_id}.md"))
    }

    fn load_manifest(&self) -> anyhow::Result<LocalManifest> {
        let path = self.manifest_path();
        if !path.exists() {
            return Ok(LocalManifest::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    fn save_manifest(&self, manifest: &LocalManifest) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let raw = serde_json::to_string_pretty(manifest)?;
        std::fs::write(self.manifest_path(), raw)?;
        Ok(())
    }

    /// Every locally-known `{page_id: sha}` pair for one scope, the local
    /// half of the diff against a fresh `wiki_manifest` call. Empty (not
    /// an error) when nothing has been cached for this scope yet.
    pub fn known_shas(&self, scope: &str, id: &str) -> anyhow::Result<HashMap<String, String>> {
        let manifest = self.load_manifest()?;
        Ok(manifest
            .scopes
            .get(&scope_key(scope, id))
            .map(|entries| {
                entries
                    .iter()
                    .map(|(page_id, entry)| (page_id.clone(), entry.sha.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Reads a cached page from disk, `None` on a cache miss (not an
    /// error -- a miss is an expected, routine outcome the caller falls
    /// back on, not a failure).
    pub fn get(&self, scope: &str, id: &str, page_id: &str) -> anyhow::Result<Option<WikiPage>> {
        let path = self.page_path(scope, id, page_id);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(Some(parse_okf_file(page_id, &raw)?))
    }

    /// Writes (or overwrites) one page into the cache and records its
    /// `sha` in the local manifest. Used both by the background sync task
    /// (refreshing many pages at once) and by `tools/wiki.rs`'s
    /// create/update handlers (caching their own write's result
    /// immediately, per the design doc: "a role that just wrote a page
    /// and immediately re-reads it should see its own write").
    pub fn put(&self, scope: &str, id: &str, page: &WikiPage) -> anyhow::Result<()> {
        let path = self.page_path(scope, id, &page.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, render_okf_file(page))?;

        let mut manifest = self.load_manifest()?;
        manifest
            .scopes
            .entry(scope_key(scope, id))
            .or_default()
            .insert(
                page.id.clone(),
                CacheEntry {
                    sha: page.sha.clone(),
                    cached_at: chrono::Utc::now(),
                },
            );
        self.save_manifest(&manifest)?;
        Ok(())
    }
}

/// OKF-shaped rendering: YAML frontmatter (`---` delimited) plus the
/// markdown body, matching what Wayfinder itself stores per the design
/// doc's "mirroring exactly what Wayfinder stores" requirement.
fn render_okf_file(page: &WikiPage) -> String {
    let mut frontmatter = String::new();
    frontmatter.push_str(&format!("id: {}\n", page.id));
    frontmatter.push_str(&format!("title: {}\n", toml_safe_yaml_string(&page.title)));
    if let Some(t) = &page.r#type {
        frontmatter.push_str(&format!("type: {}\n", toml_safe_yaml_string(t)));
    }
    if !page.tags.is_empty() {
        frontmatter.push_str("tags:\n");
        for tag in &page.tags {
            frontmatter.push_str(&format!("  - {}\n", toml_safe_yaml_string(tag)));
        }
    }
    frontmatter.push_str(&format!("sha: {}\n", page.sha));
    if let Some(updated_at) = &page.updated_at {
        frontmatter.push_str(&format!("updated_at: {}\n", updated_at));
    }
    format!("---\n{frontmatter}---\n\n{}\n", page.body)
}

/// Inverse of `render_okf_file`. Deliberately tolerant of a missing/odd
/// frontmatter field (falls back to sane defaults) rather than failing
/// the whole cache read -- a corrupt or partially-written cache file
/// should degrade to "treat as a cache miss upstream," not panic or
/// error out a tool call. Callers that need strict validation should
/// compare the parsed `sha` against a freshly fetched one, not rely on
/// this parser to catch corruption.
fn parse_okf_file(page_id: &str, raw: &str) -> anyhow::Result<WikiPage> {
    let mut title = String::new();
    let mut r#type = None;
    let mut tags = Vec::new();
    let mut sha = String::new();
    let mut updated_at = None;

    if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let frontmatter = &rest[..end];
            let body_start = end + "\n---\n".len();
            let body = rest[body_start..].trim_start_matches('\n').to_string();

            let mut in_tags = false;
            for line in frontmatter.lines() {
                if let Some(value) = line.strip_prefix("  - ") {
                    if in_tags {
                        tags.push(unyaml_string(value));
                    }
                    continue;
                }
                in_tags = false;
                if let Some(value) = line.strip_prefix("title: ") {
                    title = unyaml_string(value);
                } else if let Some(value) = line.strip_prefix("type: ") {
                    r#type = Some(unyaml_string(value));
                } else if line == "tags:" {
                    in_tags = true;
                } else if let Some(value) = line.strip_prefix("sha: ") {
                    sha = value.to_string();
                } else if let Some(value) = line.strip_prefix("updated_at: ") {
                    updated_at = Some(value.to_string());
                }
            }

            return Ok(WikiPage {
                id: page_id.to_string(),
                title,
                body: body.trim_end_matches('\n').to_string(),
                r#type,
                tags,
                sha,
                updated_at,
            });
        }
    }

    anyhow::bail!("cache file for page '{page_id}' is not in the expected OKF frontmatter shape")
}

/// Minimal YAML scalar quoting -- wraps in double quotes and escapes `"`
/// and `\` if the value contains anything that would otherwise be
/// ambiguous (a colon, a leading/trailing space, a quote). Titles/types/
/// tags here are always short, human-authored strings, not arbitrary
/// untrusted binary data, so this hand-rolled minimal escaper is
/// sufficient; it is deliberately not a general YAML encoder.
fn toml_safe_yaml_string(s: &str) -> String {
    if s.contains(':')
        || s.contains('"')
        || s.contains('\\')
        || s.starts_with(' ')
        || s.ends_with(' ')
    {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

fn unyaml_string(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_page(id: &str, sha: &str) -> WikiPage {
        WikiPage {
            id: id.to_string(),
            title: "A Decision".to_string(),
            body: "We decided X because Y.".to_string(),
            r#type: Some("decision".to_string()),
            tags: vec!["architecture".to_string(), "vcs".to_string()],
            sha: sha.to_string(),
            updated_at: Some("2026-09-15T00:00:00Z".to_string()),
        }
    }

    #[test]
    fn put_then_get_round_trips_the_page() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        let page = test_page("page-1", "sha-abc");

        cache.put("project", "proj-1", &page).unwrap();
        let fetched = cache.get("project", "proj-1", "page-1").unwrap().unwrap();

        assert_eq!(fetched.id, "page-1");
        assert_eq!(fetched.title, "A Decision");
        assert_eq!(fetched.body, "We decided X because Y.");
        assert_eq!(fetched.r#type.as_deref(), Some("decision"));
        assert_eq!(fetched.tags, vec!["architecture", "vcs"]);
        assert_eq!(fetched.sha, "sha-abc");
    }

    #[test]
    fn get_on_a_cache_miss_returns_none_not_an_error() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        assert!(cache.get("project", "proj-1", "nope").unwrap().is_none());
    }

    #[test]
    fn put_records_the_sha_in_known_shas() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        cache
            .put("project", "proj-1", &test_page("page-1", "sha-abc"))
            .unwrap();

        let known = cache.known_shas("project", "proj-1").unwrap();
        assert_eq!(known.get("page-1"), Some(&"sha-abc".to_string()));
    }

    #[test]
    fn known_shas_for_an_unseeded_scope_is_empty_not_an_error() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        assert!(cache.known_shas("org", "acme").unwrap().is_empty());
    }

    #[test]
    fn org_and_project_scopes_with_the_same_id_do_not_collide() {
        // Contrived but worth proving: scope_key composes scope+id into
        // one string, so this guards against a future refactor
        // accidentally hashing just `id`.
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        cache
            .put("org", "shared-id", &test_page("page-1", "org-sha"))
            .unwrap();
        cache
            .put("project", "shared-id", &test_page("page-1", "project-sha"))
            .unwrap();

        assert_eq!(
            cache
                .get("org", "shared-id", "page-1")
                .unwrap()
                .unwrap()
                .sha,
            "org-sha"
        );
        assert_eq!(
            cache
                .get("project", "shared-id", "page-1")
                .unwrap()
                .unwrap()
                .sha,
            "project-sha"
        );
    }

    #[test]
    fn overwriting_a_page_updates_both_the_file_and_the_manifest() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        cache
            .put("project", "proj-1", &test_page("page-1", "sha-v1"))
            .unwrap();

        let mut updated = test_page("page-1", "sha-v2");
        updated.body = "Revised content.".to_string();
        cache.put("project", "proj-1", &updated).unwrap();

        let fetched = cache.get("project", "proj-1", "page-1").unwrap().unwrap();
        assert_eq!(fetched.sha, "sha-v2");
        assert_eq!(fetched.body, "Revised content.");

        let known = cache.known_shas("project", "proj-1").unwrap();
        assert_eq!(known.get("page-1"), Some(&"sha-v2".to_string()));
    }

    #[test]
    fn a_title_containing_a_colon_and_quotes_round_trips() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        let mut page = test_page("page-1", "sha-abc");
        page.title = "Design: the \"tricky\" case".to_string();

        cache.put("project", "proj-1", &page).unwrap();
        let fetched = cache.get("project", "proj-1", "page-1").unwrap().unwrap();
        assert_eq!(fetched.title, "Design: the \"tricky\" case");
    }

    #[test]
    fn a_page_with_no_tags_and_no_type_round_trips() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        let page = WikiPage {
            id: "page-1".to_string(),
            title: "Bare Page".to_string(),
            body: "Just a body.".to_string(),
            r#type: None,
            tags: vec![],
            sha: "sha-abc".to_string(),
            updated_at: None,
        };

        cache.put("project", "proj-1", &page).unwrap();
        let fetched = cache.get("project", "proj-1", "page-1").unwrap().unwrap();
        assert_eq!(fetched.r#type, None);
        assert!(fetched.tags.is_empty());
    }

    #[test]
    fn a_multiline_body_round_trips_including_blank_lines() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        let mut page = test_page("page-1", "sha-abc");
        page.body = "Line one.\n\nLine two, after a blank line.\n- a list item".to_string();

        cache.put("project", "proj-1", &page).unwrap();
        let fetched = cache.get("project", "proj-1", "page-1").unwrap().unwrap();
        assert_eq!(fetched.body, page.body);
    }

    #[test]
    fn a_corrupt_cache_file_is_a_clean_error_not_a_panic() {
        let dir = TempDir::new().unwrap();
        let cache = WikiCache::new(dir.path());
        let path = dir
            .path()
            .join(".ta")
            .join("wiki-cache")
            .join("project")
            .join("proj-1");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("page-1.md"), "not an OKF file at all").unwrap();

        let result = cache.get("project", "proj-1", "page-1");
        assert!(result.is_err());
    }
}
