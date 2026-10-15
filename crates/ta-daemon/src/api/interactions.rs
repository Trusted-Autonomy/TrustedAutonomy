// api/interactions.rs — Human response endpoints for interactive mode.
//
// When an agent calls ta_ask_human, the question is registered in the
// QuestionRegistry (for in-process callers) AND a question file is written
// to .ta/interactions/pending/<id>.json (for the file-based MCP tool polling).
// These endpoints let any interface (ta shell, web UI, Slack bot) deliver
// the human's answer.
//
// Endpoints:
//   POST /api/interactions/:id/respond  — Answer a pending question
//   GET  /api/interactions/pending      — List pending questions

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::AppState;
use crate::question_registry::HumanAnswer;

#[derive(Debug, Deserialize)]
pub struct RespondRequest {
    pub answer: String,
    /// Set by channel plugins; checked against `[channels.access_control]`.
    #[serde(flatten)]
    pub sender: crate::api::cmd::ChannelSender,
}

#[derive(Debug, Serialize)]
pub struct RespondResponse {
    pub interaction_id: String,
    pub status: String,
}

/// `POST /api/interactions/:id/respond` — Answer a pending question.
///
/// Delivers the human's answer through two channels:
/// 1. QuestionRegistry (for future in-process use by async callers).
/// 2. File-based: writes .ta/interactions/answers/<id>.json so the
///    ta_ask_human MCP tool can poll for it. This is the primary delivery
///    mechanism for the file-based MCP tool handler.
pub async fn respond(
    State(state): State<Arc<AppState>>,
    Path(interaction_id): Path<String>,
    proof: Option<axum::Extension<crate::human_gate::HumanProof>>,
    Json(body): Json<RespondRequest>,
) -> impl IntoResponse {
    // The named human when the call carried a verified assertion, else "api".
    let responder = proof
        .and_then(|axum::Extension(p)| p.user_id)
        .unwrap_or_else(|| "api".to_string());
    // Parse the UUID — return 400 on malformed input.
    let id = match Uuid::parse_str(&interaction_id) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "Invalid interaction ID '{}': expected a UUID (e.g. 550e8400-e29b-41d4-a716-446655440000)",
                        interaction_id
                    )
                })),
            )
                .into_response();
        }
    };

    // Channel access control (CR-09): a barred sender must not answer. A request
    // with no channel fields (CLI, web UI) is unaffected.
    if let Err(resp) = crate::api::cmd::enforce_channel_access_for(
        &state,
        &body.sender,
        &format!("answer interaction {id}"),
        "POST /api/interactions/{id}/respond",
    ) {
        return *resp;
    }

    deliver_answer(&state, &state.project_root, id, body.answer, &responder).await;
    tracing::info!(
        interaction_id = %id,
        responder = %responder,
        "Interaction answered"
    );
    Json(RespondResponse {
        interaction_id: id.to_string(),
        status: "delivered".to_string(),
    })
    .into_response()
}

/// Deliver a human's answer to a pending question, by both routes: the answer
/// file the file-based `ta_ask_human` tool polls, and the in-process
/// `QuestionRegistry`. Shared by `respond` and `POST /api/attention/{id}/act`.
///
/// A missing registry entry is expected (the question came from the
/// file-based MCP tool), so this does not fail; file-write problems are logged
/// with the path and what they affect.
pub(crate) async fn deliver_answer(
    state: &AppState,
    root: &std::path::Path,
    id: Uuid,
    answer_text: String,
    responder: &str,
) {
    let now = Utc::now();
    // ── Write answer file for the file-based MCP tool polling ────
    // The ta_ask_human handler polls .ta/interactions/answers/<id>.json.
    // We write this unconditionally so the MCP tool always gets the answer,
    // regardless of whether a QuestionRegistry entry exists.
    let answers_dir = root.join(".ta").join("interactions").join("answers");

    if let Err(e) = std::fs::create_dir_all(&answers_dir) {
        tracing::warn!(
            interaction_id = %id,
            error = %e,
            "Failed to create answers directory; MCP tool polling may not receive the answer"
        );
    } else {
        let answer_path = answers_dir.join(format!("{}.json", id));
        let answer_json = serde_json::json!({
            "text": &answer_text,
            "responder_id": responder,
            "answered_at": now.to_rfc3339(),
        });
        match serde_json::to_string(&answer_json) {
            Ok(json_str) => {
                if let Err(e) = std::fs::write(&answer_path, json_str) {
                    tracing::warn!(
                        interaction_id = %id,
                        path = %answer_path.display(),
                        error = %e,
                        "Failed to write answer file; MCP tool polling may not receive the answer"
                    );
                } else {
                    tracing::debug!(
                        interaction_id = %id,
                        path = %answer_path.display(),
                        "Answer file written for MCP tool polling"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    interaction_id = %id,
                    error = %e,
                    "Failed to serialize answer JSON"
                );
            }
        }
    }

    // ── Deliver via QuestionRegistry (for in-process waiters) ─────
    let answer = HumanAnswer {
        text: answer_text,
        responder_id: responder.to_string(),
        answered_at: now,
    };
    if state.question_registry.answer(id, answer).await.is_err() {
        // Expected when the question was written by the file-based MCP tool
        // (ta_ask_human) rather than registered in-process. The answer file
        // has already been written above, so the tool will receive the
        // response on its next poll iteration.
        tracing::info!(
            interaction_id = %id,
            "No in-process registry entry for interaction (file-based answer already written)"
        );
    }
}

/// `GET /api/interactions/pending` — List pending questions.
///
/// Returns all questions currently waiting for a human response. Each entry
/// includes the interaction_id needed to answer via POST /api/interactions/:id/respond.
pub async fn list_pending(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pending = state.question_registry.list_pending().await;
    tracing::debug!(count = pending.len(), "Listed pending interactions");
    Json(pending)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use crate::question_registry::{PendingQuestion, QuestionRegistry};

    fn make_question(id: Uuid) -> PendingQuestion {
        PendingQuestion {
            interaction_id: id,
            goal_id: None,
            question: "Should I proceed?".to_string(),
            context: None,
            response_hint: "yes_no".to_string(),
            choices: vec![],
            turn: 1,
            created_at: Utc::now(),
            timeout_secs: Some(60),
        }
    }

    /// Verify that the registry round-trips correctly — answering removes from pending.
    #[tokio::test]
    async fn answer_removes_from_pending() {
        let registry = QuestionRegistry::new();
        let id = Uuid::new_v4();
        let _rx = registry.register(make_question(id)).await;

        assert_eq!(registry.len().await, 1);

        let answer = crate::question_registry::HumanAnswer {
            text: "yes".to_string(),
            responder_id: "api".to_string(),
            answered_at: Utc::now(),
        };
        registry
            .answer(id, answer)
            .await
            .expect("answer should succeed");

        assert_eq!(
            registry.len().await,
            0,
            "answered question should be removed"
        );
    }

    /// Verify that answering an unknown ID returns Err.
    #[tokio::test]
    async fn answer_unknown_returns_err() {
        let registry = QuestionRegistry::new();
        let id = Uuid::new_v4();

        let answer = crate::question_registry::HumanAnswer {
            text: "yes".to_string(),
            responder_id: "api".to_string(),
            answered_at: Utc::now(),
        };
        let result = registry.answer(id, answer).await;
        assert!(result.is_err(), "unknown ID should return Err");
    }

    /// Verify that list_pending returns all registered questions.
    #[tokio::test]
    async fn list_pending_returns_all() {
        let registry = QuestionRegistry::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        let _rx1 = registry.register(make_question(id1)).await;
        let _rx2 = registry.register(make_question(id2)).await;

        let pending = registry.list_pending().await;
        assert_eq!(pending.len(), 2);

        let ids: Vec<Uuid> = pending.iter().map(|q| q.interaction_id).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    // ── Channel access control (CR-09) ──────────────────────────────

    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::Json;

    use super::{respond, RespondRequest};
    use crate::api::cmd::ChannelSender;
    use crate::api::AppState;
    use crate::config::{ChannelAccessControl, DaemonConfig};

    fn acl_state(root: &std::path::Path, acl: ChannelAccessControl) -> Arc<AppState> {
        let mut cfg = DaemonConfig::default();
        cfg.channels.access_control = acl;
        Arc::new(AppState::new(root.to_path_buf(), cfg))
    }

    fn only(users: &[&str]) -> ChannelAccessControl {
        ChannelAccessControl {
            allowed_users: users.iter().map(|u| u.to_string()).collect(),
            ..Default::default()
        }
    }

    fn answer_from(channel: Option<&str>, sender: Option<&str>) -> RespondRequest {
        RespondRequest {
            answer: "yes".into(),
            sender: ChannelSender {
                channel: channel.map(str::to_string),
                sender_id: sender.map(str::to_string),
                sender_roles: vec![],
            },
        }
    }

    async fn answer(
        state: &Arc<AppState>,
        id: Uuid,
        body: RespondRequest,
    ) -> (StatusCode, serde_json::Value) {
        let resp = respond(State(state.clone()), Path(id.to_string()), None, Json(body))
            .await
            .into_response();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn answer_file(root: &std::path::Path, id: Uuid) -> std::path::PathBuf {
        root.join(".ta/interactions/answers")
            .join(format!("{id}.json"))
    }

    #[tokio::test]
    async fn channel_permitted_sender_can_answer() {
        let dir = tempfile::tempdir().unwrap();
        let state = acl_state(dir.path(), only(&["111"]));
        let id = Uuid::new_v4();
        let (st, _) = answer(&state, id, answer_from(Some("discord"), Some("111"))).await;
        assert_eq!(st, StatusCode::OK);
        assert!(answer_file(dir.path(), id).exists());
    }

    #[tokio::test]
    async fn channel_denied_sender_gets_403_and_question_stays_unanswered() {
        let dir = tempfile::tempdir().unwrap();
        let state = acl_state(dir.path(), only(&["111"]));
        let id = Uuid::new_v4();
        let mut rx = state.question_registry.register(make_question(id)).await;
        let (st, body) = answer(&state, id, answer_from(Some("discord"), Some("999"))).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let msg = body["error"].as_str().unwrap();
        assert!(msg.contains("999") && msg.contains("discord"), "{msg}");
        assert!(
            msg.contains("allowed_users") && msg.contains("daemon.toml"),
            "{msg}"
        );
        assert!(!answer_file(dir.path(), id).exists());
        assert!(
            rx.try_recv().is_err(),
            "the waiting caller must not be answered"
        );
        assert_eq!(state.question_registry.list_pending().await.len(), 1);
    }

    #[tokio::test]
    async fn restricted_channel_without_sender_cannot_answer() {
        let dir = tempfile::tempdir().unwrap();
        let state = acl_state(dir.path(), only(&["111"]));
        let id = Uuid::new_v4();
        let (st, body) = answer(&state, id, answer_from(Some("discord"), None)).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("sender_id"));
        assert!(!answer_file(dir.path(), id).exists());
    }

    #[tokio::test]
    async fn deny_list_beats_allow_list_on_answer() {
        let dir = tempfile::tempdir().unwrap();
        let state = acl_state(
            dir.path(),
            ChannelAccessControl {
                allowed_users: vec!["111".into()],
                denied_users: vec!["111".into()],
                ..Default::default()
            },
        );
        let id = Uuid::new_v4();
        let (st, _) = answer(&state, id, answer_from(Some("discord"), Some("111"))).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert!(!answer_file(dir.path(), id).exists());
    }

    #[tokio::test]
    async fn answer_without_channel_fields_is_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        let state = acl_state(dir.path(), only(&["111"]));
        let id = Uuid::new_v4();
        let (st, _) = answer(&state, id, answer_from(None, None)).await;
        assert_eq!(st, StatusCode::OK);
        assert!(answer_file(dir.path(), id).exists());
    }

    #[test]
    fn respond_body_accepts_plugin_fields_and_plain_body() {
        let b: RespondRequest = serde_json::from_str(
            r#"{"answer":"yes","source":"discord","channel":"discord","sender_id":"7","sender_roles":["r"]}"#,
        )
        .unwrap();
        assert_eq!(b.sender.sender_id.as_deref(), Some("7"));
        assert_eq!(b.sender.sender_roles, ["r"]);
        let b: RespondRequest = serde_json::from_str(r#"{"answer":"yes"}"#).unwrap();
        assert!(b.sender.channel.is_none() && b.sender.sender_id.is_none());
    }
}
