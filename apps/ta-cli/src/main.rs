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
pub mod framework_registry;

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

    /// Skip the daemon version guard check (for CI or scripted use).
    #[arg(long, global = true)]
    no_version_check: bool,

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
    ///
    /// The title can be a phase ID (e.g., "v0.9.8.1" or "0.9.8.1") — TA will
    /// look up the phase title from PLAN.md automatically and set --phase.
    Run {
        /// Goal title or plan phase ID (e.g., "v0.9.8.1").
        /// If this matches a phase in PLAN.md, the title and --phase are
        /// filled in automatically.
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
        /// Follow up on a previous goal (ID prefix or omit for interactive picker).
        #[arg(long)]
        follow_up: Option<Option<String>>,
        /// Follow up on a specific draft (denied, failed verify, etc.).
        #[arg(long)]
        follow_up_draft: Option<String>,
        /// Follow up on a specific goal by ID prefix.
        #[arg(long)]
        follow_up_goal: Option<String>,
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
        /// Non-interactive (headless) execution for orchestrator-driven goals.
        /// No PTY, pipes stdout, returns draft ID on completion.
        #[arg(long)]
        headless: bool,
        /// Skip pre-draft verification checks (from [verify] in workflow.toml).
        #[arg(long)]
        skip_verify: bool,
        /// Reuse an existing goal record instead of creating a new one.
        /// Used by the MCP orchestrator to avoid duplicate goal creation
        /// when `ta_goal_start` has already created the goal.
        #[arg(long)]
        goal_id: Option<String>,
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
    /// Stream and inspect lifecycle events.
    Events {
        #[command(subcommand)]
        command: commands::events::EventsCommands,
    },
    /// Manage approval tokens for non-interactive workflows.
    Token {
        #[command(subcommand)]
        command: commands::token::TokenCommands,
    },
    /// Interactive developer loop — orchestrate plan execution, goal launches,
    /// draft review, and releases from one persistent session.
    Dev {
        /// Agent system to use for orchestration (defaults to dev-loop config).
        #[arg(long)]
        agent: Option<String>,
        /// Bypass security restrictions (allows Write, Edit, Bash, etc.). Logs a warning.
        #[arg(long)]
        unrestricted: bool,
    },
    /// Interactive setup wizard for TA configuration.
    Setup {
        #[command(subcommand)]
        command: commands::setup::SetupCommands,
    },
    /// Initialize a new TA-managed project from a template.
    Init {
        #[command(subcommand)]
        command: commands::init::InitCommands,
    },
    /// Create a new project through conversational bootstrapping.
    ///
    /// Starts an interactive session with a planner agent that asks about your
    /// project, generates a scaffold, and produces a PLAN.md with versioned phases.
    New {
        #[command(subcommand)]
        command: commands::new::NewCommands,
    },
    /// Author, validate, and manage agent configurations.
    Agent {
        #[command(subcommand)]
        command: commands::agent::AgentCommands,
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
    /// Interactive TA Shell -- full TUI client for the daemon.
    Shell {
        /// Generate default .ta/shell.toml config and exit.
        #[arg(long)]
        init: bool,
        /// Use classic line-mode shell (rustyline) instead of TUI.
        #[arg(long)]
        classic: bool,
        /// Attach to an existing agent session (ID or prefix).
        #[arg(long)]
        attach: Option<String>,
        /// Daemon URL override (default: from .ta/daemon.toml or http://127.0.0.1:7700).
        #[arg(long)]
        url: Option<String>,
    },
    /// Multi-project office daemon management.
    Office {
        #[command(subcommand)]
        command: commands::office::OfficeCommands,
    },
    /// Manage channel plugins (list, install, validate).
    Plugin {
        #[command(subcommand)]
        command: commands::plugin::PluginCommands,
    },
    /// Manage multi-stage workflows with pluggable engines.
    Workflow {
        #[command(subcommand)]
        command: commands::workflow::WorkflowCommands,
    },
    /// Manage policy configuration and auto-approval.
    Policy {
        #[command(subcommand)]
        command: commands::policy::PolicyCommands,
    },
    /// Inspect and validate project configuration (channels, routing).
    Config {
        #[command(subcommand)]
        command: commands::config::ConfigCommands,
    },
    /// Unified garbage collection: goals, drafts, staging directories, and event store.
    Gc {
        /// Show what would be cleaned without making changes.
        #[arg(long)]
        dry_run: bool,
        /// Stale threshold in days (default: 7).
        #[arg(long, default_value = "7")]
        threshold_days: u32,
        /// Ignore threshold — GC everything in terminal state.
        #[arg(long)]
        all: bool,
        /// Move to .ta/goals/archive/ instead of deleting.
        #[arg(long)]
        archive: bool,
    },
    /// Project-wide status dashboard: active agents, pending drafts, next phase.
    Status,
    /// Start the MCP server on stdio.
    Serve,
    /// Review and accept the terms of use.
    AcceptTerms,
    /// View the current terms of use.
    ViewTerms,
    /// Show terms acceptance status.
    TermsStatus,
    /// Manage per-agent terms consent (v0.10.18.4).
    ///
    /// Subcommands: `ta terms show <agent>`, `ta terms accept <agent>`, `ta terms status`.
    Terms {
        /// Action: show, accept, or status.
        action: String,
        /// Agent ID (required for show/accept, optional for status).
        agent: Option<String>,
    },
    /// Run pre-draft verification checks against a staging workspace.
    ///
    /// Runs the [verify] commands from .ta/workflow.toml in the staging
    /// directory. Useful for manual verification without running `ta run`.
    Verify {
        /// Goal ID (or prefix) whose staging directory to verify.
        /// Defaults to the most recent active goal.
        goal_id: Option<String>,
    },
    /// View the interactive conversation history for a goal.
    Conversation {
        /// Goal run ID (or prefix).
        goal_id: String,
        /// Output as raw JSONL instead of formatted text.
        #[arg(long)]
        json: bool,
    },
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

/// Resolve a phase ID from the positional title argument.
///
/// If the title looks like a phase ID (e.g., "v0.9.8.1", "0.9.8.1",
/// "phase 0.9.8.1"), look it up in PLAN.md and return the full phase
/// title + phase ID. Otherwise, return the original title and phase.
fn resolve_phase_title(
    title: &Option<String>,
    phase: &Option<String>,
    project_root: &std::path::Path,
) -> (Option<String>, Option<String>) {
    let raw = match title.as_deref() {
        Some(t) => t.trim(),
        None => {
            // No title at all — if --phase is set, try to resolve from that.
            return match phase {
                Some(p) => match try_resolve_phase(p.trim(), project_root) {
                    Some((t, id)) => (Some(t), Some(id)),
                    None => (None, Some(p.clone())),
                },
                None => (None, None),
            };
        }
    };

    // Strip optional "phase " prefix (case-insensitive).
    let candidate = raw
        .strip_prefix("phase ")
        .or_else(|| raw.strip_prefix("Phase "))
        .unwrap_or(raw);

    // Check if it looks like a phase ID (starts with optional 'v', then digits and dots).
    let is_phase_like = {
        let c = candidate.strip_prefix('v').unwrap_or(candidate);
        !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
    };

    if is_phase_like {
        if let Some((resolved_title, phase_id)) = try_resolve_phase(candidate, project_root) {
            return (Some(resolved_title), Some(phase_id));
        }
    }

    // Not a phase ID — pass through as-is.
    (Some(raw.to_string()), phase.clone())
}

/// Try to find a phase in PLAN.md by ID (with or without 'v' prefix).
fn try_resolve_phase(candidate: &str, project_root: &std::path::Path) -> Option<(String, String)> {
    let phases = commands::plan::load_plan(project_root).ok()?;

    // Try exact match, then with/without 'v' prefix.
    let stripped = candidate.strip_prefix('v').unwrap_or(candidate);
    let with_v = format!("v{}", stripped);

    let phase = phases
        .iter()
        .find(|p| p.id == candidate || p.id == stripped || p.id == with_v)?;

    let title = format!("Implement {} — {}", phase.id, phase.title);
    Some((title, phase.id.clone()))
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
        // Per-agent terms management (v0.10.18.4).
        Commands::Terms { action, agent } => {
            let project_root = cli
                .project_root
                .canonicalize()
                .unwrap_or_else(|_| cli.project_root.clone());
            match action.as_str() {
                "show" => {
                    let agent_id = agent.as_deref().unwrap_or("claude-code");
                    commands::consent::show_agent_terms(agent_id);
                    return Ok(());
                }
                "accept" => {
                    let agent_id = agent.as_deref().unwrap_or("claude-code");
                    return commands::consent::prompt_and_accept(&project_root, agent_id);
                }
                "status" => {
                    commands::consent::show_status(&project_root);
                    return Ok(());
                }
                other => {
                    eprintln!(
                        "Unknown terms action '{}'. Use: show, accept, or status.",
                        other
                    );
                    return Err(anyhow::anyhow!("unknown terms action: {}", other));
                }
            }
        }
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
            follow_up_draft,
            follow_up_goal,
            objective_file,
            no_launch,
            interactive,
            macro_goal,
            resume,
            headless,
            skip_verify,
            goal_id,
        } => {
            // Phase-aware title resolution: if the positional title looks like
            // a phase ID (e.g., "v0.9.8.1", "0.9.8.1", "phase 0.9.8.1"),
            // look it up in PLAN.md and use the phase title + set --phase.
            let (resolved_title, resolved_phase) = resolve_phase_title(title, phase, &project_root);
            commands::run::execute(
                &config,
                resolved_title.as_deref(),
                agent,
                source.as_deref(),
                objective,
                resolved_phase.as_deref(),
                follow_up.as_ref(),
                follow_up_draft.as_deref(),
                follow_up_goal.as_deref(),
                objective_file.as_deref(),
                *no_launch,
                *interactive,
                *macro_goal,
                resume.as_deref(),
                *headless,
                *skip_verify,
                goal_id.as_deref(),
            )
        }
        Commands::Events { command } => commands::events::execute(command, &config),
        Commands::Token { command } => commands::token::execute(command, &config),
        Commands::Dev {
            agent,
            unrestricted,
        } => commands::dev::execute(
            &config,
            &project_root,
            agent.as_deref(),
            *unrestricted,
            cli.no_version_check,
        ),
        Commands::Session { command } => commands::session::execute(command, &config),
        Commands::Plan { command } => commands::plan::execute(command, &config),
        Commands::Context { command } => commands::context::execute(command, &config),
        Commands::Credentials { command } => commands::credentials::execute(command, &config),
        Commands::Agent { command } => commands::agent::execute(command, &config),
        Commands::Adapter { command } => commands::adapter::execute(command, &project_root),
        Commands::Setup { command } => commands::setup::execute(command, &config),
        Commands::Init { command } => commands::init::execute(command, &config),
        Commands::New { command } => commands::new::execute(command, &config),
        Commands::Release { command } => commands::release::execute(command, &config),
        Commands::Shell {
            init,
            classic,
            attach,
            url,
        } => commands::shell::execute(
            &project_root,
            attach.as_deref(),
            url.as_deref(),
            *init,
            *classic,
            cli.no_version_check,
        ),
        Commands::Office { command } => commands::office::execute(command, &project_root),
        Commands::Plugin { command } => {
            commands::plugin::run_plugin(&project_root, command)?;
            Ok(())
        }
        Commands::Workflow { command } => commands::workflow::execute(command, &config),
        Commands::Policy { command } => commands::policy::execute(command, &config),
        Commands::Config { command } => commands::config::execute(command, &config),
        Commands::Gc {
            dry_run,
            threshold_days,
            all,
            archive,
        } => commands::gc::execute(&config, *dry_run, *threshold_days, *all, *archive),
        Commands::Status => commands::status::execute(&config),
        Commands::Serve => commands::serve::execute(&project_root),
        Commands::Verify { goal_id } => commands::verify::execute(&config, goal_id.as_deref()),
        Commands::Conversation { goal_id, json } => {
            commands::conversation::execute(&config, goal_id, *json)
        }
        // Already handled above.
        Commands::AcceptTerms
        | Commands::ViewTerms
        | Commands::TermsStatus
        | Commands::Terms { .. } => unreachable!(),
    }
}
