//! `.ta/plugins/<kind>/<name>/plugin.toml` discovery convention (§2.2),
//! shared by every Plugin-category integration.

use crate::manifest::PluginManifest;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    ProjectLocal,
    UserGlobal,
}

impl fmt::Display for PluginSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginSource::ProjectLocal => write!(f, "project"),
            PluginSource::UserGlobal => write!(f, "global"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub plugin_dir: PathBuf,
    pub source: PluginSource,
}

/// `$XDG_CONFIG_HOME` if set, else `$HOME/.config`. Intentionally not
/// macOS-special-cased, matching the VCS/messaging/social behavior being
/// unified (changing this would alter where existing plugins are found).
pub fn user_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config"))
}

fn scan_kind_dir(base: &Path, kind: &str, source: PluginSource) -> Vec<DiscoveredPlugin> {
    let kind_dir = base.join("plugins").join(kind);
    let Ok(entries) = std::fs::read_dir(&kind_dir) else {
        return vec![];
    };
    let mut found = vec![];
    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let manifest_path = plugin_dir.join("plugin.toml");
        if let Ok(manifest) = PluginManifest::load(&manifest_path) {
            found.push(DiscoveredPlugin {
                manifest,
                plugin_dir,
                source,
            });
        }
    }
    found.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    found
}

/// Discover every `<kind>` plugin, project-local first then user-global.
/// If a name exists in both, both entries are returned (callers that want a
/// single result per name should use `find_plugin`, which takes the first
/// match in this same project-then-global order).
pub fn discover_plugins(kind: &str, project_root: &Path) -> Vec<DiscoveredPlugin> {
    let mut found = scan_kind_dir(&project_root.join(".ta"), kind, PluginSource::ProjectLocal);
    if let Some(config_dir) = user_config_dir() {
        found.extend(scan_kind_dir(
            &config_dir.join("ta"),
            kind,
            PluginSource::UserGlobal,
        ));
    }
    found
}

pub fn find_plugin(kind: &str, name: &str, project_root: &Path) -> Option<DiscoveredPlugin> {
    discover_plugins(kind, project_root)
        .into_iter()
        .find(|p| p.manifest.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, kind: &str, name: &str) {
        let plugin_dir = dir.join(".ta").join("plugins").join(kind).join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!("name = \"{name}\"\ntype = \"{kind}\"\ncommand = \"{name}-bin\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn discovers_project_local_plugin() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "vcs", "perforce");
        let found = discover_plugins("vcs", dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.name, "perforce");
        assert_eq!(found[0].source, PluginSource::ProjectLocal);
    }

    #[test]
    fn find_plugin_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_plugin("vcs", "nope", dir.path()).is_none());
    }

    #[test]
    fn ignores_kind_dir_with_no_plugins() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_plugins("vcs", dir.path()).is_empty());
    }
}
