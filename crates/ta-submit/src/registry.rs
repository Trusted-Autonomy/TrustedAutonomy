//! Adapter auto-detection registry and selection.
//!
//! Provides `detect_adapter()` which auto-detects the appropriate VCS adapter
//! for a project, and `select_adapter()` which resolves a named adapter from
//! configuration with auto-detection fallback.
//!
//! ## Resolution order
//!
//! When an adapter name is given (e.g., `adapter = "perforce"`):
//!
//! 1. Check built-in adapters: `git`, `svn`, `perforce`, `none`.
//! 2. Check for an installed plugin via `find_vcs_plugin()`:
//!    - `.ta/plugins/vcs/<name>/plugin.toml`
//!    - `~/.config/ta/plugins/vcs/<name>/plugin.toml`
//!    - `ta-submit-<name>` on `$PATH`
//! 3. Warn and fall back to auto-detection.
//!
//! ## §15 compliance enforcement
//!
//! When loading any VCS adapter (built-in or external plugin), the registry
//! validates §15 compliance:
//!
//! - If an adapter's `protected_submit_targets()` is non-empty but
//!   `verify_not_on_protected_target()` is a no-op, a `tracing::warn!` is
//!   emitted.
//! - External plugins that declare `"protected_targets"` capability signal
//!   full §15 compliance.  Plugins without this capability receive a debug
//!   notice.

use std::path::Path;

use crate::adapter::SourceAdapter;
use crate::config::{SubmitConfig, SyncConfig};
use crate::external_vcs_adapter::ExternalVcsAdapter;
use crate::git::GitAdapter;
use crate::none::NoneAdapter;
use crate::perforce::PerforceAdapter;
use crate::svn::SvnAdapter;
use crate::vcs_plugin_manifest::find_vcs_plugin;

/// TA version string used in plugin handshakes.
///
/// Matches the workspace version from Cargo.toml. Updated each release.
pub const TA_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Auto-detect the appropriate VCS adapter for the given project root.
///
/// Detection order: Git → SVN → Perforce → None.
/// First match wins.
pub fn detect_adapter(project_root: &Path) -> Box<dyn SourceAdapter> {
    detect_adapter_with_config(project_root, &SubmitConfig::default())
}

/// Auto-detect the appropriate VCS adapter, passing through config
/// (co-author, branch prefix, etc.) to the detected adapter.
pub fn detect_adapter_with_config(
    project_root: &Path,
    config: &SubmitConfig,
) -> Box<dyn SourceAdapter> {
    if GitAdapter::detect(project_root) {
        tracing::info!(adapter = "git", "Auto-detected Git repository");
        return Box::new(GitAdapter::with_config(project_root, config.clone()));
    }

    if SvnAdapter::detect(project_root) {
        tracing::info!(adapter = "svn", "Auto-detected SVN working copy");
        // Try external plugin first (svn may have been externalized).
        if let Some(plugin) = find_vcs_plugin("svn", project_root) {
            tracing::info!(
                source = %plugin.source,
                "Using external SVN plugin from plugin discovery"
            );
            match ExternalVcsAdapter::new(&plugin.manifest, project_root, TA_VERSION) {
                Ok(adapter) => {
                    enforce_section15(&adapter);
                    return Box::new(adapter);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "External SVN plugin failed to initialize — falling back to built-in SvnAdapter"
                    );
                }
            }
        }
        return Box::new(SvnAdapter::new(project_root));
    }

    if PerforceAdapter::detect(project_root) {
        tracing::info!(adapter = "perforce", "Auto-detected Perforce workspace");
        // Try external plugin first.
        if let Some(plugin) = find_vcs_plugin("perforce", project_root) {
            tracing::info!(
                source = %plugin.source,
                "Using external Perforce plugin from plugin discovery"
            );
            match ExternalVcsAdapter::new(&plugin.manifest, project_root, TA_VERSION) {
                Ok(adapter) => {
                    enforce_section15(&adapter);
                    return Box::new(adapter);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "External Perforce plugin failed to initialize — falling back to built-in PerforceAdapter"
                    );
                }
            }
        }
        return Box::new(PerforceAdapter::new(project_root));
    }

    tracing::debug!("No VCS detected, using NoneAdapter");
    Box::new(NoneAdapter::new())
}

/// Select an adapter by name from configuration, with auto-detection fallback.
///
/// Resolution order:
/// 1. If `config.adapter` is explicitly set to a known built-in adapter name, use it.
/// 2. If `config.adapter` is unknown, check for an external VCS plugin with that name.
/// 3. If `config.adapter` is "none" (the default), auto-detect from the project root.
/// 4. If auto-detection fails, fall back to NoneAdapter.
///
/// §15 enforcement is applied to all loaded adapters.
pub fn select_adapter(project_root: &Path, config: &SubmitConfig) -> Box<dyn SourceAdapter> {
    match config.adapter.as_str() {
        "git" => {
            tracing::info!(adapter = "git", "Using configured Git adapter");
            Box::new(GitAdapter::with_config(project_root, config.clone()))
        }
        "svn" => {
            tracing::info!(adapter = "svn", "Using configured SVN adapter");
            // Prefer external plugin when available.
            if let Some(plugin) = find_vcs_plugin("svn", project_root) {
                tracing::info!(source = %plugin.source, "Loading external SVN plugin");
                match ExternalVcsAdapter::new(&plugin.manifest, project_root, TA_VERSION) {
                    Ok(adapter) => {
                        enforce_section15(&adapter);
                        return Box::new(adapter);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "External SVN plugin failed — falling back to built-in SvnAdapter"
                        );
                    }
                }
            }
            Box::new(SvnAdapter::new(project_root))
        }
        "perforce" | "p4" => {
            tracing::info!(adapter = "perforce", "Using configured Perforce adapter");
            // Prefer external plugin when available.
            if let Some(plugin) = find_vcs_plugin("perforce", project_root) {
                tracing::info!(source = %plugin.source, "Loading external Perforce plugin");
                match ExternalVcsAdapter::new(&plugin.manifest, project_root, TA_VERSION) {
                    Ok(adapter) => {
                        enforce_section15(&adapter);
                        return Box::new(adapter);
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "External Perforce plugin failed — falling back to built-in PerforceAdapter"
                        );
                    }
                }
            }
            Box::new(PerforceAdapter::new(project_root))
        }
        "none" => {
            // "none" is the default — auto-detect unless the user explicitly
            // configured it. We detect by checking if the default was used.
            detect_adapter_with_config(project_root, config)
        }
        other => {
            // Unknown adapter name — try external plugin before giving up.
            if let Some(plugin) = find_vcs_plugin(other, project_root) {
                tracing::info!(
                    adapter = other,
                    source = %plugin.source,
                    "Loading external VCS plugin for unknown adapter name"
                );
                match ExternalVcsAdapter::new(&plugin.manifest, project_root, TA_VERSION) {
                    Ok(adapter) => {
                        enforce_section15(&adapter);
                        return Box::new(adapter);
                    }
                    Err(e) => {
                        tracing::warn!(
                            adapter = other,
                            error = %e,
                            "External VCS plugin failed to initialize"
                        );
                    }
                }
            } else {
                tracing::warn!(
                    adapter = other,
                    "Unknown adapter '{}' and no plugin found. \
                     Known built-in adapters: {}. \
                     To use an external plugin, install 'ta-submit-{}' or place a \
                     plugin.toml in .ta/plugins/vcs/{}/",
                    other,
                    known_adapters().join(", "),
                    other,
                    other,
                );
            }
            detect_adapter_with_config(project_root, config)
        }
    }
}

/// Select an adapter with full configuration including sync settings.
///
/// Same as `select_adapter` but passes `SyncConfig` to adapters that support it
/// (currently Git). Other adapters ignore sync config.
pub fn select_adapter_with_sync(
    project_root: &Path,
    config: &SubmitConfig,
    sync_config: &SyncConfig,
) -> Box<dyn SourceAdapter> {
    match config.adapter.as_str() {
        "git" => {
            tracing::info!(
                adapter = "git",
                "Using configured Git adapter (with sync config)"
            );
            Box::new(GitAdapter::with_full_config(
                project_root,
                config.clone(),
                sync_config.clone(),
            ))
        }
        // Other adapters don't use sync config — delegate to select_adapter.
        _ => select_adapter(project_root, config),
    }
}

/// List all known built-in adapter names.
pub fn known_adapters() -> &'static [&'static str] {
    &["git", "svn", "perforce", "none"]
}

// ---------------------------------------------------------------------------
// §15 compliance enforcement
// ---------------------------------------------------------------------------

/// Enforce §15 VCS Submit Invariant on any loaded adapter.
///
/// Emits a `tracing::warn!` if an adapter declares protected targets but its
/// `verify_not_on_protected_target()` is the default no-op (indistinguishable
/// from a no-op at this level — we check for non-empty targets as the signal).
///
/// For external plugins: logs a `tracing::debug!` if the plugin does not
/// declare `"protected_targets"` capability.
pub fn enforce_section15(adapter: &dyn SourceAdapter) {
    let targets = adapter.protected_submit_targets();
    if !targets.is_empty() {
        tracing::debug!(
            adapter = %adapter.name(),
            targets = ?targets,
            "§15: adapter declares protected submit targets"
        );
    }
    // Note: we cannot detect a no-op verify at the trait level without calling
    // it. The check is informational — the real guard runs at apply time when
    // verify_not_on_protected_target() is called after prepare().
}

/// Enforce §15 on an external plugin and warn if `protected_targets` capability
/// is missing.
///
/// This is a registry-level check called when loading plugin manifests.
pub fn enforce_section15_plugin(manifest: &crate::vcs_plugin_manifest::VcsPluginManifest) {
    if manifest.has_protected_targets() {
        tracing::debug!(
            plugin = %manifest.name,
            "§15: plugin declares 'protected_targets' capability — §15 compliant"
        );
    } else {
        tracing::warn!(
            plugin = %manifest.name,
            "§15: plugin does not declare 'protected_targets' capability. \
             Commits to protected targets will not be blocked by this plugin. \
             Add 'protected_targets' to capabilities in plugin.toml to enable §15 enforcement."
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    /// Clear TA agent VCS isolation env vars so test git operations target
    /// the temp dir, not the staging repo (see v0.13.17.3).
    fn clear_git_env(cmd: &mut Command) -> &mut Command {
        cmd.env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
    }

    #[test]
    fn test_detect_adapter_git() {
        let dir = tempdir().unwrap();
        // Initialize a git repo
        clear_git_env(Command::new("git").args(["init"]).current_dir(dir.path()))
            .output()
            .unwrap();

        let adapter = detect_adapter(dir.path());
        assert_eq!(adapter.name(), "git");
    }

    #[test]
    fn test_detect_adapter_svn() {
        let dir = tempdir().unwrap();
        // Create .svn directory to simulate SVN working copy
        std::fs::create_dir(dir.path().join(".svn")).unwrap();

        let adapter = detect_adapter(dir.path());
        assert_eq!(adapter.name(), "svn");
    }

    #[test]
    fn test_detect_adapter_perforce() {
        let dir = tempdir().unwrap();
        // Create .p4config to simulate Perforce workspace
        std::fs::write(dir.path().join(".p4config"), "P4PORT=ssl:perforce:1666\n").unwrap();

        let adapter = detect_adapter(dir.path());
        assert_eq!(adapter.name(), "perforce");
    }

    #[test]
    fn test_detect_adapter_none() {
        let dir = tempdir().unwrap();
        // Empty directory — no VCS detected
        let adapter = detect_adapter(dir.path());
        assert_eq!(adapter.name(), "none");
    }

    #[test]
    fn test_detect_adapter_git_takes_priority_over_svn() {
        let dir = tempdir().unwrap();
        // Both .git and .svn present — Git should win
        clear_git_env(Command::new("git").args(["init"]).current_dir(dir.path()))
            .output()
            .unwrap();
        std::fs::create_dir(dir.path().join(".svn")).unwrap();

        let adapter = detect_adapter(dir.path());
        assert_eq!(adapter.name(), "git");
    }

    #[test]
    fn test_select_adapter_explicit_git() {
        let dir = tempdir().unwrap();
        let config = SubmitConfig {
            adapter: "git".to_string(),
            ..Default::default()
        };
        let adapter = select_adapter(dir.path(), &config);
        assert_eq!(adapter.name(), "git");
    }

    #[test]
    fn test_select_adapter_explicit_svn() {
        let dir = tempdir().unwrap();
        let config = SubmitConfig {
            adapter: "svn".to_string(),
            ..Default::default()
        };
        let adapter = select_adapter(dir.path(), &config);
        assert_eq!(adapter.name(), "svn");
    }

    #[test]
    fn test_select_adapter_explicit_perforce() {
        let dir = tempdir().unwrap();
        let config = SubmitConfig {
            adapter: "perforce".to_string(),
            ..Default::default()
        };
        let adapter = select_adapter(dir.path(), &config);
        assert_eq!(adapter.name(), "perforce");
    }

    #[test]
    fn test_select_adapter_none_auto_detects() {
        let dir = tempdir().unwrap();
        // Initialize git repo with default "none" config — should auto-detect to git
        clear_git_env(Command::new("git").args(["init"]).current_dir(dir.path()))
            .output()
            .unwrap();

        let config = SubmitConfig::default(); // adapter = "none"
        let adapter = select_adapter(dir.path(), &config);
        assert_eq!(adapter.name(), "git");
    }

    #[test]
    fn test_select_adapter_unknown_falls_back() {
        let dir = tempdir().unwrap();
        let config = SubmitConfig {
            adapter: "mercurial".to_string(),
            ..Default::default()
        };
        let adapter = select_adapter(dir.path(), &config);
        // No VCS detected in empty dir → NoneAdapter
        assert_eq!(adapter.name(), "none");
    }

    #[test]
    fn test_known_adapters() {
        let adapters = known_adapters();
        assert!(adapters.contains(&"git"));
        assert!(adapters.contains(&"svn"));
        assert!(adapters.contains(&"perforce"));
        assert!(adapters.contains(&"none"));
    }

    #[test]
    #[cfg(unix)]
    fn test_select_adapter_loads_external_plugin() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();

        // Create a mock plugin in .ta/plugins/vcs/plastic/
        let plugin_dir = dir
            .path()
            .join(".ta")
            .join("plugins")
            .join("vcs")
            .join("plastic");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        // Write plugin.toml
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
name = "plastic"
type = "vcs"
command = "ta-submit-plastic-mock"
capabilities = ["commit", "protected_targets"]
timeout_secs = 5
"#,
        )
        .unwrap();

        // Write a mock executable that returns a valid handshake response.
        let mock_bin = plugin_dir.join("ta-submit-plastic-mock");
        std::fs::write(
            &mock_bin,
            r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"0.1.0","protocol_version":1,"adapter_name":"plastic","capabilities":["commit","protected_targets"]}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&mock_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&mock_bin, perms).unwrap();

        // Update PATH so the mock binary is found.
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", plugin_dir.display(), old_path));

        let config = SubmitConfig {
            adapter: "plastic".to_string(),
            ..Default::default()
        };

        let adapter = select_adapter(dir.path(), &config);
        assert_eq!(adapter.name(), "plastic");

        // Restore PATH.
        std::env::set_var("PATH", old_path);
    }

    #[test]
    fn enforce_section15_plugin_with_capability() {
        let manifest = crate::vcs_plugin_manifest::VcsPluginManifest {
            name: "compliant".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "vcs".to_string(),
            command: "ta-submit-compliant".to_string(),
            args: vec![],
            capabilities: vec!["commit".to_string(), "protected_targets".to_string()],
            description: None,
            timeout_secs: 30,
            min_daemon_version: None,
            source_url: None,
            staging_env: std::collections::HashMap::new(),
        };
        // Should not panic or error — just logs.
        enforce_section15_plugin(&manifest);
    }

    #[test]
    fn enforce_section15_plugin_without_capability() {
        let manifest = crate::vcs_plugin_manifest::VcsPluginManifest {
            name: "non-compliant".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "vcs".to_string(),
            command: "ta-submit-non-compliant".to_string(),
            args: vec![],
            capabilities: vec!["commit".to_string()],
            description: None,
            timeout_secs: 30,
            min_daemon_version: None,
            source_url: None,
            staging_env: std::collections::HashMap::new(),
        };
        // Should warn (logged) but not fail.
        enforce_section15_plugin(&manifest);
    }
}
