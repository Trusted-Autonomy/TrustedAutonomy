// run.rs — Orchestrate a full goal lifecycle: start → agent → PR build.
//
// `ta run "Fix the auth bug" --source .` is a convenience wrapper that:
// 1. Creates a goal with an overlay workspace
// 2. Injects context (e.g., CLAUDE.md) for the agent
// 3. Launches the agent with the goal as its initial prompt
// 4. When the agent exits, restores injected files and builds a PR package
//
// The user then reviews/approves/applies via `ta draft` commands.

use std::path::Path;

#[cfg(unix)]
use ta_changeset::{InteractionKind, InteractionRequest, InteractionResponse, Urgency};
use ta_changeset::{InteractiveSession, InteractiveSessionState, InteractiveSessionStore};
use ta_goal::GoalRunStore;
use ta_mcp_gateway::GatewayConfig;

use super::plan;
#[cfg(unix)]
use super::pty_capture;

// ── Per-agent launch configuration ──────────────────────────────

/// Agent launch descriptor.
/// Loaded from YAML config files (with hard-coded fallbacks for built-in agents).
///
/// Config search order:
///   1. `.ta/agents/<agent-id>.yaml`       (project override)
///   2. `~/.config/ta/agents/<agent-id>.yaml`  (user override)
///   3. `<binary-dir>/agents/<agent-id>.yaml`  (shipped defaults)
///   4. Hard-coded fallback
#[derive(serde::Deserialize, Clone, Debug)]
struct AgentLaunchConfig {
    /// The command to execute (e.g., "claude", "codex").
    command: String,
    /// Arguments to pass. `{prompt}` is replaced with the goal text.
    args_template: Vec<String>,
    /// Whether this agent reads CLAUDE.md for context injection.
    #[serde(default)]
    injects_context_file: bool,
    /// Whether to inject .claude/settings.local.json with TA permissions.
    #[serde(default)]
    injects_settings: bool,
    /// Optional command to run before the main agent launch (e.g., init).
    #[serde(default)]
    pre_launch: Option<PreLaunchConfig>,
    /// Environment variables to set for the agent process.
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    /// Shell to use for command execution: "bash", "powershell", "cmd".
    /// Auto-detected based on platform if not specified (v0.9.1).
    #[serde(default)]
    #[allow(dead_code)]
    shell: Option<String>,
    /// Human-readable name (informational only, used by `ta agent list` in future).
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    /// Description of the agent (informational only, used by `ta agent list` in future).
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    /// Interactive session configuration (v0.3.1.2).
    #[serde(default)]
    #[allow(dead_code)]
    interactive: Option<ta_changeset::InteractiveConfig>,
    /// Agent alignment profile (v0.4.0).
    /// Compiled into CapabilityManifest grants by the Policy Compiler.
    /// Read via YAML deserialization; will be used by gateway during goal start.
    #[serde(default)]
    #[allow(dead_code)]
    alignment: Option<ta_policy::AlignmentProfile>,
}

/// Pre-launch command configuration (deserialized from YAML).
#[derive(serde::Deserialize, Clone, Debug)]
struct PreLaunchConfig {
    command: String,
    args: Vec<String>,
}

/// Try to load agent config from YAML files, falling back to hard-coded defaults.
fn agent_launch_config(agent_id: &str, source_dir: Option<&Path>) -> AgentLaunchConfig {
    // Try YAML configs in priority order.
    if let Some(config) = load_agent_yaml(agent_id, source_dir) {
        return config;
    }
    // Fall back to built-in defaults.
    builtin_agent_config(agent_id)
}

/// Search for an agent YAML config in standard locations.
fn load_agent_yaml(agent_id: &str, source_dir: Option<&Path>) -> Option<AgentLaunchConfig> {
    let filename = format!("{}.yaml", agent_id);

    // 1. Project override: .ta/agents/<agent-id>.yaml
    if let Some(source) = source_dir {
        let project_path = source.join(".ta").join("agents").join(&filename);
        if let Some(config) = try_load_yaml(&project_path) {
            return Some(config);
        }
    }

    // 2. User override: ~/.config/ta/agents/<agent-id>.yaml
    if let Some(home) = dirs_path() {
        let user_path = home
            .join(".config")
            .join("ta")
            .join("agents")
            .join(&filename);
        if let Some(config) = try_load_yaml(&user_path) {
            return Some(config);
        }
    }

    // 3. Shipped defaults: <binary-dir>/agents/<agent-id>.yaml
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let shipped_path = bin_dir.join("agents").join(&filename);
            if let Some(config) = try_load_yaml(&shipped_path) {
                return Some(config);
            }
        }
    }

    None
}

/// Attempt to read and parse a single YAML config file.
fn try_load_yaml(path: &Path) -> Option<AgentLaunchConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&content).ok()
}

/// Get home directory (cross-platform).
fn dirs_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// Hard-coded built-in agent configs (fallback when no YAML file is found).
fn builtin_agent_config(agent_id: &str) -> AgentLaunchConfig {
    match agent_id {
        "claude-code" => AgentLaunchConfig {
            command: "claude".to_string(),
            args_template: vec!["{prompt}".to_string()],
            injects_context_file: true,
            injects_settings: true,
            pre_launch: None,
            env: Default::default(),
            shell: None,
            name: Some("claude-code".to_string()),
            description: Some("Anthropic's Claude Code CLI".to_string()),
            interactive: None,
            alignment: Some(ta_policy::AlignmentProfile::default_developer()),
        },
        "codex" => AgentLaunchConfig {
            command: "codex".to_string(),
            args_template: vec![
                "--approval-mode".to_string(),
                "full-auto".to_string(),
                "{prompt}".to_string(),
            ],
            injects_context_file: false,
            injects_settings: false,
            pre_launch: None,
            env: Default::default(),
            shell: None,
            name: Some("codex".to_string()),
            description: Some("OpenAI's Codex CLI".to_string()),
            interactive: None,
            alignment: Some(ta_policy::AlignmentProfile::default_developer()),
        },
        "claude-flow" => AgentLaunchConfig {
            command: "npx".to_string(),
            args_template: vec![
                "claude-flow@alpha".to_string(),
                "hive-mind".to_string(),
                "spawn".to_string(),
                "{prompt}".to_string(),
                "--claude".to_string(),
            ],
            injects_context_file: true,
            injects_settings: true,
            pre_launch: Some(PreLaunchConfig {
                command: "npx".to_string(),
                args: vec![
                    "claude-flow@alpha".to_string(),
                    "hive-mind".to_string(),
                    "init".to_string(),
                ],
            }),
            env: Default::default(),
            shell: None,
            name: Some("claude-flow".to_string()),
            description: Some("Claude Flow multi-agent orchestration".to_string()),
            interactive: None,
            alignment: Some(ta_policy::AlignmentProfile::default_developer()),
        },
        _ => AgentLaunchConfig {
            command: agent_id.to_string(),
            args_template: vec![],
            injects_context_file: false,
            injects_settings: false,
            pre_launch: None,
            env: Default::default(),
            shell: None,
            name: None,
            description: None,
            interactive: None,
            alignment: None,
        },
    }
}

// ── Public API ──────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn execute(
    config: &GatewayConfig,
    title: Option<&str>,
    agent: &str,
    source: Option<&Path>,
    objective: &str,
    phase: Option<&str>,
    follow_up: Option<&Option<String>>,
    objective_file: Option<&Path>,
    no_launch: bool,
    interactive: bool,
    macro_goal: bool,
    resume: Option<&str>,
    headless: bool,
) -> anyhow::Result<()> {
    // ── Resume an existing session ──────────────────────────────
    if let Some(session_id_prefix) = resume {
        #[cfg(unix)]
        {
            return execute_resume(config, session_id_prefix, agent);
        }
        #[cfg(not(unix))]
        {
            let _ = (session_id_prefix, agent);
            anyhow::bail!("Session resume requires PTY support (not available on Windows)");
        }
    }

    let title = title.ok_or_else(|| {
        anyhow::anyhow!("Title is required (use --resume to resume an existing session)")
    })?;

    let agent_config = agent_launch_config(agent, source);

    // 1. Start the goal (creates overlay workspace).
    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    super::goal::execute(
        &super::goal::GoalCommands::Start {
            title: title.to_string(),
            source: source.map(|p| p.to_path_buf()),
            objective: objective.to_string(),
            agent: agent.to_string(),
            phase: phase.map(|p| p.to_string()),
            follow_up: follow_up.cloned(),
            objective_file: objective_file.map(|p| p.to_path_buf()),
        },
        config,
    )?;

    // Get the goal we just created (most recent).
    let goals = goal_store.list()?;
    let goal = goals
        .first()
        .ok_or_else(|| anyhow::anyhow!("Failed to find created goal"))?;

    // Mark as macro goal if --macro was specified.
    if macro_goal {
        let mut updated_goal = goal.clone();
        updated_goal.is_macro = true;
        goal_store.save(&updated_goal)?;
    }

    let goal_id = goal.goal_run_id.to_string();
    let staging_path = goal.workspace_path.clone();

    // 2. Inject context and settings into the staging workspace.
    if agent_config.injects_context_file {
        inject_claude_md(
            &staging_path,
            title,
            &goal_id,
            goal.plan_phase.as_deref(),
            goal.source_dir.as_deref(),
            goal.parent_goal_id,
            &goal_store,
            config,
            macro_goal,
        )?;
    }
    if agent_config.injects_settings {
        inject_claude_settings(&staging_path, source)?;
    }

    // Inject TA MCP server config into .mcp.json for macro goals (#60).
    // Without this, the agent sees MCP tool documentation in CLAUDE.md but
    // can't actually call the tools because no MCP server is configured.
    if macro_goal {
        inject_mcp_server_config(&staging_path)?;
    }

    // Build the prompt string.
    let prompt = if objective.is_empty() {
        format!("Implement: {}", title)
    } else {
        format!("{}\n\nObjective: {}", title, objective)
    };

    if no_launch {
        // Restore injected files — user will run the agent manually,
        // so injected context stays only if they re-run `ta run`.
        if agent_config.injects_context_file {
            restore_claude_md(&staging_path)?;
        }
        if agent_config.injects_settings {
            restore_claude_settings(&staging_path)?;
        }
        if macro_goal {
            restore_mcp_server_config(&staging_path)?;
        }

        println!("\nWorkspace ready. To use manually:");
        println!("  cd {}", staging_path.display());
        if let Some(ref pre) = agent_config.pre_launch {
            println!(
                "  {} {}  # required init step",
                pre.command,
                pre.args.join(" ")
            );
        }
        println!("  {} {}", agent_config.command, shell_quote(&prompt));
        println!();
        println!("When done, build the draft:");
        println!("  ta draft build {}", goal_id);
        println!("  # Or: ta draft build --latest");
        return Ok(());
    }

    // 3. Run pre-launch command if needed (e.g., hive-mind init).
    if let Some(ref pre) = agent_config.pre_launch {
        println!(
            "\nRunning pre-launch: {} {}...",
            pre.command,
            pre.args.join(" ")
        );
        let pre_status = std::process::Command::new(&pre.command)
            .args(&pre.args)
            .current_dir(&staging_path)
            .status();
        match pre_status {
            Ok(exit) if exit.success() => {}
            Ok(exit) => {
                return Err(anyhow::anyhow!(
                    "Pre-launch command exited with status {}",
                    exit
                ));
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to run pre-launch command '{}': {}",
                    pre.command,
                    e
                ));
            }
        }
    }

    // 4. Create interactive session if --interactive.
    let mut session_store = if interactive {
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone())?;
        let channel_id = format!("cli:{}", std::process::id());
        let session = InteractiveSession::new(goal.goal_run_id, channel_id, agent.to_string());
        store.save(&session)?;
        println!("\nInteractive session: {}", session.session_id);
        println!("  Channel: {}", session.channel_id);
        Some((store, session))
    } else {
        None
    };

    // 5. Launch the agent in the staging directory.
    println!(
        "\nLaunching {} in staging workspace...",
        agent_config.command
    );
    println!("  Working dir: {}", staging_path.display());
    if headless {
        println!("  Mode: headless (non-interactive, piped output)");
    } else if interactive {
        println!("  Mode: interactive (PTY capture + session orchestration)");
    }
    if macro_goal {
        println!("  Mode: macro goal (inner-loop iteration enabled)");
    }
    println!();

    // Choose launch mode: headless (piped), PTY-interactive, or simple.
    // Type alias for the guidance log — on Unix this contains captured human inputs
    // from PTY sessions; on Windows the Vec is always empty.
    type GuidanceLog = Vec<(String, String)>;
    let launch_result: std::io::Result<(std::process::ExitStatus, GuidanceLog)> = if headless {
        launch_agent_headless(&agent_config, &staging_path, &prompt).map(|exit| (exit, Vec::new()))
    } else if interactive {
        #[cfg(unix)]
        {
            launch_agent_interactive(&agent_config, &staging_path, &prompt, &mut session_store).map(
                |(exit, log)| {
                    (
                        exit,
                        log.iter()
                            .map(|(req, resp)| (format!("{}", req), format!("{}", resp)))
                            .collect(),
                    )
                },
            )
        }
        #[cfg(not(unix))]
        {
            eprintln!("Warning: interactive PTY mode is not available on Windows. Falling back to simple mode.");
            launch_agent(&agent_config, &staging_path, &prompt).map(|exit| (exit, Vec::new()))
        }
    } else {
        launch_agent(&agent_config, &staging_path, &prompt).map(|exit| (exit, Vec::new()))
    };

    match launch_result {
        Ok((exit, guidance_log)) => {
            if exit.success() {
                println!("\nAgent exited. Building draft...");
            } else {
                println!(
                    "\nAgent exited with status {}. Building draft anyway...",
                    exit
                );
            }

            // Log guidance interactions to session if any.
            if let Some((ref store, ref mut session)) = session_store {
                for (req_str, resp_str) in &guidance_log {
                    session.log_message(
                        "ta-system",
                        &format!("Guidance: {} → {}", req_str, resp_str),
                    );
                }
                store.save(session)?;
            }
        }
        Err(e) => {
            // Mark interactive session as aborted on launch failure.
            if let Some((ref store, ref mut session)) = session_store {
                session.log_message("ta-system", &format!("Agent launch failed: {}", e));
                let _ = session.transition(InteractiveSessionState::Aborted);
                let _ = store.save(session);
            }

            if e.kind() == std::io::ErrorKind::NotFound {
                // Restore injected files before returning — agent won't run.
                if agent_config.injects_context_file {
                    restore_claude_md(&staging_path)?;
                }
                if agent_config.injects_settings {
                    restore_claude_settings(&staging_path)?;
                }
                if macro_goal {
                    restore_mcp_server_config(&staging_path)?;
                }

                println!(
                    "\n'{}' command not found. To use manually:",
                    agent_config.command
                );
                println!("  cd {}", staging_path.display());
                println!("  {} {}", agent_config.command, shell_quote(&prompt));
                println!();
                println!("When done, build the draft:");
                println!("  ta draft build {}", goal_id);
                return Ok(());
            }
            return Err(anyhow::anyhow!(
                "Failed to launch {}: {}",
                agent_config.command,
                e
            ));
        }
    }

    // 6. Restore injected files before diffing (removes TA injection).
    if agent_config.injects_context_file {
        restore_claude_md(&staging_path)?;
    }
    if agent_config.injects_settings {
        restore_claude_settings(&staging_path)?;
    }
    if macro_goal {
        restore_mcp_server_config(&staging_path)?;
    }

    // 7. Build draft package from the diff.
    //    In macro sessions, the agent may have already submitted/applied drafts
    //    via MCP tools, transitioning the goal out of Running state. Only build
    //    a draft if the goal is still running.
    let goal_current = goal_store
        .get(goal.goal_run_id)?
        .unwrap_or_else(|| goal.clone());
    let draft_built = if matches!(goal_current.state, ta_goal::GoalRunState::Running) {
        super::draft::execute(
            &super::draft::DraftCommands::Build {
                goal_id: goal_id.clone(),
                summary: format!("Changes from goal: {}", title),
                latest: false,
            },
            config,
        )?;
        true
    } else {
        println!(
            "\nGoal is already in {} state — skipping automatic draft build.",
            goal_current.state
        );
        println!("(Drafts were submitted during the macro session.)");
        false
    };

    // 7b. Auto-capture goal completion into memory (v0.5.6).
    if draft_built {
        auto_capture_goal_completion(config, goal, &staging_path);
    }

    // 8. Mark interactive session as completed.
    if let Some((store, mut session)) = session_store {
        if draft_built {
            session.log_message("ta-system", "Agent exited, draft built");
        } else {
            session.log_message(
                "ta-system",
                &format!("Agent exited, goal already {}", goal_current.state),
            );
        }
        let _ = session.transition(InteractiveSessionState::Completed);
        store.save(&session)?;
    }

    // In headless mode, output structured JSON for orchestrator consumption.
    if headless {
        let draft_id = if draft_built {
            // Find the most recent draft for this goal.
            find_latest_draft_id(config, &goal_id)
        } else {
            None
        };

        let output = serde_json::json!({
            "goal_id": goal_id,
            "draft_built": draft_built,
            "draft_id": draft_id,
            "state": goal_current.state.to_string(),
        });
        println!("\n__TA_HEADLESS_RESULT__:{}", output);
    } else {
        if draft_built {
            println!("\nNext steps:");
            println!("  ta draft list");
            println!("  ta draft view <draft-id>");
            println!("  ta draft approve <draft-id>");
            println!("  ta draft apply <draft-id> --git-commit");
        } else {
            println!("\nNext steps:");
            println!("  ta draft list      — view submitted drafts");
            println!("  ta goal status     — check goal state");
        }
        if interactive {
            println!("  ta session list");
            println!("  ta session show <session-id>");
        }
    }

    Ok(())
}

#[cfg(unix)]
/// Resume an existing interactive session by re-launching the agent in its workspace.
fn execute_resume(
    config: &GatewayConfig,
    session_id_prefix: &str,
    agent: &str,
) -> anyhow::Result<()> {
    let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone())?;
    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    // Find the session by ID or prefix.
    let mut session = find_session_by_prefix(&store, session_id_prefix)?;

    // Validate session state — must be paused or active to resume.
    if !session.is_alive() {
        anyhow::bail!(
            "Session {} is {} — cannot resume",
            session.session_id,
            session.state
        );
    }

    // Transition from Paused → Active if needed.
    if session.state == InteractiveSessionState::Paused {
        session.transition(InteractiveSessionState::Active)?;
    }

    // Look up the goal to find the staging workspace.
    let goal = goal_store
        .list()?
        .into_iter()
        .find(|g| g.goal_run_id == session.goal_id)
        .ok_or_else(|| anyhow::anyhow!("Goal {} not found for session", session.goal_id))?;

    let staging_path = &goal.workspace_path;
    if !staging_path.exists() {
        // v0.7.5: PTY health check — workspace is gone.
        // Offer to close the session cleanly instead of erroring.
        eprintln!(
            "Staging workspace no longer exists: {}",
            staging_path.display()
        );
        eprintln!("The child process appears to have exited or the workspace was cleaned up.");
        eprintln!();
        eprintln!("Options:");
        eprintln!(
            "  ta session close {}  — close the session cleanly",
            &session.session_id.to_string()[..8]
        );
        eprintln!(
            "  ta session abort {}  — abort and discard",
            &session.session_id.to_string()[..8]
        );
        anyhow::bail!(
            "Staging workspace no longer exists: {}",
            staging_path.display()
        );
    }

    // v0.7.5: PTY health check — verify workspace health before reattaching.
    let health = super::session::check_session_health(&store, &goal_store, &session);
    match health {
        super::session::SessionHealthStatus::WorkspaceMissing => {
            eprintln!("Warning: staging workspace health check failed.");
            eprintln!("Options:");
            eprintln!(
                "  ta session close {}  — build a draft and close",
                &session.session_id.to_string()[..8]
            );
            eprintln!(
                "  ta session abort {}  — abort the session",
                &session.session_id.to_string()[..8]
            );
            anyhow::bail!("Session workspace is not healthy — cannot resume");
        }
        super::session::SessionHealthStatus::Healthy {
            has_staging_changes,
        } => {
            if has_staging_changes {
                println!("Note: staging workspace has uncommitted changes from a previous run.");
            }
        }
    }

    let agent_config = agent_launch_config(agent, goal.source_dir.as_deref());

    // Build resume command: use agent config's resume_cmd if available,
    // otherwise use the standard launch command.
    let resume_cmd = agent_config
        .interactive
        .as_ref()
        .and_then(|ic| ic.resume_cmd.as_deref())
        .map(|cmd| cmd.replace("{session_id}", &session.session_id.to_string()));

    // Update channel ID for this terminal.
    session.channel_id = format!("cli:{}", std::process::id());
    session.log_message("ta-system", "Session resumed");
    store.save(&session)?;

    println!("\nResuming session: {}", session.session_id);
    println!("  Goal: {}", session.goal_id);
    println!("  Agent: {}", session.agent_id);
    println!("  Workspace: {}", staging_path.display());
    println!("  Mode: interactive (PTY capture)");
    println!();

    let mut session_store = Some((store, session));

    let launch_result = if let Some(ref cmd_str) = resume_cmd {
        // Use the resume command from agent config.
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        let (cmd, args) = parts
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("Empty resume command"))?;

        let resume_config = AgentLaunchConfig {
            command: cmd.to_string(),
            args_template: args.iter().map(|a| a.to_string()).collect(),
            injects_context_file: false,
            injects_settings: false,
            pre_launch: None,
            env: agent_config.env.clone(),
            shell: None,
            name: None,
            description: None,
            interactive: None,
            alignment: None,
        };

        launch_agent_interactive(&resume_config, staging_path, "", &mut session_store)
    } else {
        // Re-launch with the original prompt (empty for resume).
        launch_agent_interactive(&agent_config, staging_path, "", &mut session_store)
    };

    match launch_result {
        Ok((exit, _guidance_log)) => {
            if exit.success() {
                println!("\nAgent exited successfully.");
            } else {
                println!("\nAgent exited with status {}.", exit);
            }
        }
        Err(e) => {
            if let Some((ref store, ref mut session)) = session_store {
                session.log_message("ta-system", &format!("Resume launch failed: {}", e));
                let _ = session.transition(InteractiveSessionState::Aborted);
                let _ = store.save(session);
            }
            return Err(anyhow::anyhow!("Failed to resume agent: {}", e));
        }
    }

    // Mark session as paused (can be resumed again) or completed.
    if let Some((store, mut session)) = session_store {
        session.log_message("ta-system", "Agent exited from resumed session");
        // Transition to Paused so it can be resumed again, not Completed.
        let _ = session.transition(InteractiveSessionState::Paused);
        store.save(&session)?;
        println!("\nSession paused. To resume again:");
        println!("  ta run --resume {}", &session.session_id.to_string()[..8]);
    }

    Ok(())
}

#[cfg(unix)]
/// Find a session by full UUID or prefix match.
fn find_session_by_prefix(
    store: &InteractiveSessionStore,
    prefix: &str,
) -> anyhow::Result<InteractiveSession> {
    // Try exact UUID parse first.
    if let Ok(uuid) = uuid::Uuid::parse_str(prefix) {
        return Ok(store.load(uuid)?);
    }

    // Prefix match.
    let all = store.list()?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|s| s.session_id.to_string().starts_with(prefix))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("No session found matching '{}'", prefix),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} sessions. Use a longer prefix.",
            prefix,
            n
        ),
    }
}

// ── Agent launch ────────────────────────────────────────────────

/// Launch an agent process with template-substituted arguments.
fn launch_agent(
    config: &AgentLaunchConfig,
    staging_path: &Path,
    prompt: &str,
) -> std::io::Result<std::process::ExitStatus> {
    let mut cmd = std::process::Command::new(&config.command);
    cmd.current_dir(staging_path);

    for arg_template in &config.args_template {
        let arg = arg_template.replace("{prompt}", prompt);
        cmd.arg(arg);
    }

    // Set agent-specific environment variables from config.
    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    cmd.status()
}

/// Launch an agent in headless (non-interactive) mode.
///
/// Stdout/stderr are piped and streamed to the parent process, but no PTY is allocated.
/// Suitable for orchestrator-driven goals where no human interaction is expected.
fn launch_agent_headless(
    config: &AgentLaunchConfig,
    staging_path: &Path,
    prompt: &str,
) -> std::io::Result<std::process::ExitStatus> {
    use std::io::{BufRead, BufReader};

    let mut cmd = std::process::Command::new(&config.command);
    cmd.current_dir(staging_path);

    for arg_template in &config.args_template {
        let arg = arg_template.replace("{prompt}", prompt);
        cmd.arg(arg);
    }

    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    // Pipe stdout so we can stream it without a PTY.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::inherit());
    // No stdin — headless mode.
    cmd.stdin(std::process::Stdio::null());

    let mut child = cmd.spawn()?;

    // Stream stdout lines to the parent's stdout with a [agent] prefix.
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            println!("[agent] {}", line);
        }
    }

    child.wait()
}

#[cfg(unix)]
/// Launch an agent in interactive PTY mode with stdin interleaving and guidance logging.
///
/// Returns the exit status and a log of (InteractionRequest, InteractionResponse) pairs
/// for every human guidance message injected during the session.
fn launch_agent_interactive(
    config: &AgentLaunchConfig,
    staging_path: &Path,
    prompt: &str,
    session_store: &mut Option<(InteractiveSessionStore, InteractiveSession)>,
) -> std::io::Result<(
    std::process::ExitStatus,
    Vec<(InteractionRequest, InteractionResponse)>,
)> {
    // Build args with template substitution.
    let args: Vec<String> = config
        .args_template
        .iter()
        .map(|t| t.replace("{prompt}", prompt))
        .collect();

    // Launch via PTY.
    let pty_config = pty_capture::PtyLaunchConfig {
        command: &config.command,
        args,
        working_dir: staging_path,
        env_vars: &config.env,
        output_sink: None, // Default: TerminalSink (stdout). Replace for Slack/email.
    };

    let result = pty_capture::run_interactive_pty(pty_config)?;

    // Convert captured human inputs into InteractionRequest/Response pairs for audit.
    let mut guidance_log = Vec::new();
    for input in &result.human_inputs {
        let request = InteractionRequest::new(
            InteractionKind::Custom("guidance".to_string()),
            serde_json::json!({
                "text": input.text,
                "timestamp": input.timestamp.to_rfc3339(),
            }),
            Urgency::Advisory,
        );

        let response =
            InteractionResponse::new(request.interaction_id, ta_changeset::Decision::Approve)
                .with_reasoning(&input.text)
                .with_responder("cli:stdin");

        // Log to interactive session.
        if let Some((ref store, ref mut session)) = session_store {
            session.log_message("human", &input.text);
            let _ = store.save(session);
        }

        guidance_log.push((request, response));
    }

    // Log captured agent output to session (summary only, not full output).
    if let Some((ref store, ref mut session)) = session_store {
        let total_bytes: usize = result.captured_output.iter().map(|c| c.data.len()).sum();
        session.log_message(
            "ta-system",
            &format!(
                "PTY session captured {} bytes of agent output, {} human inputs",
                total_bytes,
                result.human_inputs.len()
            ),
        );
        let _ = store.save(session);
    }

    Ok((result.exit_status, guidance_log))
}

/// Find the most recent draft ID for a goal (headless output).
fn find_latest_draft_id(config: &GatewayConfig, goal_id: &str) -> Option<String> {
    use ta_changeset::draft_package::DraftPackage;

    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return None;
    }

    let mut drafts: Vec<DraftPackage> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let json = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str::<DraftPackage>(&json).ok()
        })
        .filter(|pkg| pkg.goal.goal_id == goal_id)
        .collect();

    drafts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    drafts.first().map(|d| d.package_id.to_string())
}

/// Simple shell quoting for display purposes.
fn shell_quote(s: &str) -> String {
    if s.contains(' ') || s.contains('\n') {
        format!("\"{}\"", s.replace('\"', "\\\""))
    } else {
        s.to_string()
    }
}

// ── Claude Code settings injection ──────────────────────────────
//
// Instead of --dangerously-skip-permissions, TA injects a
// .claude/settings.local.json that allows all standard tools but
// denies patterns from the forbidden-tools list. This makes the
// agent work uninterrupted in the staging sandbox while keeping
// community-maintained safety rails.

const SETTINGS_BACKUP: &str = ".ta/claude_settings_original";
const SETTINGS_REL_PATH: &str = ".claude/settings.local.json";
const FORBIDDEN_TOOLS_FILE: &str = ".ta-forbidden-tools";

/// Tools to allow in the injected Claude Code settings.
const DEFAULT_ALLOWED_TOOLS: &[&str] = &[
    "Bash(*)",
    "Read(*)",
    "Write(*)",
    "Edit(*)",
    "MultiEdit(*)",
    "Glob(*)",
    "Grep(*)",
    "WebFetch(*)",
    "WebSearch(*)",
    "NotebookEdit(*)",
    "Task(*)",
    "Skill(*)",
    "TodoRead(*)",
    "TodoWrite(*)",
];

/// Built-in forbidden tool patterns — community-maintained deny list.
/// These are always denied even in TA staging workspaces.
/// Add patterns here as the community identifies dangerous tools/commands.
const DEFAULT_FORBIDDEN_TOOLS: &[&str] = &[];

/// Load forbidden tool patterns from the project's `.ta-forbidden-tools` file.
/// Returns an empty vec if the file doesn't exist.
fn load_forbidden_tools(source_dir: Option<&Path>) -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_FORBIDDEN_TOOLS
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    if let Some(source) = source_dir {
        let path = source.join(FORBIDDEN_TOOLS_FILE);
        if path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                for line in contents.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('#') {
                        patterns.push(trimmed.to_string());
                    }
                }
            }
        }
    }

    patterns
}

/// Inject .claude/settings.local.json with TA permissions.
/// Allows all standard tools, denies forbidden patterns.
fn inject_claude_settings(staging_path: &Path, source_dir: Option<&Path>) -> anyhow::Result<()> {
    let settings_path = staging_path.join(SETTINGS_REL_PATH);

    // Read and save original content.
    let original_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)?
    } else {
        NO_ORIGINAL_SENTINEL.to_string()
    };

    // Save backup.
    let backup_dir = staging_path.join(".ta");
    std::fs::create_dir_all(&backup_dir)?;
    std::fs::write(staging_path.join(SETTINGS_BACKUP), &original_content)?;

    // Build allow and deny lists.
    let allow: Vec<String> = DEFAULT_ALLOWED_TOOLS
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect();
    let forbidden = load_forbidden_tools(source_dir);
    let deny: Vec<String> = forbidden.iter().map(|s| format!("\"{}\"", s)).collect();

    let settings_json = format!(
        r#"{{
  "_comment": "Injected by Trusted Autonomy. Agent works in a staging sandbox — all changes require human review before applying. See .ta-forbidden-tools to deny specific patterns.",
  "permissions": {{
    "allow": [
      {}
    ],
    "deny": [
      {}
    ]
  }}
}}"#,
        allow.join(",\n      "),
        deny.join(",\n      ")
    );

    // Ensure .claude/ directory exists.
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&settings_path, settings_json)?;
    Ok(())
}

/// Restore original .claude/settings.local.json before diffing.
fn restore_claude_settings(staging_path: &Path) -> anyhow::Result<()> {
    let backup_path = staging_path.join(SETTINGS_BACKUP);
    let settings_path = staging_path.join(SETTINGS_REL_PATH);

    if !backup_path.exists() {
        return Ok(());
    }

    let original = std::fs::read_to_string(&backup_path)?;

    if original == NO_ORIGINAL_SENTINEL {
        // Settings file didn't exist originally — remove it and the .claude/ dir if empty.
        if settings_path.exists() {
            std::fs::remove_file(&settings_path)?;
        }
        if let Some(parent) = settings_path.parent() {
            // Only remove .claude/ if it's empty (don't delete user's other configs).
            let _ = std::fs::remove_dir(parent);
        }
    } else {
        std::fs::write(&settings_path, original)?;
    }

    Ok(())
}

// ── .mcp.json injection for macro goals (#60) ──────────────────

const MCP_JSON_PATH: &str = ".mcp.json";
const MCP_JSON_BACKUP: &str = ".ta/mcp_json_original";

/// Inject TA MCP server config into a directory's `.mcp.json`.
///
/// This allows agents to call TA's MCP tools (ta_plan, ta_goal,
/// ta_draft, ta_context). Without this, the agent sees tool documentation
/// in CLAUDE.md but has no MCP server configured to handle the calls.
///
/// Used by both `ta run --macro` (staging workspace) and `ta dev` (project root).
pub(crate) fn inject_mcp_server_config(staging_path: &Path) -> anyhow::Result<()> {
    let mcp_json_path = staging_path.join(MCP_JSON_PATH);
    let backup_path = staging_path.join(MCP_JSON_BACKUP);

    // Save original content (or sentinel if file doesn't exist).
    let original_content = if mcp_json_path.exists() {
        std::fs::read_to_string(&mcp_json_path)?
    } else {
        NO_ORIGINAL_SENTINEL.to_string()
    };

    let backup_dir = staging_path.join(".ta");
    std::fs::create_dir_all(&backup_dir)?;
    std::fs::write(&backup_path, &original_content)?;

    // Build the MCP config with TA server entry.
    // Resolve the `ta` binary path for the server command.
    let ta_binary = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "ta".to_string());

    let ta_server_entry = serde_json::json!({
        "command": ta_binary,
        "args": ["serve"],
        "env": {
            "TA_PROJECT_ROOT": staging_path.display().to_string()
        }
    });

    // Merge with existing .mcp.json if present.
    let mut mcp_config: serde_json::Value = if original_content != NO_ORIGINAL_SENTINEL {
        serde_json::from_str(&original_content)
            .unwrap_or_else(|_| serde_json::json!({ "mcpServers": {} }))
    } else {
        serde_json::json!({ "mcpServers": {} })
    };

    // Add or update the "trusted-autonomy" server entry.
    if let Some(servers) = mcp_config
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
    {
        servers.insert("ta".to_string(), ta_server_entry);
    } else {
        mcp_config["mcpServers"] = serde_json::json!({
            "ta": ta_server_entry
        });
    }

    std::fs::write(&mcp_json_path, serde_json::to_string_pretty(&mcp_config)?)?;
    Ok(())
}

/// Restore the original `.mcp.json` after agent exits.
///
/// Used by both `ta run --macro` (before diff) and `ta dev` (cleanup).
pub(crate) fn restore_mcp_server_config(staging_path: &Path) -> anyhow::Result<()> {
    let mcp_json_path = staging_path.join(MCP_JSON_PATH);
    let backup_path = staging_path.join(MCP_JSON_BACKUP);

    if !backup_path.exists() {
        return Ok(());
    }

    let original = std::fs::read_to_string(&backup_path)?;

    if original == NO_ORIGINAL_SENTINEL {
        if mcp_json_path.exists() {
            std::fs::remove_file(&mcp_json_path)?;
        }
    } else {
        std::fs::write(&mcp_json_path, original)?;
    }

    std::fs::remove_file(&backup_path)?;
    Ok(())
}

// ── CLAUDE.md injection and restoration ─────────────────────────

const CLAUDE_MD_BACKUP: &str = ".ta/claude_md_original";
const NO_ORIGINAL_SENTINEL: &str = "__TA_NO_ORIGINAL__";

/// Build a plan context section for CLAUDE.md injection.
/// Returns empty string if no PLAN.md or no phase specified.
fn build_plan_section(plan_phase: Option<&str>, source_dir: Option<&Path>) -> String {
    let source = match source_dir {
        Some(s) => s,
        None => return String::new(),
    };

    let phases = match plan::load_plan(source) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };

    if phases.is_empty() {
        return String::new();
    }

    let checklist = plan::format_plan_checklist(&phases, plan_phase);

    let current_line = if let Some(phase_id) = plan_phase {
        if let Some(phase) = phases.iter().find(|p| p.id == phase_id) {
            format!(
                "\n**You are working on Phase {} — {}.**\n",
                phase.id, phase.title
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    format!(
        "\n## Plan Context\n{}\nPlan progress:\n{}\n",
        current_line, checklist
    )
}

/// Build a parent goal context section for CLAUDE.md injection.
/// Returns empty string if no parent goal or if parent's PR is not available.
fn build_parent_context_section(
    parent_goal_id: Option<uuid::Uuid>,
    goal_store: &ta_goal::GoalRunStore,
    config: &GatewayConfig,
) -> String {
    let parent_id = match parent_goal_id {
        Some(id) => id,
        None => return String::new(),
    };

    let parent_goal = match goal_store.get(parent_id) {
        Ok(Some(g)) => g,
        _ => return String::new(),
    };

    let mut context = format!(
        "\n## Follow-Up Context\n\nThis is a follow-up goal building on:\n\
         **Parent Goal:** {} ({})\n\
         **Parent Objective:** {}\n",
        parent_goal.title, parent_id, parent_goal.objective
    );

    // If parent has a PR, include artifact dispositions and discuss items.
    if let Some(pr_id) = parent_goal.pr_package_id {
        use crate::commands::draft::load_package;
        if let Ok(parent_pr) = load_package(config, pr_id) {
            let approved = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Approved
                    )
                })
                .count();
            let rejected = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Rejected
                    )
                })
                .count();
            let discuss = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Discuss
                    )
                })
                .count();

            context.push_str(&format!(
                "\n**Parent PR Status:** {} ({} approved, {} rejected, {} discuss)\n",
                parent_pr.status, approved, rejected, discuss
            ));

            // List discuss items with their rationale.
            let discuss_items: Vec<_> = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Discuss
                    )
                })
                .collect();

            if !discuss_items.is_empty() {
                context.push_str("\n### Items for Discussion:\n\n");
                context
                    .push_str("The following artifacts were marked for discussion during review. ");
                context.push_str(
                    "Please address the reviewer's concerns in this follow-up iteration.\n\n",
                );

                for artifact in discuss_items {
                    context.push_str(&format!("#### {}\n\n", artifact.resource_uri));

                    // Include rationale if available.
                    if let Some(ref why) = artifact.rationale {
                        context.push_str(&format!("**Agent's original rationale:** {}\n\n", why));
                    }

                    // Include explanation tiers if available (v0.2.3+).
                    if let Some(ref tiers) = artifact.explanation_tiers {
                        if !tiers.summary.is_empty() {
                            context
                                .push_str(&format!("**What was changed:** {}\n\n", tiers.summary));
                        }
                        if !tiers.explanation.is_empty() {
                            context.push_str(&format!("**Why:** {}\n\n", tiers.explanation));
                        }
                    }

                    // Include comment thread if available (v0.3.0 — the key missing piece!).
                    if let Some(ref comments) = artifact.comments {
                        if !comments.is_empty() {
                            context.push_str("**Review discussion:**\n\n");
                            for (idx, comment) in comments.comments.iter().enumerate() {
                                context.push_str(&format!(
                                    "{}. **{}** ({}): {}\n",
                                    idx + 1,
                                    comment.commenter,
                                    comment.created_at.format("%Y-%m-%d %H:%M UTC"),
                                    comment.text
                                ));
                            }
                            context.push('\n');
                        }
                    }

                    context.push_str("---\n\n");
                }

                context.push_str("**Your task:** Address each discussion item above. ");
                context.push_str("For each artifact, either revise it to address the concerns, ");
                context.push_str("provide clarification in your change_summary.json, or explain ");
                context.push_str("why the change is correct as-is.\n\n");
            }
        }
    }

    context
}

/// Build the macro goal context section for CLAUDE.md injection.
/// Explains the MCP tools available for inner-loop iteration.
fn build_macro_goal_section(goal_id: &str) -> String {
    format!(
        r#"
## Macro Goal Mode (Inner-Loop Iteration)

This is a **macro goal** session. You can decompose your work into sub-goals,
submit drafts for human review mid-session, and iterate based on feedback —
all without exiting.

### Available MCP Tools

Use these tools to interact with TA during your session:

- **`ta_draft`** — Manage draft packages
  - `action: "build"` — Bundle your current changes into a draft for review
  - `action: "submit"` — Submit a draft for human review (blocks until response)
  - `action: "status"` — Check the review status of a draft
  - `action: "list"` — List all drafts for this goal

- **`ta_goal`** — Manage sub-goals
  - `action: "start"` — Create a sub-goal within this macro session
  - `action: "status"` — Check the status of a sub-goal

- **`ta_plan`** — Interact with the project plan
  - `action: "read"` — Read current plan progress
  - `action: "update"` — Propose plan updates (held for human approval)

### Workflow

1. Work on a logical unit of change
2. Call `ta_draft` with `action: "build"` to package your changes
3. Call `ta_draft` with `action: "submit"` to send for human review
4. Wait for approval or feedback
5. If approved, continue to the next sub-goal
6. If denied, revise and resubmit

### Security Boundaries

- You **CAN**: propose sub-goals, build drafts, submit for review, read plan status
- You **CANNOT**: approve your own drafts, apply changes, bypass checkpoints

**Macro Goal ID:** {}
"#,
        goal_id
    )
}

/// Inject a CLAUDE.md file into the staging workspace to orient the agent.
/// Saves the original content to `.ta/claude_md_original` for later restoration.
#[allow(clippy::too_many_arguments)]
fn inject_claude_md(
    staging_path: &Path,
    title: &str,
    goal_id: &str,
    plan_phase: Option<&str>,
    source_dir: Option<&Path>,
    parent_goal_id: Option<uuid::Uuid>,
    goal_store: &ta_goal::GoalRunStore,
    config: &GatewayConfig,
    macro_goal: bool,
) -> anyhow::Result<()> {
    let claude_md_path = staging_path.join("CLAUDE.md");
    let backup_path = staging_path.join(CLAUDE_MD_BACKUP);

    // If a backup already exists from a previous injection (e.g., follow-up reusing
    // parent staging), restore the original CLAUDE.md first so we inject fresh
    // content on top of the real project file, not a stale injected copy.
    if backup_path.exists() {
        let saved_original = std::fs::read_to_string(&backup_path)?;
        if saved_original == NO_ORIGINAL_SENTINEL {
            if claude_md_path.exists() {
                std::fs::remove_file(&claude_md_path)?;
            }
        } else {
            std::fs::write(&claude_md_path, &saved_original)?;
        }
    }

    // Read and save original content.
    let original_content = if claude_md_path.exists() {
        std::fs::read_to_string(&claude_md_path)?
    } else {
        NO_ORIGINAL_SENTINEL.to_string()
    };

    // Save backup to .ta/ in staging (excluded from copy and diff).
    let backup_dir = staging_path.join(".ta");
    std::fs::create_dir_all(&backup_dir)?;
    std::fs::write(&backup_path, &original_content)?;

    // Build injected content.
    let existing_section = if original_content == NO_ORIGINAL_SENTINEL {
        String::new()
    } else {
        original_content
    };

    // Build plan context section if PLAN.md exists in source.
    let plan_section = build_plan_section(plan_phase, source_dir);

    // Build parent context section if this is a follow-up goal.
    let parent_section = build_parent_context_section(parent_goal_id, goal_store, config);

    // Build macro goal section if --macro was specified.
    let macro_section = if macro_goal {
        build_macro_goal_section(goal_id)
    } else {
        String::new()
    };

    // Build memory context section from prior sessions (v0.6.3: phase-aware).
    let memory_section = build_memory_context_section_for_inject(config, title, plan_phase);

    // Build solutions section from curated knowledge base (v0.8.1).
    let solutions_section = build_solutions_section_for_inject(config);

    let injected = format!(
        r#"# Trusted Autonomy — Mediated Goal

You are working on a TA-mediated goal in a staging workspace.

**Goal:** {}
**Goal ID:** {}
{}{}{}{}{}
## How this works

- This directory is a copy of the original project
- Work normally — Read, Write, Edit, Bash all work as expected
- When you're done, just exit. TA will diff your changes and create a draft for review
- The human reviewer will see exactly what you changed and why

## Important

- Do NOT modify files outside this directory
- All your changes will be captured as a draft for human review

## Before You Exit — Change Summary (REQUIRED)

You MUST create `.ta/change_summary.json` before exiting. The human reviewer relies on this to understand your work. Every changed file needs a clear "what I did" and "why" — reviewers who don't understand a change will reject it.

```json
{{
  "summary": "Brief description of all changes made in this session",
  "changes": [
    {{
      "path": "relative/path/to/file",
      "action": "modified|created|deleted",
      "what": "Specific description of what was changed in this target",
      "why": "Why this change was needed (motivation, not just restating what)",
      "independent": true,
      "depends_on": [],
      "depended_by": []
    }}
  ],
  "dependency_notes": "Human-readable explanation of which changes are coupled and why"
}}
```

Rules for per-target descriptions:
- **`what`** (REQUIRED): Describe specifically what you changed. NOT "updated file" — instead "Added JWT validation middleware with RS256 signature verification" or "Removed deprecated session-cookie auth fallback". The reviewer sees this as the primary description for each changed file.
- **`why`**: The motivation, not a restatement of what. "Security audit flagged session cookies as vulnerable" not "To add JWT validation".
- For lockfiles, config files, and generated files: still provide `what` (e.g., "Added jsonwebtoken v9.3 dependency") — don't leave them blank.
- `independent`: true if this change can be applied or reverted without affecting other changes
- `depends_on`: list of other file paths this change requires (e.g., if you add a function call, it depends on the file where the function is defined)
- `depended_by`: list of other file paths that would break if this change is reverted
- Be honest about dependencies — the reviewer uses this to decide which changes to accept individually

## Plan Updates (REQUIRED if PLAN.md exists)

As you complete planned work items, update PLAN.md to reflect progress:
- Move completed items from "Remaining" to "Completed" with a ✅ checkmark
- Update test counts when you add or remove tests
- Do NOT change the `<!-- status: ... -->` marker — only `ta draft apply` transitions phase status
- If you complete all remaining items in a phase, note that in your change_summary.json

## Documentation Updates

If your changes affect user-facing behavior (new commands, changed flags, new config options, workflow changes):
- Update `docs/USAGE.md` with the new/changed functionality
- Keep the tone consumer-friendly (no internal implementation details)
- Update version references if they exist in the docs
- Update the `CLAUDE.md` "Current State" section if the test count changes

---

{}
"#,
        title,
        goal_id,
        plan_section,
        parent_section,
        macro_section,
        memory_section,
        solutions_section,
        existing_section
    );

    std::fs::write(&claude_md_path, injected)?;
    Ok(())
}

/// Auto-capture goal completion into memory (v0.5.6).
///
/// Reads `.ta/change_summary.json` from the staging workspace and stores
/// the goal completion event in the memory store for future context injection.
fn auto_capture_goal_completion(
    config: &GatewayConfig,
    goal: &ta_goal::GoalRun,
    staging_path: &Path,
) {
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut store = ta_memory::FsMemoryStore::new(&memory_dir);

    let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
    let capture_config = ta_memory::auto_capture::load_config(&workflow_toml);
    let capture = ta_memory::AutoCapture::new(capture_config);

    // Try to read change_summary.json from staging.
    let summary_path = staging_path.join(".ta").join("change_summary.json");
    let change_summary = if summary_path.exists() {
        std::fs::read_to_string(&summary_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    } else {
        None
    };

    // Extract changed file list from change_summary if available.
    let changed_files = change_summary
        .as_ref()
        .and_then(|v: &serde_json::Value| v.get("changes"))
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("path").and_then(|p| p.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let event = ta_memory::GoalCompleteEvent {
        goal_id: goal.goal_run_id,
        title: goal.title.clone(),
        agent_framework: goal.agent_id.clone(),
        change_summary,
        changed_files,
        phase_id: goal.plan_phase.clone(),
    };

    if let Err(e) = capture.on_goal_complete(&mut store, &event) {
        tracing::warn!("failed to auto-capture goal completion: {}", e);
    }
}

/// Build a memory context section from prior sessions for CLAUDE.md injection (v0.6.3).
///
/// Phase-aware: filters entries by the current plan phase. Respects the
/// `backend` field in `.ta/memory.toml` (v0.7.4): "ruvector" uses semantic
/// search when available, "fs" forces filesystem-only mode.
pub fn build_memory_context_section_for_inject(
    config: &GatewayConfig,
    goal_title: &str,
    phase_id: Option<&str>,
) -> String {
    let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
    let capture_config = ta_memory::auto_capture::load_config(&workflow_toml);
    let max_entries = capture_config.max_context_entries;

    // v0.7.4: Respect backend toggle from .ta/memory.toml.
    let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
    let _backend = memory_config.backend.as_deref().unwrap_or("ruvector");

    // Try RuVector backend when configured (or default).
    #[cfg(feature = "ruvector")]
    if _backend != "fs" {
        let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
        if rvf_path.exists() {
            if let Ok(store) = ta_memory::RuVectorStore::open(&rvf_path) {
                return ta_memory::auto_capture::build_memory_context_section_with_phase(
                    &store,
                    goal_title,
                    max_entries,
                    phase_id,
                )
                .unwrap_or_default();
            }
        }
    }

    // Filesystem backend (explicit "fs" or fallback).
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let store = ta_memory::FsMemoryStore::new(&memory_dir);
    ta_memory::auto_capture::build_memory_context_section_with_phase(
        &store,
        goal_title,
        max_entries,
        phase_id,
    )
    .unwrap_or_default()
}

/// Build the solutions section for CLAUDE.md injection (v0.8.1).
///
/// Reads from `.ta/solutions/solutions.toml` and includes relevant entries
/// matched by project type.
fn build_solutions_section_for_inject(config: &GatewayConfig) -> String {
    let solutions_path = config
        .workspace_root
        .join(".ta")
        .join("solutions")
        .join("solutions.toml");
    let store = ta_memory::SolutionStore::new(&solutions_path);

    let solutions = match store.load() {
        Ok(s) => s,
        Err(_) => return String::new(),
    };

    if solutions.is_empty() {
        return String::new();
    }

    // Filter by project type.
    let schema = ta_memory::KeySchema::resolve(&config.workspace_root);
    let language = match schema.project_type {
        ta_memory::ProjectType::RustWorkspace => Some("rust"),
        ta_memory::ProjectType::TypeScript => Some("typescript"),
        ta_memory::ProjectType::Python => Some("python"),
        ta_memory::ProjectType::Go => Some("go"),
        ta_memory::ProjectType::Generic => None,
    };

    let relevant: Vec<_> = solutions
        .iter()
        .filter(|s| {
            // Include if context language matches or is unset.
            match (&s.context.language, language) {
                (Some(sl), Some(pl)) => sl == pl,
                (None, _) => true,
                (_, None) => true,
            }
        })
        .take(15) // Limit to avoid overwhelming the context.
        .collect();

    if relevant.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n## Known Solutions\n\n");
    section
        .push_str("The following problem/solution pairs were captured from previous sessions:\n\n");
    for sol in &relevant {
        section.push_str(&format!("- **{}**: {}\n", sol.problem, sol.solution));
    }
    section.push('\n');
    section
}

/// Restore the original CLAUDE.md content before computing diffs.
/// This removes TA's injection so it doesn't appear in PR packages.
fn restore_claude_md(staging_path: &Path) -> anyhow::Result<()> {
    let backup_path = staging_path.join(CLAUDE_MD_BACKUP);
    let claude_md_path = staging_path.join("CLAUDE.md");

    if !backup_path.exists() {
        return Ok(()); // No backup — nothing to restore.
    }

    let original = std::fs::read_to_string(&backup_path)?;

    if original == NO_ORIGINAL_SENTINEL {
        // CLAUDE.md didn't exist originally — remove it.
        if claude_md_path.exists() {
            std::fs::remove_file(&claude_md_path)?;
        }
    } else {
        // Restore original content.
        std::fs::write(&claude_md_path, original)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn run_creates_goal_and_restores_on_no_launch() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::write(
            project.path().join("CLAUDE.md"),
            "# Existing project instructions\n",
        )
        .unwrap();

        let config = GatewayConfig::for_project(project.path());

        // Run with --no-launch to avoid actually starting the agent.
        execute(
            &config,
            Some("Test goal"),
            "claude-code",
            Some(project.path()),
            "Test objective",
            None,
            None,
            None,
            true,
            false,
            false,
            None,
            false, // not headless
        )
        .unwrap();

        // Verify goal was created.
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        assert_eq!(goals.len(), 1);

        // With --no-launch, injected files should be restored so they
        // don't contaminate a subsequent `ta pr build` diff.
        let claude_md = std::fs::read_to_string(goals[0].workspace_path.join("CLAUDE.md")).unwrap();
        assert_eq!(claude_md, "# Existing project instructions\n");

        // Settings should also be restored (removed since it didn't exist).
        assert!(!goals[0].workspace_path.join(SETTINGS_REL_PATH).exists());
    }

    #[test]
    fn run_injects_context_for_agent() {
        // Verify that inject + restore roundtrip works for the agent path.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        std::fs::write(
            staging.path().join("CLAUDE.md"),
            "# Existing project instructions\n",
        )
        .unwrap();

        inject_claude_md(
            staging.path(),
            "Test goal",
            "goal-123",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
        )
        .unwrap();

        // Verify CLAUDE.md was injected.
        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Trusted Autonomy"));
        assert!(claude_md.contains("Test goal"));
        assert!(claude_md.contains("Existing project instructions"));

        // Verify backup was saved.
        let backup = std::fs::read_to_string(staging.path().join(CLAUDE_MD_BACKUP)).unwrap();
        assert_eq!(backup, "# Existing project instructions\n");

        inject_claude_settings(staging.path(), None).unwrap();

        // Verify settings.local.json was injected.
        let settings = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(settings.contains("Trusted Autonomy"));
        assert!(settings.contains("Bash(*)"));
    }

    #[test]
    fn inject_and_restore_claude_md_roundtrip() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let original = "# My Project\nExisting instructions.\n";
        std::fs::write(staging.path().join("CLAUDE.md"), original).unwrap();

        inject_claude_md(
            staging.path(),
            "Fix bug",
            "goal-123",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
        )
        .unwrap();

        // Verify injection happened.
        let injected = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(injected.contains("Trusted Autonomy"));
        assert!(injected.contains("Fix bug"));
        assert!(injected.contains("Existing instructions"));

        // Restore.
        restore_claude_md(staging.path()).unwrap();

        // Verify original content is back.
        let restored = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn restore_removes_claude_md_if_not_originally_present() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        // No CLAUDE.md exists initially.

        inject_claude_md(
            staging.path(),
            "New goal",
            "goal-456",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
        )
        .unwrap();

        // CLAUDE.md was created by injection.
        assert!(staging.path().join("CLAUDE.md").exists());

        // Restore should remove it.
        restore_claude_md(staging.path()).unwrap();
        assert!(!staging.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn inject_claude_md_with_macro_goal_section() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        inject_claude_md(
            staging.path(),
            "Macro goal test",
            "goal-macro-789",
            None,
            None,
            None,
            &goal_store,
            &config,
            true, // macro_goal = true
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Macro Goal Mode"));
        assert!(claude_md.contains("ta_draft"));
        assert!(claude_md.contains("ta_goal"));
        assert!(claude_md.contains("ta_plan"));
        assert!(claude_md.contains("Inner-Loop"));
        assert!(claude_md.contains("goal-macro-789"));
    }

    #[test]
    fn agent_config_returns_correct_launch_config() {
        // Pass None for source_dir — tests use built-in fallbacks (no YAML files).
        let claude = agent_launch_config("claude-code", None);
        assert_eq!(claude.command, "claude");
        assert!(claude.injects_context_file);
        assert!(claude.injects_settings);
        assert!(!claude
            .args_template
            .contains(&"--dangerously-skip-permissions".to_string()));

        let codex = agent_launch_config("codex", None);
        assert_eq!(codex.command, "codex");
        assert!(!codex.injects_context_file);
        assert!(!codex.injects_settings);

        let flow = agent_launch_config("claude-flow", None);
        assert_eq!(flow.command, "npx");
        assert!(flow.injects_context_file);
        assert!(flow.injects_settings);
        assert!(flow
            .args_template
            .contains(&"claude-flow@alpha".to_string()));
        assert!(flow.args_template.contains(&"hive-mind".to_string()));
        assert!(flow.args_template.contains(&"--claude".to_string()));
        let pre = flow.pre_launch.expect("claude-flow should have pre_launch");
        assert_eq!(pre.command, "npx");
        assert!(pre.args.contains(&"hive-mind".to_string()));
        assert!(pre.args.contains(&"init".to_string()));

        let unknown = agent_launch_config("my-custom-agent", None);
        assert_eq!(unknown.command, "my-custom-agent");
        assert!(unknown.args_template.is_empty());
        assert!(unknown.pre_launch.is_none());
        assert!(!unknown.injects_settings);
    }

    #[test]
    fn agent_config_loads_from_yaml() {
        let project = TempDir::new().unwrap();
        let agents_dir = project.path().join(".ta").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let yaml = r#"
name: test-agent
description: "A test agent"
command: my-test-cmd
args_template:
  - "--flag"
  - "{prompt}"
injects_context_file: true
injects_settings: false
env:
  MY_VAR: "hello"
"#;
        std::fs::write(agents_dir.join("test-agent.yaml"), yaml).unwrap();

        let config = agent_launch_config("test-agent", Some(project.path()));
        assert_eq!(config.command, "my-test-cmd");
        assert_eq!(config.args_template, vec!["--flag", "{prompt}"]);
        assert!(config.injects_context_file);
        assert!(!config.injects_settings);
        assert!(config.pre_launch.is_none());
        assert_eq!(config.env.get("MY_VAR").unwrap(), "hello");
    }

    #[test]
    fn agent_config_yaml_with_pre_launch() {
        let project = TempDir::new().unwrap();
        let agents_dir = project.path().join(".ta").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let yaml = r#"
name: flow-test
command: npx
args_template: ["{prompt}"]
pre_launch:
  command: npx
  args: ["flow", "init"]
"#;
        std::fs::write(agents_dir.join("flow-test.yaml"), yaml).unwrap();

        let config = agent_launch_config("flow-test", Some(project.path()));
        assert_eq!(config.command, "npx");
        let pre = config.pre_launch.expect("should have pre_launch");
        assert_eq!(pre.command, "npx");
        assert_eq!(pre.args, vec!["flow", "init"]);
    }

    #[test]
    fn inject_and_restore_settings_roundtrip() {
        let staging = TempDir::new().unwrap();

        inject_claude_settings(staging.path(), None).unwrap();

        let settings_path = staging.path().join(SETTINGS_REL_PATH);
        assert!(settings_path.exists());
        let content = std::fs::read_to_string(&settings_path).unwrap();
        assert!(content.contains("Trusted Autonomy"));
        assert!(content.contains("Bash(*)"));
        assert!(content.contains("\"deny\": ["));

        restore_claude_settings(staging.path()).unwrap();
        assert!(!settings_path.exists());
    }

    #[test]
    fn inject_settings_preserves_existing() {
        let staging = TempDir::new().unwrap();
        let claude_dir = staging.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let original = r#"{"customSetting": true}"#;
        std::fs::write(claude_dir.join("settings.local.json"), original).unwrap();

        inject_claude_settings(staging.path(), None).unwrap();

        let injected = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(injected.contains("Trusted Autonomy"));

        restore_claude_settings(staging.path()).unwrap();
        let restored = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn forbidden_tools_loaded_from_file() {
        let project = TempDir::new().unwrap();
        std::fs::write(
            project.path().join(FORBIDDEN_TOOLS_FILE),
            "# Comment line\nBash(rm -rf /*)\n\nBash(curl * | sh)\n",
        )
        .unwrap();

        let forbidden = load_forbidden_tools(Some(project.path()));
        assert_eq!(forbidden.len(), 2);
        assert_eq!(forbidden[0], "Bash(rm -rf /*)");
        assert_eq!(forbidden[1], "Bash(curl * | sh)");
    }

    #[test]
    fn forbidden_tools_empty_when_no_file() {
        let project = TempDir::new().unwrap();
        let forbidden = load_forbidden_tools(Some(project.path()));
        assert!(forbidden.is_empty());
    }

    #[test]
    fn inject_settings_includes_forbidden_tools() {
        let staging = TempDir::new().unwrap();
        let source = TempDir::new().unwrap();
        std::fs::write(
            source.path().join(FORBIDDEN_TOOLS_FILE),
            "Bash(rm -rf /*)\n",
        )
        .unwrap();

        inject_claude_settings(staging.path(), Some(source.path())).unwrap();

        let content = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(content.contains("Bash(rm -rf /*)"));
    }

    #[test]
    fn parent_context_injects_comment_threads_for_discuss_items() {
        use chrono::Utc;
        use ta_changeset::draft_package::{
            AgentIdentity, Artifact, ArtifactDisposition, ChangeType, Changes, DraftPackage,
            DraftStatus, ExplanationTiers, Goal, Iteration, Plan, Provenance, ReviewRequests, Risk,
            Signatures, Summary, WorkspaceRef,
        };
        use ta_changeset::review_session::CommentThread;
        use ta_goal::GoalRun;
        use uuid::Uuid;

        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create a parent goal with a draft package containing discuss items with comments.
        let parent_goal_id = Uuid::new_v4();
        let parent_pr_id = Uuid::new_v4();

        let mut parent_goal = GoalRun::new(
            "Fix auth bug",
            "Fix the authentication issue",
            "test-agent",
            project.path().join(".ta/staging/parent"),
            project.path().join(".ta/store/parent"),
        );
        parent_goal.goal_run_id = parent_goal_id; // Override the UUID for testing
        parent_goal.pr_package_id = Some(parent_pr_id);
        parent_goal.source_dir = Some(project.path().to_path_buf());
        goal_store.save(&parent_goal).unwrap();

        // Create a draft package with discuss items that have comment threads.
        let mut comment_thread = CommentThread::new();
        comment_thread.add("reviewer-1", "This needs error handling for null tokens");
        comment_thread.add("agent-1", "Understood, I'll add validation");
        comment_thread.add("reviewer-1", "Also consider adding tests for edge cases");

        let artifact_with_comments = Artifact {
            resource_uri: "fs://workspace/src/auth/middleware.rs".to_string(),
            change_type: ChangeType::Modify,
            diff_ref: "changeset:0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Discuss,
            rationale: Some("Refactored to use JWT".to_string()),
            dependencies: vec![],
            explanation_tiers: Some(ExplanationTiers {
                summary: "Switched auth from sessions to JWT tokens".to_string(),
                explanation: "Implemented RS256 signature verification".to_string(),
                tags: vec!["security".to_string()],
                related_artifacts: vec![],
            }),
            comments: Some(comment_thread),
            amendment: None,
        };

        let parent_draft = DraftPackage {
            package_version: "1.0.0".to_string(),
            package_id: parent_pr_id,
            created_at: Utc::now(),
            goal: Goal {
                goal_id: parent_goal_id.to_string(),
                title: "Fix auth bug".to_string(),
                objective: "Fix authentication".to_string(),
                success_criteria: vec![],
                constraints: vec![],
            },
            iteration: Iteration {
                iteration_id: "iter-1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging".to_string(),
                    ref_name: "staging/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "agent-1".to_string(),
                agent_type: "coder".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "hash-123".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Auth refactor".to_string(),
                why: "Modernize auth".to_string(),
                impact: "1 file changed".to_string(),
                rollback_plan: "Revert commit".to_string(),
                open_questions: vec![],
            },
            plan: Plan {
                completed_steps: vec![],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![artifact_with_comments],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 0,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "trace-123".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![],
                reviewers: vec![],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "hash-456".to_string(),
                agent_signature: "sig-789".to_string(),
                gateway_attestation: None,
            },
            status: DraftStatus::PendingReview,
        };

        // Save the draft package.
        super::super::draft::save_package(&config, &parent_draft).unwrap();

        // Build parent context section.
        let context = build_parent_context_section(Some(parent_goal_id), &goal_store, &config);

        // Verify the context includes follow-up information.
        assert!(context.contains("Follow-Up Context"));
        assert!(context.contains("Fix auth bug"));

        // Verify discuss items are listed.
        assert!(context.contains("Items for Discussion"));
        assert!(context.contains("fs://workspace/src/auth/middleware.rs"));

        // Verify original rationale is included.
        assert!(context.contains("Agent's original rationale:"));
        assert!(context.contains("Refactored to use JWT"));

        // Verify explanation tiers are included.
        assert!(context.contains("What was changed:"));
        assert!(context.contains("Switched auth from sessions to JWT tokens"));
        assert!(context.contains("Why:"));
        assert!(context.contains("Implemented RS256 signature verification"));

        // *** THE KEY TEST: Verify comment threads are injected! ***
        assert!(context.contains("Review discussion:"));
        assert!(context.contains("reviewer-1"));
        assert!(context.contains("This needs error handling for null tokens"));
        assert!(context.contains("agent-1"));
        assert!(context.contains("Understood, I'll add validation"));
        assert!(context.contains("Also consider adding tests for edge cases"));

        // Verify guidance is included.
        assert!(context.contains("Your task:"));
        assert!(context.contains("Address each discussion item"));
    }

    #[test]
    fn parent_context_handles_discuss_items_without_comments() {
        // Ensure graceful handling when discuss items don't have comment threads yet.
        use chrono::Utc;
        use ta_changeset::draft_package::{
            AgentIdentity, Artifact, ArtifactDisposition, ChangeType, Changes, DraftPackage,
            DraftStatus, Goal, Iteration, Plan, Provenance, ReviewRequests, Risk, Signatures,
            Summary, WorkspaceRef,
        };
        use ta_goal::GoalRun;
        use uuid::Uuid;

        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        let parent_goal_id = Uuid::new_v4();
        let parent_pr_id = Uuid::new_v4();

        let mut parent_goal = GoalRun::new(
            "Test goal",
            "Test objective",
            "test-agent",
            project.path().join(".ta/staging/parent"),
            project.path().join(".ta/store/parent"),
        );
        parent_goal.goal_run_id = parent_goal_id; // Override the UUID for testing
        parent_goal.pr_package_id = Some(parent_pr_id);
        parent_goal.source_dir = Some(project.path().to_path_buf());
        goal_store.save(&parent_goal).unwrap();

        // Artifact with Discuss disposition but NO comment thread.
        let artifact_no_comments = Artifact {
            resource_uri: "fs://workspace/test.rs".to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Discuss,
            rationale: Some("Needs review".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None, // No comments yet
            amendment: None,
        };

        let parent_draft = DraftPackage {
            package_version: "1.0.0".to_string(),
            package_id: parent_pr_id,
            created_at: Utc::now(),
            goal: Goal {
                goal_id: parent_goal_id.to_string(),
                title: "Test".to_string(),
                objective: "Test".to_string(),
                success_criteria: vec![],
                constraints: vec![],
            },
            iteration: Iteration {
                iteration_id: "iter-1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging".to_string(),
                    ref_name: "staging/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "agent-1".to_string(),
                agent_type: "coder".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "hash-123".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Test".to_string(),
                why: "Test".to_string(),
                impact: "Test".to_string(),
                rollback_plan: "Test".to_string(),
                open_questions: vec![],
            },
            plan: Plan {
                completed_steps: vec![],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![artifact_no_comments],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 0,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "trace-123".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![],
                reviewers: vec![],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "hash-456".to_string(),
                agent_signature: "sig-789".to_string(),
                gateway_attestation: None,
            },
            status: DraftStatus::PendingReview,
        };

        super::super::draft::save_package(&config, &parent_draft).unwrap();

        let context = build_parent_context_section(Some(parent_goal_id), &goal_store, &config);

        // Should still show discuss items even without comments.
        assert!(context.contains("Items for Discussion"));
        assert!(context.contains("fs://workspace/test.rs"));
        assert!(context.contains("Needs review"));

        // Should NOT crash or show "Review discussion:" when there are no comments.
        assert!(!context.contains("Review discussion:"));
    }

    #[test]
    fn inject_claude_md_restores_original_on_reinjection() {
        // Simulates the follow-up-reusing-parent-staging scenario:
        // 1. First injection writes TA header on top of original content
        // 2. Second injection (follow-up) should restore original, then inject fresh
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        let original = "# Original CLAUDE.md\n\nCurrent version: 0.4.4-alpha\n";
        std::fs::write(staging.path().join("CLAUDE.md"), original).unwrap();

        // First injection (parent goal).
        inject_claude_md(
            staging.path(),
            "Parent goal",
            "goal-parent-111",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
        )
        .unwrap();

        let first_injected = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(first_injected.contains("Trusted Autonomy"));
        assert!(first_injected.contains("Parent goal"));
        assert!(first_injected.contains("Original CLAUDE.md"));

        // Second injection (follow-up reusing same staging — no restore between).
        inject_claude_md(
            staging.path(),
            "Follow-up goal",
            "goal-followup-222",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
        )
        .unwrap();

        let second_injected = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();

        // Should contain the NEW goal info.
        assert!(
            second_injected.contains("Follow-up goal"),
            "should have follow-up title"
        );
        assert!(
            second_injected.contains("goal-followup-222"),
            "should have follow-up goal ID"
        );

        // Should contain the ORIGINAL content (not the parent's injection).
        assert!(
            second_injected.contains("0.4.4-alpha"),
            "should have original version, not stale parent version"
        );

        // Should NOT contain the parent's goal info (that was the old injection).
        assert!(
            !second_injected.contains("goal-parent-111"),
            "should not contain parent's injected goal ID"
        );

        // The original content should appear exactly once (no nesting).
        assert_eq!(
            second_injected.matches("Original CLAUDE.md").count(),
            1,
            "original content should appear exactly once, not nested"
        );

        // Restore should get back the true original.
        restore_claude_md(staging.path()).unwrap();
        let restored = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert_eq!(restored, original);
    }
}
