//! `/api/whiteboard/*` — daemon-hosted whiteboard coordination endpoints.
//! Wraps the daemon's single shared `WhiteboardTransport` instance
//! (`AppState.whiteboard_transport`) so every agent process reaches the
//! same coordination state instead of each instantiating its own (see
//! `docs/superpowers/specs/2026-09-11-daemon-hosted-whiteboard-design.md`).
//!
//! Every endpoint authenticates the caller from the Biscuit token alone: the
//! team session, the role, the right to post outcomes and the execution
//! target are read from the verified token, and the daemon stamps and filters
//! by them. Nothing about the caller's identity is taken from a request body
//! (red-team CR-04; plan phase v0.17.12.2).

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use ta_agent_whiteboard::outcome_envelope::{OutcomeBody, Verified};
use ta_agent_whiteboard::presence::{self, PresenceRecord, DEFAULT_PRESENCE_TTL};
use ta_agent_whiteboard::{discovery, scopes};

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

type ErrResponse = (StatusCode, Json<serde_json::Value>);

fn err(status: StatusCode, message: String) -> ErrResponse {
    (status, Json(serde_json::json!({ "error": message })))
}

/// The caller as proven by its token.
#[derive(Debug, Clone)]
struct Caller {
    team_session: String,
    /// `None` for a role-less token minted before role binding.
    role: Option<String>,
    may_send_outcome: bool,
    target: String,
}

/// Verify `token` authorizes `whiteboard:team_session:<team_session>` and
/// read the role, outcome right and target out of it. Returns a structured
/// 403 (never a silent empty result) on failure.
fn authenticate(
    project_root: &std::path::Path,
    token: &str,
    team_session: &str,
) -> Result<Caller, ErrResponse> {
    let scope = scopes::session_scope(team_session);
    let broker =
        ta_credential_broker::CredentialBroker::open_for_project(project_root).map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("credential broker unavailable: {e}"),
            )
        })?;
    broker.authorize_scope(token, &scope).map_err(|e| {
        err(
            StatusCode::FORBIDDEN,
            format!("not authorized for {scope}: {e}"),
        )
    })?;
    let granted = broker.effective_scopes(token).map_err(|e| {
        err(
            StatusCode::FORBIDDEN,
            format!("not authorized for {scope}: {e}"),
        )
    })?;
    Ok(Caller {
        team_session: team_session.to_string(),
        role: scopes::role_from_scopes(&granted, team_session),
        may_send_outcome: scopes::may_send_outcome(&granted),
        target: scopes::target_from_scopes(&granted),
    })
}

fn whiteboard_disabled() -> ErrResponse {
    err(
        StatusCode::SERVICE_UNAVAILABLE,
        "[whiteboard] enabled = false for this project".to_string(),
    )
}

fn internal(e: impl std::fmt::Display) -> ErrResponse {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn register_presence(
    State(state): State<Arc<AppState>>,
    Json(mut req): Json<PresenceRegisterRequest>,
) -> impl IntoResponse {
    let caller = match authenticate(&state.project_root, &req.token, &req.team_session) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };
    let Some(transport) = &state.whiteboard_transport else {
        return whiteboard_disabled().into_response();
    };
    // Stamp the verified session; whatever the client put in the body is
    // discarded.
    if req
        .record
        .team_session
        .as_deref()
        .is_some_and(|claimed| claimed != caller.team_session)
    {
        tracing::warn!(
            claimed = ?req.record.team_session,
            verified = %caller.team_session,
            agent_id = %req.record.agent_id,
            "whiteboard presence: client body named a different team_session than its token; using the token's"
        );
    }
    req.record.team_session = Some(caller.team_session.clone());
    match presence::publish_presence(transport.as_ref(), &req.record, DEFAULT_PRESENCE_TTL).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn list_presence(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PresenceListQuery>,
) -> impl IntoResponse {
    let caller = match authenticate(&state.project_root, &q.token, &q.team_session) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };
    let Some(transport) = &state.whiteboard_transport else {
        return whiteboard_disabled().into_response();
    };
    match discovery::list_active_agents_in_session(transport.as_ref(), &caller.team_session).await {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(e) => internal(e).into_response(),
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
    let caller = match authenticate(&state.project_root, &req.token, &req.team_session) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };
    let Some(transport) = &state.whiteboard_transport else {
        return whiteboard_disabled().into_response();
    };
    let message =
        ta_agent_whiteboard::handoff::HandoffMessage::new(req.sender, req.recipient, req.payload)
            .with_team_session(caller.team_session);
    match ta_agent_whiteboard::handoff::send_handoff(transport.as_ref(), &message).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

pub async fn receive_handoff(
    State(state): State<Arc<AppState>>,
    Json(req): Json<HandoffReceiveRequest>,
) -> impl IntoResponse {
    let caller = match authenticate(&state.project_root, &req.token, &req.team_session) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };
    let Some(transport) = &state.whiteboard_transport else {
        return whiteboard_disabled().into_response();
    };
    match ta_agent_whiteboard::handoff::receive_handoff_in(
        transport.as_ref(),
        Some(&caller.team_session),
        &req.recipient,
    )
    .await
    {
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
        Err(e) => internal(e).into_response(),
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
    let caller = match authenticate(&state.project_root, &req.token, &req.team_session) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };
    let Some(transport) = &state.whiteboard_transport else {
        return whiteboard_disabled().into_response();
    };
    // The task id is namespaced by the verified session, so one session can
    // neither see nor claim another session's task.
    let scoped = ta_agent_whiteboard::tasks::scoped_task_id(&caller.team_session, &req.task_id);
    match ta_agent_whiteboard::tasks::claim_task(transport.as_ref(), &scoped, &req.agent_id).await {
        Ok(claimed) => (
            StatusCode::OK,
            Json(serde_json::json!({"claimed": claimed})),
        )
            .into_response(),
        Err(e) => internal(e).into_response(),
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
    let caller = match authenticate(&state.project_root, &req.token, &req.team_session) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };
    let Some(transport) = &state.whiteboard_transport else {
        return whiteboard_disabled().into_response();
    };
    let scoped = ta_agent_whiteboard::tasks::scoped_task_id(&caller.team_session, &req.task_id);
    match ta_agent_whiteboard::tasks::complete_task(transport.as_ref(), &scoped).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => internal(e).into_response(),
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
    /// A JSON-encoded outcome with exactly `candidate_id`, `outcome`,
    /// `detail` and optionally `new_task_title` (design doc §8.5). The daemon
    /// validates it against the strict schema and size limits, stamps it and
    /// signs it (contract:
    /// `docs/superpowers/specs/2026-10-11-outcome-envelope-contract.md`).
    pub payload: String,
    /// Optional goal the sender claims to be running. Checked against the goal
    /// store: it must belong to the token's role, otherwise the request is
    /// refused. When absent the daemon resolves the role's active goal.
    #[serde(default)]
    pub goal_id: Option<String>,
}

/// The goal to stamp: the caller's claimed goal if it really is one of this
/// role's goals, otherwise the role's newest active goal, otherwise
/// `unresolved` (logged) so the CoS loop keeps working when the goal store
/// cannot attribute a launch.
fn resolve_goal_id(
    goals_dir: &std::path::Path,
    role: &str,
    claimed: Option<&str>,
) -> Result<String, String> {
    use ta_goal::goal_run::GoalRunState;
    let Ok(store) = ta_goal::GoalRunStore::new(goals_dir) else {
        tracing::warn!(role, goals_dir = %goals_dir.display(), "whiteboard outcome: goal store unreadable, stamping goal_id=unresolved");
        return Ok("unresolved".to_string());
    };
    let goals = store.list().unwrap_or_default();
    if let Some(claimed) = claimed {
        if let Some(g) = goals
            .iter()
            .find(|g| g.goal_run_id.to_string() == claimed || g.tag.as_deref() == Some(claimed))
        {
            return if g.role.as_deref() == Some(role) {
                Ok(g.goal_run_id.to_string())
            } else {
                Err(format!(
                    "goal {claimed} does not belong to role {role:?} (it belongs to {:?}). Send the outcome from the goal this role launched, or omit goal_id.",
                    g.role
                ))
            };
        }
    }
    let active = |g: &&ta_goal::GoalRun| {
        g.role.as_deref() == Some(role)
            && matches!(
                g.state,
                GoalRunState::Created
                    | GoalRunState::Configured
                    | GoalRunState::Running
                    | GoalRunState::PrReady
                    | GoalRunState::UnderReview
            )
    };
    match goals.iter().filter(active).max_by_key(|g| g.created_at) {
        Some(g) => Ok(g.goal_run_id.to_string()),
        None => {
            tracing::warn!(role, "whiteboard outcome: no active goal found for this role, stamping goal_id=unresolved");
            Ok("unresolved".to_string())
        }
    }
}

/// `POST /api/whiteboard/outcome/send`: verifies the caller's role, stamps and
/// signs the outcome, and publishes the envelope onto the report-back stream.
/// No RoleRef addressing (unlike `send_handoff`): there is exactly one
/// intended reader (the Wayfinder poller).
pub async fn send_outcome(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OutcomeSendRequest>,
) -> impl IntoResponse {
    let caller = match authenticate(&state.project_root, &req.token, &req.team_session) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };
    let Some(transport) = &state.whiteboard_transport else {
        return whiteboard_disabled().into_response();
    };
    let cfg = ta_agent_whiteboard::WhiteboardConfig::load(&state.project_root);
    let Some(role) = caller.role.clone().filter(|_| caller.may_send_outcome) else {
        tracing::warn!(
            team_session = %caller.team_session,
            role = ?caller.role,
            "whiteboard outcome refused: token does not carry whiteboard:outcome:send"
        );
        let who = match &caller.role {
            Some(r) => format!("role {r:?}"),
            None => "a token with no role (minted before role binding)".to_string(),
        };
        return err(
            StatusCode::FORBIDDEN,
            format!(
                "outcome refused: {who} in session {:?} does not hold {}. Only launches of the roles listed in `[whiteboard] outcome_roles` ({}) are granted it. If this is the Chief-of-Staff, relaunch it through the daemon so it gets a fresh role token; workers report through drafts.",
                caller.team_session,
                scopes::OUTCOME_SEND_SCOPE,
                cfg.outcome_roles.join(", ")
            ),
        )
        .into_response();
    };
    let body: OutcomeBody = match serde_json::from_str(&req.payload) {
        Ok(b) => b,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!(
                    "outcome payload rejected: {e}. It must be a JSON object with exactly candidate_id, outcome, detail and optionally new_task_title (no other fields). Nothing was sent."
                ),
            )
            .into_response();
        }
    };
    let goal_id = match resolve_goal_id(&state.goals_dir, &role, req.goal_id.as_deref()) {
        Ok(g) => g,
        Err(msg) => return err(StatusCode::FORBIDDEN, msg).into_response(),
    };
    let subject = crate::outcome_stamp::StampSubject {
        team_session: caller.team_session.clone(),
        role: role.clone(),
        goal_id,
        target: caller.target.clone(),
    };
    let envelope = match crate::outcome_stamp::stamp_and_sign(&state.project_root, &subject, body) {
        Ok(e) => e,
        Err(msg) => {
            tracing::warn!(role = %role, error = %msg, "whiteboard outcome not stamped");
            return err(StatusCode::UNPROCESSABLE_ENTITY, msg).into_response();
        }
    };
    let (msg_id, kid) = (envelope.stamp.msg_id.clone(), envelope.sig.kid.clone());
    match transport
        .stream_append(EXTERNAL_OUTCOME_STREAM, envelope.to_bytes())
        .await
    {
        Ok(()) => {
            tracing::info!(
                role = %role,
                team_session = %caller.team_session,
                target = %caller.target,
                msg_id = %msg_id,
                kid = %kid,
                "whiteboard outcome stamped, signed and published"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "ok": true,
                    "msg_id": msg_id,
                    "role": role,
                    "kid": kid,
                })),
            )
                .into_response()
        }
        Err(e) => internal(e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct OutcomeCheckRequest {
    /// The stream payload exactly as read from the report-back stream.
    pub payload: String,
    /// Remember a verified `msg_id` so a second presentation is refused.
    /// Default false (a dry run).
    #[serde(default)]
    pub consume: bool,
}

/// `POST /api/whiteboard/outcome/check`: verify a report-back payload with
/// the daemon's own key registry and `[whiteboard]` settings
/// (`require_stamped_outcomes`, `outcome_max_age_secs`, `outcome_roles`).
/// This is the daemon-side enforcement point for unstamped envelopes and a
/// stepping stone for a consumer that does not yet verify locally.
pub async fn check_outcome(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OutcomeCheckRequest>,
) -> impl IntoResponse {
    match crate::outcome_stamp::check_payload(&state.project_root, req.payload.as_bytes(), req.consume)
    {
        Ok(Verified::Stamped { body, stamp, kid }) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "verified": true,
                "stamped": true,
                "kid": kid,
                "role": stamp.role,
                "team_session": stamp.team_session,
                "goal_id": stamp.goal_id,
                "target": stamp.target,
                "msg_id": stamp.msg_id,
                "issued_at": stamp.issued_at,
                "candidate_id": body.candidate_id,
                "outcome": body.outcome,
            })),
        )
            .into_response(),
        Ok(Verified::Legacy { body }) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "verified": false,
                "stamped": false,
                "legacy": true,
                "note": "unstamped legacy envelope accepted because [whiteboard] require_stamped_outcomes is off; it proves nothing about the sender",
                "candidate_id": body.candidate_id,
                "outcome": body.outcome,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "verified": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `GET /api/whiteboard/identity`: which project this daemon serves.
/// Whiteboard clients call this BEFORE sending a session token anywhere,
/// and refuse when the answer is not their own project root, so a
/// mis-resolved port can never hand one project's token to another
/// project's daemon. Gated only by the daemon's existing local-bypass auth,
/// like `presence_for_source`; it reveals nothing beyond the project path.
pub async fn identity(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "project_root": state.project_root.display().to_string(),
        })),
    )
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
        let broker = ta_credential_broker::CredentialBroker::open_for_project(dir).unwrap();
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

    /// A per-role token as `role_launch_env` mints it.
    fn mint_role_token(
        dir: &std::path::Path,
        team_session: &str,
        role: &str,
        outcome: bool,
    ) -> String {
        let broker = ta_credential_broker::CredentialBroker::open_for_project(dir).unwrap();
        let mut granted = vec![
            scopes::session_scope(team_session),
            scopes::role_scope(team_session, role),
            scopes::target_scope("local"),
        ];
        if outcome {
            granted.push(scopes::OUTCOME_SEND_SCOPE.to_string());
        }
        broker
            .grant(uuid::Uuid::new_v4(), "test-agent", granted, 3600)
            .unwrap()
            .token
    }

    async fn post_json(
        router: &axum::routing::RouterIntoService<Body>,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn outcome_payload(candidate: &str, outcome: &str) -> String {
        serde_json::json!({
            "candidate_id": candidate,
            "outcome": outcome,
            "detail": "closed the loop",
            "new_task_title": null,
        })
        .to_string()
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

        let record =
            PresenceRecord::new("agent-1", "goal-1", "/tmp/proj").with_team_session("sess-1");
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
            &ta_agent_whiteboard::tasks::WhiteboardTask::new(
                ta_agent_whiteboard::tasks::scoped_task_id("sess-1", "t1"),
                "write the docs",
            ),
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
            &ta_agent_whiteboard::tasks::WhiteboardTask::new(
                ta_agent_whiteboard::tasks::scoped_task_id("sess-1", "t1"),
                "write the docs",
            ),
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
        let scoped = ta_agent_whiteboard::tasks::scoped_task_id("sess-1", "t1");
        let task = tasks.iter().find(|t| t.id == scoped).unwrap();
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
        let token = mint_role_token(dir.path(), "sess-1", "chief-of-staff", true);
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

    // ── CR-04: role-bound outcomes ─────────────────────────────────────────

    #[tokio::test]
    async fn a_worker_role_token_cannot_send_an_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let transport = state.whiteboard_transport.clone().unwrap();
        let worker = mint_role_token(dir.path(), "sess-1", "implementer", false);
        let router = crate::api::build_api_router(state).into_service();

        let (status, body) = post_json(
            &router,
            "/api/whiteboard/outcome/send",
            serde_json::json!({
                "token": worker, "team_session": "sess-1",
                "payload": outcome_payload("wayfinder-task:t1#n", "delegate"),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let msg = body["error"].as_str().unwrap();
        assert!(
            msg.contains("implementer") && msg.contains("whiteboard:outcome:send"),
            "{msg}"
        );
        assert!(
            transport
                .stream_read_next(EXTERNAL_OUTCOME_STREAM, "c")
                .await
                .unwrap()
                .is_none(),
            "nothing may reach the stream"
        );
    }

    #[tokio::test]
    async fn a_role_less_legacy_token_cannot_send_an_outcome_but_can_still_use_presence() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let legacy = mint_test_token(dir.path(), "sess-1");
        let router = crate::api::build_api_router(state).into_service();
        let (status, body) = post_json(
            &router,
            "/api/whiteboard/outcome/send",
            serde_json::json!({
                "token": legacy, "team_session": "sess-1",
                "payload": outcome_payload("wayfinder-task:t1", "done"),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().unwrap().contains("no role"));
        let (status, _) = post_json(
            &router,
            "/api/whiteboard/presence",
            serde_json::json!({
                "token": legacy, "team_session": "sess-1",
                "record": PresenceRecord::new("a", "g", "/tmp"),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_token_for_one_session_does_not_authorize_another() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let token = mint_role_token(dir.path(), "sess-1", "chief-of-staff", true);
        let router = crate::api::build_api_router(state).into_service();
        let (status, _) = post_json(
            &router,
            "/api/whiteboard/outcome/send",
            serde_json::json!({
                "token": token, "team_session": "sess-2",
                "payload": outcome_payload("c", "done"),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    async fn send_as_cos(
        router: &axum::routing::RouterIntoService<Body>,
        token: &str,
        candidate: &str,
    ) -> serde_json::Value {
        let (status, body) = post_json(
            router,
            "/api/whiteboard/outcome/send",
            serde_json::json!({
                "token": token, "team_session": "sess-1",
                "payload": outcome_payload(candidate, "reply"),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    #[tokio::test]
    async fn a_cos_outcome_is_stamped_and_carries_a_verifying_signature() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let transport = state.whiteboard_transport.clone().unwrap();
        let token = mint_role_token(dir.path(), "sess-1", "chief-of-staff", true);
        let router = crate::api::build_api_router(state).into_service();
        let sent = send_as_cos(&router, &token, "wayfinder-task:t1#n").await;

        let env = transport
            .stream_read_next(EXTERNAL_OUTCOME_STREAM, "poller")
            .await
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&env.payload).unwrap();
        // Additive: the original fields are intact.
        assert_eq!(v["candidate_id"], "wayfinder-task:t1#n");
        assert_eq!(v["outcome"], "reply");
        assert_eq!(v["detail"], "closed the loop");
        // New: version, stamp, signature.
        assert_eq!(v["v"], 1);
        assert_eq!(v["stamp"]["role"], "chief-of-staff");
        assert_eq!(v["stamp"]["team_session"], "sess-1");
        assert_eq!(v["stamp"]["target"], "local");
        assert_eq!(v["stamp"]["msg_id"], sent["msg_id"]);
        assert!(v["stamp"]["goal_id"].is_string() && v["stamp"]["issued_at"].is_i64());

        // A poller holding the public registry (and nothing else) verifies it.
        let ps = ta_credentials::state::ProjectState::open(
            dir.path(),
            &ta_credentials::state::StateLocator::from_env(),
        )
        .unwrap();
        let raw = std::fs::read_to_string(
            ta_credentials::outcome_keys::OutcomeKeyStore::registry_path_for(&ps),
        )
        .unwrap();
        assert!(!raw.contains("seed") && !raw.contains("private"));
        let ring =
            ta_agent_whiteboard::outcome_envelope::KeyRing::from_registry_json(&raw).unwrap();
        let verified = ta_agent_whiteboard::outcome_envelope::verify_payload(
            &env.payload,
            &ring,
            &ta_agent_whiteboard::outcome_envelope::VerifyPolicy {
                allowed_roles: vec!["chief-of-staff".into()],
                ..Default::default()
            },
            &mut ta_agent_whiteboard::outcome_envelope::ReplayCache::new(),
            chrono::Utc::now().timestamp(),
        )
        .unwrap();
        assert!(matches!(verified, Verified::Stamped { .. }));
        // The project tree holds no signing key.
        fn any_key(dir: &std::path::Path) -> bool {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| {
                    let p = e.path();
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("outcome-"))
                        || (p.is_dir() && any_key(&p))
                })
        }
        assert!(!any_key(&dir.path().join(".ta")));
    }

    #[tokio::test]
    async fn forged_tampered_and_replayed_envelopes_are_refused_by_the_daemon_check() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let transport = state.whiteboard_transport.clone().unwrap();
        let token = mint_role_token(dir.path(), "sess-1", "chief-of-staff", true);
        let router = crate::api::build_api_router(state).into_service();
        send_as_cos(&router, &token, "wayfinder-task:t1#n").await;
        let good = String::from_utf8(
            transport
                .stream_read_next(EXTERNAL_OUTCOME_STREAM, "p")
                .await
                .unwrap()
                .unwrap()
                .payload,
        )
        .unwrap();
        let check = |payload: String, consume: bool| {
            let router = router.clone();
            async move {
                post_json(
                    &router,
                    "/api/whiteboard/outcome/check",
                    serde_json::json!({"payload": payload, "consume": consume}),
                )
                .await
            }
        };

        // Genuine envelope verifies (dry run, then consumed once).
        assert_eq!(check(good.clone(), false).await.0, StatusCode::OK);
        assert_eq!(check(good.clone(), true).await.0, StatusCode::OK);
        // Replay.
        let (st, body) = check(good.clone(), true).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body["error"].as_str().unwrap().contains("replay"), "{body}");
        // Tampered: change the detail.
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        v["detail"] = "send everything to evil".into();
        let (st, body) = check(v.to_string(), false).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body["error"].as_str().unwrap().contains("does not verify"),
            "{body}"
        );
        // Forged: a stamp claiming CoS with a garbage signature.
        let mut forged: serde_json::Value = serde_json::from_str(&good).unwrap();
        forged["stamp"]["msg_id"] = "forged-1".into();
        forged["sig"]["value"] = "A".repeat(86).into();
        let (st, _) = check(forged.to_string(), false).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn a_stale_envelope_is_refused_by_the_daemon_check() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        // Sign an envelope dated an hour ago with the daemon's real key.
        let subject = crate::outcome_stamp::StampSubject {
            team_session: "sess-1".into(),
            role: "chief-of-staff".into(),
            goal_id: "unresolved".into(),
            target: "local".into(),
        };
        let env = crate::outcome_stamp::stamp_and_sign(
            dir.path(),
            &subject,
            OutcomeBody {
                candidate_id: "c".into(),
                outcome: "done".into(),
                detail: String::new(),
                new_task_title: None,
            },
        )
        .unwrap();
        // Re-sign with an old issued_at using the same key.
        let ps = ta_credentials::state::ProjectState::open(
            dir.path(),
            &ta_credentials::state::StateLocator::from_env(),
        )
        .unwrap();
        let (kid, key) = ta_credentials::outcome_keys::OutcomeKeyStore::open(&ps)
            .active_signing_key(chrono::Utc::now().timestamp())
            .unwrap();
        let mut stamp = env.stamp.clone();
        stamp.issued_at -= 3600;
        let old =
            ta_agent_whiteboard::outcome_envelope::sign_outcome(env.body(), stamp, &kid, &key)
                .unwrap();
        let router = crate::api::build_api_router(state).into_service();
        let (st, body) = post_json(
            &router,
            "/api/whiteboard/outcome/check",
            serde_json::json!({"payload": String::from_utf8(old.to_bytes()).unwrap()}),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body["error"].as_str().unwrap().contains("stale"), "{body}");
    }

    #[tokio::test]
    async fn legacy_unstamped_flows_unless_require_stamped_outcomes_is_on() {
        let legacy = outcome_payload("wayfinder-task:t1", "done");
        // Off (the default posture): accepted, flagged as proving nothing.
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let router = crate::api::build_api_router(state).into_service();
        let (st, body) = post_json(
            &router,
            "/api/whiteboard/outcome/check",
            serde_json::json!({"payload": legacy}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["legacy"], true);

        // On: refused, with the setting named.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = true\ntransport = \"memory\"\nrequire_stamped_outcomes = true\n",
        )
        .unwrap();
        let state = Arc::new(AppState::new(
            dir.path().to_path_buf(),
            DaemonConfig::default(),
        ));
        let router = crate::api::build_api_router(state).into_service();
        let (st, body) = post_json(
            &router,
            "/api/whiteboard/outcome/check",
            serde_json::json!({"payload": legacy}),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("require_stamped_outcomes"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn outcome_with_unknown_fields_is_rejected_before_anything_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let transport = state.whiteboard_transport.clone().unwrap();
        let token = mint_role_token(dir.path(), "sess-1", "chief-of-staff", true);
        let router = crate::api::build_api_router(state).into_service();
        for payload in [
            serde_json::json!({"candidate_id":"c","outcome":"delegate","detail":"","tag":"x","objective":"y"}).to_string(),
            serde_json::json!({"candidate_id":"c","outcome":"approve","detail":""}).to_string(),
            serde_json::json!({"candidate_id":"c","outcome":"done","detail":"x".repeat(20_000)}).to_string(),
            "not json".to_string(),
        ] {
            let (st, body) = post_json(
                &router,
                "/api/whiteboard/outcome/send",
                serde_json::json!({"token": token, "team_session": "sess-1", "payload": payload}),
            )
            .await;
            assert!(
                st == StatusCode::BAD_REQUEST || st == StatusCode::UNPROCESSABLE_ENTITY,
                "{st} {body}"
            );
        }
        assert!(transport
            .stream_read_next(EXTERNAL_OUTCOME_STREAM, "c")
            .await
            .unwrap()
            .is_none());
    }

    // ── Items 6 and 7: per-session isolation ───────────────────────────────

    #[tokio::test]
    async fn two_sessions_on_one_daemon_see_only_their_own_presence() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let t1 = mint_role_token(dir.path(), "sess-1", "implementer", false);
        let t2 = mint_role_token(dir.path(), "sess-2", "implementer", false);
        let router = crate::api::build_api_router(state).into_service();

        // The SAME agent id in both sessions must not collide either.
        for (token, session, goal) in [(&t1, "sess-1", "g1"), (&t2, "sess-2", "g2")] {
            let (st, _) = post_json(
                &router,
                "/api/whiteboard/presence",
                serde_json::json!({
                    "token": token, "team_session": session,
                    "record": PresenceRecord::new("shared-agent", goal, "/tmp/proj"),
                }),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
        }
        let (st, _) = post_json(
            &router,
            "/api/whiteboard/presence",
            serde_json::json!({
                "token": t1, "team_session": "sess-1",
                // A client claiming another session in the body is overridden.
                "record": PresenceRecord::new("only-in-1", "g1b", "/tmp/proj").with_team_session("sess-2"),
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK);

        let list = |token: String, session: &'static str| {
            let router = router.clone();
            async move {
                let resp = router
                    .oneshot(
                        Request::builder()
                            .uri(format!(
                                "/api/whiteboard/presence?team_session={session}&token={token}"
                            ))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                serde_json::from_slice::<Vec<PresenceRecord>>(&bytes).unwrap()
            }
        };
        let one = list(t1.clone(), "sess-1").await;
        let two = list(t2.clone(), "sess-2").await;
        assert_eq!(one.len(), 2, "{one:?}");
        assert!(one
            .iter()
            .all(|r| r.team_session.as_deref() == Some("sess-1")));
        assert!(one.iter().any(|r| r.goal_run_id == "g1"));
        assert!(!one.iter().any(|r| r.goal_run_id == "g2"));
        assert_eq!(two.len(), 1, "{two:?}");
        assert_eq!(two[0].goal_run_id, "g2");
    }

    #[tokio::test]
    async fn presence_for_source_stays_unfiltered_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let transport = state.whiteboard_transport.clone().unwrap();
        for s in ["sess-1", "sess-2"] {
            presence::publish_presence(
                transport.as_ref(),
                &PresenceRecord::new("a", "g", "/tmp/proj").with_team_session(s),
                DEFAULT_PRESENCE_TTL,
            )
            .await
            .unwrap();
        }
        let all = discovery::active_agents_for_source(transport.as_ref(), "/tmp/proj")
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn tasks_and_handoffs_are_isolated_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state_with_whiteboard_enabled(dir.path());
        let transport = state.whiteboard_transport.clone().unwrap();
        let t1 = mint_role_token(dir.path(), "sess-1", "implementer", false);
        let t2 = mint_role_token(dir.path(), "sess-2", "implementer", false);
        // A task published for sess-1 only.
        ta_agent_whiteboard::tasks::publish_task(
            transport.as_ref(),
            &ta_agent_whiteboard::tasks::WhiteboardTask::new(
                ta_agent_whiteboard::tasks::scoped_task_id("sess-1", "t1"),
                "docs",
            ),
        )
        .await
        .unwrap();
        let router = crate::api::build_api_router(state).into_service();
        let claim = |tok: &str, sess: &str| serde_json::json!({"token": tok, "team_session": sess, "task_id": "t1", "agent_id": "a"});
        let (_, other) =
            post_json(&router, "/api/whiteboard/tasks/claim", claim(&t2, "sess-2")).await;
        assert_eq!(
            other["claimed"], false,
            "sess-2 must not claim sess-1's task"
        );
        let (_, own) =
            post_json(&router, "/api/whiteboard/tasks/claim", claim(&t1, "sess-1")).await;
        assert_eq!(own["claimed"], true);

        // A handoff sent in sess-1 is not delivered in sess-2.
        post_json(
            &router,
            "/api/whiteboard/handoff/send",
            serde_json::json!({"token": t1, "team_session": "sess-1", "sender": "a",
                "recipient": {"agent": "x"}, "payload": "private"}),
        )
        .await;
        let recv = |tok: &str, sess: &str| serde_json::json!({"token": tok, "team_session": sess, "recipient": {"agent": "x"}});
        let (_, none) = post_json(
            &router,
            "/api/whiteboard/handoff/receive",
            recv(&t2, "sess-2"),
        )
        .await;
        assert!(none.is_null(), "{none}");
        let (_, got) = post_json(
            &router,
            "/api/whiteboard/handoff/receive",
            recv(&t1, "sess-1"),
        )
        .await;
        assert_eq!(got["payload"], "private");
    }
}
