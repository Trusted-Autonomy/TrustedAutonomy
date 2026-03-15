// api/input.rs — Unified `/api/input` endpoint with routing dispatch.
//
// Clients send raw text; the daemon checks the routing table (shell.toml)
// and dispatches to /api/cmd or /api/agent/ask accordingly.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::CallerIdentity;
use crate::api::AppState;
use crate::config::{ShellConfig, ShortcutEntry};

#[derive(Debug, Deserialize)]
pub struct InputRequest {
    pub text: String,
    /// Agent session ID for agent-routed input.
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InputResponse {
    /// How the input was routed.
    pub routed_to: String,
    /// The result payload.
    pub result: serde_json::Value,
}

/// Routing decision for an input string.
#[derive(Debug)]
pub enum RouteDecision {
    /// Route to command execution.
    Command(String),
    /// Route to agent.
    Agent(String),
}

/// `POST /api/input` — Unified input endpoint.
pub async fn handle_input(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<CallerIdentity>,
    Json(body): Json<InputRequest>,
) -> impl IntoResponse {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "text is required"})),
        )
            .into_response();
    }

    let shell_config = &state.shell_config;

    match route_input(&text, shell_config) {
        RouteDecision::Command(cmd) => {
            // Dispatch to command execution.
            let cmd_req = super::cmd::CmdRequest {
                command: cmd.clone(),
            };
            let cmd_state = State(state.clone());
            let cmd_identity = Extension(identity);

            // Re-use the cmd handler logic inline.
            let response =
                super::cmd::execute_command(cmd_state, cmd_identity, Json(cmd_req)).await;
            response.into_response()
        }
        RouteDecision::Agent(prompt) => {
            // Route to agent session. Auto-create one if none exists.
            let session_id = if let Some(id) = body.session_id.filter(|s| !s.is_empty()) {
                id
            } else {
                // Get or create a Q&A session using the qa_agent (not default_agent).
                // qa_agent is for interactive prompts (claude-code); default_agent is
                // for goal execution frameworks (claude-flow). See v0.10.19 item 1.
                match state
                    .agent_sessions
                    .get_or_create_default(&state.daemon_config.agent.qa_agent)
                    .await
                {
                    Ok(session) => session.session_id,
                    Err(e) => {
                        return Json(InputResponse {
                            routed_to: "agent".to_string(),
                            result: serde_json::json!({"error": e}),
                        })
                        .into_response();
                    }
                }
            };

            let ask_req = super::agent::AskRequest {
                session_id: session_id.clone(),
                prompt,
            };
            let response = super::agent::ask_agent(
                State(state.clone()),
                Extension(identity.clone()),
                Json(ask_req),
            )
            .await;
            response.into_response()
        }
    }
}

/// `GET /api/routes` — Return available routes and shortcuts for tab completion.
pub async fn list_routes(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let config = &state.shell_config;
    Json(serde_json::json!({
        "routes": config.routes.iter().map(|r| serde_json::json!({
            "prefix": r.prefix,
            "command": r.command,
        })).collect::<Vec<_>>(),
        "shortcuts": config.shortcuts.iter().map(|s| serde_json::json!({
            "match": s.r#match,
            "expand": s.expand,
        })).collect::<Vec<_>>(),
    }))
}

/// Route input text using the shell config's routes and shortcuts.
pub fn route_input(text: &str, config: &ShellConfig) -> RouteDecision {
    // First, check shortcuts: if the first word matches a shortcut, expand it.
    let expanded = expand_shortcut(text, &config.shortcuts);
    let text = expanded.as_deref().unwrap_or(text);

    // Check routes: if input matches a route prefix, it's a command.
    for route in &config.routes {
        if text.starts_with(&route.prefix) {
            let cmd = if route.strip_prefix {
                let rest = &text[route.prefix.len()..];
                if route.args.is_empty() {
                    format!("{} {}", route.command, rest)
                } else {
                    format!("{} {} {}", route.command, route.args.join(" "), rest)
                }
            } else {
                format!("{} {}", route.command, text)
            };
            return RouteDecision::Command(cmd.trim().to_string());
        }
    }

    // Check if the first word is a known `ta` subcommand.
    // This lets users type `run ...` instead of `ta run ...`.
    if let Some(first_word) = text.split_whitespace().next() {
        if config.ta_subcommands.iter().any(|s| s == first_word) {
            return RouteDecision::Command(format!("ta {}", text));
        }
    }

    // No route matched — send to agent.
    RouteDecision::Agent(text.to_string())
}

fn expand_shortcut(text: &str, shortcuts: &[ShortcutEntry]) -> Option<String> {
    let first_word = text.split_whitespace().next()?;
    for shortcut in shortcuts {
        if first_word == shortcut.r#match {
            let rest = text[first_word.len()..].trim_start();
            if rest.is_empty() {
                return Some(shortcut.expand.clone());
            } else {
                return Some(format!("{} {}", shortcut.expand, rest));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ShellConfig {
        ShellConfig::default()
    }

    #[test]
    fn route_ta_command() {
        let config = default_config();
        match route_input("ta draft list", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft list"),
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn route_git_command() {
        let config = default_config();
        match route_input("git status", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "git status"),
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn route_shell_escape() {
        let config = default_config();
        match route_input("!ls -la", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "sh -c ls -la"),
            _ => panic!("expected Command"),
        }
    }

    #[test]
    fn shortcut_expansion() {
        let config = default_config();
        match route_input("approve abc123", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft approve abc123"),
            _ => panic!("expected Command from shortcut"),
        }
    }

    #[test]
    fn shortcut_no_args() {
        let config = default_config();
        match route_input("status", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta status"),
            _ => panic!("expected Command from shortcut"),
        }
    }

    #[test]
    fn shortcut_drafts() {
        let config = default_config();
        match route_input("drafts", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft list"),
            _ => panic!("expected Command from shortcut"),
        }
    }

    #[test]
    fn shortcut_release() {
        let config = default_config();
        match route_input("release v0.10.6", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta release run --yes v0.10.6"),
            _ => panic!("expected Command from release shortcut"),
        }
    }

    #[test]
    fn shortcut_release_no_version() {
        let config = default_config();
        match route_input("release", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta release run --yes"),
            _ => panic!("expected Command from release shortcut"),
        }
    }

    #[test]
    fn bare_subcommand_routes_to_ta() {
        let config = default_config();
        // `run` is a ta subcommand — should auto-prefix with `ta `.
        match route_input("run v0.10.7 — Documentation Review", &config) {
            RouteDecision::Command(cmd) => {
                assert_eq!(cmd, "ta run v0.10.7 — Documentation Review")
            }
            _ => panic!("expected Command from subcommand match"),
        }
    }

    #[test]
    fn bare_subcommand_dev() {
        let config = default_config();
        match route_input("dev", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta dev"),
            _ => panic!("expected Command from subcommand match"),
        }
    }

    #[test]
    fn bare_subcommand_goal_with_args() {
        let config = default_config();
        match route_input("goal list", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta goal list"),
            _ => panic!("expected Command from subcommand match"),
        }
    }

    #[test]
    fn unknown_word_routes_to_agent() {
        let config = default_config();
        // Not a subcommand — should go to agent.
        match route_input("What should we work on next?", &config) {
            RouteDecision::Agent(prompt) => {
                assert_eq!(prompt, "What should we work on next?");
            }
            _ => panic!("expected Agent"),
        }
    }
}
