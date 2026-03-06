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

    // Check if command is allowed.
    if !is_command_allowed(command_str, &state.daemon_config.commands.allowed) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "command not in allowlist"})),
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

    let timeout = std::time::Duration::from_secs(state.daemon_config.commands.timeout_secs);

    match run_command(&ta_binary, &args, &state.project_root, timeout).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

async fn run_command(
    binary: &str,
    args: &[&str],
    working_dir: &std::path::Path,
    timeout: std::time::Duration,
) -> Result<CmdResponse, String> {
    let result = tokio::time::timeout(timeout, async {
        tokio::process::Command::new(binary)
            .args(args)
            .arg("--project-root")
            .arg(working_dir)
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
        Ok(Err(e)) => Err(format!("Failed to execute command: {}", e)),
        Err(_) => Err("Command timed out".to_string()),
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
fn is_command_allowed(command: &str, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return true; // No allowlist = allow everything.
    }
    allowlist.iter().any(|pattern| glob_match(pattern, command))
}

/// Check if a command is classified as a write command.
fn is_write_command(command: &str, write_patterns: &[String]) -> bool {
    write_patterns
        .iter()
        .any(|pattern| glob_match(pattern, command))
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
}
