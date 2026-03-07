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
    Json(body): Json<RespondRequest>,
) -> impl IntoResponse {
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

    let now = Utc::now();
    let answer_text = body.answer.clone();

    // ── Write answer file for the file-based MCP tool polling ────
    // The ta_ask_human handler polls .ta/interactions/answers/<id>.json.
    // We write this unconditionally so the MCP tool always gets the answer,
    // regardless of whether a QuestionRegistry entry exists.
    let answers_dir = state
        .project_root
        .join(".ta")
        .join("interactions")
        .join("answers");

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
            "responder_id": "api",
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
        responder_id: "api".to_string(),
        answered_at: now,
    };

    match state.question_registry.answer(id, answer).await {
        Ok(()) => {
            tracing::info!(
                interaction_id = %id,
                responder = "api",
                "Interaction answered"
            );
            Json(RespondResponse {
                interaction_id: id.to_string(),
                status: "delivered".to_string(),
            })
            .into_response()
        }
        Err(_returned_answer) => {
            // The QuestionRegistry has no matching entry. This is expected
            // when the question was written by the file-based MCP tool
            // (ta_ask_human) rather than registered in-process. The answer
            // file has already been written above, so the tool will receive
            // the response on its next poll iteration.
            tracing::info!(
                interaction_id = %id,
                "No in-process registry entry for interaction (file-based answer already written)"
            );
            Json(RespondResponse {
                interaction_id: id.to_string(),
                status: "delivered".to_string(),
            })
            .into_response()
        }
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
}
