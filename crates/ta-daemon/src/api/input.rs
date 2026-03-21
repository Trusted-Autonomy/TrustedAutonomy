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
                parallel: false,
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

    // Check for operational intent patterns (v0.13.1.6 item 5).
    // Natural language operational queries map to specific TA commands.
    if let Some(cmd) = resolve_operational_intent(text) {
        return RouteDecision::Command(cmd);
    }

    // No route matched — send to agent.
    RouteDecision::Agent(text.to_string())
}

/// Resolve natural-language operational intent to a concrete `ta` command.
///
/// Common patterns users type in the shell that have deterministic answers:
/// "what's stuck?" → ta goal list (filtered to stuck)
/// "clean up" / "clean old goals" → ta gc
/// "health" / "is the daemon ok?" → ta status --deep
/// "show notifications" → ta operations log
/// "list runbooks" → ta runbook list
///
/// Unrecognised patterns return None and fall through to the agent.
pub fn resolve_operational_intent(text: &str) -> Option<String> {
    let lower = text.trim().to_lowercase();

    // "what's stuck?" / "stuck goals" / "any stuck?"
    if lower.contains("stuck")
        && (lower.contains("what")
            || lower.contains("goal")
            || lower.contains("any")
            || lower == "stuck?")
    {
        return Some("ta goal list".to_string());
    }

    // "clean up" / "clean old goals" / "cleanup"
    if (lower.starts_with("clean") || lower.contains("clean up"))
        && (lower.contains("goal")
            || lower.contains("old")
            || lower.contains("stale")
            || lower == "clean up"
            || lower == "cleanup")
    {
        return Some("ta gc --dry-run".to_string());
    }

    // "disk" / "disk space" / "disk usage"
    if lower.contains("disk")
        && (lower.contains("space")
            || lower.contains("usage")
            || lower.contains("free")
            || lower.contains("pressure")
            || lower == "disk")
    {
        return Some("ta status --deep".to_string());
    }

    // "health" / "is everything ok?" / "daemon ok?" / "daemon health"
    if (lower.contains("health") || lower.contains("everything ok") || lower.contains("all ok"))
        && (lower.contains("daemon")
            || lower.contains("everything")
            || lower.contains("all")
            || lower == "health")
    {
        return Some("ta status --deep".to_string());
    }

    // "notifications" / "show notifications" / "any alerts?"
    if lower.contains("notification") || (lower.contains("alert") && lower.contains("any")) {
        return Some("ta operations log".to_string());
    }

    // "show runbooks" / "list runbooks" / "what runbooks"
    if (lower.contains("runbook") || lower.contains("runbooks"))
        && (lower.contains("list")
            || lower.contains("show")
            || lower.contains("what")
            || lower.contains("available"))
    {
        return Some("ta runbook list".to_string());
    }

    // "pending drafts" / "drafts to review" / "what needs review?"
    if (lower.contains("draft") || lower.contains("review"))
        && (lower.contains("pending")
            || lower.contains("need")
            || lower.contains("waiting")
            || lower.contains("what"))
    {
        return Some("ta draft list".to_string());
    }

    // "active goals" / "running goals" / "what's running?"
    if (lower.contains("running") || lower.contains("active")) && lower.contains("goal") {
        return Some("ta goal list".to_string());
    }

    None
}

fn expand_shortcut(text: &str, shortcuts: &[ShortcutEntry]) -> Option<String> {
    let first_word = text.split_whitespace().next()?;
    for shortcut in shortcuts {
        if first_word == shortcut.r#match {
            let rest = text[first_word.len()..].trim_start();
            if rest.is_empty() {
                return Some(shortcut.expand.clone());
            } else if shortcut.bare_only {
                // Don't expand — fall through so `ta_subcommands` can route
                // `<match> <rest>` as `ta <match> <rest>` instead.
                return None;
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
    fn shortcut_release_with_subcommand_falls_through() {
        // bare_only: release with args falls through to ta_subcommands → ta release <args>
        let config = default_config();
        match route_input("release run --yes", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta release run --yes"),
            _ => panic!("expected Command from ta_subcommands"),
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
    fn plan_bare_expands_to_list() {
        let config = default_config();
        match route_input("plan", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta plan list"),
            _ => panic!("expected Command from plan shortcut"),
        }
    }

    #[test]
    fn plan_status_routes_correctly() {
        // bare_only: `plan status` must not become `ta plan list status`
        let config = default_config();
        match route_input("plan status", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta plan status"),
            _ => panic!("expected Command: ta plan status"),
        }
    }

    #[test]
    fn plan_list_routes_correctly() {
        let config = default_config();
        match route_input("plan list", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta plan list"),
            _ => panic!("expected Command: ta plan list"),
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

    // -- v0.11.4.1 tests: routing reliability --

    #[test]
    fn draft_apply_routes_to_command() {
        // Item 1: Verify `draft apply <id>` routes to Command, not Agent.
        let config = default_config();
        match route_input("draft apply abc123", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft apply abc123"),
            _ => panic!("expected Command for 'draft apply'"),
        }
    }

    #[test]
    fn draft_view_routes_to_command() {
        let config = default_config();
        match route_input("draft view abc123", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft view abc123"),
            _ => panic!("expected Command for 'draft view'"),
        }
    }

    #[test]
    fn draft_approve_routes_to_command() {
        let config = default_config();
        match route_input("draft approve abc123", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft approve abc123"),
            _ => panic!("expected Command for 'draft approve'"),
        }
    }

    #[test]
    fn draft_deny_routes_to_command() {
        let config = default_config();
        match route_input("draft deny abc123", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft deny abc123"),
            _ => panic!("expected Command for 'draft deny'"),
        }
    }

    #[test]
    fn apply_shortcut_routes_to_command() {
        // Item 1: The "apply" shortcut expands to "ta draft apply".
        let config = default_config();
        match route_input("apply abc123", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft apply abc123"),
            _ => panic!("expected Command from 'apply' shortcut"),
        }
    }

    #[test]
    fn view_shortcut_routes_to_command() {
        let config = default_config();
        match route_input("view abc123", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta draft view abc123"),
            _ => panic!("expected Command from 'view' shortcut"),
        }
    }

    // -- v0.13.1.6 tests: operational intent routing --

    #[test]
    fn intent_stuck_goals() {
        let result = resolve_operational_intent("what's stuck?");
        assert_eq!(result, Some("ta goal list".to_string()));
    }

    #[test]
    fn intent_stuck_goals_variant() {
        let result = resolve_operational_intent("any stuck goals?");
        assert_eq!(result, Some("ta goal list".to_string()));
    }

    #[test]
    fn intent_clean_up() {
        let result = resolve_operational_intent("clean up old goals");
        assert_eq!(result, Some("ta gc --dry-run".to_string()));
    }

    #[test]
    fn intent_disk_space() {
        let result = resolve_operational_intent("how much disk space is left?");
        assert_eq!(result, Some("ta status --deep".to_string()));
    }

    #[test]
    fn intent_health() {
        let result = resolve_operational_intent("daemon health");
        assert_eq!(result, Some("ta status --deep".to_string()));
    }

    #[test]
    fn intent_notifications() {
        let result = resolve_operational_intent("show notifications");
        assert_eq!(result, Some("ta operations log".to_string()));
    }

    #[test]
    fn intent_runbook_list() {
        let result = resolve_operational_intent("list available runbooks");
        assert_eq!(result, Some("ta runbook list".to_string()));
    }

    #[test]
    fn intent_pending_drafts() {
        let result = resolve_operational_intent("what drafts need review?");
        assert_eq!(result, Some("ta draft list".to_string()));
    }

    #[test]
    fn intent_active_goals() {
        let result = resolve_operational_intent("show running goals");
        assert_eq!(result, Some("ta goal list".to_string()));
    }

    #[test]
    fn intent_unrecognized_goes_to_agent() {
        // Free-form questions that don't match operational patterns → None.
        let result = resolve_operational_intent("what's the meaning of life?");
        assert_eq!(result, None);
    }

    #[test]
    fn intent_routes_via_route_input() {
        // Verify operational intents actually get routed to Command (not Agent).
        let config = default_config();
        match route_input("what's stuck?", &config) {
            RouteDecision::Command(cmd) => assert_eq!(cmd, "ta goal list"),
            _ => panic!("expected Command from intent routing"),
        }
    }
}
