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
}

/// All optional tools TA knows about.
///
/// To add a new tool: append an entry here and create
/// `plugins/<name>/plugin.toml` with `type = "external-tool"`.
pub const EXTERNAL_TOOLS: &[ExternalTool] = &[
    ExternalTool {
        name: "claude-flow",
        label: "claude-flow agent framework",
        description: "Multi-agent orchestration with swarm support (npm)",
        detect_command: "claude-flow --version",
        install_hint: "npm install -g claude-flow",
        install: ExternalToolInstall::Npm("claude-flow"),
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
    ///   ta tools install claude-flow
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
    }
    Ok(())
}
