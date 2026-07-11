// api/dashboard_advisor.rs — Dashboard/Attention Advisor dialog (v0.17.0.12.6 item 4).
//
// A lightweight conversation surface embedded on the Studio
// Attention/Dashboard destination. Prior to v0.17.0.12.17 this was persisted
// separately from the full-tab Advisor chat (`.ta/advisor-history.json` vs
// `.ta/advisor-history.jsonl`) — two independently-persisted histories for
// what a user experiences as one Advisor. They're now one conversation, one
// store: `crate::api::advisor`'s unified `.ta/advisor-history.jsonl`.
// `DialogEntry` here is this surface's view over that shared store — see
// `crate::api::advisor::HistoryEntry` for the on-disk shape.
//
// Classification is delegated to the new `ta-advisor` crate (items 13-16):
//   - `queue_goal`  → confirmation card (title/phase/estimated duration)
//   - `info_request` → answered directly from daemon state, no goal spawned
//   - `draft_action` → pointed at the Review Drafts tab (per-draft dialog
//                       handles the actual amend/follow-up/add-to-plan action)
//   - `ambiguous`   → numbered clarification options, max 2 rounds

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::advisor::{
    append_history_entries, compute_live_context, load_history, HistoryEntry,
};
use crate::api::AppState;
use ta_advisor::{
    build_confirmation_card, classify_advisor_intent, next_clarify_step, AdvisorIntent,
    ClarifyOutcome, ClarifyState, ConfirmationCard,
};
use ta_session::AdvisorOption;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogEntry {
    /// "user" or "advisor"
    pub role: String,
    pub text: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_card: Option<ConfirmationCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

impl DialogEntry {
    fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            text: text.into(),
            timestamp: Utc::now().to_rfc3339(),
            intent: None,
            confirmation_card: None,
            options: None,
        }
    }

    fn advisor(text: impl Into<String>) -> Self {
        Self {
            role: "advisor".to_string(),
            text: text.into(),
            timestamp: Utc::now().to_rfc3339(),
            intent: None,
            confirmation_card: None,
            options: None,
        }
    }

    fn with_intent(mut self, intent: &str) -> Self {
        self.intent = Some(intent.to_string());
        self
    }

    fn with_card(mut self, card: ConfirmationCard) -> Self {
        self.confirmation_card = Some(card);
        self
    }

    fn with_options(mut self, options: Vec<String>) -> Self {
        self.options = Some(options);
        self
    }
}

impl From<DialogEntry> for HistoryEntry {
    fn from(d: DialogEntry) -> Self {
        HistoryEntry {
            role: d.role,
            text: d.text,
            timestamp: d.timestamp,
            intent: d.intent,
            confirmation_card: d.confirmation_card,
            options: d.options.map(|opts| {
                opts.into_iter()
                    .enumerate()
                    .map(|(i, label)| AdvisorOption {
                        number: (i + 1) as u32,
                        label,
                        action_type: "text".to_string(),
                        command: None,
                    })
                    .collect()
            }),
        }
    }
}

impl From<HistoryEntry> for DialogEntry {
    fn from(h: HistoryEntry) -> Self {
        DialogEntry {
            role: h.role,
            text: h.text,
            timestamp: h.timestamp,
            intent: h.intent,
            confirmation_card: h.confirmation_card,
            options: h
                .options
                .map(|opts| opts.into_iter().map(|o| o.label).collect()),
        }
    }
}

fn load_entries(state: &AppState) -> Vec<DialogEntry> {
    load_history(state).into_iter().map(Into::into).collect()
}

fn append_entries(state: &AppState, entries: &[DialogEntry]) -> std::io::Result<()> {
    let converted: Vec<HistoryEntry> = entries.iter().cloned().map(Into::into).collect();
    append_history_entries(state, &converted).map(|_| ())
}

/// How many consecutive "ambiguous" advisor turns trail the history — used to
/// derive the clarify round without any server-side session state (item 15:
/// max 2 rounds before "I need more info").
fn trailing_ambiguous_rounds(entries: &[DialogEntry]) -> ClarifyState {
    let mut round = 0u32;
    for entry in entries.iter().rev() {
        if entry.role != "advisor" {
            continue;
        }
        if entry.intent.as_deref() == Some("ambiguous") {
            round += 1;
        } else {
            break;
        }
    }
    ClarifyState { round }
}

#[derive(Debug, Serialize)]
pub struct DialogResponse {
    pub entries: Vec<DialogEntry>,
    pub total: usize,
}

/// `GET /api/advisor/dialog` — return the dashboard advisor dialog history.
pub async fn get_dialog(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let entries = load_entries(&state);
    let total = entries.len();
    Json(DialogResponse { entries, total }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct PostDialogRequest {
    pub message: String,
}

/// `POST /api/advisor/dialog` — classify a dashboard advisor message and
/// append both turns (human + advisor) to `.ta/advisor-history.jsonl`.
pub async fn post_dialog(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PostDialogRequest>,
) -> impl IntoResponse {
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "message is required"})),
        )
            .into_response();
    }

    let history = load_entries(&state);
    let classification = classify_advisor_intent(&message, false);

    let advisor_entry = match classification.intent {
        AdvisorIntent::QueueGoal => {
            let plan_path = state.project_root.join("PLAN.md");
            let current_phase = std::fs::read_to_string(&plan_path)
                .ok()
                .and_then(|content| {
                    crate::api::plan::parse_plan_phases(&content)
                        .into_iter()
                        .find(|p| p.status == "in_progress")
                        .map(|p| p.id)
                });
            let card = build_confirmation_card(&classification, current_phase.as_deref())
                .unwrap_or(ConfirmationCard {
                    title: message.clone(),
                    phase: current_phase,
                    estimated_duration_mins: 30,
                });
            let text = format!(
                "I read this as a new goal: \"{}\". Approve to queue it{}, Edit to change the title, or Cancel.",
                card.title,
                card.phase
                    .as_ref()
                    .map(|p| format!(" against phase {}", p))
                    .unwrap_or_default()
            );
            DialogEntry::advisor(text)
                .with_intent("queue_goal")
                .with_card(card)
        }
        AdvisorIntent::InfoRequest => {
            let ctx = compute_live_context(&state);
            let text = answer_info_request(&message, &ctx);
            DialogEntry::advisor(text).with_intent("info_request")
        }
        AdvisorIntent::DraftAction => {
            let text = "That sounds like an action on a specific draft (amend, follow-up, or add to plan). \
                Open the draft in Review Drafts and use its Q&A dialog — it has the full context needed to act on it.".to_string();
            DialogEntry::advisor(text).with_intent("draft_action")
        }
        AdvisorIntent::Ambiguous => {
            let state_before = trailing_ambiguous_rounds(&history);
            match next_clarify_step(state_before) {
                ClarifyOutcome::Options { options } => {
                    let text = "I'm not sure what you'd like me to do. Did you mean one of these?"
                        .to_string();
                    DialogEntry::advisor(text)
                        .with_intent("ambiguous")
                        .with_options(options)
                }
                ClarifyOutcome::NeedMoreInfo => {
                    let text = "I still need more info to act on that. Could you rephrase with more detail — e.g. what file, what behavior, or what you're trying to achieve?".to_string();
                    DialogEntry::advisor(text).with_intent("ambiguous")
                }
            }
        }
    };

    let entries_to_save = vec![DialogEntry::user(message), advisor_entry.clone()];
    if let Err(e) = append_entries(&state, &entries_to_save) {
        tracing::warn!(path = ?crate::api::advisor::advisor_history_path(&state), err = %e, "Failed to persist advisor dialog");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to persist dialog: {}", e)})),
        )
            .into_response();
    }

    Json(advisor_entry).into_response()
}

/// Answer an `info_request` purely from daemon state (item 16) — no goal is spawned.
fn answer_info_request(message: &str, ctx: &crate::api::advisor::AdvisorLiveContext) -> String {
    let lower = message.to_ascii_lowercase();

    if lower.contains("draft") || lower.contains("review") {
        return format!(
            "There {} {} draft{} pending review right now.",
            if ctx.pending_drafts == 1 { "is" } else { "are" },
            ctx.pending_drafts,
            if ctx.pending_drafts == 1 { "" } else { "s" }
        );
    }

    if lower.contains("health") {
        return format!(
            "There {} {} open health signal{} right now. Check the Health tab for details.",
            if ctx.health_signals_count == 1 {
                "is"
            } else {
                "are"
            },
            ctx.health_signals_count,
            if ctx.health_signals_count == 1 {
                ""
            } else {
                "s"
            }
        );
    }

    if lower.contains("phase") || lower.contains("plan") {
        return format!(
            "The plan has {} phase{} done and {} pending/in-progress.",
            ctx.plan_done_count,
            if ctx.plan_done_count == 1 { "" } else { "s" },
            ctx.plan_pending_count
        );
    }

    if ctx.active_goals.is_empty() {
        return "Nothing is currently running. Ask me to queue a goal, or check Review Drafts / Plan for pending work.".to_string();
    }

    let goal_lines: Vec<String> = ctx
        .active_goals
        .iter()
        .map(|g| {
            format!(
                "\"{}\" ({}, {}s elapsed{})",
                g.title,
                g.state,
                g.elapsed_secs.max(0),
                g.phase
                    .as_ref()
                    .map(|p| format!(", phase {}", p))
                    .unwrap_or_default()
            )
        })
        .collect();
    format!(
        "{} goal{} currently active: {}.",
        ctx.active_goals.len(),
        if ctx.active_goals.len() == 1 { "" } else { "s" },
        goal_lines.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: &str, intent: Option<&str>) -> DialogEntry {
        DialogEntry {
            role: role.to_string(),
            text: "x".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            intent: intent.map(|s| s.to_string()),
            confirmation_card: None,
            options: None,
        }
    }

    #[test]
    fn trailing_ambiguous_rounds_counts_consecutive_advisor_ambiguous() {
        let entries = vec![
            entry("user", None),
            entry("advisor", Some("ambiguous")),
            entry("user", None),
            entry("advisor", Some("ambiguous")),
        ];
        assert_eq!(trailing_ambiguous_rounds(&entries).round, 2);
    }

    #[test]
    fn trailing_ambiguous_rounds_resets_on_non_ambiguous() {
        let entries = vec![
            entry("advisor", Some("ambiguous")),
            entry("user", None),
            entry("advisor", Some("info_request")),
        ];
        assert_eq!(trailing_ambiguous_rounds(&entries).round, 0);
    }

    #[test]
    fn answer_info_request_reports_pending_drafts() {
        let ctx = crate::api::advisor::AdvisorLiveContext {
            active_goals: vec![],
            pending_drafts: 3,
            plan_pending_count: 0,
            plan_done_count: 0,
            health_signals_count: 0,
            generated_at: Utc::now().to_rfc3339(),
        };
        let answer = answer_info_request("how many drafts need review?", &ctx);
        assert!(answer.contains('3'));
    }
}
