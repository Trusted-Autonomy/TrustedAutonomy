// plugin.rs — `ta plugin` CLI commands for managing channel plugins.
//
// Provides:
//   - `ta plugin list` — show installed channel plugins with protocol, capabilities, validation
//   - `ta plugin install <path>` — install a plugin from a directory
//   - `ta plugin build` — build plugin binaries from source and install them

use std::path::{Path, PathBuf};

use clap::Subcommand;
use ta_changeset::plugin;

#[derive(Subcommand)]
pub enum PluginCommands {
    /// List installed channel plugins from project and global directories.
    List,
    /// Install a channel plugin from a local directory.
    Install {
        /// Path to the plugin directory (must contain channel.toml).
        path: PathBuf,
        /// Install globally (~/.config/ta/plugins/channels/) instead of project-local.
        #[arg(long)]
        global: bool,
    },
    /// Validate all installed plugins (check commands exist, URLs reachable).
    Validate,
    /// Build channel plugin binaries from source in plugins/.
    ///
    /// Discovers Rust plugins (Cargo.toml + channel.toml) in the plugins/ directory,
    /// runs `cargo build --release`, and installs the binary + manifest to
    /// .ta/plugins/channels/<name>/.
    Build {
        /// Plugin names to build (comma-separated or multiple args).
        /// If omitted, use --all to build everything.
        #[arg(value_delimiter = ',')]
        names: Vec<String>,
        /// Build all discoverable plugins in plugins/.
        #[arg(long)]
        all: bool,
    },
    /// Check installed plugin versions for compatibility and updates (v0.10.16).
    ///
    /// Reports plugins whose `min_daemon_version` exceeds the current CLI version
    /// or whose installed version differs from the source in plugins/.
    Check,
    /// Upgrade a plugin by re-building and installing from source (v0.10.16).
    ///
    /// Rebuilds the plugin from the local `plugins/` directory and installs
    /// the new version. If the plugin has a `source_url` in its manifest,
    /// logs the URL for manual fetch.
    Upgrade {
        /// Plugin name to upgrade.
        name: String,
    },
}

pub fn run_plugin(project_root: &std::path::Path, command: &PluginCommands) -> anyhow::Result<()> {
    match command {
        PluginCommands::List => list_plugins(project_root),
        PluginCommands::Install { path, global } => install_plugin(project_root, path, *global),
        PluginCommands::Validate => validate_plugins(project_root),
        PluginCommands::Build { names, all } => build_plugins(project_root, names, *all),
        PluginCommands::Check => check_plugins(project_root),
        PluginCommands::Upgrade { name } => upgrade_plugin(project_root, name),
    }
}

fn list_plugins(project_root: &std::path::Path) -> anyhow::Result<()> {
    let plugins = plugin::discover_plugins(project_root);

    if plugins.is_empty() {
        println!("No channel plugins installed.");
        println!();
        println!("Install plugins with: ta plugin install <path>");
        println!("Plugin directories scanned:");
        println!(
            "  Project: {}/.ta/plugins/channels/",
            project_root.display()
        );
        println!("  Global:  ~/.config/ta/plugins/channels/");
        return Ok(());
    }

    println!("Installed channel plugins ({}):", plugins.len());
    println!();

    for p in &plugins {
        let m = &p.manifest;
        let cmd_display = match &m.command {
            Some(cmd) => {
                let mut full = cmd.clone();
                if !m.args.is_empty() {
                    full.push(' ');
                    full.push_str(&m.args.join(" "));
                }
                full
            }
            None => "-".to_string(),
        };
        let url_display = m.deliver_url.as_deref().unwrap_or("-");

        println!("  {} v{} [{}]", m.name, m.version, p.source);
        println!("    Protocol:     {}", m.protocol);
        if m.protocol == plugin::PluginProtocol::JsonStdio {
            println!("    Command:      {}", cmd_display);
        } else {
            println!("    Deliver URL:  {}", url_display);
        }
        if let Some(ref desc) = m.description {
            println!("    Description:  {}", desc);
        }
        println!("    Capabilities: {}", m.capabilities.join(", "));
        println!("    Timeout:      {}s", m.timeout_secs);
        println!("    Directory:    {}", p.plugin_dir.display());
        println!();
    }

    Ok(())
}

fn install_plugin(
    project_root: &std::path::Path,
    source: &std::path::Path,
    global: bool,
) -> anyhow::Result<()> {
    // Check that source exists and has channel.toml.
    if !source.is_dir() {
        anyhow::bail!(
            "Plugin source '{}' is not a directory. \
             Provide a directory containing a channel.toml manifest.",
            source.display()
        );
    }

    let manifest_path = source.join("channel.toml");
    if !manifest_path.exists() {
        anyhow::bail!(
            "No channel.toml found in '{}'. \
             A valid channel plugin directory must contain a channel.toml manifest.",
            source.display()
        );
    }

    let result = plugin::install_plugin(source, project_root, global)?;
    let location = if global { "global" } else { "project" };

    println!(
        "Installed channel plugin '{}' v{} ({}).",
        result.manifest.name, result.manifest.version, location
    );
    println!("  Protocol:  {}", result.manifest.protocol);
    println!("  Directory: {}", result.plugin_dir.display());
    println!();
    println!(
        "Configure it in .ta/daemon.toml under [[channels.external]] or \
         .ta/config.yaml channels section."
    );

    Ok(())
}

fn validate_plugins(project_root: &std::path::Path) -> anyhow::Result<()> {
    let plugins = plugin::discover_plugins(project_root);

    if plugins.is_empty() {
        println!("No channel plugins installed to validate.");
        return Ok(());
    }

    println!("Validating {} channel plugins...", plugins.len());
    println!();

    let mut ok_count = 0;
    let mut err_count = 0;

    for p in &plugins {
        let m = &p.manifest;
        match m.validate() {
            Ok(()) => {
                // Additional validation: check command exists on PATH for stdio plugins.
                if m.protocol == plugin::PluginProtocol::JsonStdio {
                    if let Some(ref cmd) = m.command {
                        let program = cmd.split_whitespace().next().unwrap_or(cmd);
                        match which_program(program) {
                            true => {
                                println!("  [ok]   {} — command '{}' found", m.name, program);
                                ok_count += 1;
                            }
                            false => {
                                println!(
                                    "  [FAIL] {} — command '{}' not found on PATH",
                                    m.name, program
                                );
                                err_count += 1;
                            }
                        }
                    }
                } else if m.protocol == plugin::PluginProtocol::Http {
                    if let Some(ref url) = m.deliver_url {
                        if url.starts_with("http://") || url.starts_with("https://") {
                            println!("  [ok]   {} — URL format valid: {}", m.name, url);
                            ok_count += 1;
                        } else {
                            println!(
                                "  [FAIL] {} — URL must start with http:// or https://: {}",
                                m.name, url
                            );
                            err_count += 1;
                        }
                    }
                }
            }
            Err(e) => {
                println!("  [FAIL] {} — {}", m.name, e);
                err_count += 1;
            }
        }
    }

    println!();
    println!(
        "Validation complete: {} ok, {} failed.",
        ok_count, err_count
    );

    if err_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}

// ── Build command ──────────────────────────────────────────────────────

/// A plugin source directory discovered in `plugins/`.
struct BuildablePlugin {
    /// Directory name (e.g., "ta-channel-discord").
    dir_name: String,
    /// Full path to the plugin source directory.
    source_dir: PathBuf,
    /// Parsed channel.toml manifest.
    manifest: plugin::PluginManifest,
    /// Binary name from Cargo.toml [[bin]] or package name.
    binary_name: String,
    /// Whether this is a Rust plugin (has Cargo.toml).
    is_rust: bool,
}

/// Discover buildable plugins in the `plugins/` directory.
///
/// A buildable plugin is a subdirectory that contains `channel.toml` and either:
///   - `Cargo.toml` (Rust plugin — built with `cargo build --release` by default)
///   - A `build_command` field in `channel.toml` (non-Rust: Go, Python, Node, etc.)
///
/// The binary name is extracted from `[[bin]]` entries for Rust plugins or
/// falls back to the directory name.
fn discover_buildable_plugins(project_root: &Path) -> Vec<BuildablePlugin> {
    let plugins_dir = project_root.join("plugins");
    if !plugins_dir.is_dir() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(&plugins_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                dir = %plugins_dir.display(),
                error = %e,
                "Failed to read plugins/ directory"
            );
            return Vec::new();
        }
    };

    let mut buildable = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let cargo_path = path.join("Cargo.toml");
        let channel_path = path.join("channel.toml");

        if !channel_path.exists() {
            continue;
        }

        // Parse channel.toml.
        let manifest = match plugin::PluginManifest::load(&channel_path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    path = %channel_path.display(),
                    error = %e,
                    "Skipping plugin with invalid channel.toml"
                );
                continue;
            }
        };

        let is_rust = cargo_path.exists();

        // Non-Rust plugins need a build_command in channel.toml.
        if !is_rust && manifest.build_command.is_none() {
            continue;
        }

        // Extract binary name: from Cargo.toml for Rust, or dir name for others.
        let binary_name = if is_rust {
            extract_binary_name(&cargo_path)
                .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string())
        } else {
            // For non-Rust plugins, use command field or dir name.
            manifest
                .command
                .clone()
                .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string())
        };

        let dir_name = entry.file_name().to_string_lossy().to_string();

        buildable.push(BuildablePlugin {
            dir_name,
            source_dir: path,
            manifest,
            binary_name,
            is_rust,
        });
    }

    buildable
}

/// Extract the binary name from a Cargo.toml file.
///
/// Checks `[[bin]]` entries first (uses the first one's `name`), then
/// falls back to `[package].name`.
fn extract_binary_name(cargo_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(cargo_path).ok()?;
    let doc: toml::Value = toml::from_str(&content).ok()?;

    // Check [[bin]] entries first.
    if let Some(bins) = doc.get("bin").and_then(|b| b.as_array()) {
        if let Some(first_bin) = bins.first() {
            if let Some(name) = first_bin.get("name").and_then(|n| n.as_str()) {
                return Some(name.to_string());
            }
        }
    }

    // Fall back to [package].name.
    doc.get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
}

/// Outcome of building a single plugin.
struct BuildResult {
    name: String,
    #[allow(dead_code)]
    dir_name: String,
    success: bool,
    #[allow(dead_code)]
    binary_path: Option<PathBuf>,
    installed_dir: Option<PathBuf>,
    error_msg: Option<String>,
    binary_size: Option<u64>,
}

fn build_plugins(project_root: &Path, names: &[String], all: bool) -> anyhow::Result<()> {
    if names.is_empty() && !all {
        anyhow::bail!(
            "Specify plugin names to build, or use --all to build all plugins.\n\
             Usage:\n  ta plugin build discord           # build one plugin\n  \
             ta plugin build discord,slack      # build multiple\n  \
             ta plugin build --all              # build all in plugins/"
        );
    }

    let buildable = discover_buildable_plugins(project_root);

    if buildable.is_empty() {
        println!("No buildable plugins found in plugins/.");
        println!();
        println!("A buildable plugin is a subdirectory of plugins/ containing both:");
        println!("  - Cargo.toml (Rust project)");
        println!("  - channel.toml (plugin manifest)");
        return Ok(());
    }

    // Filter by requested names.
    let to_build: Vec<&BuildablePlugin> = if all {
        buildable.iter().collect()
    } else {
        let mut selected = Vec::new();
        for name in names {
            let found = buildable.iter().find(|p| {
                p.manifest.name == *name
                    || p.dir_name == *name
                    || p.dir_name == format!("ta-channel-{}", name)
            });
            match found {
                Some(p) => selected.push(p),
                None => {
                    let available: Vec<&str> =
                        buildable.iter().map(|p| p.manifest.name.as_str()).collect();
                    anyhow::bail!(
                        "Plugin '{}' not found in plugins/.\n\
                         Available plugins: {}",
                        name,
                        available.join(", ")
                    );
                }
            }
        }
        selected
    };

    println!(
        "Building {} plugin{}...",
        to_build.len(),
        if to_build.len() == 1 { "" } else { "s" }
    );
    println!();

    let install_base = project_root.join(".ta").join("plugins").join("channels");
    let mut results: Vec<BuildResult> = Vec::new();

    for plugin in &to_build {
        let build_kind = if plugin.is_rust { "Rust" } else { "custom" };
        println!(
            "  Building {} ({}/, {})...",
            plugin.manifest.name, plugin.dir_name, build_kind
        );

        // Determine build command: use channel.toml build_command, or default to cargo.
        let build_output = if let Some(ref build_cmd) = plugin.manifest.build_command {
            // Custom build command (non-Rust plugins).
            println!("    Running: {}", build_cmd);
            std::process::Command::new("sh")
                .args(["-c", build_cmd])
                .current_dir(&plugin.source_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
        } else {
            // Default: cargo build --release for Rust plugins.
            std::process::Command::new("cargo")
                .args(["build", "--release"])
                .current_dir(&plugin.source_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
        };

        match build_output {
            Ok(out) if out.status.success() => {
                // Install: copy files to .ta/plugins/channels/<name>/.
                let target_dir = install_base.join(&plugin.manifest.name);
                if let Err(e) = std::fs::create_dir_all(&target_dir) {
                    results.push(BuildResult {
                        name: plugin.manifest.name.clone(),
                        dir_name: plugin.dir_name.clone(),
                        success: false,
                        binary_path: None,
                        installed_dir: None,
                        error_msg: Some(format!(
                            "Failed to create install directory {}: {}",
                            target_dir.display(),
                            e
                        )),
                        binary_size: None,
                    });
                    continue;
                }

                if plugin.is_rust {
                    // Rust: find binary in target/release/.
                    let binary_path = plugin
                        .source_dir
                        .join("target")
                        .join("release")
                        .join(&plugin.binary_name);

                    if !binary_path.exists() {
                        results.push(BuildResult {
                            name: plugin.manifest.name.clone(),
                            dir_name: plugin.dir_name.clone(),
                            success: false,
                            binary_path: None,
                            installed_dir: None,
                            error_msg: Some(format!(
                                "Build succeeded but binary not found at {}",
                                binary_path.display()
                            )),
                            binary_size: None,
                        });
                        continue;
                    }

                    let binary_size = std::fs::metadata(&binary_path).ok().map(|m| m.len());
                    let installed_binary = target_dir.join(&plugin.binary_name);
                    let installed_manifest = target_dir.join("channel.toml");

                    let copy_result =
                        std::fs::copy(&binary_path, &installed_binary).and_then(|_| {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let perms = std::fs::Permissions::from_mode(0o755);
                                std::fs::set_permissions(&installed_binary, perms)?;
                            }
                            std::fs::copy(
                                plugin.source_dir.join("channel.toml"),
                                &installed_manifest,
                            )
                        });

                    match copy_result {
                        Ok(_) => {
                            println!("    Installed to {}/", target_dir.display());
                            results.push(BuildResult {
                                name: plugin.manifest.name.clone(),
                                dir_name: plugin.dir_name.clone(),
                                success: true,
                                binary_path: Some(installed_binary),
                                installed_dir: Some(target_dir),
                                error_msg: None,
                                binary_size,
                            });
                        }
                        Err(e) => {
                            results.push(BuildResult {
                                name: plugin.manifest.name.clone(),
                                dir_name: plugin.dir_name.clone(),
                                success: false,
                                binary_path: Some(binary_path),
                                installed_dir: None,
                                error_msg: Some(format!("Install failed: {}", e)),
                                binary_size,
                            });
                        }
                    }
                } else {
                    // Non-Rust: copy the entire plugin source directory.
                    let installed_manifest = target_dir.join("channel.toml");
                    match copy_plugin_dir(&plugin.source_dir, &target_dir) {
                        Ok(_) => {
                            // Ensure channel.toml is present.
                            if !installed_manifest.exists() {
                                let _ = std::fs::copy(
                                    plugin.source_dir.join("channel.toml"),
                                    &installed_manifest,
                                );
                            }
                            println!("    Installed to {}/", target_dir.display());
                            results.push(BuildResult {
                                name: plugin.manifest.name.clone(),
                                dir_name: plugin.dir_name.clone(),
                                success: true,
                                binary_path: None,
                                installed_dir: Some(target_dir),
                                error_msg: None,
                                binary_size: None,
                            });
                        }
                        Err(e) => {
                            results.push(BuildResult {
                                name: plugin.manifest.name.clone(),
                                dir_name: plugin.dir_name.clone(),
                                success: false,
                                binary_path: None,
                                installed_dir: None,
                                error_msg: Some(format!("Install failed: {}", e)),
                                binary_size: None,
                            });
                        }
                    }
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let last_lines: String = stderr
                    .lines()
                    .rev()
                    .take(10)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                let cmd_name = if plugin.manifest.build_command.is_some() {
                    "build command"
                } else {
                    "cargo build"
                };
                results.push(BuildResult {
                    name: plugin.manifest.name.clone(),
                    dir_name: plugin.dir_name.clone(),
                    success: false,
                    binary_path: None,
                    installed_dir: None,
                    error_msg: Some(format!("{} failed:\n{}", cmd_name, last_lines)),
                    binary_size: None,
                });
            }
            Err(e) => {
                let hint = if plugin.manifest.build_command.is_some() {
                    "Check that the build command is valid and its dependencies are installed."
                } else {
                    "Is cargo installed and on PATH?"
                };
                results.push(BuildResult {
                    name: plugin.manifest.name.clone(),
                    dir_name: plugin.dir_name.clone(),
                    success: false,
                    binary_path: None,
                    installed_dir: None,
                    error_msg: Some(format!("Failed to run build: {}. {}", e, hint)),
                    binary_size: None,
                });
            }
        }
    }

    // Summary.
    println!();
    let ok_count = results.iter().filter(|r| r.success).count();
    let fail_count = results.iter().filter(|r| !r.success).count();

    if ok_count > 0 {
        println!("Built successfully ({}):", ok_count);
        for r in results.iter().filter(|r| r.success) {
            let size_display = r
                .binary_size
                .map(format_binary_size)
                .unwrap_or_else(|| "?".to_string());
            println!(
                "  {} — {} ({})",
                r.name,
                r.installed_dir
                    .as_ref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default(),
                size_display
            );
        }
    }

    if fail_count > 0 {
        println!();
        println!("Failed ({}):", fail_count);
        for r in results.iter().filter(|r| !r.success) {
            println!(
                "  {} — {}",
                r.name,
                r.error_msg.as_deref().unwrap_or("unknown error")
            );
        }
    }

    if fail_count > 0 && ok_count == 0 {
        anyhow::bail!("All plugin builds failed.");
    } else if fail_count > 0 {
        anyhow::bail!(
            "{} of {} plugin builds failed. See errors above.",
            fail_count,
            results.len()
        );
    }

    Ok(())
}

/// Format a binary size in human-readable form.
fn format_binary_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Copy plugin directory contents (for non-Rust plugins), skipping build artifacts.
fn copy_plugin_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip common build artifact directories.
        if matches!(
            name.as_str(),
            "target" | "node_modules" | "__pycache__" | ".git" | "dist" | "build"
        ) {
            continue;
        }

        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_plugin_dir(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ── Check command (v0.10.16) ───────────────────────────────────────────

fn check_plugins(project_root: &Path) -> anyhow::Result<()> {
    let plugins = plugin::discover_plugins(project_root);
    let cli_version = env!("CARGO_PKG_VERSION");

    if plugins.is_empty() {
        println!("No channel plugins installed to check.");
        return Ok(());
    }

    println!(
        "Checking {} plugin{} against daemon v{}...",
        plugins.len(),
        if plugins.len() == 1 { "" } else { "s" },
        cli_version
    );
    println!();

    let mut warn_count = 0;

    for p in &plugins {
        let m = &p.manifest;
        let mut issues: Vec<String> = Vec::new();

        // Check min_daemon_version compatibility.
        if let Some(ref min_ver) = m.min_daemon_version {
            if version_less_than(cli_version, min_ver) {
                issues.push(format!(
                    "requires daemon >= {}, but current version is {}",
                    min_ver, cli_version
                ));
            }
        }

        // Check if a newer version exists in plugins/ source directory.
        let source_dir = project_root
            .join("plugins")
            .join(format!("ta-channel-{}", m.name));
        if source_dir.is_dir() {
            let source_manifest = source_dir.join("channel.toml");
            if let Ok(source) = plugin::PluginManifest::load(&source_manifest) {
                if source.version != m.version {
                    issues.push(format!(
                        "installed v{}, source has v{} — run: ta plugin upgrade {}",
                        m.version, source.version, m.name
                    ));
                }
            }
        }

        if issues.is_empty() {
            println!("  [ok]   {} v{}", m.name, m.version);
        } else {
            for issue in &issues {
                println!("  [WARN] {} v{} — {}", m.name, m.version, issue);
            }
            warn_count += issues.len();
        }
    }

    println!();
    if warn_count > 0 {
        println!(
            "Check complete: {} warning{}.",
            warn_count,
            if warn_count == 1 { "" } else { "s" }
        );
    } else {
        println!("All plugins are up to date.");
    }

    Ok(())
}

/// Simple semver-like version comparison. Returns true if `a < b`.
fn version_less_than(a: &str, b: &str) -> bool {
    // Strip -alpha or other pre-release suffixes for comparison.
    let normalize = |v: &str| -> Vec<u32> {
        v.split('-')
            .next()
            .unwrap_or(v)
            .split('.')
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    };
    let va = normalize(a);
    let vb = normalize(b);
    va < vb
}

fn upgrade_plugin(project_root: &Path, name: &str) -> anyhow::Result<()> {
    // Check if plugin is installed.
    let plugins = plugin::discover_plugins(project_root);
    let installed = plugins.iter().find(|p| p.manifest.name == name);

    if installed.is_none() {
        anyhow::bail!(
            "Plugin '{}' is not installed. Install it first with: ta plugin install <path>",
            name
        );
    }
    let installed = installed.unwrap();

    // Check for source_url hint.
    if let Some(ref url) = installed.manifest.source_url {
        println!("Plugin '{}' has source URL: {}", name, url);
        println!("  Fetch the latest version from this URL and run:");
        println!("    ta plugin install <path>");
        println!();
    }

    // Try rebuilding from source.
    let source_dir = project_root
        .join("plugins")
        .join(format!("ta-channel-{}", name));
    if !source_dir.is_dir() {
        // Also try just the name.
        let alt_dir = project_root.join("plugins").join(name);
        if !alt_dir.is_dir() {
            anyhow::bail!(
                "No source directory found for plugin '{}'. Expected:\n  \
                 plugins/ta-channel-{}/  or  plugins/{}/\n\
                 If the plugin was installed from an external source, \
                 use 'ta plugin install <path>' to update it.",
                name,
                name,
                name
            );
        }
    }

    println!(
        "Upgrading plugin '{}' (v{} → rebuild from source)...",
        name, installed.manifest.version
    );

    // Delegate to build --all for this specific plugin.
    build_plugins(project_root, &[name.to_string()], false)?;

    println!();
    println!("Plugin '{}' upgraded successfully.", name);

    Ok(())
}

/// Check if a program exists on PATH (simple which-like check).
fn which_program(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let full = dir.join(program);
                full.is_file()
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn which_finds_sh() {
        // sh should be available on all unix systems.
        assert!(which_program("sh"));
    }

    #[test]
    fn which_does_not_find_nonexistent() {
        assert!(!which_program("definitely-not-a-real-command-12345"));
    }

    #[test]
    fn list_empty_project() {
        let dir = tempfile::tempdir().unwrap();
        let result = list_plugins(dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_empty_project() {
        let dir = tempfile::tempdir().unwrap();
        let result = validate_plugins(dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn discover_buildable_empty() {
        let dir = tempfile::tempdir().unwrap();
        let buildable = discover_buildable_plugins(dir.path());
        assert!(buildable.is_empty());
    }

    #[test]
    fn discover_buildable_finds_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("ta-channel-test");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        std::fs::write(
            plugin_dir.join("Cargo.toml"),
            r#"
[package]
name = "ta-channel-test"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "ta-channel-test"
path = "src/main.rs"
"#,
        )
        .unwrap();

        std::fs::write(
            plugin_dir.join("channel.toml"),
            r#"
name = "test"
command = "ta-channel-test"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let buildable = discover_buildable_plugins(dir.path());
        assert_eq!(buildable.len(), 1);
        assert_eq!(buildable[0].manifest.name, "test");
        assert_eq!(buildable[0].binary_name, "ta-channel-test");
        assert_eq!(buildable[0].dir_name, "ta-channel-test");
        assert!(buildable[0].is_rust);
    }

    #[test]
    fn discover_buildable_finds_non_rust_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("ta-channel-python");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        // No Cargo.toml — this is a Python plugin with build_command.
        std::fs::write(
            plugin_dir.join("channel.toml"),
            r#"
name = "python-plugin"
command = "python3"
args = ["-u", "channel_plugin.py"]
protocol = "json-stdio"
build_command = "pip install -e ."
"#,
        )
        .unwrap();

        std::fs::write(plugin_dir.join("channel_plugin.py"), "print('hello')").unwrap();

        let buildable = discover_buildable_plugins(dir.path());
        assert_eq!(buildable.len(), 1);
        assert_eq!(buildable[0].manifest.name, "python-plugin");
        assert!(!buildable[0].is_rust);
        assert_eq!(
            buildable[0].manifest.build_command.as_deref(),
            Some("pip install -e .")
        );
    }

    #[test]
    fn discover_skips_non_rust_without_build_command() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("no-build");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        // No Cargo.toml AND no build_command → not buildable.
        std::fs::write(
            plugin_dir.join("channel.toml"),
            r#"
name = "no-build"
command = "node"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let buildable = discover_buildable_plugins(dir.path());
        assert!(buildable.is_empty());
    }

    #[test]
    fn discover_buildable_skips_incomplete() {
        let dir = tempfile::tempdir().unwrap();

        // Directory with Cargo.toml but no channel.toml → not buildable.
        let cargo_only = dir.path().join("plugins").join("cargo-only");
        std::fs::create_dir_all(&cargo_only).unwrap();
        std::fs::write(
            cargo_only.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"",
        )
        .unwrap();

        // Directory with channel.toml but no Cargo.toml and no build_command → not buildable.
        let channel_only = dir.path().join("plugins").join("channel-only");
        std::fs::create_dir_all(&channel_only).unwrap();
        std::fs::write(
            channel_only.join("channel.toml"),
            "name = \"x\"\ncommand = \"x\"\nprotocol = \"json-stdio\"",
        )
        .unwrap();

        let buildable = discover_buildable_plugins(dir.path());
        assert!(buildable.is_empty());
    }

    #[test]
    fn extract_binary_name_from_bin_section() {
        let dir = tempfile::tempdir().unwrap();
        let cargo_path = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo_path,
            r#"
[package]
name = "my-crate"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "my-binary"
path = "src/main.rs"
"#,
        )
        .unwrap();

        assert_eq!(
            extract_binary_name(&cargo_path),
            Some("my-binary".to_string())
        );
    }

    #[test]
    fn extract_binary_name_fallback_to_package() {
        let dir = tempfile::tempdir().unwrap();
        let cargo_path = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo_path,
            "[package]\nname = \"fallback-name\"\nversion = \"0.1.0\"\nedition = \"2021\"",
        )
        .unwrap();

        assert_eq!(
            extract_binary_name(&cargo_path),
            Some("fallback-name".to_string())
        );
    }

    #[test]
    fn build_requires_names_or_all() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_plugins(dir.path(), &[], false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--all"));
    }

    #[test]
    fn build_unknown_plugin_errors() {
        let dir = tempfile::tempdir().unwrap();

        // Create a valid buildable plugin so discovery finds something.
        let plugin_dir = dir.path().join("plugins").join("ta-channel-real");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("Cargo.toml"),
            "[package]\nname = \"ta-channel-real\"\nversion = \"0.1.0\"\nedition = \"2021\"",
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("channel.toml"),
            "name = \"real\"\ncommand = \"ta-channel-real\"\nprotocol = \"json-stdio\"",
        )
        .unwrap();

        let result = build_plugins(dir.path(), &["nonexistent".to_string()], false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("nonexistent"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn build_name_resolution_by_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("plugins").join("ta-channel-discord");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("Cargo.toml"),
            "[package]\nname = \"ta-channel-discord\"\nversion = \"0.1.0\"\nedition = \"2021\"",
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("channel.toml"),
            "name = \"discord\"\ncommand = \"ta-channel-discord\"\nprotocol = \"json-stdio\"",
        )
        .unwrap();

        let buildable = discover_buildable_plugins(dir.path());
        assert_eq!(buildable.len(), 1);

        // Resolves by manifest name "discord".
        let found = buildable.iter().find(|p| {
            p.manifest.name == "discord"
                || p.dir_name == "discord"
                || p.dir_name == format!("ta-channel-{}", "discord")
        });
        assert!(found.is_some());

        // Also resolves by full dir name.
        let found2 = buildable.iter().find(|p| {
            p.manifest.name == "ta-channel-discord"
                || p.dir_name == "ta-channel-discord"
                || p.dir_name == format!("ta-channel-{}", "ta-channel-discord")
        });
        assert!(found2.is_some());
    }

    #[test]
    fn format_binary_size_mb() {
        assert_eq!(format_binary_size(5_242_880), "5.0 MB");
    }

    #[test]
    fn format_binary_size_kb() {
        assert_eq!(format_binary_size(10_240), "10 KB");
    }

    #[test]
    fn format_binary_size_bytes() {
        assert_eq!(format_binary_size(512), "512 B");
    }

    #[test]
    fn version_less_than_basic() {
        assert!(version_less_than("0.10.0-alpha", "0.10.1-alpha"));
        assert!(version_less_than("0.9.0-alpha", "0.10.0-alpha"));
        assert!(!version_less_than("0.10.1-alpha", "0.10.0-alpha"));
        assert!(!version_less_than("0.10.0-alpha", "0.10.0-alpha"));
    }

    #[test]
    fn version_less_than_major() {
        assert!(version_less_than("0.10.0", "1.0.0"));
        assert!(!version_less_than("1.0.0", "0.10.0"));
    }

    #[test]
    fn check_plugins_empty() {
        let dir = tempfile::tempdir().unwrap();
        let result = check_plugins(dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn upgrade_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let result = upgrade_plugin(dir.path(), "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }
}
