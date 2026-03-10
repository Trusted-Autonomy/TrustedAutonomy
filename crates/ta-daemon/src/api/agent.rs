// api/agent.rs — Agent session management with real subprocess execution.
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
    /// Number of prompts sent in this session.
    pub prompt_count: usize,
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
            prompt_count: 0,
        };

        sessions.insert(session_id, session.clone());
        Ok(session)
    }

    pub async fn get_session(&self, session_id: &str) -> Option<AgentSession> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    /// Get or create a default session for implicit agent access.
    pub async fn get_or_create_default(&self, default_agent: &str) -> Result<AgentSession, String> {
        let sessions = self.sessions.lock().await;

        // Find first running session.
        if let Some(session) = sessions
            .values()
            .find(|s| s.status == SessionStatus::Running)
        {
            return Ok(session.clone());
        }
        drop(sessions);

        // No running session — create one.
        self.create_session(default_agent.to_string()).await
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
            session.prompt_count += 1;
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
pub struct AskRequest {
    pub session_id: String,
    pub prompt: String,
}

#[derive(Debug, Serialize)]
pub struct AskResponse {
    pub session_id: String,
    pub response: String,
    /// Request ID for streaming output (present when status is "processing").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// "processing" if the agent is running async, absent if response is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
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
/// Spawns `claude --print -p "<prompt>"` in the project root and streams
/// output via the GoalOutput channel system. Returns an immediate ack so
/// the client knows the agent received the request. The client subscribes
/// to `GET /api/goals/:request_id/output` for the streaming response.
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

            // Create an output channel for this request so the client can stream results.
            let request_id = format!("ask-{}", &Uuid::new_v4().to_string()[..8]);
            let tx = state.goal_output.create_channel(&request_id).await;

            let agent = s.agent.clone();
            let prompt = body.prompt.clone();
            let working_dir = state.project_root.clone();
            let timeout_secs = state.daemon_config.agent.timeout_secs;
            let goal_output = state.goal_output.clone_ref();
            let req_id = request_id.clone();
            let session_id = body.session_id.clone();

            // Spawn the agent subprocess — returns immediately.
            tokio::spawn(async move {
                use crate::api::goal_output::OutputLine;
                use tokio::io::{AsyncBufReadExt, BufReader};

                let (binary, args) = resolve_agent_command(&agent, &prompt);

                tracing::info!(
                    "Agent ask (streaming): agent={}, request_id={}, prompt_len={}",
                    agent,
                    req_id,
                    prompt.len()
                );

                let timeout = std::time::Duration::from_secs(timeout_secs.max(60));

                let result = tokio::process::Command::new(&binary)
                    .args(&args)
                    .current_dir(&working_dir)
                    .env_remove("CLAUDECODE")
                    .env_remove("CLAUDE_CODE_ENTRYPOINT")
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn();

                match result {
                    Ok(mut child) => {
                        let stdout = child.stdout.take();
                        let stderr = child.stderr.take();
                        let tx2 = tx.clone();

                        let stdout_task = tokio::spawn(async move {
                            if let Some(out) = stdout {
                                let mut reader = BufReader::new(out).lines();
                                while let Ok(Some(line)) = reader.next_line().await {
                                    let _ = tx.send(OutputLine {
                                        stream: "stdout",
                                        line,
                                    });
                                }
                            }
                        });

                        let stderr_task = tokio::spawn(async move {
                            if let Some(err) = stderr {
                                let mut reader = BufReader::new(err).lines();
                                while let Ok(Some(line)) = reader.next_line().await {
                                    let _ = tx2.send(OutputLine {
                                        stream: "stderr",
                                        line,
                                    });
                                }
                            }
                        });

                        let status = tokio::time::timeout(timeout, child.wait()).await;

                        let _ = stdout_task.await;
                        let _ = stderr_task.await;

                        match status {
                            Ok(Ok(s)) if !s.success() => {
                                tracing::warn!(
                                    "Agent ask failed (exit {}): session={}, request={}",
                                    s.code().unwrap_or(-1),
                                    session_id,
                                    req_id,
                                );
                            }
                            Ok(Err(e)) => {
                                tracing::error!("Agent wait error: {}", e);
                            }
                            Err(_) => {
                                let _ = child.kill().await;
                                tracing::warn!(
                                    "Agent timed out after {}s: request={}",
                                    timeout.as_secs(),
                                    req_id,
                                );
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to start agent '{}' (binary: {}): {}",
                            agent,
                            binary,
                            e
                        );
                    }
                }

                goal_output.remove_channel(&req_id).await;
            });

            // Return immediate ack with the request ID for streaming.
            Json(AskResponse {
                session_id: body.session_id,
                response: String::new(),
                request_id: Some(request_id),
                status: Some("processing".to_string()),
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

/// Resolve agent name to binary + args.
fn resolve_agent_command(agent: &str, prompt: &str) -> (String, Vec<String>) {
    match agent {
        "claude-code" | "claude" => (
            "claude".to_string(),
            vec!["--print".to_string(), "-p".to_string(), prompt.to_string()],
        ),
        "codex" => (
            "codex".to_string(),
            vec![
                "--quiet".to_string(),
                "--prompt".to_string(),
                prompt.to_string(),
            ],
        ),
        // Generic fallback: assume binary name matches agent name.
        other => (other.to_string(), vec![prompt.to_string()]),
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
        assert_eq!(s.prompt_count, 0);

        // List sessions.
        let sessions = mgr.list_sessions().await;
        assert_eq!(sessions.len(), 1);

        // Get session.
        let found = mgr.get_session(&s.session_id).await;
        assert!(found.is_some());

        // Touch session (increments prompt count).
        mgr.touch_session(&s.session_id).await;
        let updated = mgr.get_session(&s.session_id).await.unwrap();
        assert_eq!(updated.prompt_count, 1);

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

    #[tokio::test]
    async fn get_or_create_default_reuses_existing() {
        let mgr = AgentSessionManager::new(3);
        let s1 = mgr.create_session("claude-code".into()).await.unwrap();
        let s2 = mgr.get_or_create_default("claude-code").await.unwrap();
        assert_eq!(s1.session_id, s2.session_id);
    }

    #[tokio::test]
    async fn get_or_create_default_creates_new() {
        let mgr = AgentSessionManager::new(3);
        let s = mgr.get_or_create_default("claude-code").await.unwrap();
        assert!(s.session_id.starts_with("sess-"));
    }

    #[test]
    fn resolve_claude_code_agent() {
        let (bin, args) = resolve_agent_command("claude-code", "hello");
        assert_eq!(bin, "claude");
        assert_eq!(args, vec!["--print", "-p", "hello"]);
    }

    #[test]
    fn resolve_unknown_agent() {
        let (bin, args) = resolve_agent_command("my-agent", "test");
        assert_eq!(bin, "my-agent");
        assert_eq!(args, vec!["test"]);
    }
}
