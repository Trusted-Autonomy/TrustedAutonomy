// session.rs — Interactive and workflow session management commands.
//
// Interactive session commands (existing):
//   ta session list        — list active interactive sessions across channels
//   ta session show <id>   — display session details and message history
//   ta session resume <id> — resume a paused interactive session
//   ta session pause <id>  — pause a running session
//   ta session abort <id>  — abort a session
//   ta session status      — show session status summary
//   ta session close <id>  — close a session cleanly
//
// Workflow session commands (v0.14.11):
//   ta session start <plan-id>  — instantiate a WorkflowSession from a PlanDocument
//   ta session review [<id>]    — interactively accept/edit/skip/defer plan items
//   ta session run [<id>]       — execute accepted items as governed workflow
//   ta session projects         — list workflow sessions (project-level oversight)

use std::io::{self, BufRead, Write};

use clap::Subcommand;
use ta_changeset::{InteractiveSessionState, InteractiveSessionStore};
use ta_goal::GoalRunStore;
use ta_mcp_gateway::GatewayConfig;
use ta_memory::memory_store_from_config;
use ta_session::{
    GateMode, PlanDocument, SessionManager, WorkflowItemState, WorkflowSession,
    WorkflowSessionManager, WorkflowSessionState,
};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum SessionCommands {
    /// List interactive sessions.
    List {
        /// Show all sessions (including completed/aborted). Default: alive only.
        #[arg(long)]
        all: bool,
        /// Show workflow (project-level) sessions instead of interactive sessions.
        #[arg(long)]
        workflow: bool,
    },
    /// Show details for a specific session.
    Show {
        /// Session ID (full or prefix).
        id: String,
    },
    /// Resume a paused interactive session.
    Resume {
        /// Session ID (full or prefix).
        id: String,
        /// Agent to use for resume (defaults to session's original agent).
        #[arg(long)]
        agent: Option<String>,
    },
    /// Pause a running session (v0.6.0).
    Pause {
        /// Session ID (full or prefix).
        id: String,
    },
    /// Abort a session (v0.6.0).
    Abort {
        /// Session ID (full or prefix).
        id: String,
        /// Reason for aborting.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Show session status summary (v0.6.0).
    Status {
        /// Workflow session ID to inspect (full UUID or prefix). If omitted, shows
        /// interactive session summary.
        id: Option<String>,
        /// Auto-refresh every 2 seconds (requires a workflow session ID).
        #[arg(long)]
        live: bool,
    },
    /// Close a session cleanly (v0.7.5).
    ///
    /// Marks the session as completed. If the session's staging directory has
    /// uncommitted changes, automatically triggers `ta draft build` before closing.
    /// Prevents orphaned sessions when PTY exits abnormally (Ctrl-C, crash).
    Close {
        /// Session ID (full or prefix).
        id: String,
        /// Skip automatic draft build even if there are uncommitted changes.
        #[arg(long)]
        no_draft: bool,
    },

    // ── Workflow session commands (v0.14.11) ──────────────────────────────
    /// Start a workflow session from a PlanDocument (v0.14.11).
    ///
    /// Instantiates a WorkflowSession from the plan produced by `ta new plan --from`.
    /// The session starts in `reviewing` state — run `ta session review` next.
    Start {
        /// Plan ID returned by `ta new plan --from brief.md`.
        plan_id: String,
        /// Gate mode: auto (default), prompt, or always.
        #[arg(long, default_value = "auto")]
        gate: String,
    },

    /// Interactively review plan items before execution (v0.14.11).
    ///
    /// For each pending item: [A]ccept / [E]dit / [S]kip / [D]efer.
    /// Accepted items enter the execution queue for `ta session run`.
    Review {
        /// Workflow session ID (full UUID or prefix). If omitted, uses the
        /// most recently created session that is in `reviewing` state.
        id: Option<String>,
    },

    /// Execute accepted plan items as a governed workflow (v0.14.11).
    ///
    /// Runs each accepted item in order, with an AwaitHuman gate between items
    /// (configurable). Streams progress to stdout. On interruption, saves state
    /// so `ta session run` can resume.
    Run {
        /// Workflow session ID (full UUID or prefix). If omitted, uses the
        /// most recently created session with accepted items.
        id: Option<String>,
        /// Gate mode override (auto, prompt, always). Overrides the session's
        /// saved gate mode for this run.
        #[arg(long)]
        gate: Option<String>,
        /// Maximum number of items to execute concurrently (default: 1 = sequential).
        #[arg(long, default_value = "1")]
        parallel: usize,
    },

    /// List workflow (project-level) sessions (v0.14.11).
    ///
    /// Shows all WorkflowSessions with their state, item counts, and last activity.
    /// Use `ta session list --workflow` for the same output inline with interactive sessions.
    Projects {
        /// Show all sessions (default: active only).
        #[arg(long)]
        all: bool,
    },
}

pub fn execute(cmd: &SessionCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone())?;

    match cmd {
        SessionCommands::List { all, workflow } => {
            if *workflow {
                list_workflow_sessions(config, *all)
            } else {
                list_sessions(&store, *all)
            }
        }
        SessionCommands::Show { id } => show_session(&store, id),
        SessionCommands::Resume { id, agent } => {
            let agent = agent.as_deref().unwrap_or("claude-code");
            super::run::execute(
                config,
                None,
                agent,
                None,
                "",
                None,
                None,
                None, // follow_up_draft
                None, // follow_up_goal
                None,
                false,
                true,
                false,
                Some(id.as_str()),
                false, // not headless
                false, // skip_verify = false
                false, // quiet = false
                None,  // no existing goal id
                None,  // workflow = default (single-agent)
            )
        }
        SessionCommands::Pause { id } => pause_session(config, id),
        SessionCommands::Abort { id, reason } => abort_session(config, id, reason.as_deref()),
        SessionCommands::Status { id, live } => session_status(config, id.as_deref(), *live),
        SessionCommands::Close { id, no_draft } => close_session(config, id, *no_draft),
        // ── Workflow session commands ──────────────────────────────────────
        SessionCommands::Start { plan_id, gate } => start_session(config, plan_id, gate),
        SessionCommands::Review { id } => review_session(config, id.as_deref()),
        SessionCommands::Run { id, gate, parallel } => {
            run_session(config, id.as_deref(), gate.as_deref(), *parallel)
        }
        SessionCommands::Projects { all } => list_workflow_sessions(config, *all),
    }
}

fn list_sessions(store: &InteractiveSessionStore, all: bool) -> anyhow::Result<()> {
    let sessions = if all {
        store.list()?
    } else {
        store.list_alive()?
    };

    if sessions.is_empty() {
        if all {
            println!("No interactive sessions found.");
        } else {
            println!("No active interactive sessions. Use --all to see completed sessions.");
        }
        return Ok(());
    }

    println!(
        "{:<38} {:<38} {:<12} {:<14} {:<10}",
        "SESSION ID", "GOAL ID", "AGENT", "STATE", "ELAPSED"
    );
    println!("{}", "-".repeat(112));

    for s in &sessions {
        println!(
            "{:<38} {:<38} {:<12} {:<14} {:<10}",
            s.session_id,
            s.goal_id,
            truncate(&s.agent_id, 10),
            s.state.to_string(),
            s.elapsed_display(),
        );
    }

    println!("\n{} session(s).", sessions.len());
    Ok(())
}

fn show_session(store: &InteractiveSessionStore, id: &str) -> anyhow::Result<()> {
    // Try exact UUID parse first, then prefix match.
    let session = if let Ok(uuid) = Uuid::parse_str(id) {
        store.load(uuid)?
    } else {
        let all = store.list()?;
        let matches: Vec<_> = all
            .into_iter()
            .filter(|s| s.session_id.to_string().starts_with(id))
            .collect();
        match matches.len() {
            0 => anyhow::bail!("No session found matching '{}'", id),
            1 => matches.into_iter().next().unwrap(),
            n => anyhow::bail!("Ambiguous prefix '{}' matches {} sessions", id, n),
        }
    };

    println!("Session:   {}", session.session_id);
    println!("Goal:      {}", session.goal_id);
    println!("Channel:   {}", session.channel_id);
    println!("Agent:     {}", session.agent_id);
    println!("State:     {}", session.state);
    println!("Created:   {}", session.created_at.to_rfc3339());
    println!("Updated:   {}", session.updated_at.to_rfc3339());
    println!("Elapsed:   {}", session.elapsed_display());

    if !session.draft_ids.is_empty() {
        println!("Drafts:    {}", session.draft_ids.len());
        for draft_id in &session.draft_ids {
            println!("  - {}", draft_id);
        }
    }

    if !session.messages.is_empty() {
        println!("\nMessage log ({} messages):", session.messages.len());
        println!("{}", "-".repeat(60));
        for msg in &session.messages {
            let time = msg.timestamp.format("%H:%M:%S");
            let preview = truncate(&msg.content, 70);
            println!("  [{}] {}: {}", time, msg.sender, preview);
        }
    }

    Ok(())
}

fn pause_session(config: &GatewayConfig, id: &str) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let manager = SessionManager::new(sessions_dir)?;

    let uuid = resolve_session_id(&manager, id)?;
    manager.pause(uuid)?;
    println!("Session {} paused.", &uuid.to_string()[..8]);
    Ok(())
}

fn abort_session(config: &GatewayConfig, id: &str, reason: Option<&str>) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let manager = SessionManager::new(sessions_dir)?;

    let uuid = resolve_session_id(&manager, id)?;
    manager.abort(uuid)?;
    println!(
        "Session {} aborted.{}",
        &uuid.to_string()[..8],
        reason
            .map(|r| format!(" Reason: {}", r))
            .unwrap_or_default()
    );
    Ok(())
}

fn session_status(config: &GatewayConfig, id: Option<&str>, live: bool) -> anyhow::Result<()> {
    // If an ID is provided, show the workflow session status dashboard.
    if let Some(id) = id {
        return workflow_session_status(config, id, live);
    }

    // Default: show interactive session summary.
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let manager = SessionManager::new(sessions_dir)?;

    let active = manager.list_active()?;
    if active.is_empty() {
        println!("No active sessions.");
        return Ok(());
    }

    println!(
        "{:<38} {:<38} {:<12} {:<14} {:<6} {:<10}",
        "SESSION ID", "GOAL ID", "AGENT", "STATE", "ITER", "ELAPSED"
    );
    println!("{}", "-".repeat(118));

    for s in &active {
        println!(
            "{:<38} {:<38} {:<12} {:<14} {:<6} {:<10}",
            s.session_id,
            s.goal_id,
            truncate(&s.agent_id, 10),
            format!("{}", s.state),
            s.iteration_count,
            s.elapsed_display(),
        );
    }

    println!("\n{} active session(s).", active.len());
    Ok(())
}

fn resolve_session_id(manager: &SessionManager, id: &str) -> anyhow::Result<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(id) {
        return Ok(uuid);
    }
    let all = manager.list()?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|s| s.session_id.to_string().starts_with(id))
        .collect();
    match matches.len() {
        0 => anyhow::bail!("No session found matching '{}'", id),
        1 => Ok(matches[0].session_id),
        n => anyhow::bail!("Ambiguous prefix '{}' matches {} sessions", id, n),
    }
}

fn close_session(config: &GatewayConfig, id: &str, no_draft: bool) -> anyhow::Result<()> {
    let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone())?;
    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    // Find session by ID or prefix.
    let all = store.list()?;
    let mut session = {
        if let Ok(uuid) = Uuid::parse_str(id) {
            store.load(uuid)?
        } else {
            let matches: Vec<_> = all
                .into_iter()
                .filter(|s| s.session_id.to_string().starts_with(id))
                .collect();
            match matches.len() {
                0 => anyhow::bail!("No session found matching '{}'", id),
                1 => matches.into_iter().next().unwrap(),
                n => anyhow::bail!("Ambiguous prefix '{}' matches {} sessions", id, n),
            }
        }
    };

    // Only close sessions that are alive (active or paused).
    if !session.is_alive() {
        println!(
            "Session {} is already {} — nothing to close.",
            &session.session_id.to_string()[..8],
            session.state
        );
        return Ok(());
    }

    // Check if staging directory has changes and offer to build a draft.
    if !no_draft {
        let goal = goal_store
            .list()?
            .into_iter()
            .find(|g| g.goal_run_id == session.goal_id);

        if let Some(ref goal) = goal {
            if goal.workspace_path.exists() {
                // Check if the staging directory has any modifications by comparing
                // file count or checking for a change_summary.json.
                let change_summary_path = goal.workspace_path.join(".ta/change_summary.json");
                let has_changes = !change_summary_path.exists();

                if has_changes {
                    println!("Building draft from staging workspace before closing...");
                    match super::draft::build_package(
                        config,
                        &session.goal_id.to_string(),
                        "Auto-built on session close",
                        false,
                    ) {
                        Ok(()) => {
                            println!("Draft built successfully.");
                        }
                        Err(e) => {
                            println!(
                                "Warning: draft build failed ({}). Closing session anyway.",
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    // Transition to Completed.
    session.transition(InteractiveSessionState::Completed)?;
    session.log_message("ta-system", "Session closed via `ta session close`");
    store.save(&session)?;

    println!("Session {} closed.", &session.session_id.to_string()[..8]);
    Ok(())
}

/// Check if a session's child process is still alive.
///
/// Used by `ta session resume` to detect dead PTY processes before reattaching.
/// If the process is dead, the user is informed and offered recovery options.
#[allow(dead_code)] // Used by execute_resume (unix-only) and tests
pub fn check_session_health(
    _store: &InteractiveSessionStore,
    goal_store: &GoalRunStore,
    session: &ta_changeset::InteractiveSession,
) -> SessionHealthStatus {
    // Look up the goal to check workspace state.
    let goal = goal_store
        .list()
        .ok()
        .and_then(|goals| goals.into_iter().find(|g| g.goal_run_id == session.goal_id));

    match goal {
        None => SessionHealthStatus::WorkspaceMissing,
        Some(g) => {
            if !g.workspace_path.exists() {
                return SessionHealthStatus::WorkspaceMissing;
            }
            let has_staging_changes = !g.workspace_path.join(".ta/change_summary.json").exists();
            SessionHealthStatus::Healthy {
                has_staging_changes,
            }
        }
    }
}

/// Health status of a session for resume checks.
#[derive(Debug)]
#[allow(dead_code)] // has_staging_changes is read in execute_resume (unix-only)
pub enum SessionHealthStatus {
    /// Session workspace is intact and ready for resume.
    Healthy { has_staging_changes: bool },
    /// The staging workspace directory no longer exists.
    WorkspaceMissing,
}

// ── Workflow session helpers ──────────────────────────────────────────────────

/// `ta session start <plan-id>` — instantiate a WorkflowSession from a PlanDocument.
fn start_session(config: &GatewayConfig, plan_id: &str, gate: &str) -> anyhow::Result<()> {
    let gate_mode: GateMode = gate.parse().map_err(|e: String| {
        anyhow::anyhow!(
            "Invalid --gate value '{}': {}\nUse: auto, prompt, or always.",
            gate,
            e
        )
    })?;

    // Load the PlanDocument from memory.
    let plan_uuid = Uuid::parse_str(plan_id).map_err(|_| {
        anyhow::anyhow!(
            "Invalid plan ID '{}'. Provide the UUID printed by `ta new plan --from`.",
            plan_id
        )
    })?;

    let memory = memory_store_from_config(&config.workspace_root);
    let key = format!("plan/{}", plan_uuid);

    let entry = memory
        .recall(&key)
        .map_err(|e| {
            anyhow::anyhow!(
                "Plan '{}' not found in memory: {}\n\
                 Run `ta new plan --from brief.md` to generate a plan first.",
                plan_id,
                e
            )
        })?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Plan '{}' not found in memory.\n\
                 Run `ta new plan --from brief.md` to generate a plan first.",
                plan_id
            )
        })?;

    let plan: PlanDocument = serde_json::from_value(entry.value.clone()).map_err(|e| {
        anyhow::anyhow!(
            "Failed to deserialize PlanDocument from memory key '{}': {}",
            key,
            e
        )
    })?;

    // Check if a session already exists for this plan.
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let mgr = WorkflowSessionManager::new(sessions_dir)?;

    if let Some(existing) = mgr.find_for_plan(plan_uuid)? {
        println!(
            "A workflow session already exists for plan {} (session {}).",
            &plan_id[..8.min(plan_id.len())],
            &existing.session_id.to_string()[..8]
        );
        println!("State: {}", existing.state);
        println!();
        println!("Options:");
        println!(
            "  ta session review {}  — review plan items",
            &existing.session_id.to_string()[..8]
        );
        println!(
            "  ta session run {}     — execute accepted items",
            &existing.session_id.to_string()[..8]
        );
        return Ok(());
    }

    // Create the workflow session.
    let session = WorkflowSession::from_plan(&plan, gate_mode);
    mgr.save(&session)?;

    let short_id = &session.session_id.to_string()[..8];
    println!("Created workflow session: {}", session.session_id);
    println!(
        "  Plan:  {} ({})",
        plan.title,
        &plan_id[..8.min(plan_id.len())]
    );
    println!("  Items: {} item(s)", session.items.len());
    println!("  Gate:  {}", session.gate_mode);
    println!("  State: {}", session.state);
    println!();
    println!("Next: review plan items before execution:");
    println!(
        "  ta session review {}  — accept, edit, skip, or defer each item",
        short_id
    );

    Ok(())
}

/// `ta session review [<id>]` — interactive plan item review loop.
fn review_session(config: &GatewayConfig, id: Option<&str>) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let mgr = WorkflowSessionManager::new(sessions_dir)?;

    let mut session = resolve_workflow_session(
        &mgr,
        id,
        |s| s.state == WorkflowSessionState::Reviewing,
        "reviewing",
    )?;

    let pending_items: Vec<_> = session
        .items
        .iter()
        .filter(|i| i.state == WorkflowItemState::Pending)
        .map(|i| i.item_id)
        .collect();

    if pending_items.is_empty() {
        println!(
            "No pending items to review in session {}.",
            &session.session_id.to_string()[..8]
        );
        let accepted = session.count_by_state(&WorkflowItemState::Accepted);
        println!("{} item(s) already accepted.", accepted);
        if accepted > 0 {
            println!();
            println!(
                "Ready to execute: ta session run {}",
                &session.session_id.to_string()[..8]
            );
        }
        return Ok(());
    }

    println!("=== Reviewing plan: {} ===", session.plan_title);
    println!(
        "Session: {}  |  {} pending item(s)",
        &session.session_id.to_string()[..8],
        pending_items.len()
    );
    println!();
    println!("For each item: [A]ccept  [E]dit title  [S]kip  [D]efer  [Q]uit review");
    println!("{}", "─".repeat(72));

    let stdin = io::stdin();
    let mut accepted_count = 0usize;
    let mut skipped_count = 0usize;
    let mut deferred_count = 0usize;

    for (idx, &item_id) in pending_items.iter().enumerate() {
        // Re-read item from current session state.
        let item = match session.items.iter().find(|i| i.item_id == item_id) {
            Some(i) => i.clone(),
            None => continue,
        };

        println!();
        println!("[{}/{}] {}", idx + 1, pending_items.len(), item.title);
        if !item.acceptance_criteria.is_empty() {
            for crit in &item.acceptance_criteria {
                println!("       • {}", crit);
            }
        }
        if let Some(effort) = &item.estimated_effort {
            println!("       effort: {}", effort);
        }

        loop {
            print!("  Choice [A/E/S/D/Q]: ");
            io::stdout().flush()?;

            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            let choice = line.trim().to_ascii_uppercase();

            match choice.as_str() {
                "A" | "" => {
                    session.update_item_state(item_id, WorkflowItemState::Accepted);
                    println!("  ✓ Accepted");
                    accepted_count += 1;
                    break;
                }
                "E" => {
                    print!("  New title [{}]: ", item.title);
                    io::stdout().flush()?;
                    let mut new_title = String::new();
                    stdin.lock().read_line(&mut new_title)?;
                    let new_title = new_title.trim();
                    if !new_title.is_empty() {
                        if let Some(i) = session.items.iter_mut().find(|i| i.item_id == item_id) {
                            i.title = new_title.to_string();
                        }
                    }
                    session.update_item_state(item_id, WorkflowItemState::Accepted);
                    println!("  ✓ Edited and accepted");
                    accepted_count += 1;
                    break;
                }
                "S" => {
                    session.update_item_state(item_id, WorkflowItemState::Skipped);
                    println!("  ✗ Skipped");
                    skipped_count += 1;
                    break;
                }
                "D" => {
                    session.update_item_state(item_id, WorkflowItemState::Deferred);
                    println!("  → Deferred");
                    deferred_count += 1;
                    break;
                }
                "Q" => {
                    println!();
                    println!("Review paused.");
                    mgr.save(&session)?;
                    println!(
                        "Progress saved. Accepted: {}, Skipped: {}, Deferred: {}",
                        accepted_count, skipped_count, deferred_count
                    );
                    return Ok(());
                }
                other => {
                    println!("  Unknown choice '{}'. Use A, E, S, D, or Q.", other);
                }
            }
        }
    }

    mgr.save(&session)?;

    println!();
    println!("{}", "─".repeat(72));
    println!("Review complete:");
    println!("  Accepted: {}", accepted_count);
    println!("  Skipped:  {}", skipped_count);
    println!("  Deferred: {}", deferred_count);

    if accepted_count > 0 {
        println!();
        println!("Ready to execute {} item(s):", accepted_count);
        println!("  ta session run {}", &session.session_id.to_string()[..8]);
    } else {
        println!();
        println!("No items accepted. Nothing to execute.");
    }

    Ok(())
}

/// `ta session run [<id>] [--gate ...] [--parallel n]` — execute accepted items.
fn run_session(
    config: &GatewayConfig,
    id: Option<&str>,
    gate_override: Option<&str>,
    parallel: usize,
) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let mgr = WorkflowSessionManager::new(sessions_dir)?;

    let mut session = resolve_workflow_session(
        &mgr,
        id,
        |s| {
            matches!(
                s.state,
                WorkflowSessionState::Reviewing | WorkflowSessionState::Paused
            ) || s
                .items
                .iter()
                .any(|i| i.state == WorkflowItemState::Accepted)
        },
        "with accepted items",
    )?;

    // Apply gate override if provided.
    if let Some(gate_str) = gate_override {
        session.gate_mode = gate_str
            .parse()
            .map_err(|e: String| anyhow::anyhow!("Invalid --gate value: {}", e))?;
    }

    let accepted_count = session.count_by_state(&WorkflowItemState::Accepted);
    if accepted_count == 0 {
        println!(
            "No accepted items in session {}.",
            &session.session_id.to_string()[..8]
        );
        println!("Run `ta session review` first to accept items.");
        return Ok(());
    }

    // Transition to Running.
    if session.state != WorkflowSessionState::Running {
        session.transition(WorkflowSessionState::Running)?;
    }
    mgr.save(&session)?;

    println!("=== Running workflow session: {} ===", session.plan_title);
    println!(
        "Session: {}  |  {} item(s) to execute  |  gate: {}",
        &session.session_id.to_string()[..8],
        accepted_count,
        session.gate_mode
    );
    if parallel > 1 {
        println!("  Concurrency: {} parallel item(s)", parallel);
    }
    println!();

    let mut executed = 0usize;
    let mut failed = 0usize;
    let mut applied = 0usize;

    // Sequential execution (parallel > 1 is noted but currently executes sequentially;
    // true parallel support is in v0.14.11 item 9).
    loop {
        // Re-load fresh session state before each item.
        session = mgr.load(session.session_id)?;

        let next_item = session.next_runnable().cloned();
        let item = match next_item {
            Some(i) => i,
            None => break,
        };

        let item_id = item.item_id;
        println!(
            "── Item [{}/{}]: {} ──",
            executed + 1,
            accepted_count,
            item.title
        );

        // Mark as running.
        session.update_item_state(item_id, WorkflowItemState::Running);
        mgr.save(&session)?;

        // Spawn `ta run "<title>" --headless` as a subprocess.
        let goal_title = &item.title;
        println!("  Spawning: ta run \"{}\" --headless", goal_title);

        let ta_bin = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "ta".to_string());

        let run_result = std::process::Command::new(&ta_bin)
            .args([
                "--project-root",
                &config.workspace_root.to_string_lossy(),
                "run",
                goal_title,
                "--headless",
                "--no-version-check",
            ])
            .output();

        session = mgr.load(session.session_id)?;

        let (goal_id_str, draft_id_str) = match run_result {
            Err(e) => {
                let reason = format!("Failed to invoke ta run: {}", e);
                eprintln!("  ✗ Error: {}", reason);
                session.fail_item(item_id, &reason);
                mgr.save(&session)?;
                failed += 1;
                executed += 1;
                continue;
            }
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !output.status.success() {
                    let reason = format!(
                        "ta run exited with status {}:\n{}",
                        output.status.code().unwrap_or(-1),
                        stderr.trim()
                    );
                    eprintln!("  ✗ Failed: {}", &reason[..reason.len().min(200)]);
                    session.fail_item(item_id, &reason[..reason.len().min(500)]);
                    mgr.save(&session)?;
                    failed += 1;
                    executed += 1;
                    continue;
                }

                // Extract goal_id and draft_id from stdout/stderr.
                let mut goal_id_s: Option<String> = None;
                let mut draft_id_s: Option<String> = None;
                for line in stdout.lines().chain(stderr.lines()) {
                    if let Some(id) = line.strip_prefix("goal_id: ") {
                        goal_id_s = Some(id.trim().to_string());
                    }
                    if let Some(id) = line.strip_prefix("draft_id: ") {
                        draft_id_s = Some(id.trim().to_string());
                    }
                }
                (goal_id_s, draft_id_s)
            }
        };

        // Record goal ID if we got one.
        if let Some(ref gid) = goal_id_str {
            if let Ok(uuid) = Uuid::parse_str(gid) {
                session.set_item_goal(item_id, uuid);
            }
        }

        let draft_id_uuid = draft_id_str
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok());

        println!(
            "  ✓ Agent completed{}",
            draft_id_str
                .as_deref()
                .map(|d| format!("  draft: {}", &d[..8.min(d.len())]))
                .unwrap_or_default()
        );

        // ─── AwaitHuman gate ───────────────────────────────────────────
        session.update_item_state(item_id, WorkflowItemState::AtGate);
        mgr.save(&session)?;

        let gate_passed = match &session.gate_mode {
            GateMode::Auto => {
                // Auto mode: always proceed (the reviewer agent verdict is the gate).
                println!("  Gate: auto — proceeding.");
                true
            }
            GateMode::Prompt | GateMode::Always => {
                // Human gate: ask for explicit approval.
                println!();
                println!("  ┌─ AwaitHuman gate ─────────────────────────────────────────");
                println!("  │  Item:  {}", item.title);
                if let Some(ref draft) = draft_id_str {
                    println!("  │  Draft: {}", draft);
                    println!(
                        "  │  Review: ta draft view {}",
                        &draft[..8.min(draft.len())]
                    );
                }
                println!("  └─ [A]pply and continue  [S]kip this item  [Q]uit session ─");
                print!("  Choice [A/S/Q]: ");
                io::stdout().flush()?;
                let mut line = String::new();
                io::stdin().lock().read_line(&mut line)?;
                match line.trim().to_ascii_uppercase().as_str() {
                    "A" | "" => {
                        println!("  ✓ Approved.");
                        true
                    }
                    "S" => {
                        println!("  → Skipped at gate.");
                        session.update_item_state(item_id, WorkflowItemState::Skipped);
                        mgr.save(&session)?;
                        executed += 1;
                        continue;
                    }
                    _ => {
                        println!("  Session paused.");
                        session.update_item_state(item_id, WorkflowItemState::Accepted);
                        session.transition(WorkflowSessionState::Paused)?;
                        mgr.save(&session)?;
                        println!();
                        println!(
                            "Saved. Resume with: ta session run {}",
                            &session.session_id.to_string()[..8]
                        );
                        return Ok(());
                    }
                }
            }
        };

        if gate_passed {
            // Apply the draft if we have one.
            if let Some(ref draft_id) = draft_id_str {
                println!("  Applying draft {}...", &draft_id[..8.min(draft_id.len())]);
                let apply_result = std::process::Command::new(&ta_bin)
                    .args([
                        "--project-root",
                        &config.workspace_root.to_string_lossy(),
                        "draft",
                        "apply",
                        draft_id,
                        "--git-commit",
                    ])
                    .output();

                match apply_result {
                    Ok(o) if o.status.success() => {
                        println!("  ✓ Draft applied.");
                        // Commit artifact to session memory.
                        commit_item_to_session_memory(
                            config,
                            session.session_id,
                            item_id,
                            draft_id,
                        );
                        session.complete_item(item_id, draft_id_uuid);
                        applied += 1;
                    }
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        let reason = format!(
                            "ta draft apply failed (exit {}): {}",
                            o.status.code().unwrap_or(-1),
                            stderr.trim()
                        );
                        eprintln!("  ✗ Apply failed: {}", &reason[..reason.len().min(200)]);
                        session.fail_item(item_id, &reason[..reason.len().min(500)]);
                        failed += 1;
                    }
                    Err(e) => {
                        let reason = format!("Failed to invoke ta draft apply: {}", e);
                        eprintln!("  ✗ {}", reason);
                        session.fail_item(item_id, &reason);
                        failed += 1;
                    }
                }
            } else {
                // No draft — mark complete anyway (goal may have had no changes).
                session.complete_item(item_id, None);
                applied += 1;
            }
        }

        mgr.save(&session)?;
        executed += 1;

        // If all items are terminal, finish.
        if session.all_items_terminal() {
            break;
        }
    }

    // Reload final session state.
    session = mgr.load(session.session_id)?;

    // Transition to Complete if all items are terminal.
    if session.all_items_terminal() && session.state == WorkflowSessionState::Running {
        session.transition(WorkflowSessionState::Complete)?;
        mgr.save(&session)?;
    }

    println!();
    println!("═══ Session summary: {} ═══", session.plan_title);
    println!("  Applied:  {}", applied);
    println!("  Failed:   {}", failed);
    println!(
        "  Skipped:  {}",
        session.count_by_state(&WorkflowItemState::Skipped)
    );
    println!(
        "  Deferred: {}",
        session.count_by_state(&WorkflowItemState::Deferred)
    );
    println!("  State:    {}", session.state);

    Ok(())
}

/// Commit an applied item's artifact to session memory.
fn commit_item_to_session_memory(
    config: &GatewayConfig,
    session_id: Uuid,
    item_id: Uuid,
    draft_id: &str,
) {
    let mut memory = memory_store_from_config(&config.workspace_root);
    let key = format!("session/{}/applied/{}", session_id, item_id);
    let value = serde_json::json!({
        "session_id": session_id.to_string(),
        "item_id": item_id.to_string(),
        "draft_id": draft_id,
        "applied_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Err(e) = memory.store(
        &key,
        value,
        vec!["session".to_string(), "applied".to_string()],
        "ta-session-run",
    ) {
        tracing::warn!(
            session = %session_id,
            item = %item_id,
            error = %e,
            "Failed to commit applied item artifact to session memory"
        );
    }
}

/// `ta session projects` / `ta session list --workflow` — list workflow sessions.
fn list_workflow_sessions(config: &GatewayConfig, all: bool) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let mgr = WorkflowSessionManager::new(sessions_dir)?;

    let sessions = mgr.list()?;
    let sessions: Vec<_> = if all {
        sessions
    } else {
        sessions
            .into_iter()
            .filter(|s| s.state != WorkflowSessionState::Complete)
            .collect()
    };

    if sessions.is_empty() {
        if all {
            println!("No workflow sessions found.");
        } else {
            println!("No active workflow sessions. Use --all to see completed sessions.");
            println!("Start a session: ta session start <plan-id>");
        }
        return Ok(());
    }

    println!(
        "{:<10} {:<30} {:<12} {:<4} {:<4} {:<4} {:<4}",
        "ID", "PLAN", "STATE", "TOT", "DONE", "RUN", "FAIL"
    );
    println!("{}", "─".repeat(80));

    for s in &sessions {
        let total = s.items.len();
        let done = s.count_by_state(&WorkflowItemState::Complete);
        let running = s.count_by_state(&WorkflowItemState::Running)
            + s.count_by_state(&WorkflowItemState::AtGate);
        let failed = s.count_by_state(&WorkflowItemState::Failed);

        println!(
            "{:<10} {:<30} {:<12} {:<4} {:<4} {:<4} {:<4}",
            &s.session_id.to_string()[..8],
            truncate(&s.plan_title, 28),
            s.state.to_string(),
            total,
            done,
            running,
            failed,
        );
    }

    println!("\n{} session(s).", sessions.len());
    Ok(())
}

/// `ta session status <id> [--live]` — workflow session oversight dashboard.
fn workflow_session_status(config: &GatewayConfig, id: &str, live: bool) -> anyhow::Result<()> {
    let sessions_dir = config.workspace_root.join(".ta/sessions");
    let mgr = WorkflowSessionManager::new(sessions_dir)?;

    let render = |session: &WorkflowSession| {
        if live {
            // Clear screen for live refresh.
            print!("\x1b[2J\x1b[H");
        }
        println!("Workflow Session: {}", session.session_id);
        println!("  Plan:    {}", session.plan_title);
        println!("  State:   {}", session.state);
        println!("  Gate:    {}", session.gate_mode);
        println!(
            "  Updated: {}",
            session.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!();
        println!("  {:<3} {:<40} {:<10} DRAFT", "#", "TITLE", "STATE");
        println!("  {}", "─".repeat(68));
        for (i, item) in session.items.iter().enumerate() {
            let draft = item
                .draft_id
                .map(|d| d.to_string()[..8].to_string())
                .unwrap_or_default();
            println!(
                "  {:>3} {:<40} {:<10} {}",
                i + 1,
                truncate(&item.title, 38),
                item.state.to_string(),
                draft,
            );
        }
        println!();
        println!("  {}", session.status_summary());
    };

    let session_id = mgr.resolve_id(id)?;

    if live {
        loop {
            let session = mgr.load(session_id)?;
            let is_terminal = session.state == WorkflowSessionState::Complete;
            render(&session);
            if is_terminal {
                println!("\n  Session complete — auto-refresh stopped.");
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    } else {
        let session = mgr.load(session_id)?;
        render(&session);
    }

    Ok(())
}

/// Resolve a workflow session by ID or pick the most recent matching one.
///
/// If `id` is Some, resolves by exact UUID or prefix.
/// If `id` is None, returns the most recently updated session matching the predicate.
fn resolve_workflow_session(
    mgr: &WorkflowSessionManager,
    id: Option<&str>,
    predicate: impl Fn(&WorkflowSession) -> bool,
    description: &str,
) -> anyhow::Result<WorkflowSession> {
    if let Some(id) = id {
        let uuid = mgr.resolve_id(id).map_err(|_| {
            anyhow::anyhow!(
                "No workflow session found matching '{}'.\n\
                 Use `ta session projects` to list available sessions.",
                id
            )
        })?;
        mgr.load(uuid).map_err(|e| anyhow::anyhow!("{}", e))
    } else {
        let sessions = mgr.list()?;
        sessions.into_iter().find(|s| predicate(s)).ok_or_else(|| {
            anyhow::anyhow!(
                "No workflow session found {}.\n\
                     Use `ta session projects` to list sessions or \
                     specify a session ID.",
                description
            )
        })
    }
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
    use ta_changeset::InteractiveSession;
    use tempfile::TempDir;

    #[test]
    fn list_empty_sessions() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone()).unwrap();

        // Should not error on empty list.
        let sessions = store.list().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn show_session_by_prefix() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone()).unwrap();

        let session = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        let id_prefix = session.session_id.to_string()[..8].to_string();
        store.save(&session).unwrap();

        // Should find by prefix.
        let all = store.list().unwrap();
        let matches: Vec<_> = all
            .into_iter()
            .filter(|s| s.session_id.to_string().starts_with(&id_prefix))
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].session_id, session.session_id);
    }

    #[test]
    fn close_already_completed_session() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone()).unwrap();

        let mut session = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        session
            .transition(InteractiveSessionState::Completed)
            .unwrap();
        let prefix = session.session_id.to_string()[..8].to_string();
        store.save(&session).unwrap();

        // Closing an already completed session should succeed silently.
        let result = close_session(&config, &prefix, true);
        assert!(result.is_ok());
    }

    #[test]
    fn close_active_session() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone()).unwrap();

        let session = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        let session_id = session.session_id;
        let prefix = session_id.to_string()[..8].to_string();
        store.save(&session).unwrap();

        // Close with no_draft=true (skip draft build).
        let result = close_session(&config, &prefix, true);
        assert!(result.is_ok());

        // Session should now be completed.
        let loaded = store.load(session_id).unwrap();
        assert_eq!(loaded.state, InteractiveSessionState::Completed);
    }

    #[test]
    fn session_health_missing_workspace() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone()).unwrap();
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        let session = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        store.save(&session).unwrap();

        // No goal exists, so workspace should be considered missing.
        let health = check_session_health(&store, &goal_store, &session);
        assert!(matches!(health, SessionHealthStatus::WorkspaceMissing));
    }

    #[test]
    fn session_store_integrates_with_gateway_config() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());

        // The interactive_sessions_dir should be under .ta/
        assert!(config
            .interactive_sessions_dir
            .to_str()
            .unwrap()
            .contains(".ta"));

        // Store creation should work.
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone()).unwrap();

        let session = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "test-agent".to_string(),
        );
        store.save(&session).unwrap();

        let loaded = store.load(session.session_id).unwrap();
        assert_eq!(loaded.session_id, session.session_id);
    }

    // ── Workflow session tests ────────────────────────────────────────────────

    use ta_session::{
        GateMode as WfGateMode, PlanDocument, PlanItem, WorkflowItemState as WfItemState,
        WorkflowSession, WorkflowSessionManager, WorkflowSessionState as WfSessionState,
    };

    fn make_plan_with_items() -> PlanDocument {
        let mut plan = PlanDocument::new("Test Workflow Plan");
        let mut item = PlanItem::new("Implement feature A");
        item.acceptance_criteria.push("Feature A works".to_string());
        plan.add_item(item);
        plan.add_item(PlanItem::new("Write tests for A"));
        plan
    }

    #[test]
    fn start_session_unknown_plan_id_errors() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let result = start_session(&config, "invalid-not-a-uuid", "auto");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid plan ID"));
    }

    #[test]
    fn start_session_plan_not_in_memory_errors() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let uuid = Uuid::new_v4().to_string();
        let result = start_session(&config, &uuid, "auto");
        assert!(result.is_err());
        // Should report plan not found in memory.
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn start_session_invalid_gate_errors() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let uuid = Uuid::new_v4().to_string();
        let result = start_session(&config, &uuid, "badgate");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("gate"));
    }

    #[test]
    fn list_workflow_sessions_empty() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let result = list_workflow_sessions(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn list_workflow_sessions_shows_active_sessions() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let sessions_dir = temp.path().join(".ta/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mgr = WorkflowSessionManager::new(sessions_dir).unwrap();

        let plan = make_plan_with_items();
        let session = WorkflowSession::from_plan(&plan, WfGateMode::Auto);
        mgr.save(&session).unwrap();

        let result = list_workflow_sessions(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn list_workflow_sessions_all_includes_complete() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let sessions_dir = temp.path().join(".ta/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mgr = WorkflowSessionManager::new(sessions_dir).unwrap();

        let plan = make_plan_with_items();
        let mut session = WorkflowSession::from_plan(&plan, WfGateMode::Auto);
        // Mark all items terminal so session can be completed.
        for item in session.items.iter_mut() {
            item.state = WfItemState::Complete;
        }
        session.transition(WfSessionState::Running).unwrap();
        session.transition(WfSessionState::Complete).unwrap();
        mgr.save(&session).unwrap();

        // Without --all, complete session should be hidden.
        let result = list_workflow_sessions(&config, false);
        assert!(result.is_ok());

        // With --all, should be visible.
        let result = list_workflow_sessions(&config, true);
        assert!(result.is_ok());
    }

    #[test]
    fn workflow_session_status_nonexistent_errors() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let result = workflow_session_status(&config, "00000000", false);
        assert!(result.is_err());
    }

    #[test]
    fn workflow_session_status_existing_session() {
        let temp = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(temp.path());
        let sessions_dir = temp.path().join(".ta/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mgr = WorkflowSessionManager::new(sessions_dir).unwrap();

        let plan = make_plan_with_items();
        let session = WorkflowSession::from_plan(&plan, WfGateMode::Auto);
        let sid = session.session_id;
        mgr.save(&session).unwrap();

        let result = workflow_session_status(&config, &sid.to_string()[..8], false);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_workflow_session_by_prefix() {
        let temp = TempDir::new().unwrap();
        let sessions_dir = temp.path().join(".ta/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mgr = WorkflowSessionManager::new(sessions_dir).unwrap();

        let plan = make_plan_with_items();
        let session = WorkflowSession::from_plan(&plan, WfGateMode::Auto);
        let sid = session.session_id;
        mgr.save(&session).unwrap();

        let prefix = sid.to_string()[..8].to_string();
        let resolved = resolve_workflow_session(&mgr, Some(&prefix), |_| true, "any").unwrap();
        assert_eq!(resolved.session_id, sid);
    }

    #[test]
    fn resolve_workflow_session_by_predicate() {
        let temp = TempDir::new().unwrap();
        let sessions_dir = temp.path().join(".ta/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mgr = WorkflowSessionManager::new(sessions_dir).unwrap();

        let plan = make_plan_with_items();
        let session = WorkflowSession::from_plan(&plan, WfGateMode::Prompt);
        mgr.save(&session).unwrap();

        // Resolve without ID, using state predicate.
        let resolved = resolve_workflow_session(
            &mgr,
            None,
            |s| s.state == WfSessionState::Reviewing,
            "reviewing",
        )
        .unwrap();
        assert_eq!(resolved.plan_title, "Test Workflow Plan");
    }

    #[test]
    fn resolve_workflow_session_no_match_errors() {
        let temp = TempDir::new().unwrap();
        let sessions_dir = temp.path().join(".ta/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mgr = WorkflowSessionManager::new(sessions_dir).unwrap();

        let result = resolve_workflow_session(&mgr, None, |_| false, "impossible");
        assert!(result.is_err());
    }
}
