// api/agent.rs — Agent session management API.
//
// Manages headless agent subprocesses that persist across requests.
// The daemon owns the agent's lifecycle.
//
// Endpoints:
//   POST   /api/agent/start     — Start a new agent session
//   POST   /api/agent/ask       — Send a prompt to an agent session
//   GET    /api/agent/sessions  — List active sessions
//   DELETE /api/agent/:id       — Stop an agent session

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::api::auth::{require_write, CallerIdentity};
use crate::api::AppState;

/// An active agent session managed by the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSession {
    pub session_id: String,
    pub agent: String,
    pub status: SessionStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_active: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Running,
    Idle,
    Stopped,
}

/// Manages active agent sessions.
pub struct AgentSessionManager {
    sessions: Mutex<HashMap<String, AgentSession>>,
    max_sessions: usize,
}

impl AgentSessionManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            max_sessions,
        }
    }

    pub async fn create_session(&self, agent: String) -> Result<AgentSession, String> {
        let mut sessions = self.sessions.lock().await;

        // Check session limit.
        let active_count = sessions
            .values()
            .filter(|s| s.status != SessionStatus::Stopped)
            .count();
        if active_count >= self.max_sessions {
            return Err(format!("Maximum sessions ({}) reached", self.max_sessions));
        }

        let session_id = format!("sess-{}", &Uuid::new_v4().to_string()[..8]);
        let now = chrono::Utc::now();
        let session = AgentSession {
            session_id: session_id.clone(),
            agent,
            status: SessionStatus::Running,
            created_at: now,
            last_active: now,
        };

        sessions.insert(session_id, session.clone());
        Ok(session)
    }

    pub async fn get_session(&self, session_id: &str) -> Option<AgentSession> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    pub async fn list_sessions(&self) -> Vec<AgentSession> {
        self.sessions.lock().await.values().cloned().collect()
    }

    pub async fn stop_session(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.status = SessionStatus::Stopped;
            true
        } else {
            false
        }
    }

    pub async fn touch_session(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.last_active = chrono::Utc::now();
        }
    }
}

// ── Request/response types ──────────────────────────────────────

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct StartSessionRequest {
    pub agent: Option<String>,
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StartSessionResponse {
    pub session_id: String,
    pub status: String,
    pub agent: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AskRequest {
    pub session_id: String,
    pub prompt: String,
}

#[derive(Debug, Serialize)]
pub struct AskResponse {
    pub session_id: String,
    pub response: String,
}

// ── Handlers ────────────────────────────────────────────────────

/// `POST /api/agent/start` — Start a new agent session.
pub async fn start_session(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<CallerIdentity>,
    Json(body): Json<StartSessionRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_write(&identity) {
        return e.into_response();
    }

    let agent = body
        .agent
        .unwrap_or_else(|| state.daemon_config.agent.default_agent.clone());

    match state.agent_sessions.create_session(agent.clone()).await {
        Ok(session) => (
            StatusCode::CREATED,
            Json(StartSessionResponse {
                session_id: session.session_id,
                status: "running".to_string(),
                agent,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

/// `POST /api/agent/ask` — Send a prompt to an agent session.
///
/// In this initial implementation, the daemon acknowledges the prompt.
/// Full agent subprocess integration (launching headless claude-code, streaming
/// responses via SSE) is deferred to a follow-up when `ta shell` provides the
/// client-side rendering.
pub async fn ask_agent(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<CallerIdentity>,
    Json(body): Json<AskRequest>,
) -> impl IntoResponse {
    if let Err(e) = require_write(&identity) {
        return e.into_response();
    }

    let session = state.agent_sessions.get_session(&body.session_id).await;
    match session {
        Some(s) if s.status == SessionStatus::Running => {
            state.agent_sessions.touch_session(&body.session_id).await;
            // Placeholder: echo the prompt. Full agent subprocess wiring is
            // implemented when `ta shell` (v0.9.8) provides the client side.
            Json(AskResponse {
                session_id: body.session_id,
                response: format!(
                    "[Agent session {} ({})]: Received prompt. \
                     Full agent subprocess integration is available via `ta shell`.",
                    s.session_id, s.agent
                ),
            })
            .into_response()
        }
        Some(_) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "session is not running"})),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )
            .into_response(),
    }
}

/// `GET /api/agent/sessions` — List active agent sessions.
pub async fn list_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.agent_sessions.list_sessions().await;
    Json(sessions)
}

/// `DELETE /api/agent/:id` — Stop an agent session.
pub async fn stop_session(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<CallerIdentity>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_write(&identity) {
        return e.into_response();
    }

    if state.agent_sessions.stop_session(&session_id).await {
        Json(serde_json::json!({"session_id": session_id, "status": "stopped"})).into_response()
    } else {
        (StatusCode::NOT_FOUND, "session not found").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_lifecycle() {
        let mgr = AgentSessionManager::new(3);

        // Create a session.
        let s = mgr.create_session("claude-code".into()).await.unwrap();
        assert!(s.session_id.starts_with("sess-"));
        assert_eq!(s.status, SessionStatus::Running);

        // List sessions.
        let sessions = mgr.list_sessions().await;
        assert_eq!(sessions.len(), 1);

        // Get session.
        let found = mgr.get_session(&s.session_id).await;
        assert!(found.is_some());

        // Touch session.
        mgr.touch_session(&s.session_id).await;

        // Stop session.
        assert!(mgr.stop_session(&s.session_id).await);
        let stopped = mgr.get_session(&s.session_id).await.unwrap();
        assert_eq!(stopped.status, SessionStatus::Stopped);

        // Stop unknown session.
        assert!(!mgr.stop_session("nonexistent").await);
    }

    #[tokio::test]
    async fn session_limit_enforced() {
        let mgr = AgentSessionManager::new(2);

        mgr.create_session("a1".into()).await.unwrap();
        mgr.create_session("a2".into()).await.unwrap();

        // Third should fail.
        let result = mgr.create_session("a3".into()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stopped_sessions_dont_count_toward_limit() {
        let mgr = AgentSessionManager::new(2);

        let s1 = mgr.create_session("a1".into()).await.unwrap();
        mgr.create_session("a2".into()).await.unwrap();

        // Stop one.
        mgr.stop_session(&s1.session_id).await;

        // Now we should be able to create another.
        let result = mgr.create_session("a3".into()).await;
        assert!(result.is_ok());
    }
}
