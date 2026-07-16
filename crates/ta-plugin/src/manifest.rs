//! The canonical `plugin.toml` manifest schema (§2.2), a superset of the
//! fields historically split across VcsPluginManifest/MessagingPluginManifest/
//! SocialPluginManifest.

use crate::error::PluginError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

fn default_version() -> String {
    "0.1.0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub protocol_version: Option<u32>,
    #[serde(default)]
    pub min_daemon_version: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub staging_env: HashMap<String, String>,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        if !path.exists() {
            return Err(PluginError::ManifestNotFound {
                path: path.display().to_string(),
            });
        }
        let text = std::fs::read_to_string(path)?;
        let manifest: PluginManifest =
            toml::from_str(&text).map_err(|e| PluginError::InvalidManifest {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;
        // Declarative "tool"-kind manifests have nothing to execute (their
        // install/detect logic lives in a `[tool]` extension table parsed by
        // the caller), so they're exempt from the MissingCommand check.
        if manifest.command.trim().is_empty() && manifest.kind != "tool" {
            return Err(PluginError::MissingCommand {
                path: path.display().to_string(),
            });
        }
        Ok(manifest)
    }

    pub fn validate(&self, expected_kind: &str) -> Result<(), PluginError> {
        if self.kind != expected_kind {
            return Err(PluginError::InvalidManifest {
                path: self.name.clone(),
                reason: format!(
                    "expected type = \"{expected_kind}\", found \"{}\"",
                    self.kind
                ),
            });
        }
        Ok(())
    }

    pub fn timeout(&self, default_secs: u64) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.unwrap_or(default_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_minimal_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugin.toml");
        std::fs::write(
            &path,
            "name = \"demo\"\ntype = \"vcs\"\ncommand = \"demo-plugin\"\n",
        )
        .unwrap();
        let manifest = PluginManifest::load(&path).unwrap();
        assert_eq!(manifest.name, "demo");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.timeout_secs, None);
        assert!(manifest.validate("vcs").is_ok());
        assert!(manifest.validate("messaging").is_err());
    }

    #[test]
    fn missing_command_is_rejected_for_non_tool_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugin.toml");
        std::fs::write(&path, "name = \"demo\"\ntype = \"vcs\"\ncommand = \"\"\n").unwrap();
        assert!(matches!(
            PluginManifest::load(&path),
            Err(PluginError::MissingCommand { .. })
        ));
    }

    #[test]
    fn tool_kind_allows_empty_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugin.toml");
        std::fs::write(&path, "name = \"demo\"\ntype = \"tool\"\ncommand = \"\"\n").unwrap();
        assert!(PluginManifest::load(&path).is_ok());
    }

    #[test]
    fn missing_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope").join("plugin.toml");
        assert!(matches!(
            PluginManifest::load(&path),
            Err(PluginError::ManifestNotFound { .. })
        ));
    }
}
