//! `.ta/db-adapters.toml` — a Resource-list-category (§2.2) registry mapping
//! a DB URI scheme to the plugin name that handles it. Declarative only; no
//! executable contract lives here (that's `DbProxyPlugin`/`external_plugin`).
//!
//! TA core has no built-in awareness of which specific database engines
//! exist: `db://postgres/*`, `db://mysql/*`, and `db://sqlite/*` resolve to
//! the `postgres`/`mysql`/`sqlite` plugins only because those are the
//! *default* entries below (bundled with TA, discovered like any other
//! `.ta/plugins/db/<name>/` package) — a project can override or add to this
//! mapping in `.ta/db-adapters.toml` without touching TA core, and
//! `db://<anything-else>/*` resolves purely by matching the scheme string to
//! whatever plugin name a project has registered.

use crate::error::{ProxyError, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct DbAdapterEntry {
    pub scheme: String,
    pub plugin: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DbAdapterRegistry {
    #[serde(default, rename = "adapter")]
    pub entries: Vec<DbAdapterEntry>,
}

impl Default for DbAdapterRegistry {
    /// The default mapping ships `postgres`/`mysql`/`sqlite` — not as
    /// special-cased core logic, just as the registry's default data, same
    /// as any project could configure for its own plugins.
    fn default() -> Self {
        Self {
            entries: vec![
                DbAdapterEntry {
                    scheme: "postgres".to_string(),
                    plugin: "postgres".to_string(),
                },
                DbAdapterEntry {
                    scheme: "mysql".to_string(),
                    plugin: "mysql".to_string(),
                },
                DbAdapterEntry {
                    scheme: "sqlite".to_string(),
                    plugin: "sqlite".to_string(),
                },
            ],
        }
    }
}

impl DbAdapterRegistry {
    /// Load `.ta/db-adapters.toml`. A missing file falls back to the
    /// built-in defaults (postgres/mysql/sqlite) rather than an empty
    /// registry, so `db://postgres/...` resolves out of the box.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let mut registry: Self = toml::from_str(&text)
            .map_err(|e| ProxyError::Plugin(format!("invalid db-adapters.toml: {e}")))?;
        // Project entries take precedence; defaults fill in anything not
        // explicitly overridden so a project can add a new scheme without
        // having to re-declare postgres/mysql/sqlite.
        for default_entry in Self::default().entries {
            if !registry
                .entries
                .iter()
                .any(|e| e.scheme == default_entry.scheme)
            {
                registry.entries.push(default_entry);
            }
        }
        Ok(registry)
    }

    /// Registry with no entries at all, including no defaults — for callers
    /// that want to test/compose an explicit mapping.
    pub fn empty() -> Self {
        Self { entries: vec![] }
    }

    pub fn resolve(&self, scheme: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.scheme == scheme)
            .map(|e| e.plugin.as_str())
    }

    /// Resolve a `db://<scheme>/...` URI directly to a plugin name.
    /// Returns `None` if `uri` isn't a `db://` URI or the scheme has no
    /// registered plugin.
    pub fn resolve_uri<'a>(&'a self, uri: &str) -> Option<&'a str> {
        let rest = uri.strip_prefix("db://")?;
        let scheme = rest.split('/').next()?;
        self.resolve(scheme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_registry_file_falls_back_to_builtin_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let registry = DbAdapterRegistry::load(&dir.path().join("db-adapters.toml")).unwrap();
        assert_eq!(registry.resolve("postgres"), Some("postgres"));
        assert_eq!(registry.resolve("mysql"), Some("mysql"));
        assert_eq!(registry.resolve("sqlite"), Some("sqlite"));
        assert_eq!(registry.resolve("mongodb"), None);
    }

    #[test]
    fn empty_registry_has_no_entries() {
        let registry = DbAdapterRegistry::empty();
        assert!(registry.entries.is_empty());
        assert_eq!(registry.resolve("postgres"), None);
    }

    #[test]
    fn project_entry_overrides_builtin_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db-adapters.toml");
        std::fs::write(
            &path,
            "[[adapter]]\nscheme = \"postgres\"\nplugin = \"pg-proxy\"\n",
        )
        .unwrap();
        let registry = DbAdapterRegistry::load(&path).unwrap();
        assert_eq!(registry.resolve("postgres"), Some("pg-proxy"));
        // mysql/sqlite still fall back to builtin defaults — overriding one
        // scheme doesn't require re-declaring the others.
        assert_eq!(registry.resolve("mysql"), Some("mysql"));
        assert_eq!(registry.resolve("sqlite"), Some("sqlite"));
    }

    #[test]
    fn project_can_register_a_third_party_engine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db-adapters.toml");
        std::fs::write(
            &path,
            "[[adapter]]\nscheme = \"mongodb\"\nplugin = \"mongo-community\"\n",
        )
        .unwrap();
        let registry = DbAdapterRegistry::load(&path).unwrap();
        assert_eq!(registry.resolve("mongodb"), Some("mongo-community"));
        // TA core has no awareness of "mongodb" beyond this project-declared
        // mapping — the builtins are just data, not special-cased logic.
        assert_eq!(registry.resolve("postgres"), Some("postgres"));
    }

    #[test]
    fn resolve_uri_extracts_scheme_from_db_uri() {
        let registry = DbAdapterRegistry::default();
        assert_eq!(
            registry.resolve_uri("db://postgres/mydb/users/1"),
            Some("postgres")
        );
        assert_eq!(
            registry.resolve_uri("db://sqlite/app.db/items/5"),
            Some("sqlite")
        );
        assert_eq!(registry.resolve_uri("db://unknown-engine/x"), None);
        assert_eq!(registry.resolve_uri("not-a-db-uri"), None);
    }

    #[test]
    fn invalid_toml_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db-adapters.toml");
        std::fs::write(&path, "{{not valid toml}}").unwrap();
        assert!(DbAdapterRegistry::load(&path).is_err());
    }
}
