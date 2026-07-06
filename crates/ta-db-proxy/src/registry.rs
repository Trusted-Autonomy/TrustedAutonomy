//! `.ta/db-adapters.toml` — a Resource-list-category (§2.2) registry mapping
//! a DB URI scheme to the plugin name that handles it. Declarative only; no
//! executable contract lives here (that's `DbProxyPlugin`/`external_plugin`).

use crate::error::{ProxyError, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct DbAdapterEntry {
    pub scheme: String,
    pub plugin: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DbAdapterRegistry {
    #[serde(default, rename = "adapter")]
    pub entries: Vec<DbAdapterEntry>,
}

impl DbAdapterRegistry {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text)
            .map_err(|e| ProxyError::Plugin(format!("invalid db-adapters.toml: {e}")))
    }

    pub fn resolve(&self, scheme: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.scheme == scheme)
            .map(|e| e.plugin.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_registry_file_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let registry = DbAdapterRegistry::load(&dir.path().join("db-adapters.toml")).unwrap();
        assert!(registry.entries.is_empty());
        assert_eq!(registry.resolve("sqlite"), None);
    }

    #[test]
    fn resolves_configured_scheme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db-adapters.toml");
        std::fs::write(
            &path,
            "[[adapter]]\nscheme = \"postgres\"\nplugin = \"pg-proxy\"\n",
        )
        .unwrap();
        let registry = DbAdapterRegistry::load(&path).unwrap();
        assert_eq!(registry.resolve("postgres"), Some("pg-proxy"));
        assert_eq!(registry.resolve("mysql"), None);
    }

    #[test]
    fn invalid_toml_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db-adapters.toml");
        std::fs::write(&path, "{{not valid toml}}").unwrap();
        assert!(DbAdapterRegistry::load(&path).is_err());
    }
}
