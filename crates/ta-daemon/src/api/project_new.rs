// api/project_new.rs — Conversational project bootstrapping endpoint (v0.10.17).
//
// `POST /api/project/new` starts or continues a bootstrapping session.
// Used by Discord/Slack/email channel interfaces to create new projects remotely.
//
// Flow:
//   1. POST /api/project/new { description: "..." } → creates session, returns session_id + first response
//   2. POST /api/project/new { session_id: "...", prompt: "..." } → continues conversation
//   3. POST /api/project/new { session_id: "...", prompt: "generate" } → finalizes project

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::AppState;

/// Request body for project bootstrapping.
#[derive(Debug, Deserialize)]
pub struct ProjectNewRequest {
    /// Initial project description (for starting a new session).
    pub description: Option<String>,
    /// Session ID (for continuing an existing session).
    pub session_id: Option<String>,
    /// User prompt (for continuing a session).
    pub prompt: Option<String>,
    /// Project name (optional, agent will ask if not provided).
    pub name: Option<String>,
    /// Project template (optional).
    pub template: Option<String>,
    /// Output directory (optional, defaults to project root).
    pub output_dir: Option<String>,
    /// Version schema (optional).
    pub version_schema: Option<String>,
}

/// Response from the bootstrapping endpoint.
#[derive(Debug, Serialize)]
pub struct ProjectNewResponse {
    /// Session identifier for follow-up requests.
    pub session_id: String,
    /// Current session status.
    pub status: SessionStatus,
    /// Agent's response or question for the user.
    pub message: String,
    /// Path to the created project (only set when status is "completed").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

/// Bootstrapping session status.
///
/// All variants are part of the public API — channel interfaces transition
/// sessions through the full lifecycle (Started → Questioning → Generating → Completed).
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SessionStatus {
    /// Session started, awaiting user input.
    Started,
    /// Agent asked a clarifying question.
    Questioning,
    /// Project is being generated.
    Generating,
    /// Project creation completed.
    Completed,
    /// An error occurred.
    Error,
}

/// In-memory session state for bootstrapping conversations.
///
/// Fields are stored for the full session lifecycle — channel interfaces
/// (Discord, Slack, email) read them when delegating to `ta new run`.
#[derive(Debug)]
#[allow(dead_code)]
pub struct BootstrapSession {
    pub name: Option<String>,
    pub description: Option<String>,
    pub template: Option<String>,
    pub output_dir: Option<String>,
    pub version_schema: Option<String>,
    pub conversation: Vec<ConversationTurn>,
    pub status: SessionStatus,
    pub project_path: Option<String>,
    pub created_at: std::time::Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
}

/// Manager for bootstrap sessions.
pub struct BootstrapSessionManager {
    sessions: std::sync::Mutex<HashMap<String, BootstrapSession>>,
}

impl BootstrapSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Create a new session and return its ID.
    pub fn create_session(
        &self,
        name: Option<String>,
        description: Option<String>,
        template: Option<String>,
        output_dir: Option<String>,
        version_schema: Option<String>,
    ) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        let mut sessions = self.sessions.lock().unwrap();

        let initial_turn = description.as_ref().map(|d| ConversationTurn {
            role: "user".to_string(),
            content: d.clone(),
        });

        let mut conversation = Vec::new();
        if let Some(turn) = initial_turn {
            conversation.push(turn);
        }

        sessions.insert(
            session_id.clone(),
            BootstrapSession {
                name,
                description,
                template,
                output_dir,
                version_schema,
                conversation,
                status: SessionStatus::Started,
                project_path: None,
                created_at: std::time::Instant::now(),
            },
        );

        // Clean up old sessions (>1 hour).
        sessions.retain(|_, s| s.created_at.elapsed().as_secs() < 3600);

        session_id
    }

    /// Add a user message to the session conversation.
    pub fn add_user_message(&self, session_id: &str, message: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("Session '{}' not found or expired", session_id))?;

        session.conversation.push(ConversationTurn {
            role: "user".to_string(),
            content: message.to_string(),
        });
        Ok(())
    }

    /// Get the current session status.
    pub fn get_status(&self, session_id: &str) -> Result<SessionStatus, String> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get(session_id)
            .ok_or_else(|| format!("Session '{}' not found or expired", session_id))?;
        // Return a copy of the status.
        Ok(match &session.status {
            SessionStatus::Started => SessionStatus::Started,
            SessionStatus::Questioning => SessionStatus::Questioning,
            SessionStatus::Generating => SessionStatus::Generating,
            SessionStatus::Completed => SessionStatus::Completed,
            SessionStatus::Error => SessionStatus::Error,
        })
    }

    /// Get project details for a completed session.
    pub fn get_project_path(&self, session_id: &str) -> Option<String> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(session_id)
            .and_then(|s| s.project_path.clone())
    }
}

impl Default for BootstrapSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// `POST /api/project/new` — Start or continue a project bootstrapping session.
pub async fn create_project(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProjectNewRequest>,
) -> impl IntoResponse {
    // Case 1: Continue an existing session.
    if let Some(session_id) = &body.session_id {
        let prompt = body.prompt.as_deref().unwrap_or("");

        // Add the user message to the session.
        if let Err(e) = state
            .bootstrap_sessions
            .add_user_message(session_id, prompt)
        {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": e,
                    "hint": "Session may have expired. Start a new session with POST /api/project/new"
                })),
            )
                .into_response();
        }

        // For now, the daemon delegates to `ta new run` via cmd execution.
        // The session tracks state for multi-turn conversations via channels.
        let status = state
            .bootstrap_sessions
            .get_status(session_id)
            .unwrap_or(SessionStatus::Error);

        let response = ProjectNewResponse {
            session_id: session_id.clone(),
            status,
            message: "Message received. Use the daemon's interaction endpoints to manage \
                 the bootstrapping conversation, or run `ta new run` directly from CLI."
                .to_string(),
            project_path: state.bootstrap_sessions.get_project_path(session_id),
        };

        return Json(response).into_response();
    }

    // Case 2: Start a new session.
    let description = body.description.clone().unwrap_or_default();
    if description.is_empty() && body.name.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Either 'description' or 'name' is required to start a bootstrapping session.",
                "usage": {
                    "start": "POST /api/project/new { \"description\": \"Build a CLI for DNS management\" }",
                    "continue": "POST /api/project/new { \"session_id\": \"...\", \"prompt\": \"...\" }",
                }
            })),
        )
            .into_response();
    }

    let session_id = state.bootstrap_sessions.create_session(
        body.name.clone(),
        Some(description.clone()),
        body.template.clone(),
        body.output_dir.clone(),
        body.version_schema.clone(),
    );

    tracing::info!(
        session_id = %session_id,
        name = ?body.name,
        template = ?body.template,
        "Started project bootstrapping session"
    );

    let message = if body.name.is_some() {
        format!(
            "Bootstrapping session started. \
             Use `ta new run --name {}` from CLI for the interactive experience, \
             or continue via this API with session_id.",
            body.name.as_deref().unwrap_or("project")
        )
    } else {
        "Bootstrapping session started. \
         Use `ta new run` from CLI for the full interactive experience, \
         or continue via this API with the returned session_id."
            .to_string()
    };

    let response = ProjectNewResponse {
        session_id,
        status: SessionStatus::Started,
        message,
        project_path: None,
    };

    (StatusCode::CREATED, Json(response)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_manager_create_and_message() {
        let mgr = BootstrapSessionManager::new();
        let id = mgr.create_session(
            Some("test-project".into()),
            Some("Build a CLI".into()),
            None,
            None,
            None,
        );
        assert!(!id.is_empty());

        // Add a message.
        mgr.add_user_message(&id, "Use Rust").unwrap();

        // Check status.
        let status = mgr.get_status(&id).unwrap();
        assert_eq!(status, SessionStatus::Started);
    }

    #[test]
    fn session_manager_unknown_session() {
        let mgr = BootstrapSessionManager::new();
        assert!(mgr.add_user_message("nonexistent", "hello").is_err());
        assert!(mgr.get_status("nonexistent").is_err());
    }

    #[test]
    fn session_manager_no_project_path_initially() {
        let mgr = BootstrapSessionManager::new();
        let id = mgr.create_session(None, Some("test".into()), None, None, None);
        assert!(mgr.get_project_path(&id).is_none());
    }
}
