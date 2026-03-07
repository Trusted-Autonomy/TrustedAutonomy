//! Adapter auto-detection registry and selection.
//!
//! Provides `detect_adapter()` which auto-detects the appropriate VCS adapter
//! for a project, and `select_adapter()` which resolves a named adapter from
//! configuration with auto-detection fallback.

use std::path::Path;

use crate::adapter::SubmitAdapter;
use crate::config::SubmitConfig;
use crate::git::GitAdapter;
use crate::none::NoneAdapter;
use crate::perforce::PerforceAdapter;
use crate::svn::SvnAdapter;

/// Auto-detect the appropriate VCS adapter for the given project root.
///
/// Detection order: Git -> SVN -> Perforce -> None.
/// First match wins.
pub fn detect_adapter(project_root: &Path) -> Box<dyn SubmitAdapter> {
    if GitAdapter::detect(project_root) {
        tracing::info!(adapter = "git", "Auto-detected Git repository");
        return Box::new(GitAdapter::new(project_root));
    }

    if SvnAdapter::detect(project_root) {
        tracing::info!(adapter = "svn", "Auto-detected SVN working copy");
        return Box::new(SvnAdapter::new(project_root));
    }

    if PerforceAdapter::detect(project_root) {
        tracing::info!(adapter = "perforce", "Auto-detected Perforce workspace");
        return Box::new(PerforceAdapter::new(project_root));
    }

    tracing::debug!("No VCS detected, using NoneAdapter");
    Box::new(NoneAdapter::new())
}

/// Select an adapter by name from configuration, with auto-detection fallback.
///
/// Resolution order:
/// 1. If `config.adapter` is explicitly set to a known adapter name, use it.
/// 2. If `config.adapter` is "none" (the default), auto-detect from the project root.
/// 3. If auto-detection fails, fall back to NoneAdapter.
///
/// This replaces the raw `.git/` existence check that was previously in `draft.rs`.
pub fn select_adapter(project_root: &Path, config: &SubmitConfig) -> Box<dyn SubmitAdapter> {
    match config.adapter.as_str() {
        "git" => {
            tracing::info!(adapter = "git", "Using configured Git adapter");
            Box::new(GitAdapter::new(project_root))
        }
        "svn" => {
            tracing::info!(adapter = "svn", "Using configured SVN adapter");
            Box::new(SvnAdapter::new(project_root))
        }
        "perforce" | "p4" => {
            tracing::info!(adapter = "perforce", "Using configured Perforce adapter");
            Box::new(PerforceAdapter::new(project_root))
        }
        "none" => {
            // "none" is the default — auto-detect unless the user explicitly
            // configured it. We detect by checking if the default was used.
            detect_adapter(project_root)
        }
        other => {
            tracing::warn!(
                adapter = other,
                "Unknown adapter '{}', falling back to auto-detection. \
                 Known adapters: git, svn, perforce, none",
                other
            );
            detect_adapter(project_root)
        }
    }
}

/// List all known built-in adapter names.
pub fn known_adapters() -> &'static [&'static str] {
    &["git", "svn", "perforce", "none"]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn test_detect_adapter_git() {
        let dir = tempdir().unwrap();
        // Initialize a git repo
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
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
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
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
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
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
}
