// plugin.rs — `ta plugin` CLI commands for managing channel plugins.
//
// Provides:
//   - `ta plugin list` — show installed channel plugins with protocol, capabilities, validation
//   - `ta plugin install <path>` — install a plugin from a directory

use std::path::PathBuf;

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
}

pub fn run_plugin(project_root: &std::path::Path, command: &PluginCommands) -> anyhow::Result<()> {
    match command {
        PluginCommands::List => list_plugins(project_root),
        PluginCommands::Install { path, global } => install_plugin(project_root, path, *global),
        PluginCommands::Validate => validate_plugins(project_root),
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
}
