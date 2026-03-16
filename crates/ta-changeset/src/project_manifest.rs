// project_manifest.rs — Project manifest (.ta/project.toml) schema and parser.
//
// A project manifest declares the project's plugin requirements so that
// `ta setup` can resolve, download, and install everything needed.
//
// Schema:
// ```toml
// [project]
// name = "my-project"
// description = "My TA-managed project"
//
// [plugins.discord]
// type = "channel"
// version = ">=0.1.0"
// source = "registry:ta-channel-discord"
// env_vars = ["DISCORD_BOT_TOKEN"]
//
// [plugins.custom-webhook]
// type = "channel"
// version = ">=0.2.0"
// source = "path:./plugins/custom-webhook"
// required = false
// ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level project manifest parsed from `.ta/project.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    /// Project metadata.
    pub project: ProjectMeta,

    /// Plugin declarations keyed by plugin name.
    #[serde(default)]
    pub plugins: HashMap<String, PluginRequirement>,
}

/// Project metadata section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    /// Human-readable project name.
    pub name: String,

    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,

    /// VCS adapter to use (e.g., "git"). Defaults to auto-detection.
    #[serde(default)]
    pub vcs_adapter: Option<String>,
}

/// A single plugin requirement declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequirement {
    /// Plugin type (e.g., "channel", "submit", "build").
    #[serde(rename = "type")]
    pub plugin_type: String,

    /// Version constraint. Phase 1: `">=X.Y.Z"` (minimum version).
    /// Phase 2 (future): full semver ranges like `">=0.1.0, <1.0.0"`.
    #[serde(default = "default_version_constraint")]
    pub version: String,

    /// Where to get the plugin. Supported schemes:
    /// - `registry:<name>` — download from the TA plugin registry
    /// - `github:<owner/repo>` — download from GitHub releases
    /// - `path:<local-path>` — build from local source
    /// - `url:<download-url>` — direct tarball URL
    pub source: String,

    /// Whether this plugin is required for the project to function.
    /// Default: true. Optional plugins warn but don't block.
    #[serde(default = "default_required")]
    pub required: bool,

    /// Environment variables this plugin needs (e.g., API tokens).
    /// `ta setup` checks these and prints instructions for missing ones.
    #[serde(default)]
    pub env_vars: Vec<String>,
}

fn default_version_constraint() -> String {
    ">=0.1.0".to_string()
}

fn default_required() -> bool {
    true
}

/// Source scheme parsed from the `source` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceScheme {
    /// `registry:<plugin-name>` — fetch from the TA plugin registry.
    Registry(String),
    /// `github:<owner/repo>` — download from GitHub releases.
    GitHub(String),
    /// `path:<local-path>` — build from local source.
    Path(PathBuf),
    /// `url:<download-url>` — direct tarball download.
    Url(String),
}

/// Errors from manifest operations.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("project manifest not found: {path}")]
    NotFound { path: PathBuf },

    #[error("invalid project manifest at {path}: {reason}")]
    Invalid { path: PathBuf, reason: String },

    #[error("plugin '{name}': invalid source scheme '{scheme}'. Expected registry:, github:, path:, or url:")]
    InvalidSource { name: String, scheme: String },

    #[error("plugin '{name}': version constraint '{version}' is not valid. Use '>=X.Y.Z' format.")]
    InvalidVersion { name: String, version: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl ProjectManifest {
    /// Load a project manifest from `.ta/project.toml` under the given root.
    pub fn load(project_root: &Path) -> Result<Self, ManifestError> {
        let path = project_root.join(".ta").join("project.toml");
        Self::load_from(&path)
    }

    /// Load from an explicit path.
    pub fn load_from(path: &Path) -> Result<Self, ManifestError> {
        if !path.exists() {
            return Err(ManifestError::NotFound {
                path: path.to_path_buf(),
            });
        }
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content).map_err(|e| ManifestError::Invalid {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Check if a project manifest exists for the given root.
    pub fn exists(project_root: &Path) -> bool {
        project_root.join(".ta").join("project.toml").exists()
    }

    /// Validate all plugin declarations.
    pub fn validate(&self) -> Result<(), ManifestError> {
        for (name, req) in &self.plugins {
            // Validate source scheme.
            parse_source_scheme(name, &req.source)?;

            // Validate version constraint (Phase 1: must start with >=).
            if !req.version.starts_with(">=") {
                return Err(ManifestError::InvalidVersion {
                    name: name.clone(),
                    version: req.version.clone(),
                });
            }
            // Extract the version part after ">=" and check it looks like semver.
            let ver = req.version.trim_start_matches(">=").trim();
            if ver.is_empty() || !ver.chars().next().unwrap_or('x').is_ascii_digit() {
                return Err(ManifestError::InvalidVersion {
                    name: name.clone(),
                    version: req.version.clone(),
                });
            }
        }
        Ok(())
    }

    /// Return all required plugin names.
    pub fn required_plugins(&self) -> Vec<&str> {
        self.plugins
            .iter()
            .filter(|(_, req)| req.required)
            .map(|(name, _)| name.as_str())
            .collect()
    }
}

/// Parse a source string into a SourceScheme.
pub fn parse_source_scheme(plugin_name: &str, source: &str) -> Result<SourceScheme, ManifestError> {
    if let Some(name) = source.strip_prefix("registry:") {
        Ok(SourceScheme::Registry(name.to_string()))
    } else if let Some(repo) = source.strip_prefix("github:") {
        Ok(SourceScheme::GitHub(repo.to_string()))
    } else if let Some(path) = source.strip_prefix("path:") {
        Ok(SourceScheme::Path(PathBuf::from(path)))
    } else if let Some(url) = source.strip_prefix("url:") {
        Ok(SourceScheme::Url(url.to_string()))
    } else {
        Err(ManifestError::InvalidSource {
            name: plugin_name.to_string(),
            scheme: source.to_string(),
        })
    }
}

/// Parse a `>=X.Y.Z` version constraint and return the minimum version string.
pub fn parse_min_version(constraint: &str) -> Option<&str> {
    constraint.strip_prefix(">=").map(|v| v.trim())
}

/// Compare two semver version strings. Returns Ordering.
/// Simple semver comparison: split on '.', compare numerically.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        // Strip pre-release suffix for comparison (e.g., "0.1.0-alpha" → "0.1.0").
        let base = s.split('-').next().unwrap_or(s);
        base.split('.')
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    };
    let a_parts = parse(a);
    let b_parts = parse(b);

    for i in 0..a_parts.len().max(b_parts.len()) {
        let a_val = a_parts.get(i).copied().unwrap_or(0);
        let b_val = b_parts.get(i).copied().unwrap_or(0);
        match a_val.cmp(&b_val) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Check if `installed_version` satisfies the constraint (e.g., `>=0.1.0`).
pub fn version_satisfies(installed: &str, constraint: &str) -> bool {
    match parse_min_version(constraint) {
        Some(min) => compare_versions(installed, min) != std::cmp::Ordering::Less,
        None => false, // Invalid constraint format.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_manifest() {
        let toml_str = r#"
[project]
name = "test-project"

[plugins.discord]
type = "channel"
source = "registry:ta-channel-discord"
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.project.name, "test-project");
        assert_eq!(manifest.plugins.len(), 1);
        let discord = &manifest.plugins["discord"];
        assert_eq!(discord.plugin_type, "channel");
        assert_eq!(discord.version, ">=0.1.0"); // default
        assert!(discord.required); // default
        assert!(discord.env_vars.is_empty());
    }

    #[test]
    fn parse_full_manifest() {
        let toml_str = r#"
[project]
name = "my-project"
description = "A project with plugins"
vcs_adapter = "git"

[plugins.discord]
type = "channel"
version = ">=0.2.0"
source = "registry:ta-channel-discord"
env_vars = ["DISCORD_BOT_TOKEN", "DISCORD_CHANNEL_ID"]

[plugins.custom]
type = "channel"
version = ">=0.1.0"
source = "path:./plugins/custom"
required = false
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.project.name, "my-project");
        assert_eq!(
            manifest.project.description.as_deref(),
            Some("A project with plugins")
        );
        assert_eq!(manifest.project.vcs_adapter.as_deref(), Some("git"));
        assert_eq!(manifest.plugins.len(), 2);

        let discord = &manifest.plugins["discord"];
        assert_eq!(discord.version, ">=0.2.0");
        assert_eq!(
            discord.env_vars,
            vec!["DISCORD_BOT_TOKEN", "DISCORD_CHANNEL_ID"]
        );
        assert!(discord.required);

        let custom = &manifest.plugins["custom"];
        assert!(!custom.required);
    }

    #[test]
    fn parse_source_schemes() {
        assert_eq!(
            parse_source_scheme("p", "registry:ta-channel-discord").unwrap(),
            SourceScheme::Registry("ta-channel-discord".to_string())
        );
        assert_eq!(
            parse_source_scheme("p", "github:Trusted-Autonomy/ta-channel-discord").unwrap(),
            SourceScheme::GitHub("Trusted-Autonomy/ta-channel-discord".to_string())
        );
        assert_eq!(
            parse_source_scheme("p", "path:./plugins/custom").unwrap(),
            SourceScheme::Path(PathBuf::from("./plugins/custom"))
        );
        assert_eq!(
            parse_source_scheme("p", "url:https://example.com/plugin.tar.gz").unwrap(),
            SourceScheme::Url("https://example.com/plugin.tar.gz".to_string())
        );
    }

    #[test]
    fn invalid_source_scheme() {
        let err = parse_source_scheme("test", "ftp:something").unwrap_err();
        assert!(err.to_string().contains("invalid source scheme"));
    }

    #[test]
    fn validate_rejects_bad_version() {
        let toml_str = r#"
[project]
name = "test"

[plugins.bad]
type = "channel"
version = "0.1.0"
source = "registry:test"
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        let err = manifest.validate().unwrap_err();
        assert!(err.to_string().contains("not valid"));
    }

    #[test]
    fn validate_accepts_good_version() {
        let toml_str = r#"
[project]
name = "test"

[plugins.good]
type = "channel"
version = ">=0.1.0"
source = "registry:test"
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("project.toml"),
            r#"
[project]
name = "file-test"

[plugins.slack]
type = "channel"
version = ">=0.1.0"
source = "registry:ta-channel-slack"
"#,
        )
        .unwrap();

        let manifest = ProjectManifest::load(dir.path()).unwrap();
        assert_eq!(manifest.project.name, "file-test");
        assert!(manifest.plugins.contains_key("slack"));
    }

    #[test]
    fn load_not_found() {
        let err = ProjectManifest::load(Path::new("/nonexistent")).unwrap_err();
        assert!(matches!(err, ManifestError::NotFound { .. }));
    }

    #[test]
    fn load_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("project.toml"), "this is not valid {{{").unwrap();

        let err = ProjectManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid { .. }));
    }

    #[test]
    fn exists_check() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!ProjectManifest::exists(dir.path()));

        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("project.toml"), "[project]\nname = \"x\"\n").unwrap();
        assert!(ProjectManifest::exists(dir.path()));
    }

    #[test]
    fn required_plugins_filter() {
        let toml_str = r#"
[project]
name = "test"

[plugins.required1]
type = "channel"
source = "registry:a"

[plugins.optional1]
type = "channel"
source = "registry:b"
required = false
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        let required = manifest.required_plugins();
        assert_eq!(required.len(), 1);
        assert!(required.contains(&"required1"));
    }

    #[test]
    fn version_comparison() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.2.0", "0.1.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0", "0.2.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0-alpha", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn version_satisfies_check() {
        assert!(version_satisfies("0.2.0", ">=0.1.0"));
        assert!(version_satisfies("0.1.0", ">=0.1.0"));
        assert!(!version_satisfies("0.0.9", ">=0.1.0"));
        assert!(version_satisfies("1.0.0", ">=0.1.0"));
    }

    #[test]
    fn manifest_no_plugins() {
        let toml_str = r#"
[project]
name = "bare"
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        assert!(manifest.plugins.is_empty());
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn manifest_error_display() {
        let err = ManifestError::NotFound {
            path: PathBuf::from("/some/path"),
        };
        assert!(err.to_string().contains("/some/path"));

        let err = ManifestError::InvalidSource {
            name: "test".into(),
            scheme: "ftp:x".into(),
        };
        assert!(err.to_string().contains("ftp:x"));
    }
}
