// tools.rs — `ta tools` command: list and install optional external tools.
//
// Built-in tools are registered in this file as a static EXTERNAL_TOOLS list
// (used as-is by the onboard wizard's Step 4 checkbox list, which predates
// any project context and only ever offers the built-ins). Adding a
// *built-in* tool requires an entry in EXTERNAL_TOOLS below.
//
// Community-authored tools (v0.17.0.12.14, Plugin category §2.2) don't
// require a TA core PR: `ta tools list`/`ta tools install` additionally
// discover `.ta/plugins/tool/<name>/plugin.toml` manifests via
// `ta_plugin::discover_plugins`, using a `[tool]` extension table for the
// fields (label/detect_command/install_hint/install) that aren't part of
// the shared `PluginManifest` schema.

use std::path::Path;

use anyhow::Result;
use clap::Subcommand;
use serde::Deserialize;

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

// ---------------------------------------------------------------------------
// Community-authored tools (Plugin category, §2.2) — `.ta/plugins/tool/<name>/plugin.toml`
// ---------------------------------------------------------------------------

/// A community-authored tool, discovered from a `.ta/plugins/tool/<name>/plugin.toml`
/// manifest rather than compiled into `EXTERNAL_TOOLS`.
struct DiscoveredTool {
    name: String,
    label: String,
    description: String,
    detect_command: String,
    install_hint: String,
    install: ToolInstallSpec,
}

/// The `[tool]` extension table parsed alongside the shared `PluginManifest`
/// fields (name/type/command/description) for a `kind = "tool"` manifest.
#[derive(Debug, Clone, Deserialize)]
struct ToolManifestExtra {
    tool: ToolExtraFields,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolExtraFields {
    label: String,
    #[serde(default)]
    description: Option<String>,
    detect_command: String,
    install_hint: String,
    install: ToolInstallSpec,
}

/// Same shape as `ExternalToolInstall`, but with owned fields since it comes
/// from a parsed TOML manifest rather than a `&'static` compile-time const.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ToolInstallSpec {
    Cargo { package: String },
    Npm { package: String },
    Git { url: String, dest: String },
    ClaudePlugin { spec: String },
}

/// Discover community-authored tools from `.ta/plugins/tool/<name>/plugin.toml`
/// (project-local then user-global). Manifests whose `name` collides with a
/// built-in `EXTERNAL_TOOLS` entry are skipped — built-ins always win.
fn discover_community_tools(project_root: &Path) -> Vec<DiscoveredTool> {
    let mut tools = vec![];
    for discovered in ta_plugin::discover_plugins("tool", project_root) {
        if EXTERNAL_TOOLS
            .iter()
            .any(|t| t.name == discovered.manifest.name)
        {
            continue;
        }
        let manifest_path = discovered.plugin_dir.join("plugin.toml");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(extra) = toml::from_str::<ToolManifestExtra>(&text) else {
            continue;
        };
        tools.push(DiscoveredTool {
            name: discovered.manifest.name,
            label: extra.tool.label,
            description: extra
                .tool
                .description
                .or(discovered.manifest.description)
                .unwrap_or_default(),
            detect_command: extra.tool.detect_command,
            install_hint: extra.tool.install_hint,
            install: extra.tool.install,
        });
    }
    tools
}

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
    let project_root = std::env::current_dir().unwrap_or_default();
    let community_tools = discover_community_tools(&project_root);

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
    for tool in &community_tools {
        let installed = check_detect_command(&tool.detect_command);
        let status = if installed {
            "✓ installed"
        } else {
            "✗ not found"
        };
        println!("  {:<20} {} (community)", tool.name, status);
        println!("  {:<20} {}", "", tool.description);
        println!("  {:<20} {}", "", tool.install_hint);
        println!();
    }
    if EXTERNAL_TOOLS.iter().any(|t| !check_tool_installed(t))
        || community_tools
            .iter()
            .any(|t| !check_detect_command(&t.detect_command))
    {
        println!("Install missing tools with:  ta tools install <name>");
    }
    Ok(())
}

/// Returns true if the tool's detect_command succeeds.
pub fn check_tool_installed(tool: &ExternalTool) -> bool {
    check_detect_command(tool.detect_command)
}

/// Shared detect-command evaluation for both built-in (`ExternalTool`) and
/// community-authored (`DiscoveredTool`) tools.
fn check_detect_command(detect_command: &str) -> bool {
    if let Some(path) = detect_command.strip_prefix("test -d ") {
        let expanded = path.replace('~', &std::env::var("HOME").unwrap_or_default());
        return std::path::Path::new(&expanded).is_dir();
    }
    if let Some(plugin_name) = detect_command.strip_prefix("claude-plugin:") {
        return check_claude_plugin_installed(plugin_name);
    }
    let parts: Vec<&str> = detect_command.split_whitespace().collect();
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
    if let Some(tool) = EXTERNAL_TOOLS.iter().find(|t| t.name == name) {
        if check_tool_installed(tool) {
            println!("{} is already installed.", tool.label);
            return Ok(());
        }
        return match &tool.install {
            ExternalToolInstall::Cargo(crate_name) => {
                install_via_cargo(tool.label, crate_name, tool.install_hint)
            }
            ExternalToolInstall::Npm(pkg) => install_via_npm(tool.label, pkg, tool.install_hint),
            ExternalToolInstall::Git { url, dest } => {
                install_via_git(tool.label, url, dest, tool.install_hint)
            }
            ExternalToolInstall::ClaudePlugin(plugin_spec) => {
                install_via_claude_plugin(tool.name, tool.label, plugin_spec, tool.install_hint)
            }
        };
    }

    let project_root = std::env::current_dir().unwrap_or_default();
    let community_tools = discover_community_tools(&project_root);
    let tool = community_tools
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!("Unknown tool '{name}'. Run `ta tools list` to see available tools.")
        })?;

    if check_detect_command(&tool.detect_command) {
        println!("{} is already installed.", tool.label);
        return Ok(());
    }

    match &tool.install {
        ToolInstallSpec::Cargo { package } => {
            install_via_cargo(&tool.label, package, &tool.install_hint)
        }
        ToolInstallSpec::Npm { package } => {
            install_via_npm(&tool.label, package, &tool.install_hint)
        }
        ToolInstallSpec::Git { url, dest } => {
            install_via_git(&tool.label, url, dest, &tool.install_hint)
        }
        ToolInstallSpec::ClaudePlugin { spec } => {
            install_via_claude_plugin(&tool.name, &tool.label, spec, &tool.install_hint)
        }
    }
}

fn install_via_cargo(label: &str, crate_name: &str, install_hint: &str) -> Result<()> {
    if which::which("cargo").is_err() {
        anyhow::bail!(cargo_missing_message(
            crate_name,
            cfg!(target_os = "windows")
        ));
    }
    println!("Installing {label} via cargo install {crate_name}...");
    let status = std::process::Command::new("cargo")
        .args(["install", crate_name])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run cargo: {e}"))?;
    if !status.success() {
        anyhow::bail!("cargo install {crate_name} failed.\nTry manually: {install_hint}");
    }
    println!("{label} installed successfully.");
    Ok(())
}

fn install_via_npm(label: &str, pkg: &str, install_hint: &str) -> Result<()> {
    println!("Installing {label} via npm install -g {pkg}...");
    let status = std::process::Command::new("npm")
        .args(["install", "-g", pkg])
        .status()
        .map_err(|e| {
            anyhow::anyhow!("npm not found: {e}. Install Node.js from https://nodejs.org")
        })?;
    if !status.success() {
        anyhow::bail!("npm install failed.\nTry manually: {install_hint}");
    }
    println!("{label} installed successfully.");
    Ok(())
}

fn install_via_git(label: &str, url: &str, dest: &str, install_hint: &str) -> Result<()> {
    let expanded_dest = dest.replace('~', &std::env::var("HOME").unwrap_or_default());
    if std::path::Path::new(&expanded_dest).exists() {
        println!("{label} is already installed at {expanded_dest}.");
        return Ok(());
    }
    println!("Cloning {label} to {expanded_dest}...");
    let status = std::process::Command::new("git")
        .args(["clone", "--depth=1", url, &expanded_dest])
        .status()
        .map_err(|e| anyhow::anyhow!("git not found: {e}. Install git first."))?;
    if !status.success() {
        anyhow::bail!("git clone failed.\nTry manually: {install_hint}");
    }
    println!("{label} installed to {expanded_dest}.");
    Ok(())
}

fn install_via_claude_plugin(
    tool_name: &str,
    label: &str,
    plugin_spec: &str,
    install_hint: &str,
) -> Result<()> {
    if which::which("claude").is_err() {
        anyhow::bail!(claude_cli_missing_message(tool_name));
    }
    println!("Installing {label} via claude plugin install {plugin_spec}...");
    let status = std::process::Command::new("claude")
        .args(["plugin", "install", plugin_spec])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run claude plugin install: {e}"))?;
    if !status.success() {
        anyhow::bail!("claude plugin install failed.\nTry manually: {install_hint}");
    }
    println!("{label} installed successfully.");
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

    #[test]
    fn discovers_community_authored_tool_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/tool/widget-cli");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
name = "widget-cli"
type = "tool"
command = ""
description = "Community widget CLI"

[tool]
label = "Widget CLI"
detect_command = "widget --version"
install_hint = "cargo install widget-cli"

[tool.install]
kind = "cargo"
package = "widget-cli"
"#,
        )
        .unwrap();

        let tools = discover_community_tools(dir.path());
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "widget-cli");
        assert_eq!(tools[0].label, "Widget CLI");
        assert!(
            matches!(&tools[0].install, ToolInstallSpec::Cargo { package } if package == "widget-cli")
        );
    }

    #[test]
    fn community_tool_name_colliding_with_built_in_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join(".ta/plugins/tool/meridian");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            r#"
name = "meridian"
type = "tool"
command = ""

[tool]
label = "Fake Meridian"
detect_command = "true"
install_hint = "n/a"

[tool.install]
kind = "cargo"
package = "meridian"
"#,
        )
        .unwrap();

        let tools = discover_community_tools(dir.path());
        assert!(tools.is_empty());
    }
}
