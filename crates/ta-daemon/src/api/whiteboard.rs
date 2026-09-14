//! `/api/whiteboard/*` — daemon-hosted whiteboard coordination endpoints.
//! Wraps the daemon's single shared `WhiteboardTransport` instance
//! (`AppState.whiteboard_transport`) so every agent process reaches the
//! same coordination state instead of each instantiating its own (see
//! `docs/superpowers/specs/2026-09-11-daemon-hosted-whiteboard-design.md`).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use ta_agent_whiteboard::discovery;
use ta_agent_whiteboard::presence::{self, PresenceRecord, DEFAULT_PRESENCE_TTL};

use crate::api::AppState;

#[derive(Debug, Deserialize)]
pub struct PresenceRegisterRequest {
    pub token: String,
    pub team_session: String,
    pub record: PresenceRecord,
}

#[derive(Debug, Deserialize)]
pub struct PresenceListQuery {
    pub team_session: String,
    pub token: String,
}

/// Verify `token` authorizes `whiteboard:team_session:<team_session>`,
/// returning a structured 403 (never a silent empty result) on failure.
fn require_whiteboard_scope(
    project_root: &std::path::Path,
    token: &str,
    team_session: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let scope = format!("whiteboard:team_session:{team_session}");
    let broker =
        ta_credential_broker::CredentialBroker::open(&project_root.join(".ta")).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("credential broker unavailable: {e}")})),
            )
        })?;
    broker.authorize_scope(token, &scope).map_err(|e| {
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": format!("not authorized for {scope}: {e}")})),
        )
    })?;
    Ok(())
}

pub async fn register_presence(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PresenceRegisterRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    match presence::publish_presence(transport.as_ref(), &req.record, DEFAULT_PRESENCE_TTL).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn list_presence(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PresenceListQuery>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &q.token, &q.team_session) {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    match discovery::list_active_agents(transport.as_ref()).await {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct HandoffSendRequest {
    pub token: String,
    pub team_session: String,
    pub sender: String,
    pub recipient: ta_session::RoleRef,
    pub payload: String,
}

#[derive(Debug, Deserialize)]
pub struct HandoffReceiveRequest {
    pub token: String,
    pub team_session: String,
    pub recipient: ta_session::RoleRef,
}

pub async fn send_handoff(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HandoffSendRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    let message =
        ta_agent_whiteboard::handoff::HandoffMessage::new(req.sender, req.recipient, req.payload);
    match ta_agent_whiteboard::handoff::send_handoff(transport.as_ref(), &message).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn receive_handoff(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HandoffReceiveRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    match ta_agent_whiteboard::handoff::receive_handoff(transport.as_ref(), &req.recipient).await {
        Ok(Some(delivered)) => {
            // Auto-ack on delivery: the MCP tool call itself is the
            // consumption point (the agent already has the payload once
            // this response returns), matching this project's other
            // at-least-once-but-simple primitives rather than adding a
            // second round-trip for explicit ack.
            let payload =
                serde_json::to_value(&delivered.message).unwrap_or(serde_json::Value::Null);
            if let Err(e) =
                ta_agent_whiteboard::handoff::ack_handoff(transport.as_ref(), delivered).await
            {
                tracing::warn!(error = %e, "whiteboard: failed to ack delivered handoff, may be redelivered");
            }
            (StatusCode::OK, Json(payload)).into_response()
        }
        Ok(None) => (StatusCode::OK, Json(serde_json::Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TaskClaimRequest {
    pub token: String,
    pub team_session: String,
    pub task_id: String,
    pub agent_id: String,
}

pub async fn claim_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskClaimRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    match ta_agent_whiteboard::tasks::claim_task(transport.as_ref(), &req.task_id, &req.agent_id)
        .await
    {
        Ok(claimed) => (
            StatusCode::OK,
            Json(serde_json::json!({"claimed": claimed})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TaskCompleteRequest {
    pub token: String,
    pub team_session: String,
    pub task_id: String,
}

pub async fn complete_task(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TaskCompleteRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    match ta_agent_whiteboard::tasks::complete_task(transport.as_ref(), &req.task_id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// The report-back stream's well-known name (v0.17.11.11) — must match the
/// constant `ta-virtual-team`'s poller reads from (`EXTERNAL_OUTCOME_STREAM`
/// in that repo's `src/wayfinder/poller.rs`), the mirror image of
/// `wake_listener.rs`'s `external-intake` stream: chief-of-staff publishes
/// here after triaging Wayfinder-sourced work, the poller drains it and
/// turns each message into a `PATCH`/`POST` back to Wayfinder.
pub const EXTERNAL_OUTCOME_STREAM: &str = "external-outcome";

#[derive(Debug, Deserialize)]
pub struct OutcomeSendRequest {
    pub token: String,
    pub team_session: String,
    /// Opaque to the daemon — a JSON-encoded `outcome` message (design doc
    /// §8.5: `candidate_id`, `outcome`, `detail`, ...). The daemon only
    /// carries bytes; interpreting them is the poller's job on the other
    /// end, same "poller never judges" principle §4 already establishes for
    /// the intake direction.
    pub payload: String,
}

/// `POST /api/whiteboard/outcome/send` — publishes `payload` onto the
/// report-back stream. No RoleRef addressing (unlike `send_handoff`): there
/// is exactly one intended reader (the Wayfinder poller), the same
/// single-consumer shape `external-intake` already has in the other
/// direction.
pub async fn send_outcome(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OutcomeSendRequest>,
) -> impl IntoResponse {
    if let Err(resp) = require_whiteboard_scope(&state.project_root, &req.token, &req.team_session)
    {
        return resp.into_response();
    }
    let Some(transport) = &state.whiteboard_transport else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "[whiteboard] enabled = false for this project"})),
        )
            .into_response();
    };
    match transport
        .stream_append(EXTERNAL_OUTCOME_STREAM, req.payload.into_bytes())
        .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct PresenceForSourceQuery {
    pub source_dir: String,
}

/// Advisory-only pre-launch query — no whiteboard scope required (this
/// runs before any team-session/goal exists to mint one against), gated
/// only by the daemon's existing local-bypass auth like every other
/// same-machine caller. Mirrors `discovery::active_agents_for_source`.
pub async fn presence_for_source(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PresenceForSourceQuery>,
) -> impl IntoResponse {
    let Some(transport) = &state.whiteboard_transport else {
        return (StatusCode::OK, Json(Vec::<PresenceRecord>::new())).into_response();
    };
    match discovery::active_agents_for_source(transport.as_ref(), &q.source_dir).await {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::DaemonConfig;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state_with_whiteboard_enabled(dir: &std::path::Path) -> Arc<AppState> {
        std::fs::create_dir_all(dir.join(".ta")).unwrap();
        std::fs::write(
            dir.join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = true\ntransport = \"memory\"\n",
        )
        .unwrap();
        Arc::new(AppState::new(dir.to_path_buf(), DaemonConfig::default()))
    }

    fn mint_test_token(dir: &std::path::Path, team_session: &str) -> String {
        let broker = ta_credential_broker::CredentialBroker::open(&dir.join(".ta")).unwrap();
        broker
            .grant(
                uuid::Uuid::new_v4(),
                "test-agent",
                vec![format!("whiteboard:team_session:{team_session}")],
                3600,
            )
            .unwrap()
            .token
    }

    #[tokio::test]
    async fn register_presence_without_valid_scope_returns_403() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": "not-a-real-token",
            "team_session": "sess-1",
            "record": PresenceRecord::new("agent-1", "goal-1", "/tmp"),
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/presence")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn register_then_list_presence_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_test_token(dir.path(), "sess-1");

        let record = PresenceRecord::new("agent-1", "goal-1", "/tmp/proj");
        presence::publish_presence(
            state.whiteboard_transport.as_ref().unwrap().as_ref(),
            &record,
            DEFAULT_PRESENCE_TTL,
        )
        .await
        .unwrap();

        let router = crate::api::build_api_router(state).into_service();
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/whiteboard/presence?team_session=sess-1&token={token}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let records: Vec<PresenceRecord> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].agent_id, "agent-1");
    }

    #[tokio::test]
    async fn register_presence_with_valid_scope_returns_200_ok() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "record": PresenceRecord::new("agent-1", "goal-1", "/tmp"),
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/presence")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp_body.get("ok").and_then(|v| v.as_bool()), Some(true));
    }

    #[tokio::test]
    async fn send_then_receive_handoff_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();

        let send_body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "sender": "chief-of-staff",
            "recipient": {"agent": "implementer"},
            "payload": "please review the draft"
        });
        let send_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/handoff/send")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&send_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(send_resp.status(), StatusCode::OK);

        let receive_body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "recipient": {"agent": "implementer"}
        });
        let receive_resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/handoff/receive")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&receive_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(receive_resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(receive_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let message: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(message["payload"], "please review the draft");
    }

    #[tokio::test]
    async fn claim_task_prevents_double_claim() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_test_token(dir.path(), "sess-1");
        let transport = state.whiteboard_transport.clone().unwrap();

        ta_agent_whiteboard::tasks::publish_task(
            transport.as_ref(),
            &ta_agent_whiteboard::tasks::WhiteboardTask::new("t1", "write the docs"),
        )
        .await
        .unwrap();

        let router = crate::api::build_api_router(state).into_service();
        let claim_body = |agent: &str| serde_json::json!({"token": token, "team_session": "sess-1", "task_id": "t1", "agent_id": agent});

        let first = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/tasks/claim")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&claim_body("agent-a")).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let first_bytes = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_bytes).unwrap();
        assert_eq!(first_json["claimed"], true);

        let second = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/tasks/claim")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&claim_body("agent-b")).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let second_bytes = axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second_bytes).unwrap();
        assert_eq!(second_json["claimed"], false);
    }

    #[tokio::test]
    async fn complete_task_marks_task_done() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_test_token(dir.path(), "sess-1");
        let transport = state.whiteboard_transport.clone().unwrap();

        ta_agent_whiteboard::tasks::publish_task(
            transport.as_ref(),
            &ta_agent_whiteboard::tasks::WhiteboardTask::new("t1", "write the docs"),
        )
        .await
        .unwrap();

        let router = crate::api::build_api_router(state).into_service();
        let body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "task_id": "t1",
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/tasks/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp_body.get("ok").and_then(|v| v.as_bool()), Some(true));

        let tasks = ta_agent_whiteboard::tasks::list_tasks(transport.as_ref())
            .await
            .unwrap();
        let task = tasks.iter().find(|t| t.id == "t1").unwrap();
        assert_eq!(task.status, ta_agent_whiteboard::tasks::TaskStatus::Done);
    }

    #[tokio::test]
    async fn presence_for_source_needs_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let transport = state.whiteboard_transport.clone().unwrap();
        presence::publish_presence(
            transport.as_ref(),
            &PresenceRecord::new("agent-1", "goal-1", "/tmp/proj"),
            DEFAULT_PRESENCE_TTL,
        )
        .await
        .unwrap();

        let router = crate::api::build_api_router(state).into_service();
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/whiteboard/presence_for_source?source_dir=/tmp/proj")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn complete_task_with_whiteboard_disabled_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = false\n",
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            dir.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        let token = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "task_id": "t1",
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/tasks/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            resp_body.get("error").and_then(|v| v.as_str()),
            Some("[whiteboard] enabled = false for this project")
        );
    }

    #[tokio::test]
    async fn list_presence_with_whiteboard_disabled_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = false\n",
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            dir.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        let token = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/whiteboard/presence?team_session=sess-1&token={token}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            resp_body.get("error").and_then(|v| v.as_str()),
            Some("[whiteboard] enabled = false for this project")
        );
    }

    #[tokio::test]
    async fn receive_handoff_with_whiteboard_disabled_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = false\n",
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            dir.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        let token = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "recipient": {"agent": "implementer"},
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/handoff/receive")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            resp_body.get("error").and_then(|v| v.as_str()),
            Some("[whiteboard] enabled = false for this project")
        );
    }

    #[tokio::test]
    async fn claim_task_with_whiteboard_disabled_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = false\n",
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            dir.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        let token = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "task_id": "t1",
            "agent_id": "agent-a",
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/tasks/claim")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            resp_body.get("error").and_then(|v| v.as_str()),
            Some("[whiteboard] enabled = false for this project")
        );
    }

    #[tokio::test]
    async fn send_outcome_with_whiteboard_disabled_returns_503() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = false\n",
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            dir.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        let token = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "payload": "{}",
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/outcome/send")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn send_outcome_without_valid_scope_returns_403() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": "not-a-real-token",
            "team_session": "sess-1",
            "payload": "{}",
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/outcome/send")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn send_outcome_publishes_onto_the_external_outcome_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_test_token(dir.path(), "sess-1");
        let transport = state.whiteboard_transport.clone().unwrap();
        let router = crate::api::build_api_router(state).into_service();

        let body = serde_json::json!({
            "token": token,
            "team_session": "sess-1",
            "payload": serde_json::json!({
                "candidate_id": "wayfinder-task:t1",
                "outcome": "done",
                "detail": "closed the loop",
            }).to_string(),
        });
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/whiteboard/outcome/send")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let envelope = transport
            .stream_read_next(EXTERNAL_OUTCOME_STREAM, "test-consumer")
            .await
            .unwrap()
            .expect("the outcome payload should have been published");
        let payload: serde_json::Value = serde_json::from_slice(&envelope.payload).unwrap();
        assert_eq!(payload["candidate_id"], "wayfinder-task:t1");
        assert_eq!(payload["outcome"], "done");
    }

    /// The single most important test in this crate's whiteboard work:
    /// binds a REAL TCP listener and drives the real router through two
    /// independent `WhiteboardDaemonClient` instances (real HTTP calls,
    /// not `oneshot`) — simulating two separate `ta serve` subprocesses,
    /// which is exactly the scenario the original red-teamed bug missed
    /// (each per-agent process held its own `InMemoryTransport`, so two
    /// concurrent agents silently never saw each other). Proves presence
    /// is shared via the daemon's single owned `AppState.whiteboard_transport`
    /// rather than any in-process shortcut `oneshot` would paper over.
    #[tokio::test]
    async fn two_concurrent_agent_processes_both_see_each_other_via_presence() {
        use std::net::SocketAddr;
        use ta_mcp_gateway::daemon_client::WhiteboardDaemonClient;
        use tokio::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token_a = mint_test_token(dir.path(), "sess-1");
        let token_b = mint_test_token(dir.path(), "sess-1");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // WhiteboardDaemonClient::new resolves its base_url from
        // `.ta/daemon.pid`'s `port=` line (see daemon_client.rs), so
        // pointing both independent clients at the real bound port means
        // writing it here exactly as the real daemon startup path does.
        std::fs::write(dir.path().join(".ta/daemon.pid"), format!("port={port}\n")).unwrap();

        let router = crate::api::build_api_router(state);
        tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        // Two independent clients (own reqwest::Client, own connections) —
        // not the same client called twice — simulating two separate agent
        // processes talking to the one real daemon over the network.
        let client_a = WhiteboardDaemonClient::new(dir.path());
        let client_b = WhiteboardDaemonClient::new(dir.path());

        client_a
            .register_presence(
                &token_a,
                "sess-1",
                &PresenceRecord::new("agent-a", "goal-a", "/tmp/proj"),
            )
            .await
            .unwrap();
        client_b
            .register_presence(
                &token_b,
                "sess-1",
                &PresenceRecord::new("agent-b", "goal-b", "/tmp/proj"),
            )
            .await
            .unwrap();

        let seen_by_a = client_a.list_presence(&token_a, "sess-1").await.unwrap();
        let seen_by_b = client_b.list_presence(&token_b, "sess-1").await.unwrap();

        assert_eq!(seen_by_a.len(), 2, "agent-a should see both agents");
        assert_eq!(seen_by_b.len(), 2, "agent-b should see both agents");
        assert!(seen_by_a.iter().any(|r| r.agent_id == "agent-b"));
        assert!(seen_by_b.iter().any(|r| r.agent_id == "agent-a"));
    }
}
