//! `.ta/triggers/<type>.toml` discovery, mirroring `ta-plugin::discovery`'s
//! `.ta/plugins/<kind>/<name>/plugin.toml` convention (§2.2/§13.1) — one
//! trigger-type config per file, keyed by the file's `type` field.

use crate::manifest::TriggerManifest;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DiscoveredTrigger {
    pub manifest: TriggerManifest,
    pub config_path: PathBuf,
}

fn triggers_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("triggers")
}

/// Discover every `.ta/triggers/*.toml` config. Files that fail to parse
/// are skipped with a `tracing::warn` (not fatal — one broken config
/// shouldn't take down discovery for every other trigger type).
pub fn discover_triggers(project_root: &Path) -> Vec<DiscoveredTrigger> {
    let dir = triggers_dir(project_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut found = vec![];
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        match TriggerManifest::load(&path) {
            Ok(manifest) => found.push(DiscoveredTrigger {
                manifest,
                config_path: path,
            }),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Skipping unparseable trigger config"
                );
            }
        }
    }
    found.sort_by(|a, b| a.manifest.trigger_type.cmp(&b.manifest.trigger_type));
    found
}

/// Find a single trigger by its `type` field.
pub fn find_trigger(trigger_type: &str, project_root: &Path) -> Option<DiscoveredTrigger> {
    discover_triggers(project_root)
        .into_iter()
        .find(|t| t.manifest.trigger_type == trigger_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, filename: &str, contents: &str) {
        let triggers_dir = dir.join(".ta").join("triggers");
        std::fs::create_dir_all(&triggers_dir).unwrap();
        std::fs::write(triggers_dir.join(filename), contents).unwrap();
    }

    #[test]
    fn discovers_configured_triggers() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "schedule.toml", "type = \"schedule\"\n");
        write_config(
            dir.path(),
            "inbound-email.toml",
            "type = \"inbound-email\"\ndispatch = \"queue\"\n",
        );
        let found = discover_triggers(dir.path());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].manifest.trigger_type, "inbound-email");
        assert_eq!(found[1].manifest.trigger_type, "schedule");
    }

    #[test]
    fn find_trigger_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_trigger("schedule", dir.path()).is_none());
    }

    #[test]
    fn skips_unparseable_config_without_failing_discovery() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "broken.toml", "not valid toml {{{");
        write_config(dir.path(), "schedule.toml", "type = \"schedule\"\n");
        let found = discover_triggers(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.trigger_type, "schedule");
    }

    #[test]
    fn empty_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_triggers(dir.path()).is_empty());
    }

    #[test]
    fn ignores_non_toml_files() {
        let dir = tempfile::tempdir().unwrap();
        let triggers_dir = dir.path().join(".ta").join("triggers");
        std::fs::create_dir_all(&triggers_dir).unwrap();
        std::fs::write(triggers_dir.join("README.md"), "not a config").unwrap();
        assert!(discover_triggers(dir.path()).is_empty());
    }
}
