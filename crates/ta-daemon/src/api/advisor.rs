// api/advisor.rs — Studio Advisor API (v0.15.26 + v0.16.1.3).
//
// Provides the advisor endpoint for the global intent bar and advisor panel.
// Classifies intent on each message and returns context-aware numbered options.
//
// Endpoints:
//   POST /api/advisor/message     — classify intent and return action + numbered options
//   GET  /api/advisor/tools       — list available tools by security level
//   GET  /api/advisor/config      — return current advisor config
//   POST /api/advisor/inject      — inject a mid-run note to the active goal's agent (v0.15.28)
//   GET  /api/advisor/history     — return persistent conversation history (v0.16.1.3)
//   POST /api/advisor/history     — append messages to conversation history (v0.16.1.3)
//   GET  /api/advisor/suggestions — context-sensitive suggestion chips (v0.16.1.3)
//   GET  /api/advisor/context     — live project context for read-only queries (v0.16.1.3)

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::AppState;
use ta_goal::{GoalRunState, GoalRunStore};
use ta_runtime::{build_channel, AgentFrameworkManifest, ChannelType, HumanNote};
use ta_session::classify_intent;
use ta_session::workflow_session::AdvisorSecurity;
use ta_session::Intent;
use ta_session::{AdvisorContext, AdvisorOption, AdvisorSession};

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
    /// Optional Studio context (current tab + selection).
    /// Used to generate context-shaped numbered option menus.
    #[serde(default)]
    pub context: Option<AdvisorContext>,
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
    /// Action the Studio should take (primary action, for backwards compatibility).
    pub action: AdvisorAction,
    /// Human-readable advisor response text shown in the chat pane.
    pub response: String,
    /// Numbered options for the advisor menu (v0.15.26).
    pub options: Vec<AdvisorOption>,
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

fn parse_security(s: &str) -> AdvisorSecurity {
    match s {
        "auto" => AdvisorSecurity::Auto,
        "suggest" => AdvisorSecurity::Suggest,
        _ => AdvisorSecurity::ReadOnly,
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `POST /api/advisor/message` — Classify intent and return advisor action + numbered options.
///
/// The advisor is explicitly on the human's side: it interprets their intent,
/// presents commands at the right escalation level, flags risks, and provides
/// context-shaped numbered option menus based on the current Studio tab.
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

    let security_str = resolve_security(&state, body.security_override.as_deref());
    let security = parse_security(&security_str);
    let context = body.context.unwrap_or_default();

    // Use AdvisorSession for unified intent classification + option generation.
    let session = AdvisorSession::from_message(&message, &security, &context);

    // Build backwards-compatible primary action from the first non-cancel option.
    let primary = session
        .options
        .iter()
        .find(|o| o.action_type != "clarify" || session.options.len() == 1)
        .or_else(|| session.options.first());

    let action = if let Some(opt) = primary {
        AdvisorAction {
            action_type: opt.action_type.clone(),
            label: if opt.action_type == "button" {
                Some(opt.label.clone())
            } else {
                None
            },
            command: opt.command.clone(),
        }
    } else {
        // Fallback: use the classify_intent result directly.
        let result = classify_intent(&message);
        let (legacy_action, _) = build_legacy_action(&result, &security_str);
        legacy_action
    };

    Json(MessageResponse {
        intent: session.intent,
        confidence: session.confidence,
        extracted_goal: session.extracted_goal,
        action,
        response: session.response,
        options: session.options,
    })
    .into_response()
}

/// Build a backwards-compatible AdvisorAction from the classified intent (no options).
fn build_legacy_action(
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
                "auto" if result.is_auto_actionable() => (
                    AdvisorAction {
                        action_type: "auto_fire".to_string(),
                        label: Some("Run goal".to_string()),
                        command: Some(command.clone()),
                    },
                    format!(
                        "Intent: run a goal (confidence {:.0}%). Firing: `{}`",
                        result.confidence * 100.0,
                        command
                    ),
                ),
                "suggest" => (
                    AdvisorAction {
                        action_type: "button".to_string(),
                        label: Some("Run this goal".to_string()),
                        command: Some(command.clone()),
                    },
                    format!(
                        "I understood this as a goal request. Click the button to run: `{}`",
                        command
                    ),
                ),
                _ => (
                    AdvisorAction {
                        action_type: "text".to_string(),
                        label: None,
                        command: Some(command.clone()),
                    },
                    format!(
                        "I understood this as a goal request. Run this command to proceed:\n```\n{}\n```",
                        command
                    ),
                ),
            }
        }
        Intent::Apply => (
            AdvisorAction {
                action_type: "apply".to_string(),
                label: None,
                command: None,
            },
            "Approval noted. Studio should apply the current draft.".to_string(),
        ),
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
            "I'm not sure what you'd like me to do. Could you be more specific?".to_string(),
        ),
    }
}

/// `GET /api/advisor/tools` — List available tools at the current security level.
pub async fn get_tools(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let security = state.daemon_config.shell.advisor.security.clone();
    let tools = available_tools(&security);
    Json(ToolsResponse { security, tools }).into_response()
}

/// Return the tools available at the given security level.
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

// ── Inject endpoint (v0.15.28) ────────────────────────────────────────────────

/// Request body for `POST /api/advisor/inject`.
#[derive(Debug, Deserialize)]
pub struct InjectRequest {
    /// The note/instruction to send to the agent.
    pub message: String,
    /// Goal ID (or prefix) to target. Defaults to the most recent running goal.
    #[serde(default)]
    pub goal_id: Option<String>,
}

/// Response from `POST /api/advisor/inject`.
#[derive(Debug, Serialize)]
pub struct InjectResponse {
    /// How the note was delivered: "live-polled", "api-pushed", "queued", "answered".
    pub delivery: String,
    /// The goal ID that received the note.
    pub goal_id: String,
    /// Path to the notes file (where applicable).
    pub notes_file: Option<String>,
    /// The message that was injected.
    pub message: String,
}

/// `POST /api/advisor/inject` — Inject a mid-run human note to the active goal's agent.
///
/// Resolves the target goal (from `goal_id` or the most recently running goal),
/// builds the appropriate `AgentContextChannel`, calls `inject_note()`, and returns
/// the `NoteDelivery` result so the caller knows whether the agent received it live.
pub async fn handle_inject(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InjectRequest>,
) -> impl IntoResponse {
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "message is required"})),
        )
            .into_response();
    }

    // Load the goal store.
    let store = match GoalRunStore::new(&state.goals_dir) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": format!("Failed to load goal store from {:?}: {}", state.goals_dir, e)
                })),
            )
                .into_response();
        }
    };

    // Resolve the target goal.
    let goal = match resolve_inject_goal(&store, body.goal_id.as_deref()) {
        Ok(g) => g,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": e,
                    "hint": "Start a goal with `ta run` or pass an explicit --goal <id>."
                })),
            )
                .into_response();
        }
    };

    let goal_id_str = goal.goal_run_id.to_string();
    let staging_path = goal.workspace_path.clone();

    // Resolve channel type from the goal's agent framework manifest.
    let channel_type = AgentFrameworkManifest::resolve(&goal.agent_id, &state.project_root)
        .map(|m| m.channel_type)
        .unwrap_or(ChannelType::ClaudeCode);

    // Get context_file from manifest (default "CLAUDE.md").
    let context_file = AgentFrameworkManifest::resolve(&goal.agent_id, &state.project_root)
        .map(|m| m.context_file)
        .unwrap_or_else(|| "CLAUDE.md".to_string());

    // Build the channel.
    let channel = build_channel(&channel_type, staging_path.clone(), &context_file);

    // Build and inject the note.
    let note = HumanNote::new(&goal_id_str, &message);
    match channel.inject_note(&note) {
        Ok(delivery) => {
            // Best-effort notes file path (ClaudeCode pattern).
            let notes_file = if channel_type == ChannelType::ClaudeCode {
                let path = staging_path
                    .join(".ta/advisor-notes")
                    .join(format!("{}.md", goal_id_str));
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            };

            tracing::info!(
                goal_id = %goal_id_str,
                delivery = %delivery,
                agent_id = %goal.agent_id,
                channel = %channel_type,
                "Human note injected via POST /api/advisor/inject"
            );

            Json(InjectResponse {
                delivery: delivery.to_string(),
                goal_id: goal_id_str,
                notes_file,
                message,
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!(
                    "Failed to inject note into goal {}: {}. \
                     Check that the staging directory exists: {:?}",
                    goal_id_str, e, staging_path
                )
            })),
        )
            .into_response(),
    }
}

/// Resolve the target goal for injection.
///
/// If `goal_id_hint` is provided, find the goal by ID prefix.
/// Otherwise, return the most recently running goal.
fn resolve_inject_goal(
    store: &GoalRunStore,
    goal_id_hint: Option<&str>,
) -> Result<ta_goal::GoalRun, String> {
    let goals = store
        .list()
        .map_err(|e| format!("Failed to list goals: {}", e))?;

    if let Some(hint) = goal_id_hint {
        // Find by ID prefix.
        let matched: Vec<_> = goals
            .iter()
            .filter(|g| g.goal_run_id.to_string().starts_with(hint))
            .collect();
        match matched.len() {
            0 => Err(format!(
                "No goal found matching prefix '{}'. \
                 Use `ta goal list` to see available goals.",
                hint
            )),
            1 => Ok(matched[0].clone()),
            n => Err(format!(
                "Ambiguous prefix '{}' matches {} goals. Use a longer prefix.",
                hint, n
            )),
        }
    } else {
        // Find the most recently running goal.
        goals
            .into_iter()
            .find(|g| g.state == GoalRunState::Running)
            .ok_or_else(|| {
                "No goal is currently running. \
                 Start a goal with `ta run` or pass an explicit --goal <id>."
                    .to_string()
            })
    }
}

// ── History API (v0.16.1.3) ──────────────────────────────────────────────────

/// A single persisted conversation entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub role: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<AdvisorOption>>,
    pub timestamp: String,
}

/// Response from `GET /api/advisor/history`.
#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub entries: Vec<HistoryEntry>,
    pub total: usize,
}

/// Request body for `POST /api/advisor/history`.
#[derive(Debug, Deserialize)]
pub struct AppendHistoryRequest {
    pub entries: Vec<HistoryEntry>,
}

fn advisor_history_path(state: &AppState) -> std::path::PathBuf {
    state.project_root.join(".ta").join("advisor-history.json")
}

fn load_history(state: &AppState) -> Vec<HistoryEntry> {
    let path = advisor_history_path(state);
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<HistoryEntry>>(&s).ok())
        .unwrap_or_default()
}

fn save_history(state: &AppState, entries: &[HistoryEntry]) -> std::io::Result<()> {
    let path = advisor_history_path(state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(entries)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)
}

/// `GET /api/advisor/history` — Return the last 100 persisted conversation entries.
pub async fn get_history(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut entries = load_history(&state);
    // Return at most 100 entries.
    if entries.len() > 100 {
        entries = entries[entries.len() - 100..].to_vec();
    }
    let total = entries.len();
    Json(HistoryResponse { entries, total }).into_response()
}

/// `POST /api/advisor/history` — Append new entries to the persistent history.
///
/// The store is trimmed to 200 entries to cap disk usage.
pub async fn append_history(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AppendHistoryRequest>,
) -> impl IntoResponse {
    if body.entries.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "entries must not be empty"})),
        )
            .into_response();
    }

    let mut existing = load_history(&state);
    existing.extend(body.entries);
    // Trim to 200 entries.
    if existing.len() > 200 {
        existing = existing[existing.len() - 200..].to_vec();
    }

    if let Err(e) = save_history(&state, &existing) {
        tracing::warn!(path = ?advisor_history_path(&state), err = %e, "Failed to persist advisor history");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to persist history: {}", e)})),
        )
            .into_response();
    }

    Json(json!({"saved": existing.len()})).into_response()
}

// ── Suggestions API (v0.16.1.3) ──────────────────────────────────────────────

/// A single context-sensitive suggestion chip.
#[derive(Debug, Serialize)]
pub struct SuggestionChip {
    pub id: String,
    pub text: String,
    pub action_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// `GET /api/advisor/suggestions` — Context-sensitive suggestion chips.
///
/// Reads project context (project_type, pending phases, open drafts) and surfaces
/// relevant suggestion chips above the advisor input.
pub async fn get_suggestions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut chips: Vec<SuggestionChip> = Vec::new();

    // Check for pending drafts.
    let draft_count = crate::api::notifications::count_pending_drafts_pub(&state.pr_packages_dir);
    if draft_count > 0 {
        chips.push(SuggestionChip {
            id: "review_drafts".to_string(),
            text: format!(
                "Review {} pending draft{}",
                draft_count,
                if draft_count == 1 { "" } else { "s" }
            ),
            action_type: "switch_tab".to_string(),
            command: Some("drafts".to_string()),
        });
    }

    // Check for running goals.
    if let Ok(store) = GoalRunStore::new(&state.goals_dir) {
        if let Ok(goals) = store.list() {
            let running: Vec<_> = goals
                .iter()
                .filter(|g| g.state == GoalRunState::Running)
                .collect();
            if !running.is_empty() {
                let title = &running[0].title;
                chips.push(SuggestionChip {
                    id: "check_running_goal".to_string(),
                    text: format!(
                        "Check progress on \"{}\"",
                        title.chars().take(40).collect::<String>()
                    ),
                    action_type: "ask_question".to_string(),
                    command: Some(format!(
                        "What is the status of the running goal \"{}\"?",
                        title
                    )),
                });
            }
        }
    }

    // Check project type from workflow.toml.
    let workflow_path = state.project_root.join(".ta").join("workflow.toml");
    if let Ok(content) = std::fs::read_to_string(&workflow_path) {
        if content.contains("project_type") {
            if content.contains("unreal") || content.contains("ue5") || content.contains("ue4") {
                chips.push(SuggestionChip {
                    id: "unreal_workflow".to_string(),
                    text: "Initialize Unreal workflow template".to_string(),
                    action_type: "switch_tab".to_string(),
                    command: Some("workflows".to_string()),
                });
            }
            if content.contains("unity") {
                chips.push(SuggestionChip {
                    id: "unity_workflow".to_string(),
                    text: "Initialize Unity workflow template".to_string(),
                    action_type: "switch_tab".to_string(),
                    command: Some("workflows".to_string()),
                });
            }
        }
    }

    // Default: suggest starting a goal if everything is quiet.
    if chips.is_empty() {
        chips.push(SuggestionChip {
            id: "plan_next".to_string(),
            text: "What should I work on next?".to_string(),
            action_type: "ask_question".to_string(),
            command: Some("What is the next phase I should implement?".to_string()),
        });
        chips.push(SuggestionChip {
            id: "check_health".to_string(),
            text: "System health check".to_string(),
            action_type: "ask_question".to_string(),
            command: Some("What is the current system health status?".to_string()),
        });
    }

    Json(chips).into_response()
}

// ── Context API (v0.16.1.3) ──────────────────────────────────────────────────

/// Aggregated live project context for advisor read-only queries.
#[derive(Debug, Serialize)]
pub struct AdvisorLiveContext {
    pub active_goals: Vec<GoalSummary>,
    pub pending_drafts: usize,
    pub plan_pending_count: usize,
    pub plan_done_count: usize,
    pub health_signals_count: usize,
    pub generated_at: String,
}

/// Summary of a running goal for advisor context.
#[derive(Debug, Serialize)]
pub struct GoalSummary {
    pub goal_id: String,
    pub title: String,
    pub state: String,
    pub elapsed_secs: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

/// `GET /api/advisor/context` — Live project context for advisor read-only queries.
///
/// Aggregates goals, drafts, plan phases, and health signals into a single context
/// object the advisor frontend uses to answer check_status and question intents.
pub async fn get_context(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let now = Utc::now();

    let active_goals: Vec<GoalSummary> = GoalRunStore::new(&state.goals_dir)
        .ok()
        .and_then(|store| store.list().ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|g| matches!(g.state, GoalRunState::Running | GoalRunState::Configured))
        .map(|g| {
            let elapsed = (now - g.updated_at).num_seconds();
            GoalSummary {
                goal_id: g.goal_run_id.to_string(),
                title: g.title.clone(),
                state: format!("{:?}", g.state).to_lowercase(),
                elapsed_secs: elapsed,
                phase: g.plan_phase.clone(),
            }
        })
        .collect();

    let pending_drafts =
        crate::api::notifications::count_pending_drafts_pub(&state.pr_packages_dir);

    // Parse PLAN.md for phase counts.
    let plan_path = state.project_root.join("PLAN.md");
    let (plan_pending_count, plan_done_count) = std::fs::read_to_string(&plan_path)
        .map(|content| {
            let phases = crate::api::plan::parse_plan_phases(&content);
            let pending = phases
                .iter()
                .filter(|p| p.status == "pending" || p.status == "in_progress")
                .count();
            let done = phases.iter().filter(|p| p.status == "done").count();
            (pending, done)
        })
        .unwrap_or((0, 0));

    // Count health signals (use cached value if available; else 0 to avoid a compute on every context call).
    let health_signals_count = state
        .signals_cache
        .last_computed_at()
        .map(|_| {
            state
                .signals_cache
                .get_or_compute(
                    &state.project_root,
                    &state.goals_dir,
                    &state.pr_packages_dir,
                )
                .len()
        })
        .unwrap_or(0);

    Json(AdvisorLiveContext {
        active_goals,
        pending_drafts,
        plan_pending_count,
        plan_done_count,
        health_signals_count,
        generated_at: now.to_rfc3339(),
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
        let (action, response) = build_legacy_action(&result, "read_only");
        assert_eq!(action.action_type, "text");
        assert!(action.command.as_deref().unwrap().starts_with("ta run"));
        assert!(response.contains("ta run"));
        assert!(action.label.is_none());
    }

    #[test]
    fn suggest_goal_run_returns_button_action() {
        let result = make_goal_result(0.85);
        let (action, response) = build_legacy_action(&result, "suggest");
        assert_eq!(action.action_type, "button");
        assert_eq!(action.label.as_deref(), Some("Run this goal"));
        assert!(response.contains("Click the button"));
        assert!(action.command.is_some());
    }

    #[test]
    fn auto_high_confidence_returns_auto_fire() {
        let result = make_goal_result(0.85);
        let (action, response) = build_legacy_action(&result, "auto");
        assert_eq!(action.action_type, "auto_fire");
        assert!(response.contains("Firing"));
        assert!(action.command.is_some());
    }

    #[test]
    fn auto_low_confidence_falls_back_to_text() {
        let result = IntentResult::new(Intent::GoalRun, 0.70).with_goal("some vague request");
        let (action, _) = build_legacy_action(&result, "auto");
        assert_eq!(action.action_type, "text");
    }

    #[test]
    fn apply_intent_returns_apply_action() {
        let result = IntentResult::new(Intent::Apply, 0.95);
        let (action, _) = build_legacy_action(&result, "read_only");
        assert_eq!(action.action_type, "apply");
    }

    #[test]
    fn deny_intent_returns_deny_action() {
        let result = IntentResult::new(Intent::Deny, 0.95);
        let (action, _) = build_legacy_action(&result, "read_only");
        assert_eq!(action.action_type, "deny");
    }

    #[test]
    fn question_intent_returns_answer_action() {
        let result = IntentResult::new(Intent::Question, 0.85);
        let (action, _) = build_legacy_action(&result, "read_only");
        assert_eq!(action.action_type, "answer");
    }

    #[test]
    fn clarify_intent_returns_clarify_action() {
        let result = IntentResult::new(Intent::Clarify, 0.50);
        let (action, response) = build_legacy_action(&result, "read_only");
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
    fn advisor_session_intent_str_roundtrips() {
        use ta_session::AdvisorContext;
        let ctx = AdvisorContext::default();
        let check = |msg: &str, expected: &str| {
            let s = AdvisorSession::from_message(
                msg,
                &ta_session::workflow_session::AdvisorSecurity::ReadOnly,
                &ctx,
            );
            assert_eq!(s.intent, expected, "message: {}", msg);
        };
        check("also add tests", "goal_run");
        check("what changed?", "question");
        check("apply", "apply");
        check("skip", "deny");
        check("hmm", "clarify");
    }

    #[test]
    fn command_formatted_correctly_for_goal_run() {
        let result = IntentResult::new(Intent::GoalRun, 0.85).with_goal("add tests for login flow");
        let (action, _) = build_legacy_action(&result, "read_only");
        assert_eq!(
            action.command.as_deref(),
            Some("ta run \"add tests for login flow\"")
        );
    }

    #[test]
    fn message_response_includes_options() {
        // Directly test that AdvisorSession produces options.
        use ta_session::{AdvisorContext, AdvisorSession};
        let ctx = AdvisorContext {
            tab: "dashboard".to_string(),
            selection: None,
        };
        let session = AdvisorSession::from_message(
            "also add tests",
            &ta_session::workflow_session::AdvisorSecurity::ReadOnly,
            &ctx,
        );
        assert!(!session.options.is_empty(), "options should not be empty");
        assert!(
            session.options.iter().all(|o| o.number > 0),
            "all options must have a positive number"
        );
    }

    #[test]
    fn context_shapes_workflow_options() {
        use ta_session::{AdvisorContext, AdvisorSession};
        let ctx = AdvisorContext {
            tab: "workflows".to_string(),
            selection: Some("my-workflow".to_string()),
        };
        let session = AdvisorSession::from_message(
            "amend auto-approve",
            &ta_session::workflow_session::AdvisorSecurity::Suggest,
            &ctx,
        );
        let labels: Vec<_> = session.options.iter().map(|o| o.label.as_str()).collect();
        assert!(
            labels.contains(&"Amend auto-approve for this workflow"),
            "got: {:?}",
            labels
        );
    }
}
