// registry_client.rs — Plugin registry client and platform detection.
//
// The registry is a static JSON index served over HTTP:
//   https://registry.trustedautonomy.dev/v1/index.json
//
// This module handles:
// - Fetching and caching the registry index
// - Platform detection (os + arch → registry platform key)
// - Resolving plugin download URLs from registry entries
// - GitHub release URL construction

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Default registry URL.
pub const DEFAULT_REGISTRY_URL: &str = "https://registry.trustedautonomy.dev/v1/index.json";

/// Default cache TTL in seconds (1 hour).
pub const DEFAULT_CACHE_TTL_SECS: u64 = 3600;

/// Registry index — the top-level JSON structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    /// Schema version for forward compatibility.
    pub schema_version: u32,

    /// Map of plugin name → plugin entry.
    pub plugins: HashMap<String, RegistryPluginEntry>,
}

/// A single plugin's registry entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryPluginEntry {
    /// Plugin type (e.g., "channel").
    #[serde(rename = "type")]
    pub plugin_type: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Available versions with platform-specific downloads.
    pub versions: HashMap<String, RegistryVersion>,
}

/// A specific version's release information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryVersion {
    /// Plugin protocol version.
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,

    /// Minimum TA CLI version required.
    #[serde(default)]
    pub min_ta_version: Option<String>,

    /// Platform-specific download information.
    pub platforms: HashMap<String, PlatformDownload>,
}

fn default_protocol_version() -> u32 {
    1
}

/// Download info for a specific platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformDownload {
    /// Download URL for the tarball.
    pub url: String,

    /// SHA-256 hash of the tarball for integrity verification.
    pub sha256: String,
}

/// Detect the current platform and return the registry platform key.
///
/// Returns one of:
/// - `aarch64-apple-darwin` (Apple Silicon macOS)
/// - `x86_64-apple-darwin` (Intel macOS)
/// - `x86_64-unknown-linux-musl` (Linux x86_64)
/// - `aarch64-unknown-linux-musl` (Linux ARM64)
/// - `x86_64-pc-windows-msvc` (Windows x86_64)
pub fn detect_platform() -> String {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;

    match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("linux", "x86_64") => "x86_64-unknown-linux-musl".to_string(),
        ("linux", "aarch64") => "aarch64-unknown-linux-musl".to_string(),
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_string(),
        _ => format!("{}-unknown-{}", arch, os),
    }
}

/// Registry client for fetching and caching the plugin index.
pub struct RegistryClient {
    /// Registry index URL.
    registry_url: String,
    /// Local cache directory (e.g., `~/.cache/ta/registry/`).
    cache_dir: PathBuf,
    /// Cache TTL in seconds.
    cache_ttl_secs: u64,
}

/// Errors from registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("failed to fetch registry index from {url}: {reason}")]
    FetchFailed { url: String, reason: String },

    #[error("failed to parse registry index: {0}")]
    ParseFailed(String),

    #[error("plugin '{name}' not found in registry")]
    PluginNotFound { name: String },

    #[error("plugin '{name}' version '{version}' not found in registry")]
    VersionNotFound { name: String, version: String },

    #[error("plugin '{name}' version '{version}' has no binary for platform '{platform}'")]
    PlatformNotAvailable {
        name: String,
        version: String,
        platform: String,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryClient {
    /// Create a new registry client with the default registry URL.
    pub fn new() -> Self {
        Self {
            registry_url: DEFAULT_REGISTRY_URL.to_string(),
            cache_dir: default_cache_dir(),
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
        }
    }

    /// Create a registry client with a custom URL and cache directory.
    pub fn with_config(registry_url: String, cache_dir: PathBuf, cache_ttl_secs: u64) -> Self {
        Self {
            registry_url,
            cache_dir,
            cache_ttl_secs,
        }
    }

    /// Get the cache file path for the registry index.
    fn cache_path(&self) -> PathBuf {
        self.cache_dir.join("index.json")
    }

    /// Get the timestamp file path for cache TTL tracking.
    fn cache_timestamp_path(&self) -> PathBuf {
        self.cache_dir.join("index.timestamp")
    }

    /// Check if the cached index is still valid (within TTL).
    fn is_cache_valid(&self) -> bool {
        let ts_path = self.cache_timestamp_path();
        if !ts_path.exists() || !self.cache_path().exists() {
            return false;
        }
        match std::fs::metadata(&ts_path) {
            Ok(meta) => {
                if let Ok(modified) = meta.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        return elapsed.as_secs() < self.cache_ttl_secs;
                    }
                }
                false
            }
            Err(_) => false,
        }
    }

    /// Load the registry index from cache.
    fn load_cached(&self) -> Option<RegistryIndex> {
        if !self.is_cache_valid() {
            return None;
        }
        let content = std::fs::read_to_string(self.cache_path()).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save the registry index to cache.
    fn save_cache(&self, index: &RegistryIndex) -> Result<(), RegistryError> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let json = serde_json::to_string_pretty(index)
            .map_err(|e| RegistryError::ParseFailed(e.to_string()))?;
        std::fs::write(self.cache_path(), json)?;
        std::fs::write(self.cache_timestamp_path(), "")?;
        Ok(())
    }

    /// Fetch the registry index, using cache if available.
    ///
    /// This is a blocking HTTP call. Returns the cached index if within TTL,
    /// otherwise fetches from the registry URL and updates the cache.
    pub fn fetch_index(&self) -> Result<RegistryIndex, RegistryError> {
        // Try cache first.
        if let Some(cached) = self.load_cached() {
            tracing::debug!(
                url = %self.registry_url,
                "Using cached registry index"
            );
            return Ok(cached);
        }

        // Fetch from network.
        tracing::info!(
            url = %self.registry_url,
            "Fetching plugin registry index"
        );

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| RegistryError::FetchFailed {
                url: self.registry_url.clone(),
                reason: e.to_string(),
            })?;

        let resp =
            client
                .get(&self.registry_url)
                .send()
                .map_err(|e| RegistryError::FetchFailed {
                    url: self.registry_url.clone(),
                    reason: e.to_string(),
                })?;

        if !resp.status().is_success() {
            return Err(RegistryError::FetchFailed {
                url: self.registry_url.clone(),
                reason: format!("HTTP {}", resp.status()),
            });
        }

        let body = resp.text().map_err(|e| RegistryError::FetchFailed {
            url: self.registry_url.clone(),
            reason: e.to_string(),
        })?;

        let index: RegistryIndex =
            serde_json::from_str(&body).map_err(|e| RegistryError::ParseFailed(e.to_string()))?;

        // Cache the result.
        if let Err(e) = self.save_cache(&index) {
            tracing::warn!(error = %e, "Failed to cache registry index");
        }

        Ok(index)
    }

    /// Load a registry index from a JSON string (for testing or offline use).
    pub fn parse_index(json: &str) -> Result<RegistryIndex, RegistryError> {
        serde_json::from_str(json).map_err(|e| RegistryError::ParseFailed(e.to_string()))
    }

    /// Look up a plugin in the registry and find the best matching version
    /// for the given constraint and platform.
    pub fn resolve(
        &self,
        index: &RegistryIndex,
        plugin_name: &str,
        version_constraint: &str,
        platform: &str,
    ) -> Result<ResolvedPlugin, RegistryError> {
        let entry =
            index
                .plugins
                .get(plugin_name)
                .ok_or_else(|| RegistryError::PluginNotFound {
                    name: plugin_name.to_string(),
                })?;

        // Find the latest version that satisfies the constraint.
        let min_version =
            super::project_manifest::parse_min_version(version_constraint).unwrap_or("0.0.0");

        let mut best: Option<(&str, &RegistryVersion)> = None;
        for (ver_str, ver_info) in &entry.versions {
            if super::project_manifest::compare_versions(ver_str, min_version)
                != std::cmp::Ordering::Less
            {
                match &best {
                    Some((best_ver, _)) => {
                        if super::project_manifest::compare_versions(ver_str, best_ver)
                            == std::cmp::Ordering::Greater
                        {
                            best = Some((ver_str, ver_info));
                        }
                    }
                    None => {
                        best = Some((ver_str, ver_info));
                    }
                }
            }
        }

        let (resolved_version, version_info) =
            best.ok_or_else(|| RegistryError::VersionNotFound {
                name: plugin_name.to_string(),
                version: version_constraint.to_string(),
            })?;

        let download = version_info.platforms.get(platform).ok_or_else(|| {
            RegistryError::PlatformNotAvailable {
                name: plugin_name.to_string(),
                version: resolved_version.to_string(),
                platform: platform.to_string(),
            }
        })?;

        Ok(ResolvedPlugin {
            name: plugin_name.to_string(),
            version: resolved_version.to_string(),
            download_url: download.url.clone(),
            sha256: download.sha256.clone(),
            plugin_type: entry.plugin_type.clone(),
        })
    }

    /// Construct a GitHub release download URL for a plugin.
    ///
    /// Format: `https://github.com/{owner}/{repo}/releases/download/v{version}/{name}-{version}-{platform}.tar.gz`
    pub fn github_release_url(
        repo: &str,
        plugin_name: &str,
        version: &str,
        platform: &str,
    ) -> String {
        format!(
            "https://github.com/{}/releases/download/v{}/{}-{}-{}.tar.gz",
            repo, version, plugin_name, version, platform
        )
    }
}

/// A fully resolved plugin download target.
#[derive(Debug, Clone)]
pub struct ResolvedPlugin {
    /// Plugin name.
    pub name: String,
    /// Resolved version string.
    pub version: String,
    /// Download URL.
    pub download_url: String,
    /// Expected SHA-256 hash.
    pub sha256: String,
    /// Plugin type (e.g., "channel").
    pub plugin_type: String,
}

/// Get the default cache directory for registry data.
fn default_cache_dir() -> PathBuf {
    if let Ok(cache) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("ta").join("registry");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("ta")
            .join("registry");
    }
    PathBuf::from("/tmp/ta-registry-cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_platform_returns_known_format() {
        let platform = detect_platform();
        // Should contain arch and os components.
        assert!(
            platform.contains('-'),
            "platform key should contain a dash: {}",
            platform
        );
        // Should match one of the known formats or a fallback.
        let known = [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "x86_64-pc-windows-msvc",
        ];
        // On CI, we might get different platforms, so just verify format.
        if !known.contains(&platform.as_str()) {
            assert!(
                platform.contains("unknown"),
                "fallback should contain 'unknown': {}",
                platform
            );
        }
    }

    #[test]
    fn parse_registry_index() {
        let json = r#"{
            "schema_version": 1,
            "plugins": {
                "ta-channel-discord": {
                    "type": "channel",
                    "description": "Discord channel plugin",
                    "versions": {
                        "0.1.0": {
                            "protocol_version": 1,
                            "min_ta_version": "0.11.0",
                            "platforms": {
                                "aarch64-apple-darwin": {
                                    "url": "https://example.com/discord-0.1.0-aarch64-apple-darwin.tar.gz",
                                    "sha256": "abc123"
                                },
                                "x86_64-unknown-linux-musl": {
                                    "url": "https://example.com/discord-0.1.0-linux.tar.gz",
                                    "sha256": "def456"
                                }
                            }
                        },
                        "0.2.0": {
                            "protocol_version": 1,
                            "platforms": {
                                "aarch64-apple-darwin": {
                                    "url": "https://example.com/discord-0.2.0-aarch64-apple-darwin.tar.gz",
                                    "sha256": "ghi789"
                                }
                            }
                        }
                    }
                }
            }
        }"#;

        let index = RegistryClient::parse_index(json).unwrap();
        assert_eq!(index.schema_version, 1);
        assert_eq!(index.plugins.len(), 1);
        let discord = &index.plugins["ta-channel-discord"];
        assert_eq!(discord.plugin_type, "channel");
        assert_eq!(discord.versions.len(), 2);
    }

    #[test]
    fn resolve_latest_version() {
        let json = r#"{
            "schema_version": 1,
            "plugins": {
                "test-plugin": {
                    "type": "channel",
                    "versions": {
                        "0.1.0": {
                            "platforms": {
                                "aarch64-apple-darwin": {
                                    "url": "https://example.com/v0.1.0.tar.gz",
                                    "sha256": "aaa"
                                }
                            }
                        },
                        "0.2.0": {
                            "platforms": {
                                "aarch64-apple-darwin": {
                                    "url": "https://example.com/v0.2.0.tar.gz",
                                    "sha256": "bbb"
                                }
                            }
                        },
                        "0.3.0": {
                            "platforms": {
                                "aarch64-apple-darwin": {
                                    "url": "https://example.com/v0.3.0.tar.gz",
                                    "sha256": "ccc"
                                }
                            }
                        }
                    }
                }
            }
        }"#;

        let index = RegistryClient::parse_index(json).unwrap();
        let client = RegistryClient::new();

        // Should resolve to 0.3.0 (latest satisfying >=0.1.0).
        let resolved = client
            .resolve(&index, "test-plugin", ">=0.1.0", "aarch64-apple-darwin")
            .unwrap();
        assert_eq!(resolved.version, "0.3.0");
        assert_eq!(resolved.sha256, "ccc");

        // Should resolve to 0.3.0 (latest satisfying >=0.2.0).
        let resolved = client
            .resolve(&index, "test-plugin", ">=0.2.0", "aarch64-apple-darwin")
            .unwrap();
        assert_eq!(resolved.version, "0.3.0");

        // Should resolve to 0.3.0 for exact match.
        let resolved = client
            .resolve(&index, "test-plugin", ">=0.3.0", "aarch64-apple-darwin")
            .unwrap();
        assert_eq!(resolved.version, "0.3.0");
    }

    #[test]
    fn resolve_version_not_found() {
        let json = r#"{
            "schema_version": 1,
            "plugins": {
                "test-plugin": {
                    "type": "channel",
                    "versions": {
                        "0.1.0": {
                            "platforms": {
                                "aarch64-apple-darwin": {
                                    "url": "https://example.com/v0.1.0.tar.gz",
                                    "sha256": "aaa"
                                }
                            }
                        }
                    }
                }
            }
        }"#;

        let index = RegistryClient::parse_index(json).unwrap();
        let client = RegistryClient::new();

        let err = client
            .resolve(&index, "test-plugin", ">=1.0.0", "aarch64-apple-darwin")
            .unwrap_err();
        assert!(matches!(err, RegistryError::VersionNotFound { .. }));
    }

    #[test]
    fn resolve_plugin_not_found() {
        let json = r#"{"schema_version": 1, "plugins": {}}"#;
        let index = RegistryClient::parse_index(json).unwrap();
        let client = RegistryClient::new();

        let err = client
            .resolve(&index, "nonexistent", ">=0.1.0", "aarch64-apple-darwin")
            .unwrap_err();
        assert!(matches!(err, RegistryError::PluginNotFound { .. }));
    }

    #[test]
    fn resolve_platform_not_available() {
        let json = r#"{
            "schema_version": 1,
            "plugins": {
                "test-plugin": {
                    "type": "channel",
                    "versions": {
                        "0.1.0": {
                            "platforms": {
                                "x86_64-unknown-linux-musl": {
                                    "url": "https://example.com/v0.1.0.tar.gz",
                                    "sha256": "aaa"
                                }
                            }
                        }
                    }
                }
            }
        }"#;

        let index = RegistryClient::parse_index(json).unwrap();
        let client = RegistryClient::new();

        let err = client
            .resolve(&index, "test-plugin", ">=0.1.0", "aarch64-apple-darwin")
            .unwrap_err();
        assert!(matches!(err, RegistryError::PlatformNotAvailable { .. }));
    }

    #[test]
    fn github_release_url_format() {
        let url = RegistryClient::github_release_url(
            "Trusted-Autonomy/ta-channel-discord",
            "ta-channel-discord",
            "0.1.0",
            "aarch64-apple-darwin",
        );
        assert_eq!(
            url,
            "https://github.com/Trusted-Autonomy/ta-channel-discord/releases/download/v0.1.0/ta-channel-discord-0.1.0-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn cache_validity() {
        let dir = tempfile::tempdir().unwrap();
        let client = RegistryClient::with_config(
            "https://example.com/index.json".to_string(),
            dir.path().to_path_buf(),
            3600,
        );

        // No cache yet.
        assert!(!client.is_cache_valid());

        // Create cache files.
        let index = RegistryIndex {
            schema_version: 1,
            plugins: HashMap::new(),
        };
        client.save_cache(&index).unwrap();

        // Cache should be valid now.
        assert!(client.is_cache_valid());

        // Load from cache.
        let cached = client.load_cached();
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().schema_version, 1);
    }

    #[test]
    fn cache_expired() {
        let dir = tempfile::tempdir().unwrap();
        let client = RegistryClient::with_config(
            "https://example.com/index.json".to_string(),
            dir.path().to_path_buf(),
            0, // 0 second TTL = always expired.
        );

        let index = RegistryIndex {
            schema_version: 1,
            plugins: HashMap::new(),
        };
        client.save_cache(&index).unwrap();

        // Should be expired immediately with TTL=0.
        // Note: This may pass on fast machines since the timestamp check
        // uses file modification time. Using TTL=0 ensures expiry.
        assert!(!client.is_cache_valid());
    }

    #[test]
    fn registry_error_display() {
        let err = RegistryError::PluginNotFound {
            name: "test".into(),
        };
        assert!(err.to_string().contains("test"));

        let err = RegistryError::PlatformNotAvailable {
            name: "test".into(),
            version: "0.1.0".into(),
            platform: "arm".into(),
        };
        assert!(err.to_string().contains("arm"));
    }
}
