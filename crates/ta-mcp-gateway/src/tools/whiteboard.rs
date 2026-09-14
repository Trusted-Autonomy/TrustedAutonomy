//! `ta_whiteboard_*` MCP tools — the live capability an agent calls during a
//! team-session goal run to coordinate with its peers via the
//! daemon-hosted whiteboard (Task 6's `crate::daemon_client::WhiteboardDaemonClient`).
//!
//! ## Token source (security-load-bearing — do not change without re-reading
//! this comment)
//!
//! Every tool here authenticates using the `team_session`/`token` pair read
//! from `workspace_root.join(".ta").join("whiteboard-session.json")` — a
//! file written by `ta run --team-session-id <id>`
//! (`apps/ta-cli/src/commands/run.rs`'s `write_whiteboard_session_file`)
//! into this specific goal's staging workspace, using the real project
//! root's `.ta/team-sessions/<id>/state.json`. That file is the *only*
//! source: the token is never accepted as an MCP tool-call parameter (an
//! LLM could simply supply a different team-session's token) and never
//! read from an environment variable (unreliable/unverified MCP-client env
//! inheritance — see the Task 7 investigation report). A goal not launched
//! via `--team-session-id` (or one where the project has no
//! `[whiteboard] enabled = true`) has no such file, and every tool below
//! fails with a clear, structured `McpError::invalid_request` — never a
//! silent no-op and never a caller-supplied substitute.
//!
//! ## Sync-to-async bridging
//!
//! Tool handlers are plain sync `fn`s that may already be running inside
//! the gateway's own async runtime, so calling `WhiteboardDaemonClient`'s
//! `async fn`s must not `block_on` directly on the calling thread (risk of
//! nesting runtimes — see `whiteboard_check.rs`'s module doc). Each handler
//! spawns a dedicated OS thread, builds a
//! `tokio::runtime::Builder::new_current_thread()` runtime on it, and joins
//! the result back — the same pattern `whiteboard_check.rs` already uses.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use ta_agent_whiteboard::presence::PresenceRecord;
use ta_session::RoleRef;

use crate::daemon_client::WhiteboardDaemonClient;
use crate::server::GatewayState;

// ── Token source ────────────────────────────────────────────────────────

#[derive(Debug)]
struct WhiteboardSession {
    team_session: String,
    token: String,
    /// The real project root (not this goal's staging path) — see
    /// `apps/ta-cli/src/commands/run.rs`'s `write_whiteboard_session_file`,
    /// which populates this from `config.workspace_root` so presence
    /// records group agents on the same real project together, matching
    /// `whiteboard_check.rs`'s pre-launch conflict check.
    source_dir: String,
}

#[derive(Deserialize)]
struct RawWhiteboardSession {
    team_session: String,
    token: String,
    source_dir: String,
}

/// Reads `workspace_root/.ta/whiteboard-session.json` — see this module's
/// doc comment for why this is the only allowed token source.
fn load_whiteboard_session(workspace_root: &Path) -> Result<WhiteboardSession, McpError> {
    let path = workspace_root.join(".ta").join("whiteboard-session.json");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        McpError::invalid_request(
            format!(
                "ta_whiteboard_*: no whiteboard session found at {} ({e}). This goal was not \
                 launched as part of a team session with whiteboard coordination enabled — \
                 ta_whiteboard_* tools are only available for goals launched via \
                 `ta run --team-session-id <id>` where the project's `[whiteboard] enabled = \
                 true`.",
                path.display()
            ),
            None,
        )
    })?;
    let parsed: RawWhiteboardSession = serde_json::from_str(&raw).map_err(|e| {
        McpError::internal_error(
            format!(
                "ta_whiteboard_*: malformed {} ({e}). This file is written by `ta run \
                 --team-session-id`; if it's corrupted, re-launch the goal.",
                path.display()
            ),
            None,
        )
    })?;
    Ok(WhiteboardSession {
        team_session: parsed.team_session,
        token: parsed.token,
        source_dir: parsed.source_dir,
    })
}

fn workspace_root(state: &Arc<Mutex<GatewayState>>) -> Result<PathBuf, McpError> {
    let locked = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    Ok(locked.config.workspace_root.clone())
}

/// Runs `f` to completion on a dedicated OS thread with its own
/// single-threaded Tokio runtime, joining the result back onto the calling
/// thread. See this module's doc comment for why a direct `block_on` on the
/// calling thread is not safe here.
fn run_on_dedicated_thread<F, Fut, T>(f: F) -> Result<T, McpError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send + 'static,
{
    let result = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("failed to start whiteboard worker runtime: {e}"))?;
        rt.block_on(f())
    })
    .join()
    .map_err(|_| anyhow::anyhow!("whiteboard worker thread panicked"))
    .and_then(|inner| inner);

    result.map_err(|e| McpError::internal_error(e.to_string(), None))
}

fn success_json(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::json(value)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

// ── ta_whiteboard_presence_register ────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PresenceRegisterParams {
    /// Identifier for this agent (free text, e.g. the role or agent id).
    pub agent_id: String,
    /// The goal run this presence advertisement is for.
    pub goal_run_id: String,
    /// Current phase/stage label, if any (e.g. a PLAN.md phase ID or team-session stage name).
    #[serde(default)]
    pub phase: Option<String>,
    /// Resources being touched (file globs / api_impact-style strings).
    #[serde(default)]
    pub resources: Vec<String>,
}

pub fn handle_presence_register(
    state: &Arc<Mutex<GatewayState>>,
    params: PresenceRegisterParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = workspace_root(state)?;
    let session = load_whiteboard_session(&workspace_root)?;

    let mut record = PresenceRecord::new(
        params.agent_id.clone(),
        params.goal_run_id.clone(),
        session.source_dir.clone(),
    );
    if let Some(phase) = params.phase.clone() {
        record = record.with_phase(phase);
    }
    if !params.resources.is_empty() {
        record = record.with_resources(params.resources.clone());
    }

    run_on_dedicated_thread(move || async move {
        // The real project's daemon, not staging's -- staging's `.ta/` is
        // always freshly created (never contains daemon.pid), so resolving
        // against `workspace_root` here would silently fall back to the
        // default port 7700 for any project running its daemon elsewhere.
        let client = WhiteboardDaemonClient::new(Path::new(&session.source_dir));
        client
            .register_presence(&session.token, &session.team_session, &record)
            .await
    })?;

    success_json(serde_json::json!({ "registered": true }))
}

// ── ta_whiteboard_presence_list ─────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct PresenceListParams {}

pub fn handle_presence_list(
    state: &Arc<Mutex<GatewayState>>,
    _params: PresenceListParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = workspace_root(state)?;
    let session = load_whiteboard_session(&workspace_root)?;

    let records = run_on_dedicated_thread(move || async move {
        let client = WhiteboardDaemonClient::new(Path::new(&session.source_dir));
        client
            .list_presence(&session.token, &session.team_session)
            .await
    })?;

    success_json(serde_json::json!({ "agents": records }))
}

// ── ta_whiteboard_handoff_send ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HandoffSendParams {
    /// Identifier for the sending agent.
    pub sender: String,
    /// Who should receive this handoff — by role or by specific agent id.
    pub recipient: RoleRef,
    /// The handoff payload (free text).
    pub payload: String,
}

pub fn handle_handoff_send(
    state: &Arc<Mutex<GatewayState>>,
    params: HandoffSendParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = workspace_root(state)?;
    let session = load_whiteboard_session(&workspace_root)?;

    run_on_dedicated_thread(move || async move {
        let client = WhiteboardDaemonClient::new(Path::new(&session.source_dir));
        client
            .send_handoff(
                &session.token,
                &session.team_session,
                &params.sender,
                &params.recipient,
                &params.payload,
            )
            .await
    })?;

    success_json(serde_json::json!({ "sent": true }))
}

// ── ta_whiteboard_handoff_receive ───────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct HandoffReceiveParams {
    /// Who is checking for a handoff — by role or by specific agent id.
    pub recipient: RoleRef,
}

pub fn handle_handoff_receive(
    state: &Arc<Mutex<GatewayState>>,
    params: HandoffReceiveParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = workspace_root(state)?;
    let session = load_whiteboard_session(&workspace_root)?;

    let handoff = run_on_dedicated_thread(move || async move {
        let client = WhiteboardDaemonClient::new(Path::new(&session.source_dir));
        client
            .receive_handoff(&session.token, &session.team_session, &params.recipient)
            .await
    })?;

    success_json(serde_json::json!({ "handoff": handoff }))
}

// ── ta_whiteboard_task_claim ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TaskClaimParams {
    /// The task identifier being claimed.
    pub task_id: String,
    /// Identifier for the claiming agent.
    pub agent_id: String,
}

pub fn handle_task_claim(
    state: &Arc<Mutex<GatewayState>>,
    params: TaskClaimParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = workspace_root(state)?;
    let session = load_whiteboard_session(&workspace_root)?;

    let claimed = run_on_dedicated_thread(move || async move {
        let client = WhiteboardDaemonClient::new(Path::new(&session.source_dir));
        client
            .claim_task(
                &session.token,
                &session.team_session,
                &params.task_id,
                &params.agent_id,
            )
            .await
    })?;

    success_json(serde_json::json!({ "claimed": claimed }))
}

// ── ta_whiteboard_task_complete ──────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TaskCompleteParams {
    /// The task identifier being marked complete.
    pub task_id: String,
}

pub fn handle_task_complete(
    state: &Arc<Mutex<GatewayState>>,
    params: TaskCompleteParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = workspace_root(state)?;
    let session = load_whiteboard_session(&workspace_root)?;

    run_on_dedicated_thread(move || async move {
        let client = WhiteboardDaemonClient::new(Path::new(&session.source_dir));
        client
            .complete_task(&session.token, &session.team_session, &params.task_id)
            .await
    })?;

    success_json(serde_json::json!({ "completed": true }))
}

// ── ta_whiteboard_outcome_send ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OutcomeSendParams {
    /// Correlates back to the candidate this outcome is for (the
    /// `candidate_id` from the "## New intake" context this role was woken
    /// with, if any) — required so the Wayfinder poller can act on the
    /// right task, never freshly generated.
    pub candidate_id: String,
    /// `"done"`, `"blocked"`, or `"new_work"`.
    pub outcome: String,
    /// Free-text detail — what happened, why, or what's blocking it.
    pub detail: String,
    /// Only meaningful when `outcome == "new_work"`: a title for the new
    /// Wayfinder task the poller should create.
    #[serde(default)]
    pub new_task_title: Option<String>,
}

pub fn handle_outcome_send(
    state: &Arc<Mutex<GatewayState>>,
    params: OutcomeSendParams,
) -> Result<CallToolResult, McpError> {
    let workspace_root = workspace_root(state)?;
    let session = load_whiteboard_session(&workspace_root)?;

    let payload = serde_json::json!({
        "candidate_id": params.candidate_id,
        "outcome": params.outcome,
        "detail": params.detail,
        "new_task_title": params.new_task_title,
    })
    .to_string();

    run_on_dedicated_thread(move || async move {
        let client = WhiteboardDaemonClient::new(Path::new(&session.source_dir));
        client
            .send_outcome(&session.token, &session.team_session, &payload)
            .await
    })?;

    success_json(serde_json::json!({ "sent": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_whiteboard_session_errors_clearly_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_whiteboard_session(dir.path()).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no whiteboard session found"), "{msg}");
        assert!(msg.contains("--team-session-id"), "{msg}");
    }

    #[test]
    fn load_whiteboard_session_errors_clearly_when_malformed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(dir.path().join(".ta/whiteboard-session.json"), "not json").unwrap();
        let err = load_whiteboard_session(dir.path()).unwrap_err();
        assert!(format!("{err}").contains("malformed"));
    }

    #[test]
    fn load_whiteboard_session_reads_team_session_and_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/whiteboard-session.json"),
            serde_json::json!({
                "team_session": "sess-1",
                "token": "tok-abc",
                "source_dir": "/real/project/root",
            })
            .to_string(),
        )
        .unwrap();
        let session = load_whiteboard_session(dir.path()).unwrap();
        assert_eq!(session.team_session, "sess-1");
        assert_eq!(session.token, "tok-abc");
        assert_eq!(
            session.source_dir, "/real/project/root",
            "source_dir must come from the session file (the real project root), \
             not be derived from the staging workspace_root"
        );
    }

    /// Regression test for a bug where every handler built
    /// `WhiteboardDaemonClient::new(&workspace_root)` (staging) instead of
    /// `session.source_dir` (the real project root). Staging's `.ta/` is
    /// always freshly created and never contains a copy of the real
    /// project's `daemon.pid` (see `ta-workspace`'s hardcoded `.ta/`
    /// exclusion), so the bug silently resolved to the default port 7700
    /// for any project running its daemon elsewhere — this proves the fix
    /// resolves against `source_dir` and gets a different, correct port.
    #[test]
    fn daemon_client_must_resolve_against_source_dir_not_workspace_root() {
        let workspace_root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace_root.path().join(".ta")).unwrap();
        std::fs::write(
            workspace_root.path().join(".ta/whiteboard-session.json"),
            serde_json::json!({
                "team_session": "sess-1",
                "token": "tok-abc",
                "source_dir": "placeholder",
            })
            .to_string(),
        )
        .unwrap();
        // workspace_root (staging) never has daemon.pid in real usage --
        // deliberately left without one here to match that.

        let source_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source_dir.path().join(".ta")).unwrap();
        std::fs::write(
            source_dir.path().join(".ta/daemon.pid"),
            "pid=999\nport=8899\n",
        )
        .unwrap();

        let session = load_whiteboard_session(workspace_root.path()).unwrap();
        // The fixed handler path: resolve against session.source_dir, not
        // the workspace_root passed to load_whiteboard_session.
        let resolved_against_workspace_root =
            crate::daemon_client::resolve_daemon_url(workspace_root.path());
        let resolved_against_source_dir =
            crate::daemon_client::resolve_daemon_url(std::path::Path::new(source_dir.path()));
        assert_eq!(
            resolved_against_workspace_root, "http://127.0.0.1:7700",
            "staging has no daemon.pid, so resolving against it silently falls back to 7700 \
             -- this is exactly the bug, kept here to document what NOT to resolve against"
        );
        assert_eq!(
            resolved_against_source_dir, "http://127.0.0.1:8899",
            "resolving against the real project root (source_dir) must find its daemon.pid \
             and use the real port, not the default"
        );
        // Sanity: the session really does carry a source_dir distinct from
        // workspace_root, which is what handlers must use.
        assert_ne!(
            session.source_dir,
            workspace_root.path().display().to_string()
        );
    }
}
