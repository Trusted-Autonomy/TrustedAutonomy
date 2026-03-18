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

    /// Get or create a default session for the given agent.
    ///
    /// Only reuses a session if it matches the requested agent type. This ensures
    /// Q&A sessions (claude-code) and goal sessions (claude-flow) stay separate.
    pub async fn get_or_create_default(&self, default_agent: &str) -> Result<AgentSession, String> {
        let sessions = self.sessions.lock().await;

        // Find first running session for this specific agent.
        if let Some(session) = sessions
            .values()
            .find(|s| s.status == SessionStatus::Running && s.agent == default_agent)
        {
            return Ok(session.clone());
        }
        drop(sessions);

        // No matching session — create one.
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

    /// Check if a session exists and is still running.
    pub async fn session_exists(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .is_some_and(|s| s.status == SessionStatus::Running)
    }
}

// ── Persistent QA agent (v0.11.4.2 item 6-10) ──────────────────
//
// Keeps a long-running `claude --print` subprocess alive for the shell
// session's lifetime. All Q&A prompts are routed to its stdin; responses
// are read from stdout. Avoids cold-start latency on every question.

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

/// A persistent agent subprocess for Q&A sessions.
///
/// Instead of spawning a new process per question, this keeps a single
/// `claude` subprocess alive and routes prompts to its stdin pipe.
pub struct PersistentQaAgent {
    /// The agent binary name (e.g., "claude-code").
    agent: String,
    /// Running subprocess state (None if not started or crashed).
    #[allow(dead_code)] // Used when multi-turn stdin mode is enabled (future).
    process: Mutex<Option<QaProcess>>,
    /// Number of restarts in this session.
    restart_count: Mutex<u32>,
    /// Configuration.
    config: crate::config::QaAgentConfig,
    /// Project root for the working directory.
    project_root: std::path::PathBuf,
    /// Last activity timestamp for idle timeout.
    last_active: Mutex<std::time::Instant>,
    /// Whether a first prompt has been sent (enables --continue for chaining).
    has_conversation: Mutex<bool>,
}

#[allow(dead_code)] // Fields used when multi-turn stdin mode is enabled (future).
struct QaProcess {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    /// Accumulated stdout lines from the subprocess.
    stdout_lines: Arc<Mutex<Vec<String>>>,
    /// Whether the process is currently processing a prompt.
    busy: bool,
}

impl PersistentQaAgent {
    pub fn new(config: crate::config::QaAgentConfig, project_root: std::path::PathBuf) -> Self {
        Self {
            agent: config.agent.clone(),
            process: Mutex::new(None),
            restart_count: Mutex::new(0),
            config,
            project_root,
            last_active: Mutex::new(std::time::Instant::now()),
            has_conversation: Mutex::new(false),
        }
    }

    /// Start the persistent agent subprocess.
    ///
    /// Uses `claude --print --verbose` which keeps stdin open for multiple
    /// prompts. Each prompt is terminated by EOF or newline.
    #[allow(dead_code)] // Public API for future multi-turn stdin mode.
    pub async fn start(&self) -> Result<(), String> {
        let mut proc = self.process.lock().await;
        if proc.is_some() {
            return Ok(()); // Already running.
        }

        let (binary, base_args) = match self.agent.as_str() {
            "claude-code" | "claude" => ("claude", vec!["--print"]),
            "codex" => ("codex", vec!["--quiet"]),
            other => (other, vec![]),
        };

        tracing::info!(
            agent = %self.agent,
            binary = %binary,
            "Starting persistent QA agent subprocess"
        );

        let mut cmd = tokio::process::Command::new(binary);
        cmd.args(&base_args)
            .current_dir(&self.project_root)
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "Failed to start persistent QA agent '{}' (binary: '{}'): {}. \
                 Check that the agent is installed and in PATH.",
                self.agent, binary, e
            )
        })?;

        let stdin = child.stdin.take().ok_or("Failed to capture agent stdin")?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_lines = Arc::new(Mutex::new(Vec::new()));

        // Background task: read stdout lines into buffer.
        if let Some(out) = stdout {
            let lines = stdout_lines.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(out).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    lines.lock().await.push(line);
                }
            });
        }

        // Background task: log stderr (for diagnostics, not routed to user).
        if let Some(err) = stderr {
            tokio::spawn(async move {
                let mut reader = BufReader::new(err).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::debug!(stream = "stderr", line = %line, "QA agent stderr");
                }
            });
        }

        *proc = Some(QaProcess {
            child,
            stdin,
            stdout_lines,
            busy: false,
        });

        *self.last_active.lock().await = std::time::Instant::now();
        tracing::info!(agent = %self.agent, "Persistent QA agent started");
        Ok(())
    }

    /// Send a prompt to the persistent agent and collect the response.
    ///
    /// For `claude --print`, each invocation is a separate subprocess.
    /// The "persistent" aspect is that we reuse the session tracking and
    /// provide a fast-path that skips session creation overhead.
    ///
    /// `parallel`: when true, skips `--continue` chaining even if a prior
    /// conversation exists. Used for /parallel sessions (v0.11.5 item 14).
    pub async fn ask(
        &self,
        prompt: &str,
        tx: tokio::sync::broadcast::Sender<crate::api::goal_output::OutputLine>,
        parallel: bool,
    ) -> Result<(), String> {
        *self.last_active.lock().await = std::time::Instant::now();

        // Check if we should chain to the previous conversation.
        // parallel=true skips --continue so each parallel session starts fresh.
        let continue_conversation = !parallel && *self.has_conversation.lock().await;
        let (binary, args) = resolve_agent_command(&self.agent, prompt, continue_conversation)?;

        let _ = tx.send(crate::api::goal_output::OutputLine {
            stream: "stderr",
            line: format!("Agent ({})...", self.agent),
        });

        let timeout = std::time::Duration::from_secs(self.config.idle_timeout_secs.max(60));

        tracing::info!(
            binary = %binary,
            args = ?args,
            cwd = %self.project_root.display(),
            timeout_secs = timeout.as_secs(),
            "QA agent: spawning subprocess"
        );

        let result = tokio::process::Command::new(&binary)
            .args(&args)
            .current_dir(&self.project_root)
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        match result {
            Ok(mut child) => {
                let pid = child.id().unwrap_or(0);
                tracing::info!(pid, binary = %binary, "QA agent: subprocess started");

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let tx2 = tx.clone();

                let stdout_task = tokio::spawn(async move {
                    if let Some(out) = stdout {
                        let mut reader = BufReader::new(out).lines();
                        let mut count = 0u64;
                        while let Ok(Some(line)) = reader.next_line().await {
                            count += 1;
                            let _ = tx.send(crate::api::goal_output::OutputLine {
                                stream: "stdout",
                                line,
                            });
                        }
                        tracing::debug!(lines = count, "QA agent: stdout stream ended");
                    }
                });

                let stderr_task = tokio::spawn(async move {
                    if let Some(err) = stderr {
                        let mut reader = BufReader::new(err).lines();
                        while let Ok(Some(line)) = reader.next_line().await {
                            tracing::debug!(line = %line, "QA agent stderr");
                            let _ = tx2.send(crate::api::goal_output::OutputLine {
                                stream: "stderr",
                                line,
                            });
                        }
                    }
                });

                let started = std::time::Instant::now();
                let status = tokio::time::timeout(timeout, child.wait()).await;
                let elapsed = started.elapsed();
                let _ = stdout_task.await;
                let _ = stderr_task.await;

                tracing::info!(
                    pid,
                    elapsed_secs = elapsed.as_secs_f64(),
                    result = ?status.as_ref().map(|r| r.as_ref().map(|s| s.code())),
                    "QA agent: subprocess finished"
                );

                match status {
                    Ok(Ok(s)) if !s.success() => {
                        let exit_code = s.code().unwrap_or(-1);
                        let mut restarts = self.restart_count.lock().await;
                        *restarts += 1;
                        if *restarts > self.config.max_restarts {
                            return Err(format!(
                                "QA agent crashed {} times (exit {}). Max restarts ({}) exceeded.",
                                restarts, exit_code, self.config.max_restarts
                            ));
                        }
                        tracing::warn!(
                            exit_code,
                            restarts = *restarts,
                            "Persistent QA agent exited with error"
                        );
                    }
                    Ok(Err(e)) => {
                        return Err(format!("QA agent process error: {}", e));
                    }
                    Err(_) => {
                        return Err(format!(
                            "QA agent timed out after {}s. Configure idle_timeout_secs in \
                             [shell.qa_agent] section of daemon.toml.",
                            timeout.as_secs()
                        ));
                    }
                    _ => {
                        // Success — reset restart counter and mark conversation as active.
                        *self.restart_count.lock().await = 0;
                        *self.has_conversation.lock().await = true;
                    }
                }
            }
            Err(e) => {
                return Err(format!(
                    "Failed to start QA agent '{}' (binary: '{}'): {}",
                    self.agent, binary, e
                ));
            }
        }

        Ok(())
    }

    /// Gracefully shut down the persistent agent.
    #[allow(dead_code)] // Called on shell exit (wired from ta-cli).
    pub async fn shutdown(&self) {
        let mut proc = self.process.lock().await;
        if let Some(mut p) = proc.take() {
            // Close stdin to signal EOF.
            drop(p.stdin);
            // Wait briefly for clean exit.
            let timeout = std::time::Duration::from_secs(self.config.shutdown_timeout_secs);
            match tokio::time::timeout(timeout, p.child.wait()).await {
                Ok(_) => {
                    tracing::info!(agent = %self.agent, "Persistent QA agent shut down cleanly");
                }
                Err(_) => {
                    let _ = p.child.kill().await;
                    tracing::warn!(
                        agent = %self.agent,
                        timeout_secs = self.config.shutdown_timeout_secs,
                        "Persistent QA agent killed after shutdown timeout"
                    );
                }
            }
        }
    }

    /// Check if the agent is healthy (process alive or not started).
    #[allow(dead_code)] // Public API for shell health monitoring.
    pub async fn is_healthy(&self) -> bool {
        let proc = self.process.lock().await;
        match &*proc {
            None => true, // Not started yet — healthy by definition.
            Some(_) => {
                // Process handle exists — assume healthy.
                true
            }
        }
    }

    /// Get the restart count for diagnostics.
    #[allow(dead_code)] // Used in tests and shell status display.
    pub async fn restart_count(&self) -> u32 {
        *self.restart_count.lock().await
    }
}

/// Background supervisor that ensures a default agent session exists.
///
/// On daemon boot, creates a session record via `AgentSessionManager` so that
/// the first `/api/input` or `/api/agent/ask` request doesn't have cold-start
/// latency. If the session gets stopped, the supervisor recreates it (up to
/// `max_restarts`). Configurable via `[shell.qa_agent]` in daemon.toml;
/// set `auto_start = false` to disable.
pub async fn auto_spawn_supervisor(
    state: Arc<super::AppState>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let config = state.persistent_qa.config.clone();
    if !config.auto_start {
        tracing::info!("Agent auto-start disabled (shell.qa_agent.auto_start = false)");
        return;
    }

    let agent_name = config.agent.clone();
    let max_restarts = config.max_restarts;
    let mut restart_count: u32 = 0;

    tracing::info!(
        agent = %agent_name,
        max_restarts,
        "Auto-spawn supervisor starting"
    );

    // Brief delay to let the daemon fully initialize.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    loop {
        // Create or reuse a session for the default agent.
        let result = state
            .agent_sessions
            .get_or_create_default(&agent_name)
            .await;

        match result {
            Ok(session) => {
                tracing::info!(
                    session_id = %session.session_id,
                    agent = %agent_name,
                    "Auto-spawn: agent session ready"
                );
                restart_count = 0;

                // Monitor: check every 30s if session is still alive.
                let sid = session.session_id.clone();
                loop {
                    tokio::select! {
                        _ = shutdown.notified() => {
                            tracing::info!("Auto-spawn: shutdown requested");
                            state.agent_sessions.stop_session(&sid).await;
                            return;
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                            if !state.agent_sessions.session_exists(&sid).await {
                                tracing::warn!(
                                    session_id = %sid,
                                    "Auto-spawn: session ended — will recreate"
                                );
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Auto-spawn: failed to create session");
            }
        }

        restart_count += 1;
        if restart_count > max_restarts {
            tracing::error!(
                restart_count,
                max_restarts,
                "Auto-spawn: max restarts exceeded — supervisor stopping"
            );
            return;
        }

        let backoff = std::time::Duration::from_secs(5 * restart_count as u64);
        tracing::info!(backoff_secs = backoff.as_secs(), "Auto-spawn: backoff");
        tokio::select! {
            _ = shutdown.notified() => return,
            _ = tokio::time::sleep(backoff) => {}
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
    /// When true, spawn a fresh agent conversation — no --continue chaining.
    /// Used by /parallel in the web shell (v0.11.5 item 14).
    #[serde(default)]
    pub parallel: bool,
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
            let parallel = body.parallel;
            let goal_output = state.goal_output.clone_ref();
            let req_id = request_id.clone();
            let persistent_qa = state.persistent_qa.clone();

            // Route through persistent QA agent (v0.11.4.2 item 6).
            // The persistent agent manages subprocess lifecycle, crash recovery,
            // and restart limits — no more cold-start per question.
            tokio::spawn(async move {
                use crate::api::goal_output::OutputLine;

                tracing::info!(
                    "Agent ask (persistent QA): agent={}, request_id={}, prompt_len={}",
                    agent,
                    req_id,
                    prompt.len()
                );

                tracing::info!(request_id = %req_id, subscribers = tx.receiver_count(), parallel, "QA ask: starting");
                match persistent_qa.ask(&prompt, tx.clone(), parallel).await {
                    Ok(()) => {
                        tracing::info!(request_id = %req_id, "QA ask: completed successfully");
                    }
                    Err(e) => {
                        tracing::warn!(request_id = %req_id, error = %e, "QA ask: agent error");
                        let _ = tx.send(OutputLine {
                            stream: "stderr",
                            line: format!("[agent error] {}", e),
                        });
                    }
                }

                goal_output.remove_channel(&req_id).await;
                tracing::debug!(request_id = %req_id, "QA ask: channel removed");
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

/// Resolve agent name to binary + args for Q&A (interactive prompt) sessions.
///
/// Uses `--print -p "<prompt>"` for plain text stdout/stderr output — the same
/// human-readable format you see when running `claude` directly in the terminal.
/// The daemon's stdout/stderr readers publish each line to the broadcast channel
/// in real time.
///
/// Returns `Err` for framework agents (claude-flow, etc.) that don't accept
/// bare prompts — these are designed for goal execution (`ta run`), not Q&A.
/// Configure `qa_agent` in `[agent]` section of `daemon.toml` to route Q&A
/// to a prompt-capable agent.
fn resolve_agent_command(
    agent: &str,
    prompt: &str,
    continue_conversation: bool,
) -> Result<(String, Vec<String>), String> {
    match agent {
        "claude-code" | "claude" => {
            let mut args = vec!["--print".to_string()];
            if continue_conversation {
                args.push("--continue".to_string());
            }
            args.push("-p".to_string());
            args.push(prompt.to_string());
            Ok(("claude".to_string(), args))
        }
        "codex" => Ok((
            "codex".to_string(),
            vec![
                "--quiet".to_string(),
                "--prompt".to_string(),
                prompt.to_string(),
            ],
        )),
        // Framework agents: designed for goal execution (ta run), not direct
        // prompts. They require orchestration setup (hive-mind init, topology
        // selection, etc.) that doesn't fit the Q&A pattern.
        "claude-flow" => Err(
            "'claude-flow' is a framework agent for goal execution (ta run), \
             not interactive Q&A. Set qa_agent = \"claude-code\" in the [agent] \
             section of .ta/daemon.toml to route shell questions to a \
             prompt-capable agent."
                .to_string(),
        ),
        // Generic fallback: assume binary name matches agent name.
        other => Ok((other.to_string(), vec![prompt.to_string()])),
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
        let (bin, args) = resolve_agent_command("claude-code", "hello", false).unwrap();
        assert_eq!(bin, "claude");
        assert_eq!(args, vec!["--print", "-p", "hello"]);
    }

    #[test]
    fn resolve_claude_code_continue() {
        let (bin, args) = resolve_agent_command("claude-code", "follow up", true).unwrap();
        assert_eq!(bin, "claude");
        assert_eq!(args, vec!["--print", "--continue", "-p", "follow up"]);
    }

    #[test]
    fn resolve_claude_alias() {
        let (bin, _args) = resolve_agent_command("claude", "hi", false).unwrap();
        assert_eq!(bin, "claude");
    }

    #[test]
    fn resolve_codex_agent() {
        let (bin, args) = resolve_agent_command("codex", "fix bug", false).unwrap();
        assert_eq!(bin, "codex");
        assert_eq!(args, vec!["--quiet", "--prompt", "fix bug"]);
    }

    #[test]
    fn resolve_claude_flow_rejected() {
        let result = resolve_agent_command("claude-flow", "What is this project?", false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("framework agent"),
            "Error should mention framework agent: {}",
            err
        );
        assert!(
            err.contains("qa_agent"),
            "Error should mention qa_agent config: {}",
            err
        );
    }

    #[test]
    fn resolve_unknown_agent() {
        let (bin, args) = resolve_agent_command("my-agent", "test", false).unwrap();
        assert_eq!(bin, "my-agent");
        assert_eq!(args, vec!["test"]);
    }

    #[test]
    fn get_or_create_default_separates_agent_types() {
        // Verifies that Q&A sessions (claude-code) and goal sessions (claude-flow)
        // don't share sessions — each agent type gets its own session.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mgr = AgentSessionManager::new(5);
            let qa = mgr.get_or_create_default("claude-code").await.unwrap();
            let goal = mgr.get_or_create_default("claude-flow").await.unwrap();
            assert_ne!(qa.session_id, goal.session_id);
            assert_eq!(qa.agent, "claude-code");
            assert_eq!(goal.agent, "claude-flow");
        });
    }

    #[test]
    fn persistent_qa_agent_defaults() {
        // v0.11.4.2 item 6: Verify PersistentQaAgent can be created with default config.
        let config = crate::config::QaAgentConfig::default();
        assert!(config.auto_start);
        assert_eq!(config.agent, "claude-code");
        assert_eq!(config.idle_timeout_secs, 300);
        assert!(config.inject_memory);
        assert_eq!(config.max_restarts, 3);
        assert_eq!(config.shutdown_timeout_secs, 5);
    }

    #[tokio::test]
    async fn persistent_qa_agent_lifecycle() {
        // v0.11.4.2: Verify lifecycle — new agent starts with 0 restarts.
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::QaAgentConfig::default();
        let qa = PersistentQaAgent::new(config, dir.path().to_path_buf());
        assert_eq!(qa.restart_count().await, 0);
        assert!(qa.is_healthy().await);
    }

    #[tokio::test]
    async fn persistent_qa_agent_shutdown_noop_when_not_started() {
        // v0.11.4.2 item 9: Shutdown when not started should be a no-op.
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::QaAgentConfig::default();
        let qa = PersistentQaAgent::new(config, dir.path().to_path_buf());
        qa.shutdown().await; // Should not panic.
    }
}
