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

    let args: Vec<&str> = args_str.split_whitespace().collect();
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
        let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let working_dir = state.project_root.clone();
        let cmd_str = command_str.to_string();

        // Try to extract the goal ID from args for output streaming.
        // Goal IDs appear in `ta run` output events; we use the command string
        // as a key until the real goal ID is known.
        let goal_output = state.goal_output.clone_ref();
        let output_key = extract_goal_key(&args_owned);
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
                .args(&args_owned)
                .current_dir(&working_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();

            match result {
                Ok(mut child) => {
                    // Stream stdout and stderr line-by-line.
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

                    let stderr_task = tokio::spawn(async move {
                        if let Some(err) = stderr {
                            let mut reader = BufReader::new(err).lines();
                            while let Ok(Some(line)) = reader.next_line().await {
                                let _ = tx2.send(OutputLine {
                                    stream: "stderr",
                                    line,
                                });
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
                            tracing::warn!(
                                "Background command failed (exit {}): {}",
                                code,
                                cmd_str
                            );
                        }
                        Err(e) => {
                            tracing::error!("Background command wait error: {} — {}", cmd_str, e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Background command spawn error: {} — {}", cmd_str, e);
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

async fn run_command(
    binary: &str,
    args: &[&str],
    working_dir: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<CmdResponse, String> {
    // Global args (--project-root, --accept-terms) must come before the subcommand.
    let result = tokio::time::timeout(timeout, async {
        tokio::process::Command::new(binary)
            .arg("--project-root")
            .arg(working_dir)
            .arg("--accept-terms")
            .args(args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
    })
    .await;

    match result {
        Ok(Ok(output)) => Ok(CmdResponse {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }),
        Ok(Err(e)) => Err(format!(
            "Failed to execute '{}': {}. Is the ta binary at the expected path?",
            binary, e
        )),
        Err(_) => Err(format!(
            "Command timed out after {}s. For long-running commands like 'ta run' or 'ta dev', \
             configure [commands].long_timeout_secs in .ta/daemon.toml or run directly outside the shell.",
            timeout.as_secs()
        )),
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
}
