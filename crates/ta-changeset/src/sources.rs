//! External source resolver for workflow/agent YAML configs.
//!
//! Provides types and caching logic for fetching workflow and agent definitions
//! from remote sources (registries, GitHub repos, raw URLs). The actual HTTP
//! fetching is intentionally **not** handled here — callers (e.g., the CLI)
//! supply fetched content and this module handles parsing, caching, and
//! lockfile management.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur when resolving external sources.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The source string could not be parsed into a known scheme.
    #[error("invalid source string: {input}")]
    InvalidSource { input: String },

    /// An HTTP fetch (or equivalent) failed.
    #[error("failed to fetch from {origin}: {reason}")]
    FetchFailed { origin: String, reason: String },

    /// A cache operation failed.
    #[error("cache error: {reason}")]
    CacheError { reason: String },

    /// The fetched version does not match the locked version.
    #[error("version mismatch for {name}: expected {expected}, got {actual}")]
    VersionMismatch {
        name: String,
        expected: String,
        actual: String,
    },

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// ExternalSource
// ---------------------------------------------------------------------------

/// Where to fetch an external workflow or agent definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalSource {
    /// A named entry in the TA community registry.
    Registry { org: String, name: String },

    /// A file in a GitHub repository.
    GitHub {
        org: String,
        repo: String,
        path: Option<String>,
        #[serde(rename = "ref")]
        ref_: Option<String>,
    },

    /// A raw URL pointing directly at a YAML file.
    Url { url: String },
}

impl ExternalSource {
    /// Parse a human-friendly source string into an [`ExternalSource`].
    ///
    /// Supported formats:
    /// - `registry:org/name`
    /// - `gh:org/repo`  or  `gh:org/repo/path/to/file.yaml`  or  `gh:org/repo@ref`
    /// - `https://...`  or  `http://...`
    pub fn parse(source: &str) -> Result<Self, SourceError> {
        let source = source.trim();

        if let Some(rest) = source.strip_prefix("registry:") {
            let parts: Vec<&str> = rest.splitn(2, '/').collect();
            if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
                return Err(SourceError::InvalidSource {
                    input: source.to_string(),
                });
            }
            return Ok(ExternalSource::Registry {
                org: parts[0].to_string(),
                name: parts[1].to_string(),
            });
        }

        if let Some(rest) = source.strip_prefix("gh:") {
            // Split off @ref if present
            let (path_part, ref_) = if let Some(idx) = rest.find('@') {
                (&rest[..idx], Some(rest[idx + 1..].to_string()))
            } else {
                (rest, None)
            };

            let segments: Vec<&str> = path_part.splitn(3, '/').collect();
            if segments.len() < 2 || segments[0].is_empty() || segments[1].is_empty() {
                return Err(SourceError::InvalidSource {
                    input: source.to_string(),
                });
            }
            let path = if segments.len() == 3 && !segments[2].is_empty() {
                Some(segments[2].to_string())
            } else {
                None
            };
            return Ok(ExternalSource::GitHub {
                org: segments[0].to_string(),
                repo: segments[1].to_string(),
                path,
                ref_,
            });
        }

        if source.starts_with("https://") || source.starts_with("http://") {
            return Ok(ExternalSource::Url {
                url: source.to_string(),
            });
        }

        Err(SourceError::InvalidSource {
            input: source.to_string(),
        })
    }

    /// Build the URL that a fetcher should GET to retrieve content for this source.
    pub fn fetch_url(&self) -> String {
        match self {
            ExternalSource::Registry { org, name } => {
                format!(
                    "https://registry.trustedautonomy.dev/v1/{}/{}.yaml",
                    org, name
                )
            }
            ExternalSource::GitHub {
                org,
                repo,
                path,
                ref_,
            } => {
                let branch = ref_.as_deref().unwrap_or("main");
                let file_path = path.as_deref().unwrap_or("workflow-package.yaml");
                format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/{}",
                    org, repo, branch, file_path
                )
            }
            ExternalSource::Url { url } => url.clone(),
        }
    }
}

impl fmt::Display for ExternalSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExternalSource::Registry { org, name } => write!(f, "registry:{}/{}", org, name),
            ExternalSource::GitHub {
                org,
                repo,
                path,
                ref_,
            } => {
                write!(f, "gh:{}/{}", org, repo)?;
                if let Some(p) = path {
                    write!(f, "/{}", p)?;
                }
                if let Some(r) = ref_ {
                    write!(f, "@{}", r)?;
                }
                Ok(())
            }
            ExternalSource::Url { url } => write!(f, "{}", url),
        }
    }
}

// ---------------------------------------------------------------------------
// PackageManifest
// ---------------------------------------------------------------------------

/// Describes a workflow/agent package fetched from an external source.
///
/// This corresponds to the `workflow-package.yaml` file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    /// Human-readable package name (e.g. `"ci-review"`).
    pub name: String,
    /// Semver version string.
    pub version: String,
    /// Optional author or organization.
    pub author: Option<String>,
    /// One-line description.
    pub description: Option<String>,
    /// Semver constraint on TA version (e.g. `">=0.9.8"`).
    pub ta_version: Option<String>,
    /// Relative file paths included in this package.
    pub files: Vec<String>,
}

// ---------------------------------------------------------------------------
// CachedItem / SourceCache
// ---------------------------------------------------------------------------

/// Metadata about a cached workflow or agent definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedItem {
    /// Logical name (e.g. `"ci-review"`).
    pub name: String,
    /// Version that was cached.
    pub version: String,
    /// Original source string (Display form of [`ExternalSource`]).
    pub source: String,
    /// ISO-8601 timestamp when the item was cached.
    pub cached_at: String,
    /// Absolute path to the cached YAML file.
    pub file_path: PathBuf,
}

/// Manages a local cache directory for externally-sourced definitions.
///
/// Cache layout:
/// ```text
/// ~/.ta/cache/{kind}/
///   {name}.yaml          — the cached YAML content
///   {name}.meta.json     — sidecar with CachedItem metadata
/// ```
pub struct SourceCache {
    cache_dir: PathBuf,
}

impl SourceCache {
    /// Create a new cache rooted at `~/.ta/cache/{kind}/`.
    ///
    /// `kind` is typically `"workflows"` or `"agents"`.
    pub fn new(kind: &str) -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());
        Self {
            cache_dir: PathBuf::from(home).join(".ta").join("cache").join(kind),
        }
    }

    /// Create a cache rooted at a custom directory (useful for tests).
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { cache_dir: dir }
    }

    /// Return the cache root directory.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    fn yaml_path(&self, name: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.yaml", name))
    }

    fn meta_path(&self, name: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.meta.json", name))
    }

    /// Return a cached item by name, or `None` if it is not cached.
    pub fn get(&self, name: &str) -> Option<CachedItem> {
        let meta_path = self.meta_path(name);
        let data = std::fs::read_to_string(&meta_path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Store content in the cache. Returns the resulting [`CachedItem`].
    pub fn store(
        &self,
        name: &str,
        content: &str,
        source: &ExternalSource,
        version: &str,
    ) -> Result<CachedItem, SourceError> {
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| SourceError::CacheError {
            reason: format!(
                "failed to create cache directory {}: {}",
                self.cache_dir.display(),
                e
            ),
        })?;

        let yaml_path = self.yaml_path(name);
        std::fs::write(&yaml_path, content).map_err(|e| SourceError::CacheError {
            reason: format!("failed to write {}: {}", yaml_path.display(), e),
        })?;

        let item = CachedItem {
            name: name.to_string(),
            version: version.to_string(),
            source: source.to_string(),
            cached_at: chrono::Utc::now().to_rfc3339(),
            file_path: yaml_path,
        };

        let meta_path = self.meta_path(name);
        let meta_json =
            serde_json::to_string_pretty(&item).map_err(|e| SourceError::CacheError {
                reason: format!("failed to serialize metadata: {}", e),
            })?;
        std::fs::write(&meta_path, meta_json).map_err(|e| SourceError::CacheError {
            reason: format!("failed to write {}: {}", meta_path.display(), e),
        })?;

        Ok(item)
    }

    /// Remove a cached item. Returns `true` if it existed.
    pub fn remove(&self, name: &str) -> Result<bool, SourceError> {
        let yaml_path = self.yaml_path(name);
        let meta_path = self.meta_path(name);

        let existed = yaml_path.exists() || meta_path.exists();
        if yaml_path.exists() {
            std::fs::remove_file(&yaml_path)?;
        }
        if meta_path.exists() {
            std::fs::remove_file(&meta_path)?;
        }
        Ok(existed)
    }

    /// List all cached items.
    pub fn list(&self) -> Vec<CachedItem> {
        let mut items = Vec::new();
        let entries = match std::fs::read_dir(&self.cache_dir) {
            Ok(entries) => entries,
            Err(_) => return items,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json")
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".meta.json"))
            {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(item) = serde_json::from_str::<CachedItem>(&data) {
                        items.push(item);
                    }
                }
            }
        }
        items.sort_by_key(|i| i.name.clone());
        items
    }
}

// ---------------------------------------------------------------------------
// Lockfile
// ---------------------------------------------------------------------------

/// A single pinned dependency in the lockfile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    /// Package name.
    pub name: String,
    /// Pinned version.
    pub version: String,
    /// Source string (Display form of [`ExternalSource`]).
    pub source: String,
    /// SHA-256 hex digest of the fetched content.
    pub checksum: String,
}

/// Version-pinning lockfile persisted as YAML.
///
/// Stored at `.ta/workflow.lock` (or `.ta/agent.lock`) and tracks the
/// exact version + checksum of every external dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lockfile {
    /// Ordered list of locked entries.
    pub entries: Vec<LockEntry>,
}

impl Lockfile {
    /// Create an empty lockfile.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Load a lockfile from disk. Returns an empty lockfile if the file does
    /// not exist.
    pub fn load(path: &Path) -> Result<Self, SourceError> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let data = std::fs::read_to_string(path)?;
        serde_yaml::from_str(&data).map_err(|e| SourceError::CacheError {
            reason: format!("failed to parse lockfile {}: {}", path.display(), e),
        })
    }

    /// Persist the lockfile to disk as YAML.
    pub fn save(&self, path: &Path) -> Result<(), SourceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let yaml = serde_yaml::to_string(self).map_err(|e| SourceError::CacheError {
            reason: format!("failed to serialize lockfile: {}", e),
        })?;
        std::fs::write(path, yaml)?;
        Ok(())
    }

    /// Add or update an entry. If an entry with the same name already exists,
    /// it is replaced.
    pub fn add(&mut self, entry: LockEntry) {
        self.remove(&entry.name);
        self.entries.push(entry);
    }

    /// Remove an entry by name. Returns `true` if it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.name != name);
        self.entries.len() < before
    }

    /// Look up an entry by name.
    pub fn get(&self, name: &str) -> Option<&LockEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Return all entries as a slice.
    pub fn entries(&self) -> &[LockEntry] {
        &self.entries
    }
}

impl Default for Lockfile {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Compute the SHA-256 hex digest of `content`.
pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Verify that `content` matches the expected `checksum`.
pub fn verify_checksum(content: &str, checksum: &str) -> bool {
    sha256_hex(content) == checksum
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // -- ExternalSource::parse -----------------------------------------------

    #[test]
    fn parse_registry_source() {
        let src = ExternalSource::parse("registry:trustedautonomy/workflows").unwrap();
        assert_eq!(
            src,
            ExternalSource::Registry {
                org: "trustedautonomy".into(),
                name: "workflows".into(),
            }
        );
    }

    #[test]
    fn parse_github_simple() {
        let src = ExternalSource::parse("gh:myorg/ta-workflows").unwrap();
        assert_eq!(
            src,
            ExternalSource::GitHub {
                org: "myorg".into(),
                repo: "ta-workflows".into(),
                path: None,
                ref_: None,
            }
        );
    }

    #[test]
    fn parse_github_with_path() {
        let src = ExternalSource::parse("gh:myorg/repo/path/to/file.yaml").unwrap();
        assert_eq!(
            src,
            ExternalSource::GitHub {
                org: "myorg".into(),
                repo: "repo".into(),
                path: Some("path/to/file.yaml".into()),
                ref_: None,
            }
        );
    }

    #[test]
    fn parse_github_with_ref() {
        let src = ExternalSource::parse("gh:myorg/repo@v1.2.3").unwrap();
        assert_eq!(
            src,
            ExternalSource::GitHub {
                org: "myorg".into(),
                repo: "repo".into(),
                path: None,
                ref_: Some("v1.2.3".into()),
            }
        );
    }

    #[test]
    fn parse_github_with_path_and_ref() {
        let src = ExternalSource::parse("gh:myorg/repo/workflows/ci.yaml@main").unwrap();
        assert_eq!(
            src,
            ExternalSource::GitHub {
                org: "myorg".into(),
                repo: "repo".into(),
                path: Some("workflows/ci.yaml".into()),
                ref_: Some("main".into()),
            }
        );
    }

    #[test]
    fn parse_url_https() {
        let src = ExternalSource::parse("https://example.com/workflow.yaml").unwrap();
        assert_eq!(
            src,
            ExternalSource::Url {
                url: "https://example.com/workflow.yaml".into(),
            }
        );
    }

    #[test]
    fn parse_url_http() {
        let src = ExternalSource::parse("http://localhost:8080/w.yaml").unwrap();
        assert_eq!(
            src,
            ExternalSource::Url {
                url: "http://localhost:8080/w.yaml".into(),
            }
        );
    }

    #[test]
    fn parse_invalid_returns_error() {
        assert!(ExternalSource::parse("ftp://bad").is_err());
        assert!(ExternalSource::parse("registry:").is_err());
        assert!(ExternalSource::parse("registry:org").is_err());
        assert!(ExternalSource::parse("gh:").is_err());
        assert!(ExternalSource::parse("gh:org").is_err());
        assert!(ExternalSource::parse("").is_err());
    }

    // -- Display / round-trip ------------------------------------------------

    #[test]
    fn display_round_trip() {
        let cases = vec![
            "registry:trustedautonomy/workflows",
            "gh:myorg/repo",
            "gh:myorg/repo/path/to/file.yaml",
            "https://example.com/workflow.yaml",
        ];
        for input in cases {
            let src = ExternalSource::parse(input).unwrap();
            assert_eq!(src.to_string(), input, "round-trip failed for {}", input);
        }
    }

    // -- fetch_url -----------------------------------------------------------

    #[test]
    fn fetch_url_registry() {
        let src = ExternalSource::Registry {
            org: "ta".into(),
            name: "ci".into(),
        };
        assert_eq!(
            src.fetch_url(),
            "https://registry.trustedautonomy.dev/v1/ta/ci.yaml"
        );
    }

    #[test]
    fn fetch_url_github_defaults() {
        let src = ExternalSource::GitHub {
            org: "org".into(),
            repo: "repo".into(),
            path: None,
            ref_: None,
        };
        assert_eq!(
            src.fetch_url(),
            "https://raw.githubusercontent.com/org/repo/main/workflow-package.yaml"
        );
    }

    #[test]
    fn fetch_url_github_custom() {
        let src = ExternalSource::GitHub {
            org: "org".into(),
            repo: "repo".into(),
            path: Some("defs/w.yaml".into()),
            ref_: Some("v2".into()),
        };
        assert_eq!(
            src.fetch_url(),
            "https://raw.githubusercontent.com/org/repo/v2/defs/w.yaml"
        );
    }

    // -- SourceCache ---------------------------------------------------------

    #[test]
    fn cache_store_get_list_remove() {
        let dir = tempdir().unwrap();
        let cache = SourceCache::with_dir(dir.path().to_path_buf());

        let source = ExternalSource::Registry {
            org: "ta".into(),
            name: "ci".into(),
        };
        let content = "name: ci\nversion: '1.0'\n";

        // store
        let item = cache.store("ci", content, &source, "1.0").unwrap();
        assert_eq!(item.name, "ci");
        assert_eq!(item.version, "1.0");
        assert!(item.file_path.exists());

        // get
        let fetched = cache.get("ci").unwrap();
        assert_eq!(fetched.name, "ci");
        assert_eq!(fetched.version, "1.0");

        // list
        let items = cache.list();
        assert_eq!(items.len(), 1);

        // remove
        let removed = cache.remove("ci").unwrap();
        assert!(removed);
        assert!(cache.get("ci").is_none());
        assert!(cache.list().is_empty());
    }

    #[test]
    fn cache_get_missing_returns_none() {
        let dir = tempdir().unwrap();
        let cache = SourceCache::with_dir(dir.path().to_path_buf());
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn cache_remove_missing_returns_false() {
        let dir = tempdir().unwrap();
        let cache = SourceCache::with_dir(dir.path().to_path_buf());
        assert!(!cache.remove("nonexistent").unwrap());
    }

    // -- Lockfile ------------------------------------------------------------

    #[test]
    fn lockfile_add_get_remove() {
        let mut lock = Lockfile::new();
        assert!(lock.get("ci").is_none());

        lock.add(LockEntry {
            name: "ci".into(),
            version: "1.0".into(),
            source: "registry:ta/ci".into(),
            checksum: "abc123".into(),
        });
        assert_eq!(lock.get("ci").unwrap().version, "1.0");

        // update replaces
        lock.add(LockEntry {
            name: "ci".into(),
            version: "2.0".into(),
            source: "registry:ta/ci".into(),
            checksum: "def456".into(),
        });
        assert_eq!(lock.get("ci").unwrap().version, "2.0");
        assert_eq!(lock.entries.len(), 1);

        assert!(lock.remove("ci"));
        assert!(!lock.remove("ci"));
    }

    #[test]
    fn lockfile_save_load_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.lock");

        let mut lock = Lockfile::new();
        lock.add(LockEntry {
            name: "review".into(),
            version: "0.3.0".into(),
            source: "gh:ta/review".into(),
            checksum: "aabbcc".into(),
        });
        lock.save(&path).unwrap();

        let loaded = Lockfile::load(&path).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.get("review").unwrap().version, "0.3.0");
    }

    #[test]
    fn lockfile_load_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.lock");
        let lock = Lockfile::load(&path).unwrap();
        assert!(lock.entries.is_empty());
    }

    // -- Checksum helpers ----------------------------------------------------

    #[test]
    fn sha256_and_verify() {
        let content = "hello, world";
        let hash = sha256_hex(content);
        assert!(verify_checksum(content, &hash));
        assert!(!verify_checksum("different", &hash));
    }

    // -- PackageManifest serde -----------------------------------------------

    #[test]
    fn package_manifest_yaml_round_trip() {
        let yaml = r#"
name: ci-review
version: "1.2.0"
author: trustedautonomy
description: CI review workflow
ta_version: ">=0.9.8"
files:
  - workflow.yaml
  - agents/reviewer.yaml
"#;
        let manifest: PackageManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.name, "ci-review");
        assert_eq!(manifest.version, "1.2.0");
        assert_eq!(manifest.author.as_deref(), Some("trustedautonomy"));
        assert_eq!(manifest.ta_version.as_deref(), Some(">=0.9.8"));
        assert_eq!(manifest.files.len(), 2);

        // re-serialize and parse again
        let reserialized = serde_yaml::to_string(&manifest).unwrap();
        let re: PackageManifest = serde_yaml::from_str(&reserialized).unwrap();
        assert_eq!(re.name, manifest.name);
    }

    // -- ExternalSource serde ------------------------------------------------

    #[test]
    fn external_source_json_serde() {
        let src = ExternalSource::GitHub {
            org: "org".into(),
            repo: "repo".into(),
            path: Some("w.yaml".into()),
            ref_: Some("v1".into()),
        };
        let json = serde_json::to_string(&src).unwrap();
        let de: ExternalSource = serde_json::from_str(&json).unwrap();
        assert_eq!(de, src);
    }
}
