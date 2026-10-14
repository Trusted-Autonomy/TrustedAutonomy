//! Plugin-aware entry point to the shared VCS detector (v0.17.12.5).
//!
//! The detector itself lives in `ta_workspace::vcs_detect` (so crates below
//! `ta-submit` can use it). This module adds the one thing that needs
//! manifest discovery: the detection markers VCS adapter plugins declare in
//! `plugin.toml` under `[detect]`.

use std::path::Path;

use ta_workspace::{PluginMarkers, VcsDetection};

use crate::vcs_plugin_manifest::discover_vcs_plugins;

/// Markers declared by installed VCS adapter plugins (project-local first,
/// then user-global). Plugins with no `[detect]` markers are omitted.
pub fn plugin_markers(project_root: &Path) -> Vec<PluginMarkers> {
    discover_vcs_plugins(project_root)
        .into_iter()
        .filter(|p| !p.manifest.detect.markers.is_empty())
        .map(|p| PluginMarkers {
            name: p.manifest.name,
            markers: p.manifest.detect.markers,
        })
        .collect()
}

/// Detect the VCS for `project_root`, including VCS adapter plugins.
///
/// This is the function `ta init`, `ta setup vcs` and `ta doctor` call.
pub fn detect_vcs(project_root: &Path) -> VcsDetection {
    ta_workspace::detect_vcs(project_root, &plugin_markers(project_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_workspace::VcsKind;

    #[test]
    fn project_local_plugin_with_markers_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/vcs/hgish");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            "name = \"hgish\"\ncommand = \"ta-submit-hgish\"\n\n[detect]\nmarkers = [\".hgish\"]\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join(".hgish")).unwrap();

        let markers = plugin_markers(dir.path());
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].name, "hgish");

        let d = detect_vcs(dir.path());
        assert_eq!(d.kind, VcsKind::Plugin);
        assert_eq!(d.plugin.as_deref(), Some("hgish"));
        assert_eq!(d.evidence[0].path, ".hgish");
    }

    #[test]
    fn plugin_without_detect_markers_is_not_consulted() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/vcs/quiet");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            "name = \"quiet\"\ncommand = \"ta-submit-quiet\"\n",
        )
        .unwrap();
        assert!(plugin_markers(dir.path()).is_empty());
    }
}
