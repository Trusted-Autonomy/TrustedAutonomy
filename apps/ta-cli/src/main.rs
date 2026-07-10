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

/// Subcommands for `ta gc` that target specific subsystems.
#[derive(Debug, Subcommand)]
enum GcSubcommand {
    /// Remove unreferenced SHA blobs from the managed-paths store (v0.17.0).
    ///
    /// Scans `.ta/sha-fs/` and removes any blob not referenced by a live journal
    /// entry in `.ta/governed/journal.jsonl`. Prints the total bytes reclaimed.
    ///
    /// Examples:
    ///   ta gc governed-paths
    ///   ta gc governed-paths --retain-days 14
    ///   ta gc governed-paths --dry-run
    GovernedPaths {
        /// Blobs referenced by journal entries newer than this many days are always kept (default: 30).
        #[arg(long, default_value = "30")]
        retain_days: u32,
        /// Show what would be removed without making changes.
        #[arg(long)]
        dry_run: bool,
    },
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum Commands {
    // ── VERB-SET CONSOLIDATION (v0.17.0.12.16) ────────────────────────────────
    //
    // Primary CLI surface per docs/design/ta-concepts-and-architecture.md §5/§11:
    // ten orthogonal verbs, nouns as positional subjects
    // (`ta <verb> <noun> [id] [flags]`). `run` (below) and `draft`'s
    // `apply`/`approve`/`deny` already fit this shape and need no change.
    // See `apps/ta-cli/src/commands/verb.rs` for the noun/verb mapping table.
    /// Create a resource (provisioning) — replaces New/Init/Add/Install.
    ///
    /// Examples:
    ///   ta create persona reviewer
    ///   ta create plugin ./plugins/my-plugin
    ///   ta create agent my-framework
    ///
    /// Run `ta create --help` to see the full noun list, or docs/USAGE.md's
    /// CLI Verb Reference for the old→new lookup table.
    Create {
        /// Resource kind (e.g. "goal", "persona", "plugin", "agent").
        noun: String,
        /// Resource ID/name, if applicable.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// List resources of a given kind — replaces the 15+ independent List implementations.
    ///
    /// Examples: `ta list goal`, `ta list draft`, `ta list plugin`
    List {
        /// Resource kind (e.g. "goal", "draft", "session").
        noun: String,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Show single-item detail — replaces View/Status/Inspect naming inconsistency.
    ///
    /// Examples: `ta show goal <id>`, `ta show draft <id>`, `ta show agent <name>`
    Show {
        /// Resource kind.
        noun: String,
        /// Resource ID/name.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Update a resource — replaces Set/Assign/MoveItem/AddItem/Reload.
    ///
    /// Examples: `ta update team implementer claude-opus-4-8`, `ta update persona reviewer --agent auto`
    Update {
        /// Resource kind.
        noun: String,
        /// Resource ID/name.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Remove a resource — replaces Delete/Remove/Revoke/Uninstall.
    ///
    /// Examples: `ta remove goal <id>`, `ta remove plugin <name>`
    ///
    /// `ta remove goal` fixed the phase-reset-even-when-done bug (found
    /// 2026-07-03) as part of this unification: a `done` plan phase is never
    /// reset to `pending` on goal removal, only an `in_progress` one is.
    Remove {
        /// Resource kind.
        noun: String,
        /// Resource ID/name.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Approve a draft (or other Decision-gated resource) for the next graph stage.
    ///
    /// Already fits the verb+noun shape via `ta draft approve` — this is the
    /// top-level spelling. Example: `ta approve draft <id>`
    Approve {
        /// Resource kind (currently only "draft").
        noun: String,
        /// Resource ID.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Deny a draft (or other Decision-gated resource) with a reason.
    ///
    /// Example: `ta deny draft <id> --reason "..."`
    Deny {
        /// Resource kind (currently only "draft").
        noun: String,
        /// Resource ID.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Fire the Commit stage for a resource — TA's defining action.
    ///
    /// Already fits the verb+noun shape via `ta draft apply` — this is the
    /// top-level spelling. Example: `ta apply draft <id>`
    Apply {
        /// Resource kind (currently only "draft").
        noun: String,
        /// Resource ID.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Correctness check — replaces Validate/Verify/Check/Audit naming inconsistency.
    ///
    /// Examples: `ta check plan-phase v0.15.0`, `ta check agent claude-code`
    Check {
        /// Resource kind.
        noun: String,
        /// Resource ID/name, if applicable.
        id: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },

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
        ///
        /// When omitted, resolves through the full `Switch` action tier
        /// hierarchy (v0.17.0.12.13): persona-level `agent` binding →
        /// workflow YAML `agent_framework` → [agent].default in
        /// .ta/workflow.toml → [workload_agents].<--workload> in
        /// .ta/workflow.toml → [agent].default in daemon.toml (falls back to
        /// legacy [agent].default_framework) → "claude-code". A literal
        /// "auto" at any tier hands the choice to the supervisor's
        /// recommendation (logged to .ta/agent-recommendations.jsonl).
        #[arg(long)]
        agent: Option<String>,
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
        /// Attach a file of context to the goal (compiler errors, build logs, stack traces, etc.).
        ///
        /// Contents are prepended to the agent's CLAUDE.md context block under
        /// "## User-Provided Context". Use `-` to read from stdin:
        ///   cargo test 2>&1 | ta run "fix failing tests" --context -
        #[arg(long, value_name = "PATH")]
        context: Option<PathBuf>,
        /// Workload type for the workload-type-level agent tier (v0.17.0.12.13).
        ///
        /// Looked up in .ta/workflow.toml's [workload_agents] table (e.g.
        /// `bugfix = "claude-opus-4-8"`). Full automatic workload
        /// classification lands in v0.17.0.12.20 (`ta-brain`); until then this
        /// is an explicit, opt-in override.
        #[arg(long)]
        workload: Option<String>,
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
    ///
    /// Subcommands:
    ///   ta gc governed-paths  — remove unreferenced SHA blobs from managed paths
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
        /// Governed-paths GC subcommand.
        #[command(subcommand)]
        gc_subcommand: Option<GcSubcommand>,
    },
    /// System-wide health check: runtime chain, auth validation, agent binaries, daemon, VCS.
    ///
    /// Validates the full TA runtime and reports the active authentication mode for the
    /// configured agent framework. Auth checking is agent-agnostic — it reads the framework
    /// manifest and validates whichever auth method applies (API key, session, local service).
    ///
    /// Use `--fix` to interactively clean up detected health issues (disk pressure, stale
    /// goals, orphaned staging dirs, stale drafts). Use `--fix --yes` for non-interactive
    /// cleanup (same as `ta gc`).
    ///
    /// Examples:
    ///   ta doctor              # human-readable output
    ///   ta doctor --json       # machine-readable JSON for CI
    ///   ta doctor --fix        # diagnose + interactively offer to clean each finding
    ///   ta doctor --fix --yes  # diagnose + auto-fix all (non-interactive, same as ta gc)
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
        /// Diagnose + offer to fix each health issue interactively (v0.15.30.6).
        ///
        /// For each signal, prints the issue and proposed action, then prompts before acting.
        /// Combine with --yes for non-interactive mode (equivalent to `ta gc`).
        #[arg(long)]
        fix: bool,
        /// Apply all fixes automatically without prompting (use with --fix, v0.15.30.6).
        ///
        /// `ta doctor --fix --yes` is equivalent to `ta gc` but with richer output.
        #[arg(long)]
        yes: bool,
        /// Optional subcommand: "fix <component>".
        ///
        /// Currently supports:
        ///   ta doctor fix projfs   — enable the Windows Client-ProjFS feature (requires elevation)
        #[arg(trailing_var_arg = true, value_name = "SUBCOMMAND [COMPONENT]")]
        extra_args: Vec<String>,
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
    /// Manage your global developer style constitution (~/.config/ta/style.md).
    ///
    /// The style file is prepended to every `ta run` session under a
    /// `## Developer Style` heading. Build one with `ta style init` (interview),
    /// apply a curated template, import from a URL, or discover from a codebase.
    ///
    /// Examples:
    ///   ta style init                         # interactive interview
    ///   ta style template list                # list built-in templates
    ///   ta style template apply pragmatic     # apply a template
    ///   ta style import https://example.com/style.md
    ///   ta style discover                     # infer from current codebase
    ///   ta style show                         # print current style
    ///   ta style edit                         # open in $EDITOR
    ///   ta style clear                        # remove style file
    Style {
        #[command(subcommand)]
        command: commands::style::StyleCommands,
    },
    /// Manage the virtual team: agent role assignments, security levels, and personas.
    ///
    /// `ta team list` shows roles configured in .ta/team.toml.
    /// `ta team assign <role> <agent-id>` adds or updates a role assignment.
    ///
    /// Examples:
    ///   ta team list
    ///   ta team assign reviewer claude-sonnet-4-6 --security auto --persona strict-reviewer
    ///   ta team assign implementer claude-opus-4-8
    Team {
        #[command(subcommand)]
        command: commands::team::TeamCommands,
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
    /// Trigger layer (v0.17.0.12.19): data-defined trigger types that feed goal creation.
    ///
    /// Per-type trigger configs live at `.ta/triggers/<type>.toml`, the same
    /// data-defined pattern used for plugins and personas.
    ///
    /// Examples:
    ///   ta intake list
    ///   ta intake fire schedule
    ///   ta intake fire inbound-email --dry-run
    ///   ta intake queue
    Intake {
        #[command(subcommand)]
        command: commands::intake::IntakeCommands,
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

    /// Effort and KPI analytics via Meridian (v0.17.0.12).
    ///
    /// Delegates to the `meridian` binary on PATH. TA emits token counts and
    /// timing data in `.ta/velocity-history.jsonl` so Meridian can report
    /// cost-per-phase, throughput, and KPI alignment rather than time-as-proxy.
    ///
    /// When Meridian is installed, TA automatically adds it as an MCP sidecar
    /// to every goal — agents get meridian tools as native tool calls.
    ///
    /// Subcommands:
    ///   ta meridian analyze  — run KPI analysis against velocity data
    ///   ta meridian help     — list tools exposed by meridian serve (MCP)
    ///   ta meridian init     — create meridian.toml with starter KPI definitions
    ///   ta meridian suggest  — surface KPI alignment gaps with suggestions
    ///
    /// Install Meridian: cargo install meridian
    Meridian {
        #[command(subcommand)]
        command: commands::meridian::MeridianCommands,
    },
    /// Check and manage optional external tools used by TA.
    ///
    /// `ta tools list` — show all optional tools and whether they are installed.
    /// `ta tools install <name>` — install a specific tool by name.
    ///
    /// Optional tools are listed in EXTERNAL_TOOLS (commands/tools.rs) and
    /// documented in plugins/<name>/plugin.toml. Adding a new tool requires
    /// one entry in EXTERNAL_TOOLS and a plugin.toml manifest.
    ///
    /// Examples:
    ///   ta tools list
    ///   ta tools install meridian
    ///   ta tools install claude-flow
    Tools {
        #[command(subcommand)]
        command: commands::tools::ToolsCommands,
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
    /// Manage this project's manifest (`.ta/project-manifest.md`).
    ///
    /// The manifest is a 1–2 page document describing this project's public
    /// interface. Other projects can link to it so agents automatically get
    /// cross-project context at goal start.
    ///
    /// Examples:
    ///   ta manifest init
    ///   ta manifest validate
    ///   ta manifest show
    ///   ta manifest show cinepipe-train
    Manifest {
        #[command(subcommand)]
        command: commands::manifest::ManifestCommands,
    },
    /// Manage cross-project links (`.ta/links.toml`).
    ///
    /// Linked projects' manifests are injected into agent context at goal start,
    /// giving agents cross-project awareness without copy-pasting documentation.
    ///
    /// Examples:
    ///   ta link add ../cinepipe-train --relationship workspace-member
    ///   ta link add github:myorg/pragma --relationship dependency
    ///   ta link list
    ///   ta link status
    ///   ta link refresh
    ///   ta link remove cinepipe-train
    Link {
        #[command(subcommand)]
        command: commands::link::LinkCommands,
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
    /// Sync the local workspace with upstream changes, or reconcile a resource
    /// with its remote/registry — replaces Gc/Prune/Migrate/reconcile-with-remote.
    ///
    /// With no noun: calls the configured VCS adapter's sync operation (e.g., git
    /// fetch + merge/rebase). Emits `sync_completed` or `sync_conflict` events.
    /// Configure sync behavior in `[source.sync]` in `.ta/workflow.toml`.
    ///
    /// With a noun: reconciles that resource, e.g. `ta sync goal` (garbage-collect
    /// zombie goals), `ta sync agent` (migrate a framework), `ta sync community`
    /// (refresh cached resources).
    Sync {
        /// Resource kind to sync. Omit for the default VCS workspace sync.
        noun: Option<String>,
        /// Remaining flags, forwarded to the underlying command unchanged.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra: Vec<String>,
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

    /// Manage context compression via the headroom proxy (v0.17.0.7).
    ///
    /// Context compression routes agent API calls through a local headroom proxy
    /// that compresses tool outputs, logs, and file reads before they reach the
    /// Anthropic API — reducing token consumption by 60–95% and extending the
    /// effective context window.
    ///
    /// Subcommands: `status`, `enable`, `disable`.
    ///
    /// Examples:
    ///   ta compression status
    ///   ta compression enable
    ///   ta compression disable
    Compression {
        #[command(subcommand)]
        command: commands::compression::CompressionCommands,
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

/// Windows' default main-thread stack (1MB) is far smaller than
/// Linux/macOS's (8MB default). Debug builds' uninlined stack frames for
/// clap's generated subcommand dispatch can exceed that 1MB limit on
/// Windows even though the same code path is fine elsewhere — confirmed by
/// `scripts/diagnose-windows-stack-overflow.ps1` (PR #537): the crash is
/// 100% deterministic at the default stack and 100% clean at 64MB, with no
/// signs of unbounded/infinite recursion. Running the real work on a
/// spawned thread with an explicit larger stack fixes it for every
/// invocation of the binary, not just the two tests that first exposed it.
fn main() -> anyhow::Result<()> {
    match std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(run)
        .expect("failed to spawn ta worker thread")
        .join()
    {
        Ok(result) => result,
        Err(_) => {
            // The worker thread's own panic hook already printed the
            // panic message/backtrace; just propagate a nonzero exit.
            std::process::exit(101);
        }
    }
}

fn run() -> anyhow::Result<()> {
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

    dispatch_raw(command, &config, &project_root, cli.no_version_check, true)
}

/// Build the one-line, non-fatal deprecation notice for a legacy noun-first
/// invocation, pointing at the new verb+noun equivalent when one is mapped
/// (v0.17.0.12.16). A pure function (no I/O) so the exact wording is
/// unit-testable; `print_deprecation_notice` is the thin I/O wrapper.
fn deprecation_notice_text<T: std::fmt::Debug>(legacy_top: &str, cmd: &T) -> String {
    let action = commands::verb::action_word_from_debug(cmd);
    match commands::verb::new_form_for(legacy_top, &action) {
        Some(new_form) => format!(
            "[deprecated-cli] `ta {legacy_top} {action}` \u{2192} use `{new_form}` instead (same behavior). See docs/USAGE.md's CLI Verb Reference."
        ),
        None => format!(
            "[deprecated-cli] `ta {legacy_top}` subcommands are being consolidated into the 10-verb CLI (create/list/show/update/remove/run/approve/deny/apply/check/sync, v0.17.0.12.16). This action isn't mapped to a new verb yet and continues to work unchanged. See docs/USAGE.md."
        ),
    }
}

/// Print a one-line, non-fatal deprecation notice for a legacy noun-first
/// invocation. Never blocks execution — the legacy form keeps working.
/// Called exactly once per process, only for a directly-typed legacy
/// invocation (`dispatch_raw`'s `warn_legacy = true` path) — a verb+noun
/// invocation that internally forwards to the same legacy `Commands` variant
/// (`run_verb_noun`) always passes `warn_legacy = false`, so the notice never
/// double-prints and never prints for the new primary surface.
fn print_deprecation_notice<T: std::fmt::Debug>(legacy_top: &str, cmd: &T) {
    eprintln!("{}", deprecation_notice_text(legacy_top, cmd));
}

/// Resolve a `ta <verb> <noun> [id] [extra...]` invocation to its legacy
/// equivalent and dispatch it through the exact same code path as a direct
/// legacy invocation (`dispatch_raw` with `warn_legacy = false`) — the new
/// verb+noun surface and the legacy noun-first surface always execute
/// identical code, never two copies of the same behavior.
fn run_verb_noun(
    verb: &str,
    noun: &str,
    id: Option<&str>,
    extra: &[String],
    config: &GatewayConfig,
    project_root: &std::path::Path,
    no_version_check: bool,
) -> anyhow::Result<()> {
    let argv = commands::verb::resolve(verb, noun, id, extra)?;
    let parsed = Cli::try_parse_from(&argv)?;
    let resolved = parsed.command.ok_or_else(|| {
        anyhow::anyhow!(
            "internal error: `ta {verb} {noun}` resolved to an argv with no command (this is a bug in commands::verb::resolve, please report it)"
        )
    })?;
    dispatch_raw(&resolved, config, project_root, no_version_check, false)
}

/// The full command dispatch table. Shared by direct CLI invocation
/// (`main`, `warn_legacy = true`) and by verb+noun forwarding
/// (`run_verb_noun`, `warn_legacy = false`) so both surfaces run byte-identical
/// code for every command (v0.17.0.12.16).
fn dispatch_raw(
    command: &Commands,
    config: &GatewayConfig,
    project_root: &std::path::Path,
    no_version_check: bool,
    warn_legacy: bool,
) -> anyhow::Result<()> {
    match command {
        Commands::Status { deep } => commands::status::execute(config, *deep),
        Commands::Goal { command } => {
            if warn_legacy {
                print_deprecation_notice("goal", command);
            }
            commands::goal::execute(command, config)
        }
        Commands::Draft { command } => {
            if warn_legacy {
                print_deprecation_notice("draft", command);
            }
            commands::draft::execute(command, config)
        }
        Commands::Pr { command } => commands::pr::execute(command, config),
        Commands::Audit { command } => commands::audit::execute(command, config),
        // ── Verb-set consolidation (v0.17.0.12.16) — primary CLI surface ──
        Commands::Create { noun, id, extra } => run_verb_noun(
            "create",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::List { noun, extra } => run_verb_noun(
            "list",
            noun,
            None,
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::Show { noun, id, extra } => run_verb_noun(
            "show",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::Update { noun, id, extra } => run_verb_noun(
            "update",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::Remove { noun, id, extra } => run_verb_noun(
            "remove",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::Approve { noun, id, extra } => run_verb_noun(
            "approve",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::Deny { noun, id, extra } => run_verb_noun(
            "deny",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::Apply { noun, id, extra } => run_verb_noun(
            "apply",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
        Commands::Check { noun, id, extra } => run_verb_noun(
            "check",
            noun,
            id.as_deref(),
            extra,
            config,
            project_root,
            no_version_check,
        ),
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
            context,
            workload,
        } => {
            // First-run gate: warn if provider is not yet configured.
            commands::onboard::check_provider_configured(*skip_onboard_check)?;

            // Phase-aware title resolution: if the positional title looks like
            // a phase ID (e.g., "v0.9.8.1", "0.9.8.1", "phase 0.9.8.1"),
            // look it up in PLAN.md and use the phase title + set --phase.
            let (resolved_title, resolved_phase) = resolve_phase_title(title, phase, project_root);

            // Agent runtime resolution (v0.17.0.12.13): --agent → persona
            // binding → workflow YAML/workflow.toml → workload type →
            // daemon.toml → "claude-code" ("auto" at any tier hands off to
            // the supervisor's recommendation).
            let resolved_agent = commands::run::resolve_effective_agent_full(
                agent.as_deref(),
                workflow.as_deref(),
                persona.as_deref(),
                workload.as_deref(),
                resolved_title.as_deref(),
                project_root,
            );

            // serial-phases: dispatch to execute_serial_phases when --phases is provided.
            if workflow.as_deref() == Some("serial-phases") || phases.is_some() {
                if let Some(phase_list) = phases {
                    if !phase_list.is_empty() {
                        let run_title = resolved_title.as_deref().unwrap_or("Serial phases run");
                        return commands::run::execute_serial_phases(
                            config,
                            run_title,
                            &resolved_agent,
                            objective,
                            phase_list,
                            gates,
                            *quiet,
                        );
                    }
                }
            }

            // swarm: dispatch to execute_swarm when --sub-goals is provided.
            if !sub_goals.is_empty() {
                let run_title = resolved_title.as_deref().unwrap_or("Swarm run");
                return commands::run::execute_swarm(
                    config,
                    run_title,
                    &resolved_agent,
                    objective,
                    sub_goals,
                    gates,
                    *integrate,
                    *quiet,
                );
            }

            // Default: single-agent execution.
            commands::run::execute(
                config,
                resolved_title.as_deref(),
                &resolved_agent,
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
                context.as_deref(),
            )
        }
        Commands::Events { command } => {
            if warn_legacy {
                print_deprecation_notice("events", command);
            }
            commands::events::execute(command, config)
        }
        Commands::Token { command } => {
            if warn_legacy {
                print_deprecation_notice("token", command);
            }
            commands::token::execute(command, config)
        }
        Commands::Dev {
            agent,
            unrestricted,
        } => commands::dev::execute(
            config,
            project_root,
            agent.as_deref(),
            *unrestricted,
            no_version_check,
        ),
        Commands::Session { command } => {
            if warn_legacy {
                print_deprecation_notice("session", command);
            }
            commands::session::execute(command, config)
        }
        Commands::Plan { command } => {
            if warn_legacy {
                print_deprecation_notice("plan", command);
            }
            commands::plan::execute(command, config)
        }
        Commands::Persona { command } => {
            if warn_legacy {
                print_deprecation_notice("persona", command);
            }
            commands::persona::execute(command, config)
        }
        Commands::Context { command } => {
            if warn_legacy {
                print_deprecation_notice("context", command);
            }
            commands::context::execute(command, config)
        }
        Commands::Credentials { command } => {
            if warn_legacy {
                print_deprecation_notice("credentials", command);
            }
            commands::credentials::execute(command, config)
        }
        Commands::Advisor { command } => commands::advisor::execute(command, config),
        Commands::Advise { message, goal } => {
            commands::advisor::advise(config, message, goal.as_deref())
        }
        Commands::Agent { command } => {
            if warn_legacy {
                print_deprecation_notice("agent", command);
            }
            commands::agent::execute(command, config)
        }
        Commands::Style { command } => commands::style::execute(command),
        Commands::Team { command } => {
            if warn_legacy {
                print_deprecation_notice("team", command);
            }
            commands::team::execute(command, config)
        }
        Commands::Constitution { command } => commands::constitution::execute(command, config),
        Commands::Memory { command } => commands::memory::execute(command, config),
        Commands::Adapter { command } => commands::adapter::execute(command, project_root),
        Commands::Install => commands::install::execute(project_root),
        Commands::Setup { command } => commands::setup::execute(command, config),
        Commands::Init { command } => commands::init::execute(command, config),
        Commands::New { command } => commands::new::execute(command, config),
        Commands::Release { command } => commands::release::execute(command, config),
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
                    project_root,
                    attach.as_deref(),
                    url.as_deref(),
                    *init,
                    *classic,
                    no_version_check,
                )
            } else if *init {
                commands::shell::init_config(project_root)
            } else {
                commands::shell::open_web_shell(project_root, url.as_deref())
            }
        }
        Commands::Daemon { command } => {
            if warn_legacy {
                print_deprecation_notice("daemon", command);
            }
            commands::daemon::execute(command, project_root)
        }
        Commands::Office { command } => {
            if warn_legacy {
                print_deprecation_notice("office", command);
            }
            commands::office::execute(command, project_root)
        }
        Commands::Plugin { command } => {
            if warn_legacy {
                print_deprecation_notice("plugin", command);
            }
            commands::plugin::run_plugin(project_root, command)?;
            Ok(())
        }
        Commands::Intake { command } => commands::intake::run_intake(project_root, command),
        Commands::Template { command } => {
            if warn_legacy {
                print_deprecation_notice("template", command);
            }
            commands::template::execute(command, config)
        }
        Commands::Publish { message, yes } => {
            commands::publish::execute(project_root, message.as_deref(), *yes)
        }
        Commands::Workflow { command } => {
            if warn_legacy {
                print_deprecation_notice("workflow", command);
            }
            commands::workflow::execute(command, config)
        }
        Commands::Stats { command } => commands::stats::execute(command, config),
        Commands::Community { command } => {
            if warn_legacy {
                print_deprecation_notice("community", command);
            }
            commands::community::execute(command, config)
        }
        Commands::Manifest { command } => commands::manifest::execute(command, config),
        Commands::Link { command } => commands::link::execute(command, config),
        Commands::Policy { command } => commands::policy::execute(command, config),
        Commands::Config { command } => commands::config::execute(command, config),
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
            gc_subcommand,
        } => match gc_subcommand {
            Some(GcSubcommand::GovernedPaths {
                retain_days,
                dry_run: gp_dry_run,
            }) => commands::gc::execute_governed_paths(config, *retain_days, *gp_dry_run),
            None => commands::gc::execute(
                config,
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
        },
        Commands::Operations { command } => commands::operations::execute(command, config),
        Commands::Runbook { command } => commands::runbook::execute(command, config),
        Commands::Connector { command } => {
            if warn_legacy {
                print_deprecation_notice("connector", command);
            }
            commands::connector::execute(command, config)
        }
        Commands::Compression { command } => commands::compression::execute(command, config),
        Commands::Webhook { command } => commands::webhook::execute(command, config),
        Commands::Meridian { command } => commands::meridian::execute(command, config),
        Commands::Tools { command } => commands::tools::execute(command),
        Commands::Serve => {
            // First-run gate: warn if provider is not yet configured.
            // TA_SKIP_ONBOARD_CHECK=1 bypasses in CI.
            let skip = std::env::var("TA_SKIP_ONBOARD_CHECK").is_ok_and(|v| v == "1");
            commands::onboard::check_provider_configured(skip)?;
            commands::serve::execute(project_root)
        }
        Commands::Build { test } => commands::build::execute(config, *test),
        Commands::Sync { noun, extra } => match noun {
            Some(n) => run_verb_noun(
                "sync",
                n,
                None,
                extra,
                config,
                project_root,
                no_version_check,
            ),
            None => commands::sync::execute(config),
        },
        Commands::Verify { goal_id } => commands::verify::execute(config, goal_id.as_deref()),
        Commands::Analysis { command } => commands::analysis::execute(command, config),
        Commands::Doctor {
            json,
            fix_denied,
            fix,
            yes,
            extra_args,
        } => match extra_args.as_slice() {
            [sub, comp, ..] if sub == "fix" && comp == "projfs" => {
                commands::doctor::execute_fix_projfs()
            }
            _ => commands::doctor::execute(config, *json, *fix_denied, *fix, *yes),
        },
        Commands::Upgrade(args) => commands::upgrade::execute(config, args),
        Commands::Conversation { goal_id, json } => {
            commands::conversation::execute(config, goal_id, *json)
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
            project_root,
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

#[cfg(test)]
mod verb_dispatch_tests {
    use super::*;

    /// Parse a `ta ...` argv into `Commands` the same way the real binary does.
    ///
    /// Runs on a dedicated large-stack thread: `Commands` is a large enum
    /// (the `Run` variant alone has ~25 fields, `#[allow(clippy::large_enum_variant)]`
    /// above), and clap's derive-generated parser builds it on the stack —
    /// large enough to overflow the default ~2MB test-thread stack.
    fn parse(args: &[&str]) -> Commands {
        let owned: Vec<String> = std::iter::once("ta".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect();
        parse_argv(owned)
    }

    /// Same as `parse`, but takes an already-built argv (e.g. from
    /// `commands::verb::resolve`) instead of individual args.
    fn parse_argv(argv: Vec<String>) -> Commands {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                Cli::try_parse_from(&argv)
                    .unwrap_or_else(|e| panic!("failed to parse {argv:?}: {e}"))
                    .command
                    .unwrap_or_else(|| panic!("no command parsed from {argv:?}"))
            })
            .expect("spawn parse thread")
            .join()
            .expect("parse thread panicked")
    }

    #[test]
    fn parses_all_ten_verb_shapes() {
        assert!(matches!(
            parse(&["create", "persona", "reviewer"]),
            Commands::Create { noun, id: Some(id), .. } if noun == "persona" && id == "reviewer"
        ));
        assert!(matches!(
            parse(&["list", "goal"]),
            Commands::List { noun, .. } if noun == "goal"
        ));
        assert!(matches!(
            parse(&["show", "goal", "abc123"]),
            Commands::Show { noun, id: Some(id), .. } if noun == "goal" && id == "abc123"
        ));
        assert!(matches!(
            parse(&["update", "team", "implementer", "claude-opus-4-8"]),
            Commands::Update { noun, id: Some(id), .. } if noun == "team" && id == "implementer"
        ));
        assert!(matches!(
            parse(&["remove", "goal", "abc123"]),
            Commands::Remove { noun, id: Some(id), .. } if noun == "goal" && id == "abc123"
        ));
        assert!(matches!(
            parse(&["approve", "draft", "d1"]),
            Commands::Approve { noun, id: Some(id), .. } if noun == "draft" && id == "d1"
        ));
        assert!(matches!(
            parse(&["deny", "draft", "d1", "--reason", "no"]),
            Commands::Deny { noun, id: Some(id), .. } if noun == "draft" && id == "d1"
        ));
        assert!(matches!(
            parse(&["apply", "draft", "d1"]),
            Commands::Apply { noun, id: Some(id), .. } if noun == "draft" && id == "d1"
        ));
        assert!(matches!(
            parse(&["check", "plan-phase", "v0.15.0"]),
            Commands::Check { noun, id: Some(id), .. } if noun == "plan-phase" && id == "v0.15.0"
        ));
        assert!(matches!(
            parse(&["sync", "goal"]),
            Commands::Sync { noun: Some(n), .. } if n == "goal"
        ));
        // `ta sync` with no noun still parses (bare VCS sync, unchanged).
        assert!(matches!(
            parse(&["sync"]),
            Commands::Sync { noun: None, .. }
        ));
    }

    #[test]
    fn extra_flags_pass_through_the_trailing_var_arg() {
        match parse(&["remove", "goal", "abc123", "--reason", "stale work"]) {
            Commands::Remove { noun, id, extra } => {
                assert_eq!(noun, "goal");
                assert_eq!(id.as_deref(), Some("abc123"));
                assert_eq!(extra, vec!["--reason", "stale work"]);
            }
            other => panic!("expected Commands::Remove, got {other:?}"),
        }
    }

    /// End-to-end proof that the new verb+noun surface and the legacy
    /// noun-first surface resolve to the *exact same* `Commands` value —
    /// not just equivalent-looking ones. `ta remove goal <id>` must
    /// round-trip through `commands::verb::resolve` + `Cli::try_parse_from`
    /// to literally `Commands::Goal { command: GoalCommands::Delete { .. } }`,
    /// the same variant `ta goal delete <id>` produces directly. Both are
    /// then dispatched by the same `dispatch_raw`, so behavior is identical
    /// by construction, including the goal-delete phase-reset-even-when-done
    /// fix (v0.17.0.12.16 item 3) — there is only one `delete_goal` code path.
    #[test]
    fn remove_goal_resolves_to_the_same_command_as_legacy_goal_delete() {
        let legacy = parse(&["goal", "delete", "abc123", "--reason", "no longer needed"]);
        let argv = commands::verb::resolve(
            "remove",
            "goal",
            Some("abc123"),
            &["--reason".to_string(), "no longer needed".to_string()],
        )
        .unwrap();
        let via_verb = parse_argv(argv);

        match (&legacy, &via_verb) {
            (
                Commands::Goal {
                    command:
                        commands::goal::GoalCommands::Delete {
                            id: id1,
                            reason: r1,
                        },
                },
                Commands::Goal {
                    command:
                        commands::goal::GoalCommands::Delete {
                            id: id2,
                            reason: r2,
                        },
                },
            ) => {
                assert_eq!(id1, id2);
                assert_eq!(r1, r2);
            }
            other => panic!("expected both to resolve to Goal::Delete, got {other:?}"),
        }
    }

    #[test]
    fn draft_apply_top_level_resolves_to_same_command_as_draft_apply() {
        let legacy = parse(&["draft", "apply", "d1"]);
        let argv = commands::verb::resolve("apply", "draft", Some("d1"), &[]).unwrap();
        let via_verb = parse_argv(argv);
        assert!(matches!(legacy, Commands::Draft { .. }));
        assert!(matches!(via_verb, Commands::Draft { .. }));
    }

    #[test]
    fn deprecation_notice_names_the_new_form_when_mapped() {
        let cmd = commands::goal::GoalCommands::Delete {
            id: "abc123".to_string(),
            reason: None,
        };
        let text = deprecation_notice_text("goal", &cmd);
        assert!(text.contains("ta goal delete"));
        assert!(text.contains("ta remove goal"));
        assert!(text.starts_with("[deprecated-cli]"));
    }

    #[test]
    fn deprecation_notice_is_generic_for_unmapped_actions() {
        let cmd = commands::goal::GoalCommands::Input {
            id: "abc123".to_string(),
            text: "hello".to_string(),
        };
        let text = deprecation_notice_text("goal", &cmd);
        assert!(text.starts_with("[deprecated-cli]"));
        assert!(text.contains("isn't mapped to a new verb yet"));
    }

    #[test]
    fn unknown_noun_via_new_surface_is_a_clean_error_not_a_panic() {
        let err = commands::verb::resolve("list", "spaceship", None, &[]).unwrap_err();
        assert!(err.to_string().contains("Unknown noun"));
    }
}
