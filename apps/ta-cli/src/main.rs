//! # ta-cli
//!
//! Command-line interface for Trusted Autonomy.
//!
//! Provides human review and approval workflow for agent-staged changes:
//! - `ta goal list/status` — inspect active goal runs
//! - `ta draft list/view/approve/deny/apply` — review and manage draft packages
//! - `ta audit verify/tail` — inspect the tamper-evident audit trail
//! - `ta adapter list/install` — manage agent adapter integrations
//! - `ta serve` — start MCP server on stdio

mod commands;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ta_mcp_gateway::GatewayConfig;

/// Trusted Autonomy CLI — review and approve agent changes.
#[derive(Parser)]
#[command(
    name = "ta",
    version,
    long_version = long_version(),
    about
)]
struct Cli {
    /// Project root directory (defaults to current directory).
    #[arg(long, default_value = ".")]
    project_root: PathBuf,

    /// Accept terms of use non-interactively (for CI/scripted usage).
    #[arg(long, global = true)]
    accept_terms: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage goal runs.
    Goal {
        #[command(subcommand)]
        command: commands::goal::GoalCommands,
    },
    /// Review and manage draft packages.
    Draft {
        #[command(subcommand)]
        command: commands::draft::DraftCommands,
    },
    /// Review and manage PR packages (deprecated: use 'draft').
    #[command(hide = true)]
    Pr {
        #[command(subcommand)]
        command: commands::pr::PrCommands,
    },
    /// Inspect the audit trail.
    Audit {
        #[command(subcommand)]
        command: commands::audit::AuditCommands,
    },
    /// Run an agent in a TA-mediated staging workspace.
    Run {
        /// Goal title describing what to accomplish.
        title: Option<String>,
        /// Agent system to use (claude-code, codex, etc.).
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Source directory to overlay (defaults to project root).
        #[arg(long)]
        source: Option<PathBuf>,
        /// Detailed objective for the goal.
        #[arg(long, default_value = "")]
        objective: String,
        /// Plan phase this goal implements (e.g., "4b").
        #[arg(long)]
        phase: Option<String>,
        /// Follow up on a previous goal (ID prefix or omit for latest).
        #[arg(long)]
        follow_up: Option<Option<String>>,
        /// Read objective from a file instead of --objective.
        #[arg(long)]
        objective_file: Option<PathBuf>,
        /// Don't launch the agent — just set up the workspace.
        #[arg(long)]
        no_launch: bool,
        /// Run in interactive mode with PTY capture and session orchestration.
        #[arg(long)]
        interactive: bool,
        /// Run as a macro goal with inner-loop iteration.
        /// Agent stays in-session, can decompose work into sub-goals,
        /// submit drafts for review, and iterate based on feedback.
        #[arg(long, alias = "macro")]
        macro_goal: bool,
        /// Resume an existing interactive session (ID or prefix).
        #[arg(long)]
        resume: Option<String>,
    },
    /// Manage interactive sessions.
    Session {
        #[command(subcommand)]
        command: commands::session::SessionCommands,
    },
    /// View and track the project development plan.
    Plan {
        #[command(subcommand)]
        command: commands::plan::PlanCommands,
    },
    /// Manage persistent context memory across agents and sessions.
    Context {
        #[command(subcommand)]
        command: commands::context::ContextCommands,
    },
    /// Manage stored credentials for external services.
    Credentials {
        #[command(subcommand)]
        command: commands::credentials::CredentialsCommands,
    },
    /// Manage agent adapter integrations.
    Adapter {
        #[command(subcommand)]
        command: commands::adapter::AdapterCommands,
    },
    /// Run the configurable release pipeline.
    Release {
        #[command(subcommand)]
        command: commands::release::ReleaseCommands,
    },
    /// Start the MCP server on stdio.
    Serve,
    /// Review and accept the terms of use.
    AcceptTerms,
    /// View the current terms of use.
    ViewTerms,
    /// Show terms acceptance status.
    TermsStatus,
}

/// Build the long version string: "0.1.0-alpha (abc1234 2026-02-11)"
const fn long_version() -> &'static str {
    concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("TA_GIT_HASH"),
        " ",
        env!("TA_BUILD_DATE"),
        ")"
    )
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Handle --accept-terms flag (non-interactive acceptance).
    if cli.accept_terms {
        commands::terms::accept_non_interactive()?;
    }

    // Terms-related commands don't require prior acceptance.
    match &cli.command {
        Commands::AcceptTerms => return commands::terms::prompt_and_accept(),
        Commands::ViewTerms => {
            commands::terms::view_terms();
            return Ok(());
        }
        Commands::TermsStatus => return commands::terms::show_status(),
        _ => {}
    }

    // All other commands require terms acceptance.
    if let Err(e) = commands::terms::check_accepted() {
        eprintln!("Error: {}", e);
        eprintln!();
        eprintln!("To accept terms interactively:");
        eprintln!("  ta accept-terms");
        eprintln!();
        eprintln!("To accept non-interactively (CI/scripts):");
        eprintln!("  ta --accept-terms <command>");
        return Err(e);
    }

    let project_root = cli.project_root.canonicalize().unwrap_or(cli.project_root);
    let config = GatewayConfig::for_project(&project_root);

    // Startup health check: warn about stale drafts (v0.3.6).
    commands::draft::check_stale_drafts(&config);

    match &cli.command {
        Commands::Goal { command } => commands::goal::execute(command, &config),
        Commands::Draft { command } => commands::draft::execute(command, &config),
        Commands::Pr { command } => commands::pr::execute(command, &config),
        Commands::Audit { command } => commands::audit::execute(command, &config),
        Commands::Run {
            title,
            agent,
            source,
            objective,
            phase,
            follow_up,
            objective_file,
            no_launch,
            interactive,
            macro_goal,
            resume,
        } => commands::run::execute(
            &config,
            title.as_deref(),
            agent,
            source.as_deref(),
            objective,
            phase.as_deref(),
            follow_up.as_ref(),
            objective_file.as_deref(),
            *no_launch,
            *interactive,
            *macro_goal,
            resume.as_deref(),
        ),
        Commands::Session { command } => commands::session::execute(command, &config),
        Commands::Plan { command } => commands::plan::execute(command, &config),
        Commands::Context { command } => commands::context::execute(command, &config),
        Commands::Credentials { command } => commands::credentials::execute(command, &config),
        Commands::Adapter { command } => commands::adapter::execute(command, &project_root),
        Commands::Release { command } => commands::release::execute(command, &config),
        Commands::Serve => commands::serve::execute(&project_root),
        // Already handled above.
        Commands::AcceptTerms | Commands::ViewTerms | Commands::TermsStatus => unreachable!(),
    }
}
