// api/draft_dialog.rs — Per-draft Q&A dialog (v0.17.0.12.6 items 11, 12).
//
// Each draft review panel gets its own chat window for questions about that
// specific draft ("Is this safe to apply?", "What does change X do?", "Can I
// apply just the UI changes?") plus advisor actions ("add item X to the
// plan", "create a --follow-up goal to fix Y", "amend this draft to also
// include Z"). Persisted per-draft at `.ta/drafts/<id>-dialog.jsonl`.
//
// Draft-action requests (amend/follow-up/add-to-plan) are recorded to
// `.ta/advisor-pending-actions.jsonl` rather than writing directly to shared
// files like PLAN.md — v0.17.0.12.7 ("Merge Shared Files for Parallel Work")
// adds the conflict-safe apply-time merge and patch queue those pending
// actions are meant to feed; this phase only needs to capture the request
// without clobbering a file a concurrent goal might also be touching.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::api::AppState;
use ta_advisor::{classify_advisor_intent, AdvisorIntent, DraftActionKind};
use ta_changeset::draft_package::DraftPackage;

const MAX_DIALOG_ENTRIES: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftDialogEntry {
    /// "user" or "advisor"
    pub role: String,
    pub text: String,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Directive for the Studio UI to act on: "apply", "deny", or
    /// "recorded_pending_action".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl DraftDialogEntry {
    fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            text: text.into(),
            timestamp: Utc::now().to_rfc3339(),
            intent: None,
            action: None,
        }
    }

    fn advisor(text: impl Into<String>) -> Self {
        Self {
            role: "advisor".to_string(),
            text: text.into(),
            timestamp: Utc::now().to_rfc3339(),
            intent: None,
            action: None,
        }
    }

    fn with_intent(mut self, intent: &str) -> Self {
        self.intent = Some(intent.to_string());
        self
    }

    fn with_action(mut self, action: &str) -> Self {
        self.action = Some(action.to_string());
        self
    }
}

fn dialog_path(state: &AppState, draft_id: &str) -> std::path::PathBuf {
    state
        .project_root
        .join(".ta")
        .join("drafts")
        .join(format!("{}-dialog.jsonl", draft_id))
}

fn pending_actions_path(state: &AppState) -> std::path::PathBuf {
    state
        .project_root
        .join(".ta")
        .join("advisor-pending-actions.jsonl")
}

fn load_entries(path: &std::path::Path) -> Vec<DraftDialogEntry> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<DraftDialogEntry>(l).ok())
        .collect()
}

fn append_entries(path: &std::path::Path, entries: &[DraftDialogEntry]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut existing = load_entries(path);
    existing.extend(entries.iter().cloned());
    if existing.len() > MAX_DIALOG_ENTRIES {
        existing = existing[existing.len() - MAX_DIALOG_ENTRIES..].to_vec();
        let mut file = std::fs::File::create(path)?;
        for entry in &existing {
            let line = serde_json::to_string(entry)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            writeln!(file, "{}", line)?;
        }
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for entry in entries {
        let line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{}", line)?;
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingAdvisorAction {
    id: String,
    draft_id: String,
    kind: String,
    message: String,
    created_at: String,
    status: String,
}

fn record_pending_action(
    state: &AppState,
    draft_id: &str,
    kind: DraftActionKind,
    message: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let path = pending_actions_path(state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let record = PendingAdvisorAction {
        id: Uuid::new_v4().to_string(),
        draft_id: draft_id.to_string(),
        kind: format!("{:?}", kind).to_lowercase(),
        message: message.to_string(),
        created_at: Utc::now().to_rfc3339(),
        status: "pending".to_string(),
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(&record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(file, "{}", line)
}

#[derive(Debug, Serialize)]
pub struct DraftDialogResponse {
    pub entries: Vec<DraftDialogEntry>,
    pub total: usize,
}

/// `GET /api/drafts/:id/dialog` — return the per-draft Q&A dialog history.
pub async fn get_dialog(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
) -> impl IntoResponse {
    let entries = load_entries(&dialog_path(&state, &draft_id));
    let total = entries.len();
    Json(DraftDialogResponse { entries, total }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct PostDraftDialogRequest {
    pub message: String,
}

/// `POST /api/drafts/:id/dialog` — classify a per-draft message and append
/// both turns to `.ta/drafts/<id>-dialog.jsonl`.
pub async fn post_dialog(
    State(state): State<Arc<AppState>>,
    Path(draft_id): Path<String>,
    Json(body): Json<PostDraftDialogRequest>,
) -> impl IntoResponse {
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "message is required"})),
        )
            .into_response();
    }

    let draft = load_draft_for_dialog(&state.pr_packages_dir, &draft_id);
    let classification = classify_advisor_intent(&message, true);

    let advisor_entry = match classification.intent {
        AdvisorIntent::DraftAction => match classification.draft_action_kind {
            Some(DraftActionKind::ApplyOrDeny) => {
                let lower = message.to_ascii_lowercase();
                let action =
                    if lower.contains("deny") || lower.contains("skip") || lower.contains("no") {
                        "deny"
                    } else {
                        "apply"
                    };
                let text = format!(
                    "Understood — I'll treat that as \"{}\" for this draft.",
                    action
                );
                DraftDialogEntry::advisor(text)
                    .with_intent("draft_action")
                    .with_action(action)
            }
            Some(kind) => {
                if let Err(e) = record_pending_action(&state, &draft_id, kind, &message) {
                    tracing::warn!(
                        draft_id = %draft_id, err = %e,
                        "Failed to record advisor pending action from draft dialog"
                    );
                }
                let text = format!(
                    "Got it — I've recorded this as a pending {} action: \"{}\". \
                     It'll show up under Pending Actions for human follow-through \
                     (this doesn't touch PLAN.md or start a goal automatically).",
                    match kind {
                        DraftActionKind::AddToPlan => "add-to-plan",
                        DraftActionKind::FollowUp => "follow-up",
                        DraftActionKind::Amend => "amend",
                        DraftActionKind::ApplyOrDeny => "apply/deny",
                    },
                    message
                );
                DraftDialogEntry::advisor(text)
                    .with_intent("draft_action")
                    .with_action("recorded_pending_action")
            }
            None => DraftDialogEntry::advisor(
                "I understood that as an action on this draft, but couldn't tell which kind. \
                 Try being explicit: \"amend this draft...\", \"create a follow-up goal to...\", \
                 or \"add item ... to the plan\".",
            )
            .with_intent("draft_action"),
        },
        AdvisorIntent::InfoRequest | AdvisorIntent::QueueGoal | AdvisorIntent::Ambiguous => {
            let text = answer_draft_question(&message, draft.as_ref());
            DraftDialogEntry::advisor(text).with_intent("info_request")
        }
    };

    let path = dialog_path(&state, &draft_id);
    let to_save = vec![DraftDialogEntry::user(message), advisor_entry.clone()];
    if let Err(e) = append_entries(&path, &to_save) {
        tracing::warn!(path = ?path, err = %e, "Failed to persist draft dialog");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to persist dialog: {}", e)})),
        )
            .into_response();
    }

    Json(advisor_entry).into_response()
}

fn load_draft_for_dialog(
    pr_packages_dir: &std::path::Path,
    draft_id: &str,
) -> Option<DraftPackage> {
    let uuid = Uuid::parse_str(draft_id).ok()?;
    let path = pr_packages_dir.join(format!("{}.json", uuid));
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Answer a question about this specific draft using its own data — no LLM,
/// no goal spawn. Falls back to a generic pointer when nothing matches.
fn answer_draft_question(message: &str, draft: Option<&DraftPackage>) -> String {
    let Some(draft) = draft else {
        return "I couldn't load this draft's details to answer that. Try refreshing the draft review panel.".to_string();
    };
    let lower = message.to_ascii_lowercase();

    if lower.contains("safe") || lower.contains("risk") {
        return match &draft.supervisor_review {
            Some(review) => format!(
                "Supervisor verdict: {:?} (scope_ok: {}). {}",
                review.verdict, review.scope_ok, review.summary
            ),
            None => "No supervisor review is attached to this draft yet — check the Changes list and Constitution Check sections before applying.".to_string(),
        };
    }

    if lower.contains("what does") || lower.contains("what changed") {
        if let Some(artifact) = draft.changes.artifacts.iter().find(|a| {
            lower.contains(
                &a.resource_uri
                    .rsplit('/')
                    .next()
                    .unwrap_or(&a.resource_uri)
                    .to_ascii_lowercase(),
            )
        }) {
            return format!(
                "{} was {:?} ({}).{}",
                artifact.resource_uri,
                artifact.change_type,
                artifact.diff_ref,
                artifact
                    .rationale
                    .as_ref()
                    .map(|r| format!(" Rationale: {}", r))
                    .unwrap_or_default()
            );
        }
        return format!(
            "This draft changes {} file(s). Ask about a specific filename for details, or see the Changes section for the full list.",
            draft.changes.artifacts.len()
        );
    }

    if lower.contains("just the") || lower.contains("only the") {
        return "Use the checkboxes in the Changes section to select exactly which files to apply, then Apply — deselected files are excluded from that apply.".to_string();
    }

    format!(
        "This draft (\"{}\") changes {} file(s). Ask about safety, a specific file, or applying a subset.",
        draft.goal.title,
        draft.changes.artifacts.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_draft() -> DraftPackage {
        let value = json!({
            "package_version": "1.0.0",
            "package_id": Uuid::new_v4().to_string(),
            "created_at": "2026-01-01T00:00:00Z",
            "goal": {
                "goal_id": "aabbccdd-0000-0000-0000-000000000000",
                "title": "Test goal",
                "objective": "test",
                "success_criteria": [],
                "constraints": []
            },
            "iteration": {
                "iteration_id": "iter-1",
                "sequence": 1,
                "workspace_ref": {"type": "staging_dir", "ref": "test"}
            },
            "agent_identity": {
                "agent_id": "test-agent",
                "agent_type": "test",
                "constitution_id": "default",
                "capability_manifest_hash": "abc"
            },
            "summary": {"what_changed": "test", "why": "test", "impact": "none", "rollback_plan": "none", "open_questions": [], "alternatives_considered": []},
            "plan": {"completed_steps": [], "next_steps": [], "decision_log": []},
            "changes": {"artifacts": [], "patch_sets": [], "pending_actions": []},
            "risk": {"risk_score": 0, "findings": [], "policy_decisions": []},
            "provenance": {"inputs": [], "tool_trace_hash": "test"},
            "review_requests": {"requested_actions": [], "reviewers": [], "required_approvals": 1},
            "signatures": {"package_hash": "test", "agent_signature": "test"}
        });
        serde_json::from_value(value).expect("minimal draft package should deserialize")
    }

    #[test]
    fn answer_draft_question_reports_no_review() {
        let draft = minimal_draft();
        let answer = answer_draft_question("is this safe to apply?", Some(&draft));
        assert!(answer.contains("No supervisor review"));
    }

    #[test]
    fn answer_draft_question_handles_missing_draft() {
        let answer = answer_draft_question("is this safe?", None);
        assert!(answer.contains("couldn't load"));
    }
}
