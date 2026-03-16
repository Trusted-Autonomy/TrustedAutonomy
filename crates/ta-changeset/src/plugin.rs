// plugin.rs — Channel plugin manifest and discovery for out-of-process plugins.
//
// Plugins are external executables (any language) that implement a channel
// adapter using one of two protocols:
//   1. JSON-over-stdio: TA spawns the plugin, sends ChannelQuestion on stdin,
//      reads DeliveryResult from stdout.
//   2. HTTP callback: TA POSTs ChannelQuestion to a configured URL.
//
// Plugins are discovered from:
//   - `.ta/plugins/channels/` (project-local)
//   - `~/.config/ta/plugins/channels/` (user-global)
//   - `[[channels.external]]` entries in daemon.toml (inline config)
//
// Each plugin directory contains a `channel.toml` manifest describing the
// plugin's name, protocol, command, and capabilities.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Protocol used by an external channel plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginProtocol {
    /// JSON-over-stdio: TA spawns the plugin process, writes ChannelQuestion
    /// JSON to stdin, reads DeliveryResult JSON line from stdout.
    JsonStdio,
    /// HTTP callback: TA POSTs ChannelQuestion JSON to a URL, reads
    /// DeliveryResult from the response body.
    Http,
}

impl std::fmt::Display for PluginProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginProtocol::JsonStdio => write!(f, "json-stdio"),
            PluginProtocol::Http => write!(f, "http"),
        }
    }
}

/// Parsed `channel.toml` plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Plugin name (e.g., "teams", "pagerduty", "custom-webhook").
    pub name: String,

    /// Plugin version (semver).
    #[serde(default = "default_version")]
    pub version: String,

    /// Command to spawn for json-stdio plugins.
    /// Can be a bare executable name (resolved via PATH) or a full path.
    /// Ignored for http protocol.
    #[serde(default)]
    pub command: Option<String>,

    /// Additional arguments to pass to the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Communication protocol.
    pub protocol: PluginProtocol,

    /// URL to POST questions to (only for http protocol).
    #[serde(default)]
    pub deliver_url: Option<String>,

    /// Environment variable name holding an auth token (http protocol).
    #[serde(default)]
    pub auth_token_env: Option<String>,

    /// Capabilities this plugin supports.
    #[serde(default = "default_capabilities")]
    pub capabilities: Vec<String>,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Timeout in seconds for a single delivery attempt.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// Custom build command for non-Rust plugins.
    ///
    /// Rust plugins default to `cargo build --release` when this is absent.
    /// Non-Rust plugins specify their own build step:
    ///   - Go: `"go build -o ta-channel-teams ."`
    ///   - Python: `"pip install -e ."`
    ///   - Node: `"npm run build"`
    #[serde(default)]
    pub build_command: Option<String>,

    /// Minimum daemon version required by this plugin (v0.10.16).
    /// Semver string (e.g., "0.10.0-alpha"). If set and the daemon version
    /// is lower, plugin validation warns about incompatibility.
    #[serde(default)]
    pub min_daemon_version: Option<String>,

    /// Source URL for remote install / upgrade (v0.10.16).
    /// Used by `ta plugin upgrade` to fetch the latest version.
    #[serde(default)]
    pub source_url: Option<String>,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_capabilities() -> Vec<String> {
    vec!["deliver_question".to_string()]
}

fn default_timeout_secs() -> u64 {
    30
}

/// A discovered plugin with its manifest and source path.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// Parsed manifest.
    pub manifest: PluginManifest,
    /// Directory containing the channel.toml (the plugin root).
    pub plugin_dir: PathBuf,
    /// Whether this came from project-local or user-global directory.
    pub source: PluginSource,
}

/// Where a plugin was discovered from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginSource {
    /// `.ta/plugins/channels/` in the project root.
    ProjectLocal,
    /// `~/.config/ta/plugins/channels/` (user-global).
    UserGlobal,
    /// `[[channels.external]]` in daemon.toml (inline config).
    InlineConfig,
}

impl std::fmt::Display for PluginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginSource::ProjectLocal => write!(f, "project"),
            PluginSource::UserGlobal => write!(f, "global"),
            PluginSource::InlineConfig => write!(f, "config"),
        }
    }
}

/// Errors from plugin operations.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin manifest not found: {path}")]
    ManifestNotFound { path: PathBuf },

    #[error("invalid plugin manifest at {path}: {reason}")]
    InvalidManifest { path: PathBuf, reason: String },

    #[error("plugin '{name}' requires command for json-stdio protocol")]
    MissingCommand { name: String },

    #[error("plugin '{name}' requires deliver_url for http protocol")]
    MissingDeliverUrl { name: String },

    #[error("duplicate plugin name '{name}' — found in {first} and {second}")]
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

impl PluginManifest {
    /// Load a plugin manifest from a `channel.toml` file.
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        if !path.exists() {
            return Err(PluginError::ManifestNotFound {
                path: path.to_path_buf(),
            });
        }
        let content = std::fs::read_to_string(path)?;
        let manifest: Self =
            toml::from_str(&content).map_err(|e| PluginError::InvalidManifest {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate internal consistency of the manifest.
    pub fn validate(&self) -> Result<(), PluginError> {
        match self.protocol {
            PluginProtocol::JsonStdio => {
                if self.command.is_none() {
                    return Err(PluginError::MissingCommand {
                        name: self.name.clone(),
                    });
                }
            }
            PluginProtocol::Http => {
                if self.deliver_url.is_none() {
                    return Err(PluginError::MissingDeliverUrl {
                        name: self.name.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Discover channel plugins from standard directories.
///
/// Scans both project-local and user-global plugin directories for
/// `channel.toml` manifests. Returns all successfully parsed plugins,
/// logging warnings for any that fail to parse.
pub fn discover_plugins(project_root: &Path) -> Vec<DiscoveredPlugin> {
    let mut plugins = Vec::new();

    // Project-local plugins: .ta/plugins/channels/
    let project_dir = project_root.join(".ta").join("plugins").join("channels");
    scan_plugin_dir(&project_dir, PluginSource::ProjectLocal, &mut plugins);

    // User-global plugins: ~/.config/ta/plugins/channels/
    if let Some(config_dir) = dirs_config_dir() {
        let global_dir = config_dir.join("ta").join("plugins").join("channels");
        scan_plugin_dir(&global_dir, PluginSource::UserGlobal, &mut plugins);
    }

    plugins
}

/// Scan a directory for plugin subdirectories containing `channel.toml`.
fn scan_plugin_dir(dir: &Path, source: PluginSource, plugins: &mut Vec<DiscoveredPlugin>) {
    if !dir.is_dir() {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                error = %e,
                "Failed to read plugin directory"
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("channel.toml");
        if !manifest_path.exists() {
            continue;
        }

        match PluginManifest::load(&manifest_path) {
            Ok(manifest) => {
                tracing::debug!(
                    plugin = %manifest.name,
                    protocol = %manifest.protocol,
                    source = %source,
                    "Discovered channel plugin"
                );
                plugins.push(DiscoveredPlugin {
                    manifest,
                    plugin_dir: path,
                    source: source.clone(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "Skipping invalid channel plugin"
                );
            }
        }
    }
}

/// Install a channel plugin from a source directory to the target plugin dir.
///
/// Copies the entire plugin directory (including channel.toml and any binaries)
/// to the target location.
pub fn install_plugin(
    source: &Path,
    project_root: &Path,
    global: bool,
) -> Result<DiscoveredPlugin, PluginError> {
    // Load and validate the manifest first.
    let manifest_path = source.join("channel.toml");
    let manifest = PluginManifest::load(&manifest_path)?;

    // Determine target directory.
    let target_base = if global {
        dirs_config_dir()
            .ok_or_else(|| {
                PluginError::InstallFailed("cannot determine user config directory".into())
            })?
            .join("ta")
            .join("plugins")
            .join("channels")
    } else {
        project_root.join(".ta").join("plugins").join("channels")
    };

    let target_dir = target_base.join(&manifest.name);

    // Create target directory.
    std::fs::create_dir_all(&target_dir)?;

    // Copy all files from source to target.
    copy_dir_contents(source, &target_dir)?;

    let plugin_source = if global {
        PluginSource::UserGlobal
    } else {
        PluginSource::ProjectLocal
    };

    Ok(DiscoveredPlugin {
        manifest,
        plugin_dir: target_dir,
        source: plugin_source,
    })
}

/// Recursively copy directory contents (public wrapper for plugin_resolver).
pub fn copy_dir_contents_public(src: &Path, dst: &Path) -> Result<(), PluginError> {
    copy_dir_contents(src, dst)
}

/// Recursively copy directory contents.
fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), PluginError> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Get the user's config directory (platform-appropriate).
fn dirs_config_dir() -> Option<PathBuf> {
    // Use XDG on Linux, ~/Library/Application Support on macOS, etc.
    // Simple implementation: check XDG_CONFIG_HOME, fall back to ~/.config
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg));
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_stdio_manifest() {
        let toml_str = r#"
name = "teams"
version = "0.1.0"
command = "python3 ta-channel-teams.py"
protocol = "json-stdio"
capabilities = ["deliver_question"]
description = "Microsoft Teams channel plugin"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.name, "teams");
        assert_eq!(manifest.protocol, PluginProtocol::JsonStdio);
        assert_eq!(
            manifest.command.as_deref(),
            Some("python3 ta-channel-teams.py")
        );
        assert!(manifest.deliver_url.is_none());
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn parse_http_manifest() {
        let toml_str = r#"
name = "pagerduty"
protocol = "http"
deliver_url = "https://my-service.com/ta/deliver"
auth_token_env = "TA_PAGERDUTY_TOKEN"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.name, "pagerduty");
        assert_eq!(manifest.protocol, PluginProtocol::Http);
        assert_eq!(
            manifest.deliver_url.as_deref(),
            Some("https://my-service.com/ta/deliver")
        );
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn json_stdio_requires_command() {
        let toml_str = r#"
name = "broken"
protocol = "json-stdio"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let err = manifest.validate().unwrap_err();
        assert!(err.to_string().contains("requires command"));
    }

    #[test]
    fn http_requires_deliver_url() {
        let toml_str = r#"
name = "broken"
protocol = "http"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let err = manifest.validate().unwrap_err();
        assert!(err.to_string().contains("requires deliver_url"));
    }

    #[test]
    fn default_values() {
        let toml_str = r#"
name = "minimal"
command = "my-plugin"
protocol = "json-stdio"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.capabilities, vec!["deliver_question"]);
        assert_eq!(manifest.timeout_secs, 30);
        assert!(manifest.args.is_empty());
    }

    #[test]
    fn load_manifest_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("channel.toml");
        std::fs::write(
            &manifest_path,
            r#"
name = "test-plugin"
command = "test-cmd"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let manifest = PluginManifest::load(&manifest_path).unwrap();
        assert_eq!(manifest.name, "test-plugin");
    }

    #[test]
    fn load_manifest_not_found() {
        let err = PluginManifest::load(Path::new("/nonexistent/channel.toml")).unwrap_err();
        assert!(matches!(err, PluginError::ManifestNotFound { .. }));
    }

    #[test]
    fn load_manifest_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("channel.toml");
        std::fs::write(&manifest_path, "this is not valid toml {{{").unwrap();

        let err = PluginManifest::load(&manifest_path).unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest { .. }));
    }

    #[test]
    fn discover_plugins_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        let plugins_dir = dir.path().join(".ta").join("plugins").join("channels");

        // Create two plugin directories
        let plugin1_dir = plugins_dir.join("teams");
        std::fs::create_dir_all(&plugin1_dir).unwrap();
        std::fs::write(
            plugin1_dir.join("channel.toml"),
            r#"
name = "teams"
command = "ta-channel-teams"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let plugin2_dir = plugins_dir.join("pagerduty");
        std::fs::create_dir_all(&plugin2_dir).unwrap();
        std::fs::write(
            plugin2_dir.join("channel.toml"),
            r#"
name = "pagerduty"
protocol = "http"
deliver_url = "https://example.com/deliver"
"#,
        )
        .unwrap();

        let plugins = discover_plugins(dir.path());
        assert_eq!(plugins.len(), 2);

        let names: Vec<&str> = plugins.iter().map(|p| p.manifest.name.as_str()).collect();
        assert!(names.contains(&"teams"));
        assert!(names.contains(&"pagerduty"));
    }

    #[test]
    fn discover_plugins_skips_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let plugins_dir = dir.path().join(".ta").join("plugins").join("channels");

        // Valid plugin
        let valid_dir = plugins_dir.join("good");
        std::fs::create_dir_all(&valid_dir).unwrap();
        std::fs::write(
            valid_dir.join("channel.toml"),
            r#"
name = "good"
command = "good-plugin"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        // Invalid plugin (missing required field)
        let bad_dir = plugins_dir.join("bad");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("channel.toml"), "this is broken").unwrap();

        let plugins = discover_plugins(dir.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "good");
    }

    #[test]
    fn discover_plugins_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = discover_plugins(dir.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn install_plugin_to_project() {
        let project = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();

        // Create a source plugin
        std::fs::write(
            source.path().join("channel.toml"),
            r#"
name = "my-plugin"
command = "my-plugin-cmd"
protocol = "json-stdio"
"#,
        )
        .unwrap();
        std::fs::write(source.path().join("my-plugin-cmd"), "#!/bin/bash\necho ok").unwrap();

        let result = install_plugin(source.path(), project.path(), false).unwrap();
        assert_eq!(result.manifest.name, "my-plugin");
        assert_eq!(result.source, PluginSource::ProjectLocal);

        // Verify files were copied
        let installed_manifest = project
            .path()
            .join(".ta/plugins/channels/my-plugin/channel.toml");
        assert!(installed_manifest.exists());
    }

    #[test]
    fn plugin_protocol_display() {
        assert_eq!(format!("{}", PluginProtocol::JsonStdio), "json-stdio");
        assert_eq!(format!("{}", PluginProtocol::Http), "http");
    }

    #[test]
    fn plugin_source_display() {
        assert_eq!(format!("{}", PluginSource::ProjectLocal), "project");
        assert_eq!(format!("{}", PluginSource::UserGlobal), "global");
        assert_eq!(format!("{}", PluginSource::InlineConfig), "config");
    }

    #[test]
    fn manifest_with_args() {
        let toml_str = r#"
name = "python-plugin"
command = "python3"
args = ["-u", "channel_plugin.py"]
protocol = "json-stdio"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.args, vec!["-u", "channel_plugin.py"]);
    }

    #[test]
    fn manifest_with_build_command() {
        let toml_str = r#"
name = "go-plugin"
command = "ta-channel-teams"
protocol = "json-stdio"
build_command = "go build -o ta-channel-teams ."
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(
            manifest.build_command.as_deref(),
            Some("go build -o ta-channel-teams .")
        );
    }

    #[test]
    fn manifest_without_build_command() {
        let toml_str = r#"
name = "rust-plugin"
command = "ta-channel-rust"
protocol = "json-stdio"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert!(manifest.build_command.is_none());
    }

    #[test]
    fn plugin_error_display() {
        let err = PluginError::MissingCommand {
            name: "test".into(),
        };
        assert!(err.to_string().contains("test"));
        assert!(err.to_string().contains("command"));

        let err = PluginError::DuplicateName {
            name: "dup".into(),
            first: "project".into(),
            second: "global".into(),
        };
        assert!(err.to_string().contains("dup"));
    }
}
