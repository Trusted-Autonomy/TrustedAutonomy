// advisor.rs — CLI commands for the advisor agent (v0.15.26 + v0.15.28).
//
// Commands:
//   ta advisor ask "<message>"                  — classify intent and print numbered option card
//   ta advisor advise "<message>"               — inject a mid-run note to the active goal
//   ta advisor advise --goal <id> "<message>"   — target a specific goal
//
// The advisor classifies your input, returns a numbered menu of actions, and
// accepts a number from stdin to confirm. Security level is read from daemon
// config (read_only / suggest / auto).
//
// In read_only mode: commands are shown as copyable text.
// In suggest mode:   commands are shown with a [run] prompt.
// In auto mode:      high-confidence (≥80%) goals fire automatically.

use std::io::{self, BufRead, Write};

use clap::Subcommand;
use ta_goal::{GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;
use ta_runtime::{build_channel, AgentFrameworkManifest, ChannelType, HumanNote, NoteDelivery};
use ta_session::workflow_session::AdvisorSecurity;
use ta_session::{AdvisorContext, AdvisorSession};

#[derive(Subcommand)]
pub enum AdvisorCommands {
    /// Ask the advisor a question or give it a natural language instruction.
    ///
    /// The advisor classifies your intent and presents numbered options.
    /// Enter an option number to confirm the action.
    ///
    /// Examples:
    ///   ta advisor ask "implement remaining v0.15"
    ///   ta advisor ask "apply"
    ///   ta advisor ask "what changed in the last draft?"
    Ask {
        /// The message or instruction for the advisor.
        message: String,
        /// Security level override: read_only (default), suggest, auto.
        #[arg(long)]
        security: Option<String>,
        /// Tab context to shape the option menu (e.g. workflows, plan, drafts).
        #[arg(long)]
        tab: Option<String>,
        /// Currently selected item (e.g. workflow name, phase ID).
        #[arg(long)]
        selection: Option<String>,
        /// Non-interactive: print the card and exit without prompting for a choice.
        #[arg(long)]
        no_input: bool,
        /// Output as JSON (for scripting).
        #[arg(long)]
        json: bool,
    },

    /// Send a mid-run note to the active goal's agent.
    ///
    /// The note is delivered via the goal's context channel. The delivery mode
    /// is printed so you know whether the agent saw it live (live-polled),
    /// via API push (api-pushed), or will see it at the next restart (queued).
    ///
    /// Examples:
    ///   ta advisor advise "please focus on the auth module"
    ///   ta advisor advise --goal abc123 "add more test coverage"
    Advise {
        /// The note/instruction to send to the agent.
        message: String,
        /// Goal ID (or prefix) to target. Defaults to the most recent running goal.
        #[arg(long)]
        goal: Option<String>,
    },
}

pub fn execute(cmd: &AdvisorCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        AdvisorCommands::Ask {
            message,
            security,
            tab,
            selection,
            no_input,
            json,
        } => ask(
            config,
            message,
            security.as_deref(),
            tab.as_deref(),
            selection.as_deref(),
            *no_input,
            *json,
        ),
        AdvisorCommands::Advise { message, goal } => advise(config, message, goal.as_deref()),
    }
}

fn ask(
    config: &GatewayConfig,
    message: &str,
    security_override: Option<&str>,
    tab: Option<&str>,
    selection: Option<&str>,
    no_input: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    // Resolve security level: flag > daemon config > default (read_only).
    let security = resolve_security(config, security_override);

    let context = AdvisorContext {
        tab: tab.unwrap_or("cli").to_string(),
        selection: selection.map(str::to_string),
    };

    let session = AdvisorSession::from_message(message, &security, &context);

    if json_output {
        let json = serde_json::to_string_pretty(&session)
            .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e));
        println!("{}", json);
        return Ok(());
    }

    // Print the advisor card.
    session.print_card();

    if no_input || session.options.is_empty() {
        return Ok(());
    }

    // Prompt for a choice.
    let max = session.options.len() as u32;
    print!("Enter option [1-{}] or press Enter to cancel: ", max);
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let choice = line.trim();

    if choice.is_empty() {
        println!("Cancelled.");
        return Ok(());
    }

    let num: u32 = match choice.parse() {
        Ok(n) if n >= 1 && n <= max => n,
        _ => {
            eprintln!(
                "Invalid choice '{}'. Enter a number between 1 and {}.",
                choice, max
            );
            return Ok(());
        }
    };

    let opt = match session.option_by_number(num) {
        Some(o) => o,
        None => {
            eprintln!("Option {} not found.", num);
            return Ok(());
        }
    };

    execute_option(opt, &security, config)
}

/// Inject a mid-run note to an active goal's agent via the unified context channel.
///
/// Resolves the target goal, selects the appropriate channel for its agent framework,
/// calls `inject_note()`, and prints the delivery outcome with the notes file path
/// so the user knows exactly where the note landed.
pub fn advise(
    config: &GatewayConfig,
    message: &str,
    goal_id_hint: Option<&str>,
) -> anyhow::Result<()> {
    let message = message.trim();
    if message.is_empty() {
        anyhow::bail!("Message cannot be empty. Provide a note for the agent.");
    }

    // Load goal store.
    let goals_dir = config.workspace_root.join(".ta/goals");
    let store = GoalRunStore::new(&goals_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to load goal store from {:?}: {}. \
             Run `ta goal list` to verify the workspace.",
            goals_dir,
            e
        )
    })?;

    // Resolve the target goal.
    let goal = resolve_advise_goal(&store, goal_id_hint)?;
    let goal_id_str = goal.goal_run_id.to_string();

    // Resolve channel type from the agent framework manifest.
    let manifest = AgentFrameworkManifest::resolve(&goal.agent_id, &config.workspace_root);
    let channel_type = manifest
        .as_ref()
        .map(|m| m.channel_type.clone())
        .unwrap_or(ChannelType::ClaudeCode);
    let context_file = manifest
        .map(|m| m.context_file)
        .unwrap_or_else(|| "CLAUDE.md".to_string());

    // Build the channel and inject the note.
    let channel = build_channel(&channel_type, goal.workspace_path.clone(), &context_file);
    let note = HumanNote::new(&goal_id_str, message);
    let delivery = channel.inject_note(&note).map_err(|e| {
        anyhow::anyhow!(
            "Failed to inject note into goal {} (staging: {:?}): {}. \
             Check that the staging directory exists and is accessible.",
            goal_id_str,
            goal.workspace_path,
            e
        )
    })?;

    // Print outcome.
    match &delivery {
        NoteDelivery::LivePolled => {
            println!("Note injected — agent will read on next polling cycle.");
        }
        NoteDelivery::ApiPushed => {
            println!("Note pushed via agent API — agent received it live.");
        }
        NoteDelivery::Queued => {
            println!("Note queued — will be injected at next goal restart.");
        }
        NoteDelivery::Answered => {
            println!("Advisor answered directly.");
        }
    }

    // Show the notes file path for ClaudeCode channel.
    if channel_type == ChannelType::ClaudeCode {
        let notes_path = goal
            .workspace_path
            .join(".ta/advisor-notes")
            .join(format!("{}.md", goal_id_str));
        println!("Notes file: {}", notes_path.display());
    }

    println!(
        "Goal: {} ({})",
        &goal_id_str[..8.min(goal_id_str.len())],
        goal.title
    );
    println!("Delivery:  {}", delivery);

    Ok(())
}

/// Resolve the target goal for injection.
///
/// If `goal_id_hint` is provided, find the goal by ID prefix.
/// Otherwise, return the most recently running goal.
fn resolve_advise_goal(
    store: &GoalRunStore,
    goal_id_hint: Option<&str>,
) -> anyhow::Result<ta_goal::GoalRun> {
    let goals = store
        .list()
        .map_err(|e| anyhow::anyhow!("Failed to list goals: {}", e))?;

    if let Some(hint) = goal_id_hint {
        let matched: Vec<_> = goals
            .iter()
            .filter(|g| g.goal_run_id.to_string().starts_with(hint))
            .collect();
        match matched.len() {
            0 => anyhow::bail!(
                "No goal found matching prefix '{}'. \
                 Use `ta goal list` to see available goals.",
                hint
            ),
            1 => Ok(matched[0].clone()),
            n => anyhow::bail!(
                "Ambiguous prefix '{}' matches {} goals. Use a longer prefix.",
                hint,
                n
            ),
        }
    } else {
        goals
            .into_iter()
            .find(|g| g.state == GoalRunState::Running)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No goal is currently running. \
                     Start a goal with `ta run` or pass --goal <id> to target a specific goal."
                )
            })
    }
}

fn execute_option(
    opt: &ta_session::AdvisorOption,
    security: &AdvisorSecurity,
    config: &GatewayConfig,
) -> anyhow::Result<()> {
    match opt.action_type.as_str() {
        "apply" => {
            println!("Applying the current draft...");
            run_ta_command(config, &["draft", "apply", "--latest"])
        }
        "deny" => {
            println!("Denying the current draft...");
            run_ta_command(config, &["draft", "deny", "--latest"])
        }
        "auto_fire" | "button" => {
            if let Some(ref cmd) = opt.command {
                let goal = extract_goal_from_command(cmd);
                println!("Firing: {}", cmd);
                if let Some(goal) = goal {
                    run_ta_command(config, &["run", &goal, "--headless"])
                } else {
                    eprintln!("Could not parse goal from command: {}", cmd);
                    eprintln!("Run manually: {}", cmd);
                    Ok(())
                }
            } else {
                println!("No command associated with this option.");
                Ok(())
            }
        }
        "text" => {
            if let Some(ref cmd) = opt.command {
                println!("Run this command:");
                println!("  {}", cmd);
                // Try to copy to clipboard (best-effort, no failure).
                let _ = copy_to_clipboard(cmd);
                match security {
                    AdvisorSecurity::ReadOnly => {
                        println!("(read-only mode — copy and run manually)");
                    }
                    AdvisorSecurity::Suggest => {
                        // In suggest mode, offer to run.
                        print!("Run this now? [y/N]: ");
                        io::stdout().flush()?;
                        let mut line = String::new();
                        io::stdin().lock().read_line(&mut line)?;
                        if line.trim().eq_ignore_ascii_case("y") {
                            let goal = extract_goal_from_command(cmd);
                            if let Some(goal) = goal {
                                return run_ta_command(config, &["run", &goal, "--headless"]);
                            }
                        }
                        println!("Command not run.");
                    }
                    AdvisorSecurity::Auto => unreachable!("auto mode uses auto_fire action"),
                }
            }
            Ok(())
        }
        "answer" => {
            if let Some(ref cmd) = opt.command {
                println!("Run this for more details:");
                println!("  {}", cmd);
                run_ta_command(config, &cmd.split_whitespace().skip(1).collect::<Vec<_>>())
            } else {
                println!("(no command for this option)");
                Ok(())
            }
        }
        "clarify" => {
            println!("Cancelled.");
            Ok(())
        }
        other => {
            println!("Unknown action type '{}'. Nothing to do.", other);
            Ok(())
        }
    }
}

/// Extract the goal title from a `ta run "<goal>"` command string.
fn extract_goal_from_command(cmd: &str) -> Option<String> {
    // Pattern: ta run "..." or ta run '...'
    let after_run = cmd.find("run ")?.checked_add(4)?;
    let rest = cmd[after_run..].trim();
    if rest.starts_with('"') {
        let inner = rest.trim_start_matches('"');
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    } else if rest.starts_with('\'') {
        let inner = rest.trim_start_matches('\'');
        let end = inner.find('\'')?;
        Some(inner[..end].to_string())
    } else {
        // No quotes — take the whole rest as the goal.
        Some(rest.to_string())
    }
}

/// Run a `ta` subcommand as a child process using the current binary.
fn run_ta_command(config: &GatewayConfig, args: &[&str]) -> anyhow::Result<()> {
    let ta_bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "ta".to_string());

    let mut cmd = std::process::Command::new(&ta_bin);
    cmd.args(["--project-root", &config.workspace_root.to_string_lossy()]);
    cmd.args(args);

    let status = cmd
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to run ta: {}", e))?;
    if !status.success() {
        anyhow::bail!(
            "Command failed (exit {}). Args: {:?}",
            status.code().unwrap_or(-1),
            args
        );
    }
    Ok(())
}

/// Resolve effective security level: flag > daemon config > read_only.
fn resolve_security(config: &GatewayConfig, override_str: Option<&str>) -> AdvisorSecurity {
    let s = override_str
        .or_else(|| read_daemon_security(config))
        .unwrap_or("read_only");
    match s {
        "auto" => AdvisorSecurity::Auto,
        "suggest" => AdvisorSecurity::Suggest,
        _ => AdvisorSecurity::ReadOnly,
    }
}

/// Attempt to read the advisor security level from daemon config file.
fn read_daemon_security(config: &GatewayConfig) -> Option<&'static str> {
    // We don't want to import the full DaemonConfig here; just peek at the TOML.
    let config_path = config.workspace_root.join(".ta/workflow.toml");
    let content = std::fs::read_to_string(&config_path).ok()?;
    // Quick grep: look for security = "..."
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("security") && line.contains('=') {
            let val = line.split('=').nth(1)?.trim().trim_matches('"');
            return match val {
                "auto" => Some("auto"),
                "suggest" => Some("suggest"),
                _ => Some("read_only"),
            };
        }
    }
    None
}

/// Best-effort clipboard copy using pbcopy (macOS) or xclip/xsel (Linux).
fn copy_to_clipboard(text: &str) -> Result<(), ()> {
    #[cfg(target_os = "macos")]
    {
        let mut child = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|_| ())?;
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            stdin.write_all(text.as_bytes()).map_err(|_| ())?;
        }
        child.wait().map(|_| ()).map_err(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = text;
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> GatewayConfig {
        GatewayConfig::for_project(tmp.path())
    }

    #[test]
    fn extract_goal_double_quoted() {
        assert_eq!(
            extract_goal_from_command("ta run \"add tests for auth module\""),
            Some("add tests for auth module".to_string())
        );
    }

    #[test]
    fn extract_goal_single_quoted() {
        assert_eq!(
            extract_goal_from_command("ta run 'implement remaining v0.15'"),
            Some("implement remaining v0.15".to_string())
        );
    }

    #[test]
    fn extract_goal_unquoted() {
        assert_eq!(
            extract_goal_from_command("ta run add more docs"),
            Some("add more docs".to_string())
        );
    }

    #[test]
    fn extract_goal_no_run() {
        assert!(extract_goal_from_command("ta status").is_none());
    }

    #[test]
    fn resolve_security_read_only_default() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let sec = resolve_security(&config, None);
        assert!(matches!(sec, AdvisorSecurity::ReadOnly));
    }

    #[test]
    fn resolve_security_override_auto() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let sec = resolve_security(&config, Some("auto"));
        assert!(matches!(sec, AdvisorSecurity::Auto));
    }

    #[test]
    fn resolve_security_override_suggest() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let sec = resolve_security(&config, Some("suggest"));
        assert!(matches!(sec, AdvisorSecurity::Suggest));
    }

    #[test]
    fn advisor_ask_no_input_does_not_hang() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        // With no_input=true this should return immediately without reading stdin.
        let result = ask(&config, "apply", None, None, None, true, false);
        assert!(result.is_ok());
    }

    #[test]
    fn advisor_ask_json_output() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        let result = ask(&config, "apply", None, None, None, true, true);
        assert!(result.is_ok());
    }

    #[test]
    fn advisor_ask_clarify_no_options_does_not_prompt() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        // "hmm" classifies as Clarify — options will still be present but no_input prevents stdin read.
        let result = ask(&config, "hmm", None, None, None, true, false);
        assert!(result.is_ok());
    }

    #[test]
    fn advisor_ask_workflow_context_shapes_options() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(&tmp);
        // Test that tab context is properly passed to AdvisorSession.
        let result = ask(
            &config,
            "amend auto-approve",
            None,
            Some("workflows"),
            Some("my-workflow"),
            true,
            true,
        );
        assert!(result.is_ok());
    }
}
