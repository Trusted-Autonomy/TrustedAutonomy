// wake_listener.rs — Daemon-hosted wake-on-demand listener (v0.17.11.10).
//
// A role registered as a wake-on-demand listener is launched via `ta run`
// the moment a message arrives on one of its registered keys, instead of
// waiting for its turn in `team_session.rs`'s fixed round-robin rotation.
// Generic and key-routed (`WakeListenerConfig { role, keys }`) so a second
// listener later is pure config, not a redesign — only chief-of-staff is
// wired up today. Full design: docs/superpowers/specs/
// 2026-09-14-daemon-wake-on-demand-listener-design.md.
//
// Single-flight by construction, with no separate bookkeeping needed: each
// `(session, listener)` pair gets its own dedicated, sequential async task
// (spawned once in `start()`) — a task's own loop body can only be doing
// one thing at a time, so it is structurally impossible for the same
// listener to have two `ta run` invocations in flight simultaneously. This
// also resolves the Wayfinder-dispatch design doc's previously-open §9.5
// concurrency question for chief-of-staff, as a side effect.
//
// Static config, not ephemeral liveness: `wake_on_demand_listeners` lives
// directly in `TeamSessionState`'s `state.json`, alongside `stages` — not
// in a separate KV bucket the way `presence.rs`'s liveness records are,
// since a registration here doesn't need to expire or heartbeat.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ta_agent_whiteboard::WhiteboardTransport;
use ta_session::team::TeamConfig;

use crate::team_session::{build_ta_run_args, RoleFinding, TeamSessionState, TeamSessionStatus};

/// How often an idle listener (nothing currently on its stream) re-polls.
/// Deliberately short and decoupled from `team_session.rs`'s rotation
/// period — this independence is the whole point of this module.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// One role's wake-on-demand registration: launch `role` via `ta run`
/// whenever a message arrives on any of `keys`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WakeListenerConfig {
    pub role: String,
    pub keys: Vec<String>,
    /// Opaque classification tag applied to every goal this listener
    /// launches (via `ta run --workflow-tag <tag>`), e.g.
    /// "brain-maintenance". `None` for a listener whose launches shouldn't
    /// be classified. Fully opaque to TA core -- downstream products define
    /// what tags mean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_tag: Option<String>,
}

impl WakeListenerConfig {
    pub fn new(role: impl Into<String>, keys: Vec<String>, workflow_tag: Option<String>) -> Self {
        Self {
            role: role.into(),
            keys,
            workflow_tag,
        }
    }
}

/// How often the discovery loop (below) re-scans for wake-on-demand
/// listeners registered after this daemon process itself started — either
/// a new session, or a new listener added to an existing one. Matches
/// `team_session.rs`'s `SESSION_DISCOVERY_INTERVAL` (and `watchdog.rs`'s
/// own `interval_secs` convention) for consistency. Fixes the same class of
/// bug found live, 2026-10-02, and already fixed for `team_session::start`'s
/// rotation supervisor: a listener added to a session while the daemon is
/// already running was previously never discovered at all -- not stuck,
/// no task was ever spawned for it, since this module's own one-time
/// enumeration at process-launch was the *only* place listeners were ever
/// discovered.
const LISTENER_DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);

/// Identifies one wake-on-demand listener for dedup purposes: which session
/// it belongs to, plus its role and registered keys. `(role, keys)` is not
/// globally unique on its own -- the same role can be registered under
/// more than one session -- so the session id is part of the key too.
type ListenerIdentity = (String, String, Vec<String>);

fn listener_identity(session_id: &str, listener: &WakeListenerConfig) -> ListenerIdentity {
    (
        session_id.to_string(),
        listener.role.clone(),
        listener.keys.clone(),
    )
}

/// Spawns `run_listener_loop` for every wake-on-demand listener declared on
/// every non-`Stopped` team session, skipping any listener whose identity
/// is already in `known` (and recording newly-spawned ones into it) --
/// shared by `start()`'s initial scan and its ongoing discovery loop so
/// both apply the identical "spawn once per listener identity" rule. Keyed
/// by session id + role + keys (see `ListenerIdentity`), not session id
/// alone, because one session can declare multiple listeners, and
/// listeners can be added to an existing session over time, not just whole
/// new sessions appearing.
#[allow(clippy::too_many_arguments)]
fn spawn_new_listeners(
    project_root: &Path,
    ta_bin: &Path,
    transport: &Arc<dyn WhiteboardTransport>,
    shutdown: &Arc<tokio::sync::Notify>,
    known: &mut std::collections::HashSet<ListenerIdentity>,
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
) {
    for id in TeamSessionState::list_ids(project_root) {
        let Ok(Some(state)) = TeamSessionState::load(project_root, &id) else {
            continue;
        };
        // Mirrors team_session::start's own "don't spawn a loop for an
        // already-Stopped session" filter -- a stopped session doesn't come
        // back without a fresh registration, so there's nothing for a
        // listener loop to wait around for.
        if state.status == TeamSessionStatus::Stopped {
            continue;
        }
        for listener in state.wake_on_demand_listeners.clone() {
            if !known.insert(listener_identity(&id, &listener)) {
                continue;
            }
            let pr = project_root.to_path_buf();
            let bin = ta_bin.to_path_buf();
            let sd = shutdown.clone();
            let t = transport.clone();
            let session_id = id.clone();
            tracing::info!(
                session = %session_id,
                role = %listener.role,
                keys = ?listener.keys,
                "wake_listener: starting listener"
            );
            handles.push(tokio::spawn(async move {
                run_listener_loop(pr, session_id, listener, bin, t, sd).await;
            }));
        }
    }
}

/// Discovers all team sessions' `wake_on_demand_listeners` and spawns one
/// supervised watcher task per `(session, listener)` pair — mirrors
/// `team_session::start`'s "read config, spawn one task per entry" shape.
/// Also mirrors `team_session::start`'s ongoing discovery loop (added
/// alongside it, 2026-10-02): a listener added after this initial scan
/// (new session, or a new listener on an existing one) is picked up within
/// `LISTENER_DISCOVERY_INTERVAL`, not only on the next daemon restart.
///
/// A no-op (returns an empty `Vec`) when `[whiteboard] enabled = false` —
/// there is no transport to watch a stream on, and that's the expected,
/// default state for a project that hasn't opted into coordination.
pub fn start(
    app_state: &Arc<crate::api::AppState>,
    shutdown: Arc<tokio::sync::Notify>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let Some(transport) = app_state.whiteboard_transport.clone() else {
        tracing::debug!(
            "wake_listener: [whiteboard] disabled for this project, no listeners started"
        );
        return Vec::new();
    };

    let ta_bin = PathBuf::from(crate::web::find_ta_binary_web());
    let project_root = app_state.project_root.clone();
    let mut handles = Vec::new();
    let mut known: std::collections::HashSet<ListenerIdentity> = std::collections::HashSet::new();

    spawn_new_listeners(
        &project_root,
        &ta_bin,
        &transport,
        &shutdown,
        &mut known,
        &mut handles,
    );

    {
        let pr = project_root.clone();
        let bin = ta_bin.clone();
        let sd = shutdown.clone();
        let t = transport.clone();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(LISTENER_DISCOVERY_INTERVAL) => {}
                    _ = sd.notified() => return,
                }
                let mut discovered = Vec::new();
                spawn_new_listeners(&pr, &bin, &t, &sd, &mut known, &mut discovered);
            }
        }));
    }

    handles
}

/// Outcome of checking a team session's live status against disk, decoupled
/// from async/sleep/logging so it can be unit-tested directly against real
/// on-disk `TeamSessionState` fixtures.
#[derive(Debug, PartialEq, Eq)]
enum ListenerGate {
    /// `Active` (or any status without special handling): proceed to read
    /// and launch as normal.
    Proceed,
    /// Session is `Stopped` or no longer exists: the listener loop should
    /// return permanently. Carries a human-readable reason for logging.
    Exit(&'static str),
    /// Session is `Paused`/`Suspended`, or its state couldn't be read: sleep
    /// one poll interval and check again. Carries an optional reason to
    /// warn-log (set only for the error case; pause/suspend is expected,
    /// routine operation and logs at a lower level by the caller).
    WaitAndRetry(Option<String>),
}

/// Pure status check used at the top of every `run_listener_loop` iteration.
/// Mirrors `team_session::run_one_cycle`'s own pause/stop handling for the
/// round-robin rotation, applied here to wake-on-demand listeners.
fn gate_on_session_status(project_root: &Path, session_id: &str) -> ListenerGate {
    match TeamSessionState::load(project_root, session_id) {
        Ok(Some(state)) if state.status == TeamSessionStatus::Stopped => {
            ListenerGate::Exit("team session stopped, listener exiting")
        }
        Ok(Some(state))
            if state.status == TeamSessionStatus::Paused
                || state.status == TeamSessionStatus::Suspended =>
        {
            ListenerGate::WaitAndRetry(None)
        }
        Ok(Some(_)) => ListenerGate::Proceed,
        Ok(None) => ListenerGate::Exit("team session no longer exists, listener exiting"),
        Err(e) => ListenerGate::WaitAndRetry(Some(format!(
            "failed to read team session state, will retry next poll: {e}"
        ))),
    }
}

/// The supervised loop for one `(session, listener)` pair. Runs until the
/// daemon shuts down or the team session disappears. See this module's doc
/// comment for why no external single-flight tracking is needed — this
/// loop's own sequential body is the single-flight guarantee.
async fn run_listener_loop(
    project_root: PathBuf,
    session_id: String,
    listener: WakeListenerConfig,
    ta_bin: PathBuf,
    transport: Arc<dyn WhiteboardTransport>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let consumer = format!("wake-listener:{}", listener.role);
    if let Err(e) = transport.connect().await {
        tracing::error!(
            role = %listener.role,
            error = %e,
            "wake_listener: failed to connect transport, listener not started"
        );
        return;
    }

    loop {
        // Live pause/stop check, re-read fresh every round -- not just once
        // at startup -- so `ta team-session pause <id>` actually stops new
        // inference from being spawned by this listener, the same way it
        // already stops the round-robin rotation (team_session.rs's own
        // `run_one_cycle`). Found live, 2026-10-01: before this check
        // existed, "paused" only ever applied to the rotation; wake-on-demand
        // listeners kept firing real `ta run` invocations regardless, which
        // is not what a human pausing a session would reasonably expect.
        //
        // Not reading from the stream while paused loses nothing -- JetStream
        // pull-consumer position is durable server-side, so a message that
        // arrives while paused is still there, unconsumed, once resumed.
        match gate_on_session_status(&project_root, &session_id) {
            ListenerGate::Proceed => {}
            ListenerGate::Exit(reason) => {
                tracing::info!(session = %session_id, role = %listener.role, "wake_listener: {reason}");
                return;
            }
            ListenerGate::WaitAndRetry(reason) => {
                if let Some(reason) = reason {
                    tracing::warn!(session = %session_id, role = %listener.role, "wake_listener: {reason}");
                }
                tokio::select! {
                    _ = tokio::time::sleep(POLL_INTERVAL) => {}
                    _ = shutdown.notified() => return,
                }
                continue;
            }
        }

        let mut launched_this_round = false;

        for key in &listener.keys {
            let next = tokio::select! {
                r = transport.stream_read_next(key, &consumer) => r,
                _ = shutdown.notified() => return,
            };

            let envelope = match next {
                Ok(Some(envelope)) => envelope,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        role = %listener.role,
                        key = %key,
                        error = %e,
                        "wake_listener: stream_read_next failed, will retry next poll"
                    );
                    continue;
                }
            };

            launched_this_round = true;
            let pr = project_root.clone();
            let sid = session_id.clone();
            let role = listener.role.clone();
            let bin = ta_bin.clone();
            let payload = envelope.payload.clone();
            let workflow_tag = listener.workflow_tag.clone();

            let launch_result = tokio::select! {
                r = tokio::task::spawn_blocking(move || {
                    launch_wake_on_demand(&pr, &sid, &role, &bin, &payload, workflow_tag.as_deref())
                }) => r,
                _ = shutdown.notified() => return,
            };

            match launch_result {
                Ok(Ok(())) => {
                    if let Err(e) = transport.stream_ack(key, &consumer, &envelope.msg_id).await {
                        tracing::warn!(
                            role = %listener.role,
                            key = %key,
                            error = %e,
                            "wake_listener: failed to ack message after successful launch -- \
                             will be redelivered next poll"
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        role = %listener.role,
                        key = %key,
                        error = %e,
                        "wake_listener: launch failed, message left unacked for redelivery"
                    );
                }
                Err(join_err) => {
                    tracing::error!(
                        role = %listener.role,
                        key = %key,
                        error = %join_err,
                        "wake_listener: launch task panicked, message left unacked for redelivery"
                    );
                }
            }
        }

        if !launched_this_round {
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = shutdown.notified() => return,
            }
        }
        // Launched at least one this round: loop immediately (no sleep) to
        // drain any remaining backlog before idling again.
    }
}

/// Synchronous launch of one wake-on-demand invocation — runs on a
/// blocking thread (see `run_listener_loop`), mirroring
/// `team_session::run_one_cycle`'s own subprocess-launch shape.
fn launch_wake_on_demand(
    project_root: &Path,
    session_id: &str,
    role: &str,
    ta_bin: &Path,
    payload: &[u8],
    workflow_tag: Option<&str>,
) -> std::io::Result<()> {
    let mut state = match TeamSessionState::load(project_root, session_id)? {
        Some(s) => s,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("wake_listener: team session '{session_id}' no longer exists"),
            ));
        }
    };

    let content = String::from_utf8_lossy(payload).to_string();
    let context_path = write_wake_on_demand_context(project_root, &state, role, &content)?;

    let team_config = TeamConfig::load(project_root).unwrap_or_default();
    let label = "wake-on-demand".to_string();
    let args = build_ta_run_args(
        project_root,
        &state,
        &label,
        role,
        &team_config,
        &context_path,
        workflow_tag,
    )
    .map_err(|e| {
        tracing::error!(session_id = %session_id, role = %role, error = %e, "refusing to launch wake-on-demand role");
        std::io::Error::other(e)
    })?;

    crate::team_session::ensure_stable_codesign(ta_bin, project_root);
    let output = std::process::Command::new(ta_bin)
        .args(&args)
        .current_dir(project_root)
        .output()?;

    if output.status.success() {
        let summary = String::from_utf8_lossy(&output.stdout).trim().to_string();
        state.findings.push(RoleFinding {
            stage: label,
            role: role.to_string(),
            completed_at: chrono::Utc::now(),
            summary: if summary.is_empty() {
                format!("Wake-on-demand role '{role}' completed with no stdout output.")
            } else {
                summary
            },
        });
        state.save(project_root)?;
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "wake_listener: ta run for role '{role}' exited with status {:?}, stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Renders context for a wake-on-demand invocation: `team_session.rs`'s
/// existing session-context framing (objective, role prompt, budget, prior
/// findings), plus the triggering message's raw content appended under its
/// own heading — no wake-on-demand-specific behavior baked into
/// `render_session_context` itself, per the design doc's "reuse the
/// existing mechanism, add a new caller" principle.
fn write_wake_on_demand_context(
    project_root: &Path,
    state: &TeamSessionState,
    role: &str,
    content: &str,
) -> std::io::Result<PathBuf> {
    let mut rendered = crate::team_session::render_session_context(project_root, state, role);
    rendered.push_str("\n## New intake\n\n");
    rendered.push_str(content);
    rendered.push('\n');

    let dir = TeamSessionState::state_dir(project_root, &state.id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "wake-context-{role}-{}.md",
        chrono::Utc::now().timestamp_millis()
    ));
    std::fs::write(&path, rendered)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_session::TeamSessionConfig;
    use ta_agent_whiteboard::InMemoryTransport;

    fn sample_config() -> TeamSessionConfig {
        TeamSessionConfig {
            name: "trading-desk".to_string(),
            workflow_path: "wf.yaml".to_string(),
            team_toml_path: "team.toml".to_string(),
            objective: "Generate income".to_string(),
            budget: None,
            role_prompts: std::collections::HashMap::new(),
            whiteboard_token: None,
            whiteboard_token_expires_at: None,
        }
    }

    #[tokio::test]
    async fn spawn_new_listeners_skips_already_known_and_picks_up_newly_added() {
        // Regression coverage for the same discovery gap fixed for
        // team_session::start (2026-10-02): a listener added to a session
        // after the daemon's initial scan must still get picked up -- by
        // the ongoing discovery loop calling this function again, not only
        // on a daemon restart. Exercises spawn_new_listeners directly
        // (not the full start()) since that's where the dedup logic lives.
        let tmp = tempfile::tempdir().unwrap();
        let listener_a =
            WakeListenerConfig::new("chief-of-staff", vec!["intake-a".to_string()], None);
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new())
            .with_wake_on_demand_listeners(vec![listener_a]);
        state.save(tmp.path()).unwrap();

        let transport: Arc<dyn WhiteboardTransport> = Arc::new(InMemoryTransport::new());
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let ta_bin = Path::new("ta");
        let mut known = std::collections::HashSet::new();
        let mut handles = Vec::new();

        spawn_new_listeners(
            tmp.path(),
            ta_bin,
            &transport,
            &shutdown,
            &mut known,
            &mut handles,
        );
        assert_eq!(
            handles.len(),
            1,
            "first scan must spawn the one existing listener"
        );
        assert_eq!(known.len(), 1);

        // Re-scan with nothing changed on disk -- must not double-spawn.
        spawn_new_listeners(
            tmp.path(),
            ta_bin,
            &transport,
            &shutdown,
            &mut known,
            &mut handles,
        );
        assert_eq!(
            handles.len(),
            1,
            "re-scanning an unchanged session must not spawn a second task for \
             the same listener"
        );

        // Add a second listener to the SAME session, simulating
        // `ta team-session start --wake-on-demand` run again against an
        // already-running daemon.
        let mut state = TeamSessionState::load(tmp.path(), "sess-1")
            .unwrap()
            .unwrap();
        state.wake_on_demand_listeners.push(WakeListenerConfig::new(
            "implementer",
            vec!["intake-b".to_string()],
            None,
        ));
        state.save(tmp.path()).unwrap();

        spawn_new_listeners(
            tmp.path(),
            ta_bin,
            &transport,
            &shutdown,
            &mut known,
            &mut handles,
        );
        assert_eq!(
            handles.len(),
            2,
            "a listener added after the initial scan must be discovered and \
             spawned, not require a daemon restart"
        );
        assert_eq!(known.len(), 2);

        shutdown.notify_waiters();
    }

    #[test]
    fn wake_listener_config_round_trips_through_team_session_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new())
            .with_wake_on_demand_listeners(vec![WakeListenerConfig::new(
                "chief-of-staff",
                vec!["external-intake".to_string()],
                None,
            )]);
        state.save(tmp.path()).unwrap();

        let loaded = TeamSessionState::load(tmp.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.wake_on_demand_listeners.len(), 1);
        assert_eq!(loaded.wake_on_demand_listeners[0].role, "chief-of-staff");
        assert_eq!(
            loaded.wake_on_demand_listeners[0].keys,
            vec!["external-intake".to_string()]
        );
        assert_eq!(loaded.wake_on_demand_listeners[0].workflow_tag, None);
    }

    #[test]
    fn gate_proceeds_for_active_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new());
        state.save(tmp.path()).unwrap();

        assert_eq!(
            gate_on_session_status(tmp.path(), "sess-1"),
            ListenerGate::Proceed
        );
    }

    #[test]
    fn gate_waits_and_retries_for_paused_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new());
        state.status = TeamSessionStatus::Paused;
        state.save(tmp.path()).unwrap();

        assert_eq!(
            gate_on_session_status(tmp.path(), "sess-1"),
            ListenerGate::WaitAndRetry(None),
            "a paused session must not proceed to read/launch -- it should wait and re-check, \
             the same way team_session's own round-robin rotation respects pause"
        );
    }

    #[test]
    fn gate_waits_and_retries_for_suspended_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new());
        state.status = TeamSessionStatus::Suspended;
        state.save(tmp.path()).unwrap();

        assert_eq!(
            gate_on_session_status(tmp.path(), "sess-1"),
            ListenerGate::WaitAndRetry(None)
        );
    }

    #[test]
    fn gate_exits_for_stopped_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new());
        state.status = TeamSessionStatus::Stopped;
        state.save(tmp.path()).unwrap();

        assert_eq!(
            gate_on_session_status(tmp.path(), "sess-1"),
            ListenerGate::Exit("team session stopped, listener exiting")
        );
    }

    #[test]
    fn gate_exits_when_session_no_longer_exists() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            gate_on_session_status(tmp.path(), "no-such-session"),
            ListenerGate::Exit("team session no longer exists, listener exiting")
        );
    }

    #[test]
    fn wake_listener_config_with_workflow_tag_round_trips_through_team_session_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new())
            .with_wake_on_demand_listeners(vec![WakeListenerConfig::new(
                "specialist",
                vec!["some-key".to_string()],
                Some("brain-maintenance".to_string()),
            )]);
        state.save(tmp.path()).unwrap();

        let loaded = TeamSessionState::load(tmp.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.wake_on_demand_listeners[0].workflow_tag.as_deref(),
            Some("brain-maintenance")
        );
    }

    #[test]
    fn state_json_without_wake_on_demand_listeners_field_still_loads() {
        // Backward compat: a state.json written before this field existed.
        let tmp = tempfile::tempdir().unwrap();
        let dir = TeamSessionState::state_dir(tmp.path(), "sess-1");
        std::fs::create_dir_all(&dir).unwrap();
        let json = serde_json::json!({
            "id": "sess-1",
            "config": sample_config(),
            "stages": [],
            "status": "active",
            "current_stage_index": 0,
            "findings": [],
            "restart_count": 0,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        });
        std::fs::write(dir.join("state.json"), json.to_string()).unwrap();

        let loaded = TeamSessionState::load(tmp.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert!(loaded.wake_on_demand_listeners.is_empty());
    }

    #[test]
    fn write_wake_on_demand_context_includes_role_prompt_and_message_content() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = sample_config();
        config.role_prompts.insert(
            "chief-of-staff".to_string(),
            "You triage intake.".to_string(),
        );
        let state = TeamSessionState::new("sess-1".to_string(), config, Vec::new());

        let path =
            write_wake_on_demand_context(tmp.path(), &state, "chief-of-staff", "raw chat text")
                .unwrap();
        let rendered = std::fs::read_to_string(path).unwrap();
        assert!(rendered.contains("You triage intake."));
        assert!(rendered.contains("## New intake"));
        assert!(rendered.contains("raw chat text"));
    }

    #[tokio::test]
    async fn a_message_published_before_the_listener_starts_is_still_delivered() {
        // Proves the durability property this whole mechanism depends on:
        // stream_append doesn't require a consumer to already exist.
        let transport = InMemoryTransport::new();
        transport.connect().await.unwrap();
        transport
            .stream_append("external-intake", b"hello".to_vec())
            .await
            .unwrap();

        let envelope = transport
            .stream_read_next("external-intake", "wake-listener:chief-of-staff")
            .await
            .unwrap()
            .expect("message published before any consumer existed must still be delivered");
        assert_eq!(envelope.payload, b"hello");
    }

    #[test]
    fn launch_wake_on_demand_errors_clearly_when_session_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let err = launch_wake_on_demand(
            tmp.path(),
            "no-such-session",
            "chief-of-staff",
            Path::new("ta"),
            b"content",
            None,
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(format!("{err}").contains("no-such-session"));
    }

    #[test]
    fn start_returns_no_handles_when_whiteboard_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let app_state = Arc::new(crate::api::AppState::new(
            tmp.path().to_path_buf(),
            crate::config::DaemonConfig::default(),
        ));
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let handles = start(&app_state, shutdown);
        assert!(handles.is_empty());
    }
}
