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

use crate::team_session::{build_ta_run_args, RoleFinding, TeamSessionState};

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
}

impl WakeListenerConfig {
    pub fn new(role: impl Into<String>, keys: Vec<String>) -> Self {
        Self {
            role: role.into(),
            keys,
        }
    }
}

/// Discovers all team sessions' `wake_on_demand_listeners` and spawns one
/// supervised watcher task per `(session, listener)` pair — mirrors
/// `team_session::start`'s "read config, spawn one task per entry" shape
/// and its one-time-at-startup discovery (a listener added to a session
/// while the daemon is already running needs a daemon restart to pick up,
/// same limitation `team_session::start` already has for new sessions).
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

    for id in TeamSessionState::list_ids(&project_root) {
        let Ok(Some(state)) = TeamSessionState::load(&project_root, &id) else {
            continue;
        };
        for listener in state.wake_on_demand_listeners.clone() {
            let pr = project_root.clone();
            let bin = ta_bin.clone();
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
    handles
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

            let launch_result = tokio::select! {
                r = tokio::task::spawn_blocking(move || {
                    launch_wake_on_demand(&pr, &sid, &role, &bin, &payload)
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
    let args = build_ta_run_args(&state, &label, role, &team_config, &context_path);

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
        }
    }

    #[test]
    fn wake_listener_config_round_trips_through_team_session_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), Vec::new())
            .with_wake_on_demand_listeners(vec![WakeListenerConfig::new(
                "chief-of-staff",
                vec!["external-intake".to_string()],
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
