//! # ta-daemon
//!
//! Trusted Autonomy MCP server daemon and HTTP API.
//!
//! Starts an MCP server on stdio that Claude Code (or any MCP client)
//! connects to. All agent tool calls flow through the gateway's policy
//! engine, staging workspace, and audit log.
//!
//! Additionally serves a full HTTP API (v0.9.7) that any interface
//! (terminal, web, Discord, Slack, email) can connect to for commands,
//! agent conversations, and event streams.
//!
//! ## Usage
//!
//! MCP mode (default — started by MCP client via `.mcp.json`):
//! ```json
//! {
//!   "mcpServers": {
//!     "trusted-autonomy": {
//!       "type": "stdio",
//!       "command": "cargo",
//!       "args": ["run", "-p", "ta-daemon"]
//!     }
//!   }
//! }
//! ```
//!
//! API mode (`--api`):
//! ```sh
//! ta-daemon --api                    # Starts HTTP API on 127.0.0.1:7700
//! ta-daemon --api --web-port 8080    # Also serves web UI on port 8080
//! ```

mod api;
pub mod channel_dispatcher;
mod config;
pub mod config_watcher;
pub mod external_channel;
pub mod office;
pub mod project_context;
pub mod question_registry;
pub mod router;
mod web;

use anyhow::Result;
use clap::Parser;
use rmcp::ServiceExt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tracing_subscriber::EnvFilter;

use ta_mcp_gateway::{GatewayConfig, TaGatewayServer};

/// Trusted Autonomy MCP server and HTTP API daemon.
#[derive(Parser)]
#[command(
    name = "ta-daemon",
    version,
    about = "Trusted Autonomy MCP server and HTTP API daemon"
)]
struct Cli {
    /// Project root directory (defaults to current directory).
    #[arg(long, default_value = ".")]
    project_root: PathBuf,

    /// Port for the web review UI. When set, serves a browser-based
    /// dashboard for reviewing draft packages.
    #[arg(long)]
    web_port: Option<u16>,

    /// Run in API server mode instead of MCP stdio mode.
    /// Starts the full HTTP API on the configured bind address and port.
    #[arg(long)]
    api: bool,

    /// Path to an office.yaml for multi-project mode.
    /// Can also be set via TA_OFFICE_CONFIG env var.
    #[arg(long)]
    office_config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Strip agent-session env vars so subprocess agents don't detect nesting.
    // This allows the daemon to be started from inside a Claude Code session.
    std::env::remove_var("CLAUDECODE");
    std::env::remove_var("CLAUDE_CODE_ENTRYPOINT");

    // Logs go to stderr so they don't interfere with MCP on stdout.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("ta_mcp_gateway=info".parse()?)
                .add_directive("ta_daemon=info".parse()?),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    let project_root = cli.project_root.canonicalize()?;

    tracing::info!("Starting Trusted Autonomy daemon");
    tracing::info!("Project root: {}", project_root.display());

    // Check for office config (CLI flag or env var).
    let office_config_path = cli
        .office_config
        .or_else(|| std::env::var("TA_OFFICE_CONFIG").ok().map(PathBuf::from));

    if let Some(ref path) = office_config_path {
        match office::OfficeConfig::load(path) {
            Ok(config) => {
                tracing::info!(
                    office = %config.office.name,
                    projects = config.projects.len(),
                    "Running in multi-project office mode"
                );
                match office::ProjectRegistry::from_config(&config) {
                    Ok(registry) => {
                        tracing::info!(
                            count = registry.len(),
                            "Loaded projects: {:?}",
                            registry.names()
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Failed to build project registry: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Cannot load office config: {}", e);
            }
        }
    }

    // Load daemon configuration.
    let daemon_config = config::DaemonConfig::load(&project_root);

    // Set up cross-platform signal handling (v0.10.16).
    // The shutdown notifier is shared with background tasks so they can
    // gracefully terminate when SIGINT/SIGTERM is received.
    let shutdown = Arc::new(Notify::new());
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
                tokio::select! {
                    _ = ctrl_c => {
                        tracing::info!("Received SIGINT (Ctrl-C), initiating graceful shutdown");
                    }
                    _ = sigterm.recv() => {
                        tracing::info!("Received SIGTERM, initiating graceful shutdown");
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = ctrl_c.await;
                tracing::info!("Received Ctrl-C, initiating graceful shutdown");
            }
            shutdown.notify_waiters();
        });
    }

    if cli.api {
        // API server mode: start the full HTTP API.
        tracing::info!("Running in API server mode");

        // Optionally also serve the legacy web UI on a separate port.
        if let Some(web_port) = cli.web_port {
            let gateway_config = GatewayConfig::for_project(&project_root);
            let dir = gateway_config.pr_packages_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = web::serve_web_ui(dir, web_port).await {
                    tracing::error!("Web UI server error: {}", e);
                }
            });
        }

        web::serve_daemon_api(project_root, daemon_config, shutdown).await?;
    } else {
        // MCP stdio mode (default): backward-compatible with existing setup.
        let gateway_config = GatewayConfig::for_project(&project_root);
        let pr_packages_dir = gateway_config.pr_packages_dir.clone();
        let web_port = cli.web_port.or(gateway_config.web_ui_port);

        let server = TaGatewayServer::new(gateway_config)?;

        tracing::info!("MCP server ready, waiting for client connection");

        // Spawn optional web UI server.
        if let Some(port) = web_port {
            let dir = pr_packages_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = web::serve_web_ui(dir, port).await {
                    tracing::error!("Web UI server error: {}", e);
                }
            });
        }

        // Spawn the daemon API alongside MCP if configured.
        {
            let root = project_root.clone();
            let dc = daemon_config.clone();
            let sd = shutdown.clone();
            tokio::spawn(async move {
                if let Err(e) = web::serve_daemon_api(root, dc, sd).await {
                    tracing::error!("Daemon API error: {}", e);
                }
            });
        }

        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .inspect_err(|e| tracing::error!("serving error: {:?}", e))?;

        service.waiting().await?;

        tracing::info!("MCP server shutting down");
    }

    Ok(())
}
