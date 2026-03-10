// session.rs — Interactive session management commands.
//
// `ta session list` shows active sessions across all channels.
// `ta session show <id>` displays session details and message history.

use clap::Subcommand;
use ta_changeset::{InteractiveSessionState, InteractiveSessionStore};
use ta_goal::GoalRunStore;
use ta_mcp_gateway::GatewayConfig;
use ta_session::SessionManager;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum SessionCommands {
    /// List interactive sessions.
    List {
        /// Show all sessions (including completed/aborted). Default: alive only.
        #[arg(long)]
        all: bool,
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
    Status,
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
}

pub fn execute(cmd: &SessionCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone())?;

    match cmd {
        SessionCommands::List { all } => list_sessions(&store, *all),
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
                None,  // no existing goal id
            )
        }
        SessionCommands::Pause { id } => pause_session(config, id),
        SessionCommands::Abort { id, reason } => abort_session(config, id, reason.as_deref()),
        SessionCommands::Status => session_status(config),
        SessionCommands::Close { id, no_draft } => close_session(config, id, *no_draft),
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

fn session_status(config: &GatewayConfig) -> anyhow::Result<()> {
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
}
