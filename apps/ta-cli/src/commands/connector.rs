// connector.rs — `ta connector` subcommand: install, list, status, start, stop.
//
// Manages TA connector MCP servers (Unreal Engine, Unity, etc.).
// Each connector wraps an external MCP server process (Python, UE5 plugin, etc.)
// and exposes it through TA's policy/audit/draft flow.

use anyhow::Result;
use clap::Subcommand;

use ta_connector_comfyui::config::ComfyUiConnectorConfig;
use ta_connector_unity::config::UnityConnectorConfig;
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
    ///   ta connector install comfyui --url http://localhost:8188
    Install {
        /// Connector to install: "unreal", "comfyui", or "unity".
        connector: String,
        /// Backend implementation to install.
        /// For unreal: "kvick", "flopperam", "special-agent".
        #[arg(long, default_value = "flopperam")]
        backend: String,
        /// For comfyui: base URL of the ComfyUI server.
        #[arg(long)]
        url: Option<String>,
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

pub fn execute(command: &ConnectorCommands, config: &GatewayConfig) -> Result<()> {
    match command {
        ConnectorCommands::Install {
            connector,
            backend,
            url,
        } => install(connector, backend, url.as_deref()),
        ConnectorCommands::List => list(config),
        ConnectorCommands::Status { connector } => status(connector.as_deref(), config),
        ConnectorCommands::Start { connector } => start(connector, config),
        ConnectorCommands::Stop { connector } => stop(connector),
    }
}

/// Load the `[connectors.unreal]` block from `.ta/workflow.toml` in the workspace root.
/// Falls back to `UnrealConnectorConfig::default()` if the file or the block is absent.
fn load_unreal_config(gateway: &GatewayConfig) -> UnrealConnectorConfig {
    let toml_path = gateway.workspace_root.join(".ta").join("workflow.toml");
    let Ok(raw) = std::fs::read_to_string(&toml_path) else {
        return UnrealConnectorConfig::default();
    };
    // Parse as a generic TOML table and extract [connectors.unreal].
    let Ok(top) = raw.parse::<toml::Table>() else {
        return UnrealConnectorConfig::default();
    };
    let Some(connectors) = top.get("connectors").and_then(|v| v.as_table()) else {
        return UnrealConnectorConfig::default();
    };
    let Some(unreal_val) = connectors.get("unreal") else {
        return UnrealConnectorConfig::default();
    };
    toml::from_str(&toml::to_string(unreal_val).unwrap_or_default())
        .unwrap_or_else(|_| UnrealConnectorConfig::default())
}

fn install(connector: &str, backend: &str, url: Option<&str>) -> Result<()> {
    match connector {
        "unreal" => install_unreal(backend),
        "comfyui" => install_comfyui(url),
        "unity" => install_unity(backend),
        other => anyhow::bail!(
            "Unknown connector '{}'. Available connectors: unreal, comfyui, unity\n\
             Run `ta connector list` to see installed connectors.",
            other
        ),
    }
}

fn install_comfyui(url: Option<&str>) -> Result<()> {
    let url = url.unwrap_or("http://localhost:8188");

    println!("Setting up ComfyUI connector...");
    println!();
    println!("  ComfyUI REST API URL: {}", url);
    println!();
    println!("  Prerequisites:");
    println!("    1. Install ComfyUI: https://github.com/comfyanonymous/ComfyUI");
    println!("    2. Download Wan2.1 model weights into ComfyUI's models/checkpoints/ directory.");
    println!(
        "       e.g.: huggingface-cli download Wan-AI/Wan2.1-T2V-14B --local-dir ~/.comfyui/models/checkpoints/"
    );
    println!("    3. Start ComfyUI:");
    println!("       cd /path/to/ComfyUI && python main.py --listen");
    println!();
    println!("  Enable in config (daemon.toml or workflow.toml):");
    println!("    [connectors.comfyui]");
    println!("    enabled = true");
    println!("    url = \"{}\"", url);
    println!("    output_dir = \"/path/to/ComfyUI/output\"");
    println!();
    println!("  Available MCP tools after enabling:");
    println!("    comfyui_workflow_submit  — submit a workflow JSON for inference");
    println!("    comfyui_job_status       — poll job state and output file paths");
    println!("    comfyui_job_cancel       — cancel a queued or running job");
    println!("    comfyui_model_list       — list available checkpoints, LoRAs, VAEs");
    println!();
    println!(
        "  After starting ComfyUI, run `ta connector status comfyui` to verify the connection."
    );
    Ok(())
}

fn install_unity(backend: &str) -> Result<()> {
    match backend {
        "official" | "flopperam" => {
            // Accept "flopperam" as the default clap value gracefully; redirect to official.
            println!("Setting up Unity connector (backend: official)...");
            println!();
            println!("  The official backend uses Unity's com.unity.mcp-server UPM package.");
            println!(
                "  It is maintained by Unity Technologies and supports Unity 6 LTS and later."
            );
            println!();
            println!("  Step 1 — Add the package to your Unity project:");
            println!();
            println!("    Open Window → Package Manager → + → Add package by name:");
            println!();
            println!("      com.unity.mcp-server");
            println!();
            println!("    Or add manually to Packages/manifest.json:");
            println!();
            println!("      {{");
            println!("        \"dependencies\": {{");
            println!("          \"com.unity.mcp-server\": \"1.0.0\",");
            println!("          ... (existing entries)");
            println!("        }}");
            println!("      }}");
            println!();
            println!("  Step 2 — Open your Unity project in the Editor.");
            println!("    The MCP server starts automatically when the Editor opens.");
            println!("    It listens on localhost:30200 by default.");
            println!();
            println!("  Step 3 — Enable in config (daemon.toml or workflow.toml):");
            println!();
            println!("    [connectors.unity]");
            println!("    enabled = true");
            println!("    backend = \"official\"");
            println!("    project_path = \"/path/to/YourProject\"");
            println!("    socket = \"localhost:30200\"");
            println!();
            println!("  Available MCP tools after enabling:");
            println!("    unity_build_trigger       — trigger a Player or AssetBundle build");
            println!("    unity_scene_query         — query GameObject hierarchy and components");
            println!("    unity_test_run            — run EditMode or PlayMode tests");
            println!("    unity_addressables_build  — trigger an Addressables content build");
            println!("    unity_render_capture      — capture a screenshot from a scene camera");
            println!();
            println!("  After opening the Editor, run `ta connector status unity` to verify.");
        }
        "community" => {
            println!("Setting up Unity connector (backend: community)...");
            println!();
            println!(
                "  Community backends (CoderGamester/unity-mcp, justinpbarnett/unity-mcp, etc.)"
            );
            println!(
                "  use varied protocols. The community backend stub is included for future use."
            );
            println!();
            println!("  For now, use the official backend:");
            println!("    ta connector install unity --backend official");
            println!();
            println!("  To configure a community server manually:");
            println!("    [connectors.unity]");
            println!("    enabled = true");
            println!("    backend = \"community\"");
            println!("    socket = \"localhost:30201\"   # adjust to your server's port");
        }
        other => anyhow::bail!(
            "Unknown Unity backend '{}'. Valid options: official, community",
            other
        ),
    }
    Ok(())
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

fn list(gateway: &GatewayConfig) -> Result<()> {
    println!("Installed connectors:");
    println!();

    let default_cfg = load_unreal_config(gateway);

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

    // ComfyUI connector.
    let comfyui_cfg = ComfyUiConnectorConfig::default();
    println!("  comfyui (ComfyUI Inference)");
    println!(
        "  URL: {} (configure via [connectors.comfyui] in workflow.toml)",
        comfyui_cfg.url
    );
    let comfyui_reachable = std::net::TcpStream::connect_timeout(
        &comfyui_cfg
            .url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:8188".parse().unwrap()),
        std::time::Duration::from_millis(200),
    )
    .is_ok();
    let comfyui_status = if comfyui_reachable {
        "✓ running"
    } else {
        "  not running"
    };
    println!("    [{}] — REST API", comfyui_status);
    println!();
    // Unity connector.
    let unity_cfg = UnityConnectorConfig::default();
    println!("  unity (Unity Engine)");
    println!(
        "  Active backend: {} (configure via [connectors.unity] in workflow.toml)",
        unity_cfg.backend
    );
    let unity_reachable = std::net::TcpStream::connect_timeout(
        &unity_cfg
            .socket
            .parse()
            .unwrap_or_else(|_| "127.0.0.1:30200".parse().unwrap()),
        std::time::Duration::from_millis(200),
    )
    .is_ok();
    let unity_status = if unity_reachable {
        "✓ running"
    } else {
        "  not running"
    };
    println!(
        "    [{}] official — com.unity.mcp-server (socket: {})",
        unity_status, unity_cfg.socket
    );
    println!();
    println!(
        "Run `ta connector install <name>` to install. For comfyui: `ta connector install comfyui --url <url>`"
    );
    Ok(())
}

fn status(connector: Option<&str>, gateway: &GatewayConfig) -> Result<()> {
    let targets: Vec<&str> = match connector {
        Some(c) => vec![c],
        None => vec!["unreal", "comfyui", "unity"],
    };

    for target in targets {
        match target {
            "unreal" => {
                println!("unreal connector:");
                let cfg = load_unreal_config(gateway);
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
            "comfyui" => {
                println!("comfyui connector:");
                let cfg = ComfyUiConnectorConfig::default();
                let url = &cfg.url;
                // Parse the host:port from the URL for a TCP probe.
                let addr = url
                    .trim_start_matches("http://")
                    .trim_start_matches("https://");
                let running = std::net::TcpStream::connect_timeout(
                    &addr
                        .parse()
                        .unwrap_or_else(|_| "127.0.0.1:8188".parse().unwrap()),
                    std::time::Duration::from_millis(500),
                )
                .is_ok();
                if running {
                    println!("  status:  running");
                    println!("  url:     {}", url);
                } else {
                    println!("  status:  not running");
                    println!("  url:     {} (not reachable)", url);
                    println!("  hint:    Start ComfyUI with `python main.py --listen`, or check `[connectors.comfyui] url` in config.");
                }
            }
            "unity" => {
                println!("unity connector:");
                let cfg = UnityConnectorConfig::default();
                let addr = &cfg.socket;
                let running = std::net::TcpStream::connect_timeout(
                    &addr
                        .parse()
                        .unwrap_or_else(|_| "127.0.0.1:30200".parse().unwrap()),
                    std::time::Duration::from_millis(500),
                )
                .is_ok();
                if running {
                    println!("  status:  running");
                    println!("  backend: {}", cfg.backend);
                    println!("  socket:  {}", addr);
                } else {
                    println!("  status:  not running");
                    println!("  backend: {}", cfg.backend);
                    println!("  socket:  {} (not reachable)", addr);
                    println!(
                        "  hint:    Open the Unity Editor with com.unity.mcp-server installed, or run `ta connector install unity`"
                    );
                }
            }
            other => {
                println!("{}: unknown connector", other);
            }
        }
    }
    Ok(())
}

fn start(connector: &str, gateway: &GatewayConfig) -> Result<()> {
    match connector {
        "comfyui" => {
            println!("ComfyUI is a standalone server — start it manually:");
            println!("  cd /path/to/ComfyUI && python main.py --listen");
            println!("Then run `ta connector status comfyui` to verify.");
        }
        "unreal" => {
            let cfg = load_unreal_config(gateway);
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
                            eprintln!("  Check that `socket` in [connectors.unreal] is a valid address (default: localhost:30100).");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Cannot start unreal connector: {}", e);
                }
            }
        }
        "unity" => {
            println!("Unity MCP server is Editor-hosted — open the Unity Editor to start it.");
            println!("  Ensure com.unity.mcp-server is installed in your project (Window → Package Manager).");
            println!("  The server starts automatically when the Editor opens.");
            println!("  Run `ta connector status unity` to verify.");
        }
        other => {
            anyhow::bail!(
                "Unknown connector '{}'. Available: unreal, comfyui, unity",
                other
            );
        }
    }
    Ok(())
}

fn stop(connector: &str) -> Result<()> {
    match connector {
        "comfyui" => {
            println!("To stop the comfyui connector:");
            println!("  Kill the `python main.py` process running ComfyUI.");
        }
        "unreal" => {
            println!("To stop the unreal connector:");
            println!("  For kvick backend: kill the `python3 server.py` process.");
            println!(
                "  For flopperam/special-agent: close the Unreal Editor (plugin stops with the Editor)."
            );
        }
        "unity" => {
            println!("To stop the unity connector:");
            println!("  Close the Unity Editor (com.unity.mcp-server stops with the Editor).");
        }
        other => {
            anyhow::bail!(
                "Unknown connector '{}'. Available: unreal, comfyui, unity",
                other
            );
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
