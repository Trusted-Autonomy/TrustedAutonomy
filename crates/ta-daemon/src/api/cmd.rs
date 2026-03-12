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
            let result = tokio::process::Command::new(&binary)
                .arg("--project-root")
                .arg(&working_dir)
                .arg("--accept-terms")
                .args(&args)
                .current_dir(&working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();

            match result {
                Ok(mut child) => {
                    // Stream stdout and stderr line-by-line, collecting stderr
                    // for failure context.
                    use crate::api::goal_output::OutputLine;
                    use tokio::io::{AsyncBufReadExt, BufReader};

                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();
                    let tx2 = tx.clone();

                    let stdout_task = tokio::spawn(async move {
                        if let Some(out) = stdout {
                            let mut reader = BufReader::new(out).lines();
                            while let Ok(Some(line)) = reader.next_line().await {
                                let _ = tx.send(OutputLine {
                                    stream: "stdout",
                                    line,
                                });
                            }
                        }
                    });

                    let stderr_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
                    let stderr_lines2 = stderr_lines.clone();
                    let goal_output2 = goal_output.clone();
                    let output_key2 = output_key.clone();
                    let stderr_task = tokio::spawn(async move {
                        if let Some(err) = stderr {
                            let mut reader = BufReader::new(err).lines();
                            while let Ok(Some(line)) = reader.next_line().await {
                                // Detect [goal started] events and register the goal UUID
                                // as an alias so :tail <uuid> resolves to this channel.
                                if line.contains("[goal started]") {
                                    if let Some(goal_uuid) = extract_goal_uuid_from_event(&line) {
                                        goal_output2.add_alias(&goal_uuid, &output_key2).await;
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

                    let status = child.wait().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;

                    match status {
                        Ok(s) if s.success() => {
                            tracing::info!("Background command completed: {}", cmd_str);
                        }
                        Ok(s) => {
                            let code = s.code().unwrap_or(-1);
                            let stderr_tail = stderr_lines.lock().await.join("\n");
                            tracing::warn!(
                                "Background command failed (exit {}): {}",
                                code,
                                cmd_str
                            );
                            emit_command_failed_event(&events_dir, &cmd_str, code, &stderr_tail);
                        }
                        Err(e) => {
                            tracing::error!("Background command wait error: {} — {}", cmd_str, e);
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

            // Clean up the output channel.
            goal_output.remove_channel(&output_key).await;
        });

        return Json(CmdResponse {
            exit_code: 0,
            stdout: format!(
                "Started in background: {}\nOutput key: {}\nTrack progress with: ta goal list\nStream output with: :tail {}\n",
                command_str, output_key_response, &output_key_response[..8.min(output_key_response.len())]
            ),
            stderr: String::new(),
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

    let mut child = tokio::process::Command::new(binary)
        .arg("--project-root")
        .arg(working_dir)
        .arg("--accept-terms")
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
                    // Kill the child and return timeout error.
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

/// Emit a `command_failed` event so the failure is visible to agents and the SSE stream.
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
}
