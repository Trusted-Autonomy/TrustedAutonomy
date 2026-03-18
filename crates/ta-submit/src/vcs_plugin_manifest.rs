//! VCS adapter plugin manifest (`plugin.toml`) and discovery.
//!
//! VCS adapter plugins are external executables that implement the
//! [`VcsPluginRequest`] / [`VcsPluginResponse`] JSON-over-stdio protocol.
//!
//! ## Plugin directories (searched in order)
//!
//! 1. `.ta/plugins/vcs/<name>/` — project-local
//! 2. `~/.config/ta/plugins/vcs/<name>/` — user-global
//! 3. `$PATH` — bare executable `ta-submit-<name>` (no manifest required)
//!
//! ## Manifest format (`plugin.toml`)
//!
//! ```toml
//! name = "perforce"
//! version = "0.1.0"
//! type = "vcs"
//! command = "ta-submit-perforce"
//! protocol = "json-stdio"
//! capabilities = ["commit", "push", "review", "protected_targets"]
//! description = "Perforce / Helix Core VCS adapter"
//! timeout_secs = 30
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// Parsed `plugin.toml` manifest for a VCS adapter plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcsPluginManifest {
    /// Adapter name (e.g., "perforce", "svn").  Must be unique.
    pub name: String,

    /// Plugin version (semver).
    #[serde(default = "default_version")]
    pub version: String,

    /// Plugin type — must be `"vcs"` for VCS plugins.
    #[serde(rename = "type", default = "default_type")]
    pub plugin_type: String,

    /// Executable command to spawn for json-stdio protocol.
    ///
    /// Bare name (resolved via PATH) or an absolute path.  Required.
    pub command: String,

    /// Additional arguments passed to the command on every invocation.
    #[serde(default)]
    pub args: Vec<String>,

    /// Capabilities this plugin exposes.
    ///
    /// Standard values: `"commit"`, `"push"`, `"review"`, `"sync"`,
    /// `"save_state"`, `"check_review"`, `"merge_review"`, `"protected_targets"`.
    ///
    /// Plugins claiming `"protected_targets"` signal §15 compliance.
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Per-call timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// Minimum TA daemon version required by this plugin.
    #[serde(default)]
    pub min_daemon_version: Option<String>,

    /// Source URL for remote install / upgrade.
    #[serde(default)]
    pub source_url: Option<String>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_type() -> String {
    "vcs".to_string()
}

fn default_timeout_secs() -> u64 {
    30
}

impl VcsPluginManifest {
    /// Load a manifest from a `plugin.toml` file.
    pub fn load(path: &Path) -> Result<Self, VcsPluginError> {
        if !path.exists() {
            return Err(VcsPluginError::ManifestNotFound {
                path: path.to_path_buf(),
            });
        }
        let content = std::fs::read_to_string(path)?;
        let manifest: Self =
            toml::from_str(&content).map_err(|e| VcsPluginError::InvalidManifest {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate internal consistency.
    pub fn validate(&self) -> Result<(), VcsPluginError> {
        if self.plugin_type != "vcs" {
            return Err(VcsPluginError::InvalidManifest {
                path: PathBuf::from("<inline>"),
                reason: format!("expected type = \"vcs\", got \"{}\"", self.plugin_type),
            });
        }
        if self.command.trim().is_empty() {
            return Err(VcsPluginError::MissingCommand {
                name: self.name.clone(),
            });
        }
        Ok(())
    }

    /// Whether this plugin declares §15 protected-targets support.
    pub fn has_protected_targets(&self) -> bool {
        self.capabilities.iter().any(|c| c == "protected_targets")
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from VCS plugin operations.
#[derive(Debug, thiserror::Error)]
pub enum VcsPluginError {
    #[error("plugin manifest not found: {path}")]
    ManifestNotFound { path: PathBuf },

    #[error("invalid plugin manifest at {path}: {reason}")]
    InvalidManifest { path: PathBuf, reason: String },

    #[error("plugin '{name}' requires 'command' field")]
    MissingCommand { name: String },

    #[error("duplicate VCS plugin name '{name}' — found in {first} and {second}")]
    DuplicateName {
        name: String,
        first: String,
        second: String,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("plugin install failed: {0}")]
    InstallFailed(String),
}

// ---------------------------------------------------------------------------
// Discovered plugin
// ---------------------------------------------------------------------------

/// Where a plugin was discovered from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VcsPluginSource {
    /// `.ta/plugins/vcs/` in the project root.
    ProjectLocal,
    /// `~/.config/ta/plugins/vcs/` (user-global).
    UserGlobal,
    /// Bare executable on `$PATH` (no manifest directory).
    Path,
}

impl std::fmt::Display for VcsPluginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VcsPluginSource::ProjectLocal => write!(f, "project"),
            VcsPluginSource::UserGlobal => write!(f, "global"),
            VcsPluginSource::Path => write!(f, "PATH"),
        }
    }
}

/// A discovered VCS plugin with its manifest and origin.
#[derive(Debug, Clone)]
pub struct DiscoveredVcsPlugin {
    /// Parsed manifest.
    pub manifest: VcsPluginManifest,
    /// Directory containing `plugin.toml` (None for PATH-discovered plugins).
    pub plugin_dir: Option<PathBuf>,
    /// Discovery source.
    pub source: VcsPluginSource,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Discover all VCS adapter plugins for the given project root.
///
/// Resolution order:
/// 1. `.ta/plugins/vcs/` — project-local (highest priority)
/// 2. `~/.config/ta/plugins/vcs/` — user-global
///
/// PATH discovery (`ta-submit-<name>`) is performed on-demand in the registry
/// (see `crate::registry`) when a named adapter is not found in the above dirs.
pub fn discover_vcs_plugins(project_root: &Path) -> Vec<DiscoveredVcsPlugin> {
    let mut plugins = Vec::new();

    // 1. Project-local
    let project_dir = project_root.join(".ta").join("plugins").join("vcs");
    scan_vcs_plugin_dir(&project_dir, VcsPluginSource::ProjectLocal, &mut plugins);

    // 2. User-global
    if let Some(config_dir) = user_config_dir() {
        let global_dir = config_dir.join("ta").join("plugins").join("vcs");
        scan_vcs_plugin_dir(&global_dir, VcsPluginSource::UserGlobal, &mut plugins);
    }

    plugins
}

/// Scan a directory for VCS plugin subdirectories containing `plugin.toml`.
fn scan_vcs_plugin_dir(dir: &Path, source: VcsPluginSource, out: &mut Vec<DiscoveredVcsPlugin>) {
    if !dir.is_dir() {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                error = %e,
                "Failed to read VCS plugin directory"
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("plugin.toml");
        if !manifest_path.exists() {
            continue;
        }

        match VcsPluginManifest::load(&manifest_path) {
            Ok(manifest) => {
                tracing::debug!(
                    plugin = %manifest.name,
                    source = %source,
                    "Discovered VCS plugin"
                );
                out.push(DiscoveredVcsPlugin {
                    manifest,
                    plugin_dir: Some(path),
                    source: source.clone(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "Skipping invalid VCS plugin manifest"
                );
            }
        }
    }
}

/// Find a VCS plugin by adapter name, searching project-local then user-global.
///
/// If no manifest-based plugin is found, synthesizes a minimal manifest for a
/// bare `ta-submit-<name>` executable on `$PATH`.
pub fn find_vcs_plugin(adapter_name: &str, project_root: &Path) -> Option<DiscoveredVcsPlugin> {
    // Search manifest-based plugins.
    let all = discover_vcs_plugins(project_root);
    if let Some(p) = all.into_iter().find(|p| p.manifest.name == adapter_name) {
        return Some(p);
    }

    // Fall back to bare PATH executable: `ta-submit-<name>`.
    let bare_cmd = format!("ta-submit-{}", adapter_name);
    if which_on_path(&bare_cmd) {
        tracing::info!(
            adapter = %adapter_name,
            command = %bare_cmd,
            "Found VCS plugin as bare executable on PATH"
        );
        return Some(DiscoveredVcsPlugin {
            manifest: VcsPluginManifest {
                name: adapter_name.to_string(),
                version: "unknown".to_string(),
                plugin_type: "vcs".to_string(),
                command: bare_cmd,
                args: vec![],
                capabilities: vec![],
                description: None,
                timeout_secs: 30,
                min_daemon_version: None,
                source_url: None,
            },
            plugin_dir: None,
            source: VcsPluginSource::Path,
        });
    }

    None
}

/// Check whether a binary exists on PATH.
fn which_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path_var| std::env::split_paths(&path_var).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Get the user's config directory.
fn user_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg));
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join("plugin.toml"), content).unwrap();
    }

    #[test]
    fn load_valid_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            r#"
name = "perforce"
version = "0.1.0"
type = "vcs"
command = "ta-submit-perforce"
protocol = "json-stdio"
capabilities = ["commit", "push", "protected_targets"]
description = "Perforce adapter"
"#,
        );
        let manifest = VcsPluginManifest::load(&dir.path().join("plugin.toml")).unwrap();
        assert_eq!(manifest.name, "perforce");
        assert_eq!(manifest.version, "0.1.0");
        assert!(manifest.has_protected_targets());
    }

    #[test]
    fn load_manifest_missing() {
        let err = VcsPluginManifest::load(Path::new("/nonexistent/plugin.toml")).unwrap_err();
        assert!(matches!(err, VcsPluginError::ManifestNotFound { .. }));
    }

    #[test]
    fn validate_wrong_type() {
        let manifest = VcsPluginManifest {
            name: "bad".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "channel".to_string(),
            command: "some-cmd".to_string(),
            args: vec![],
            capabilities: vec![],
            description: None,
            timeout_secs: 30,
            min_daemon_version: None,
            source_url: None,
        };
        let err = manifest.validate().unwrap_err();
        assert!(err.to_string().contains("vcs"));
    }

    #[test]
    fn validate_empty_command() {
        let manifest = VcsPluginManifest {
            name: "bad".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "vcs".to_string(),
            command: "   ".to_string(),
            args: vec![],
            capabilities: vec![],
            description: None,
            timeout_secs: 30,
            min_daemon_version: None,
            source_url: None,
        };
        let err = manifest.validate().unwrap_err();
        assert!(matches!(err, VcsPluginError::MissingCommand { .. }));
    }

    #[test]
    fn has_protected_targets_true() {
        let manifest = VcsPluginManifest {
            name: "p4".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "vcs".to_string(),
            command: "ta-submit-perforce".to_string(),
            args: vec![],
            capabilities: vec!["commit".to_string(), "protected_targets".to_string()],
            description: None,
            timeout_secs: 30,
            min_daemon_version: None,
            source_url: None,
        };
        assert!(manifest.has_protected_targets());
    }

    #[test]
    fn has_protected_targets_false() {
        let manifest = VcsPluginManifest {
            name: "custom".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "vcs".to_string(),
            command: "ta-submit-custom".to_string(),
            args: vec![],
            capabilities: vec!["commit".to_string()],
            description: None,
            timeout_secs: 30,
            min_daemon_version: None,
            source_url: None,
        };
        assert!(!manifest.has_protected_targets());
    }

    #[test]
    fn discover_vcs_plugins_finds_manifests() {
        let root = tempfile::tempdir().unwrap();
        let vcs_dir = root.path().join(".ta").join("plugins").join("vcs");

        // Create a valid plugin
        let p4_dir = vcs_dir.join("perforce");
        std::fs::create_dir_all(&p4_dir).unwrap();
        write_manifest(
            &p4_dir,
            r#"
name = "perforce"
type = "vcs"
command = "ta-submit-perforce"
capabilities = ["commit", "protected_targets"]
"#,
        );

        let plugins = discover_vcs_plugins(root.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "perforce");
        assert_eq!(plugins[0].source, VcsPluginSource::ProjectLocal);
    }

    #[test]
    fn discover_vcs_plugins_skips_invalid() {
        let root = tempfile::tempdir().unwrap();
        let vcs_dir = root.path().join(".ta").join("plugins").join("vcs");

        // Valid
        let good_dir = vcs_dir.join("good");
        std::fs::create_dir_all(&good_dir).unwrap();
        write_manifest(
            &good_dir,
            r#"name = "good"
type = "vcs"
command = "ta-submit-good"
"#,
        );

        // Invalid (bad TOML)
        let bad_dir = vcs_dir.join("bad");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("plugin.toml"), "{{not valid toml}}").unwrap();

        let plugins = discover_vcs_plugins(root.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "good");
    }

    #[test]
    fn discover_vcs_plugins_empty_returns_empty() {
        let root = tempfile::tempdir().unwrap();
        let plugins = discover_vcs_plugins(root.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn vcs_plugin_source_display() {
        assert_eq!(format!("{}", VcsPluginSource::ProjectLocal), "project");
        assert_eq!(format!("{}", VcsPluginSource::UserGlobal), "global");
        assert_eq!(format!("{}", VcsPluginSource::Path), "PATH");
    }

    #[test]
    fn default_timeout_is_30() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            r#"name = "minimal"
type = "vcs"
command = "ta-submit-minimal"
"#,
        );
        let manifest = VcsPluginManifest::load(&dir.path().join("plugin.toml")).unwrap();
        assert_eq!(manifest.timeout_secs, 30);
    }
}
