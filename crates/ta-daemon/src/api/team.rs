// api/team.rs — Team/role assignment API (v0.17.0.12.17 item 3).
//
// `.ta/team.toml` (ta_session::team::TeamConfig) already existed as of
// v0.17.0.3 but had no Studio UI at all — confirmed zero mentions of
// team/role/reviewer/implementer in Studio's render code
// (docs/design/ta-concepts-and-architecture.md §6). This is that UI's
// backend: list current role assignments and assign/remove one.
//
// GET    /api/team          — list current role → agent/security/persona assignments
// POST   /api/team/assign   — assign (or update) the agent/security/persona for a role
// DELETE /api/team/{role}   — remove a role's assignment

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use ta_session::agent_action::TeamRole;
use ta_session::team::TeamConfig;
use ta_session::workflow_session::AdvisorSecurity;

use super::AppState;

/// `GET /api/team` — list current role assignments from `.ta/team.toml`.
pub async fn list_team(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    match TeamConfig::load(&project_root) {
        Ok(config) => Json(json!({ "members": config.members })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to read .ta/team.toml: {}", e)})),
        )
            .into_response(),
    }
}

/// Request body for `POST /api/team/assign`.
#[derive(Debug, Deserialize)]
pub struct AssignTeamMemberRequest {
    pub role: String,
    pub agent_id: String,
    #[serde(default)]
    pub security: AdvisorSecurity,
    #[serde(default)]
    pub persona: Option<String>,
}

/// `POST /api/team/assign` — assign (or update) the agent/security/persona
/// backing a role. Upserts by role name, matching `TeamConfig::assign`.
pub async fn assign_team_member(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AssignTeamMemberRequest>,
) -> impl IntoResponse {
    let role_name = body.role.trim();
    if role_name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "role is required"})),
        )
            .into_response();
    }
    let agent_id = body.agent_id.trim();
    if agent_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "agent_id is required"})),
        )
            .into_response();
    }

    let project_root = state.active_project_root.read().unwrap().clone();
    let mut config = match TeamConfig::load(&project_root) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to read .ta/team.toml: {}", e)})),
            )
                .into_response()
        }
    };

    config.assign(
        TeamRole::new(role_name),
        agent_id.to_string(),
        body.security,
        body.persona.filter(|p| !p.trim().is_empty()),
    );

    match config.save(&project_root) {
        Ok(()) => Json(json!({ "ok": true, "members": config.members })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save .ta/team.toml: {}", e)})),
        )
            .into_response(),
    }
}

/// `DELETE /api/team/{role}` — remove a role's assignment.
pub async fn remove_team_member(
    State(state): State<Arc<AppState>>,
    Path(role): Path<String>,
) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    let mut config = match TeamConfig::load(&project_root) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to read .ta/team.toml: {}", e)})),
            )
                .into_response()
        }
    };

    let before = config.members.len();
    config.members.retain(|m| m.role.as_str() != role);
    if config.members.len() == before {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("No team member assigned to role '{}'", role)})),
        )
            .into_response();
    }

    match config.save(&project_root) {
        Ok(()) => Json(json!({ "ok": true, "members": config.members })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to save .ta/team.toml: {}", e)})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use axum::routing::{delete, get, post};
    use axum::Router;
    use tower::ServiceExt;

    fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new(
            std::path::PathBuf::from(dir.path()),
            crate::config::DaemonConfig::default(),
        ));
        (state, dir)
    }

    fn test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/team", get(list_team))
            .route("/api/team/assign", post(assign_team_member))
            .route("/api/team/{role}", delete(remove_team_member))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_team_empty_when_no_team_toml() {
        let (state, _dir) = test_state();
        let app = test_router(state);
        let resp = app
            .oneshot(Request::get("/api/team").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(val["members"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn assign_then_list_round_trips() {
        let (state, _dir) = test_state();
        let app = test_router(state.clone());
        let resp = app
            .oneshot(
                Request::post("/api/team/assign")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "role": "reviewer",
                            "agent_id": "claude-sonnet-4-6",
                            "security": "auto",
                            "persona": "strict-reviewer"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let app = test_router(state);
        let resp = app
            .oneshot(Request::get("/api/team").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let members = val["members"].as_array().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0]["role"], "reviewer");
        assert_eq!(members[0]["agent_id"], "claude-sonnet-4-6");
        assert_eq!(members[0]["security"], "auto");
        assert_eq!(members[0]["persona"], "strict-reviewer");
    }

    #[tokio::test]
    async fn assign_upserts_existing_role() {
        let (state, _dir) = test_state();
        let app = test_router(state.clone());
        for agent in ["claude-opus-4-8", "claude-sonnet-4-6"] {
            let app = app.clone();
            app.oneshot(
                Request::post("/api/team/assign")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "role": "implementer",
                            "agent_id": agent,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        }

        let app = test_router(state);
        let resp = app
            .oneshot(Request::get("/api/team").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let members = val["members"].as_array().unwrap();
        assert_eq!(members.len(), 1, "assign should upsert, not duplicate");
        assert_eq!(members[0]["agent_id"], "claude-sonnet-4-6");
    }

    #[tokio::test]
    async fn assign_rejects_missing_role() {
        let (state, _dir) = test_state();
        let app = test_router(state);
        let resp = app
            .oneshot(
                Request::post("/api/team/assign")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "role": "",
                            "agent_id": "claude-sonnet-4-6",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn remove_deletes_assignment() {
        let (state, _dir) = test_state();
        let app = test_router(state.clone());
        app.oneshot(
            Request::post("/api/team/assign")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "role": "qa",
                        "agent_id": "claude-sonnet-4-6",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

        let app = test_router(state.clone());
        let resp = app
            .oneshot(Request::delete("/api/team/qa").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let app = test_router(state);
        let resp = app
            .oneshot(Request::get("/api/team").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(val["members"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn remove_unknown_role_returns_404() {
        let (state, _dir) = test_state();
        let app = test_router(state);
        let resp = app
            .oneshot(
                Request::delete("/api/team/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
