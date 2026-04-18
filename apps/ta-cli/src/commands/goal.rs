// goal.rs — Goal subcommands: start, list, status, constitution.

use std::cmp::Reverse;
use std::path::PathBuf;

use clap::Subcommand;
use ta_goal::{
    GoalHistoryLedger, GoalOutcome, GoalRun, GoalRunState, GoalRunStore, HistoryFilter,
    VelocityEntry, VelocityStore,
};
use ta_mcp_gateway::GatewayConfig;
use ta_policy::constitution::{
    AccessConstitution, ConstitutionEntry, ConstitutionStore, EnforcementMode,
};
use ta_workspace::OverlayWorkspace;
use uuid::Uuid;

/// Resolve a draft ID prefix to the goal that produced it.
/// Scans all goals for matching `pr_package_id`.
fn resolve_draft_to_goal(
    prefix: &str,
    goals: &[ta_goal::GoalRun],
    _config: &GatewayConfig,
) -> Option<Uuid> {
    for goal in goals {
        if let Some(pr_id) = goal.pr_package_id {
            if pr_id.to_string().starts_with(prefix) {
                return Some(goal.goal_run_id);
            }
        }
    }
    None
}

/// Check if the parent's staging directory can be reused (exists and config allows it).
///
/// Returns `Some(parent_goal)` if eligible, `None` otherwise.
/// The caller decides whether to actually extend (based on user prompt or forced decision).
fn check_parent_staging_eligible(
    store: &GoalRunStore,
    parent_goal_id: Uuid,
    config: &GatewayConfig,
) -> anyhow::Result<Option<ta_goal::GoalRun>> {
    let parent = store
        .get(parent_goal_id)?
        .ok_or_else(|| anyhow::anyhow!("Parent goal {} not found", parent_goal_id))?;

    // Check if parent staging directory still exists.
    if !parent.workspace_path.exists() {
        return Ok(None);
    }

    // Load workflow config for follow-up preferences.
    let workflow_config = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );

    if workflow_config.follow_up.default_mode == "standalone" {
        return Ok(None);
    }

    Ok(Some(parent))
}

/// Decide whether a follow-up goal should extend the parent's staging directory.
///
/// When `explicit` is true (user passed `--follow-up-goal <id>`), the intent is
/// unambiguous — skip confirmation and reuse staging directly. When false (auto-
/// detected follow-up), prompt for confirmation since the user may not have
/// intended to reuse staging.
///
/// Returns `Some(parent_goal)` if the parent's staging exists and should be reused,
/// or `None` if a fresh staging copy should be created.
fn should_extend_parent_staging(
    store: &GoalRunStore,
    parent_goal_id: Uuid,
    config: &GatewayConfig,
    explicit: bool,
) -> anyhow::Result<Option<ta_goal::GoalRun>> {
    let parent = match check_parent_staging_eligible(store, parent_goal_id, config)? {
        Some(p) => p,
        None => return Ok(None),
    };

    // Show the parent's draft info.
    if let Some(pr_id) = parent.pr_package_id {
        eprintln!(
            "Parent goal \"{}\" has staging at {} (draft: {})",
            parent.title,
            parent.workspace_path.display(),
            &pr_id.to_string()[..8]
        );
    } else {
        eprintln!(
            "Parent goal \"{}\" has staging at {}",
            parent.title,
            parent.workspace_path.display()
        );
    }

    // Explicit follow-up (--follow-up-goal <id>): no confirmation needed.
    // The user already specified exactly which goal to extend.
    if explicit {
        eprintln!("Reusing staging (explicit --follow-up-goal).");
        return Ok(Some(parent));
    }

    // Auto-detected follow-up: prompt for confirmation.
    eprint!("Continue in staging for \"{}\"? [Y/n] ", parent.title);

    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_ok() {
        let trimmed = input.trim().to_lowercase();
        if trimmed.is_empty() || trimmed == "y" || trimmed == "yes" {
            return Ok(Some(parent));
        }
    }

    Ok(None)
}

/// Start a follow-up goal that extends the parent's staging directory.
/// Used by both the interactive path and tests.
#[allow(clippy::too_many_arguments)]
pub fn start_goal_extending_parent(
    config: &GatewayConfig,
    store: &GoalRunStore,
    title: &str,
    objective: &str,
    agent: &str,
    phase: Option<&str>,
    parent: &ta_goal::GoalRun,
    parent_goal_id: Uuid,
) -> anyhow::Result<ta_goal::GoalRun> {
    let mut goal = ta_goal::GoalRun::new(
        title,
        objective,
        agent,
        parent.workspace_path.clone(),
        config.store_dir.join(Uuid::new_v4().to_string()),
    );
    goal.parent_goal_id = Some(parent_goal_id);
    goal.store_path = config.store_dir.join(goal.goal_run_id.to_string());
    goal.source_dir = parent.source_dir.clone();
    goal.plan_phase = phase.map(|p| p.to_string());
    // Reuse the parent's source snapshot so diffs are against the original source.
    goal.source_snapshot = parent.source_snapshot.clone();

    goal.transition(GoalRunState::Configured)?;
    goal.transition(GoalRunState::Running)?;

    store.save_with_tag(&mut goal)?;
    Ok(goal)
}

#[derive(Subcommand)]
pub enum GoalCommands {
    /// Start a new goal run with an overlay workspace.
    Start {
        /// Goal title (e.g., "Fix authentication bug").
        title: String,
        /// Source directory to overlay (defaults to project root).
        #[arg(long)]
        source: Option<PathBuf>,
        /// Detailed objective for the goal.
        #[arg(long, default_value = "")]
        objective: String,
        /// Agent identity.
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Plan phase this goal implements (e.g., "4b").
        #[arg(long)]
        phase: Option<String>,
        /// Follow up on a previous goal (ID prefix or omit for latest).
        #[arg(long)]
        follow_up: Option<Option<String>>,
        /// Read objective from a file instead of --objective.
        #[arg(long)]
        objective_file: Option<PathBuf>,
    },
    /// List goal runs (default: active only; use --all for everything).
    List {
        /// Filter by state (e.g., "running", "pr_ready", "completed").
        #[arg(long)]
        state: Option<String>,
        /// Show only non-terminal goals (default behavior).
        #[arg(long)]
        active: bool,
        /// Show all goals including terminal states.
        #[arg(long)]
        all: bool,
    },
    /// Show details for a specific goal run.
    Status {
        /// Goal run ID.
        id: String,
        /// Output as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Delete a goal run and its staging directory.
    Delete {
        /// Goal run ID.
        id: String,
        /// Reason for deleting (recorded in the audit ledger).
        #[arg(long)]
        reason: Option<String>,
    },
    /// Manage access constitutions for goals (v0.4.3).
    Constitution {
        #[command(subcommand)]
        command: ConstitutionCommands,
    },
    /// Garbage-collect zombie goals and stale staging directories (v0.9.5.1).
    Gc {
        /// Show what would be cleaned without making changes.
        #[arg(long)]
        dry_run: bool,
        /// Also delete staging directories for terminal-state goals.
        #[arg(long)]
        include_staging: bool,
        /// Stale threshold in days (default: 7).
        #[arg(long, default_value = "7")]
        threshold_days: u32,
    },
    /// Detailed goal inspection: PID, process health, elapsed time, last event, staging path, draft state, agent log tail.
    Inspect {
        /// Goal run ID (or prefix).
        id: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Analyze a failed/stuck goal: timeline, last output, state transitions, errors, likely cause.
    PostMortem {
        /// Goal run ID (or prefix).
        id: String,
    },
    /// Check prerequisites before starting a goal: disk space, daemon, agent binary, VCS, env vars.
    PreFlight {
        /// Goal title (for context).
        title: Option<String>,
    },
    /// Browse the goal history ledger (archived/completed goals, v0.9.8.1).
    History {
        /// Filter by plan phase.
        #[arg(long)]
        phase: Option<String>,
        /// Filter by agent ID.
        #[arg(long)]
        agent: Option<String>,
        /// Filter by date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
        /// Output as raw JSONL.
        #[arg(long)]
        json: bool,
        /// Maximum entries to show (default: 20).
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Send text to a running goal's agent stdin (v0.12.4.1).
    ///
    /// Thin CLI wrapper over POST /api/goals/{id}/input for scripting and
    /// testing without a channel plugin. Use "latest" as the ID to route to
    /// the most recently started still-running goal.
    Input {
        /// Goal ID, short prefix, or "latest".
        id: String,
        /// Text to deliver to the agent's stdin.
        text: String,
    },

    /// Recover a goal from an incorrect state (v0.13.14).
    ///
    /// Handles common failure modes:
    ///   - `failed` state with a valid draft (watchdog race)
    ///   - `running` with a dead agent PID (zombie not cleaned up)
    ///   - `finalizing` stuck > 300s (draft creation interrupted)
    ///
    /// Use `--list` to see all recoverable goals without prompting.
    Recover {
        /// Goal ID or prefix. Omit to use --latest.
        id: Option<String>,
        /// Show all recoverable goals without prompting for recovery.
        #[arg(long)]
        list: bool,
        /// Recover the most recently affected goal.
        #[arg(long)]
        latest: bool,
    },

    /// Bulk cleanup of old goals and drafts (v0.14.7.2).
    ///
    /// Removes goal records, staging directories, and associated draft packages
    /// for terminal goals that are no longer needed. Always writes an audit record
    /// per purged goal. Refuses to purge active goals (Running, PrReady, UnderReview).
    ///
    /// Examples:
    ///   ta goal purge --state closed,denied,applied --older-than 30d
    ///   ta goal purge --id <id>
    ///   ta goal purge --state completed --older-than 7d --dry-run
    Purge {
        /// Remove a specific goal by ID or prefix.
        #[arg(long)]
        id: Option<String>,
        /// Filter by comma-separated terminal states to purge (e.g., "closed,denied,applied,completed").
        /// Only terminal states are accepted; active states are refused.
        #[arg(long)]
        state: Option<String>,
        /// Only purge goals older than this duration (e.g., "7d", "30d", "90d").
        #[arg(long)]
        older_than: Option<String>,
        /// Show what would be removed without making changes.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Access constitution subcommands (v0.4.3).
#[derive(Subcommand)]
pub enum ConstitutionCommands {
    /// View the access constitution for a goal.
    View {
        /// Goal run ID.
        goal_id: String,
    },
    /// Create or update an access constitution for a goal.
    Set {
        /// Goal run ID.
        goal_id: String,
        /// URI patterns to declare (repeatable, format: "pattern:intent").
        #[arg(long = "access", required = true)]
        access_entries: Vec<String>,
        /// Enforcement mode: warning (default) or error.
        #[arg(long, default_value = "warning")]
        enforcement: String,
    },
    /// Propose an access constitution based on an agent's historical patterns.
    Propose {
        /// Goal run ID.
        goal_id: String,
        /// Agent ID to base the proposal on.
        #[arg(long)]
        agent: Option<String>,
    },
    /// List all goals that have constitutions.
    List,
    /// Verify command behavior against TA-CONSTITUTION.md rules (v0.11.3).
    ///
    /// Checks current workspace state against constitutional invariants:
    /// branch restoration, injection cleanup, audit completeness, etc.
    Verify {
        /// Goal run ID (optional — if omitted, checks all active goals).
        goal_id: Option<String>,
    },
}

/// Find a parent goal by ID prefix (goal ID or draft ID), or return the latest goal if no prefix given.
fn find_parent_goal(
    store: &GoalRunStore,
    id_prefix: Option<&str>,
    config: &GatewayConfig,
) -> anyhow::Result<Uuid> {
    match id_prefix {
        Some(prefix) => {
            // Match by goal ID prefix (first N characters).
            let all_goals = store.list()?;
            let matches: Vec<_> = all_goals
                .iter()
                .filter(|g| g.goal_run_id.to_string().starts_with(prefix))
                .collect();

            match matches.len() {
                0 => {
                    // No goal matched — try matching as a draft ID and resolve to its goal.
                    if let Some(goal_id) = resolve_draft_to_goal(prefix, &all_goals, config) {
                        return Ok(goal_id);
                    }
                    anyhow::bail!("No goal or draft found matching prefix '{}'", prefix);
                }
                1 => Ok(matches[0].goal_run_id),
                _ => {
                    anyhow::bail!(
                        "Ambiguous prefix '{}' matches {} goals. Use a longer prefix.",
                        prefix,
                        matches.len()
                    )
                }
            }
        }
        None => {
            // Find the most recent goal (prefer unapplied, fall back to latest applied).
            let all_goals = store.list()?;
            if all_goals.is_empty() {
                anyhow::bail!(
                    "No previous goals found. Cannot use --follow-up without an existing goal."
                );
            }

            // Sort by updated_at descending.
            let mut sorted = all_goals.clone();
            sorted.sort_by_key(|g| Reverse(g.updated_at));

            // Prefer goals that haven't been applied yet.
            let unapplied = sorted
                .iter()
                .find(|g| !matches!(g.state, GoalRunState::Applied | GoalRunState::Completed));

            if let Some(goal) = unapplied {
                Ok(goal.goal_run_id)
            } else {
                // Fall back to the most recent goal.
                Ok(sorted[0].goal_run_id)
            }
        }
    }
}

/// Returns true if the goal subcommand is `start` — used by the terms gate in
/// main.rs to identify agent-spawning operations that require acceptance.
pub fn is_start_command(cmd: &GoalCommands) -> bool {
    matches!(cmd, GoalCommands::Start { .. })
}

pub fn execute(cmd: &GoalCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let store = GoalRunStore::new(&config.goals_dir)?;

    match cmd {
        GoalCommands::Start {
            title,
            source,
            objective,
            agent,
            phase,
            follow_up,
            objective_file,
        } => start_goal(
            config,
            &store,
            title,
            source.as_deref(),
            objective,
            agent,
            phase.as_deref(),
            follow_up.as_ref(),
            objective_file.as_deref(),
        ),
        GoalCommands::List { state, active, all } => {
            list_goals(&store, config, state.as_deref(), *active, *all)
        }
        GoalCommands::History {
            phase,
            agent,
            since,
            json,
            limit,
        } => goal_history(
            config,
            phase.as_deref(),
            agent.as_deref(),
            since.as_deref(),
            *json,
            *limit,
        ),
        GoalCommands::Status { id, json } => show_status(&store, config, id, *json),
        GoalCommands::Delete { id, reason } => delete_goal(&store, config, id, reason.as_deref()),
        GoalCommands::Constitution { command } => execute_constitution(command, config, &store),
        GoalCommands::Inspect { id, json } => goal_inspect(config, &store, id, *json),
        GoalCommands::PostMortem { id } => goal_post_mortem(config, &store, id),
        GoalCommands::PreFlight { title } => goal_pre_flight(config, title.as_deref()),
        GoalCommands::Gc {
            dry_run,
            include_staging,
            threshold_days,
        } => gc_goals(&store, config, *dry_run, *include_staging, *threshold_days),
        GoalCommands::Input { id, text } => goal_input(config, id, text),
        GoalCommands::Recover { id, list, latest } => {
            goal_recover(config, &store, id.as_deref(), *list, *latest)
        }
        GoalCommands::Purge {
            id,
            state,
            older_than,
            dry_run,
        } => purge_goals(
            config,
            &store,
            id.as_deref(),
            state.as_deref(),
            older_than.as_deref(),
            *dry_run,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn start_goal(
    config: &GatewayConfig,
    store: &GoalRunStore,
    title: &str,
    source: Option<&std::path::Path>,
    objective: &str,
    agent: &str,
    phase: Option<&str>,
    follow_up: Option<&Option<String>>,
    objective_file: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    // Resolve objective from file if specified.
    let final_objective = if let Some(obj_file) = objective_file {
        std::fs::read_to_string(obj_file)?
    } else if objective.is_empty() {
        title.to_string()
    } else {
        objective.to_string()
    };

    // Find parent goal if --follow-up is specified.
    let parent_goal_id = if let Some(follow_up_arg) = follow_up {
        Some(find_parent_goal(store, follow_up_arg.as_deref(), config)?)
    } else {
        None
    };

    let source_dir = match source {
        Some(p) => p.canonicalize()?,
        None => config.workspace_root.clone(),
    };

    // v0.4.1.2: Check if we should extend the parent's staging directory.
    // Explicit follow-up (user provided a specific ID) skips confirmation.
    let follow_up_explicit = follow_up.map(|f| f.is_some()).unwrap_or(false);
    let extend_parent = if let Some(pid) = parent_goal_id {
        should_extend_parent_staging(store, pid, config, follow_up_explicit)?
    } else {
        None
    };

    if let Some(ref parent) = extend_parent {
        // v0.4.1.2: Reuse parent's staging directory — no fresh copy needed.
        let pid = parent_goal_id.unwrap(); // safe: extend_parent is Some only when parent_goal_id is Some
        let goal = start_goal_extending_parent(
            config,
            store,
            title,
            &final_objective,
            agent,
            phase,
            parent,
            pid,
        )?;

        println!(
            "Goal started: {} (extending parent staging)",
            goal.goal_run_id
        );
        println!("  Title:   {}", goal.title);
        println!("  Parent:  {}", parent.goal_run_id);
        println!("  Staging: {} (reused)", goal.workspace_path.display());
        println!();
        println!("Agent workspace ready. To enter:");
        println!("  cd {}", goal.workspace_path.display());
    } else {
        // Fresh staging copy (original behavior or standalone follow-up).
        let mut goal = ta_goal::GoalRun::new(
            title,
            &final_objective,
            agent,
            PathBuf::new(), // placeholder — set after overlay creation
            config.store_dir.join("placeholder"), // placeholder
        );
        goal.parent_goal_id = parent_goal_id;
        let goal_id = goal.goal_run_id.to_string();

        // V1 TEMPORARY: Load exclude patterns, merging VCS adapter patterns
        // (e.g. ".git/" for Git) so VCS metadata is never captured in staging
        // diffs or overwritten on apply.
        let excludes = super::draft::load_excludes_with_adapter(&source_dir);

        // v0.13.13: Use configured staging strategy (default: Full).
        let workflow = ta_submit::config::WorkflowConfig::load_or_default(&source_dir);
        let staging_mode = match workflow.staging.strategy {
            ta_submit::config::StagingStrategy::Full => ta_workspace::OverlayStagingMode::Full,
            ta_submit::config::StagingStrategy::Smart => ta_workspace::OverlayStagingMode::Smart,
            ta_submit::config::StagingStrategy::RefsCow => {
                ta_workspace::OverlayStagingMode::RefsCow
            }
            ta_submit::config::StagingStrategy::ProjFs => ta_workspace::OverlayStagingMode::ProjFs,
        };
        let overlay = OverlayWorkspace::create_with_strategy(
            &goal_id,
            &source_dir,
            &config.staging_dir,
            excludes,
            staging_mode,
        )?;

        // v0.2.1: Capture source snapshot for conflict detection.
        let snapshot_json = overlay
            .snapshot()
            .and_then(|snap| serde_json::to_value(snap).ok());

        // Update goal with actual paths.
        goal.workspace_path = overlay.staging_dir().to_path_buf();
        goal.store_path = config.store_dir.join(&goal_id);
        goal.source_dir = Some(source_dir);
        goal.plan_phase = phase.map(|p| p.to_string());
        goal.source_snapshot = snapshot_json;

        // Transition: Created → Configured → Running.
        goal.transition(GoalRunState::Configured)?;
        goal.transition(GoalRunState::Running)?;

        store.save_with_tag(&mut goal)?;

        println!("Goal started: {}", goal.goal_run_id);
        if let Some(ref tag) = goal.tag {
            println!("  Tag:     {}", tag);
        }
        println!("  Title:   {}", goal.title);
        println!("  Staging: {}", overlay.staging_dir().display());
        println!();
        println!("Agent workspace ready. To enter:");
        println!("  cd {}", overlay.staging_dir().display());
    }

    Ok(())
}

/// Resolve a goal ID from a tag, full UUID, or an 8+ character prefix.
fn resolve_goal_id(id: &str, store: &GoalRunStore) -> anyhow::Result<Uuid> {
    // Try tag resolution first (v0.11.2.3).
    if let Ok(Some(g)) = store.resolve_tag(id) {
        return Ok(g.goal_run_id);
    }

    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(uuid);
    }

    if id.len() < 8 {
        anyhow::bail!(
            "No goal found matching '{}' (not a tag and too short for UUID prefix -- use at least 8 characters)",
            id
        );
    }

    let goals = store.list()?;
    let matches: Vec<_> = goals
        .iter()
        .filter(|g| g.goal_run_id.to_string().starts_with(id))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("No goal found matching '{}'", id),
        1 => Ok(matches[0].goal_run_id),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} goals. Use a longer prefix or a goal tag.",
            id,
            n
        ),
    }
}

fn list_goals(
    store: &GoalRunStore,
    config: &GatewayConfig,
    state: Option<&str>,
    active: bool,
    all: bool,
) -> anyhow::Result<()> {
    let mut goals = if let Some(state_filter) = state {
        store.list_by_state(state_filter)?
    } else {
        store.list()?
    };

    // Default: show active (non-terminal) goals unless --all is passed or a state filter is explicit.
    // v0.14.7.2: Also retain Failed goals that have a staging directory on disk — these are
    // recoverable and should not disappear from the default view.
    if !all && state.is_none() || active {
        goals.retain(|g| {
            if matches!(g.state, GoalRunState::Applied | GoalRunState::Completed) {
                return false;
            }
            if let GoalRunState::Failed { .. } = &g.state {
                // Only show Failed goals that have recoverable staging directories.
                return !g.workspace_path.as_os_str().is_empty() && g.workspace_path.exists();
            }
            true
        });
    }

    if goals.is_empty() {
        println!("No goal runs found.");
        return Ok(());
    }

    // Load draft packages to show inline draft/VCS status.
    let packages = load_all_packages_silent(config);

    println!(
        "{:<24} {:<10} {:<22} {:<26} {:<10} {:<10} {:<14}",
        "TAG", "ID", "TITLE", "STATE", "HEALTH", "DRAFT", "VCS"
    );
    println!("{}", "-".repeat(116));

    // v0.14.7.2: Track zombie goals for footer hint.
    let mut zombie_count = 0u32;
    // v0.14.7.2: Track recoverable failed goals for footer.
    let mut recoverable_failed = 0u32;

    for g in &goals {
        let tag = g.display_tag();
        let id_short = g.shortref();
        let title_display = if g.is_macro {
            format!("[M] {}", truncate(&g.title, 18))
        } else if let Some(ref macro_id) = g.parent_macro_id {
            format!(
                "  +- {} (<- {})",
                truncate(&g.title, 10),
                &macro_id.to_string()[..8]
            )
        } else if let Some(parent_id) = g.parent_goal_id {
            format!(
                "{} (-> {})",
                truncate(&g.title, 14),
                &parent_id.to_string()[..8]
            )
        } else {
            truncate(&g.title, 20)
        };

        // Process health check (v0.11.2.4).
        let health = process_health_label(g);

        // Find the latest draft for this goal.
        let (draft_col, vcs_col) = goal_draft_vcs_columns(g, &packages);

        // v0.13.17.2: Show "ta-building-draft [Xs]" for Finalizing goals instead of
        // the raw "finalizing" state, which was previously indistinguishable from a
        // red "no heartbeat" banner in some display contexts.
        // v0.14.7.2: Show "⚠ recoverable" for Failed goals with staging present.
        let state_display = if let GoalRunState::Finalizing {
            finalize_started_at,
            ..
        } = &g.state
        {
            let elapsed = (chrono::Utc::now() - *finalize_started_at)
                .num_seconds()
                .unsigned_abs();
            format!("building-draft [{}s]", elapsed)
        } else if matches!(&g.state, GoalRunState::Failed { .. }) {
            recoverable_failed += 1;
            "failed [⚠ recoverable]".to_string()
        } else if let GoalRunState::DraftPending { pending_since, .. } = &g.state {
            // v0.14.7.2: Show elapsed time for DraftPending goals.
            let elapsed = (chrono::Utc::now() - *pending_since)
                .num_seconds()
                .unsigned_abs();
            format!("draft_pending [{}s]", elapsed)
        } else {
            // v0.14.7.2: Detect zombie running goals (Running + dead PID).
            if g.state == GoalRunState::Running {
                if let Some(pid) = g.agent_pid {
                    if !is_process_alive(pid) {
                        zombie_count += 1;
                    }
                }
            }
            g.state.to_string()
        };

        println!(
            "{:<24} {:<10} {:<22} {:<26} {:<10} {:<10} {:<14}",
            tag, id_short, title_display, state_display, health, draft_col, vcs_col,
        );
    }
    println!("\n{} goal(s) total.", goals.len());

    // v0.14.7.2: Footer hints for recoverable and zombie goals.
    if recoverable_failed > 0 {
        println!(
            "  {} goal(s) marked recoverable. Run 'ta goal recover <id>' to inspect and recover work from staging.",
            recoverable_failed
        );
    }
    if zombie_count > 0 {
        println!(
            "  ⚠ {} zombie/stale goal(s) found (Running with dead PID). Run 'ta goal gc' to clean up.",
            zombie_count
        );
    }

    Ok(())
}

/// Load all draft packages silently (errors become empty vec).
fn load_all_packages_silent(
    config: &ta_mcp_gateway::GatewayConfig,
) -> Vec<ta_changeset::DraftPackage> {
    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return vec![];
    }
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                .filter_map(|e| {
                    std::fs::read_to_string(e.path())
                        .ok()
                        .and_then(|s| serde_json::from_str::<ta_changeset::DraftPackage>(&s).ok())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Get (draft_status, vcs_status) display columns for a goal.
fn goal_draft_vcs_columns(
    goal: &ta_goal::GoalRun,
    packages: &[ta_changeset::DraftPackage],
) -> (String, String) {
    // Find draft matching this goal.
    let goal_id_str = goal.goal_run_id.to_string();
    let draft = packages
        .iter()
        .filter(|p| p.goal.goal_id == goal_id_str)
        .max_by_key(|p| p.created_at);

    let draft_col = match draft {
        Some(d) => format!("{}", d.status),
        None => "\u{2014}".to_string(),
    };

    let vcs_col = match draft.and_then(|d| d.vcs_status.as_ref()) {
        Some(vcs) => {
            let pr_id = vcs
                .review_id
                .as_ref()
                .map(|id| format!("PR #{}", id))
                .unwrap_or_default();
            let state = vcs.review_state.as_deref().unwrap_or("?");
            if pr_id.is_empty() {
                vcs.branch.clone()
            } else {
                format!("{} ({})", pr_id, state)
            }
        }
        None => "\u{2014}".to_string(),
    };

    (draft_col, vcs_col)
}

fn goal_history(
    config: &GatewayConfig,
    phase: Option<&str>,
    agent: Option<&str>,
    since: Option<&str>,
    json: bool,
    limit: usize,
) -> anyhow::Result<()> {
    let ledger = GoalHistoryLedger::for_project(&config.workspace_root);
    let since_dt = if let Some(s) = since {
        Some(
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|e| anyhow::anyhow!("Invalid date '{}': {} (expected YYYY-MM-DD)", s, e))?
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc(),
        )
    } else {
        None
    };

    let filter = HistoryFilter {
        phase: phase.map(|s| s.to_string()),
        agent: agent.map(|s| s.to_string()),
        since: since_dt,
        limit: Some(limit),
    };

    let entries = ledger.read(&filter)?;

    if entries.is_empty() {
        println!("No history entries found.");
        return Ok(());
    }

    if json {
        for entry in &entries {
            println!("{}", serde_json::to_string(entry)?);
        }
        return Ok(());
    }

    println!(
        "{:<10} {:<30} {:<12} {:<8} {:<12} {:<8}",
        "ID", "TITLE", "STATE", "PHASE", "COMPLETED", "MINS"
    );
    println!("{}", "-".repeat(80));

    for entry in &entries {
        let completed = entry
            .completed
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "-".to_string());
        let duration = format!("{}", entry.duration_mins);

        println!(
            "{:<10} {:<30} {:<12} {:<8} {:<12} {:<8}",
            &entry.id.to_string()[..8],
            truncate(&entry.title, 28),
            entry.state,
            entry.phase.as_deref().unwrap_or("-"),
            completed,
            duration,
        );
    }
    println!("\n{} history entry(ies).", entries.len());

    Ok(())
}

/// Send text to a running goal's agent stdin via the daemon API (v0.12.4.1).
///
/// POST /api/goals/{id}/input — thin wrapper for scripting without a channel plugin.
fn goal_input(config: &GatewayConfig, id: &str, text: &str) -> anyhow::Result<()> {
    let daemon_url = super::daemon::resolve_daemon_url(&config.workspace_root, None);
    let url = format!("{}/api/goals/{}/input", daemon_url, id);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {}", e))?;

    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "input": text }))
        .send()
        .map_err(|e| {
            anyhow::anyhow!(
                "Cannot reach daemon at {}: {}. Is the daemon running? Try: ta daemon start",
                daemon_url,
                e
            )
        })?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .unwrap_or_else(|_| serde_json::json!({"error": "non-JSON response"}));

    if status.is_success() {
        let goal_id = body["goal_id"].as_str().unwrap_or(id);
        let len = body["input_length"].as_u64().unwrap_or(text.len() as u64);
        println!(
            "Delivered {} bytes to goal {} stdin.",
            len,
            &goal_id[..8.min(goal_id.len())]
        );
        Ok(())
    } else {
        let err = body["error"].as_str().unwrap_or("unknown error");
        let hint = body["hint"].as_str().unwrap_or("");
        if hint.is_empty() {
            anyhow::bail!(
                "Failed to deliver input to goal '{}': {} (HTTP {})",
                id,
                err,
                status
            )
        } else {
            anyhow::bail!(
                "Failed to deliver input to goal '{}': {} (HTTP {})\nHint: {}",
                id,
                err,
                status,
                hint
            )
        }
    }
}

/// Recover a goal from an incorrect state (v0.13.14).
///
/// Handles:
///   - `failed` with valid draft → restore to `pr_ready`
///   - `running` with dead PID → rebuild draft or cancel
///   - `finalizing` stuck > 300s → rebuild draft or cancel
fn goal_recover(
    config: &GatewayConfig,
    store: &GoalRunStore,
    id: Option<&str>,
    list_only: bool,
    latest: bool,
) -> anyhow::Result<()> {
    let all_goals = store.list()?;
    let now = chrono::Utc::now();

    // --- Collect recoverable goals ---
    let mut recoverable: Vec<(&GoalRun, String)> = Vec::new();
    for goal in &all_goals {
        let diagnosis = diagnose_goal(goal, &now, config);
        if let Some(diag) = diagnosis {
            recoverable.push((goal, diag));
        }
    }

    if list_only {
        if recoverable.is_empty() {
            println!("No goals in potentially-recoverable states.");
        } else {
            println!("{} recoverable goal(s):", recoverable.len());
            println!();
            for (goal, diag) in &recoverable {
                let short_id = &goal.goal_run_id.to_string()[..8];
                println!("  {} — {} ({})", short_id, goal.title, goal.state);
                println!("    {}", diag);
                if let Some(pkg_id) = goal.pr_package_id {
                    let short_pkg = &pkg_id.to_string()[..8];
                    let pkg_path = config
                        .workspace_root
                        .join(".ta/pr_packages")
                        .join(format!("{}.json", pkg_id));
                    if pkg_path.exists() {
                        println!("    Draft: {} (present)", short_pkg);
                    } else {
                        println!("    Draft: {} (missing)", short_pkg);
                    }
                }
                println!();
            }
        }
        return Ok(());
    }

    // --- Select goal to recover ---
    let target: &GoalRun = if let Some(id_prefix) = id {
        // User specified an ID prefix.
        let goal_id = resolve_goal_id(id_prefix, store)?;
        all_goals
            .iter()
            .find(|g| g.goal_run_id == goal_id)
            .ok_or_else(|| anyhow::anyhow!("Goal {} not found", id_prefix))?
    } else if latest || id.is_none() {
        // Use the most recently updated recoverable goal.
        recoverable.first().map(|(g, _)| *g).ok_or_else(|| {
            anyhow::anyhow!(
                "No recoverable goals found. Use `ta goal recover --list` to inspect all goals."
            )
        })?
    } else {
        anyhow::bail!("Specify a goal ID or use --latest / --list");
    };

    let diag = diagnose_goal(target, &now, config).unwrap_or_else(|| {
        format!(
            "Unknown issue — showing raw state for manual inspection (state: {})",
            target.state
        )
    });

    // --- Show diagnosis ---
    println!();
    println!(
        "Goal: \"{}\" ({})",
        target.title,
        &target.goal_run_id.to_string()[..8]
    );
    println!("Current state: {}", target.state);
    if let Some(pid) = target.agent_pid {
        println!("Agent PID: {}", pid);
    }
    if let Some(pkg_id) = target.pr_package_id {
        let pkg_path = config
            .workspace_root
            .join(".ta/pr_packages")
            .join(format!("{}.json", pkg_id));
        let pkg_status = if pkg_path.exists() {
            "present and valid"
        } else {
            "file missing"
        };
        println!("Draft: {} — {}", &pkg_id.to_string()[..8], pkg_status);
    } else {
        println!("Draft: none");
    }
    println!();
    println!("Detected issue: {}", diag);
    println!();

    // --- Choose recovery action ---
    let has_valid_draft = target.pr_package_id.is_some_and(|pkg_id| {
        config
            .workspace_root
            .join(".ta/pr_packages")
            .join(format!("{}.json", pkg_id))
            .exists()
    });

    if has_valid_draft {
        println!("Recovery options:");
        println!("  [1] Restore state to pr_ready (draft is valid, proceed to review)");
        println!("  [2] Rebuild draft from staging (re-run diff against current source)");
        println!("  [3] Mark as cancelled (discard goal and draft)");
        println!("  [4] Show goal JSON (inspect and exit)");
        println!("  [5] Abort (no changes)");
        println!();
        print!("Choice [1-5]: ");
    } else {
        println!("Recovery options:");
        println!("  [1] Rebuild draft from staging (re-run diff against current source)");
        println!("  [2] Mark as cancelled (discard goal and draft)");
        println!("  [3] Show goal JSON (inspect and exit)");
        println!("  [4] Abort (no changes)");
        println!();
        print!("Choice [1-4]: ");
    }

    use std::io::Write;
    std::io::stdout().flush().ok();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let choice: u32 = input.trim().parse().unwrap_or(0);

    if has_valid_draft {
        match choice {
            1 => {
                // Option 1: restore to pr_ready
                let pkg_id = target.pr_package_id.unwrap();
                if let Ok(Some(mut g)) = store.get(target.goal_run_id) {
                    // Force state to pr_ready directly (bypass normal state machine since
                    // we're recovering from an incorrect state).
                    g.state = GoalRunState::PrReady;
                    g.pr_package_id = Some(pkg_id);
                    g.updated_at = chrono::Utc::now();
                    store.save(&g)?;
                    println!();
                    println!(
                        "Restored goal {} to pr_ready state.",
                        &target.goal_run_id.to_string()[..8]
                    );
                    println!("Draft {} is ready for review.", &pkg_id.to_string()[..8]);
                    println!("Run: ta draft view {}", &pkg_id.to_string()[..8]);
                }
            }
            2 => {
                // Option 2: rebuild draft
                println!();
                println!("Rebuilding draft from staging...");
                super::draft::execute(
                    &super::draft::DraftCommands::Build {
                        goal_id: target.goal_run_id.to_string(),
                        summary: format!("Recovered draft for: {}", target.title),
                        latest: false,
                        apply_context_file: None,
                    },
                    config,
                )?;
                println!("Draft rebuilt. Run `ta draft list` to review.");
            }
            3 => {
                // Option 3: cancel
                if let Ok(Some(mut g)) = store.get(target.goal_run_id) {
                    g.state = GoalRunState::Failed {
                        reason: "Cancelled by user via ta goal recover".to_string(),
                    };
                    g.updated_at = chrono::Utc::now();
                    store.save(&g)?;
                    println!();
                    println!(
                        "Goal {} marked as cancelled.",
                        &target.goal_run_id.to_string()[..8]
                    );
                }
            }
            4 => {
                println!();
                println!("{}", serde_json::to_string_pretty(target)?);
            }
            _ => {
                println!("Aborted — no changes made.");
            }
        }
    } else {
        match choice {
            1 => {
                println!();
                println!("Rebuilding draft from staging...");
                super::draft::execute(
                    &super::draft::DraftCommands::Build {
                        goal_id: target.goal_run_id.to_string(),
                        summary: format!("Recovered draft for: {}", target.title),
                        latest: false,
                        apply_context_file: None,
                    },
                    config,
                )?;
                println!("Draft rebuilt. Run `ta draft list` to review.");
            }
            2 => {
                if let Ok(Some(mut g)) = store.get(target.goal_run_id) {
                    g.state = GoalRunState::Failed {
                        reason: "Cancelled by user via ta goal recover".to_string(),
                    };
                    g.updated_at = chrono::Utc::now();
                    store.save(&g)?;
                    println!();
                    println!(
                        "Goal {} marked as cancelled.",
                        &target.goal_run_id.to_string()[..8]
                    );
                }
            }
            3 => {
                println!();
                println!("{}", serde_json::to_string_pretty(target)?);
            }
            _ => {
                println!("Aborted — no changes made.");
            }
        }
    }

    Ok(())
}

/// A single checkpoint in the agent progress journal (v0.14.7.2).
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
struct ProgressCheckpoint {
    label: String,
    at: String,
    detail: String,
}

/// The agent progress journal stored at `.ta/ta-progress.json` in staging (v0.14.7.2).
///
/// Written by the agent during execution; survives process crashes so recovery
/// tools can show how far the agent got before failure.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
struct ProgressJournal {
    goal_id: String,
    checkpoints: Vec<ProgressCheckpoint>,
}

/// Try to load the progress journal from a staging workspace.
///
/// Returns `None` if the file is absent or unparseable.
fn load_progress_journal(workspace_path: &std::path::Path) -> Option<ProgressJournal> {
    let path = workspace_path.join(".ta").join("ta-progress.json");
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<ProgressJournal>(&content).ok()
}

/// Produce a plain-English diagnosis for a goal in a potentially-recoverable state.
///
/// Returns `None` if the goal is not in a recoverable state.
fn diagnose_goal(
    goal: &GoalRun,
    now: &chrono::DateTime<chrono::Utc>,
    config: &GatewayConfig,
) -> Option<String> {
    match &goal.state {
        GoalRunState::Failed { reason } => {
            // Check if a valid draft exists — indicates watchdog race.
            if let Some(pkg_id) = goal.pr_package_id {
                let pkg_path = config
                    .workspace_root
                    .join(".ta/pr_packages")
                    .join(format!("{}.json", pkg_id));
                if pkg_path.exists() {
                    return Some(
                        "Watchdog overrode clean exit with failed state — draft is valid."
                            .to_string(),
                    );
                }
            }
            // v0.14.7.2: Check if staging directory has agent work + progress journal.
            if !goal.workspace_path.as_os_str().is_empty() && goal.workspace_path.exists() {
                let last_checkpoint = load_progress_journal(&goal.workspace_path)
                    .and_then(|j| j.checkpoints.into_iter().last())
                    .map(|cp| {
                        format!(
                            "Last checkpoint: '{}' at {} — {}",
                            cp.label, cp.at, cp.detail
                        )
                    })
                    .unwrap_or_else(|| "(no progress journal)".to_string());

                // v0.15.6.2: Specific messaging for finalize_timeout failure.
                // The agent completed its work; only the draft build was interrupted.
                // Recovery re-runs ONLY the draft build step, not the full agent session.
                if reason.contains("finalize_timeout") || reason.contains("Finalizing timed out") {
                    return Some(format!(
                        "Finalizing timed out — agent work is complete, only draft build \
                         was interrupted. Staging directory is present. Rebuild the draft \
                         without re-running the agent. {}",
                        last_checkpoint
                    ));
                }

                return Some(format!(
                    "Failed with staging directory present — agent work may be recoverable. \
                     Reason: {}. {}",
                    reason.chars().take(120).collect::<String>(),
                    last_checkpoint
                ));
            }
            None
        }
        GoalRunState::Running => {
            // Check if PID is dead.
            if let Some(pid) = goal.agent_pid {
                if !is_process_alive(pid) {
                    return Some(format!(
                        "Goal is running but agent process PID {} is gone (zombie not cleaned up).",
                        pid
                    ));
                }
            } else {
                let age_secs = (*now - goal.updated_at).num_seconds().unsigned_abs();
                if age_secs > 3600 {
                    return Some(format!(
                        "Goal is running with no PID for {}s — may have been orphaned.",
                        age_secs
                    ));
                }
            }
            None
        }
        GoalRunState::Finalizing {
            finalize_started_at,
            run_pid,
            ..
        } => {
            let elapsed = (*now - *finalize_started_at).num_seconds().unsigned_abs();
            // v0.13.17.2: Always show Finalizing goals in recover (option: rebuild draft).
            // Previously only showed if elapsed > 1800s — but even fresh Finalizing goals
            // should be recoverable so users can manually rebuild without state transitions.
            let pid_status = if let Some(pid) = run_pid {
                if is_process_alive(*pid) {
                    format!(
                        "ta run process (PID {}) is alive — draft build in progress",
                        pid
                    )
                } else {
                    format!(
                        "ta run process (PID {}) is dead — draft build may have been interrupted",
                        pid
                    )
                }
            } else {
                "no ta run PID recorded".to_string()
            };
            if elapsed > 1800 {
                Some(format!(
                    "Draft creation stuck for {}s (threshold: 1800s). {}.",
                    elapsed, pid_status
                ))
            } else {
                Some(format!(
                    "Goal is building draft ({}s elapsed). {}. Use recover to rebuild if stuck.",
                    elapsed, pid_status
                ))
            }
        }
        _ => None,
    }
}

fn show_status(
    store: &GoalRunStore,
    config: &GatewayConfig,
    id: &str,
    json_output: bool,
) -> anyhow::Result<()> {
    let goal_run_id = resolve_goal_id(id, store)?;
    match store.get(goal_run_id)? {
        Some(g) => {
            if json_output {
                let json = serde_json::to_string_pretty(&g)?;
                println!("{}", json);
                return Ok(());
            }

            // Unified view: goal + draft + VCS in one output (v0.11.2.3).
            println!("Tag:      {}", g.display_tag());
            println!("Goal Run: {}", g.goal_run_id);
            println!("Title:    {}", g.title);
            println!("Objective: {}", g.objective);
            // v0.13.17.2: Show enriched state for Finalizing goals.
            if let GoalRunState::Finalizing {
                finalize_started_at,
                ..
            } = &g.state
            {
                let elapsed = (chrono::Utc::now() - *finalize_started_at)
                    .num_seconds()
                    .unsigned_abs();
                println!("State:    TA Building Draft [{}s elapsed]", elapsed);
            } else {
                println!("State:    {}", g.state);
            }
            if let Some(ref note) = g.progress_note {
                println!("Progress: {}", note);
            }
            if let Some(ref vcs) = g.vcs_isolation {
                println!("VCS:      {}", vcs);
            }
            if let Some(ref by) = g.initiated_by {
                println!("By:       {}", by);
            }
            println!("Agent:    {}", g.agent_id);
            println!("Created:  {}", g.created_at.to_rfc3339());
            println!("Updated:  {}", g.updated_at.to_rfc3339());
            if let Some(ref src) = g.source_dir {
                println!("Source:   {}", src.display());
            }
            if let Some(ref phase) = g.plan_phase {
                println!("Phase:    {}", phase);
            }
            if let Some(parent_id) = g.parent_goal_id {
                println!("Parent:   {} (follow-up)", parent_id);
            }
            if let Some(ref macro_id) = g.parent_macro_id {
                println!("Macro:    {} (sub-goal of macro)", macro_id);
            }
            if g.is_macro {
                println!("Mode:     macro goal (inner-loop iteration)");
            }
            println!("Staging:  {}", g.workspace_path.display());

            // Draft status section (v0.11.2.3).
            if let Some(pr_id) = g.pr_package_id {
                println!("\n--- Draft ---");
                println!("Draft ID: {}", pr_id);
                // Try to load the draft package for detailed status.
                {
                    let packages = load_all_packages_silent(config);
                    let goal_id_str = g.goal_run_id.to_string();
                    if let Some(draft) = packages.iter().find(|p| p.goal.goal_id == goal_id_str) {
                        println!("Status:   {}", draft.status);
                        println!("Files:    {}", draft.changes.artifacts.len());
                        if let Some(ref vcs) = draft.vcs_status {
                            println!("\n--- VCS ---");
                            println!("Branch:   {}", vcs.branch);
                            if let Some(ref url) = vcs.review_url {
                                println!("PR URL:   {}", url);
                            }
                            if let Some(ref id) = vcs.review_id {
                                if let Some(ref state) = vcs.review_state {
                                    println!("PR:       #{} ({})", id, state);
                                } else {
                                    println!("PR:       #{}", id);
                                }
                            }
                            if let Some(ref sha) = vcs.commit_sha {
                                println!("Commit:   {}", sha);
                            }
                            println!("Checked:  {}", vcs.last_checked.to_rfc3339());
                        }
                    }
                }
            } else {
                println!("Draft:    (none)");
            }

            // Show sub-goal tree for macro goals.
            if g.is_macro && !g.sub_goal_ids.is_empty() {
                println!("\nSub-goals ({}):", g.sub_goal_ids.len());
                for sub_id in &g.sub_goal_ids {
                    match store.get(*sub_id)? {
                        Some(sg) => {
                            let draft_status = sg
                                .pr_package_id
                                .map(|id| format!(" [draft: {}]", &id.to_string()[..8]))
                                .unwrap_or_default();
                            println!(
                                "  {} {} [{}]{}",
                                &sub_id.to_string()[..8],
                                truncate(&sg.title, 40),
                                sg.state,
                                draft_status,
                            );
                        }
                        None => {
                            println!("  {} (not found)", sub_id);
                        }
                    }
                }
            }
        }
        None => {
            eprintln!("Goal run not found: {}", id);
            std::process::exit(1);
        }
    }

    Ok(())
}

fn delete_goal(
    store: &GoalRunStore,
    config: &GatewayConfig,
    id: &str,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let goal_run_id = resolve_goal_id(id, store)?;
    let goal = store.get(goal_run_id)?;

    match goal {
        Some(g) => {
            // Determine disposition: abandoned if no draft was ever produced.
            let has_draft = g.pr_package_id.is_some();
            let is_terminal = matches!(
                g.state,
                GoalRunState::Applied | GoalRunState::Completed | GoalRunState::Failed { .. }
            );

            let disposition = if !has_draft && !is_terminal {
                ta_audit::AuditDisposition::Abandoned
            } else {
                ta_audit::AuditDisposition::Cancelled
            };

            // Record velocity entry for non-terminal goals being deleted.
            if !is_terminal {
                let cancel_msg = reason.unwrap_or("user deleted goal");
                let entry = VelocityEntry::from_goal(&g, GoalOutcome::Cancelled)
                    .with_cancel_reason(cancel_msg);
                let vs = VelocityStore::for_project(&config.workspace_root);
                let _ = vs.append(&entry);
            }

            // Write audit ledger entry before removing data.
            {
                let ledger_path = ta_audit::GoalAuditLedger::path_for(&config.workspace_root);
                if let Ok(mut ledger) = ta_audit::GoalAuditLedger::open(&ledger_path) {
                    let now = chrono::Utc::now();
                    let total = now.signed_duration_since(g.created_at).num_seconds();
                    let mut entry = ta_audit::AuditEntry {
                        goal_id: g.goal_run_id,
                        title: g.title.clone(),
                        objective: None,
                        disposition,
                        phase: g.plan_phase.clone(),
                        agent: g.agent_id.clone(),
                        created_at: g.created_at,
                        pr_ready_at: None,
                        recorded_at: now,
                        build_seconds: total,
                        review_seconds: 0,
                        total_seconds: total,
                        draft_id: g.pr_package_id,
                        ai_summary: None,
                        reviewer: None,
                        denial_reason: None,
                        cancel_reason: reason.map(|s| s.to_string()),
                        artifact_count: 0,
                        lines_changed: 0,
                        artifacts: Vec::new(),
                        policy_result: None,
                        parent_goal_id: g.parent_goal_id,
                        previous_hash: None,
                    };
                    if let Err(e) = ledger.append(&mut entry) {
                        tracing::warn!("Failed to write goal audit entry for delete: {}", e);
                    }
                }
            }

            // v0.15.13.5: Reset plan phase from in_progress → pending on delete/cancel.
            if let Some(ref phase_id) = g.plan_phase {
                let note = format!(
                    "phase reset to pending — goal cancelled ({})",
                    reason.unwrap_or("user deleted goal")
                );
                if let Err(e) =
                    super::plan::reset_phase_if_in_progress(&config.workspace_root, phase_id, &note)
                {
                    tracing::warn!(
                        phase = %phase_id,
                        error = %e,
                        "Failed to reset plan phase on goal delete"
                    );
                } else {
                    println!("Plan: phase {} reset to pending (goal cancelled)", phase_id);
                }
            }

            // Remove the staging directory if it exists.
            let workspace = &g.workspace_path;
            if workspace.exists() {
                std::fs::remove_dir_all(workspace)?;
                println!("Removed staging directory: {}", workspace.display());
            }

            // Remove goal metadata from the store.
            store.delete(goal_run_id)?;
            println!("Deleted goal: {} ({})", g.title, goal_run_id);
        }
        None => {
            eprintln!("Goal run not found: {}", id);
            std::process::exit(1);
        }
    }

    Ok(())
}

// ── Constitution subcommands (v0.4.3) ──

fn execute_constitution(
    cmd: &ConstitutionCommands,
    config: &GatewayConfig,
    goal_store: &GoalRunStore,
) -> anyhow::Result<()> {
    let store = ConstitutionStore::for_workspace(&config.workspace_root);

    match cmd {
        ConstitutionCommands::View { goal_id } => match store.load(goal_id)? {
            Some(c) => {
                println!("Access Constitution for goal {}", c.goal_id);
                println!("{}", "=".repeat(50));
                println!("Created by: {}", c.created_by);
                println!("Created at: {}", c.created_at.format("%Y-%m-%d %H:%M"));
                println!("Enforcement: {}", c.enforcement);
                println!();

                if c.access.is_empty() {
                    println!("  (no access entries declared)");
                } else {
                    println!("{:<50} INTENT", "PATTERN");
                    println!("{}", "-".repeat(80));
                    for entry in &c.access {
                        println!("{:<50} {}", entry.pattern, entry.intent);
                    }
                }
            }
            None => {
                println!("No access constitution found for goal {}", goal_id);
                println!(
                    "Create one with: ta goal constitution set {} --access 'pattern:intent'",
                    goal_id
                );
            }
        },

        ConstitutionCommands::Set {
            goal_id,
            access_entries,
            enforcement,
        } => {
            let enforcement_mode = match enforcement.as_str() {
                "error" => EnforcementMode::Error,
                _ => EnforcementMode::Warning,
            };

            let access: Vec<ConstitutionEntry> = access_entries
                .iter()
                .map(|entry| {
                    // Parse "pattern:intent" format.
                    if let Some((pattern, intent)) = entry.split_once(':') {
                        ConstitutionEntry {
                            pattern: pattern.trim().to_string(),
                            intent: intent.trim().to_string(),
                        }
                    } else {
                        ConstitutionEntry {
                            pattern: entry.trim().to_string(),
                            intent: "(no intent specified)".to_string(),
                        }
                    }
                })
                .collect();

            let constitution = AccessConstitution {
                goal_id: goal_id.clone(),
                created_by: "human".to_string(),
                created_at: chrono::Utc::now(),
                access,
                enforcement: enforcement_mode,
            };

            store.save(&constitution)?;
            println!(
                "Access constitution saved for goal {} ({} entries, enforcement: {})",
                goal_id,
                constitution.access.len(),
                enforcement_mode,
            );
        }

        ConstitutionCommands::Propose { goal_id, agent } => {
            // Resolve the agent ID from the goal if not specified.
            let goal_uuid = resolve_goal_id(goal_id, goal_store)?;
            let goal = goal_store
                .get(goal_uuid)?
                .ok_or_else(|| anyhow::anyhow!("Goal not found: {}", goal_id))?;
            let agent_id = agent.as_deref().unwrap_or(&goal.agent_id);

            // Load baseline patterns for this agent.
            let baselines_dir = config.workspace_root.join(".ta").join("baselines");
            let baseline_store = ta_audit::BaselineStore::new(baselines_dir);
            let patterns = match baseline_store.load(agent_id)? {
                Some(b) => b.resource_patterns,
                None => {
                    println!(
                        "No baseline found for agent '{}'. Run `ta audit baseline {}` first.",
                        agent_id, agent_id
                    );
                    println!("Proposing constitution from goal objective only...");
                    Vec::new()
                }
            };

            let constitution =
                ta_policy::constitution::propose_constitution(goal_id, &goal.objective, &patterns);

            store.save(&constitution)?;
            println!("Proposed access constitution for goal {}", goal_id);
            println!("  Agent: {}", agent_id);
            println!("  Entries: {}", constitution.access.len());
            println!("  Enforcement: {}", constitution.enforcement);
            println!();
            for entry in &constitution.access {
                println!("  {} — {}", entry.pattern, entry.intent);
            }
            println!();
            println!("Review with: ta goal constitution view {}", goal_id);
        }

        ConstitutionCommands::List => {
            let goals = store.list_goals()?;
            if goals.is_empty() {
                println!("No access constitutions found.");
                return Ok(());
            }

            println!("{:<40} ENTRIES  ENFORCEMENT", "GOAL ID");
            println!("{}", "-".repeat(70));

            for goal_id in &goals {
                if let Ok(Some(c)) = store.load(goal_id) {
                    println!("{:<40} {:<8} {}", goal_id, c.access.len(), c.enforcement,);
                }
            }
            println!("\n{} constitution(s) total.", goals.len());
        }
        ConstitutionCommands::Verify { goal_id } => {
            verify_constitution(config, goal_store, goal_id.as_deref())?;
        }
    }

    Ok(())
}

/// Verify constitution invariants for active goals (v0.11.3).
///
/// Checks workspace state against TA-CONSTITUTION.md rules:
/// - Rule 3.2: Infrastructure exclusion (no .ta/goals/ in staging)
/// - Rule 4.3: Injection cleanup (CLAUDE.md state matches goal state)
/// - Rule 1.5: Audit chain (audit log exists and is non-empty)
/// - Rule 2.1: Feature branch isolation (applied goals used drafts)
fn verify_constitution(
    config: &GatewayConfig,
    goal_store: &GoalRunStore,
    goal_id: Option<&str>,
) -> anyhow::Result<()> {
    // Load TA-CONSTITUTION.md.
    let constitution_path = config.workspace_root.join("docs/TA-CONSTITUTION.md");
    let alt_path = config.workspace_root.join("TA-CONSTITUTION.md");
    let const_path = if constitution_path.exists() {
        constitution_path
    } else if alt_path.exists() {
        alt_path
    } else {
        anyhow::bail!(
            "No TA-CONSTITUTION.md found.\n\
             Searched: docs/TA-CONSTITUTION.md, TA-CONSTITUTION.md\n\
             Create the constitution document to enable verification."
        );
    };

    println!(
        "Verifying constitutional compliance ({})...",
        const_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("TA-CONSTITUTION.md")
    );
    println!();

    let goals = if let Some(id_prefix) = goal_id {
        let all = goal_store.list()?;
        let matched: Vec<_> = all
            .into_iter()
            .filter(|g| g.goal_run_id.to_string().starts_with(id_prefix))
            .collect();
        if matched.is_empty() {
            anyhow::bail!(
                "No goal found matching '{}'. Run `ta goal list` to see goals.",
                id_prefix
            );
        }
        matched
    } else {
        let all = goal_store.list()?;
        all.into_iter()
            .filter(|g| g.state == GoalRunState::Running || g.state == GoalRunState::PrReady)
            .collect()
    };

    let mut pass_count = 0u32;
    let mut fail_count = 0u32;

    // Rule 3.2: Infrastructure exclusion — staging must not contain leaked TA state.
    println!("  Rule 3.2 (Infrastructure Exclusion):");
    for goal in &goals {
        let staging_goals = goal.workspace_path.join(".ta").join("goals");
        if staging_goals.exists() {
            println!(
                "    [FAIL] Goal {} — .ta/goals/ found in staging at {}",
                &goal.goal_run_id.to_string()[..8],
                staging_goals.display()
            );
            fail_count += 1;
        } else {
            println!(
                "    [OK]   Goal {} — No leaked TA state in staging",
                &goal.goal_run_id.to_string()[..8]
            );
            pass_count += 1;
        }
    }
    if goals.is_empty() {
        println!("    (no active goals to check)");
    }

    // Rule 4.3: Injection cleanup — CLAUDE.md injection must match goal state.
    println!("  Rule 4.3 (Injection Cleanup):");
    for goal in &goals {
        let claude_md = goal.workspace_path.join("CLAUDE.md");
        if claude_md.exists() {
            if let Ok(content) = std::fs::read_to_string(&claude_md) {
                let has_injection =
                    content.contains("## Plan Context") && content.contains("TA-mediated goal");
                if has_injection && goal.state == GoalRunState::Running {
                    println!(
                        "    [OK]   Goal {} — Injection active (goal is running)",
                        &goal.goal_run_id.to_string()[..8]
                    );
                    pass_count += 1;
                } else if has_injection {
                    println!(
                        "    [WARN] Goal {} — Injection content still present (state: {})",
                        &goal.goal_run_id.to_string()[..8],
                        goal.state
                    );
                    fail_count += 1;
                } else {
                    pass_count += 1;
                }
            }
        } else {
            pass_count += 1;
        }
    }

    // Rule 1.5: Audit chain — audit log must exist and be non-empty.
    println!("  Rule 1.5 (Append-Only Audit):");
    let audit_log = config.workspace_root.join(".ta/audit.jsonl");
    if audit_log.exists() {
        let content = std::fs::read_to_string(&audit_log).unwrap_or_default();
        let entries = content.lines().filter(|l| !l.trim().is_empty()).count();
        if entries > 0 {
            println!("    [OK]   Audit log exists with {} entries", entries);
            pass_count += 1;
        } else {
            println!("    [WARN] Audit log exists but is empty");
            fail_count += 1;
        }
    } else {
        println!("    [INFO] No audit log found at .ta/audit.jsonl");
        pass_count += 1;
    }

    // Rule 2.1: Feature branch isolation — applied goals must use drafts.
    println!("  Rule 2.1 (Feature Branch Isolation):");
    let applied_goals: Vec<_> = if goal_id.is_some() {
        goals
            .iter()
            .filter(|g| g.state == GoalRunState::Applied)
            .collect()
    } else {
        // Collect owned goals first, then reference them.
        vec![]
    };
    let all_goals_for_rule2;
    let applied_goals_final: Vec<&GoalRun> = if goal_id.is_some() {
        applied_goals.to_vec()
    } else {
        all_goals_for_rule2 = goal_store.list().unwrap_or_default();
        all_goals_for_rule2
            .iter()
            .filter(|g| g.state == GoalRunState::Applied)
            .take(5)
            .collect()
    };
    if applied_goals_final.is_empty() {
        println!("    (no applied goals to check)");
    }
    for goal in &applied_goals_final {
        if goal.pr_package_id.is_some() {
            println!(
                "    [OK]   Goal {} — Has draft package (branch isolation)",
                &goal.goal_run_id.to_string()[..8]
            );
            pass_count += 1;
        } else {
            println!(
                "    [WARN] Goal {} — Applied without draft package",
                &goal.goal_run_id.to_string()[..8]
            );
            fail_count += 1;
        }
    }

    // v0.14.7.2: TRACE-1 — Every staging dir in .ta/staging/ has a goal record.
    println!("  TRACE-1 (§5.6 — Orphaned Staging Dirs):");
    let staging_root = config.workspace_root.join(".ta").join("staging");
    if staging_root.exists() {
        let all_goals = goal_store.list().unwrap_or_default();
        let known_staging: std::collections::HashSet<std::path::PathBuf> =
            all_goals.iter().map(|g| g.workspace_path.clone()).collect();
        match std::fs::read_dir(&staging_root) {
            Ok(entries) => {
                let mut orphan_count = 0u32;
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && !known_staging.contains(&path) {
                        println!(
                            "    [FAIL] Orphaned staging dir (no goal record): {}",
                            path.display()
                        );
                        fail_count += 1;
                        orphan_count += 1;
                    }
                }
                if orphan_count == 0 {
                    println!("    [OK]   No orphaned staging directories found");
                    pass_count += 1;
                }
            }
            Err(e) => {
                println!("    [INFO] Cannot read .ta/staging/: {}", e);
                pass_count += 1;
            }
        }
    } else {
        println!("    [OK]   No .ta/staging/ directory (nothing to check)");
        pass_count += 1;
    }

    // v0.14.7.2: TRACE-2 — Applied/Completed goals must not have staging present.
    println!("  TRACE-2 (§5.7 — Applied Goals with Staging Present):");
    {
        let all_goals = goal_store.list().unwrap_or_default();
        let mut cleanup_failures = 0u32;
        for g in &all_goals {
            let is_terminal_clean =
                matches!(g.state, GoalRunState::Applied | GoalRunState::Completed);
            if is_terminal_clean
                && !g.workspace_path.as_os_str().is_empty()
                && g.workspace_path.exists()
            {
                println!(
                    "    [FAIL] Goal {} ({}) — staging dir still present after {} (cleanup failure): {}",
                    &g.goal_run_id.to_string()[..8],
                    g.display_tag(),
                    g.state,
                    g.workspace_path.display()
                );
                fail_count += 1;
                cleanup_failures += 1;
            }
        }
        if cleanup_failures == 0 {
            println!("    [OK]   No applied/completed goals with lingering staging dirs");
            pass_count += 1;
        }
    }

    println!();
    println!(
        "Constitution verification: {} passed, {} warnings/failures.",
        pass_count, fail_count
    );

    if fail_count > 0 {
        println!(
            "\nReview TA-CONSTITUTION.md for details on each rule.\n\
             Some warnings may be expected during active goal execution."
        );
    }

    Ok(())
}

/// Garbage-collect zombie goals and stale staging directories (v0.9.5.1).
fn gc_goals(
    store: &GoalRunStore,
    config: &GatewayConfig,
    dry_run: bool,
    include_staging: bool,
    threshold_days: u32,
) -> anyhow::Result<()> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(threshold_days as i64);
    let goals = store.list()?;

    let mut zombie_count = 0u32;
    let mut staging_count = 0u32;
    let mut staging_bytes = 0u64;

    for goal in &goals {
        // 1. Zombie detection: goals stuck in `running` past threshold.
        if goal.state == GoalRunState::Running && goal.updated_at < cutoff {
            if dry_run {
                println!(
                    "[dry-run] Would transition to failed: {} \"{}\" (running for {}d)",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                    (chrono::Utc::now() - goal.updated_at).num_days(),
                );
            } else {
                // Write audit ledger entry before transitioning.
                write_gc_audit_entry(config, goal, "stale: exceeded threshold");
                let mut g = goal.clone();
                let _ = g.transition(GoalRunState::Failed {
                    reason: format!("gc: stale goal exceeded {}d threshold", threshold_days),
                });
                store.save(&g)?;
                println!(
                    "Transitioned to failed: {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            }
            zombie_count += 1;
        }

        // 2. Missing staging detection: non-terminal goals whose staging dir is gone.
        let is_terminal = matches!(
            goal.state,
            GoalRunState::Applied | GoalRunState::Completed | GoalRunState::Failed { .. }
        );
        if !is_terminal
            && goal.state != GoalRunState::Created
            && !goal.workspace_path.as_os_str().is_empty()
            && !goal.workspace_path.exists()
        {
            if dry_run {
                println!(
                    "[dry-run] Would mark failed (missing staging): {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            } else {
                // Write audit ledger entry before marking failed.
                write_gc_audit_entry(config, goal, "missing staging workspace");
                let mut g = goal.clone();
                let _ = g.transition(GoalRunState::Failed {
                    reason: "gc: missing staging workspace".to_string(),
                });
                store.save(&g)?;
                println!(
                    "Marked failed (missing staging): {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            }
            zombie_count += 1;
        }

        // 3. Staging cleanup for terminal goals (only with --include-staging).
        if include_staging
            && is_terminal
            && goal.updated_at < cutoff
            && !goal.workspace_path.as_os_str().is_empty()
            && goal.workspace_path.exists()
        {
            let dir_size = dir_size_bytes(&goal.workspace_path);
            if dry_run {
                println!(
                    "[dry-run] Would remove staging: {} ({}, goal: {})",
                    goal.workspace_path.display(),
                    format_bytes(dir_size),
                    &goal.goal_run_id.to_string()[..8],
                );
            } else {
                std::fs::remove_dir_all(&goal.workspace_path)?;
                println!(
                    "Removed staging: {} ({}, goal: {})",
                    goal.workspace_path.display(),
                    format_bytes(dir_size),
                    &goal.goal_run_id.to_string()[..8],
                );
            }
            staging_count += 1;
            staging_bytes += dir_size;
        }
    }

    println!(
        "\n{}Transitioned {} zombie goal(s) to failed. Reclaimed {} staging director{} ({}).",
        if dry_run { "[dry-run] " } else { "" },
        zombie_count,
        staging_count,
        if staging_count == 1 { "y" } else { "ies" },
        format_bytes(staging_bytes),
    );

    Ok(())
}

/// Bulk purge of old terminal goals (v0.14.7.2).
///
/// Removes goal records, staging directories, and associated draft packages.
/// Always writes an audit record per purged goal. Refuses to purge active goals.
fn purge_goals(
    config: &GatewayConfig,
    store: &GoalRunStore,
    id: Option<&str>,
    state_filter: Option<&str>,
    older_than: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    // States that may NOT be purged (still active).
    const PROTECTED_STATES: &[&str] = &[
        "running",
        "pr_ready",
        "under_review",
        "awaiting_input",
        "finalizing",
    ];

    // Parse --older-than (e.g., "30d", "7d").
    let cutoff: Option<chrono::DateTime<chrono::Utc>> = if let Some(age_str) = older_than {
        let days: u64 = if let Some(d) = age_str.strip_suffix('d') {
            d.parse::<u64>().map_err(|_| {
                anyhow::anyhow!(
                    "Invalid --older-than value '{}'. Use format like '30d'.",
                    age_str
                )
            })?
        } else {
            anyhow::bail!(
                "Invalid --older-than value '{}'. Use format like '30d' (days only).",
                age_str
            );
        };
        Some(chrono::Utc::now() - chrono::Duration::days(days as i64))
    } else {
        None
    };

    // Parse --state list.
    let allowed_states: Option<Vec<&str>> =
        state_filter.map(|s| s.split(',').map(str::trim).collect());

    // Validate no protected states in the filter.
    if let Some(ref states) = allowed_states {
        for s in states {
            if PROTECTED_STATES.contains(s) {
                anyhow::bail!(
                    "Cannot purge goals in active state '{}'. Only terminal states are purgeable \
                     (applied, completed, failed, denied, closed).",
                    s
                );
            }
        }
    }

    let goals = store.list()?;

    // Collect goals to purge.
    let to_purge: Vec<&GoalRun> = if let Some(id_prefix) = id {
        // Specific goal by ID.
        let goal_id = resolve_goal_id(id_prefix, store)?;
        goals
            .iter()
            .filter(|g| {
                g.goal_run_id == goal_id
                    && !PROTECTED_STATES.contains(&g.state.to_string().as_str())
            })
            .collect()
    } else {
        goals
            .iter()
            .filter(|g| {
                // Exclude protected states.
                if PROTECTED_STATES.contains(&g.state.to_string().as_str()) {
                    return false;
                }
                // Apply state filter.
                if let Some(ref states) = allowed_states {
                    if !states.contains(&g.state.to_string().as_str()) {
                        return false;
                    }
                }
                // Apply age filter.
                if let Some(cutoff_time) = cutoff {
                    if g.updated_at >= cutoff_time {
                        return false;
                    }
                }
                true
            })
            .collect()
    };

    if to_purge.is_empty() {
        println!("No goals matched the purge criteria.");
        return Ok(());
    }

    let mut purged_count = 0u32;
    let mut staging_bytes = 0u64;

    for goal in &to_purge {
        let short_id = &goal.goal_run_id.to_string()[..8];
        let age_days = (chrono::Utc::now() - goal.updated_at).num_days();

        if dry_run {
            println!(
                "[dry-run] Would purge: {} \"{}\" (state: {}, age: {}d)",
                short_id,
                truncate(&goal.title, 40),
                goal.state,
                age_days
            );
            if !goal.workspace_path.as_os_str().is_empty() && goal.workspace_path.exists() {
                let sz = dir_size_bytes(&goal.workspace_path);
                println!(
                    "           staging: {} ({})",
                    goal.workspace_path.display(),
                    format_bytes(sz)
                );
            }
            if let Some(pkg_id) = goal.pr_package_id {
                let pkg_path = config
                    .workspace_root
                    .join(".ta/pr_packages")
                    .join(format!("{}.json", pkg_id));
                if pkg_path.exists() {
                    println!("           draft: {}", short_id);
                }
            }
            continue;
        }

        // Write audit record.
        write_purge_audit_entry(config, goal);

        // Remove staging directory.
        if !goal.workspace_path.as_os_str().is_empty() && goal.workspace_path.exists() {
            let sz = dir_size_bytes(&goal.workspace_path);
            staging_bytes += sz;
            if let Err(e) = std::fs::remove_dir_all(&goal.workspace_path) {
                eprintln!("  warn: failed to remove staging for {}: {}", short_id, e);
            }
        }

        // Remove associated draft package.
        if let Some(pkg_id) = goal.pr_package_id {
            let pkg_path = config
                .workspace_root
                .join(".ta/pr_packages")
                .join(format!("{}.json", pkg_id));
            if pkg_path.exists() {
                if let Err(e) = std::fs::remove_file(&pkg_path) {
                    eprintln!(
                        "  warn: failed to remove draft {} for goal {}: {}",
                        &pkg_id.to_string()[..8],
                        short_id,
                        e
                    );
                }
            }
        }

        // Delete goal record.
        if let Err(e) = store.delete(goal.goal_run_id) {
            eprintln!("  warn: failed to delete goal record {}: {}", short_id, e);
        } else {
            println!(
                "Purged: {} \"{}\" (state: {}, age: {}d)",
                short_id,
                truncate(&goal.title, 40),
                goal.state,
                age_days
            );
            purged_count += 1;
        }
    }

    if dry_run {
        println!("\n[dry-run] {} goal(s) would be purged.", to_purge.len());
    } else {
        println!(
            "\nPurged {} goal(s). Reclaimed {} of staging space.",
            purged_count,
            format_bytes(staging_bytes)
        );
    }

    Ok(())
}

/// Write an audit entry for a purged goal.
fn write_purge_audit_entry(config: &GatewayConfig, goal: &GoalRun) {
    let ledger_path = ta_audit::GoalAuditLedger::path_for(&config.workspace_root);
    match ta_audit::GoalAuditLedger::open(&ledger_path) {
        Ok(mut ledger) => {
            let now = chrono::Utc::now();
            let total = now.signed_duration_since(goal.created_at).num_seconds();
            let mut entry = ta_audit::AuditEntry {
                goal_id: goal.goal_run_id,
                title: goal.title.clone(),
                objective: None,
                disposition: ta_audit::AuditDisposition::Gc,
                phase: goal.plan_phase.clone(),
                agent: goal.agent_id.clone(),
                created_at: goal.created_at,
                pr_ready_at: None,
                recorded_at: now,
                build_seconds: total,
                review_seconds: 0,
                total_seconds: total,
                draft_id: goal.pr_package_id,
                ai_summary: None,
                reviewer: None,
                denial_reason: None,
                cancel_reason: Some("purge: deliberate user cleanup via ta goal purge".to_string()),
                artifact_count: 0,
                lines_changed: 0,
                artifacts: Vec::new(),
                policy_result: None,
                parent_goal_id: goal.parent_goal_id,
                previous_hash: None,
            };
            if let Err(e) = ledger.append(&mut entry) {
                tracing::warn!(
                    "Failed to write purge audit entry for goal {}: {}",
                    goal.goal_run_id,
                    e
                );
            }
        }
        Err(e) => {
            tracing::warn!("Cannot open ledger for purge audit: {}", e);
        }
    }
}

/// Write a gc audit entry for a goal being transitioned by the garbage collector.
fn write_gc_audit_entry(config: &GatewayConfig, goal: &GoalRun, gc_reason: &str) {
    let ledger_path = ta_audit::GoalAuditLedger::path_for(&config.workspace_root);
    match ta_audit::GoalAuditLedger::open(&ledger_path) {
        Ok(mut ledger) => {
            let now = chrono::Utc::now();
            let total = now.signed_duration_since(goal.created_at).num_seconds();
            let mut entry = ta_audit::AuditEntry {
                goal_id: goal.goal_run_id,
                title: goal.title.clone(),
                objective: None,
                disposition: ta_audit::AuditDisposition::Gc,
                phase: goal.plan_phase.clone(),
                agent: goal.agent_id.clone(),
                created_at: goal.created_at,
                pr_ready_at: None,
                recorded_at: now,
                build_seconds: total,
                review_seconds: 0,
                total_seconds: total,
                draft_id: goal.pr_package_id,
                ai_summary: None,
                reviewer: None,
                denial_reason: None,
                cancel_reason: Some(format!("gc: {}", gc_reason)),
                artifact_count: 0,
                lines_changed: 0,
                artifacts: Vec::new(),
                policy_result: None,
                parent_goal_id: goal.parent_goal_id,
                previous_hash: None,
            };
            if let Err(e) = ledger.append(&mut entry) {
                tracing::warn!(
                    "Failed to write gc audit entry for goal {}: {}",
                    goal.goal_run_id,
                    e
                );
            }
        }
        Err(e) => {
            tracing::warn!("Failed to open goal audit ledger for gc entry: {}", e);
        }
    }
}

/// Approximate directory size in bytes (non-recursive for speed — counts immediate files).
fn dir_size_bytes(path: &std::path::Path) -> u64 {
    walkdir(path)
}

fn walkdir(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += walkdir(&entry.path());
                }
            }
        }
    }
    total
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max - 3])
    } else {
        s.to_string()
    }
}

/// Check if a process with the given PID is alive (v0.11.2.4).
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string()) && !stdout.contains("No tasks")
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Compute process health label for a goal's agent (v0.11.2.4).
///
/// Returns a short label for the HEALTH column in `ta goal list`:
///   "alive"   — PID is running
///   "dead"    — PID has exited
///   "unknown" — no PID stored (legacy goal or spawn failure)
///   "—"       — terminal state (no process to check)
fn process_health_label(goal: &ta_goal::GoalRun) -> &'static str {
    match &goal.state {
        GoalRunState::Running
        | GoalRunState::Finalizing { .. }
        | GoalRunState::DraftPending { .. }
        | GoalRunState::AwaitingInput { .. } => match goal.agent_pid {
            Some(pid) => {
                if is_process_alive(pid) {
                    "alive"
                } else {
                    "dead"
                }
            }
            None => "unknown",
        },
        _ => "—",
    }
}

// ── Self-service operations (v0.11.3) ──

enum DiskCheck {
    Ok(u64),
    Low(u64),
    Unknown,
}

fn check_disk_space_df(path: &std::path::Path) -> DiskCheck {
    let output = std::process::Command::new("df")
        .args(["-k", &path.display().to_string()])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = stdout.lines().nth(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 4 {
                    if let Ok(kb) = fields[3].parse::<u64>() {
                        let bytes = kb * 1024;
                        let threshold = 2 * 1024 * 1024 * 1024; // 2 GB
                        return if bytes >= threshold {
                            DiskCheck::Ok(bytes)
                        } else {
                            DiskCheck::Low(bytes)
                        };
                    }
                }
            }
            DiskCheck::Unknown
        }
        _ => DiskCheck::Unknown,
    }
}

fn load_draft_summary(pr_packages_dir: &std::path::Path, pr_id: Uuid) -> Option<(String, usize)> {
    let path = pr_packages_dir.join(format!("{}.json", pr_id));
    let content = std::fs::read_to_string(&path).ok()?;
    let pkg: serde_json::Value = serde_json::from_str(&content).ok()?;
    let status = pkg["status"].as_str().unwrap_or("unknown").to_string();
    let artifacts = pkg["changes"]["artifacts"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    Some((status, artifacts))
}

fn read_agent_log_tail(
    config: &GatewayConfig,
    goal: &ta_goal::GoalRun,
    lines: usize,
) -> Vec<String> {
    let output_dir = config
        .workspace_root
        .join(".ta/goal-output")
        .join(goal.goal_run_id.to_string());
    let stdout_path = output_dir.join("stdout.log");
    if let Ok(content) = std::fs::read_to_string(&stdout_path) {
        let all: Vec<&str> = content.lines().collect();
        let start = all.len().saturating_sub(lines);
        all[start..].iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    }
}

fn read_recent_events(
    config: &GatewayConfig,
    goal: &ta_goal::GoalRun,
    count: usize,
) -> Vec<String> {
    let events_file = config.workspace_root.join(".ta/events/events.jsonl");
    if let Ok(content) = std::fs::read_to_string(&events_file) {
        let goal_id_str = goal.goal_run_id.to_string();
        let matching: Vec<String> = content
            .lines()
            .filter(|line| line.contains(&goal_id_str))
            .map(|line| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    let event_type = v["event_type"].as_str().unwrap_or("?");
                    let ts = v["timestamp"].as_str().unwrap_or("?");
                    let time = if ts.len() > 11 { &ts[11..19] } else { ts };
                    format!("[{}] {}", time, event_type)
                } else {
                    line.to_string()
                }
            })
            .collect();
        let start = matching.len().saturating_sub(count);
        matching[start..].to_vec()
    } else {
        Vec::new()
    }
}

/// Detailed goal inspection: PID, process health, elapsed time, staging, draft, agent log tail.
fn goal_inspect(
    config: &GatewayConfig,
    store: &GoalRunStore,
    id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let goal_run_id = resolve_goal_id(id, store)?;
    let goal = store
        .get(goal_run_id)?
        .ok_or_else(|| anyhow::anyhow!("Goal not found: {}", id))?;

    let elapsed = chrono::Utc::now().signed_duration_since(goal.created_at);
    let process_alive = goal.agent_pid.map(is_process_alive).unwrap_or(false);
    let staging_exists = goal.workspace_path.exists();
    let staging_size = if staging_exists {
        dir_size_bytes(&goal.workspace_path)
    } else {
        0
    };
    let draft_info = goal
        .pr_package_id
        .and_then(|pr_id| load_draft_summary(&config.pr_packages_dir, pr_id));
    let agent_log_tail = read_agent_log_tail(config, &goal, 20);
    let recent_events = read_recent_events(config, &goal, 10);

    if json {
        let obj = serde_json::json!({
            "goal_id": goal.goal_run_id.to_string(),
            "tag": goal.tag,
            "title": goal.title,
            "objective": goal.objective,
            "state": goal.state.to_string(),
            "agent": goal.agent_id,
            "plan_phase": goal.plan_phase,
            "created_at": goal.created_at.to_rfc3339(),
            "updated_at": goal.updated_at.to_rfc3339(),
            "elapsed_minutes": elapsed.num_minutes(),
            "agent_pid": goal.agent_pid,
            "process_alive": process_alive,
            "staging_path": goal.workspace_path.display().to_string(),
            "staging_exists": staging_exists,
            "staging_size_bytes": staging_size,
            "draft_id": goal.pr_package_id.map(|id| id.to_string()),
            "draft_status": draft_info.as_ref().map(|(s, _)| s.clone()),
            "parent_goal_id": goal.parent_goal_id.map(|id| id.to_string()),
            "is_macro": goal.is_macro,
            "recent_events": recent_events,
            "agent_log_tail": agent_log_tail,
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    let tag_display = goal.tag.as_deref().unwrap_or("-");
    println!("Goal: {} ({})", goal.title, tag_display);
    println!("  ID:          {}", goal.goal_run_id);
    println!("  State:       {}", goal.state);
    println!("  Agent:       {}", goal.agent_id);
    if let Some(phase) = &goal.plan_phase {
        println!("  Plan phase:  {}", phase);
    }
    println!(
        "  Created:     {}",
        goal.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!(
        "  Updated:     {}",
        goal.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!(
        "  Elapsed:     {}h {}m",
        elapsed.num_hours(),
        elapsed.num_minutes() % 60
    );
    println!();

    println!("Process:");
    match goal.agent_pid {
        Some(pid) => {
            let status = if process_alive { "alive" } else { "dead" };
            println!("  PID:    {} ({})", pid, status);
        }
        None => println!("  PID:    not recorded"),
    }
    println!();

    println!("Staging:");
    println!("  Path:   {}", goal.workspace_path.display());
    println!("  Exists: {}", staging_exists);
    if staging_exists {
        println!("  Size:   {}", format_bytes(staging_size));
    }
    if let Some(source) = &goal.source_dir {
        println!("  Source: {}", source.display());
    }
    println!();

    println!("Draft:");
    match &goal.pr_package_id {
        Some(pr_id) => {
            println!("  ID:     {}", pr_id);
            if let Some((status, artifact_count)) = &draft_info {
                println!("  Status: {}", status);
                println!("  Artifacts: {}", artifact_count);
            }
        }
        None => println!("  (no draft built)"),
    }

    if let Some(parent) = &goal.parent_goal_id {
        println!();
        println!("Follow-up of: {}", &parent.to_string()[..8]);
    }
    if goal.is_macro {
        println!();
        println!("Macro goal: {} sub-goals", goal.sub_goal_ids.len());
    }

    if !recent_events.is_empty() {
        println!();
        println!("Recent events:");
        for event in &recent_events {
            println!("  {}", event);
        }
    }

    // v0.14.7.2: Show agent progress journal checkpoints.
    if staging_exists {
        if let Some(journal) = load_progress_journal(&goal.workspace_path) {
            println!();
            println!(
                "Agent Progress ({} checkpoint(s)):",
                journal.checkpoints.len()
            );
            if journal.checkpoints.is_empty() {
                println!("  (no checkpoints recorded)");
            }
            for cp in &journal.checkpoints {
                println!("  [{}] {} — {}", cp.at, cp.label, cp.detail);
            }
        }
    }

    if !agent_log_tail.is_empty() {
        println!();
        println!("Agent output (last {} lines):", agent_log_tail.len());
        for line in &agent_log_tail {
            println!("  {}", line);
        }
    }

    Ok(())
}

/// Analyze a failed/stuck goal: timeline, last output, state transitions, errors, likely cause.
fn goal_post_mortem(config: &GatewayConfig, store: &GoalRunStore, id: &str) -> anyhow::Result<()> {
    let goal_run_id = resolve_goal_id(id, store)?;
    let goal = store
        .get(goal_run_id)?
        .ok_or_else(|| anyhow::anyhow!("Goal not found: {}", id))?;

    let elapsed = chrono::Utc::now().signed_duration_since(goal.created_at);
    let run_duration = goal.updated_at.signed_duration_since(goal.created_at);

    println!(
        "Post-mortem: {} \"{}\"",
        &goal.goal_run_id.to_string()[..8],
        goal.title
    );
    println!("{}", "=".repeat(65));
    println!();

    println!("Final state: {}", goal.state);
    if let GoalRunState::Failed { reason } = &goal.state {
        println!("Failure reason: {}", reason);
    }
    println!(
        "Duration: {}h {}m (created -> last update)",
        run_duration.num_hours(),
        run_duration.num_minutes() % 60
    );
    println!(
        "Time since creation: {}h {}m",
        elapsed.num_hours(),
        elapsed.num_minutes() % 60
    );
    println!();

    println!("Process:");
    match goal.agent_pid {
        Some(pid) => {
            let alive = is_process_alive(pid);
            if alive {
                println!("  Agent process {} is still running (possible zombie)", pid);
            } else {
                println!("  Agent process {} has exited", pid);
            }
        }
        None => println!("  No agent PID recorded"),
    }
    println!();

    println!("Staging:");
    if goal.workspace_path.exists() {
        let size = dir_size_bytes(&goal.workspace_path);
        println!(
            "  Directory exists: {} ({})",
            goal.workspace_path.display(),
            format_bytes(size)
        );
        let summary_path = goal.workspace_path.join(".ta/change_summary.json");
        if summary_path.exists() {
            println!("  change_summary.json: present (agent completed its work)");
        } else {
            println!("  change_summary.json: missing (agent may not have finished)");
        }
    } else {
        println!(
            "  Directory missing: {} (cleaned or never created)",
            goal.workspace_path.display()
        );
    }
    println!();

    println!("Draft:");
    match &goal.pr_package_id {
        Some(pr_id) => {
            if let Some((status, count)) = load_draft_summary(&config.pr_packages_dir, *pr_id) {
                println!(
                    "  ID: {} -- Status: {} -- {} artifacts",
                    &pr_id.to_string()[..8],
                    status,
                    count
                );
            } else {
                println!(
                    "  ID: {} -- package file not found (may have been cleaned)",
                    &pr_id.to_string()[..8]
                );
            }
        }
        None => println!("  No draft was built"),
    }
    println!();

    println!("Diagnosis:");
    let mut causes: Vec<String> = Vec::new();

    if let GoalRunState::Failed { reason } = &goal.state {
        if reason.contains("gc:") {
            causes.push(
                "Goal was garbage-collected (exceeded stale threshold or staging was missing)."
                    .to_string(),
            );
        }
        if reason.contains("timeout") || reason.contains("timed out") {
            causes.push(
                "Agent operation timed out. Check [verify] timeout configuration.".to_string(),
            );
        }
        if reason.contains("missing staging") {
            causes.push(
                "Staging directory was removed externally while goal was active.".to_string(),
            );
        }
        if reason.contains("process") || reason.contains("crash") {
            causes.push("Agent process exited unexpectedly.".to_string());
        }
    }

    if goal.state == GoalRunState::Running {
        let idle_minutes = chrono::Utc::now()
            .signed_duration_since(goal.updated_at)
            .num_minutes();
        if idle_minutes > 60 {
            causes.push(format!(
                "Goal has been in 'running' state with no updates for {} minutes. The agent may be stuck or crashed.",
                idle_minutes
            ));
        }
        if let Some(pid) = goal.agent_pid {
            if !is_process_alive(pid) {
                causes.push(format!(
                    "Agent process (pid {}) is no longer alive but goal is still 'running'. Transition to failed with: ta goal gc",
                    pid
                ));
            }
        }
    }

    if causes.is_empty() {
        if goal.state == GoalRunState::PrReady {
            println!("  Goal completed normally -- draft is ready for review.");
            println!("  Next: ta draft view");
        } else if goal.state == GoalRunState::Completed || goal.state == GoalRunState::Applied {
            println!("  Goal completed successfully -- no issues detected.");
        } else {
            println!("  No specific diagnosis available. Check agent logs for details.");
        }
    } else {
        for (i, cause) in causes.iter().enumerate() {
            println!("  {}. {}", i + 1, cause);
        }
    }

    println!();
    println!("Suggested actions:");
    let short_id = &goal.goal_run_id.to_string()[..8];
    match &goal.state {
        GoalRunState::Failed { .. } => {
            println!("  - Review agent output: ta conversation {}", short_id);
            println!("  - Retry the goal: ta run --follow-up-goal {}", short_id);
            println!("  - Clean up: ta gc");
        }
        GoalRunState::Running => {
            if goal
                .agent_pid
                .map(|pid| !is_process_alive(pid))
                .unwrap_or(false)
            {
                println!("  - Transition to failed: ta goal gc");
                println!("  - Retry: ta run --follow-up-goal {}", short_id);
            } else {
                println!("  - Check agent output: ta conversation {}", short_id);
                println!(
                    "  - Wait for agent to finish, or check process: ps -p {}",
                    goal.agent_pid.unwrap_or(0)
                );
            }
        }
        GoalRunState::PrReady => {
            println!("  - Review the draft: ta draft view");
            println!("  - Approve: ta draft approve");
        }
        _ => {
            println!("  - Check goal status: ta goal status {}", short_id);
        }
    }

    Ok(())
}

/// Check prerequisites before starting a goal.
fn goal_pre_flight(config: &GatewayConfig, title: Option<&str>) -> anyhow::Result<()> {
    println!(
        "Pre-flight check{}",
        title.map(|t| format!(" for \"{}\"", t)).unwrap_or_default()
    );
    println!("{}", "=".repeat(42));

    let mut issues: Vec<(String, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut passed = 0u32;

    print!("  Disk space... ");
    match check_disk_space_df(&config.workspace_root) {
        DiskCheck::Ok(avail) => {
            println!("ok ({} available)", format_bytes(avail));
            passed += 1;
        }
        DiskCheck::Low(avail) => {
            println!(
                "WARNING ({} available, recommend >2 GB)",
                format_bytes(avail)
            );
            warnings.push(format!(
                "Low disk space: {} available. Free space or run .",
                format_bytes(avail)
            ));
        }
        DiskCheck::Unknown => {
            println!("unknown (could not determine)");
            warnings.push("Could not determine available disk space.".to_string());
        }
    }

    print!("  Daemon... ");
    let daemon_url = super::daemon::resolve_daemon_url(&config.workspace_root, None);
    let daemon_ok = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("{}/api/status", daemon_url)).send().ok())
        .is_some();
    if daemon_ok {
        println!("ok (responding at {})", daemon_url);
        passed += 1;
    } else {
        println!("not running");
        warnings.push(format!(
            "Daemon not running at {}. It will be auto-started by .",
            daemon_url
        ));
    }

    print!("  Agent binary... ");
    match super::version_guard::find_daemon_binary() {
        Ok(path) => {
            println!("ok ({})", path.display());
            passed += 1;
        }
        Err(e) => {
            println!("MISSING");
            issues.push((
                format!("ta-daemon binary not found: {}", e),
                "Install ta-daemon or check PATH.".to_string(),
            ));
        }
    }

    print!("  .ta directory... ");
    if config.workspace_root.join(".ta").exists() {
        println!("ok");
        passed += 1;
    } else {
        println!("MISSING");
        issues.push((
            ".ta directory not found.".to_string(),
            "Initialize with: ta init setup".to_string(),
        ));
    }

    print!("  VCS (git)... ");
    if config.workspace_root.join(".git").exists() {
        println!("ok");
        passed += 1;
    } else {
        println!("not a git repo");
        warnings.push("Not a git repository.  will not work.".to_string());
    }

    print!("  PLAN.md... ");
    if config.workspace_root.join("PLAN.md").exists() {
        println!("ok");
        passed += 1;
    } else {
        println!("not found");
        warnings.push("No PLAN.md found. Create one with  for phase tracking.".to_string());
    }

    print!("  workflow.toml... ");
    if config.workspace_root.join(".ta/workflow.toml").exists() {
        println!("ok");
        passed += 1;
    } else {
        println!("using defaults");
    }

    print!("  Active goals... ");
    let active_count = GoalRunStore::new(&config.goals_dir)
        .ok()
        .map(|s| {
            s.list()
                .unwrap_or_default()
                .iter()
                .filter(|g| matches!(g.state, GoalRunState::Running | GoalRunState::Configured))
                .count()
        })
        .unwrap_or(0);
    if active_count == 0 {
        println!("none (clean slate)");
        passed += 1;
    } else {
        println!("{} active", active_count);
        warnings.push(format!(
            "{} goal(s) already active. Multiple concurrent goals use more disk space.",
            active_count
        ));
    }

    println!();
    if issues.is_empty() && warnings.is_empty() {
        println!(
            "All checks passed ({}/{}). Ready to start a goal.",
            passed, passed
        );
    } else {
        println!(
            "{} passed, {} warnings, {} issues",
            passed,
            warnings.len(),
            issues.len()
        );
        if !warnings.is_empty() {
            println!();
            println!("Warnings:");
            for w in &warnings {
                println!("  ! {}", w);
            }
        }
        if !issues.is_empty() {
            println!();
            println!("Issues (must fix before starting):");
            for (issue, fix) in &issues {
                println!("  x {}", issue);
                println!("    Fix: {}", fix);
            }
        }
    }

    if !issues.is_empty() {
        Err(anyhow::anyhow!(
            "{} pre-flight check(s) failed",
            issues.len()
        ))
    } else {
        Ok(())
    }
}

// `ta doctor` was redesigned and moved to commands/doctor.rs (v0.15.17).
// This old implementation body is no longer called; it is kept only to
// preserve compile-ability until tests below are updated to the new command.
// TODO(v0.15.18): delete this function entirely.
fn _old_doctor_impl(config: &GatewayConfig) -> anyhow::Result<()> {
    println!("TA Doctor -- System Health Check");
    println!("{}", "=".repeat(43));

    let mut pass = 0u32;
    let mut warn = 0u32;
    let mut fail = 0u32;

    print!("  TA version... ");
    println!("{}", env!("CARGO_PKG_VERSION"));
    pass += 1;

    print!("  .ta directory... ");
    let ta_dir = config.workspace_root.join(".ta");
    if ta_dir.exists() {
        let subdirs = ["goals", "pr_packages", "events"];
        let all_ok = subdirs.iter().all(|sd| ta_dir.join(sd).exists());
        if all_ok {
            println!("ok (goals, pr_packages, events present)");
            pass += 1;
        } else {
            println!("partial (some directories missing -- will be created on first use)");
            warn += 1;
        }
    } else {
        println!("MISSING -- run  to initialize");
        fail += 1;
    }

    print!("  Daemon binary... ");
    match super::version_guard::find_daemon_binary() {
        Ok(path) => {
            println!("ok ({})", path.display());
            pass += 1;
        }
        Err(e) => {
            println!("MISSING ({})", e);
            fail += 1;
        }
    }

    print!("  Daemon... ");
    let daemon_url = super::daemon::resolve_daemon_url(&config.workspace_root, None);
    let daemon_healthy = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("{}/api/status", daemon_url)).send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    if daemon_healthy {
        println!("ok (healthy at {})", daemon_url);
        pass += 1;
    } else {
        println!("not running (start with: ta daemon start)");
        warn += 1;
    }

    print!("  Git... ");
    if config.workspace_root.join(".git").exists() {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&config.workspace_root)
            .output();
        match output {
            Ok(o) if o.status.success() => {
                let changes = String::from_utf8_lossy(&o.stdout);
                let change_count = changes.lines().count();
                if change_count == 0 {
                    println!("ok (clean working tree)");
                } else {
                    println!("ok ({} uncommitted changes)", change_count);
                }
                pass += 1;
            }
            _ => {
                println!("error (git command failed)");
                warn += 1;
            }
        }
    } else {
        println!("not a git repo");
        warn += 1;
    }

    print!("  Disk space... ");
    match check_disk_space_df(&config.workspace_root) {
        DiskCheck::Ok(avail) => {
            println!("ok ({} available)", format_bytes(avail));
            pass += 1;
        }
        DiskCheck::Low(avail) => {
            println!("LOW ({} available, recommend >2 GB)", format_bytes(avail));
            warn += 1;
        }
        DiskCheck::Unknown => {
            println!("unknown");
            warn += 1;
        }
    }

    print!("  Plugins... ");
    let plugins = ta_changeset::plugin::discover_plugins(&config.workspace_root);
    if plugins.is_empty() {
        println!("none installed");
    } else {
        println!("{} installed", plugins.len());
        for p in &plugins {
            println!(
                "    {} v{} [{}]",
                p.manifest.name, p.manifest.version, p.source
            );
        }
    }
    pass += 1;

    print!("  Goals... ");
    match GoalRunStore::new(&config.goals_dir) {
        Ok(goal_store) => {
            let goals = goal_store.list().unwrap_or_default();
            let active = goals
                .iter()
                .filter(|g| matches!(g.state, GoalRunState::Running | GoalRunState::Configured))
                .count();
            let failed = goals
                .iter()
                .filter(|g| matches!(g.state, GoalRunState::Failed { .. }))
                .count();
            let pending_review = goals
                .iter()
                .filter(|g| {
                    g.state == GoalRunState::PrReady || g.state == GoalRunState::UnderReview
                })
                .count();
            println!(
                "{} total ({} active, {} failed, {} pending review)",
                goals.len(),
                active,
                failed,
                pending_review
            );
            pass += 1;
            let zombies: Vec<_> = goals
                .iter()
                .filter(|g| g.state == GoalRunState::Running)
                .filter(|g| {
                    g.agent_pid
                        .map(|pid| !is_process_alive(pid))
                        .unwrap_or(false)
                })
                .collect();
            if !zombies.is_empty() {
                println!(
                    "    WARNING: {} zombie goal(s) detected (running but agent dead)",
                    zombies.len()
                );
                println!("    Fix with: ta gc");
                warn += 1;
            }
        }
        Err(_) => {
            println!("no goal store");
        }
    }

    // ── VCS checks (v0.13.13) ────────────────────────────────────
    {
        use ta_workspace::partitioning::{git_is_ignored, VcsBackend, LOCAL_TA_PATHS};
        let vcs = VcsBackend::detect(&config.workspace_root);
        print!("  VCS... ");
        match &vcs {
            VcsBackend::Git => {
                // Verify git status works.
                let git_ok = std::process::Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(&config.workspace_root)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if git_ok {
                    println!("git (ok)");
                    pass += 1;
                } else {
                    println!("git (git status failed — check git installation)");
                    warn += 1;
                }
                // Check that local-only .ta/ paths are in .gitignore.
                let mut unignored: Vec<&str> = Vec::new();
                for path in LOCAL_TA_PATHS {
                    let full = config
                        .workspace_root
                        .join(".ta")
                        .join(path.trim_end_matches('/'));
                    if full.exists() {
                        if let Ok(false) = git_is_ignored(&config.workspace_root, path) {
                            unignored.push(path);
                        }
                    }
                }
                if !unignored.is_empty() {
                    println!(
                        "  [warn] {} local .ta/ path(s) are not in .gitignore:",
                        unignored.len()
                    );
                    for p in &unignored {
                        println!("    .ta/{}", p);
                    }
                    println!("    Fix: ta setup vcs");
                    warn += 1;
                }
            }
            VcsBackend::Perforce => {
                // Verify p4 info responds.
                let p4_ok = std::process::Command::new("p4")
                    .arg("info")
                    .current_dir(&config.workspace_root)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if p4_ok {
                    println!("perforce (ok)");
                    pass += 1;
                } else {
                    println!("perforce (p4 info failed — check P4PORT/P4CLIENT)");
                    warn += 1;
                }
                // Warn if P4IGNORE is not set.
                if std::env::var("P4IGNORE").is_err() {
                    println!("  [warn] P4IGNORE env var is not set.");
                    println!("    TA local paths may be submitted accidentally.");
                    println!("    Fix: export P4IGNORE=.p4ignore  (and re-run `ta setup vcs`)");
                    warn += 1;
                }
            }
            VcsBackend::None => {
                println!("none detected (ok — skipping VCS checks)");
                pass += 1;
            }
        }

        // ── [commit] auto_stage completeness check (v0.14.3.7) ───────
        {
            let workflow_path = config.workspace_root.join(".ta/workflow.toml");
            let workflow = ta_submit::config::WorkflowConfig::load_or_default(&workflow_path);
            let configured: std::collections::HashSet<String> =
                workflow.commit.auto_stage.iter().cloned().collect();
            let mut uncovered: Vec<&str> = Vec::new();
            for lock_file in ta_submit::GitAdapter::BUILTIN_LOCK_FILES {
                if config.workspace_root.join(lock_file).exists()
                    && !configured.contains(*lock_file)
                {
                    uncovered.push(lock_file);
                }
            }
            if !uncovered.is_empty() {
                println!("  [warn] lock file(s) present but not in [commit] auto_stage:");
                for f in &uncovered {
                    println!("    {}  — will be auto-staged by built-in list", f);
                    println!(
                        "    To make this explicit: add \"{}\" to [commit] auto_stage in .ta/workflow.toml",
                        f
                    );
                    println!("    Or run: ta setup vcs");
                }
                warn += 1;
            }
        }

        // ── Staging strategy check ────────────────────────────────
        print!("  Staging strategy... ");
        let workflow = ta_submit::config::WorkflowConfig::load_or_default(&config.workspace_root);
        let strategy = &workflow.staging.strategy;
        match strategy {
            ta_submit::config::StagingStrategy::Full => {
                // Warn if workspace is large (>1 GB).
                let workspace_bytes = dir_size_bytes(&config.workspace_root);
                let workspace_mb = workspace_bytes / (1024 * 1024);
                if workspace_mb > 1024 {
                    println!(
                        "full (workspace is {} GB — consider strategy=smart with a .taignore)",
                        workspace_mb / 1024
                    );
                    println!("    Add to .ta/workflow.toml: [staging]\\nstrategy = \"smart\"");
                    warn += 1;
                } else {
                    println!("full (ok)");
                    pass += 1;
                }
            }
            ta_submit::config::StagingStrategy::Smart => {
                println!("smart (ok)");
                pass += 1;
            }
            ta_submit::config::StagingStrategy::RefsCow => {
                println!("refs-cow (ok — Windows ReFS CoW)");
                pass += 1;
            }
            ta_submit::config::StagingStrategy::ProjFs => {
                println!("projfs (ok — Windows ProjFS virtual workspace)");
                pass += 1;
            }
        }
    }

    // ── GC health checks (v0.14.12) ───────────────────────────────────────────

    // (a) Stale staging dirs: subdirs older than 7 days with no active goal.
    {
        print!("  GC: stale staging dirs... ");
        let staging_dir = config.workspace_root.join(".ta").join("staging");
        if staging_dir.exists() {
            let seven_days_secs: u64 = 7 * 24 * 3600;
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            // Collect active goal staging paths.
            let active_staging: std::collections::HashSet<std::path::PathBuf> =
                match GoalRunStore::new(&config.goals_dir) {
                    Ok(gs) => gs
                        .list()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|g| {
                            matches!(
                                g.state,
                                GoalRunState::Running
                                    | GoalRunState::Configured
                                    | GoalRunState::PrReady
                                    | GoalRunState::UnderReview
                                    | GoalRunState::Finalizing { .. }
                            )
                        })
                        .map(|g| g.workspace_path)
                        .collect(),
                    Err(_) => std::collections::HashSet::new(),
                };

            let stale_count = std::fs::read_dir(&staging_dir)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .filter(|e| !active_staging.contains(&e.path()))
                        .filter(|e| {
                            e.metadata()
                                .and_then(|m| m.modified())
                                .and_then(|t| {
                                    t.duration_since(std::time::UNIX_EPOCH)
                                        .map_err(|_| std::io::Error::other("time error"))
                                })
                                .map(|t| now_secs.saturating_sub(t.as_secs()) > seven_days_secs)
                                .unwrap_or(false)
                        })
                        .count()
                })
                .unwrap_or(0);

            if stale_count > 0 {
                println!(
                    "{} stale dir(s) (>7 days, no active goal) — run `ta gc`",
                    stale_count
                );
                warn += 1;
            } else {
                println!("ok");
                pass += 1;
            }
        } else {
            println!("ok (no staging dir)");
            pass += 1;
        }
    }

    // (b) events.jsonl size check: warn if > 10 MB.
    {
        print!("  GC: events.jsonl size... ");
        let events_file = config
            .workspace_root
            .join(".ta")
            .join("events")
            .join("events.jsonl");
        if events_file.exists() {
            let size_bytes = events_file.metadata().map(|m| m.len()).unwrap_or(0);
            let size_mb = size_bytes as f64 / (1024.0 * 1024.0);
            if size_bytes > 10 * 1024 * 1024 {
                println!(
                    "LARGE ({:.1} MB) — consider running `ta gc` to rotate old events",
                    size_mb
                );
                warn += 1;
            } else {
                println!("ok ({:.1} MB)", size_mb);
                pass += 1;
            }
        } else {
            println!("ok (no events file)");
            pass += 1;
        }
    }

    // (c) DraftPending goals stuck > 1 hour.
    {
        print!("  GC: draft_pending timeouts... ");
        match GoalRunStore::new(&config.goals_dir) {
            Ok(gs) => {
                let one_hour_secs: i64 = 3600;
                let now = chrono::Utc::now();
                let stuck: Vec<_> = gs
                    .list()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|g| {
                        if let GoalRunState::DraftPending { pending_since, .. } = &g.state {
                            (now - *pending_since).num_seconds() > one_hour_secs
                        } else {
                            false
                        }
                    })
                    .collect();
                if stuck.is_empty() {
                    println!("ok");
                    pass += 1;
                } else {
                    println!(
                        "{} goal(s) stuck in DraftPending >1h — run `ta gc`",
                        stuck.len()
                    );
                    warn += 1;
                }
            }
            Err(_) => {
                println!("ok (no goal store)");
                pass += 1;
            }
        }
    }

    // ── Ollama health check (v0.14.9) ─────────────────────────────────────────
    // Check if Ollama is reachable when any ta-agent-ollama-backed framework is configured.
    {
        let manifests = ta_runtime::AgentFrameworkManifest::discover(&config.workspace_root);
        let builtin_ollama = ta_runtime::AgentFrameworkManifest::builtin("ollama").is_some();
        let has_ollama_agent =
            builtin_ollama || manifests.iter().any(|m| m.command == "ta-agent-ollama");
        if has_ollama_agent {
            print!("  Ollama (local agent)... ");
            let ollama_ok = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .ok()
                .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if ollama_ok {
                println!("ok (http://localhost:11434)");
                pass += 1;
            } else {
                println!("not running");
                println!(
                    "     Ollama not reachable at http://localhost:11434 — start with: ollama serve"
                );
                warn += 1;
            }
        }
    }

    // ── Stale ephemeral file check (v0.15.14.7) ──────────────────────────────
    // `.ta-decisions.json` at the project root is a staging-only artifact.
    // If it exists in the source tree, a prior `ta draft apply` incorrectly
    // applied it. Report it and offer removal so it cannot bleed into the next goal.
    {
        let stale_decisions = config.workspace_root.join(".ta-decisions.json");
        if stale_decisions.exists() {
            println!("  Stale ephemeral file check... WARN");
            println!(
                "    .ta-decisions.json found in project root ({})",
                stale_decisions.display()
            );
            println!("    This file is written by agents during a goal run and must not");
            println!("    exist in the source tree — it may bleed into subsequent goals.");
            println!("    Fix: rm .ta-decisions.json   (or: ta doctor --fix-ephemeral)");
            warn += 1;
        } else {
            print!("  Stale ephemeral file check... ");
            println!("ok");
            pass += 1;
        }
    }

    // ── Version consistency check ─────────────────────────────────────────────
    // Compares Cargo.toml workspace version vs CLAUDE.md "Current version" line.
    // A mismatch means bump-version.sh was only partially applied or run in the
    // wrong directory.
    {
        use super::draft::{read_cargo_version, read_claude_md_version};
        print!("  Version consistency (Cargo.toml vs CLAUDE.md)... ");
        let cargo_ver = read_cargo_version(&config.workspace_root);
        let claude_ver = read_claude_md_version(&config.workspace_root);
        match (cargo_ver, claude_ver) {
            (Some(ref cv), Some(ref mv)) if cv == mv => {
                println!("ok ({cv})");
                pass += 1;
            }
            (Some(ref cv), Some(ref mv)) => {
                println!("MISMATCH");
                println!("    Cargo.toml: {cv}");
                println!("    CLAUDE.md:  {mv}");
                println!("    Fix (run from project root): ./scripts/bump-version.sh {cv}");
                println!("    Note: never run bump-version.sh from inside a staging directory.");
                fail += 1;
            }
            (Some(ref cv), None) => {
                println!("ok ({cv}, no version line in CLAUDE.md)");
                pass += 1;
            }
            (None, _) => {
                println!("unknown (no top-level version in Cargo.toml)");
                warn += 1;
            }
        }
    }

    // ── Staging version mismatch check ───────────────────────────────────────
    // For each goal in a reviewable state that still has a staging directory,
    // compare the staging version vs the source version. A mismatch means the
    // draft apply will fail unless the source is bumped first.
    {
        use super::draft::read_cargo_version;
        let source_ver = read_cargo_version(&config.workspace_root);
        if let (Ok(goal_store), Some(source_ver)) =
            (GoalRunStore::new(&config.goals_dir), source_ver)
        {
            let goals = goal_store.list().unwrap_or_default();
            let reviewable: Vec<_> = goals
                .iter()
                .filter(|g| {
                    matches!(
                        g.state,
                        GoalRunState::PrReady
                            | GoalRunState::UnderReview
                            | GoalRunState::Approved { .. }
                    )
                })
                .collect();

            let mut mismatches: Vec<(String, String, String)> = Vec::new(); // (goal_id, staging_ver, goal_title)
            for goal in &reviewable {
                if goal.workspace_path.join("Cargo.toml").exists() {
                    if let Some(staging_ver) = read_cargo_version(&goal.workspace_path) {
                        if staging_ver != source_ver {
                            mismatches.push((
                                goal.goal_run_id.to_string(),
                                staging_ver,
                                goal.title.clone(),
                            ));
                        }
                    }
                }
            }

            if mismatches.is_empty() {
                print!("  Staging version mismatch... ");
                println!("ok");
                pass += 1;
            } else {
                println!(
                    "  Staging version mismatch... {} issue(s) found",
                    mismatches.len()
                );
                for (goal_id, staging_ver, title) in &mismatches {
                    let short_id = &goal_id[..8.min(goal_id.len())];
                    println!("    Goal {short_id} ({title}):");
                    println!("      Staging version: {staging_ver}");
                    println!("      Source version:  {source_ver}");
                    println!("      Option A — source is behind (draft bumped correctly):");
                    println!("        cd <project-root>");
                    println!("        ./scripts/bump-version.sh {staging_ver}");
                    println!("        ta draft apply {short_id}");
                    println!("      Option B — staging has wrong version (rebuild draft):");
                    println!(
                        "        ta draft deny {short_id} --reason \"version mismatch, redo\""
                    );
                    println!("        ta run \"<goal>\" --follow-up --phase <phase>");
                    println!("      Note: never run bump-version.sh from inside the staging dir.");
                }
                warn += 1;
            }
        }
    }

    println!();
    println!("{} passed, {} warnings, {} failures", pass, warn, fail);
    if fail > 0 {
        Err(anyhow::anyhow!("{} health check(s) failed", fail))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn start_goal_creates_overlay_and_goal_run() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        start_goal(
            &config,
            &store,
            "Test goal",
            Some(project.path()),
            "Test objective",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        // Verify goal was created.
        let goals = store.list().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].title, "Test goal");
        assert_eq!(goals[0].state, GoalRunState::Running);
        assert!(goals[0].source_dir.is_some());

        // Verify staging workspace was created.
        assert!(goals[0].workspace_path.exists());
        assert!(goals[0].workspace_path.join("README.md").exists());
        assert!(goals[0].workspace_path.join("src/main.rs").exists());
    }

    // ── v0.4.1.2 tests: follow-up draft continuity ──

    #[test]
    fn follow_up_extend_reuses_parent_staging() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create parent goal.
        start_goal(
            &config,
            &store,
            "Parent goal",
            Some(project.path()),
            "Parent objective",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        let parent = &goals[0];
        let parent_id = parent.goal_run_id;
        let parent_workspace = parent.workspace_path.clone();

        // Create follow-up that extends parent staging.
        let follow_up = start_goal_extending_parent(
            &config,
            &store,
            "Follow-up goal",
            "Follow-up objective",
            "test-agent",
            None,
            parent,
            parent_id,
        )
        .unwrap();

        // Follow-up should reuse the same workspace path.
        assert_eq!(follow_up.workspace_path, parent_workspace);
        assert_eq!(follow_up.parent_goal_id, Some(parent_id));
        assert_eq!(follow_up.source_dir, parent.source_dir);
        assert_eq!(follow_up.state, GoalRunState::Running);

        // Both goals stored.
        let all_goals = store.list().unwrap();
        assert_eq!(all_goals.len(), 2);
    }

    #[test]
    fn check_parent_staging_returns_none_when_staging_missing() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create parent goal.
        start_goal(
            &config,
            &store,
            "Parent goal",
            Some(project.path()),
            "Parent objective",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        let parent_id = goals[0].goal_run_id;
        let parent_workspace = goals[0].workspace_path.clone();

        // Remove parent staging directory.
        std::fs::remove_dir_all(&parent_workspace).unwrap();

        // check_parent_staging_eligible should return None.
        let result = check_parent_staging_eligible(&store, parent_id, &config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_goal_removes_metadata_and_staging() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create a goal to delete.
        start_goal(
            &config,
            &store,
            "Doomed goal",
            Some(project.path()),
            "Will be deleted",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        assert_eq!(goals.len(), 1);
        let goal_id = goals[0].goal_run_id;
        let staging_path = goals[0].workspace_path.clone();
        assert!(staging_path.exists());

        // Delete the goal.
        delete_goal(&store, &config, &goal_id.to_string(), None).unwrap();

        // Verify metadata is removed.
        assert!(store.get(goal_id).unwrap().is_none());

        // Verify staging directory is removed.
        assert!(!staging_path.exists());
    }

    #[test]
    fn gc_transitions_zombie_goals_to_failed() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create a goal.
        start_goal(
            &config,
            &store,
            "Old goal",
            Some(project.path()),
            "Will become zombie",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        assert_eq!(goals.len(), 1);
        let goal_id = goals[0].goal_run_id;

        // Manually backdate the goal's updated_at to make it stale.
        let mut g = store.get(goal_id).unwrap().unwrap();
        g.updated_at = chrono::Utc::now() - chrono::Duration::days(10);
        store.save(&g).unwrap();

        // Run gc in dry-run mode — goal should be listed but not changed.
        gc_goals(&store, &config, true, false, 7).unwrap();
        let g = store.get(goal_id).unwrap().unwrap();
        assert_eq!(g.state, GoalRunState::Running); // unchanged

        // Run gc for real — goal should transition to failed.
        gc_goals(&store, &config, false, false, 7).unwrap();
        let g = store.get(goal_id).unwrap().unwrap();
        assert!(matches!(g.state, GoalRunState::Failed { .. }));
    }

    #[test]
    fn gc_detects_missing_staging() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create a goal.
        start_goal(
            &config,
            &store,
            "Missing staging",
            Some(project.path()),
            "Staging will disappear",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        let goal_id = goals[0].goal_run_id;
        let staging = goals[0].workspace_path.clone();

        // Remove staging directory.
        std::fs::remove_dir_all(&staging).unwrap();

        // Run gc — should detect missing staging and mark failed.
        gc_goals(&store, &config, false, false, 7).unwrap();
        let g = store.get(goal_id).unwrap().unwrap();
        assert!(matches!(g.state, GoalRunState::Failed { .. }));
    }

    #[test]
    fn resolve_goal_id_by_prefix() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        execute(
            &GoalCommands::Start {
                title: "Prefix goal".to_string(),
                source: Some(temp.path().to_path_buf()),
                objective: "Test goal prefix matching".to_string(),
                agent: "test-agent".to_string(),
                phase: None,
                follow_up: None,
                objective_file: None,
            },
            &config,
        )
        .unwrap();

        let goals = store.list().unwrap();
        let goal_id = goals[0].goal_run_id;

        // Full UUID resolves.
        let resolved = resolve_goal_id(&goal_id.to_string(), &store).unwrap();
        assert_eq!(resolved, goal_id);

        // 8-char prefix resolves.
        let prefix = &goal_id.to_string()[..8];
        let resolved = resolve_goal_id(prefix, &store).unwrap();
        assert_eq!(resolved, goal_id);

        // Short prefix rejected.
        let result = resolve_goal_id("abc", &store);
        assert!(result.is_err());
    }

    // ── v0.11.3 tests: inspect, post-mortem, pre-flight, doctor ──

    #[test]
    fn disk_check_returns_result() {
        let dir = tempfile::tempdir().unwrap();
        let result = check_disk_space_df(dir.path());
        match result {
            DiskCheck::Ok(bytes) => assert!(bytes > 0),
            DiskCheck::Low(bytes) => assert!(bytes > 0),
            DiskCheck::Unknown => {} // acceptable
        }
    }

    #[test]
    fn format_bytes_display() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1500), "1.5 KB");
        assert_eq!(format_bytes(1_500_000), "1.4 MB");
        assert_eq!(format_bytes(1_500_000_000), "1.4 GB");
    }

    #[test]
    fn is_process_alive_current_process() {
        assert!(is_process_alive(std::process::id()));
    }

    #[test]
    fn is_process_alive_dead_process() {
        assert!(!is_process_alive(99999999));
    }

    #[test]
    fn load_draft_summary_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_draft_summary(dir.path(), uuid::Uuid::new_v4());
        assert!(result.is_none());
    }

    #[test]
    fn pre_flight_runs_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // Should not panic; may return error (missing .ta dir) but that is ok.
        let _ = goal_pre_flight(&config, Some("test"));
    }

    #[test]
    fn goal_inspect_runs_for_existing_goal() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        start_goal(
            &config,
            &store,
            "Inspect target",
            Some(project.path()),
            "Test inspect",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        let id = goals[0].goal_run_id.to_string();

        // Human-readable output should succeed.
        goal_inspect(&config, &store, &id, false).unwrap();

        // JSON output should succeed.
        goal_inspect(&config, &store, &id, true).unwrap();
    }

    #[test]
    fn goal_post_mortem_runs_for_existing_goal() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        start_goal(
            &config,
            &store,
            "PostMortem target",
            Some(project.path()),
            "Test post-mortem",
            "test-agent",
            None,
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        let id = goals[0].goal_run_id.to_string();

        goal_post_mortem(&config, &store, &id).unwrap();
    }

    #[test]
    fn read_recent_events_empty_when_no_events() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let goal = ta_goal::GoalRun::new(
            "Test",
            "obj",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let events = read_recent_events(&config, &goal, 10);
        assert!(events.is_empty());
    }

    #[test]
    fn read_agent_log_tail_empty_when_no_log() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let goal = ta_goal::GoalRun::new(
            "Test",
            "obj",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let lines = read_agent_log_tail(&config, &goal, 20);
        assert!(lines.is_empty());
    }

    #[test]
    fn doctor_gc_checks_emit_warning_for_stale_staging() {
        // Verifies that doctor runs without panicking for a project with a staging dir.
        let dir = tempfile::tempdir().unwrap();
        let staging_dir = dir.path().join(".ta").join("staging");
        std::fs::create_dir_all(&staging_dir).unwrap();

        let stale = staging_dir.join("old-goal-1234");
        std::fs::create_dir_all(&stale).unwrap();

        let config = GatewayConfig::for_project(dir.path());
        // doctor::execute returns Err only on hard failures (not warnings).
        let _ = crate::commands::doctor::execute(&config, false);
    }

    // ── v0.15.13.5: delete resets in_progress plan phase ──────────────

    #[test]
    fn delete_goal_resets_in_progress_phase_to_pending() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();

        // Write a PLAN.md with the target phase in_progress.
        std::fs::write(
            project.path().join("PLAN.md"),
            "### v0.99.2 — Delete reset test\n<!-- status: in_progress -->\n\n- [ ] item\n",
        )
        .unwrap();

        let config = GatewayConfig::for_project(project.path());
        let store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Start a goal with a linked phase.
        start_goal(
            &config,
            &store,
            "Delete reset test",
            Some(project.path()),
            "test",
            "test-agent",
            Some("v0.99.2"),
            None,
            None,
        )
        .unwrap();

        let goals = store.list().unwrap();
        let goal_id = goals[0].goal_run_id.to_string();

        delete_goal(&store, &config, &goal_id, None).unwrap();

        let plan_after = std::fs::read_to_string(project.path().join("PLAN.md")).unwrap();
        assert!(
            plan_after.contains("<!-- status: pending -->"),
            "phase should be reset to pending after goal delete: {}",
            plan_after
        );
        assert!(
            !plan_after.contains("<!-- status: in_progress -->"),
            "in_progress marker should be gone: {}",
            plan_after
        );
    }
}
