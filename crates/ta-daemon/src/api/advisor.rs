// api/advisor.rs — Studio Advisor API (v0.15.21).
//
// Replaces the QA agent pattern in Studio with an opinionated, human-side
// advisor. The advisor classifies intent on each message and responds
// according to the configured security level.
//
// Endpoints:
//   POST /api/advisor/message  — classify intent and return action
//   GET  /api/advisor/tools    — list available tools by security level
//   GET  /api/advisor/config   — return current advisor config

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::AppState;
use ta_session::classify_intent;
use ta_session::Intent;

// ── Request / Response types ──────────────────────────────────────────────────

/// Request body for `POST /api/advisor/message`.
#[derive(Debug, Deserialize)]
pub struct MessageRequest {
    /// The human's message text.
    pub message: String,
    /// Optional security level override for this request.
    /// Overrides the daemon config for this call only.
    #[serde(default)]
    pub security_override: Option<String>,
}

/// The action the Studio UI should take based on the classified intent.
#[derive(Debug, Serialize)]
pub struct AdvisorAction {
    /// Action type:
    /// - `"text"`: show the command as copyable text (read_only mode)
    /// - `"button"`: render as a clickable "Run this" button (suggest mode)
    /// - `"auto_fire"`: advisor determined it should fire — Studio calls /api/goal/start
    /// - `"apply"`: human approved; Studio should apply the current draft
    /// - `"deny"`: human declined; Studio should deny the current draft
    /// - `"answer"`: forward to agent for a question answer
    /// - `"clarify"`: advisor needs more information
    #[serde(rename = "type")]
    pub action_type: String,
    /// Human-readable label for buttons.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The exact `ta run "..."` command to show or fire (set for GoalRun intents).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Response from `POST /api/advisor/message`.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    /// Classified intent.
    pub intent: String,
    /// Confidence score [0.0, 1.0].
    pub confidence: f32,
    /// Extracted goal prompt for GoalRun intents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_goal: Option<String>,
    /// Action the Studio should take.
    pub action: AdvisorAction,
    /// Human-readable advisor response text shown in the chat pane.
    pub response: String,
}

/// Response from `GET /api/advisor/tools`.
#[derive(Debug, Serialize)]
pub struct ToolsResponse {
    pub security: String,
    pub tools: Vec<AdvisorTool>,
}

/// A single tool available to the advisor at the given security level.
#[derive(Debug, Serialize)]
pub struct AdvisorTool {
    pub name: String,
    pub description: String,
    pub read_only: bool,
}

/// Response from `GET /api/advisor/config`.
#[derive(Debug, Serialize)]
pub struct AdvisorConfigResponse {
    /// Current security level.
    pub security: String,
    /// Human-readable description of what the advisor can do.
    pub description: String,
}

// ── Security level resolution ─────────────────────────────────────────────────

/// Resolve the effective security level string from the request (override) or config.
fn resolve_security(state: &AppState, override_str: Option<&str>) -> String {
    override_str
        .unwrap_or(state.daemon_config.shell.advisor.security.as_str())
        .to_string()
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `POST /api/advisor/message` — Classify intent and return advisor action.
///
/// The advisor is explicitly on the human's side: it interprets their intent,
/// presents commands at the right escalation level, and flags risks.
pub async fn handle_message(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MessageRequest>,
) -> impl IntoResponse {
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "message is required"})),
        )
            .into_response();
    }

    let security = resolve_security(&state, body.security_override.as_deref());
    let result = classify_intent(&message);

    let (action, response) = build_action_and_response(&result, &security);

    Json(MessageResponse {
        intent: intent_to_str(&result.intent),
        confidence: result.confidence,
        extracted_goal: result.extracted_goal.clone(),
        action,
        response,
    })
    .into_response()
}

/// Build the AdvisorAction and human-readable response for the classified intent.
fn build_action_and_response(
    result: &ta_session::IntentResult,
    security: &str,
) -> (AdvisorAction, String) {
    match &result.intent {
        Intent::GoalRun => {
            let goal = result
                .extracted_goal
                .as_deref()
                .unwrap_or("the requested change");
            let command = format!("ta run \"{}\"", goal);

            match security {
                "auto" if result.is_auto_actionable() => {
                    let action = AdvisorAction {
                        action_type: "auto_fire".to_string(),
                        label: Some("Run goal".to_string()),
                        command: Some(command.clone()),
                    };
                    let resp = format!(
                        "Intent: run a goal (confidence {:.0}%). Firing: `{}`",
                        result.confidence * 100.0,
                        command
                    );
                    (action, resp)
                }
                "suggest" => {
                    let action = AdvisorAction {
                        action_type: "button".to_string(),
                        label: Some("Run this goal".to_string()),
                        command: Some(command.clone()),
                    };
                    let resp = format!(
                        "I understood this as a goal request. Click the button to run: `{}`",
                        command
                    );
                    (action, resp)
                }
                _ => {
                    // read_only (default)
                    let action = AdvisorAction {
                        action_type: "text".to_string(),
                        label: None,
                        command: Some(command.clone()),
                    };
                    let resp = format!(
                        "I understood this as a goal request. Run this command to proceed:\n```\n{}\n```",
                        command
                    );
                    (action, resp)
                }
            }
        }

        Intent::Apply => {
            let (action_type, resp) = match security {
                "auto" | "suggest" => (
                    "apply".to_string(),
                    "Approval noted. Studio should apply the current draft.".to_string(),
                ),
                _ => (
                    "apply".to_string(),
                    "To apply the draft, run `ta draft apply <id>` or use the Studio review panel."
                        .to_string(),
                ),
            };
            (
                AdvisorAction {
                    action_type,
                    label: None,
                    command: None,
                },
                resp,
            )
        }

        Intent::Deny => (
            AdvisorAction {
                action_type: "deny".to_string(),
                label: None,
                command: None,
            },
            "Understood — the draft will be marked as denied.".to_string(),
        ),

        Intent::Question => (
            AdvisorAction {
                action_type: "answer".to_string(),
                label: None,
                command: None,
            },
            format!(
                "I'll look into that for you (confidence {:.0}%).",
                result.confidence * 100.0
            ),
        ),

        Intent::Clarify => (
            AdvisorAction {
                action_type: "clarify".to_string(),
                label: None,
                command: None,
            },
            "I'm not sure what you'd like me to do. Could you be more specific? For example: \
             \"apply\", \"skip\", \"run <goal>\", or ask a question about the changes."
                .to_string(),
        ),
    }
}

fn intent_to_str(intent: &Intent) -> String {
    match intent {
        Intent::GoalRun => "goal_run",
        Intent::Question => "question",
        Intent::Clarify => "clarify",
        Intent::Apply => "apply",
        Intent::Deny => "deny",
    }
    .to_string()
}

/// `GET /api/advisor/tools` — List available tools at the current security level.
pub async fn get_tools(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let security = state.daemon_config.shell.advisor.security.clone();
    let tools = available_tools(&security);
    Json(ToolsResponse { security, tools }).into_response()
}

/// Return the tools available at the given security level.
///
/// `auto` / `suggest`: includes `ta_goal_start` and `ta_draft_list`.
/// `read_only` (default): read-only tools only.
fn available_tools(security: &str) -> Vec<AdvisorTool> {
    let read_only_tools = vec![
        AdvisorTool {
            name: "ta_draft_view".to_string(),
            description: "View a draft package and its changes".to_string(),
            read_only: true,
        },
        AdvisorTool {
            name: "ta_plan_status".to_string(),
            description: "Show plan phase status and progress".to_string(),
            read_only: true,
        },
        AdvisorTool {
            name: "ta_fs_read".to_string(),
            description: "Read file contents from the workspace".to_string(),
            read_only: true,
        },
    ];

    match security {
        "auto" | "suggest" => {
            let mut tools = read_only_tools;
            tools.push(AdvisorTool {
                name: "ta_goal_start".to_string(),
                description: "Start a new goal run (requires human confirmation in suggest mode)"
                    .to_string(),
                read_only: false,
            });
            tools.push(AdvisorTool {
                name: "ta_draft_list".to_string(),
                description: "List pending drafts awaiting review".to_string(),
                read_only: true,
            });
            tools
        }
        _ => read_only_tools,
    }
}

/// `GET /api/advisor/config` — Return current advisor configuration.
pub async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let security = &state.daemon_config.shell.advisor.security;
    let description = match security.as_str() {
        "auto" => "Advisor may fire goals automatically at ≥80% intent confidence.",
        "suggest" => "Advisor presents goal commands as clickable buttons for human confirmation.",
        _ => "Advisor answers questions and shows commands as copyable text only.",
    };
    Json(AdvisorConfigResponse {
        security: security.clone(),
        description: description.to_string(),
    })
    .into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ta_session::IntentResult;

    fn make_goal_result(confidence: f32) -> ta_session::IntentResult {
        IntentResult::new(Intent::GoalRun, confidence).with_goal("add tests for the auth module")
    }

    #[test]
    fn read_only_goal_run_returns_text_action() {
        let result = make_goal_result(0.85);
        let (action, response) = build_action_and_response(&result, "read_only");
        assert_eq!(action.action_type, "text");
        assert!(action.command.as_deref().unwrap().starts_with("ta run"));
        assert!(response.contains("ta run"));
        assert!(action.label.is_none());
    }

    #[test]
    fn suggest_goal_run_returns_button_action() {
        let result = make_goal_result(0.85);
        let (action, response) = build_action_and_response(&result, "suggest");
        assert_eq!(action.action_type, "button");
        assert_eq!(action.label.as_deref(), Some("Run this goal"));
        assert!(response.contains("Click the button"));
        assert!(action.command.is_some());
    }

    #[test]
    fn auto_high_confidence_returns_auto_fire() {
        let result = make_goal_result(0.85);
        let (action, response) = build_action_and_response(&result, "auto");
        assert_eq!(action.action_type, "auto_fire");
        assert!(response.contains("Firing"));
        assert!(action.command.is_some());
    }

    #[test]
    fn auto_low_confidence_falls_back_to_text() {
        // Confidence below 0.80 — should NOT auto-fire even in auto mode.
        let result = IntentResult::new(Intent::GoalRun, 0.70).with_goal("some vague request");
        let (action, _) = build_action_and_response(&result, "auto");
        assert_eq!(action.action_type, "text");
    }

    #[test]
    fn apply_intent_returns_apply_action() {
        let result = IntentResult::new(Intent::Apply, 0.95);
        let (action, _) = build_action_and_response(&result, "read_only");
        assert_eq!(action.action_type, "apply");
    }

    #[test]
    fn deny_intent_returns_deny_action() {
        let result = IntentResult::new(Intent::Deny, 0.95);
        let (action, _) = build_action_and_response(&result, "read_only");
        assert_eq!(action.action_type, "deny");
    }

    #[test]
    fn question_intent_returns_answer_action() {
        let result = IntentResult::new(Intent::Question, 0.85);
        let (action, _) = build_action_and_response(&result, "read_only");
        assert_eq!(action.action_type, "answer");
    }

    #[test]
    fn clarify_intent_returns_clarify_action() {
        let result = IntentResult::new(Intent::Clarify, 0.50);
        let (action, response) = build_action_and_response(&result, "read_only");
        assert_eq!(action.action_type, "clarify");
        assert!(response.contains("more specific"));
    }

    #[test]
    fn available_tools_read_only_excludes_goal_start() {
        let tools = available_tools("read_only");
        assert!(!tools.iter().any(|t| t.name == "ta_goal_start"));
        assert!(tools.iter().any(|t| t.name == "ta_draft_view"));
        assert!(tools.iter().any(|t| t.name == "ta_plan_status"));
        assert!(tools.iter().any(|t| t.name == "ta_fs_read"));
    }

    #[test]
    fn available_tools_suggest_includes_goal_start() {
        let tools = available_tools("suggest");
        assert!(tools.iter().any(|t| t.name == "ta_goal_start"));
        assert!(tools.iter().any(|t| t.name == "ta_draft_list"));
    }

    #[test]
    fn available_tools_auto_includes_goal_start() {
        let tools = available_tools("auto");
        assert!(tools.iter().any(|t| t.name == "ta_goal_start"));
    }

    #[test]
    fn intent_to_str_roundtrips() {
        assert_eq!(intent_to_str(&Intent::GoalRun), "goal_run");
        assert_eq!(intent_to_str(&Intent::Question), "question");
        assert_eq!(intent_to_str(&Intent::Clarify), "clarify");
        assert_eq!(intent_to_str(&Intent::Apply), "apply");
        assert_eq!(intent_to_str(&Intent::Deny), "deny");
    }

    #[test]
    fn command_formatted_correctly_for_goal_run() {
        let result = IntentResult::new(Intent::GoalRun, 0.85).with_goal("add tests for login flow");
        let (action, _) = build_action_and_response(&result, "read_only");
        assert_eq!(
            action.command.as_deref(),
            Some("ta run \"add tests for login flow\"")
        );
    }
}
