// api/cmd.rs — Command execution endpoint (`POST /api/cmd`).
//
// Executes `ta` CLI commands by forking the `ta` binary and capturing output.
// Commands are validated against the allowlist in daemon.toml.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::{require_write, CallerIdentity};
use crate::api::AppState;

#[derive(Debug, Deserialize)]
pub struct CmdRequest {
    pub command: String,
}

#[derive(Debug, Serialize)]
pub struct CmdResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Output key for a background command (v0.11.7 item 3).
    /// When set, the web shell should auto-tail this key immediately.
    /// Format: the same key passed to `:tail <key>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_key: Option<String>,
}

/// `POST /api/cmd` — Execute a `ta` CLI command.
pub async fn execute_command(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<CallerIdentity>,
    Json(body): Json<CmdRequest>,
) -> impl IntoResponse {
    let command_str = body.command.trim();
    if command_str.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "command is required"})),
        )
            .into_response();
    }

    // Check if command is allowed (deny takes precedence over allow).
    let filter = state.daemon_config.commands.access_filter();
    if !filter.permits(command_str) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "command not permitted (check allowed/denied in daemon.toml)"})),
        )
            .into_response();
    }

    // Check write scope for write commands.
    if is_write_command(command_str, &state.daemon_config.commands.write_commands) {
        if let Err(e) = require_write(&identity) {
            return e.into_response();
        }
    }

    // Parse the command into args. Strip leading "ta " if present.
    let args_str = command_str.strip_prefix("ta ").unwrap_or(command_str);

    let args: Vec<String> = match parse_command_args(args_str) {
        ParseResult::Parsed(args) => args,
        ParseResult::Ambiguous(options) => {
            // Return disambiguation options for the caller to present.
            return Json(serde_json::json!({
                "ambiguous": true,
                "message": "Ambiguous command — did you mean:",
                "options": options.iter().enumerate().map(|(i, opt)| {
                    serde_json::json!({
                        "index": i + 1,
                        "description": opt.description,
                        "command": opt.command,
                    })
                }).collect::<Vec<_>>(),
            }))
            .into_response();
        }
    };
    if args.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "empty command after parsing"})),
        )
            .into_response();
    }

    // Find the `ta` binary. Prefer the one adjacent to the current binary.
    let ta_binary = find_ta_binary();

    // Long-running commands (ta run, ta dev) are spawned in the background.
    // They return immediately with a status message — progress is tracked via
    // events (SSE) and goal status, not by blocking the HTTP response.
    let is_long = is_long_running(command_str, &state.daemon_config.commands.long_running);

    if is_long {
        let binary = ta_binary.clone();
        let working_dir = state.project_root.clone();
        let cmd_str = command_str.to_string();

        // Try to extract the goal ID from args for output streaming.
        // Goal IDs appear in `ta run` output events; we use the command string
        // as a key until the real goal ID is known.
        let goal_output = state.goal_output.clone_ref();
        let events_dir = state.events_dir.clone();
        let output_key = extract_goal_key(&args);
        let tx = goal_output.create_channel(&output_key).await;
        let output_key_display = output_key.clone();
        let output_key_response = output_key.clone();

        tokio::spawn(async move {
            tracing::info!(
                "Background command started: {} (output key: {})",
                cmd_str,
                output_key_display
            );
            // Check agent consent before spawning (v0.10.18.4).
            // If consent is missing, emit an error event instead of silently accepting terms.
            let consent_path = working_dir.join(".ta/consent.json");
            let has_consent = consent_path.exists();

            // Build the command args. Inject --headless for background goals so the
            // agent produces streaming output instead of running silent with piped fds.
            // Pass --accept-terms only if the user has previously given consent via
            // `ta terms accept` (stored in .ta/consent.json).
            let mut cmd_builder = tokio::process::Command::new(&binary);
            cmd_builder.arg("--project-root").arg(&working_dir);

            if has_consent {
                cmd_builder.arg("--accept-terms");
            }

            // Inject --headless so ta run uses launch_agent_headless() with
            // explicit piping and [agent] prefix output (v0.10.18.4 item 1).
            let needs_headless = args
                .first()
                .map(|a| a == "run" || a == "dev")
                .unwrap_or(false)
                && !args.iter().any(|a| a == "--headless");
            if needs_headless {
                // Insert --headless after the subcommand (first arg).
                if let Some(subcmd) = args.first() {
                    cmd_builder.arg(subcmd);
                    cmd_builder.arg("--headless");
                    cmd_builder.args(&args[1..]);
                } else {
                    cmd_builder.args(&args);
                }
            } else {
                cmd_builder.args(&args);
            }

            // Pipe stdin for interactive prompt relay (v0.10.18.5 item 3).
            let goal_input = state.goal_input.clone();
            let output_key_stdin = output_key.clone();

            let result = cmd_builder
                .current_dir(&working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(std::process::Stdio::piped())
                .spawn();

            match result {
                Ok(mut child) => {
                    // Stream stdout and stderr line-by-line, collecting stderr
                    // for failure context.
                    use crate::api::goal_output::OutputLine;
                    use tokio::io::{AsyncBufReadExt, BufReader};

                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();

                    // Store stdin handle for interactive relay (v0.10.18.5 item 3).
                    if let Some(stdin) = child.stdin.take() {
                        goal_input.register(&output_key_stdin, stdin).await;
                    }

                    let tx2 = tx.clone();
                    let tx_bookend = tx.clone();
                    let tx_heartbeat = tx.clone();

                    // Load output schema for parsing stream-json (v0.11.2.2).
                    let output_schema = {
                        let loader = ta_output_schema::SchemaLoader::new(&working_dir);
                        loader
                            .load("claude-code")
                            .unwrap_or_else(|_| ta_output_schema::OutputSchema::passthrough())
                    };
                    let stdout_task = tokio::spawn(async move {
                        if let Some(out) = stdout {
                            let mut reader = BufReader::new(out).lines();
                            while let Ok(Some(line)) = reader.next_line().await {
                                // Schema-driven stream-json parsing (v0.11.2.2).
                                match ta_output_schema::parse_line(&output_schema, &line) {
                                    ta_output_schema::ParseResult::Text(text) => {
                                        let stream = if is_interactive_prompt(&text) {
                                            "prompt"
                                        } else {
                                            "stdout"
                                        };
                                        let _ = tx.send(OutputLine { stream, line: text });
                                    }
                                    ta_output_schema::ParseResult::ToolUse(name) => {
                                        let _ = tx.send(OutputLine {
                                            stream: "stdout",
                                            line: format!("[tool] {}", name),
                                        });
                                    }
                                    ta_output_schema::ParseResult::NotJson => {
                                        // Non-JSON lines: relay as-is.
                                        let stream = if is_interactive_prompt(&line) {
                                            "prompt"
                                        } else {
                                            "stdout"
                                        };
                                        let _ = tx.send(OutputLine { stream, line });
                                    }
                                    ta_output_schema::ParseResult::Model(_)
                                    | ta_output_schema::ParseResult::Suppress => {
                                        // Internal protocol events — skip.
                                    }
                                }
                            }
                        }
                    });

                    let stderr_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
                    let stderr_lines2 = stderr_lines.clone();
                    let goal_output2 = goal_output.clone();
                    let goal_input2 = goal_input.clone();
                    let output_key2 = output_key.clone();
                    // Shared goal UUID detected from sentinel — used by state-poll task.
                    let detected_goal_id: std::sync::Arc<tokio::sync::Mutex<Option<uuid::Uuid>>> =
                        std::sync::Arc::new(tokio::sync::Mutex::new(None));
                    #[allow(unused_variables)]
                    let detected_goal_id2 = detected_goal_id.clone();
                    let events_dir2 = events_dir.clone();
                    let working_dir2 = working_dir.clone();
                    let stderr_task = tokio::spawn(async move {
                        if let Some(err) = stderr {
                            let mut reader = BufReader::new(err).lines();
                            while let Ok(Some(line)) = reader.next_line().await {
                                // Detect goal-started events and register the goal UUID
                                // as an alias so :tail <uuid> and stdin relay resolve correctly.
                                // Uses ta_events::GOAL_STARTED_SENTINEL — must stay in sync with run.rs emitter.
                                if line.contains(ta_events::GOAL_STARTED_SENTINEL) {
                                    if let Some(goal_uuid) = extract_goal_uuid_from_event(&line) {
                                        goal_output2.add_alias(&goal_uuid, &output_key2).await;
                                        goal_input2.add_alias(&goal_uuid, &output_key2).await;
                                        // Emit GoalStarted SSE event so channel plugins see it.
                                        if let Ok(uid) = uuid::Uuid::parse_str(&goal_uuid) {
                                            emit_goal_started_event(
                                                &events_dir2,
                                                uid,
                                                &working_dir2,
                                            );
                                            *detected_goal_id2.lock().await = Some(uid);
                                        }
                                    }
                                }
                                let _ = tx2.send(OutputLine {
                                    stream: "stderr",
                                    line: line.clone(),
                                });
                                // Keep last 20 lines for failure context.
                                let mut lines = stderr_lines2.lock().await;
                                if lines.len() >= 20 {
                                    lines.remove(0);
                                }
                                lines.push(line);
                            }
                        }
                    });
                    // State-poll task: watches GoalRunStore for state transitions and
                    // emits GoalCompleted / ReviewRequested SSE events for channel plugins.
                    let events_dir3 = events_dir.clone();
                    let working_dir3 = working_dir.clone();
                    let poll_done = std::sync::Arc::new(tokio::sync::Notify::new());
                    let poll_done2 = poll_done.clone();
                    let state_poll_task = tokio::spawn(async move {
                        let mut last_state: Option<String> = None;
                        loop {
                            tokio::select! {
                                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                                _ = poll_done2.notified() => {
                                    // One final poll on process exit.
                                }
                            }
                            let goal_id = *detected_goal_id.lock().await;
                            let Some(goal_id) = goal_id else { continue };
                            let goal_dir = working_dir3.join(".ta/goals");
                            let store = ta_goal::store::GoalRunStore::new(&goal_dir);
                            let Ok(store) = store else { continue };
                            let Ok(Some(goal)) = store.get(goal_id) else {
                                continue;
                            };
                            let state_str = goal.state.to_string();
                            if last_state.as_deref() == Some(&state_str) {
                                continue;
                            }
                            last_state = Some(state_str.clone());
                            match state_str.as_str() {
                                "completed" => {
                                    emit_goal_completed_event(&events_dir3, goal_id, &goal.title);
                                }
                                "pr_ready" => {
                                    // Emit ReviewRequested so channel plugins show draft-ready.
                                    let pr_dir = working_dir3.join(".ta/pr_packages");
                                    if let Some(d) = latest_draft_for_goal(&pr_dir, goal_id) {
                                        emit_draft_ready_events(
                                            &events_dir3,
                                            goal_id,
                                            d.id,
                                            &goal.title,
                                            &d.summary,
                                            d.artifact_count,
                                        );
                                    }
                                }
                                "failed" | "denied" => {
                                    emit_goal_failed_event(&events_dir3, goal_id, &goal.title);
                                }
                                _ => {}
                            }
                            // Stop polling terminal states.
                            if matches!(
                                state_str.as_str(),
                                "completed" | "failed" | "denied" | "applied"
                            ) {
                                break;
                            }
                        }
                    });

                    // Heartbeat task: emit periodic "still running" messages when
                    // no output activity for N seconds (v0.11.2.3 item 14).
                    let heartbeat_tx = tx_heartbeat;
                    let heartbeat_interval_secs = state
                        .daemon_config
                        .operations
                        .as_ref()
                        .and_then(|ops| ops.heartbeat_interval_secs)
                        .unwrap_or(10) as u64;
                    let heartbeat_task = tokio::spawn(async move {
                        let mut elapsed: u64 = 0;
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(
                                heartbeat_interval_secs,
                            ))
                            .await;
                            elapsed += heartbeat_interval_secs;
                            let _ = heartbeat_tx.send(OutputLine {
                                stream: "stderr",
                                line: format!("[heartbeat] still running... {}s elapsed", elapsed),
                            });
                        }
                    });

                    let status = child.wait().await;
                    heartbeat_task.abort(); // Stop heartbeat when command exits.
                    poll_done.notify_one(); // Trigger final state poll before aborting.
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    // Give the state poll task a moment for its final check, then abort.
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    state_poll_task.abort();

                    match status {
                        Ok(s) if s.success() => {
                            tracing::info!("Background command completed: {}", cmd_str);
                            // Emit completion bookend (v0.10.18.4 item 11).
                            let _ = tx_bookend.send(OutputLine {
                                stream: "stdout",
                                line: format!("\u{2713} {} completed", cmd_str),
                            });
                        }
                        Ok(s) => {
                            let code = s.code().unwrap_or(-1);
                            let stderr_tail = stderr_lines.lock().await.join("\n");
                            tracing::warn!(
                                "Background command failed (exit {}): {}",
                                code,
                                cmd_str
                            );
                            // Emit failure bookend with context (v0.10.18.4 item 11).
                            let mut bookend =
                                format!("\u{2717} {} failed (exit {})", cmd_str, code);
                            // Append last 10 lines of stderr for context.
                            let tail_lines = stderr_lines.lock().await;
                            let start = tail_lines.len().saturating_sub(10);
                            for stderr_line in &tail_lines[start..] {
                                bookend.push_str(&format!("\n  {}", stderr_line));
                            }
                            let _ = tx_bookend.send(OutputLine {
                                stream: "stderr",
                                line: bookend,
                            });
                            emit_command_failed_event(&events_dir, &cmd_str, code, &stderr_tail);
                        }
                        Err(e) => {
                            tracing::error!("Background command wait error: {} — {}", cmd_str, e);
                            let _ = tx_bookend.send(OutputLine {
                                stream: "stderr",
                                line: format!("\u{2717} {} failed: {}", cmd_str, e),
                            });
                            emit_command_failed_event(&events_dir, &cmd_str, -1, &e.to_string());
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Background command spawn error: {} — {}", cmd_str, e);
                    emit_command_failed_event(
                        &events_dir,
                        &cmd_str,
                        -1,
                        &format!("spawn failed: {}", e),
                    );
                }
            }

            // Clean up output channel and stdin handle.
            goal_output.remove_channel(&output_key).await;
            goal_input.remove(&output_key).await;
        });

        return Json(CmdResponse {
            exit_code: 0,
            stdout: format!(
                "Started in background: {}\nOutput key: {}\nTrack with: ta goal list\nTail output: :tail {}\n",
                command_str, output_key_response, output_key_response
            ),
            stderr: String::new(),
            background_key: Some(output_key_response),
        })
        .into_response();
    }

    let timeout = std::time::Duration::from_secs(state.daemon_config.commands.timeout_secs);

    match run_command(&ta_binary, &args, &state.project_root, timeout).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            let detail = format!(
                "{} (command: \"{}\", timeout: {}s)",
                e, command_str, state.daemon_config.commands.timeout_secs,
            );
            tracing::warn!("Command failed: {}", detail);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": detail})),
            )
                .into_response()
        }
    }
}

/// Run a command with activity-aware timeout.
///
/// The timeout resets whenever stdout or stderr produces output. This means
/// a command that is actively producing progress output (e.g., `draft apply`
/// logging each file) will never time out, while a command that hangs silently
/// will time out after `idle_timeout` seconds of inactivity.
async fn run_command(
    binary: &str,
    args: &[String],
    working_dir: &std::path::Path,
    idle_timeout: std::time::Duration,
) -> Result<CmdResponse, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Check agent consent (v0.10.18.4). Only pass --accept-terms if consent exists.
    let consent_path = working_dir.join(".ta/consent.json");
    let mut cmd_builder = tokio::process::Command::new(binary);
    cmd_builder.arg("--project-root").arg(working_dir);
    if consent_path.exists() {
        cmd_builder.arg("--accept-terms");
    }
    let mut child = cmd_builder
        .args(args)
        .current_dir(working_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to execute '{}': {}. Is the ta binary at the expected path?",
                binary, e
            )
        })?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    // Collect output lines, using a shared "last activity" timestamp to
    // implement activity-aware timeout.
    let last_activity = std::sync::Arc::new(tokio::sync::Mutex::new(tokio::time::Instant::now()));

    let stdout_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let stderr_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));

    let la1 = last_activity.clone();
    let sl = stdout_lines.clone();
    let stdout_task = tokio::spawn(async move {
        if let Some(out) = stdout_pipe {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                *la1.lock().await = tokio::time::Instant::now();
                sl.lock().await.push(line);
            }
        }
    });

    let la2 = last_activity.clone();
    let el = stderr_lines.clone();
    let stderr_task = tokio::spawn(async move {
        if let Some(err) = stderr_pipe {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                *la2.lock().await = tokio::time::Instant::now();
                el.lock().await.push(line);
            }
        }
    });

    // Poll: wait for the child to exit, but check for idle timeout periodically.
    let check_interval = std::time::Duration::from_secs(5);
    let status = loop {
        match tokio::time::timeout(check_interval, child.wait()).await {
            Ok(result) => break result,
            Err(_) => {
                // Check if we've been idle too long.
                let elapsed = last_activity.lock().await.elapsed();
                if elapsed > idle_timeout {
                    // Kill the child and return timeout error (v0.11.4.1 item 3).
                    tracing::warn!(
                        binary = %binary,
                        idle_secs = %elapsed.as_secs(),
                        timeout_secs = %idle_timeout.as_secs(),
                        "Command idle timeout — killing process"
                    );
                    let _ = child.kill().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    let stderr_text = stderr_lines.lock().await.join("\n");
                    let mut msg = format!(
                        "Command timed out after {}s of inactivity ({}s elapsed total). \
                         Configure commands.timeout_secs in .ta/daemon.toml to increase.",
                        idle_timeout.as_secs(),
                        elapsed.as_secs()
                    );
                    if !stderr_text.is_empty() {
                        // Show last few lines of output for context.
                        let tail: Vec<&str> = stderr_text.lines().rev().take(3).collect();
                        msg.push_str("\nLast output:");
                        for line in tail.iter().rev() {
                            msg.push_str(&format!("\n  {}", line));
                        }
                    }
                    return Err(msg);
                }
            }
        }
    };

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    match status {
        Ok(s) => Ok(CmdResponse {
            exit_code: s.code().unwrap_or(-1),
            stdout: stdout_lines.lock().await.join("\n"),
            stderr: stderr_lines.lock().await.join("\n"),
            background_key: None,
        }),
        Err(e) => Err(format!("Command wait error: {}", e)),
    }
}

fn find_ta_binary() -> String {
    // Try adjacent to current binary first.
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let ta_path = dir.join("ta");
            if ta_path.exists() {
                return ta_path.to_string_lossy().to_string();
            }
        }
    }
    // Fall back to PATH.
    "ta".to_string()
}

/// Check if a command matches the allowlist using simple glob patterns.
/// Used in tests only; production code uses `AccessFilter::permits()`.
#[cfg(test)]
fn is_command_allowed(command: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return true; // No allowlist = allow everything.
    }
    allowlist.iter().any(|pattern| glob_match(pattern, command))
}

/// Check if a command is long-running (gets extended timeout).
fn is_long_running(command: &str, long_patterns: &[String]) -> bool {
    long_patterns
        .iter()
        .any(|pattern| glob_match(pattern, command))
}

/// Check if a command is classified as a write command.
fn is_write_command(command: &str, write_patterns: &[String]) -> bool {
    write_patterns
        .iter()
        .any(|pattern| glob_match(pattern, command))
}

/// Extract a key for output streaming from command args.
/// Uses the phase arg (e.g., "v0.9.8.1") or the goal title, falling back to a UUID.
fn extract_goal_key(args: &[String]) -> String {
    // Look for a phase-like argument (vN.N.N pattern) or use the first non-flag arg.
    for arg in args {
        if arg.starts_with("v0.") || arg.starts_with("v1.") {
            return arg.clone();
        }
    }
    // Look for a quoted title or first positional arg after subcommand.
    for (i, arg) in args.iter().enumerate() {
        if i > 0 && !arg.starts_with('-') && !arg.starts_with("--") {
            return arg.clone();
        }
    }
    // Fallback to UUID.
    uuid::Uuid::new_v4().to_string()
}

/// Extract a goal UUID from a `[goal started]` event line.
/// Expected format: `[goal started] "title" (uuid)`
fn extract_goal_uuid_from_event(line: &str) -> Option<String> {
    let paren_start = line.rfind('(')?;
    let paren_end = line.rfind(')')?;
    if paren_end <= paren_start + 1 {
        return None;
    }
    let candidate = &line[paren_start + 1..paren_end];
    // Validate it looks like a UUID (8-4-4-4-12 or at least 8+ hex chars).
    if candidate.len() >= 8 && candidate.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Commands that take a free-form title as their first positional argument.
/// For these, all non-flag words after the subcommand are joined into one arg.
const TITLE_SUBCOMMANDS: &[&str] = &["run", "dev"];

/// Known `--long` flags for title-subcommands, used to distinguish real flags
/// from title text that happens to start with `--`.
const RUN_FLAGS: &[&str] = &[
    "--agent",
    "--source",
    "--objective",
    "--phase",
    "--follow-up",
    "--objective-file",
    "--no-launch",
    "--interactive",
    "--macro-goal",
    "--macro",
    "--resume",
    "--headless",
    "--goal-id",
];

/// Flags that take a value (the next word is consumed as the value, not title text).
const RUN_VALUE_FLAGS: &[&str] = &[
    "--agent",
    "--source",
    "--objective",
    "--phase",
    "--follow-up",
    "--objective-file",
    "--resume",
    "--goal-id",
];

const DEV_FLAGS: &[&str] = &["--agent", "--unrestricted"];
const DEV_VALUE_FLAGS: &[&str] = &["--agent"];

/// Return the known flags for a title subcommand.
fn known_flags_for(subcommand: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match subcommand {
        "run" => (RUN_FLAGS, RUN_VALUE_FLAGS),
        "dev" => (DEV_FLAGS, DEV_VALUE_FLAGS),
        _ => (&[], &[]),
    }
}

/// A single possible interpretation of an ambiguous command.
#[derive(Debug, Clone, PartialEq)]
pub struct Interpretation {
    /// Human-readable description (e.g., `run goal "v0.10.7 -- build Blah" with --agent claude-flow`).
    pub description: String,
    /// The fully-formed command string to execute if this interpretation is chosen.
    pub command: String,
}

/// Result of parsing a command line that may be ambiguous.
#[derive(Debug, PartialEq)]
pub enum ParseResult {
    /// Unambiguous parse — ready to execute.
    Parsed(Vec<String>),
    /// Ambiguous — multiple plausible interpretations for the user to choose from.
    Ambiguous(Vec<Interpretation>),
}

/// Split a string into tokens, respecting double-quoted groups.
/// `run "my title" --flag` → `["run", "my title", "--flag"]`.
fn split_respecting_quotes(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                // Don't include the quote character in the token.
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Parse command args, joining multi-word titles for subcommands that expect them.
///
/// Uses known-flag awareness to distinguish real flags from title text.
/// Detects typos like `-- source .` (should be `--source .`) and ambiguous
/// patterns like `run v0.10.7 -- build Blah --agent claude-flow`.
/// Respects quoted titles: `run "my title" --agent codex` is always unambiguous.
///
/// Returns `Parsed` for unambiguous input, `Ambiguous` with interpretations otherwise.
fn parse_command_args(args_str: &str) -> ParseResult {
    // If the input contains quotes, use quote-aware splitting — always unambiguous.
    if args_str.contains('"') {
        let tokens = split_respecting_quotes(args_str);
        return ParseResult::Parsed(tokens);
    }

    let words: Vec<&str> = args_str.split_whitespace().collect();
    if words.is_empty() {
        return ParseResult::Parsed(Vec::new());
    }

    if !TITLE_SUBCOMMANDS.contains(&words[0]) || words.len() <= 1 {
        return ParseResult::Parsed(words.iter().map(|s| s.to_string()).collect());
    }

    let subcmd = words[0];
    let (all_flags, value_flags) = known_flags_for(subcmd);

    // First pass: check for typos like `-- source .` → `--source .`.
    // If word[i] == "--" and word[i+1] matches a known flag stem, suggest correction.
    let mut typo_corrections: Vec<(usize, String)> = Vec::new();
    for i in 1..words.len() {
        if words[i] == "--" && i + 1 < words.len() {
            let candidate = format!("--{}", words[i + 1]);
            if all_flags.contains(&candidate.as_str()) {
                typo_corrections.push((i, candidate));
            }
        }
    }

    if !typo_corrections.is_empty() {
        return build_typo_disambiguation(
            subcmd,
            &words,
            &typo_corrections,
            all_flags,
            value_flags,
        );
    }

    // Second pass: parse with known-flag awareness.
    let mut title_parts: Vec<&str> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    let mut has_ambiguous = false;
    let mut i = 1;
    while i < words.len() {
        let word = words[i];
        if all_flags.contains(&word) {
            // Known flag.
            flags.push(word.to_string());
            i += 1;
            if value_flags.contains(&word) && i < words.len() {
                flags.push(words[i].to_string());
                i += 1;
            }
        } else if word == "--" {
            // Bare `--` — ambiguous (em-dash? end-of-flags?).
            has_ambiguous = true;
            title_parts.push(word);
            i += 1;
        } else if word.starts_with("--") {
            // Unknown `--something` — could be title text or a typo.
            has_ambiguous = true;
            title_parts.push(word);
            i += 1;
        } else if flags.is_empty() {
            title_parts.push(word);
            i += 1;
        } else {
            // Bare word after flags, not consumed as a flag value — unusual.
            title_parts.push(word);
            i += 1;
        }
    }

    if title_parts.is_empty() {
        return ParseResult::Parsed(words.iter().map(|s| s.to_string()).collect());
    }

    if has_ambiguous && !flags.is_empty() {
        // Multiple plausible parses — disambiguate.
        return build_flag_disambiguation(subcmd, &words, &title_parts, &flags);
    }

    // Unambiguous: join title parts, preserving original whitespace from args_str.
    let title_span = span_from_parts(args_str, &title_parts);
    let mut result = vec![subcmd.to_string(), title_span.to_string()];
    result.extend(flags);
    ParseResult::Parsed(result)
}

/// Extract the contiguous substring from `args_str` that spans from the first
/// to the last part in `parts` (preserving original whitespace between them).
fn span_from_parts<'a>(args_str: &'a str, parts: &[&str]) -> &'a str {
    let first = parts[0];
    let last = parts[parts.len() - 1];
    let start = first.as_ptr() as usize - args_str.as_ptr() as usize;
    let end = last.as_ptr() as usize - args_str.as_ptr() as usize + last.len();
    &args_str[start..end]
}

/// Build disambiguation for typo corrections like `-- source .` → `--source .`.
fn build_typo_disambiguation(
    subcmd: &str,
    words: &[&str],
    corrections: &[(usize, String)],
    all_flags: &[&str],
    value_flags: &[&str],
) -> ParseResult {
    let mut options = Vec::new();

    // Option 1: Literal — everything after subcmd is the title (no flag interpretation).
    let full_title = words[1..].join(" ");
    options.push(Interpretation {
        description: format!("{} goal \"{}\"", subcmd, full_title),
        command: format!("ta {} \"{}\"", subcmd, full_title),
    });

    // Option 2: Corrected — apply typo fixes and re-parse.
    let mut corrected_words: Vec<String> = Vec::new();
    let mut skip_next = false;
    for (idx, word) in words.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if let Some((_, correction)) = corrections.iter().find(|(ci, _)| *ci == idx) {
            corrected_words.push(correction.clone());
            skip_next = true; // Skip the next word (it was merged into the flag).
        } else {
            corrected_words.push(word.to_string());
        }
    }

    // Parse the corrected version to build a proper description.
    let mut title_parts = Vec::new();
    let mut flag_parts: Vec<String> = Vec::new();
    let mut ci = 1;
    while ci < corrected_words.len() {
        let w = &corrected_words[ci];
        if all_flags.contains(&w.as_str()) {
            flag_parts.push(w.clone());
            ci += 1;
            if value_flags.contains(&w.as_str()) && ci < corrected_words.len() {
                flag_parts.push(corrected_words[ci].clone());
                ci += 1;
            }
        } else {
            title_parts.push(w.clone());
            ci += 1;
        }
    }

    let corrected_title = title_parts.join(" ");
    let flag_desc = describe_flags(&flag_parts);
    let mut cmd_parts = vec![format!("ta {}", subcmd)];
    if !corrected_title.is_empty() {
        cmd_parts.push(format!("\"{}\"", corrected_title));
    }
    cmd_parts.extend(flag_parts.iter().cloned());

    let desc = if flag_desc.is_empty() {
        format!("{} goal \"{}\"", subcmd, corrected_title)
    } else {
        format!("{} goal \"{}\" with {}", subcmd, corrected_title, flag_desc)
    };
    options.push(Interpretation {
        description: desc,
        command: cmd_parts.join(" "),
    });

    // Deduplicate.
    options.dedup_by(|a, b| a.command == b.command);

    if options.len() == 1 {
        // Only one interpretation — auto-select it. Re-parse the corrected words.
        let mut result = vec![subcmd.to_string()];
        if !corrected_title.is_empty() {
            result.push(corrected_title);
        }
        result.extend(flag_parts);
        return ParseResult::Parsed(result);
    }

    ParseResult::Ambiguous(options)
}

/// Build disambiguation when unknown `--` tokens appear alongside known flags.
fn build_flag_disambiguation(
    subcmd: &str,
    words: &[&str],
    title_parts: &[&str],
    flags: &[String],
) -> ParseResult {
    let mut options = Vec::new();

    // Option 1: Everything after subcmd is the title.
    let full_title = words[1..].join(" ");
    options.push(Interpretation {
        description: format!("{} goal \"{}\"", subcmd, full_title),
        command: format!("ta {} \"{}\"", subcmd, full_title),
    });

    // Option 2: Title + flags split as parsed.
    let title_text = title_parts.join(" ");
    let flag_desc = describe_flags(flags);
    let mut cmd_parts = vec![format!("ta {} \"{}\"", subcmd, title_text)];
    cmd_parts.extend(flags.iter().cloned());

    options.push(Interpretation {
        description: format!("{} goal \"{}\" with {}", subcmd, title_text, flag_desc),
        command: cmd_parts.join(" "),
    });

    // Deduplicate.
    options.dedup_by(|a, b| a.command == b.command);

    if options.len() == 1 {
        let mut result = vec![subcmd.to_string(), title_text.to_string()];
        result.extend(flags.iter().cloned());
        return ParseResult::Parsed(result);
    }

    ParseResult::Ambiguous(options)
}

/// Produce a human-readable summary of flags (e.g., "--agent set to claude-flow, --headless").
fn describe_flags(flags: &[String]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < flags.len() {
        if flags[i].starts_with("--") && i + 1 < flags.len() && !flags[i + 1].starts_with("--") {
            parts.push(format!("{} set to {}", flags[i], flags[i + 1]));
            i += 2;
        } else {
            parts.push(flags[i].clone());
            i += 1;
        }
    }
    parts.join(", ")
}

/// Simple glob matching: `*` matches any sequence of characters.
fn glob_match(pattern: &str, input: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(" *") {
        input.starts_with(prefix)
    } else if pattern == "*" {
        true
    } else {
        pattern == input
    }
}

// NOTE: parse_stream_json_line() and extract_text_content() removed in v0.11.2.2.
// Replaced by schema-driven ta_output_schema::parse_line(). See agents/output-schemas/*.yaml.

/// Emit a `command_failed` event so the failure is visible to agents and the SSE stream.
fn emit_sse_event(events_dir: &std::path::Path, event: ta_events::schema::SessionEvent) {
    use ta_events::store::{EventStore, FsEventStore};
    let store = FsEventStore::new(events_dir);
    let envelope = ta_events::schema::EventEnvelope::new(event);
    if let Err(e) = store.append(&envelope) {
        tracing::warn!("Failed to emit SSE event: {}", e);
    }
}

fn emit_goal_started_event(
    events_dir: &std::path::Path,
    goal_id: uuid::Uuid,
    project_root: &std::path::Path,
) {
    use ta_events::schema::SessionEvent;
    // Load goal title from store if possible.
    let title = load_goal_title(project_root, goal_id).unwrap_or_else(|| "(goal)".to_string());
    emit_sse_event(
        events_dir,
        SessionEvent::GoalStarted {
            goal_id,
            title,
            agent_id: "ta-run".to_string(),
            phase: None,
        },
    );
}

fn emit_goal_completed_event(events_dir: &std::path::Path, goal_id: uuid::Uuid, title: &str) {
    use ta_events::schema::SessionEvent;
    emit_sse_event(
        events_dir,
        SessionEvent::GoalCompleted {
            goal_id,
            title: title.to_string(),
            duration_secs: None,
        },
    );
}

fn emit_goal_failed_event(events_dir: &std::path::Path, goal_id: uuid::Uuid, _title: &str) {
    use ta_events::schema::SessionEvent;
    emit_sse_event(
        events_dir,
        SessionEvent::GoalFailed {
            goal_id,
            error: "Agent exited with error".to_string(),
            exit_code: None,
        },
    );
}

fn emit_draft_ready_events(
    events_dir: &std::path::Path,
    goal_id: uuid::Uuid,
    draft_id: uuid::Uuid,
    title: &str,
    summary: &str,
    artifact_count: usize,
) {
    use ta_events::schema::SessionEvent;
    emit_sse_event(
        events_dir,
        SessionEvent::DraftBuilt {
            goal_id,
            draft_id,
            artifact_count,
        },
    );
    emit_sse_event(
        events_dir,
        SessionEvent::ReviewRequested {
            goal_id,
            draft_id,
            summary: if summary.is_empty() {
                format!(
                    "Draft ready for '{}' — {} file(s) changed.",
                    title, artifact_count
                )
            } else {
                summary.to_string()
            },
        },
    );
}

fn load_goal_title(project_root: &std::path::Path, goal_id: uuid::Uuid) -> Option<String> {
    let store = ta_goal::store::GoalRunStore::new(project_root.join(".ta/goals")).ok()?;
    store.get(goal_id).ok()?.map(|g| g.title)
}

struct LatestDraft {
    id: uuid::Uuid,
    summary: String,
    artifact_count: usize,
}

fn latest_draft_for_goal(pr_dir: &std::path::Path, goal_id: uuid::Uuid) -> Option<LatestDraft> {
    use ta_changeset::draft_package::DraftPackage;
    let goal_str = goal_id.to_string();
    std::fs::read_dir(pr_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str::<DraftPackage>(&s).ok())
        .filter(|d| d.goal.goal_id == goal_str)
        .max_by_key(|d| d.created_at)
        .map(|d| LatestDraft {
            id: d.package_id,
            summary: d.summary.what_changed.clone(),
            artifact_count: d.changes.artifacts.len(),
        })
}

fn emit_command_failed_event(
    events_dir: &std::path::Path,
    command: &str,
    exit_code: i32,
    stderr_tail: &str,
) {
    use ta_events::schema::SessionEvent;
    use ta_events::store::{EventStore, FsEventStore};

    let store = FsEventStore::new(events_dir);
    let event = SessionEvent::CommandFailed {
        command: command.to_string(),
        exit_code,
        stderr: stderr_tail.to_string(),
    };
    let envelope = ta_events::schema::EventEnvelope::new(event);

    if let Err(e) = store.append(&envelope) {
        tracing::warn!("Failed to emit command_failed event: {}", e);
    }
}

/// Detect whether a line of agent output looks like an interactive prompt (v0.10.18.5 item 4).
///
/// Two-layer heuristic (v0.11.2.5):
///   1. Strong positive signals always match: `[y/N]`, `[Y/n]`, `[yes/no]`, numbered choices.
///   2. Weak signals (`:` suffix, `?` suffix) are rejected if the line looks like code,
///      markdown, or agent progress output rather than a genuine prompt.
fn is_interactive_prompt(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    // ── Layer 1: Strong positive signals (always match) ────────────
    // These patterns are unambiguous interactive prompts.
    if trimmed.contains("[y/N]")
        || trimmed.contains("[Y/n]")
        || trimmed.contains("[yes/no]")
        || trimmed.contains("[Y/N]")
    {
        return true;
    }
    // Numbered choice indicators (e.g., "[1] mesh [2] hierarchical").
    if trimmed.contains("[1]") && trimmed.contains("[2]") {
        return true;
    }

    // ── Rejection filters (v0.11.2.5 Layer 1) ─────────────────────
    // Lines containing these patterns are agent progress/code output, not prompts.

    // Markdown bold (`**word**`).
    if trimmed.contains("**") {
        return false;
    }
    // Backtick-quoted code (`` `word` ``).
    if trimmed.matches('`').count() >= 2 {
        return false;
    }
    // File path separators — agent listing source files.
    if trimmed.contains("/src/")
        || trimmed.contains(".rs")
        || trimmed.contains(".ts")
        || trimmed.contains(".js")
        || trimmed.contains(".py")
    {
        return false;
    }
    // Bracket-prefixed progress lines: [agent], [apply], [info], [tool], etc.
    if trimmed.starts_with('[') && trimmed.contains(']') {
        // Exception: lines that are ONLY a numbered choice like "[1] or [2]:" are allowed.
        // We already matched those above, so reject everything else bracket-prefixed.
        return false;
    }
    // Lines with parentheses followed by colon — code references like `fn foo(bar):`.
    if trimmed.contains('(') && trimmed.contains(')') {
        return false;
    }

    // ── Layer 1: Weak positive signals (with rejection guard) ──────
    // Lines ending with "?" (questions).
    if trimmed.ends_with('?') {
        return true;
    }
    // Lines ending with ":" or ": " (input prompts).
    if trimmed.ends_with(": ") || trimmed.ends_with(':') {
        // Only match short, conversational lines — not log output.
        if trimmed.len() < 120 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_exact() {
        assert!(glob_match("ta status", "ta status"));
        assert!(!glob_match("ta status", "ta plan list"));
    }

    #[test]
    fn glob_match_wildcard_suffix() {
        assert!(glob_match("ta draft *", "ta draft list"));
        assert!(glob_match("ta draft *", "ta draft approve abc123"));
        assert!(!glob_match("ta draft *", "ta goal list"));
    }

    #[test]
    fn glob_match_star_matches_all() {
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn command_allowlist() {
        let allow = vec!["ta draft *".to_string(), "ta status".to_string()];
        assert!(is_command_allowed("ta draft list", &allow));
        assert!(is_command_allowed("ta status", &allow));
        assert!(!is_command_allowed("ta goal start foo", &allow));
    }

    #[test]
    fn empty_allowlist_allows_all() {
        assert!(is_command_allowed("anything", &[]));
    }

    #[test]
    fn write_command_detection() {
        let write = vec![
            "ta draft approve *".to_string(),
            "ta draft deny *".to_string(),
        ];
        assert!(is_write_command("ta draft approve abc", &write));
        assert!(!is_write_command("ta draft list", &write));
    }

    #[test]
    fn access_filter_deny_takes_precedence() {
        use crate::config::CommandConfig;
        let config = CommandConfig {
            allowed: vec!["ta draft *".to_string()],
            denied: vec!["ta draft apply *".to_string()],
            ..Default::default()
        };
        let filter = config.access_filter();
        assert!(filter.permits("ta draft list"));
        assert!(!filter.permits("ta draft apply abc123"));
    }

    #[test]
    fn default_command_config_allows_all() {
        use crate::config::CommandConfig;
        let config = CommandConfig::default();
        let filter = config.access_filter();
        assert!(filter.permits("ta status"));
        assert!(filter.permits("anything"));
    }

    /// Helper to unwrap a Parsed result for simple assertions.
    fn parsed(result: ParseResult) -> Vec<String> {
        match result {
            ParseResult::Parsed(args) => args,
            ParseResult::Ambiguous(opts) => {
                panic!("expected Parsed, got Ambiguous with {} options", opts.len())
            }
        }
    }

    /// Helper to unwrap an Ambiguous result.
    fn ambiguous(result: ParseResult) -> Vec<Interpretation> {
        match result {
            ParseResult::Ambiguous(opts) => opts,
            ParseResult::Parsed(args) => panic!("expected Ambiguous, got Parsed: {:?}", args),
        }
    }

    #[test]
    fn parse_args_run_multi_word_title() {
        let args = parsed(parse_command_args("run v0.10.7 — Documentation Review"));
        assert_eq!(args, vec!["run", "v0.10.7 — Documentation Review"]);
    }

    #[test]
    fn parse_args_run_with_flags() {
        let args = parsed(parse_command_args(
            "run v0.10.7 Fix things --agent codex --headless",
        ));
        assert_eq!(
            args,
            vec![
                "run",
                "v0.10.7 Fix things",
                "--agent",
                "codex",
                "--headless"
            ]
        );
    }

    #[test]
    fn parse_args_run_single_word() {
        let args = parsed(parse_command_args("run v0.10.7"));
        assert_eq!(args, vec!["run", "v0.10.7"]);
    }

    #[test]
    fn parse_args_non_title_command() {
        let args = parsed(parse_command_args("draft list"));
        assert_eq!(args, vec!["draft", "list"]);
    }

    #[test]
    fn parse_args_dev_bare() {
        let args = parsed(parse_command_args("dev"));
        assert_eq!(args, vec!["dev"]);
    }

    #[test]
    fn parse_args_typo_dash_space_source() {
        // `-- source .` should be detected as ambiguous: typo for `--source .`.
        let opts = ambiguous(parse_command_args("run v0.10.7 -- source ."));
        assert!(opts.len() >= 2, "expected at least 2 interpretations");
        // One option should mention --source.
        assert!(
            opts.iter().any(|o| o.command.contains("--source")),
            "expected a --source correction option"
        );
    }

    #[test]
    fn parse_args_ambiguous_unknown_flag_with_known() {
        // `--build` is not a known run flag, but `--agent` is.
        let opts = ambiguous(parse_command_args(
            "run v0.10.7 --build Blah --agent claude-flow",
        ));
        assert!(opts.len() >= 2);
    }

    #[test]
    fn parse_args_bare_double_dash_no_known_flags() {
        // `run v0.10.7 -- build Blah` with no known flags present.
        // Only one interpretation is possible (everything is the title), so
        // it should be Parsed, not Ambiguous.
        let args = parsed(parse_command_args("run v0.10.7 -- build Blah"));
        assert_eq!(args, vec!["run", "v0.10.7 -- build Blah"]);
    }

    #[test]
    fn parse_args_em_dash_unicode() {
        // Real em-dash (—) is not `--`, should always be unambiguous title text.
        let args = parsed(parse_command_args(
            "run v0.10.7 — Documentation Review & Consolidation",
        ));
        assert_eq!(
            args,
            vec!["run", "v0.10.7 — Documentation Review & Consolidation"]
        );
    }

    #[test]
    fn parse_args_quoted_title() {
        // Quoted titles are always unambiguous — no disambiguation needed.
        let args = parsed(parse_command_args(
            "run \"v0.10.7 -- build Blah\" --agent claude-flow",
        ));
        assert_eq!(
            args,
            vec!["run", "v0.10.7 -- build Blah", "--agent", "claude-flow"]
        );
    }

    #[test]
    fn parse_args_quoted_title_with_flags() {
        let args = parsed(parse_command_args(
            "run \"Fix the thing\" --headless --agent codex",
        ));
        assert_eq!(
            args,
            vec!["run", "Fix the thing", "--headless", "--agent", "codex"]
        );
    }

    #[test]
    fn split_quotes_basic() {
        let tokens = split_respecting_quotes("run \"hello world\" --flag");
        assert_eq!(tokens, vec!["run", "hello world", "--flag"]);
    }

    #[test]
    fn extract_goal_uuid_from_goal_started_event() {
        let line =
            r#"[goal started] "v0.10.13 — ta plan add" (492fac59-eda4-4e87-bf65-9e2edd2e70ce)"#;
        assert_eq!(
            extract_goal_uuid_from_event(line),
            Some("492fac59-eda4-4e87-bf65-9e2edd2e70ce".to_string())
        );
    }

    #[test]
    fn extract_goal_uuid_short_id() {
        let line = r#"[goal started] "title" (abcd1234)"#;
        assert_eq!(
            extract_goal_uuid_from_event(line),
            Some("abcd1234".to_string())
        );
    }

    #[test]
    fn extract_goal_uuid_no_parens() {
        assert_eq!(extract_goal_uuid_from_event("no parens here"), None);
    }

    #[test]
    fn extract_goal_uuid_non_hex() {
        let line = r#"[goal started] "title" (not-hex-zzzz)"#;
        assert_eq!(extract_goal_uuid_from_event(line), None);
    }

    /// Validates that the sentinel emitted by `ta run --headless` (run.rs) and the
    /// sentinel scanned by the daemon background runner (cmd.rs) are identical.
    /// If this test fails, the two sites have drifted and tail/auto-tail will break.
    #[test]
    fn goal_started_sentinel_round_trip() {
        let uuid = "492fac59-eda4-4e87-bf65-9e2edd2e70ce";
        let title = "v0.11.4.5 — Shell Large-Paste Compaction";
        // Simulate what run.rs emits in headless mode.
        let emitted = format!(
            "{} \"{}\" ({})",
            ta_events::GOAL_STARTED_SENTINEL,
            title,
            uuid
        );
        // Simulate what cmd.rs scans for.
        assert!(
            emitted.contains(ta_events::GOAL_STARTED_SENTINEL),
            "emitted line must contain GOAL_STARTED_SENTINEL"
        );
        // Simulate uuid extraction.
        assert_eq!(
            extract_goal_uuid_from_event(&emitted),
            Some(uuid.to_string()),
            "UUID must survive the emit→scan round trip"
        );
    }

    // ── v0.11.2.2 schema-driven parsing tests ──────────────────

    fn test_schema() -> ta_output_schema::OutputSchema {
        let loader = ta_output_schema::SchemaLoader::embedded_only();
        loader.load("claude-code").unwrap()
    }

    #[test]
    fn schema_parse_content_block_delta() {
        let schema = test_schema();
        let line = r#"{"type":"content_block_delta","delta":{"text":"incremental text"}}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("incremental text".into())
        );
    }

    #[test]
    fn schema_parse_tool_use() {
        let schema = test_schema();
        let line = r#"{"type":"tool_use","name":"Read"}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::ToolUse("Read".into())
        );
    }

    #[test]
    fn schema_parse_content_block_start_tool() {
        let schema = test_schema();
        let line =
            r#"{"type":"content_block_start","content_block":{"type":"tool_use","name":"Edit"}}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::ToolUse("Edit".into())
        );
    }

    #[test]
    fn schema_parse_result() {
        let schema = test_schema();
        let line = r#"{"type":"result","result":"Task completed"}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("[result] Task completed".into())
        );
    }

    #[test]
    fn schema_parse_suppressed_events() {
        let schema = test_schema();
        assert_eq!(
            ta_output_schema::parse_line(&schema, r#"{"type":"message_start"}"#),
            ta_output_schema::ParseResult::Suppress
        );
        assert_eq!(
            ta_output_schema::parse_line(&schema, r#"{"type":"message_stop"}"#),
            ta_output_schema::ParseResult::Suppress
        );
        assert_eq!(
            ta_output_schema::parse_line(&schema, r#"{"type":"ping"}"#),
            ta_output_schema::ParseResult::Suppress
        );
        assert_eq!(
            ta_output_schema::parse_line(&schema, r#"{"type":"content_block_stop"}"#),
            ta_output_schema::ParseResult::Suppress
        );
    }

    #[test]
    fn schema_parse_non_json_passthrough() {
        let schema = test_schema();
        let line = "[agent] some output text";
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::NotJson
        );
    }

    #[test]
    fn schema_parse_malformed_json() {
        let schema = test_schema();
        let line = "{not valid json";
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::NotJson
        );
    }

    #[test]
    fn schema_parse_content_array() {
        let schema = test_schema();
        let line = r#"{"type":"assistant","content":[{"type":"text","text":"Hello"},{"type":"text","text":" World"}]}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("Hello World".into())
        );
    }

    #[test]
    fn schema_parse_nested_message_content() {
        let schema = test_schema();
        let line = r#"{"type":"assistant","message":{"model":"claude-opus-4-6","content":[{"type":"text","text":"Nested"}]}}"#;
        assert_eq!(
            ta_output_schema::parse_line(&schema, line),
            ta_output_schema::ParseResult::Text("Nested".into())
        );
    }

    // ── v0.10.18.5 tests ──────────────────────────────────────

    #[test]
    fn prompt_detection_yes_no() {
        assert!(is_interactive_prompt("Continue? [y/N]"));
        assert!(is_interactive_prompt("Proceed? [Y/n]"));
        assert!(is_interactive_prompt("Are you sure? [yes/no]"));
        assert!(is_interactive_prompt("Delete all files? [Y/N]"));
    }

    #[test]
    fn prompt_detection_numbered_choices() {
        assert!(is_interactive_prompt(
            "Select topology: [1] mesh [2] hierarchical"
        ));
        assert!(is_interactive_prompt(
            "[1] option A [2] option B [3] option C"
        ));
    }

    #[test]
    fn prompt_detection_question_mark() {
        assert!(is_interactive_prompt("Which database?"));
        assert!(is_interactive_prompt("Enter your name?"));
    }

    #[test]
    fn prompt_detection_colon_suffix() {
        assert!(is_interactive_prompt("Enter cluster name:"));
        assert!(is_interactive_prompt("Username: "));
        assert!(is_interactive_prompt("Password:"));
    }

    #[test]
    fn prompt_detection_not_log_lines() {
        // Regular output lines should NOT be detected as prompts.
        assert!(!is_interactive_prompt("Building project..."));
        assert!(!is_interactive_prompt(
            "INFO: Starting server on port 8080 and listening for connections"
        ));
        assert!(!is_interactive_prompt("Compiling ta-daemon v0.10.18"));
        assert!(!is_interactive_prompt(""));
        // Long log line ending with colon — should be excluded by length heuristic.
        let long_log = format!("INFO: {}: message", "a]".repeat(100));
        assert!(!is_interactive_prompt(&long_log));
    }

    // ── v0.11.2.5 hardened rejection tests ───────────────────────

    #[test]
    fn prompt_detection_rejects_markdown_bold() {
        // Markdown bold — agent listing code locations.
        assert!(!is_interactive_prompt(
            "**API** (crates/ta-daemon/src/api/status.rs):"
        ));
        assert!(!is_interactive_prompt("**Config loaded**:"));
        assert!(!is_interactive_prompt("**Step 2**: Run the migration"));
    }

    #[test]
    fn prompt_detection_rejects_code_backticks() {
        assert!(!is_interactive_prompt("Running `cargo build`:"));
        assert!(!is_interactive_prompt("The `status` field:"));
    }

    #[test]
    fn prompt_detection_rejects_file_paths() {
        assert!(!is_interactive_prompt(
            "Modified crates/ta-daemon/src/api/status.rs:"
        ));
        assert!(!is_interactive_prompt("Checking file.ts:"));
        assert!(!is_interactive_prompt("Error in main.py:"));
        assert!(!is_interactive_prompt("Editing src/lib.rs:"));
    }

    #[test]
    fn prompt_detection_rejects_bracket_prefixed() {
        // Agent progress lines.
        assert!(!is_interactive_prompt("[agent] Config loaded:"));
        assert!(!is_interactive_prompt("[apply] Applying changes:"));
        assert!(!is_interactive_prompt("[info] Processing:"));
        assert!(!is_interactive_prompt("[tool] Read file:"));
    }

    #[test]
    fn prompt_detection_rejects_parenthesized_code_refs() {
        // Code references with parentheses.
        assert!(!is_interactive_prompt("fn main():"));
        assert!(!is_interactive_prompt("execute_command(state):"));
    }

    #[test]
    fn prompt_detection_still_matches_real_prompts() {
        // Ensure hardening didn't break real prompt detection.
        assert!(is_interactive_prompt("Do you want to continue? [y/N]"));
        assert!(is_interactive_prompt("Enter your name:"));
        assert!(is_interactive_prompt("Choose [1] or [2]:"));
        assert!(is_interactive_prompt("Password:"));
        assert!(is_interactive_prompt("Username: "));
        assert!(is_interactive_prompt("Which database?"));
        assert!(is_interactive_prompt("Are you sure?"));
        assert!(is_interactive_prompt(
            "Select topology: [1] mesh [2] hierarchical"
        ));
    }

    #[tokio::test]
    async fn goal_input_manager_lifecycle() {
        use crate::api::goal_output::GoalInputManager;

        let mgr = GoalInputManager::new();

        // Create a mock child process with piped stdin.
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();

        // Register and send input.
        mgr.register("goal-1", stdin).await;
        let result = mgr.send_input("goal-1", "hello").await;
        assert!(result.is_ok());

        // Sending to nonexistent goal fails.
        let result = mgr.send_input("nonexistent", "test").await;
        assert!(result.is_err());

        // Clean up.
        mgr.remove("goal-1").await;
        let result = mgr.send_input("goal-1", "after remove").await;
        assert!(result.is_err());

        // Kill the child process.
        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn goal_input_manager_alias() {
        use crate::api::goal_output::GoalInputManager;

        let mgr = GoalInputManager::new();
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();

        mgr.register("primary-key", stdin).await;
        mgr.add_alias("alias-key", "primary-key").await;

        // Can send via alias.
        let result = mgr.send_input("alias-key", "via alias").await;
        assert!(result.is_ok());

        let _ = child.kill().await;
    }

    // ── Prompt detection tests ────────────────────────────────────

    #[test]
    fn prompt_detects_yes_no_confirmations() {
        assert!(is_interactive_prompt("Continue? [y/N]"));
        assert!(is_interactive_prompt("Overwrite file? [Y/n]"));
        assert!(is_interactive_prompt("Are you sure? [yes/no]"));
        assert!(is_interactive_prompt("Proceed? [Y/N]"));
    }

    #[test]
    fn prompt_detects_numbered_choices() {
        assert!(is_interactive_prompt("[1] mesh  [2] hierarchical"));
    }

    #[test]
    fn prompt_detects_questions() {
        assert!(is_interactive_prompt("What branch should I use?"));
        assert!(is_interactive_prompt("Ready to deploy?"));
    }

    #[test]
    fn prompt_detects_colon_input_lines() {
        assert!(is_interactive_prompt("Enter your name:"));
        assert!(is_interactive_prompt("Password: "));
    }

    #[test]
    fn prompt_rejects_markdown_bold() {
        // Agent status lines like "**API** (crates/ta-daemon/src/api/status.rs):"
        // must NOT trigger prompt detection.
        assert!(!is_interactive_prompt(
            "**API** (crates/ta-daemon/src/api/status.rs):"
        ));
        assert!(!is_interactive_prompt("**Summary**: the changes look good"));
    }

    #[test]
    fn prompt_rejects_code_backticks() {
        assert!(!is_interactive_prompt(
            "Updated `config.toml` with new settings:"
        ));
        assert!(!is_interactive_prompt(
            "The `is_interactive_prompt` function:"
        ));
    }

    #[test]
    fn prompt_rejects_file_paths() {
        assert!(!is_interactive_prompt(
            "Modified apps/ta-cli/src/commands/run.rs:"
        ));
        assert!(!is_interactive_prompt("Check src/lib.ts for the issue:"));
        assert!(!is_interactive_prompt("Updated main.py:"));
    }

    #[test]
    fn prompt_rejects_bracket_prefixed_progress() {
        // Agent progress lines like "[agent] Working..." or "[tool] Edit".
        assert!(!is_interactive_prompt("[agent] Reading file..."));
        assert!(!is_interactive_prompt("[tool] Edit"));
        assert!(!is_interactive_prompt("[apply] Copying artifacts:"));
    }

    #[test]
    fn prompt_rejects_code_references_with_parens() {
        assert!(!is_interactive_prompt("fn launch_agent_headless(config):"));
        assert!(!is_interactive_prompt(
            "Method signature changed (old → new):"
        ));
    }

    #[test]
    fn prompt_rejects_long_lines() {
        let long = format!("This is a very long agent output line that should not be detected as a prompt because it exceeds the length threshold for conversational prompts ending with a colon: {}", "x".repeat(50));
        assert!(!is_interactive_prompt(&long));
    }

    // ── End-to-end: stream-json → schema → prompt classification ──

    #[test]
    fn stream_json_text_not_misclassified_as_prompt() {
        // When stream-json output is properly parsed by the schema,
        // the extracted text should not trigger false prompt detection.
        let parsed_texts = vec![
            "Hello world",                           // assistant text
            "Working on it...",                      // progress text
            "[result] Changes applied successfully", // result
            "[init] model: claude-opus-4-6",         // system init
        ];

        for text in parsed_texts {
            assert!(
                !is_interactive_prompt(text),
                "Schema-parsed text should not be a prompt: {:?}",
                text
            );
        }
    }

    #[test]
    fn tool_use_label_not_misclassified_as_prompt() {
        // The daemon renders tool use as "[tool] Read" — this should not
        // trigger prompt detection because of the bracket-prefix rejection.
        assert!(!is_interactive_prompt("[tool] Read"));
        assert!(!is_interactive_prompt("[tool] Edit"));
        assert!(!is_interactive_prompt("[tool] Bash"));
    }
}
