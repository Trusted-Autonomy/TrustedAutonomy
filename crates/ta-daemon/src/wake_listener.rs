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
use ta_mcp_gateway::secret_redact::redact_lines;
use ta_session::team::TeamConfig;
use uuid::Uuid;

use crate::team_session::{build_ta_run_args, RoleFinding, TeamSessionState, TeamSessionStatus};
use crate::wake_retry::{
    process_message, recover_pending, sanitize_component, tail_lines, AttemptStore,
    LaunchRateGuard, ListenerIds, WakeRetryPolicy,
};

/// How many trailing stderr lines a failed launch's error (and so the
/// daemon log and dead-letter record) carries. The full output is always
/// in the per-launch log file.
const STDERR_TAIL_LINES: usize = 40;

/// How often an idle listener (nothing currently on its stream) re-polls.
/// Deliberately short and decoupled from `team_session.rs`'s rotation
/// period — this independence is the whole point of this module.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// One role's wake-on-demand registration: launch `role` via `ta run`
/// whenever a message arrives on any of `keys`.
///
/// `role` and `agent_id` answer two different questions, and both matter
/// once more than one listener shares a role (e.g. three `engineer`
/// listeners registered for horizontal capacity — see
/// `run_listener_loop`'s durable-consumer-name doc comment for why they
/// compete for the same stream): `role` is "find me any capable of this
/// work" (the routing/capability key, shared on purpose); `agent_id` is
/// "this specific seat" (globally unique, stable for this listener's
/// entire lifetime, spans every goal it ever launches -- unlike a goal's
/// own `goal_run_id`, which is fresh per invocation and means nothing once
/// that one goal ends). A whiteboard claim or presence record that needs
/// to mean something beyond a single goal's lifetime (v0.17.11.2's shared,
/// persistent task list) should be attributed to `agent_id`, never `role`
/// or `goal_run_id` alone -- `role` can't disambiguate between siblings,
/// and `goal_run_id` can't be recognized again once that goal exits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WakeListenerConfig {
    /// Globally unique and long-lived: generated once when this listener
    /// is registered (`ta team-session start`'s `--wake-on-demand`
    /// parsing) and persisted in `state.json` from then on -- stable
    /// across every goal this listener ever launches and across daemon
    /// restarts. `Uuid::nil()` only for a listener loaded from a
    /// pre-v0.17.11.16 `state.json` that predates this field; such a
    /// listener has no persistent identity until the session is
    /// recreated (no automatic migration -- see this crate's CLAUDE.md on
    /// not silently reassigning identity data).
    #[serde(default)]
    pub agent_id: Uuid,
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
            agent_id: Uuid::new_v4(),
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

/// `true` the first time it is called for this (project, session, role, key)
/// in this process, so the startup recovery sweep runs once per stream.
fn claim_recovery(project_root: &Path, session: &str, role: &str, key: &str) -> bool {
    static SWEPT: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
        std::sync::Mutex::new(None);
    let id = format!(
        "{}\u{1f}{session}\u{1f}{role}\u{1f}{key}",
        project_root.display()
    );
    SWEPT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(id)
}

/// The supervised loop for one `(session, listener)` pair. Runs until the
/// daemon shuts down or the team session disappears. See this module's doc
/// comment for why no external single-flight tracking is needed — this
/// loop's own sequential body is the single-flight guarantee.
pub(crate) async fn run_listener_loop(
    project_root: PathBuf,
    session_id: String,
    listener: WakeListenerConfig,
    ta_bin: PathBuf,
    transport: Arc<dyn WhiteboardTransport>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let consumer = format!("wake-listener:{}", listener.role);
    let attempts = AttemptStore::for_listener(&project_root, &session_id, &listener.role);
    let mut rate = LaunchRateGuard::default();
    // Refuse to start on timing settings that could let the transport
    // redeliver a message that is still running (see validate_timings).
    if let Err(reason) = WakeRetryPolicy::load(&project_root).validate_timings() {
        tracing::error!(
            session = %session_id,
            role = %listener.role,
            reason = %reason,
            "wake_listener: listener NOT started because the wake timing settings conflict"
        );
        return;
    }
    if let Err(e) = transport.connect().await {
        tracing::error!(
            role = %listener.role,
            error = %e,
            "wake_listener: failed to connect transport, listener not started"
        );
        return;
    }

    // Ack wait last applied to this listener's consumer, per key.
    let mut applied_ack_wait: Option<Duration> = None;
    let mut last_timing_warning: Option<std::time::Instant> = None;
    // The startup sweep for messages an earlier run left pending runs once,
    // as soon as the ack wait has been applied.
    let mut recovered = false;

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
        // Re-read each round so a [whiteboard] wake_* change in
        // workflow.toml applies without a daemon restart.
        let policy = WakeRetryPolicy::load(&project_root);

        // A settings edit that breaks the timing rules pauses this listener
        // (the stream is left unread, nothing is lost) instead of risking a
        // redelivery of a running launch; fixing the file resumes it.
        if let Err(reason) = policy.validate_timings() {
            if last_timing_warning.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
                tracing::error!(
                    session = %session_id,
                    role = %listener.role,
                    reason = %reason,
                    "wake_listener: paused, not reading messages, until the wake timing \
                     settings are fixed"
                );
                last_timing_warning = Some(std::time::Instant::now());
            }
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = shutdown.notified() => return,
            }
            continue;
        }
        if applied_ack_wait != Some(policy.ack_wait) {
            let mut all_applied = true;
            for key in &listener.keys {
                if let Err(e) = transport
                    .stream_set_ack_wait(key, &consumer, policy.ack_wait)
                    .await
                {
                    all_applied = false;
                    tracing::warn!(
                        session = %session_id,
                        role = %listener.role,
                        key = %key,
                        ack_wait_secs = policy.ack_wait.as_secs(),
                        error = %e,
                        "wake_listener: could not set the consumer's ack wait; will retry next \
                         poll. Until then a launch longer than the transport's default ack wait \
                         relies on heartbeats alone."
                    );
                }
            }
            if all_applied {
                tracing::info!(
                    session = %session_id,
                    role = %listener.role,
                    ack_wait_secs = policy.ack_wait.as_secs(),
                    heartbeat_secs = policy.ack_heartbeat.as_secs(),
                    launch_timeout_secs = policy.launch_timeout.as_secs(),
                    "wake_listener: ack wait set"
                );
                applied_ack_wait = Some(policy.ack_wait);
            }
        }
        if !recovered && applied_ack_wait.is_some() {
            recovered = true;
            // Once per (project, session, role, key) per daemon process: a
            // sibling listener starting later must not reset a consumer
            // whose messages are being worked on.
            let keys: Vec<String> = listener
                .keys
                .iter()
                .filter(|k| claim_recovery(&project_root, &session_id, &listener.role, k))
                .cloned()
                .collect();
            if !keys.is_empty() {
                recover_pending(
                    transport.as_ref(),
                    &project_root,
                    &session_id,
                    &listener.role,
                    &keys,
                    &consumer,
                    &policy,
                    chrono::Utc::now(),
                )
                .await;
            }
        }

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

            let ids = ListenerIds {
                project_root: &project_root,
                session: &session_id,
                role: &listener.role,
                key,
                consumer: &consumer,
            };
            let pr = project_root.clone();
            let sid = session_id.clone();
            let role = listener.role.clone();
            let agent_id = listener.agent_id;
            let bin = ta_bin.clone();
            let payload = envelope.payload.clone();
            let workflow_tag = listener.workflow_tag.clone();
            let msg_id = envelope.msg_id.clone();
            let launch_timeout = policy.launch_timeout;
            let launch = move || async move {
                match tokio::task::spawn_blocking(move || {
                    launch_wake_on_demand(
                        &pr,
                        &sid,
                        &role,
                        agent_id,
                        &bin,
                        &payload,
                        workflow_tag.as_deref(),
                        &msg_id,
                        launch_timeout,
                    )
                })
                .await
                {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(join_err) => Err(format!("launch task panicked: {join_err}")),
                }
            };

            let outcome = tokio::select! {
                o = process_message(
                    transport.as_ref(),
                    &attempts,
                    &policy,
                    &mut rate,
                    &ids,
                    &envelope,
                    chrono::Utc::now,
                    launch,
                ) => o,
                _ = shutdown.notified() => return,
            };
            if outcome.made_progress() {
                launched_this_round = true;
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
#[allow(clippy::too_many_arguments)]
fn launch_wake_on_demand(
    project_root: &Path,
    session_id: &str,
    role: &str,
    agent_id: Uuid,
    ta_bin: &Path,
    payload: &[u8],
    workflow_tag: Option<&str>,
    msg_id: &str,
    launch_timeout: Duration,
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
    let files = write_wake_on_demand_context(project_root, &state, role, &content)?;
    let context_path = files.context.clone();

    let team_config = TeamConfig::load(project_root).unwrap_or_default();
    let label = "wake-on-demand".to_string();
    let args = build_ta_run_args(
        project_root,
        &state,
        &label,
        role,
        agent_id,
        &team_config,
        &context_path,
        workflow_tag,
    )
    .map_err(|e| {
        tracing::error!(session_id = %session_id, role = %role, error = %e, "refusing to launch wake-on-demand role");
        std::io::Error::other(e)
    })?;
    let args = with_intake_file(args, &files.intake);

    crate::team_session::ensure_stable_codesign(ta_bin, project_root);
    let output = run_and_record_launch(
        project_root,
        session_id,
        role,
        msg_id,
        ta_bin,
        &args,
        launch_timeout,
    )?;

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
}

/// Runs one launch, always saving its full stdout and stderr under
/// `.ta/logs/wake-launches/`. A non-zero exit becomes an error carrying the
/// log path and the last `STDERR_TAIL_LINES` lines of stderr (the daemon log
/// used to keep only the first line, which hid why the live CoS failed).
fn run_and_record_launch(
    project_root: &Path,
    session_id: &str,
    role: &str,
    msg_id: &str,
    ta_bin: &Path,
    args: &[String],
    launch_timeout: Duration,
) -> std::io::Result<std::process::Output> {
    let started_at = chrono::Utc::now();
    let mut command = std::process::Command::new(ta_bin);
    command.args(args).current_dir(project_root);
    let (output, timed_out) = output_with_timeout(command, launch_timeout)?;

    let log_path = launch_log_path(project_root, session_id, role, msg_id, started_at);
    if let Err(e) = write_launch_log(&log_path, ta_bin, args, msg_id, started_at, &output) {
        tracing::warn!(
            path = %log_path.display(),
            error = %e,
            "wake_listener: could not write the launch log"
        );
    } else {
        tracing::info!(
            session = %session_id,
            role = %role,
            msg_id = %msg_id,
            exit_code = ?output.status.code(),
            log = %log_path.display(),
            "wake_listener: launch finished, full output saved"
        );
    }

    if timed_out {
        Err(std::io::Error::other(format!(
            "wake_listener: ta run for role '{role}' (session '{session_id}', msg {msg_id}) was \
             killed after {}s without finishing. Output so far: {}. Raise \
             wake_launch_timeout_secs (and wake_ack_wait_secs if you set it) in [whiteboard] of \
             .ta/workflow.toml if launches legitimately run this long.",
            launch_timeout.as_secs(),
            log_path.display()
        )))
    } else if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(std::io::Error::other(format!(
            "wake_listener: ta run for role '{role}' (session '{session_id}', msg {msg_id}) \
             exited with status {:?}. Full stdout and stderr: {}\n\
             Last {STDERR_TAIL_LINES} lines of stderr:\n{}",
            output.status.code(),
            log_path.display(),
            tail_lines(&redact_lines(stderr.trim_end()), STDERR_TAIL_LINES)
        )))
    }
}

/// Runs `command` to completion like `Command::output`, but kills it if it
/// is still running after `timeout`. Returns the collected output and
/// whether it was killed.
fn output_with_timeout(
    mut command: std::process::Command,
    timeout: Duration,
) -> std::io::Result<(std::process::Output, bool)> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            buf
        })
    }
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = std::time::Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // After a kill, a grandchild may still hold the pipes open, and joining
    // the readers would then block this launch (and its heartbeats) forever.
    // Detach them instead; they end when the pipes close.
    let (stdout, stderr) = if timed_out {
        (Vec::new(), Vec::new())
    } else {
        (
            out.join().unwrap_or_default(),
            err.join().unwrap_or_default(),
        )
    };
    Ok((
        std::process::Output {
            status,
            stdout,
            stderr,
        },
        timed_out,
    ))
}

/// `.ta/logs/wake-launches/<session>-<role>-<UTC timestamp>-<msg_id prefix>.log`
fn launch_log_path(
    project_root: &Path,
    session_id: &str,
    role: &str,
    msg_id: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> PathBuf {
    let msg_prefix: String = sanitize_component(msg_id).chars().take(12).collect();
    project_root
        .join(".ta")
        .join("logs")
        .join("wake-launches")
        .join(format!(
            "{}-{}-{}-{}.log",
            sanitize_component(session_id),
            sanitize_component(role),
            at.format("%Y%m%dT%H%M%S%.3fZ"),
            msg_prefix
        ))
}

/// Full record of one launch: what ran, how it exited, everything it
/// printed. `ta run --headless` echoes the agent's stream-json on stdout,
/// so this also captures the agent transcript as seen by the daemon.
fn write_launch_log(
    path: &Path,
    ta_bin: &Path,
    args: &[String],
    msg_id: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    output: &std::process::Output,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = String::new();
    body.push_str(&format!("# wake-on-demand launch\nmsg_id: {msg_id}\n"));
    body.push_str(&format!("started_at: {}\n", started_at.to_rfc3339()));
    body.push_str(&format!(
        "finished_at: {}\n",
        chrono::Utc::now().to_rfc3339()
    ));
    body.push_str(&format!(
        "command: {} {}\n",
        ta_bin.display(),
        args.join(" ")
    ));
    body.push_str(&format!("exit_code: {:?}\n", output.status.code()));
    body.push_str("\n===== stdout =====\n");
    body.push_str(&redact_lines(&String::from_utf8_lossy(&output.stdout)));
    body.push_str("\n===== stderr =====\n");
    body.push_str(&redact_lines(&String::from_utf8_lossy(&output.stderr)));
    std::fs::write(path, body)
}

/// The two files written for one wake-on-demand launch.
#[derive(Debug)]
struct WakeLaunchFiles {
    /// Trusted session context (objective, role prompt, budget, prior
    /// findings), passed as `--objective-file`.
    context: PathBuf,
    /// The triggering message exactly as received, passed as
    /// `--intake-file`. It comes from untrusted sources (chat, forum posts,
    /// meeting notes), so `ta run` puts it into the agent's first message
    /// inside a fenced block labeled as untrusted data, and states its
    /// `candidate_id` outside the fence as a value parsed by code.
    intake: PathBuf,
}

/// Renders context for a wake-on-demand invocation: `team_session.rs`'s
/// existing session-context framing (objective, role prompt, budget, prior
/// findings) in one file, and the triggering message's raw content in a
/// separate intake file. The intake is kept out of the objective file on
/// purpose: the objective is delivered as trusted text, the intake never is.
fn write_wake_on_demand_context(
    project_root: &Path,
    state: &TeamSessionState,
    role: &str,
    content: &str,
) -> std::io::Result<WakeLaunchFiles> {
    let mut rendered = crate::team_session::render_session_context(project_root, state, role);
    rendered.push_str("\n## New intake\n\n");
    rendered.push_str(
        "TA delivers this wake's intake in your first message, inside a fenced block \
         labeled as untrusted data, with its candidate_id stated before the block.\n",
    );

    let dir = TeamSessionState::state_dir(project_root, &state.id);
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Utc::now().timestamp_millis();
    let context = dir.join(format!("wake-context-{role}-{stamp}.md"));
    std::fs::write(&context, rendered)?;
    let intake = dir.join(format!("wake-intake-{role}-{stamp}.json"));
    std::fs::write(&intake, content)?;
    Ok(WakeLaunchFiles { context, intake })
}

/// Appends `--intake-file <path>` to a wake launch's `ta run` arguments.
fn with_intake_file(mut args: Vec<String>, intake: &Path) -> Vec<String> {
    args.push("--intake-file".to_string());
    args.push(intake.to_string_lossy().to_string());
    args
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
    fn wake_listener_config_new_generates_a_real_non_nil_agent_id() {
        let a = WakeListenerConfig::new("engineer", vec!["implement".to_string()], None);
        let b = WakeListenerConfig::new("engineer", vec!["implement".to_string()], None);
        assert_ne!(a.agent_id, Uuid::nil());
        assert_ne!(
            a.agent_id, b.agent_id,
            "two listeners registered under the same role must still get distinct, \
             independently-stable agent ids -- role is the shared routing key, agent_id is not"
        );
    }

    #[test]
    fn agent_id_round_trips_through_team_session_state() {
        let tmp = tempfile::tempdir().unwrap();
        let listener = WakeListenerConfig::new("engineer", vec!["implement".to_string()], None);
        let expected_id = listener.agent_id;
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new())
            .with_wake_on_demand_listeners(vec![listener]);
        state.save(tmp.path()).unwrap();

        let loaded = TeamSessionState::load(tmp.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.wake_on_demand_listeners[0].agent_id, expected_id);
    }

    #[test]
    fn state_json_without_agent_id_field_defaults_to_nil_rather_than_failing_to_load() {
        // Backward compat: a state.json written before this field existed
        // (pre-v0.17.11.16) must still load -- with a nil agent_id flagging
        // "no persistent identity assigned yet" rather than crashing. Built
        // by saving a real state then stripping just the new field from the
        // on-disk JSON, rather than hand-maintaining a full literal state.json
        // shape here that would silently drift from the real struct over time.
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new())
            .with_wake_on_demand_listeners(vec![WakeListenerConfig::new(
                "chief-of-staff",
                vec!["external-intake".to_string()],
                None,
            )]);
        state.save(tmp.path()).unwrap();

        let state_path = TeamSessionState::state_dir(tmp.path(), "sess-1").join("state.json");
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        raw["wake_on_demand_listeners"][0]
            .as_object_mut()
            .unwrap()
            .remove("agent_id");
        std::fs::write(&state_path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let loaded = TeamSessionState::load(tmp.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.wake_on_demand_listeners[0].agent_id, Uuid::nil());
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

        let files =
            write_wake_on_demand_context(tmp.path(), &state, "chief-of-staff", "raw chat text")
                .unwrap();
        let rendered = std::fs::read_to_string(&files.context).unwrap();
        assert!(rendered.contains("You triage intake."));
        assert!(rendered.contains("## New intake"));
        // The untrusted message never goes into the trusted objective file;
        // it is written, unchanged, to the separate intake file.
        assert!(!rendered.contains("raw chat text"), "{rendered}");
        assert_eq!(
            std::fs::read_to_string(&files.intake).unwrap(),
            "raw chat text"
        );
    }

    #[test]
    fn wake_launch_args_pass_the_intake_file_alongside_the_objective_file() {
        let base = vec![
            "run".to_string(),
            "title".to_string(),
            "--objective-file".to_string(),
            "/x/ctx.md".to_string(),
        ];
        let args = with_intake_file(base, Path::new("/x/intake.json"));
        let i = args.iter().position(|a| a == "--intake-file").unwrap();
        assert_eq!(args[i + 1], "/x/intake.json");
        assert!(args.contains(&"--objective-file".to_string()));
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
            Uuid::new_v4(),
            Path::new("ta"),
            b"content",
            None,
            "0",
            Duration::from_secs(60),
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(format!("{err}").contains("no-such-session"));
    }

    #[cfg(unix)]
    #[test]
    fn a_launch_past_its_timeout_is_killed_with_an_actionable_error() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("slow-ta.sh");
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let err = run_and_record_launch(
            tmp.path(),
            "sess-1",
            "chief-of-staff",
            "seq-7",
            &fake,
            &[],
            Duration::from_millis(300),
        )
        .unwrap_err()
        .to_string();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(err.contains("wake_launch_timeout_secs"), "{err}");
        assert!(
            err.contains("seq-7") && err.contains("chief-of-staff"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_launch_saves_full_output_and_reports_the_stderr_tail() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();

        // A fake `ta` that prints 100 stderr lines and fails, like the live
        // "No changes detected" exit whose detail the daemon log lost.
        let fake = tmp.path().join("fake-ta.sh");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho transcript-on-stdout\n\
             i=1; while [ $i -le 100 ]; do echo \"err line $i\" >&2; i=$((i+1)); done\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Calls the recording runner directly (not launch_wake_on_demand)
        // so the test never invokes codesign on the developer's machine.
        let err = run_and_record_launch(
            tmp.path(),
            "sess-1",
            "chief-of-staff",
            "seq-42",
            &fake,
            &["run".to_string(), "--headless".to_string()],
            Duration::from_secs(60),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("err line 100"), "{err}");
        assert!(err.contains("err line 61"), "{err}");
        assert!(
            !err.contains("err line 60\n"),
            "only the last 40 lines: {err}"
        );

        let dir = tmp.path().join(".ta/logs/wake-launches");
        let logs: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(logs.len(), 1);
        let name = logs[0].file_name().to_string_lossy().to_string();
        assert!(name.starts_with("sess-1-chief-of-staff-"), "{name}");
        assert!(name.ends_with("-seq-42.log"), "{name}");
        assert!(err.contains(&name), "error must name the log file: {err}");
        let body = std::fs::read_to_string(logs[0].path()).unwrap();
        assert!(body.contains("transcript-on-stdout"));
        assert!(body.contains("err line 1\n"));
        assert!(body.contains("err line 100"));
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

    /// A session with no rotation stages: the rotation supervisor launches
    /// nothing, and the wake listener still launches the role for a message.
    #[cfg(unix)]
    #[tokio::test]
    async fn wake_only_session_launches_the_role_on_a_message_and_runs_no_rotation() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("invocations.log");
        let ta_bin = tmp.path().join("fake-ta");
        std::fs::write(
            &ta_bin,
            format!("#!/bin/sh\necho launched >> '{}'\nexit 0\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&ta_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let listener =
            WakeListenerConfig::new("chief-of-staff", vec!["intake-a".to_string()], None);
        TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new())
            .with_wake_on_demand_listeners(vec![listener.clone()])
            .save(tmp.path())
            .unwrap();

        let transport: Arc<dyn WhiteboardTransport> = Arc::new(InMemoryTransport::new());
        transport
            .stream_append("intake-a", b"hello".to_vec())
            .await
            .unwrap();
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let rotation = tokio::spawn(crate::team_session::run_team_session(
            tmp.path().to_path_buf(),
            "sess-1".to_string(),
            ta_bin.clone(),
            shutdown.clone(),
        ));
        let wake = tokio::spawn(run_listener_loop(
            tmp.path().to_path_buf(),
            "sess-1".to_string(),
            listener,
            ta_bin,
            transport,
            shutdown,
        ));

        let count = || {
            std::fs::read_to_string(&log)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while count() == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Give a (wrongly) running rotation time to launch as well.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        rotation.abort();
        wake.abort();

        assert_eq!(
            count(),
            1,
            "exactly one launch: the wake message. A second would be a rotation cycle."
        );
        let state = TeamSessionState::load(tmp.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(state.findings.len(), 1);
        assert_eq!(state.findings[0].stage, "wake-on-demand");
        assert_eq!(state.findings[0].role, "chief-of-staff");
    }
}
