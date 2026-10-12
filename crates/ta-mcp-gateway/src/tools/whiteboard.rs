//! `ta_whiteboard_*` MCP tools — the live capability an agent calls during a
//! team-session goal run to coordinate with its peers via the
//! daemon-hosted whiteboard (Task 6's `crate::daemon_client::WhiteboardDaemonClient`).
//!
//! ## Token source (security-load-bearing: do not change without re-reading
//! this comment)
//!
//! Every tool here authenticates using the session read from the
//! environment of this gateway process: `TA_WHITEBOARD_TOKEN`,
//! `TA_WHITEBOARD_TEAM_SESSION` and `TA_WHITEBOARD_SOURCE_DIR`. The daemon
//! mints a per-role token at each launch and hands it to `ta run` in its
//! environment; the agent's MCP config (`.ta/mcp-agent.json`) carries only
//! the `${TA_WHITEBOARD_TOKEN:-}` placeholder, which the agent expands from
//! its own environment. No token is ever written to a file under the
//! project's `.ta/` (red-team CR-03), and the token is never accepted as an
//! MCP tool-call parameter (an LLM could simply supply a different
//! team-session's token). A goal not launched as a whiteboard-enabled team
//! role has no such variables, and every tool below fails with a clear,
//! structured `McpError::invalid_request`: never a silent no-op and never a
//! caller-supplied substitute.
//!
//! ## Sync-to-async bridging
//!
//! Tool handlers are plain sync `fn`s that may already be running inside
//! the gateway's own async runtime, so calling `WhiteboardDaemonClient`'s
//! `async fn`s must not `block_on` directly on the calling thread (risk of
//! nesting runtimes — see `whiteboard_check.rs`'s module doc). Each handler
//! calls `super::sync_bridge::run_on_dedicated_thread`, which spawns a
//! dedicated OS thread, builds a
//! `tokio::runtime::Builder::new_current_thread()` runtime on it, and joins
//! the result back, the same pattern `whiteboard_check.rs` already uses,
//! and now shared with `tools/wiki.rs` for the identical need.

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

const ENV_TOKEN: &str = "TA_WHITEBOARD_TOKEN";
const ENV_TEAM_SESSION: &str = "TA_WHITEBOARD_TEAM_SESSION";
const ENV_SOURCE_DIR: &str = "TA_WHITEBOARD_SOURCE_DIR";

#[cfg(test)]
type TestSessions = Mutex<std::collections::HashMap<PathBuf, (String, String, String)>>;

#[cfg(test)]
fn test_sessions() -> &'static TestSessions {
    static S: std::sync::OnceLock<TestSessions> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Test-only: register the session a given workspace root should see, so
/// tests never mutate the process environment (which is shared by parallel tests).
#[cfg(test)]
fn set_test_session(workspace_root: &Path, team_session: &str, token: &str, source_dir: &str) {
    test_sessions().lock().unwrap().insert(
        workspace_root.to_path_buf(),
        (team_session.into(), token.into(), source_dir.into()),
    );
}

/// Pure: build the session from an environment lookup.
fn session_from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<WhiteboardSession, McpError> {
    let get = |k: &str| {
        env(k)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let (Some(token), Some(team_session), Some(source_dir)) =
        (get(ENV_TOKEN), get(ENV_TEAM_SESSION), get(ENV_SOURCE_DIR))
    else {
        return Err(McpError::invalid_request(
            format!(
                "ta_whiteboard_*: no whiteboard session in this agent's environment ({ENV_TOKEN}, \
                 {ENV_TEAM_SESSION} and {ENV_SOURCE_DIR} must all be set). This goal was not \
                 launched as a role of a team session with whiteboard coordination enabled. \
                 ta_whiteboard_* tools are only available to roles launched by the daemon for a \
                 team session where the project's `[whiteboard] enabled = true`. If this IS such a \
                 role, check that the agent's MCP config forwards the {ENV_TOKEN} variable."
            ),
            None,
        ));
    };
    Ok(WhiteboardSession {
        team_session,
        token,
        source_dir,
    })
}

/// Reads the whiteboard session for this gateway process; see this module's
/// doc comment for why the environment is the only allowed token source.
fn load_whiteboard_session(workspace_root: &Path) -> Result<WhiteboardSession, McpError> {
    #[cfg(test)]
    {
        if let Some((team_session, token, source_dir)) =
            test_sessions().lock().unwrap().get(workspace_root).cloned()
        {
            return Ok(WhiteboardSession {
                team_session,
                token,
                source_dir,
            });
        }
        session_from_env(&|_| None)
    }
    #[cfg(not(test))]
    {
        let _ = workspace_root;
        session_from_env(&|k| std::env::var(k).ok())
    }
}

fn workspace_root(state: &Arc<Mutex<GatewayState>>) -> Result<PathBuf, McpError> {
    let locked = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    Ok(locked.config.workspace_root.clone())
}

use super::sync_bridge::run_on_dedicated_thread;

fn success_json(value: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::json(value)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

// ── ta_whiteboard_presence_register ────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PresenceRegisterParams {
    /// Your Agent ID -- the `**Agent ID:**` value from your own injected
    /// context, if you were launched by a wake-on-demand listener (see
    /// `WakeListenerConfig`'s doc comment in ta-daemon's wake_listener.rs).
    /// Use that, not your role name: a claim or presence record that needs
    /// to mean something beyond this one goal's lifetime must be
    /// attributable to one specific, stable seat -- your role is shared by
    /// every other listener registered under it (that's the point of
    /// sharing a role: horizontal capacity), so it can never disambiguate
    /// you from a sibling. If you have no Agent ID (an ordinary goal, not
    /// launched by a listener), fall back to your role or another
    /// consistent label -- just never pass the bare role string when a
    /// real Agent ID is available.
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
    /// Your Agent ID -- see `PresenceRegisterParams.agent_id`'s doc comment
    /// for why this should be your stable Agent ID, not your role name,
    /// whenever one is available.
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
    /// `candidate_id` TA states, outside the fenced untrusted block, in the
    /// "## New intake" section of the prompt this role was woken with, if
    /// any). Required so the Wayfinder poller can act on the
    /// right task, never freshly generated.
    pub candidate_id: String,
    /// `"done"`, `"blocked"`, or `"new_work"` for a normal worker. In a
    /// chat-mode (Chief-of-Staff) server only `"reply"`, `"delegate"` and
    /// `"done"` are accepted, see [`CHAT_OUTCOMES`].
    pub outcome: String,
    /// Free-text detail: what happened, why, or what's blocking it. For
    /// `"reply"` (chat mode) it is the text sent back to the requester. For
    /// `"delegate"` it is a JSON object, as text, with exactly two string
    /// fields: `{"tag":"<tag>","objective":"<what the role must do>"}`.
    pub detail: String,
    /// Only meaningful when `outcome == "new_work"`: a title for the new
    /// Wayfinder task the poller should create.
    #[serde(default)]
    pub new_task_title: Option<String>,
}

/// The only outcomes a chat-mode (Chief-of-Staff) server accepts, in the
/// vocabulary the Wayfinder poller consumes: `reply` (detail = text for the
/// requester), `delegate` (detail = `{"tag":..,"objective":..}`, the poller
/// resolves the tag to a worker role) and `done` (nothing more to do).
pub const CHAT_OUTCOMES: [&str; 3] = ["reply", "delegate", "done"];

/// Description of `ta_whiteboard_outcome_send` advertised by a chat-mode
/// server, replacing the normal done/blocked/new_work wording.
pub const CHAT_OUTCOME_TOOL_DESCRIPTION: &str = "Send the Chief-of-Staff outcome for the intake you were woken with. \
outcome must be exactly one of: \"reply\" (answer the requester directly; detail is the plain text for them), \
\"delegate\" (hand the work to a worker role through the poller; detail is a JSON object as text with exactly two \
string fields {\"tag\":\"<tag from your vocabulary>\",\"objective\":\"<what the role must do>\"}), or \
\"done\" (nothing more to do; detail is a one line summary). Any other value, including new_work and blocked, is \
rejected. candidate_id must be the one stated before the fenced intake, nonce included.";

/// Error for an outcome outside the chat-mode vocabulary, or `None` when it
/// is allowed. Exact, case-sensitive match: that is what the poller matches.
fn chat_outcome_error(outcome: &str) -> Option<String> {
    if CHAT_OUTCOMES.contains(&outcome) {
        return None;
    }
    Some(format!(
        "ta_whiteboard_outcome_send: outcome \"{outcome}\" is not allowed in chat mode. Allowed values: \
         \"reply\" (answer the requester directly; detail is the text for them), \"delegate\" (hand \
         the work to a worker role via the poller; detail is a JSON object as text with exactly \
         the string fields tag and objective), \"done\" (nothing to do). Nothing was sent. Call \
         the tool again with one of those three values."
    ))
}

/// Rewrites the chat-mode server's `ta_whiteboard_outcome_send` tool
/// definition so the advertised description and `outcome` schema list
/// exactly [`CHAT_OUTCOMES`]. Called when the chat-mode router is built.
pub fn advertise_chat_outcome_vocabulary(tool: &mut rmcp::model::Tool) {
    tool.description = Some(CHAT_OUTCOME_TOOL_DESCRIPTION.into());
    let mut schema = (*tool.input_schema).clone();
    if let Some(outcome) = schema
        .get_mut("properties")
        .and_then(|p| p.get_mut("outcome"))
        .and_then(|o| o.as_object_mut())
    {
        outcome.insert("type".into(), serde_json::json!("string"));
        outcome.insert("enum".into(), serde_json::json!(CHAT_OUTCOMES));
        outcome.insert(
            "description".into(),
            serde_json::json!(
                "reply = answer the requester directly (detail is the text); delegate = hand the \
                 work to a worker role via the poller (detail is {\"tag\":..,\"objective\":..} \
                 as JSON text); done = nothing more to do."
            ),
        );
    }
    tool.input_schema = Arc::new(schema);
}

pub fn handle_outcome_send(
    state: &Arc<Mutex<GatewayState>>,
    params: OutcomeSendParams,
) -> Result<CallToolResult, McpError> {
    // Chat mode (the Chief-of-Staff): enforce the reply/delegate/done
    // vocabulary here, at the tool, before anything is sent.
    let in_chat_mode = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?
        .chat_lock
        .is_some();
    if in_chat_mode {
        if let Some(msg) = chat_outcome_error(&params.outcome) {
            return Err(McpError::invalid_params(msg, None));
        }
    }
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
    fn session_errors_clearly_when_the_environment_has_no_session() {
        let err = session_from_env(&|_| None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("TA_WHITEBOARD_TOKEN"), "{msg}");
        assert!(msg.contains("MCP config"), "{msg}");
        // A partial environment (token but no session) is also refused.
        let only_token = |k: &str| (k == ENV_TOKEN).then(|| "t".to_string());
        assert!(session_from_env(&only_token).is_err());
        // An empty value (an unexpanded `${VAR:-}` placeholder) counts as unset.
        let empty = |_: &str| Some(String::new());
        assert!(session_from_env(&empty).is_err());
    }

    #[test]
    fn session_reads_team_session_token_and_source_dir_from_the_environment() {
        let env = |k: &str| match k {
            ENV_TOKEN => Some("tok-abc".to_string()),
            ENV_TEAM_SESSION => Some("sess-1".to_string()),
            ENV_SOURCE_DIR => Some("/real/project/root".to_string()),
            _ => None,
        };
        let session = session_from_env(&env).unwrap();
        assert_eq!(session.team_session, "sess-1");
        assert_eq!(session.token, "tok-abc");
        assert_eq!(session.source_dir, "/real/project/root");
    }

    #[test]
    fn a_leftover_session_file_in_the_project_is_never_a_token_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/whiteboard-session.json"),
            r#"{"team_session":"s","token":"tok","source_dir":"/x"}"#,
        )
        .unwrap();
        assert!(load_whiteboard_session(dir.path()).is_err());
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
        set_test_session(workspace_root.path(), "sess-1", "tok-abc", "placeholder");
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
            crate::daemon_client::resolve_daemon_url(workspace_root.path()).unwrap();
        let resolved_against_source_dir =
            crate::daemon_client::resolve_daemon_url(std::path::Path::new(source_dir.path()))
                .unwrap();
        let staging_default = crate::daemon_port::default_port_for(workspace_root.path()).unwrap();
        assert_eq!(
            resolved_against_workspace_root,
            format!("http://127.0.0.1:{staging_default}"),
            "staging has no daemon.pid, so resolving against it silently falls back to a \
             default port -- this is exactly the bug, kept here to document what NOT to \
             resolve against"
        );
        assert_ne!(resolved_against_workspace_root, resolved_against_source_dir);
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

    /// End-to-end through the real `ta_whiteboard_outcome_send` handler, the
    /// way a chat-mode chief-of-staff calls it: the handler's workspace_root
    /// is the STAGING dir, whose session (from the environment) names the real
    /// project root as `source_dir`, whose `.ta/daemon.pid` was written by
    /// the daemon itself in its `pid=` + `bind=` shape on a non-default
    /// port. The POST must land on that daemon (and therefore not on 7700).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn outcome_send_handler_reaches_the_projects_real_daemon_from_staging() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".ta")).unwrap();
        let daemon = crate::daemon_client::test_support::spawn_mock_daemon(project.path()).await;
        assert_ne!(daemon.port, 7700);
        std::fs::write(
            project.path().join(".ta/daemon.pid"),
            format!("pid=4242\nbind=127.0.0.1:{}\n", daemon.port),
        )
        .unwrap();

        let staging = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(staging.path().join(".ta")).unwrap();
        set_test_session(
            staging.path(),
            "sess-cos",
            "tok-cos",
            &project.path().display().to_string(),
        );

        let mut config = crate::config::GatewayConfig::for_project(staging.path());
        config.credential_vault_use_keychain = false;
        let state = Arc::new(Mutex::new(GatewayState::new(config).unwrap()));

        let result = tokio::task::spawn_blocking(move || {
            handle_outcome_send(
                &state,
                OutcomeSendParams {
                    candidate_id: "wayfinder-task:t1".to_string(),
                    outcome: "done".to_string(),
                    detail: "triaged".to_string(),
                    new_task_title: None,
                },
            )
        })
        .await
        .unwrap();
        assert!(result.is_ok(), "{result:?}");

        let got = daemon.outcomes.lock().unwrap().clone();
        assert_eq!(
            got.len(),
            1,
            "the outcome POST must arrive at the pid-file daemon"
        );
        assert_eq!(got[0]["token"], "tok-cos");
        assert_eq!(got[0]["team_session"], "sess-cos");
        let payload: serde_json::Value =
            serde_json::from_str(got[0]["payload"].as_str().unwrap()).unwrap();
        assert_eq!(payload["candidate_id"], "wayfinder-task:t1");
    }
}
