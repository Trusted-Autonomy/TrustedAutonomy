//! Memory backend plugin manifest (`memory.toml`) and discovery.
//!
//! Memory backend plugins are external executables that implement the
//! JSON-over-stdio protocol defined in `plugin_protocol.rs`.
//!
//! ## Plugin directories (searched in order)
//!
//! 1. `.ta/plugins/memory/<name>/` — project-local
//! 2. `~/.config/ta/plugins/memory/<name>/` — user-global
//! 3. `$PATH` — bare executable `ta-memory-<name>` (no manifest required)
//!
//! ## Manifest format (`memory.toml`)
//!
//! ```toml
//! name = "supermemory"
//! version = "0.1.0"
//! command = "ta-memory-supermemory"
//! capabilities = ["semantic_search"]
//! description = "Supermemory cloud memory backend"
//! timeout_secs = 30
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// Parsed `memory.toml` manifest for a memory backend plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPluginManifest {
    /// Plugin/backend name (e.g., "supermemory", "redis").
    pub name: String,

    /// Plugin version (semver).
    #[serde(default = "default_version")]
    pub version: String,

    /// Executable command to spawn.
    ///
    /// Bare name (resolved via PATH) or an absolute path.  Required.
    pub command: String,

    /// Additional arguments passed to the command on every invocation.
    #[serde(default)]
    pub args: Vec<String>,

    /// Capabilities this plugin exposes.
    ///
    /// Standard values: `"semantic_search"`, `"ttl"`, `"phase_filter"`.
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Per-call timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_timeout_secs() -> u64 {
    30
}

impl MemoryPluginManifest {
    /// Load a manifest from a `memory.toml` file.
    pub fn load(path: &Path) -> Result<Self, MemoryPluginError> {
        if !path.exists() {
            return Err(MemoryPluginError::ManifestNotFound {
                path: path.to_path_buf(),
            });
        }
        let content = std::fs::read_to_string(path)?;
        let manifest: Self =
            toml::from_str(&content).map_err(|e| MemoryPluginError::InvalidManifest {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Build a manifest for a PATH-discovered plugin (no manifest file required).
    pub fn from_command(name: &str, command: &str) -> Self {
        Self {
            name: name.to_string(),
            version: "unknown".to_string(),
            command: command.to_string(),
            args: vec![],
            capabilities: vec![],
            description: None,
            timeout_secs: 30,
        }
    }

    /// Validate internal consistency.
    pub fn validate(&self) -> Result<(), MemoryPluginError> {
        if self.command.trim().is_empty() {
            return Err(MemoryPluginError::MissingCommand {
                name: self.name.clone(),
            });
        }
        Ok(())
    }

    /// Whether this plugin declares semantic search capability.
    pub fn has_semantic_search(&self) -> bool {
        self.capabilities.iter().any(|c| c == "semantic_search")
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from memory plugin manifest operations.
#[derive(Debug, thiserror::Error)]
pub enum MemoryPluginError {
    #[error("plugin manifest not found: {path}")]
    ManifestNotFound { path: PathBuf },

    #[error("invalid plugin manifest at {path}: {reason}")]
    InvalidManifest { path: PathBuf, reason: String },

    #[error("plugin '{name}' requires 'command' field")]
    MissingCommand { name: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Discovery source
// ---------------------------------------------------------------------------

/// Where a plugin was discovered from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryPluginSource {
    /// `.ta/plugins/memory/` in the project root.
    ProjectLocal,
    /// `~/.config/ta/plugins/memory/` (user-global).
    UserGlobal,
    /// Bare executable on `$PATH` (no manifest directory).
    Path,
}

impl std::fmt::Display for MemoryPluginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryPluginSource::ProjectLocal => write!(f, "project"),
            MemoryPluginSource::UserGlobal => write!(f, "global"),
            MemoryPluginSource::Path => write!(f, "PATH"),
        }
    }
}

/// A discovered memory plugin with its manifest and origin.
#[derive(Debug, Clone)]
pub struct DiscoveredMemoryPlugin {
    /// Parsed manifest.
    pub manifest: MemoryPluginManifest,
    /// Directory containing `memory.toml` (None for PATH-discovered plugins).
    pub plugin_dir: Option<PathBuf>,
    /// Discovery source.
    pub source: MemoryPluginSource,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Find a specific memory plugin by name for the given project root.
///
/// Search order:
/// 1. `.ta/plugins/memory/<name>/memory.toml` — project-local
/// 2. `~/.config/ta/plugins/memory/<name>/memory.toml` — user-global
/// 3. `ta-memory-<name>` on `$PATH` — bare binary
///
/// Returns `None` if no plugin is found.
pub fn find_memory_plugin(name: &str, project_root: &Path) -> Option<DiscoveredMemoryPlugin> {
    // 1. Project-local.
    let project_dir = project_root
        .join(".ta")
        .join("plugins")
        .join("memory")
        .join(name);
    if let Some(p) = try_load_from_dir(&project_dir, MemoryPluginSource::ProjectLocal) {
        return Some(p);
    }

    // 2. User-global.
    if let Some(config_dir) = dirs_config_dir() {
        let global_dir = config_dir
            .join("ta")
            .join("plugins")
            .join("memory")
            .join(name);
        if let Some(p) = try_load_from_dir(&global_dir, MemoryPluginSource::UserGlobal) {
            return Some(p);
        }
    }

    // 3. PATH: look for `ta-memory-<name>`.
    let bin_name = format!("ta-memory-{}", name);
    if which::which(&bin_name).is_ok() {
        return Some(DiscoveredMemoryPlugin {
            manifest: MemoryPluginManifest::from_command(name, &bin_name),
            plugin_dir: None,
            source: MemoryPluginSource::Path,
        });
    }

    None
}

/// Discover all installed memory plugins for the given project root.
///
/// Returns all discovered plugins in discovery order (project → global → PATH).
pub fn discover_all_memory_plugins(project_root: &Path) -> Vec<DiscoveredMemoryPlugin> {
    let mut found: Vec<DiscoveredMemoryPlugin> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Project-local.
    let project_plugins_dir = project_root.join(".ta").join("plugins").join("memory");
    collect_from_dir(
        &project_plugins_dir,
        MemoryPluginSource::ProjectLocal,
        &mut found,
        &mut seen_names,
    );

    // 2. User-global.
    if let Some(config_dir) = dirs_config_dir() {
        let global_plugins_dir = config_dir.join("ta").join("plugins").join("memory");
        collect_from_dir(
            &global_plugins_dir,
            MemoryPluginSource::UserGlobal,
            &mut found,
            &mut seen_names,
        );
    }

    // 3. PATH: scan for `ta-memory-*` binaries.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir_str in path_var.split(':') {
            let dir = Path::new(dir_str);
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if let Some(plugin_name) = name_str.strip_prefix("ta-memory-") {
                        if !plugin_name.is_empty() && !seen_names.contains(plugin_name) {
                            seen_names.insert(plugin_name.to_string());
                            found.push(DiscoveredMemoryPlugin {
                                manifest: MemoryPluginManifest::from_command(
                                    plugin_name,
                                    &name_str,
                                ),
                                plugin_dir: None,
                                source: MemoryPluginSource::Path,
                            });
                        }
                    }
                }
            }
        }
    }

    found
}

fn try_load_from_dir(dir: &Path, source: MemoryPluginSource) -> Option<DiscoveredMemoryPlugin> {
    let manifest_path = dir.join("memory.toml");
    if manifest_path.exists() {
        match MemoryPluginManifest::load(&manifest_path) {
            Ok(manifest) => {
                tracing::debug!(
                    plugin = %manifest.name,
                    path = %manifest_path.display(),
                    "Discovered memory plugin"
                );
                return Some(DiscoveredMemoryPlugin {
                    manifest,
                    plugin_dir: Some(dir.to_path_buf()),
                    source,
                });
            }
            Err(e) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "Failed to load memory plugin manifest — skipping"
                );
            }
        }
    }
    None
}

fn collect_from_dir(
    plugins_dir: &Path,
    source: MemoryPluginSource,
    found: &mut Vec<DiscoveredMemoryPlugin>,
    seen_names: &mut std::collections::HashSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            if let Some(p) = try_load_from_dir(&entry.path(), source.clone()) {
                if !seen_names.contains(&p.manifest.name) {
                    seen_names.insert(p.manifest.name.clone());
                    found.push(p);
                }
            }
        }
    }
}

/// Platform-appropriate config directory for user-global plugins.
fn dirs_config_dir() -> Option<PathBuf> {
    // Use $XDG_CONFIG_HOME on Linux, ~/Library/Application Support on macOS,
    // %APPDATA% on Windows. Fall back to ~/.config on unknown platforms.
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .or_else(|| std::env::var("APPDATA").ok().map(PathBuf::from))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn manifest_load_valid() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("memory.toml");
        std::fs::write(
            &manifest_path,
            r#"
name = "test-backend"
command = "ta-memory-test"
capabilities = ["semantic_search"]
timeout_secs = 10
"#,
        )
        .unwrap();
        let m = MemoryPluginManifest::load(&manifest_path).unwrap();
        assert_eq!(m.name, "test-backend");
        assert_eq!(m.command, "ta-memory-test");
        assert_eq!(m.timeout_secs, 10);
        assert!(m.has_semantic_search());
    }

    #[test]
    fn manifest_load_missing_returns_error() {
        let dir = tempdir().unwrap();
        let err = MemoryPluginManifest::load(&dir.path().join("nonexistent.toml")).unwrap_err();
        assert!(matches!(err, MemoryPluginError::ManifestNotFound { .. }));
    }

    #[test]
    fn manifest_validate_empty_command() {
        let m = MemoryPluginManifest {
            name: "bad".to_string(),
            version: "0.1.0".to_string(),
            command: "  ".to_string(),
            args: vec![],
            capabilities: vec![],
            description: None,
            timeout_secs: 30,
        };
        assert!(matches!(
            m.validate(),
            Err(MemoryPluginError::MissingCommand { .. })
        ));
    }

    #[test]
    fn from_command_builds_minimal_manifest() {
        let m = MemoryPluginManifest::from_command("redis", "ta-memory-redis");
        assert_eq!(m.name, "redis");
        assert_eq!(m.command, "ta-memory-redis");
        assert!(!m.has_semantic_search());
    }

    #[test]
    fn find_memory_plugin_project_local() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir
            .path()
            .join(".ta")
            .join("plugins")
            .join("memory")
            .join("test-local");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("memory.toml"),
            r#"
name = "test-local"
command = "ta-memory-test-local"
"#,
        )
        .unwrap();

        let p = find_memory_plugin("test-local", dir.path());
        assert!(p.is_some());
        let plugin = p.unwrap();
        assert_eq!(plugin.manifest.name, "test-local");
        assert_eq!(plugin.source, MemoryPluginSource::ProjectLocal);
    }

    #[test]
    fn find_memory_plugin_not_found_returns_none() {
        let dir = tempdir().unwrap();
        let result = find_memory_plugin("nonexistent-backend-xyz", dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn discover_all_finds_project_plugins() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir
            .path()
            .join(".ta")
            .join("plugins")
            .join("memory")
            .join("alpha");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("memory.toml"),
            r#"name = "alpha"
command = "ta-memory-alpha"
"#,
        )
        .unwrap();

        let plugins = discover_all_memory_plugins(dir.path());
        assert!(plugins.iter().any(|p| p.manifest.name == "alpha"));
    }
}
