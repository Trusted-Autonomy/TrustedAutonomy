//! Build adapter auto-detection registry and selection.
//!
//! Provides `detect_build_adapter()` which auto-detects the appropriate build
//! adapter for a project, and `select_build_adapter()` which resolves a named
//! adapter from configuration with auto-detection fallback.

use std::path::Path;

use crate::adapter::BuildAdapter;
use crate::cargo::CargoAdapter;
use crate::npm::NpmAdapter;
use crate::script::ScriptAdapter;
use crate::webhook::WebhookAdapter;

/// Configuration for build adapter selection.
///
/// Mirrors the `[build]` section in `.ta/workflow.toml`.
pub struct BuildAdapterConfig {
    /// Adapter name: "cargo", "npm", "script", "webhook", or "auto" (default).
    pub adapter: String,
    /// Custom build command (used by script adapter, or as override for cargo/npm).
    pub command: Option<String>,
    /// Custom test command (used by script adapter, or as override for cargo/npm).
    pub test_command: Option<String>,
    /// Webhook URL (required for webhook adapter).
    pub webhook_url: Option<String>,
}

impl Default for BuildAdapterConfig {
    fn default() -> Self {
        Self {
            adapter: "auto".to_string(),
            command: None,
            test_command: None,
            webhook_url: None,
        }
    }
}

/// Auto-detect the appropriate build adapter for the given project root.
///
/// Detection order: Cargo -> npm -> Make (script) -> None.
/// First match wins.
pub fn detect_build_adapter(project_root: &Path) -> Option<Box<dyn BuildAdapter>> {
    if CargoAdapter::detect(project_root) {
        tracing::info!(adapter = "cargo", "Auto-detected Cargo project");
        return Some(Box::new(CargoAdapter::new(project_root)));
    }

    if NpmAdapter::detect(project_root) {
        tracing::info!(adapter = "npm", "Auto-detected npm project");
        return Some(Box::new(NpmAdapter::new(project_root)));
    }

    if ScriptAdapter::detect(project_root) {
        tracing::info!(adapter = "script", "Auto-detected Makefile project");
        return Some(Box::new(ScriptAdapter::make(project_root)));
    }

    tracing::debug!("No build system detected");
    None
}

/// Select a build adapter by name, with auto-detection fallback.
///
/// Resolution order:
/// 1. If `config.adapter` is explicitly set to a known name, use it.
/// 2. If `config.adapter` is "auto" (the default), auto-detect from the project root.
/// 3. If auto-detection finds nothing, return None.
pub fn select_build_adapter(
    project_root: &Path,
    config: &BuildAdapterConfig,
) -> Option<Box<dyn BuildAdapter>> {
    match config.adapter.as_str() {
        "cargo" => {
            tracing::info!(adapter = "cargo", "Using configured Cargo adapter");
            Some(Box::new(CargoAdapter::with_commands(
                project_root,
                config.command.clone(),
                config.test_command.clone(),
            )))
        }
        "npm" => {
            tracing::info!(adapter = "npm", "Using configured npm adapter");
            Some(Box::new(NpmAdapter::with_commands(
                project_root,
                config.command.clone(),
                config.test_command.clone(),
            )))
        }
        "script" => {
            let build_cmd = config.command.clone().unwrap_or_else(|| "make".to_string());
            let test_cmd = config
                .test_command
                .clone()
                .unwrap_or_else(|| "make test".to_string());
            tracing::info!(adapter = "script", "Using configured script adapter");
            Some(Box::new(ScriptAdapter::new(
                project_root,
                build_cmd,
                test_cmd,
            )))
        }
        "webhook" => {
            let url = match &config.webhook_url {
                Some(url) => url.clone(),
                None => {
                    tracing::warn!(
                        "Webhook adapter selected but no webhook_url configured. \
                         Set [build] webhook_url in .ta/workflow.toml."
                    );
                    return None;
                }
            };
            tracing::info!(adapter = "webhook", "Using configured webhook adapter");
            Some(Box::new(WebhookAdapter::new(url)))
        }
        "auto" | "none" => {
            if config.adapter == "none" {
                return None;
            }
            // Auto-detect, but apply command overrides if present.
            let detected = detect_build_adapter(project_root);
            if let Some(adapter) = &detected {
                // If custom commands were provided with "auto", apply them via script adapter.
                if config.command.is_some() || config.test_command.is_some() {
                    let build_cmd =
                        config
                            .command
                            .clone()
                            .unwrap_or_else(|| match adapter.name() {
                                "cargo" => "cargo build --workspace".to_string(),
                                "npm" => "npm run build".to_string(),
                                _ => "make".to_string(),
                            });
                    let test_cmd =
                        config
                            .test_command
                            .clone()
                            .unwrap_or_else(|| match adapter.name() {
                                "cargo" => "cargo test --workspace".to_string(),
                                "npm" => "npm test".to_string(),
                                _ => "make test".to_string(),
                            });
                    return Some(Box::new(ScriptAdapter::new(
                        project_root,
                        build_cmd,
                        test_cmd,
                    )));
                }
            }
            detected
        }
        other => {
            tracing::warn!(
                adapter = other,
                "Unknown build adapter '{}', falling back to auto-detection. \
                 Known adapters: cargo, npm, script, webhook, auto, none",
                other
            );
            detect_build_adapter(project_root)
        }
    }
}

/// List all known built-in build adapter names.
pub fn known_build_adapters() -> &'static [&'static str] {
    &["cargo", "npm", "script", "webhook", "auto", "none"]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detect_cargo_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"t\"").unwrap();
        let adapter = detect_build_adapter(dir.path());
        assert!(adapter.is_some());
        assert_eq!(adapter.unwrap().name(), "cargo");
    }

    #[test]
    fn detect_npm_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let adapter = detect_build_adapter(dir.path());
        assert!(adapter.is_some());
        assert_eq!(adapter.unwrap().name(), "npm");
    }

    #[test]
    fn detect_makefile_project() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), "all:\n\techo ok").unwrap();
        let adapter = detect_build_adapter(dir.path());
        assert!(adapter.is_some());
        assert_eq!(adapter.unwrap().name(), "script");
    }

    #[test]
    fn detect_empty_dir_returns_none() {
        let dir = tempdir().unwrap();
        assert!(detect_build_adapter(dir.path()).is_none());
    }

    #[test]
    fn cargo_has_priority_over_npm() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let adapter = detect_build_adapter(dir.path()).unwrap();
        assert_eq!(adapter.name(), "cargo");
    }

    #[test]
    fn select_explicit_cargo() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "cargo".to_string(),
            ..Default::default()
        };
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        assert_eq!(adapter.name(), "cargo");
    }

    #[test]
    fn select_explicit_npm() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "npm".to_string(),
            ..Default::default()
        };
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        assert_eq!(adapter.name(), "npm");
    }

    #[test]
    fn select_explicit_script() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "script".to_string(),
            command: Some("echo build".to_string()),
            test_command: Some("echo test".to_string()),
            ..Default::default()
        };
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        assert_eq!(adapter.name(), "script");
    }

    #[test]
    fn select_none_returns_none() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "none".to_string(),
            ..Default::default()
        };
        assert!(select_build_adapter(dir.path(), &config).is_none());
    }

    #[test]
    fn select_auto_detects() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let config = BuildAdapterConfig::default(); // adapter = "auto"
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        assert_eq!(adapter.name(), "cargo");
    }

    #[test]
    fn select_unknown_falls_back() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "gradle".to_string(),
            ..Default::default()
        };
        assert!(select_build_adapter(dir.path(), &config).is_none());
    }

    #[test]
    fn known_build_adapters_list() {
        let adapters = known_build_adapters();
        assert!(adapters.contains(&"cargo"));
        assert!(adapters.contains(&"npm"));
        assert!(adapters.contains(&"script"));
        assert!(adapters.contains(&"webhook"));
        assert!(adapters.contains(&"auto"));
        assert!(adapters.contains(&"none"));
    }

    #[test]
    fn select_webhook_without_url_returns_none() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "webhook".to_string(),
            ..Default::default()
        };
        assert!(select_build_adapter(dir.path(), &config).is_none());
    }

    #[test]
    fn select_webhook_with_url() {
        let dir = tempdir().unwrap();
        let config = BuildAdapterConfig {
            adapter: "webhook".to_string(),
            webhook_url: Some("https://ci.example.com".to_string()),
            ..Default::default()
        };
        let adapter = select_build_adapter(dir.path(), &config).unwrap();
        assert_eq!(adapter.name(), "webhook");
    }
}
