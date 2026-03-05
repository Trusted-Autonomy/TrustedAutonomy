// goal.rs — Goal subcommands: start, list, status, constitution.

use std::path::PathBuf;

use clap::Subcommand;
use ta_goal::{GoalRunState, GoalRunStore};
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
    /// List all goal runs.
    List {
        /// Filter by state (e.g., "running", "pr_ready", "completed").
        #[arg(long)]
        state: Option<String>,
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
        GoalCommands::List { state } => list_goals(&store, state.as_deref()),
        GoalCommands::Status { id, json } => show_status(&store, id, *json),
        GoalCommands::Delete { id } => delete_goal(&store, id),
        GoalCommands::Constitution { command } => execute_constitution(command, config, &store),
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

/// Resolve a goal ID from a full UUID or an 8+ character prefix.
fn resolve_goal_id(id: &str, store: &GoalRunStore) -> anyhow::Result<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(uuid);
    }

    if id.len() < 8 {
        anyhow::bail!(
            "ID prefix '{}' is too short -- use at least 8 characters (or a full UUID)",
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
            "Ambiguous prefix '{}' matches {} goals. Use a longer prefix.",
            id,
            n
        ),
    }
}

fn list_goals(store: &GoalRunStore, state: Option<&str>) -> anyhow::Result<()> {
    let goals = if let Some(state_filter) = state {
        store.list_by_state(state_filter)?
    } else {
        store.list()?
    };

    if goals.is_empty() {
        println!("No goal runs found.");
        return Ok(());
    }

    println!(
        "{:<38} {:<30} {:<14} {:<12}",
        "ID", "TITLE", "STATE", "AGENT"
    );
    println!("{}", "-".repeat(94));

    for g in &goals {
        let title_with_chain = if g.is_macro {
            format!("[M] {}", truncate(&g.title, 24))
        } else if let Some(ref macro_id) = g.parent_macro_id {
            format!(
                "  +- {} (<- {})",
                truncate(&g.title, 16),
                &macro_id.to_string()[..8]
            )
        } else if let Some(parent_id) = g.parent_goal_id {
            format!(
                "{} (-> {})",
                truncate(&g.title, 20),
                &parent_id.to_string()[..8]
            )
        } else {
            truncate(&g.title, 28)
        };

        println!(
            "{:<38} {:<30} {:<14} {:<12}",
            g.goal_run_id,
            title_with_chain,
            g.state.to_string(),
            g.agent_id,
        );
    }
    println!("\n{} goal(s) total.", goals.len());

    Ok(())
}

fn show_status(store: &GoalRunStore, id: &str, json_output: bool) -> anyhow::Result<()> {
    let goal_run_id = resolve_goal_id(id, store)?;
    match store.get(goal_run_id)? {
        Some(g) => {
            if json_output {
                let json = serde_json::to_string_pretty(&g)?;
                println!("{}", json);
                return Ok(());
            }
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
            if let Some(pr_id) = g.pr_package_id {
                println!("Draft:    {}", pr_id);
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max - 3])
    } else {
        s.to_string()
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
