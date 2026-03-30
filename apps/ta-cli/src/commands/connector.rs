// connector.rs — `ta connector` subcommand: install, list, status, start, stop.
//
// Manages TA connector MCP servers (Unreal Engine, Unity, etc.).
// Each connector wraps an external MCP server process (Python, UE5 plugin, etc.)
// and exposes it through TA's policy/audit/draft flow.

use anyhow::Result;
use clap::Subcommand;

use ta_connector_unreal::{backends::make_backend, config::UnrealConnectorConfig};

use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum ConnectorCommands {
    /// Install a connector's backend MCP server.
    ///
    /// Downloads and installs the named backend into `~/.ta/mcp-servers/`.
    /// After installing, copy the plugin into your UE5 project (instructions are printed).
    ///
    /// Examples:
    ///   ta connector install unreal --backend flopperam
    ///   ta connector install unreal --backend kvick
    Install {
        /// Connector to install: "unreal" or "unity".
        connector: String,
        /// Backend implementation to install.
        /// For unreal: "kvick", "flopperam", "special-agent".
        #[arg(long, default_value = "flopperam")]
        backend: String,
    },
    /// List installed connectors and their current backend selection.
    List,
    /// Show whether a connector's MCP server process is running.
    Status {
        /// Connector name (e.g., "unreal", "unity"). Shows all if omitted.
        connector: Option<String>,
    },
    /// Start a connector's MCP server process.
    Start {
        /// Connector name (e.g., "unreal").
        connector: String,
    },
    /// Stop a connector's MCP server process.
    Stop {
        /// Connector name (e.g., "unreal").
        connector: String,
    },
}

pub fn execute(command: &ConnectorCommands, _config: &GatewayConfig) -> Result<()> {
    match command {
        ConnectorCommands::Install { connector, backend } => install(connector, backend),
        ConnectorCommands::List => list(),
        ConnectorCommands::Status { connector } => status(connector.as_deref()),
        ConnectorCommands::Start { connector } => start(connector),
        ConnectorCommands::Stop { connector } => stop(connector),
    }
}

fn install(connector: &str, backend: &str) -> Result<()> {
    match connector {
        "unreal" => install_unreal(backend),
        "unity" => {
            println!(
                "Unity connector is planned for v0.14.16. Use `ta connector install unreal` for now."
            );
            Ok(())
        }
        other => anyhow::bail!(
            "Unknown connector '{}'. Available connectors: unreal\n\
             Run `ta connector list` to see installed connectors.",
            other
        ),
    }
}

fn install_unreal(backend: &str) -> Result<()> {
    let install_dir = default_install_dir(backend, "unreal");
    println!("Installing Unreal connector (backend: {})...", backend);
    println!();

    match backend {
        "kvick" => {
            println!("  Backend: kvick-games/UnrealMCP (Python, simple scene ops)");
            println!("  Target:  {}", install_dir.display());
            println!();
            println!("  Manual install steps:");
            println!("    1. Clone the repo:");
            println!(
                "       git clone https://github.com/kvick-games/UnrealMCP \"{}\"",
                install_dir.display()
            );
            println!("    2. Install Python dependencies:");
            println!(
                "       cd \"{}\" && pip install -r requirements.txt",
                install_dir.display()
            );
            println!("    3. Enable in config (daemon.toml or workflow.toml):");
            println!("       [connectors.unreal]");
            println!("       enabled = true");
            println!("       backend = \"kvick\"");
            println!("       [connectors.unreal.backends.kvick]");
            println!("       install_path = \"{}\"", install_dir.display());
        }
        "flopperam" => {
            println!("  Backend: flopperam/unreal-engine-mcp (C++ UE5 plugin, MRQ/Sequencer)");
            println!("  Target:  {}", install_dir.display());
            println!();
            println!("  Manual install steps:");
            println!("    1. Clone the repo:");
            println!(
                "       git clone https://github.com/flopperam/unreal-engine-mcp \"{}\"",
                install_dir.display()
            );
            println!("    2. Copy the plugin into your UE5 project:");
            println!(
                "       cp -r \"{}/MCPPlugin\" \"/path/to/YourGame/Plugins/MCPPlugin\"",
                install_dir.display()
            );
            println!("    3. Rebuild your UE5 project (plugin auto-compiles on Editor launch).");
            println!("    4. Enable in config:");
            println!("       [connectors.unreal]");
            println!("       enabled = true");
            println!("       backend = \"flopperam\"");
            println!("       ue_project_path = \"/path/to/YourGame/YourGame.uproject\"");
            println!("       [connectors.unreal.backends.flopperam]");
            println!("       install_path = \"{}\"", install_dir.display());
        }
        "special-agent" => {
            println!(
                "  Backend: ArtisanGameworks/SpecialAgentPlugin (71+ tools, environment-building)"
            );
            println!("  Target:  {}", install_dir.display());
            println!();
            println!("  Manual install steps:");
            println!("    1. Clone the repo:");
            println!(
                "       git clone https://github.com/ArtisanGameworks/SpecialAgentPlugin \"{}\"",
                install_dir.display()
            );
            println!("    2. Copy the plugin into your UE5 project Plugins/ directory.");
            println!("    3. Rebuild your UE5 project.");
            println!("    4. Enable in config:");
            println!("       [connectors.unreal]");
            println!("       enabled = true");
            println!("       backend = \"special-agent\"");
            println!("       [connectors.unreal.backends.special-agent]");
            println!("       install_path = \"{}\"", install_dir.display());
        }
        other => anyhow::bail!(
            "Unknown Unreal backend '{}'. Valid options: kvick, flopperam, special-agent",
            other
        ),
    }

    println!();
    println!("After installing, run `ta connector status unreal` to verify the connection.");
    Ok(())
}

fn list() -> Result<()> {
    println!("Installed connectors:");
    println!();

    // Check unreal backends using defaults.
    let default_cfg = UnrealConnectorConfig::default();

    let backends = [
        (
            "kvick",
            &default_cfg.backends.kvick.install_path,
            "kvick-games/UnrealMCP",
        ),
        (
            "flopperam",
            &default_cfg.backends.flopperam.install_path,
            "flopperam/unreal-engine-mcp",
        ),
        (
            "special-agent",
            &default_cfg.backends.special_agent.install_path,
            "ArtisanGameworks/SpecialAgentPlugin",
        ),
    ];

    println!("  unreal (Unreal Engine 5)");
    println!(
        "  Active backend: {} (default; configure via [connectors.unreal] in workflow.toml)",
        default_cfg.backend
    );
    println!();

    for (name, _install_path, source) in &backends {
        let install_dir = default_install_dir(name, "unreal");
        let installed = install_dir.exists();
        let status_marker = if installed {
            "✓ installed"
        } else {
            "  not installed"
        };
        println!("    [{status_marker}] {name:<14} — {source}");
        if installed {
            println!(
                "                       install_path: {}",
                install_dir.display()
            );
        }
    }

    println!();
    println!("  unity (Unity Engine)");
    println!("    [ not installed] — planned for v0.14.16");
    println!();
    println!("Run `ta connector install <name> --backend <backend>` to install.");
    Ok(())
}

fn status(connector: Option<&str>) -> Result<()> {
    let targets: Vec<&str> = match connector {
        Some(c) => vec![c],
        None => vec!["unreal"],
    };

    for target in targets {
        match target {
            "unreal" => {
                println!("unreal connector:");
                let cfg = UnrealConnectorConfig::default();
                let addr = &cfg.socket;
                // Try a TCP connection to see if the MCP server is listening.
                let running = std::net::TcpStream::connect(addr).is_ok();
                if running {
                    println!("  status:  running");
                    println!("  socket:  {}", addr);
                } else {
                    println!("  status:  not running");
                    println!("  socket:  {} (not reachable)", addr);
                    println!("  hint:    Start the Unreal Editor with the plugin enabled, or run `ta connector start unreal`");
                }
            }
            other => {
                println!("{}: unknown connector", other);
            }
        }
    }
    Ok(())
}

fn start(connector: &str) -> Result<()> {
    match connector {
        "unreal" => {
            let cfg = UnrealConnectorConfig::default();
            match make_backend(&cfg) {
                Ok(backend) => {
                    println!(
                        "Starting {} backend for unreal connector...",
                        backend.name()
                    );
                    match backend.spawn() {
                        Ok(handle) => {
                            if handle.pid > 0 {
                                println!("  Started (pid {})", handle.pid);
                            } else {
                                println!(
                                    "  Backend is Editor-hosted — launch the Unreal Editor to start it."
                                );
                            }
                            println!("  Listening on: {}", handle.socket_addr);
                        }
                        Err(e) => {
                            eprintln!("  Failed to start: {}", e);
                            eprintln!(
                                "  Run `ta connector install unreal` for setup instructions."
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Cannot start unreal connector: {}", e);
                }
            }
        }
        other => {
            anyhow::bail!("Unknown connector '{}'. Available: unreal", other);
        }
    }
    Ok(())
}

fn stop(connector: &str) -> Result<()> {
    match connector {
        "unreal" => {
            println!("To stop the unreal connector:");
            println!("  For kvick backend: kill the `python3 server.py` process.");
            println!(
                "  For flopperam/special-agent: close the Unreal Editor (plugin stops with the Editor)."
            );
        }
        other => {
            anyhow::bail!("Unknown connector '{}'. Available: unreal", other);
        }
    }
    Ok(())
}

fn default_install_dir(backend: &str, connector: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home)
        .join(".ta")
        .join("mcp-servers")
        .join(format!("{}-{}", connector, backend))
}
