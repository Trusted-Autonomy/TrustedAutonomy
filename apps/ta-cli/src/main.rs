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
///
/// Run `ta` with no arguments to show the project status dashboard.
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

    /// Print startup timing for each phase (config load, daemon connect, dispatch).
    /// Useful for diagnosing slow CLI startup on Windows or cold-start environments.
    #[arg(long, global = true)]
    startup_profile: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    // ── DASHBOARD ───────────────────────────────────────────────────────────
    /// Project-wide status dashboard: active agents, pending drafts, next phase.
    ///
    /// Shows urgent items (stuck goals, pending approvals) first, then active work,
    /// recent completions, and suggested next actions. Run with no arguments to
    /// get the same view: `ta` is equivalent to `ta status`.
    Status {
        /// Deep status: daemon health, disk usage, pending questions, recent events.
        #[arg(long)]
        deep: bool,
    },

    // ── CORE WORKFLOW ───────────────────────────────────────────────────────
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
        /// Agent persona to apply (name of .ta/personas/<name>.toml).
        #[arg(long)]
        persona: Option<String>,
        /// Suppress streaming agent output; still print completion/failure summary.
        /// Default for daemon-dispatched and channel-dispatched goals.
        /// Inverse: omit --quiet (current interactive default) shows full output.
        #[arg(long)]
        quiet: bool,
        /// Reuse an existing goal record instead of creating a new one.
        /// Used by the MCP orchestrator to avoid duplicate goal creation
        /// when `ta_goal_start` has already created the goal.
        #[arg(long)]
        goal_id: Option<String>,
        /// Workflow to execute (e.g., 'serial-phases', 'swarm', 'single-agent').
        ///
        /// Resolves in priority order:
        /// 1. This flag (explicit override)
        /// 2. .ta/config.yaml channels.default_workflow (project-level default)
        /// 3. Built-in "single-agent" (backwards-compatible default)
        ///
        /// serial-phases: use with --phases to enable multi-phase gate evaluation.
        /// swarm:         use with --sub-goals to enable parallel sub-goal execution.
        ///
        /// Run `ta workflow list --builtin` to see available workflows.
        #[arg(long)]
        workflow: Option<String>,
        /// Phases to execute (serial-phases workflow only).
        ///
        /// Comma-separated phase IDs, e.g. --phases v0.13.7.1,v0.13.7.2
        /// Each phase runs as a follow-up goal reusing the same staging directory.
        /// Requires --workflow serial-phases.
        #[arg(long, value_delimiter = ',')]
        phases: Option<Vec<String>>,
        /// Gate commands to evaluate after each phase (serial-phases) or sub-goal (swarm).
        ///
        /// Built-in gates: "build", "test", "clippy".
        /// Any other string is run as a shell command in the staging directory.
        /// Multiple gates: --gates build --gates test
        /// Default (serial-phases): no gates (agent is trusted to leave staging correct).
        #[arg(long)]
        gates: Vec<String>,
        /// Sub-goals for the swarm workflow.
        ///
        /// Each value is the title of one sub-goal agent. Each sub-goal runs
        /// independently in its own staging directory.
        /// Example: --sub-goals "Add auth endpoint" --sub-goals "Add auth tests"
        /// Requires --workflow swarm.
        #[arg(long)]
        sub_goals: Vec<String>,
        /// Run an integration agent after all swarm sub-goals complete (swarm workflow).
        ///
        /// The integration agent receives the list of all passed staging paths
        /// and merges the results into a single coherent output.
        #[arg(long)]
        integrate: bool,
        /// Skip the provider-configured check (for CI/scripted use).
        ///
        /// By default, `ta run` checks that a provider has been configured via
        /// `ta onboard`. Pass this flag to bypass that check in CI or automation.
        #[arg(long)]
        skip_onboard_check: bool,
    },
    /// Review and manage draft packages.
    Draft {
        #[command(subcommand)]
        command: commands::draft::DraftCommands,
    },
    /// Manage goal runs.
    Goal {
        #[command(subcommand)]
        command: commands::goal::GoalCommands,
    },
    /// View and track the project development plan.
    Plan {
        #[command(subcommand)]
        command: commands::plan::PlanCommands,
    },
    /// Manage agent personas for role-based behavior.
    Persona {
        #[command(subcommand)]
        command: commands::persona::PersonaCommands,
    },
    /// Interactive TA Shell — opens the web shell in your browser (default).
    ///
    /// Use --tui or TA_SHELL_TUI=1 for the terminal-based shell.
    Shell {
        /// Generate default .ta/shell.toml config and exit.
        #[arg(long)]
        init: bool,
        /// Use terminal TUI shell instead of web UI.
        /// Can also be enabled with TA_SHELL_TUI=1 env var.
        #[arg(long)]
        tui: bool,
        /// Use classic line-mode shell (rustyline) instead of TUI.
        /// Implies --tui.
        #[arg(long)]
        classic: bool,
        /// Attach to an existing agent session (ID or prefix). Implies --tui.
        #[arg(long)]
        attach: Option<String>,
        /// Daemon URL override (default: from .ta/daemon.toml or http://127.0.0.1:7700).
        #[arg(long)]
        url: Option<String>,
    },

    // ── OPERATIONS ──────────────────────────────────────────────────────────
    /// View, list, and run operational runbooks (v0.13.1.6).
    ///
    /// Runbooks automate common recovery procedures: disk pressure cleanup,
    /// zombie goal recovery, stale draft cleanup, and more.
    /// Built-in runbooks ship with TA; project-local runbooks live in .ta/runbooks/.
    Runbook {
        #[command(subcommand)]
        command: commands::runbook::RunbookCommands,
    },
    /// View and manage autonomous daemon operations (v0.13.1).
    ///
    /// The daemon watchdog continuously monitors goal health, disk space,
    /// and plugin status. Corrective action proposals are logged here.
    Operations {
        #[command(subcommand)]
        command: commands::operations::OperationsCommands,
    },
    /// Manage the TA daemon lifecycle (start, stop, restart, status, log).
    Daemon {
        #[command(subcommand)]
        command: commands::daemon::DaemonCommands,
    },
    /// Unified garbage collection: goals, drafts, staging directories, and event store.
    Gc {
        /// Show what would be cleaned without making changes.
        #[arg(long)]
        dry_run: bool,
        /// Stale threshold in days for applied/completed goals (default: 7).
        /// Failed goals use [gc] failed_staging_retention_hours from daemon.toml (default: 4h).
        #[arg(long, default_value = "7")]
        threshold_days: u32,
        /// Ignore threshold — GC everything in terminal state.
        #[arg(long)]
        all: bool,
        /// Move to .ta/goals/archive/ instead of deleting.
        #[arg(long)]
        archive: bool,
        /// Also prune old events from .ta/events/ (v0.11.3).
        #[arg(long)]
        include_events: bool,
        /// Run lifecycle compaction: remove fat artifacts (staging, draft packages)
        /// for applied/closed goals older than --compact-after-days (v0.13.1).
        #[arg(long)]
        compact: bool,
        /// Age threshold for compaction in days (default: 30). Only used with --compact.
        #[arg(long, default_value = "30")]
        compact_after_days: u32,
        /// Run GC even if a release pipeline lockfile is present.
        #[arg(long)]
        force: bool,
        /// Show a status table of all goals with staging dirs: state, age, size (v0.15.6.2).
        #[arg(long)]
        status: bool,
        /// Delete all staging dirs for non-running terminal goals (v0.15.6.2).
        /// Prompts for confirmation unless --dry-run is also set.
        #[arg(long)]
        delete_stale: bool,
    },
    /// System-wide health check: runtime chain, auth validation, agent binaries, daemon, VCS.
    ///
    /// Validates the full TA runtime and reports the active authentication mode for the
    /// configured agent framework. Auth checking is agent-agnostic — it reads the framework
    /// manifest and validates whichever auth method applies (API key, session, local service).
    ///
    /// Examples:
    ///   ta doctor              # human-readable output
    ///   ta doctor --json       # machine-readable JSON for CI
    ///   ta doctor --fix-denied # interactively clean up pr_ready goals with denied drafts
    Doctor {
        /// Output results as a JSON array (for CI / scripted use).
        #[arg(long)]
        json: bool,
        /// Interactively clean up goals that are pr_ready with a denied draft (v0.15.18).
        ///
        /// For each such goal, prompts to delete staging + mark closed, or skip.
        #[arg(long)]
        fix_denied: bool,
    },

    /// Upgrade project-level TA configuration to the current binary version (v0.15.18).
    ///
    /// Detects project-level changes required since the project was last initialized or
    /// upgraded (e.g., new gitignore entries, config schema fields). Applies them automatically.
    ///
    /// Examples:
    ///   ta upgrade              # apply all pending steps
    ///   ta upgrade --dry-run    # show what would be applied without changing anything
    ///   ta upgrade --force      # re-run all steps regardless of version
    ///   ta upgrade --acknowledge ".ta/review/"  # suppress a warning for intentional omission
    Upgrade(commands::upgrade::UpgradeArgs),

    // ── ONBOARDING ──────────────────────────────────────────────────────────
    /// First-time setup wizard: configure AI provider, agent, and planning framework.
    ///
    /// Guides you through selecting an AI provider (Anthropic Claude or Ollama),
    /// entering your API key, choosing an implementation agent (claude-code, codex,
    /// claude-flow), and selecting a planning framework (default, BMAD, GSD).
    ///
    /// Configuration is written to ~/.config/ta/config.toml.
    ///
    /// Examples:
    ///   ta onboard                     # interactive TUI wizard
    ///   ta onboard --status            # show current configuration
    ///   ta onboard --reset             # clear config and re-run wizard
    ///   ta onboard --force             # re-run even if already configured
    ///   ta onboard --web               # open Studio setup page in browser
    ///   ta onboard --non-interactive --provider anthropic --api-key sk-ant-...
    Onboard {
        /// Open the Studio setup page in your browser instead of the TUI.
        #[arg(long)]
        web: bool,
        /// Non-interactive mode: configure using flags without prompting.
        #[arg(long)]
        non_interactive: bool,
        /// Called from a platform installer; implies --non-interactive with installer defaults.
        #[arg(long, hide = true)]
        from_installer: bool,
        /// Re-run the wizard even if TA is already configured.
        #[arg(long)]
        force: bool,
        /// Show current configuration without running the wizard.
        #[arg(long)]
        status: bool,
        /// Clear configuration and re-run the wizard.
        #[arg(long)]
        reset: bool,
        /// AI provider to use (anthropic or ollama). Used with --non-interactive.
        #[arg(long)]
        provider: Option<String>,
        /// Anthropic API key. Used with --non-interactive --provider anthropic.
        #[arg(long)]
        api_key: Option<String>,
        /// Implementation agent to set as default (claude-code, codex, claude-flow).
        #[arg(long)]
        agent: Option<String>,
        /// Planning framework to use (default, bmad, gsd). Used with --non-interactive.
        #[arg(long)]
        planning_framework: Option<String>,
        /// Skip the provider-configured check (for CI/scripted use).
        /// Only used internally; callers should use --non-interactive.
        #[arg(long, hide = true)]
        skip_onboard_check: bool,
    },

    // ── ADVANCED ────────────────────────────────────────────────────────────
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
    /// Manage interactive sessions.
    Session {
        #[command(subcommand)]
        command: commands::session::SessionCommands,
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
    /// Open the TA Studio setup wizard in your browser.
    ///
    /// Starts the daemon if not already running, then opens the web UI at
    /// http://localhost:7700/setup so you can complete the 5-step wizard:
    /// agent system, VCS, notifications, first project, and summary.
    ///
    /// Run this once after installation to get started.
    Install,
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
    /// Ask the advisor agent a question or give it a natural language instruction.
    ///
    /// Classifies your intent, presents numbered options, and executes the
    /// selected action. Security level follows `[shell.advisor]` in workflow.toml.
    ///
    /// Examples:
    ///   ta advisor ask "implement remaining v0.15"
    ///   ta advisor ask "apply" --security suggest
    ///   ta advisor ask "what changed?" --tab plan --no-input
    Advisor {
        #[command(subcommand)]
        command: commands::advisor::AdvisorCommands,
    },

    /// Send a mid-run note to the active goal's agent (shorthand for `ta advisor advise`).
    ///
    /// The note is delivered via the goal's context channel. The delivery mode
    /// is printed so you know whether the agent saw it live (live-polled),
    /// via API push (api-pushed), or will see it at the next restart (queued).
    ///
    /// Examples:
    ///   ta advise "please focus on the auth module"
    ///   ta advise --goal abc123 "add more test coverage"
    Advise {
        /// The note/instruction to send to the agent.
        message: String,
        /// Goal ID (or prefix) to target. Defaults to the most recent running goal.
        #[arg(long)]
        goal: Option<String>,
    },
    /// Author, validate, and manage agent configurations.
    Agent {
        #[command(subcommand)]
        command: commands::agent::AgentCommands,
    },
    /// Manage the project behavioral constitution (.ta/constitution.md).
    ///
    /// `ta constitution init` asks an agent to draft a behavioral contract
    /// from PLAN.md and CLAUDE.md. The output is a TA draft for review.
    Constitution {
        #[command(subcommand)]
        command: commands::constitution::ConstitutionCommands,
    },
    /// Inspect and manage the semantic memory store (v0.12.5).
    ///
    /// `ta memory backend` shows the active backend, entry count, and storage size.
    /// `ta memory list` prints stored entries (alias for `ta context list`).
    Memory {
        #[command(subcommand)]
        command: commands::memory::MemoryCommands,
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
    /// Manage creative project templates (install, list, remove, publish, search).
    ///
    /// Templates provide project scaffolding including workflow.toml, .taignore,
    /// optional memory.toml, and an onboarding goal prompt.
    ///
    /// Examples:
    ///   ta template list
    ///   ta template install blender-addon
    ///   ta template install github:myorg/my-template
    ///   ta template install ./my-local-template
    Template {
        #[command(subcommand)]
        command: commands::template::TemplateCommands,
    },
    /// One-step publish: apply the latest approved draft, commit, push, and create a PR.
    ///
    /// Finds the most recently approved draft, applies it, stages and commits
    /// changes with git, pushes to the remote, and optionally opens a GitHub PR.
    Publish {
        /// Commit message (defaults to the draft title).
        #[arg(long, short)]
        message: Option<String>,
        /// Skip confirmation prompts (non-interactive mode).
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Manage multi-stage workflows with pluggable engines.
    Workflow {
        #[command(subcommand)]
        command: commands::workflow::WorkflowCommands,
    },
    /// Feature velocity stats and outcome telemetry (v0.13.10).
    ///
    /// `ta stats velocity` shows aggregate, per-contributor breakdown, and phase conflicts.
    /// `ta stats velocity-detail` shows a per-goal breakdown table.
    /// `ta stats export` exports full history as JSON or CSV.
    /// `ta stats migrate` promotes local history to the committed shared file.
    Stats {
        #[command(subcommand)]
        command: commands::stats::StatsCommands,
    },
    /// Access and manage community knowledge resources (v0.13.6).
    ///
    /// `ta community list` shows configured resources with sync status.
    /// `ta community sync` refreshes the local cache from GitHub or local sources.
    /// `ta community search <query>` searches across all enabled resources.
    /// `ta community get <id>` fetches and displays a specific document.
    ///
    /// Configure resources in `.ta/community-resources.toml`.
    Community {
        #[command(subcommand)]
        command: commands::community::CommunityCommands,
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
    /// Start the MCP server on stdio.
    Serve,
    /// Build the project using the configured build adapter.
    ///
    /// Auto-detects the build system (Cargo, npm, Make) or uses the adapter
    /// configured in `[build]` in `.ta/workflow.toml`. Emits `build_completed`
    /// or `build_failed` events.
    Build {
        /// Also run the test suite after building.
        #[arg(long)]
        test: bool,
    },
    /// Sync the local workspace with upstream changes.
    ///
    /// Calls the configured VCS adapter's sync operation (e.g., git fetch + merge/rebase).
    /// Emits `sync_completed` or `sync_conflict` events. Configure sync behavior
    /// in `[source.sync]` in `.ta/workflow.toml`.
    Sync,
    /// Run pre-draft verification checks against a staging workspace.
    ///
    /// Runs the [verify] commands from .ta/workflow.toml in the staging
    /// directory. Useful for manual verification without running `ta run`.
    Verify {
        /// Goal ID (or prefix) whose staging directory to verify.
        /// Defaults to the most recent active goal.
        goal_id: Option<String>,
    },
    /// Language-aware static analysis with optional agent correction loop (v0.15.14.3).
    ///
    /// Reads `[analysis.<lang>]` configuration from `.ta/workflow.toml` and runs
    /// the configured tool (mypy, pyright, cargo-clippy, golangci-lint, eslint).
    ///
    /// Examples:
    ///   ta analysis run
    ///   ta analysis run --lang python
    ///   ta analysis run --fix
    Analysis {
        #[command(subcommand)]
        command: commands::analysis::AnalysisCommands,
    },
    /// View the interactive conversation history for a goal.
    Conversation {
        /// Goal run ID (or prefix).
        goal_id: String,
        /// Output as raw JSONL instead of formatted text.
        #[arg(long)]
        json: bool,
    },

    /// Manage connector MCP servers (Unreal Engine, Unity).
    ///
    /// Subcommands: `install`, `list`, `status`, `start`, `stop`.
    ///
    /// Examples:
    ///   ta connector install unreal --backend flopperam
    ///   ta connector list
    ///   ta connector status unreal
    Connector {
        #[command(subcommand)]
        command: commands::connector::ConnectorCommands,
    },

    /// Test and manage inbound VCS webhook triggers (v0.14.8.3).
    ///
    /// Simulates webhook events locally to verify trigger configuration
    /// without needing a real VCS event. Use `ta webhook test` to check
    /// that your workflow.toml triggers fire correctly.
    ///
    /// Examples:
    ///   ta webhook test github pull_request.closed --branch main
    ///   ta webhook test vcs changelist_submitted --change 12345
    Webhook {
        #[command(subcommand)]
        command: commands::webhook::WebhookCommands,
    },

    // ── TERMS ───────────────────────────────────────────────────────────────
    /// Review and accept the terms of use.
    ///
    /// In interactive terminals, displays the terms and prompts for acceptance.
    /// Use `--yes` to accept non-interactively (CI/scripted usage).
    #[command(hide = true)]
    AcceptTerms {
        /// Accept without prompting (for CI / install scripts).
        #[arg(long)]
        yes: bool,
    },
    /// View the current terms of use.
    #[command(hide = true)]
    ViewTerms,
    /// Show terms acceptance status.
    #[command(hide = true)]
    TermsStatus,
    /// Manage per-agent terms consent (v0.10.18.4).
    ///
    /// Subcommands: `ta terms show <agent>`, `ta terms accept <agent>`, `ta terms status`.
    #[command(hide = true)]
    Terms {
        /// Action: show, accept, or status.
        action: String,
        /// Agent ID (required for show/accept, optional for status).
        agent: Option<String>,
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
    let startup_begin = std::time::Instant::now();
    let cli = Cli::parse();
    let t_parse = startup_begin.elapsed();

    // Handle --accept-terms flag (non-interactive acceptance).
    if cli.accept_terms {
        commands::terms::accept_non_interactive()?;
    }

    // Terms-related commands don't require prior acceptance.
    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::AcceptTerms { yes } => {
                if *yes {
                    return commands::terms::accept_non_interactive();
                } else {
                    return commands::terms::prompt_and_accept();
                }
            }
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
    }

    // Mutating commands (ta init, ta run, ta goal start) require terms acceptance.
    // Read-only commands (ta plan list, ta draft view, ta stats, etc.) are exempt.
    let needs_terms = cli.command.as_ref().is_some_and(requires_terms_acceptance);
    if needs_terms {
        commands::terms::ensure_accepted()?;
    }

    let project_root = cli.project_root.canonicalize().unwrap_or(cli.project_root);
    let t_project_root = startup_begin.elapsed();
    let config = GatewayConfig::for_project(&project_root);
    let t_config = startup_begin.elapsed();

    if cli.startup_profile {
        eprintln!(
            "[startup-profile] arg parse:       {:>6.1}ms",
            t_parse.as_secs_f64() * 1000.0
        );
        eprintln!(
            "[startup-profile] project root:    {:>6.1}ms  (+{:.1}ms)",
            t_project_root.as_secs_f64() * 1000.0,
            (t_project_root - t_parse).as_secs_f64() * 1000.0
        );
        eprintln!(
            "[startup-profile] config load:     {:>6.1}ms  (+{:.1}ms)",
            t_config.as_secs_f64() * 1000.0,
            (t_config - t_project_root).as_secs_f64() * 1000.0
        );
    }

    // Startup health check: warn about stale drafts (v0.3.6).
    commands::draft::check_stale_drafts(&config);
    let t_health = startup_begin.elapsed();

    if cli.startup_profile {
        eprintln!(
            "[startup-profile] health check:    {:>6.1}ms  (+{:.1}ms)",
            t_health.as_secs_f64() * 1000.0,
            (t_health - t_config).as_secs_f64() * 1000.0
        );
    }

    // No subcommand → show status dashboard (v0.13.1.6 item 2).
    let command = match &cli.command {
        Some(cmd) => cmd,
        None => return commands::status::execute(&config, false),
    };

    let t_dispatch = startup_begin.elapsed();
    if cli.startup_profile {
        eprintln!(
            "[startup-profile] command dispatch: {:>6.1}ms  (+{:.1}ms)",
            t_dispatch.as_secs_f64() * 1000.0,
            (t_dispatch - t_health).as_secs_f64() * 1000.0
        );
        eprintln!("[startup-profile] ---");
        eprintln!(
            "[startup-profile] total to dispatch: {:.1}ms",
            t_dispatch.as_secs_f64() * 1000.0
        );
    }

    match command {
        Commands::Status { deep } => commands::status::execute(&config, *deep),
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
            persona,
            quiet,
            goal_id,
            workflow,
            phases,
            gates,
            sub_goals,
            integrate,
            skip_onboard_check,
        } => {
            // First-run gate: warn if provider is not yet configured.
            commands::onboard::check_provider_configured(*skip_onboard_check)?;

            // Phase-aware title resolution: if the positional title looks like
            // a phase ID (e.g., "v0.9.8.1", "0.9.8.1", "phase 0.9.8.1"),
            // look it up in PLAN.md and use the phase title + set --phase.
            let (resolved_title, resolved_phase) = resolve_phase_title(title, phase, &project_root);

            // serial-phases: dispatch to execute_serial_phases when --phases is provided.
            if workflow.as_deref() == Some("serial-phases") || phases.is_some() {
                if let Some(phase_list) = phases {
                    if !phase_list.is_empty() {
                        let run_title = resolved_title.as_deref().unwrap_or("Serial phases run");
                        return commands::run::execute_serial_phases(
                            &config, run_title, agent, objective, phase_list, gates, *quiet,
                        );
                    }
                }
            }

            // swarm: dispatch to execute_swarm when --sub-goals is provided.
            if !sub_goals.is_empty() {
                let run_title = resolved_title.as_deref().unwrap_or("Swarm run");
                return commands::run::execute_swarm(
                    &config, run_title, agent, objective, sub_goals, gates, *integrate, *quiet,
                );
            }

            // Default: single-agent execution.
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
                *quiet,
                goal_id.as_deref(),
                workflow.as_deref(),
                persona.as_deref(),
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
        Commands::Persona { command } => commands::persona::execute(command, &config),
        Commands::Context { command } => commands::context::execute(command, &config),
        Commands::Credentials { command } => commands::credentials::execute(command, &config),
        Commands::Advisor { command } => commands::advisor::execute(command, &config),
        Commands::Advise { message, goal } => {
            commands::advisor::advise(&config, message, goal.as_deref())
        }
        Commands::Agent { command } => commands::agent::execute(command, &config),
        Commands::Constitution { command } => commands::constitution::execute(command, &config),
        Commands::Memory { command } => commands::memory::execute(command, &config),
        Commands::Adapter { command } => commands::adapter::execute(command, &project_root),
        Commands::Install => commands::install::execute(&project_root),
        Commands::Setup { command } => commands::setup::execute(command, &config),
        Commands::Init { command } => commands::init::execute(command, &config),
        Commands::New { command } => commands::new::execute(command, &config),
        Commands::Release { command } => commands::release::execute(command, &config),
        Commands::Shell {
            init,
            tui,
            classic,
            attach,
            url,
        } => {
            // TUI mode if --tui, --classic, --attach, or TA_SHELL_TUI=1.
            let use_tui = *tui
                || *classic
                || attach.is_some()
                || std::env::var("TA_SHELL_TUI").is_ok_and(|v| v == "1");

            if use_tui {
                commands::shell::execute(
                    &project_root,
                    attach.as_deref(),
                    url.as_deref(),
                    *init,
                    *classic,
                    cli.no_version_check,
                )
            } else if *init {
                commands::shell::init_config(&project_root)
            } else {
                commands::shell::open_web_shell(&project_root, url.as_deref())
            }
        }
        Commands::Daemon { command } => commands::daemon::execute(command, &project_root),
        Commands::Office { command } => commands::office::execute(command, &project_root),
        Commands::Plugin { command } => {
            commands::plugin::run_plugin(&project_root, command)?;
            Ok(())
        }
        Commands::Template { command } => commands::template::execute(command, &config),
        Commands::Publish { message, yes } => {
            commands::publish::execute(&project_root, message.as_deref(), *yes)
        }
        Commands::Workflow { command } => commands::workflow::execute(command, &config),
        Commands::Stats { command } => commands::stats::execute(command, &config),
        Commands::Community { command } => commands::community::execute(command, &config),
        Commands::Policy { command } => commands::policy::execute(command, &config),
        Commands::Config { command } => commands::config::execute(command, &config),
        Commands::Gc {
            dry_run,
            threshold_days,
            all,
            archive,
            include_events,
            compact,
            compact_after_days,
            force,
            status,
            delete_stale,
        } => commands::gc::execute(
            &config,
            *dry_run,
            *threshold_days,
            *all,
            *archive,
            *include_events,
            *compact,
            *compact_after_days,
            *force,
            *status,
            *delete_stale,
        ),
        Commands::Operations { command } => commands::operations::execute(command, &config),
        Commands::Runbook { command } => commands::runbook::execute(command, &config),
        Commands::Connector { command } => commands::connector::execute(command, &config),
        Commands::Webhook { command } => commands::webhook::execute(command, &config),
        Commands::Serve => {
            // First-run gate: warn if provider is not yet configured.
            // TA_SKIP_ONBOARD_CHECK=1 bypasses in CI.
            let skip = std::env::var("TA_SKIP_ONBOARD_CHECK").is_ok_and(|v| v == "1");
            commands::onboard::check_provider_configured(skip)?;
            commands::serve::execute(&project_root)
        }
        Commands::Build { test } => commands::build::execute(&config, *test),
        Commands::Sync => commands::sync::execute(&config),
        Commands::Verify { goal_id } => commands::verify::execute(&config, goal_id.as_deref()),
        Commands::Analysis { command } => commands::analysis::execute(command, &config),
        Commands::Doctor { json, fix_denied } => {
            commands::doctor::execute(&config, *json, *fix_denied)
        }
        Commands::Upgrade(args) => commands::upgrade::execute(&config, args),
        Commands::Conversation { goal_id, json } => {
            commands::conversation::execute(&config, goal_id, *json)
        }
        Commands::Onboard {
            web,
            non_interactive,
            from_installer,
            force,
            status,
            reset,
            provider,
            api_key,
            agent,
            planning_framework,
            skip_onboard_check: _,
        } => commands::onboard::execute(
            *web,
            *non_interactive,
            *from_installer,
            *force,
            *status,
            *reset,
            provider.as_deref(),
            api_key.as_deref(),
            agent.as_deref(),
            planning_framework.as_deref(),
            &project_root,
        ),
        // Already handled above.
        Commands::AcceptTerms { .. }
        | Commands::ViewTerms
        | Commands::TermsStatus
        | Commands::Terms { .. } => unreachable!(),
    }
}

/// Returns true for commands that cause agent-mediated workspace mutations and
/// therefore require terms acceptance before proceeding.
///
/// Read-only commands (ta plan list, ta draft view, ta goal list, ta stats, …)
/// return false and are never gated on terms acceptance.
fn requires_terms_acceptance(cmd: &Commands) -> bool {
    match cmd {
        Commands::Init { .. } | Commands::Run { .. } => true,
        Commands::Goal { command } => commands::goal::is_start_command(command),
        _ => false,
    }
}
