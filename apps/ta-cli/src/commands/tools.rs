// tools.rs — `ta tools` command: list and install optional external tools.
//
// Optional tools are registered in this file as a static EXTERNAL_TOOLS list.
// Adding a new tool requires:
//   1. An entry in EXTERNAL_TOOLS below
//   2. A corresponding plugins/<name>/plugin.toml manifest (documentation)
//
// The onboard wizard (onboard.rs) imports EXTERNAL_TOOLS to populate its
// Step 4 "Optional Components" checkbox list dynamically.

use anyhow::Result;
use clap::Subcommand;

/// An optional external tool that TA can use but does not require.
pub struct ExternalTool {
    /// Short slug used as the install key, e.g. "meridian"
    pub name: &'static str,
    /// Display label shown in TUI and CLI, e.g. "Meridian KPI analytics"
    pub label: &'static str,
    /// One-line description
    pub description: &'static str,
    /// Command to probe for presence (exit 0 = installed).
    /// Use "test -d <path>" for directory-based installs (e.g. BMAD).
    pub detect_command: &'static str,
    /// Human-readable install hint printed when tool is missing
    pub install_hint: &'static str,
    /// Programmatic install method
    pub install: ExternalToolInstall,
}

/// How to install an optional external tool.
#[allow(dead_code)]
pub enum ExternalToolInstall {
    /// `cargo install <crate>`
    Cargo(&'static str),
    /// `npm install -g <package>`
    Npm(&'static str),
    /// `git clone --depth=1 <url> <dest>`
    Git {
        url: &'static str,
        dest: &'static str,
    },
    /// `claude plugin install <plugin-spec>` (Claude Code plugin registry)
    ClaudePlugin(&'static str),
}

/// All optional tools TA knows about.
///
/// To add a new tool: append an entry here and create
/// `plugins/<name>/plugin.toml` with `type = "external-tool"`.
pub const EXTERNAL_TOOLS: &[ExternalTool] = &[
    ExternalTool {
        name: "superpowers",
        label: "Superpowers Claude Code plugin",
        description: "Agent skills and orchestration plugin for Claude Code",
        detect_command: "claude-plugin:superpowers",
        install_hint: "claude plugin install superpowers@superpowers-dev",
        install: ExternalToolInstall::ClaudePlugin("superpowers@superpowers-dev"),
    },
    ExternalTool {
        name: "bmad",
        label: "BMAD planning library",
        description: "Structured multi-role planning: Analyst, Architect, PM roles",
        detect_command: "test -d ~/.bmad/agents",
        install_hint: "git clone --depth=1 https://github.com/bmadcode/bmad-method ~/.bmad",
        install: ExternalToolInstall::Git {
            url: "https://github.com/bmadcode/bmad-method",
            dest: "~/.bmad",
        },
    },
    ExternalTool {
        name: "meridian",
        label: "Meridian KPI analytics",
        description: "Velocity reports, cost-per-phase, and goal-alignment scoring (cargo)",
        detect_command: "meridian --version",
        install_hint: "cargo install meridian",
        install: ExternalToolInstall::Cargo("meridian"),
    },
];

#[derive(Debug, Subcommand)]
pub enum ToolsCommands {
    /// Show all optional tools and whether they are installed.
    ///
    /// Example:
    ///   ta tools list
    #[command(alias = "check")]
    List,

    /// Install a specific optional tool by name.
    ///
    /// Example:
    ///   ta tools install meridian
    ///   ta tools install superpowers
    ///   ta tools install bmad
    Install {
        /// Tool name (run `ta tools list` to see available names)
        name: String,
    },
}

pub fn execute(command: &ToolsCommands) -> Result<()> {
    match command {
        ToolsCommands::List => list_tools(),
        ToolsCommands::Install { name } => install_tool(name),
    }
}

fn list_tools() -> Result<()> {
    println!("Optional tools for Trusted Autonomy:\n");
    for tool in EXTERNAL_TOOLS {
        let installed = check_tool_installed(tool);
        let status = if installed {
            "✓ installed"
        } else {
            "✗ not found"
        };
        println!("  {:<20} {}", tool.name, status);
        println!("  {:<20} {}", "", tool.description);
        println!("  {:<20} {}", "", tool.install_hint);
        println!();
    }
    if EXTERNAL_TOOLS.iter().any(|t| !check_tool_installed(t)) {
        println!("Install missing tools with:  ta tools install <name>");
    }
    Ok(())
}

/// Returns true if the tool's detect_command succeeds.
pub fn check_tool_installed(tool: &ExternalTool) -> bool {
    if let Some(path) = tool.detect_command.strip_prefix("test -d ") {
        let expanded = path.replace('~', &std::env::var("HOME").unwrap_or_default());
        return std::path::Path::new(&expanded).is_dir();
    }
    if let Some(plugin_name) = tool.detect_command.strip_prefix("claude-plugin:") {
        return check_claude_plugin_installed(plugin_name);
    }
    let parts: Vec<&str> = tool.detect_command.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }
    std::process::Command::new(parts[0])
        .args(&parts[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check whether a Claude Code plugin is installed by querying `claude plugin list`.
///
/// Returns false if `claude` is not on PATH or the plugin name is not in the output.
fn check_claude_plugin_installed(plugin_name: &str) -> bool {
    let output = std::process::Command::new("claude")
        .args(["plugin", "list"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.contains(plugin_name)
        }
        Err(_) => false,
    }
}

/// Build the "cargo not found" guidance message (v0.17.0.12.8).
///
/// On Windows, `cargo install` commonly fails with a confusing linker error when
/// the Rust toolchain (or the MSVC Build Tools it needs for linking) isn't
/// installed. Front-load that guidance instead of letting the raw cargo error
/// surface. Non-Windows platforms get the plain rustup pointer.
fn cargo_missing_message(crate_name: &str, is_windows: bool) -> String {
    if is_windows {
        format!(
            "cargo not found on PATH. Install the Rust toolchain first:\n\
             \x20 1. Install rustup: https://rustup.rs (or `winget install Rustlang.Rustup`)\n\
             \x20 2. Install the MSVC C++ Build Tools (required by the linker):\n\
             \x20    https://visualstudio.microsoft.com/visual-cpp-build-tools/\n\
             \x20 3. Restart your terminal, then re-run: ta tools install {crate_name}"
        )
    } else {
        format!(
            "cargo not found on PATH. Install the Rust toolchain first: https://rustup.rs\n\
             Then re-run: ta tools install {crate_name}"
        )
    }
}

/// Build the "claude CLI not found" guidance message (v0.17.0.12.8).
fn claude_cli_missing_message(tool_name: &str) -> String {
    format!(
        "The `claude` CLI is not on PATH — install Claude Code first: https://claude.ai/code\n\
         Then re-run: ta tools install {tool_name}"
    )
}

pub fn install_tool(name: &str) -> Result<()> {
    let tool = EXTERNAL_TOOLS
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!("Unknown tool '{name}'. Run `ta tools list` to see available tools.")
        })?;

    if check_tool_installed(tool) {
        println!("{} is already installed.", tool.label);
        return Ok(());
    }

    match &tool.install {
        ExternalToolInstall::Cargo(crate_name) => {
            if which::which("cargo").is_err() {
                anyhow::bail!(cargo_missing_message(
                    crate_name,
                    cfg!(target_os = "windows")
                ));
            }
            println!(
                "Installing {} via cargo install {}...",
                tool.label, crate_name
            );
            let status = std::process::Command::new("cargo")
                .args(["install", crate_name])
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to run cargo: {e}"))?;
            if !status.success() {
                anyhow::bail!(
                    "cargo install {crate_name} failed.\nTry manually: {}",
                    tool.install_hint
                );
            }
            println!("{} installed successfully.", tool.label);
        }
        ExternalToolInstall::Npm(pkg) => {
            println!("Installing {} via npm install -g {}...", tool.label, pkg);
            let status = std::process::Command::new("npm")
                .args(["install", "-g", pkg])
                .status()
                .map_err(|e| {
                    anyhow::anyhow!("npm not found: {e}. Install Node.js from https://nodejs.org")
                })?;
            if !status.success() {
                anyhow::bail!("npm install failed.\nTry manually: {}", tool.install_hint);
            }
            println!("{} installed successfully.", tool.label);
        }
        ExternalToolInstall::Git { url, dest } => {
            let expanded_dest = dest.replace('~', &std::env::var("HOME").unwrap_or_default());
            if std::path::Path::new(&expanded_dest).exists() {
                println!("{} is already installed at {}.", tool.label, expanded_dest);
                return Ok(());
            }
            println!("Cloning {} to {}...", tool.label, expanded_dest);
            let status = std::process::Command::new("git")
                .args(["clone", "--depth=1", url, &expanded_dest])
                .status()
                .map_err(|e| anyhow::anyhow!("git not found: {e}. Install git first."))?;
            if !status.success() {
                anyhow::bail!("git clone failed.\nTry manually: {}", tool.install_hint);
            }
            println!("{} installed to {}.", tool.label, expanded_dest);
        }
        ExternalToolInstall::ClaudePlugin(plugin_spec) => {
            if which::which("claude").is_err() {
                anyhow::bail!(claude_cli_missing_message(tool.name));
            }
            println!(
                "Installing {} via claude plugin install {}...",
                tool.label, plugin_spec
            );
            let status = std::process::Command::new("claude")
                .args(["plugin", "install", plugin_spec])
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to run claude plugin install: {e}"))?;
            if !status.success() {
                anyhow::bail!(
                    "claude plugin install failed.\nTry manually: {}",
                    tool.install_hint
                );
            }
            println!("{} installed successfully.", tool.label);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_missing_message_windows_mentions_rustup_and_build_tools() {
        let msg = cargo_missing_message("meridian", true);
        assert!(msg.contains("rustup.rs"));
        assert!(msg.contains("Build Tools"));
        assert!(msg.contains("ta tools install meridian"));
    }

    #[test]
    fn cargo_missing_message_non_windows_omits_build_tools() {
        let msg = cargo_missing_message("meridian", false);
        assert!(msg.contains("rustup.rs"));
        assert!(!msg.contains("Build Tools"));
        assert!(msg.contains("ta tools install meridian"));
    }

    #[test]
    fn claude_cli_missing_message_is_actionable() {
        let msg = claude_cli_missing_message("superpowers");
        assert!(msg.contains("claude.ai/code"));
        assert!(msg.contains("ta tools install superpowers"));
    }

    #[test]
    fn external_tools_includes_meridian() {
        assert!(EXTERNAL_TOOLS.iter().any(|t| t.name == "meridian"));
    }
}
