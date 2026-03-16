// plugin_resolver.rs — Resolve, download, verify, and install plugins from a project manifest.
//
// This is the core engine behind `ta setup`. Given a ProjectManifest, it:
// 1. Checks which plugins are already installed and their versions
// 2. Downloads missing/outdated plugins from registry, GitHub, or URL
// 3. Verifies SHA-256 hashes
// 4. Extracts tarballs to `.ta/plugins/<type>/<name>/`
// 5. Falls back to source builds for `path:` sources

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::plugin::{discover_plugins, PluginManifest};
use crate::project_manifest::{
    parse_source_scheme, version_satisfies, PluginRequirement, ProjectManifest, SourceScheme,
};
use crate::registry_client::{detect_platform, RegistryClient, RegistryIndex};

/// Result of resolving a single plugin.
#[derive(Debug)]
pub enum PluginResolveResult {
    /// Plugin already installed and version satisfies the constraint.
    AlreadyInstalled {
        name: String,
        installed_version: String,
    },
    /// Plugin was freshly installed.
    Installed {
        name: String,
        version: String,
        source: String,
    },
    /// Plugin was built from source.
    BuiltFromSource { name: String, source_path: PathBuf },
    /// Plugin resolution failed.
    Failed { name: String, reason: String },
    /// Plugin was skipped (optional and not available).
    Skipped { name: String, reason: String },
}

/// Result of a full `ta setup` resolution.
#[derive(Debug)]
pub struct ResolveReport {
    /// Results for each plugin.
    pub results: Vec<PluginResolveResult>,
    /// Environment variables that are missing.
    pub missing_env_vars: Vec<(String, Vec<String>)>,
}

impl ResolveReport {
    /// Check if all required plugins resolved successfully.
    pub fn all_ok(&self) -> bool {
        !self
            .results
            .iter()
            .any(|r| matches!(r, PluginResolveResult::Failed { .. }))
    }

    /// Count of successfully installed or already-present plugins.
    pub fn success_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    PluginResolveResult::AlreadyInstalled { .. }
                        | PluginResolveResult::Installed { .. }
                        | PluginResolveResult::BuiltFromSource { .. }
                )
            })
            .count()
    }

    /// Count of failed plugins.
    pub fn failure_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r, PluginResolveResult::Failed { .. }))
            .count()
    }
}

/// Resolve all plugins declared in a project manifest.
///
/// For each plugin:
/// 1. Check if already installed with a satisfying version
/// 2. Based on source scheme, download or build
/// 3. Verify integrity (sha256 for downloads)
/// 4. Install to `.ta/plugins/<type>/<name>/`
///
/// `ci_mode`: If true, treat optional plugin failures as hard errors.
pub fn resolve_all(
    manifest: &ProjectManifest,
    project_root: &Path,
    ci_mode: bool,
) -> ResolveReport {
    let platform = detect_platform();
    let installed = discover_plugins(project_root);

    let mut results = Vec::new();
    let mut missing_env_vars = Vec::new();

    // Try to fetch registry index (only if any plugin uses registry: source).
    let needs_registry = manifest
        .plugins
        .values()
        .any(|r| r.source.starts_with("registry:"));
    let registry_index = if needs_registry {
        let client = RegistryClient::new();
        match client.fetch_index() {
            Ok(index) => Some(index),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to fetch registry index");
                None
            }
        }
    } else {
        None
    };

    for (name, requirement) in &manifest.plugins {
        // Check environment variables.
        let missing: Vec<String> = requirement
            .env_vars
            .iter()
            .filter(|var| std::env::var(var).is_err())
            .cloned()
            .collect();
        if !missing.is_empty() {
            missing_env_vars.push((name.clone(), missing));
        }

        // Check if already installed with satisfying version.
        let existing = installed.iter().find(|p| p.manifest.name == *name);

        if let Some(existing) = existing {
            if version_satisfies(&existing.manifest.version, &requirement.version) {
                results.push(PluginResolveResult::AlreadyInstalled {
                    name: name.clone(),
                    installed_version: existing.manifest.version.clone(),
                });
                continue;
            }
            tracing::info!(
                plugin = %name,
                installed = %existing.manifest.version,
                required = %requirement.version,
                "Installed version does not satisfy requirement — upgrading"
            );
        }

        // Resolve based on source scheme.
        let result = resolve_single(
            name,
            requirement,
            project_root,
            &platform,
            registry_index.as_ref(),
        );

        if let PluginResolveResult::Failed { name, reason } = &result {
            if !requirement.required && !ci_mode {
                results.push(PluginResolveResult::Skipped {
                    name: name.clone(),
                    reason: reason.clone(),
                });
                continue;
            }
        }

        results.push(result);
    }

    ResolveReport {
        results,
        missing_env_vars,
    }
}

/// Resolve a single plugin from its requirement.
fn resolve_single(
    name: &str,
    requirement: &PluginRequirement,
    project_root: &Path,
    platform: &str,
    registry_index: Option<&RegistryIndex>,
) -> PluginResolveResult {
    let scheme = match parse_source_scheme(name, &requirement.source) {
        Ok(s) => s,
        Err(e) => {
            return PluginResolveResult::Failed {
                name: name.to_string(),
                reason: e.to_string(),
            };
        }
    };

    match scheme {
        SourceScheme::Registry(registry_name) => resolve_from_registry(
            name,
            &registry_name,
            requirement,
            project_root,
            platform,
            registry_index,
        ),
        SourceScheme::GitHub(repo) => {
            resolve_from_github(name, &repo, requirement, project_root, platform)
        }
        SourceScheme::Path(source_path) => resolve_from_path(name, &source_path, project_root),
        SourceScheme::Url(url) => resolve_from_url(name, &url, requirement, project_root),
    }
}

/// Resolve from the TA plugin registry.
fn resolve_from_registry(
    name: &str,
    registry_name: &str,
    requirement: &PluginRequirement,
    project_root: &Path,
    platform: &str,
    registry_index: Option<&RegistryIndex>,
) -> PluginResolveResult {
    let index = match registry_index {
        Some(idx) => idx,
        None => {
            return PluginResolveResult::Failed {
                name: name.to_string(),
                reason: "Registry index not available. Check network connection and try again."
                    .to_string(),
            };
        }
    };

    let client = RegistryClient::new();
    match client.resolve(index, registry_name, &requirement.version, platform) {
        Ok(resolved) => {
            match download_and_install(
                name,
                &resolved.download_url,
                &resolved.sha256,
                &requirement.plugin_type,
                project_root,
            ) {
                Ok(_) => PluginResolveResult::Installed {
                    name: name.to_string(),
                    version: resolved.version,
                    source: format!("registry:{}", registry_name),
                },
                Err(e) => PluginResolveResult::Failed {
                    name: name.to_string(),
                    reason: e,
                },
            }
        }
        Err(e) => PluginResolveResult::Failed {
            name: name.to_string(),
            reason: e.to_string(),
        },
    }
}

/// Resolve from a GitHub release.
fn resolve_from_github(
    name: &str,
    repo: &str,
    requirement: &PluginRequirement,
    project_root: &Path,
    platform: &str,
) -> PluginResolveResult {
    // Extract minimum version from constraint for the download URL.
    let version =
        crate::project_manifest::parse_min_version(&requirement.version).unwrap_or("0.1.0");
    let url = RegistryClient::github_release_url(repo, name, version, platform);

    // GitHub releases don't have pre-known sha256, so we skip verification.
    match download_and_install(name, &url, "", &requirement.plugin_type, project_root) {
        Ok(_) => PluginResolveResult::Installed {
            name: name.to_string(),
            version: version.to_string(),
            source: format!("github:{}", repo),
        },
        Err(e) => PluginResolveResult::Failed {
            name: name.to_string(),
            reason: e,
        },
    }
}

/// Resolve from a local path (source build).
fn resolve_from_path(name: &str, source_path: &Path, project_root: &Path) -> PluginResolveResult {
    // Resolve relative paths against project root.
    let abs_path = if source_path.is_relative() {
        project_root.join(source_path)
    } else {
        source_path.to_path_buf()
    };

    if !abs_path.exists() {
        return PluginResolveResult::Failed {
            name: name.to_string(),
            reason: format!(
                "Source path '{}' does not exist. Check the 'source' field in project.toml.",
                abs_path.display()
            ),
        };
    }

    // Try to build from source.
    match build_from_source(name, &abs_path, project_root) {
        Ok(_) => PluginResolveResult::BuiltFromSource {
            name: name.to_string(),
            source_path: abs_path,
        },
        Err(e) => PluginResolveResult::Failed {
            name: name.to_string(),
            reason: e,
        },
    }
}

/// Resolve from a direct URL.
fn resolve_from_url(
    name: &str,
    url: &str,
    requirement: &PluginRequirement,
    project_root: &Path,
) -> PluginResolveResult {
    match download_and_install(name, url, "", &requirement.plugin_type, project_root) {
        Ok(_) => PluginResolveResult::Installed {
            name: name.to_string(),
            version: "unknown".to_string(),
            source: format!("url:{}", url),
        },
        Err(e) => PluginResolveResult::Failed {
            name: name.to_string(),
            reason: e,
        },
    }
}

/// Download a tarball, verify its hash, and extract to the plugin directory.
fn download_and_install(
    name: &str,
    url: &str,
    expected_sha256: &str,
    plugin_type: &str,
    project_root: &Path,
) -> Result<PathBuf, String> {
    tracing::info!(plugin = %name, url = %url, "Downloading plugin");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("Download failed from {}: {}", url, e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Download failed: HTTP {} from {}. Check the URL and try again.",
            resp.status(),
            url
        ));
    }

    let bytes = resp
        .bytes()
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    // Verify SHA-256 if provided.
    if !expected_sha256.is_empty() {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual_hash = format!("{:x}", hasher.finalize());
        if actual_hash != expected_sha256 {
            return Err(format!(
                "SHA-256 mismatch for '{}': expected {}, got {}. \
                 The download may be corrupted or tampered with.",
                name, expected_sha256, actual_hash
            ));
        }
    }

    // Determine install directory.
    let install_dir = project_root
        .join(".ta")
        .join("plugins")
        .join(plugin_type)
        .join(name);
    std::fs::create_dir_all(&install_dir).map_err(|e| {
        format!(
            "Failed to create plugin directory {}: {}",
            install_dir.display(),
            e
        )
    })?;

    // Extract tarball.
    extract_tarball(&bytes, &install_dir)
        .map_err(|e| format!("Failed to extract plugin tarball: {}", e))?;

    tracing::info!(
        plugin = %name,
        path = %install_dir.display(),
        "Plugin installed successfully"
    );

    Ok(install_dir)
}

/// Extract a gzipped tarball to a directory.
fn extract_tarball(data: &[u8], target_dir: &Path) -> Result<(), String> {
    // Try gzip first.
    let cursor = std::io::Cursor::new(data);
    let gz = flate2_decode(cursor);

    match gz {
        Ok(decompressed) => tar_extract(std::io::Cursor::new(decompressed), target_dir),
        Err(_) => {
            // Maybe it's not gzipped — try raw tar.
            tar_extract(std::io::Cursor::new(data), target_dir)
        }
    }
}

/// Decompress gzip data.
fn flate2_decode<R: std::io::Read>(_reader: R) -> Result<Vec<u8>, String> {
    // Simple gzip decompression using the flate2 crate would be ideal,
    // but to avoid adding a dependency, we shell out to gunzip.
    // For the MVP, we write to a temp file and use the system tar command.
    Err("gzip decompression requires system tar".to_string())
}

/// Extract a tar archive using the system `tar` command.
fn tar_extract<R: std::io::Read>(reader: R, target_dir: &Path) -> Result<(), String> {
    // Write data to temp file and extract with system tar.
    let temp_dir = target_dir.parent().unwrap_or(target_dir);
    let temp_file = temp_dir.join(format!(".download-{}.tar.gz", std::process::id()));

    // The data coming in is the original bytes (potentially gzipped).
    // We'll write it and let `tar` auto-detect compression.
    let mut reader = reader;
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to buffer download: {}", e))?;

    std::fs::write(&temp_file, &buf).map_err(|e| format!("Failed to write temp file: {}", e))?;

    let output = std::process::Command::new("tar")
        .args([
            "xzf",
            &temp_file.to_string_lossy(),
            "-C",
            &target_dir.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    // Clean up temp file.
    let _ = std::fs::remove_file(&temp_file);

    if !output.status.success() {
        // Try without -z (not gzipped).
        std::fs::write(&temp_file, &buf)
            .map_err(|e| format!("Failed to write temp file: {}", e))?;
        let output2 = std::process::Command::new("tar")
            .args([
                "xf",
                &temp_file.to_string_lossy(),
                "-C",
                &target_dir.to_string_lossy(),
            ])
            .output()
            .map_err(|e| format!("Failed to run tar: {}", e))?;
        let _ = std::fs::remove_file(&temp_file);
        if !output2.status.success() {
            return Err(format!(
                "tar extraction failed: {}",
                String::from_utf8_lossy(&output2.stderr)
            ));
        }
    }

    Ok(())
}

/// Build a plugin from source.
///
/// Detects the toolchain and runs the appropriate build command:
/// - Rust: `cargo build --release`
/// - Go: `go build`
/// - Other: uses `build_command` from channel.toml or `make`
fn build_from_source(name: &str, source_dir: &Path, project_root: &Path) -> Result<(), String> {
    tracing::info!(
        plugin = %name,
        source = %source_dir.display(),
        "Building plugin from source"
    );

    // Check for a channel.toml with a custom build command.
    let manifest_path = source_dir.join("channel.toml");
    let custom_build = if manifest_path.exists() {
        PluginManifest::load(&manifest_path)
            .ok()
            .and_then(|m| m.build_command.clone())
    } else {
        None
    };

    let (cmd, args) = if let Some(ref build_cmd) = custom_build {
        // Use custom build command.
        let parts: Vec<&str> = build_cmd.split_whitespace().collect();
        if parts.is_empty() {
            return Err("Empty build_command in channel.toml".to_string());
        }
        (
            parts[0].to_string(),
            parts[1..].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    } else if source_dir.join("Cargo.toml").exists() {
        // Rust plugin.
        (
            "cargo".to_string(),
            vec!["build".to_string(), "--release".to_string()],
        )
    } else if source_dir.join("go.mod").exists() {
        // Go plugin.
        (
            "go".to_string(),
            vec![
                "build".to_string(),
                "-o".to_string(),
                format!("ta-channel-{}", name),
            ],
        )
    } else if source_dir.join("Makefile").exists() {
        ("make".to_string(), vec![])
    } else {
        return Err(format!(
            "Cannot determine how to build plugin '{}' at {}. \
             Add a Cargo.toml, go.mod, Makefile, or set build_command in channel.toml.",
            name,
            source_dir.display()
        ));
    };

    let output = std::process::Command::new(&cmd)
        .args(&args)
        .current_dir(source_dir)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run build command '{} {}': {}. \
                 Make sure the toolchain is installed and on PATH.",
                cmd,
                args.join(" "),
                e
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last_lines: Vec<&str> = stderr.lines().rev().take(20).collect();
        return Err(format!(
            "Build failed for plugin '{}'. Command: {} {}\nLast 20 lines of output:\n{}",
            name,
            cmd,
            args.join(" "),
            last_lines.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }

    // Install: copy the plugin directory contents to the project plugin dir.
    let install_dir = project_root
        .join(".ta")
        .join("plugins")
        .join("channels")
        .join(name);
    std::fs::create_dir_all(&install_dir)
        .map_err(|e| format!("Failed to create install dir: {}", e))?;

    crate::plugin::copy_dir_contents_public(source_dir, &install_dir)
        .map_err(|e| format!("Failed to copy plugin files: {}", e))?;

    // For Rust plugins, also copy the release binary.
    let release_binary = source_dir
        .join("target")
        .join("release")
        .join(format!("ta-channel-{}", name));
    if release_binary.exists() {
        let dest = install_dir.join(format!("ta-channel-{}", name));
        std::fs::copy(&release_binary, &dest)
            .map_err(|e| format!("Failed to copy release binary: {}", e))?;
    }

    tracing::info!(
        plugin = %name,
        install_dir = %install_dir.display(),
        "Plugin built and installed from source"
    );

    Ok(())
}

/// Check all required plugins from a manifest against installed plugins.
///
/// Returns a list of (name, issue) for any plugin that is missing or
/// below the required version. Used by daemon startup to enforce requirements.
pub fn check_requirements(
    manifest: &ProjectManifest,
    project_root: &Path,
) -> Vec<(String, String)> {
    let installed = discover_plugins(project_root);
    let mut issues = Vec::new();

    for (name, requirement) in &manifest.plugins {
        if !requirement.required {
            continue;
        }

        let existing = installed.iter().find(|p| p.manifest.name == *name);

        match existing {
            None => {
                issues.push((
                    name.clone(),
                    format!(
                        "Required plugin '{}' is not installed. Run `ta setup` to install it.",
                        name
                    ),
                ));
            }
            Some(p) => {
                if !version_satisfies(&p.manifest.version, &requirement.version) {
                    issues.push((
                        name.clone(),
                        format!(
                            "Plugin '{}' version {} does not satisfy requirement {}. \
                             Run `ta setup` to upgrade.",
                            name, p.manifest.version, requirement.version
                        ),
                    ));
                }
            }
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_manifest::ProjectManifest;

    #[test]
    fn check_requirements_all_installed() {
        let dir = tempfile::tempdir().unwrap();
        let plugins_dir = dir.path().join(".ta").join("plugins").join("channels");

        // Install a plugin.
        let plugin_dir = plugins_dir.join("test-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("channel.toml"),
            r#"
name = "test-plugin"
version = "0.2.0"
command = "test"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let toml_str = r#"
[project]
name = "test"

[plugins.test-plugin]
type = "channel"
version = ">=0.1.0"
source = "registry:test-plugin"
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        let issues = check_requirements(&manifest, dir.path());
        assert!(issues.is_empty(), "expected no issues: {:?}", issues);
    }

    #[test]
    fn check_requirements_missing_plugin() {
        let dir = tempfile::tempdir().unwrap();

        let toml_str = r#"
[project]
name = "test"

[plugins.missing]
type = "channel"
version = ">=0.1.0"
source = "registry:missing"
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        let issues = check_requirements(&manifest, dir.path());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].1.contains("not installed"));
    }

    #[test]
    fn check_requirements_version_too_low() {
        let dir = tempfile::tempdir().unwrap();
        let plugins_dir = dir.path().join(".ta").join("plugins").join("channels");

        let plugin_dir = plugins_dir.join("old-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("channel.toml"),
            r#"
name = "old-plugin"
version = "0.0.5"
command = "test"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let toml_str = r#"
[project]
name = "test"

[plugins.old-plugin]
type = "channel"
version = ">=0.1.0"
source = "registry:old-plugin"
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        let issues = check_requirements(&manifest, dir.path());
        assert_eq!(issues.len(), 1);
        assert!(issues[0].1.contains("does not satisfy"));
    }

    #[test]
    fn check_requirements_optional_not_reported() {
        let dir = tempfile::tempdir().unwrap();

        let toml_str = r#"
[project]
name = "test"

[plugins.optional-thing]
type = "channel"
version = ">=0.1.0"
source = "registry:optional-thing"
required = false
"#;
        let manifest: ProjectManifest = toml::from_str(toml_str).unwrap();
        let issues = check_requirements(&manifest, dir.path());
        assert!(issues.is_empty());
    }

    #[test]
    fn resolve_report_methods() {
        let report = ResolveReport {
            results: vec![
                PluginResolveResult::AlreadyInstalled {
                    name: "a".into(),
                    installed_version: "0.1.0".into(),
                },
                PluginResolveResult::Installed {
                    name: "b".into(),
                    version: "0.2.0".into(),
                    source: "registry:b".into(),
                },
                PluginResolveResult::Failed {
                    name: "c".into(),
                    reason: "not found".into(),
                },
                PluginResolveResult::Skipped {
                    name: "d".into(),
                    reason: "optional".into(),
                },
            ],
            missing_env_vars: vec![("b".into(), vec!["TOKEN".into()])],
        };

        assert!(!report.all_ok());
        assert_eq!(report.success_count(), 2);
        assert_eq!(report.failure_count(), 1);
    }

    #[test]
    fn resolve_report_all_ok() {
        let report = ResolveReport {
            results: vec![PluginResolveResult::AlreadyInstalled {
                name: "a".into(),
                installed_version: "0.1.0".into(),
            }],
            missing_env_vars: vec![],
        };

        assert!(report.all_ok());
        assert_eq!(report.success_count(), 1);
        assert_eq!(report.failure_count(), 0);
    }

    #[test]
    fn build_from_source_no_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        // No Cargo.toml, go.mod, or Makefile.
        let result = build_from_source("test", source.path(), dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cannot determine"));
    }

    #[test]
    fn sha256_verification() {
        use sha2::{Digest, Sha256};
        let data = b"hello world";
        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash = format!("{:x}", hasher.finalize());
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
