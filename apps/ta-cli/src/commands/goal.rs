// goal.rs — Goal subcommands: start, list, status, constitution.

use std::path::PathBuf;

use clap::Subcommand;
use ta_goal::{GoalHistoryLedger, GoalRunState, GoalRunStore, HistoryFilter};
use ta_mcp_gateway::GatewayConfig;
use ta_policy::constitution::{
    AccessConstitution, ConstitutionEntry, ConstitutionStore, EnforcementMode,
};
use ta_workspace::{ExcludePatterns, OverlayWorkspace};
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
/// Returns `Some(parent_goal)` if the parent's staging exists and should be reused,
/// or `None` if a fresh staging copy should be created.
fn should_extend_parent_staging(
    store: &GoalRunStore,
    parent_goal_id: Uuid,
    config: &GatewayConfig,
) -> anyhow::Result<Option<ta_goal::GoalRun>> {
    let parent = match check_parent_staging_eligible(store, parent_goal_id, config)? {
        Some(p) => p,
        None => return Ok(None),
    };

    // Show the parent's draft info if available.
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

    // Prompt user (default yes). In non-interactive contexts (tests, CI),
    // fall back to the configured default.
    eprint!("Continue in staging for \"{}\"? [Y/n] ", parent.title);

    // Read response — accept empty/y/Y as yes.
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

    store.save(&goal)?;
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
            sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

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
        GoalCommands::Delete { id } => delete_goal(&store, id),
        GoalCommands::Constitution { command } => execute_constitution(command, config, &store),
        GoalCommands::Gc {
            dry_run,
            include_staging,
            threshold_days,
        } => gc_goals(&store, config, *dry_run, *include_staging, *threshold_days),
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
    let extend_parent = if let Some(pid) = parent_goal_id {
        should_extend_parent_staging(store, pid, config)?
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

        // V1 TEMPORARY: Load exclude patterns from .taignore or defaults.
        let excludes = ExcludePatterns::load(&source_dir);
        let overlay =
            OverlayWorkspace::create(&goal_id, &source_dir, &config.staging_dir, excludes)?;

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

        store.save(&goal)?;

        println!("Goal started: {}", goal.goal_run_id);
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
    if !all && state.is_none() || active {
        goals.retain(|g| {
            !matches!(
                g.state,
                GoalRunState::Applied | GoalRunState::Completed | GoalRunState::Failed { .. }
            )
        });
    }

    if goals.is_empty() {
        println!("No goal runs found.");
        return Ok(());
    }

    // Load draft packages to show inline draft/VCS status.
    let packages = load_all_packages_silent(config);

    println!(
        "{:<22} {:<22} {:<12} {:<10} {:<10} {:<14}",
        "TAG", "TITLE", "STATE", "HEALTH", "DRAFT", "VCS"
    );
    println!("{}", "-".repeat(90));

    for g in &goals {
        let tag = g.display_tag();
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

        println!(
            "{:<22} {:<22} {:<12} {:<10} {:<10} {:<14}",
            truncate(&tag, 20),
            title_display,
            g.state.to_string(),
            health,
            draft_col,
            vcs_col,
        );
    }
    println!("\n{} goal(s) total.", goals.len());

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
            println!("State:    {}", g.state);
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

fn delete_goal(store: &GoalRunStore, id: &str) -> anyhow::Result<()> {
    let goal_run_id = resolve_goal_id(id, store)?;
    let goal = store.get(goal_run_id)?;

    match goal {
        Some(g) => {
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
    }

    Ok(())
}

/// Garbage-collect zombie goals and stale staging directories (v0.9.5.1).
fn gc_goals(
    store: &GoalRunStore,
    _config: &GatewayConfig,
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
        GoalRunState::Running | GoalRunState::AwaitingInput { .. } => match goal.agent_pid {
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
        delete_goal(&store, &goal_id.to_string()).unwrap();

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
}
