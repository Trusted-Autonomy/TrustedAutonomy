// self_update.rs: the daemon's periodic self-check (`[daemon] auto_update`).
//
// Every `update_check_interval_secs` (default 300) the daemon compares its own
// build with `<sibling ta-daemon> --version`. The decision logic lives in the
// reusable `ta-lifecycle` crate (so the VT poller daemon runs the same code);
// this module only supplies the daemon's side of `UpdateEnv`:
//
//   * idle = no running goals, no recently active agent sessions, no in-flight
//     wake launches, no live `.ta/apply.lock`;
//   * restart = hand off to a detached `ta daemon restart --port <port>`, the
//     same drain-aware path as the manual command, so a goal that starts in
//     the meantime is waited for, never interrupted;
//   * the latest outcome is published for `/api/status` and `ta status`.
//
// `auto_update = "ask"` (the default) never restarts: it only reports, and the
// CLI's existing prompt applies the update, so nothing here ever blocks on a
// person.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ta_lifecycle::{
    run_update_check, AutoUpdateMode, BuildIdentity, DrainSnapshot, InflightCounter, Level,
    RestartGuard, UpdateEnv, UpdateReport,
};
use tokio::sync::Notify;

use crate::api::AppState;

/// Wake-on-demand launches currently running (see `wake_listener`).
pub static WAKE_LAUNCHES: InflightCounter = InflightCounter::new();

/// Agent sessions count as activity only if used within this window; a
/// session left open in a UI is not work in progress.
const SESSION_ACTIVE_WINDOW_SECS: i64 = 300;

/// Delay before the first check, so a freshly restarted daemon reports the
/// completed update soon instead of one full interval later.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);

static LATEST: Mutex<Option<UpdateReport>> = Mutex::new(None);

/// The latest self-check result, for the status API. `None` before the first
/// check has run.
pub fn current_report() -> Option<UpdateReport> {
    LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn publish(report: UpdateReport) {
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(report);
}

/// Goals, in-flight launches and apply lock. Shared by `/api/drain/status`
/// and the self-check so both agree on what "busy" means. `inflight` is the
/// current `WAKE_LAUNCHES` count (a parameter so tests do not share the static).
pub fn core_snapshot(goals_dir: &Path, project_root: &Path, inflight: usize) -> DrainSnapshot {
    let active_goals = match ta_goal::GoalRunStore::new(goals_dir) {
        Ok(store) => store
            .list()
            .unwrap_or_default()
            .iter()
            .filter(|g| {
                matches!(
                    g.state,
                    ta_goal::GoalRunState::Running | ta_goal::GoalRunState::Configured
                )
            })
            .count(),
        Err(_) => 0,
    };
    let applying =
        ta_lifecycle::apply_lock_state(project_root, &ta_lifecycle::process_is_alive).is_blocking();
    let mut snap = DrainSnapshot {
        status: String::new(),
        active_goals: active_goals as u64,
        active_sessions: 0,
        inflight_launches: inflight as u64,
        applying,
    };
    snap.status = if snap.is_idle() { "clean" } else { "draining" }.into();
    snap
}

/// Count sessions that are running and were used recently.
pub fn recently_active_sessions(sessions: &[crate::api::agent::AgentSession]) -> u64 {
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(SESSION_ACTIVE_WINDOW_SECS);
    sessions
        .iter()
        .filter(|s| {
            s.status == crate::api::agent::SessionStatus::Running && s.last_active >= cutoff
        })
        .count() as u64
}

/// The identity of this running binary.
pub fn running_identity() -> BuildIdentity {
    BuildIdentity::new(env!("CARGO_PKG_VERSION"), Some(env!("TA_GIT_HASH")))
}

/// Arguments for the detached restart helper (`ta daemon restart`).
pub fn restart_helper_args(port: u16) -> Vec<String> {
    vec![
        "daemon".into(),
        "restart".into(),
        "--port".into(),
        port.to_string(),
    ]
}

struct DaemonEnv {
    project_root: PathBuf,
    daemon_bin: Result<PathBuf, String>,
    port: u16,
    snapshot: DrainSnapshot,
}

impl UpdateEnv for DaemonEnv {
    fn running(&self) -> BuildIdentity {
        running_identity()
    }

    fn installed(&self) -> Result<BuildIdentity, String> {
        let bin = self.daemon_bin.as_ref().map_err(|e| e.clone())?;
        ta_lifecycle::read_installed_identity(bin)
    }

    fn drain(&self) -> Result<DrainSnapshot, String> {
        Ok(self.snapshot.clone())
    }

    fn now_unix(&self) -> u64 {
        ta_lifecycle::Clock::now_unix(&ta_lifecycle::SystemClock)
    }

    fn restart(&self, target: &BuildIdentity) -> Result<(), String> {
        spawn_restart_helper(&self.project_root, self.port, target)
    }
}

#[cfg(unix)]
fn spawn_restart_helper(
    project_root: &Path,
    port: u16,
    target: &BuildIdentity,
) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let ta_bin = PathBuf::from(crate::web::find_ta_binary_web());
    let log = project_root.join(".ta").join("daemon.log");
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("cannot open {} for the restart helper: {e}", log.display()))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("cannot clone log handle: {e}"))?;

    let mut child = Command::new(&ta_bin)
        .args(restart_helper_args(port))
        .current_dir(project_root)
        .env("TA_DAEMON_SELF_UPDATE", "1")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        // Own process group: the helper must outlive the daemon it stops.
        .process_group(0)
        .spawn()
        .map_err(|e| {
            format!(
                "cannot start `{} daemon restart`: {e}. Run `ta daemon restart` yourself, or make sure `ta` is next to ta-daemon",
                ta_bin.display()
            )
        })?;
    let target = target.describe();
    std::thread::spawn(move || match child.wait() {
        Ok(status) if status.success() => {
            tracing::info!(target = %target, "self-update: restart helper finished");
        }
        Ok(status) => tracing::error!(
            target = %target,
            status = %status,
            "self-update: restart helper failed; the daemon keeps running the old build. \
             Run `ta daemon restart` and check .ta/daemon.log"
        ),
        Err(e) => {
            tracing::error!(target = %target, error = %e, "self-update: cannot wait for restart helper")
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn spawn_restart_helper(_: &Path, _: u16, _: &BuildIdentity) -> Result<(), String> {
    Err("automatic restart is not supported on this platform; run `ta daemon restart`".into())
}

/// Spawn the periodic self-check. A no-op (with one log line) for `never`.
pub fn start(app_state: &Arc<AppState>, shutdown: Arc<Notify>) {
    let configured = app_state.daemon_config.daemon.auto_update;
    let interval = app_state.daemon_config.daemon.check_interval();
    let effective = configured.effective();

    if configured == AutoUpdateMode::Never {
        tracing::info!(
            "self-update: [daemon] auto_update = \"never\", the daemon will not check for newer installed builds. \
             Set it to \"ask\" or \"when_idle\" in .ta/daemon.toml to change that."
        );
        publish(ta_lifecycle::UpdateOutcome::Disabled.report(configured, now()));
        return;
    }
    if effective != configured {
        tracing::warn!(
            configured = %configured,
            effective = %effective,
            "self-update: [daemon] auto_update = \"{configured}\" is inert on this platform \
             (a running daemon cannot be replaced safely here); behaving as \"{effective}\": updates are \
             reported, apply them with `ta daemon restart`"
        );
    }
    tracing::info!(
        mode = %effective,
        interval_secs = interval.as_secs(),
        "self-update: checking for newer installed builds (set [daemon] auto_update / update_check_interval_secs in .ta/daemon.toml to change)"
    );

    let state = app_state.clone();
    tokio::spawn(async move {
        let mut wait = FIRST_CHECK_DELAY.min(interval);
        let mut last_logged = String::new();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.notified() => return,
            }
            wait = interval;
            check_once(&state, configured, &mut last_logged).await;
        }
    });
}

fn now() -> u64 {
    ta_lifecycle::Clock::now_unix(&ta_lifecycle::SystemClock)
}

async fn check_once(state: &Arc<AppState>, mode: AutoUpdateMode, last_logged: &mut String) {
    let mut snapshot = core_snapshot(&state.goals_dir, &state.project_root, WAKE_LAUNCHES.count());
    let sessions = state.agent_sessions.list_sessions().await;
    snapshot.active_sessions = recently_active_sessions(&sessions);
    if snapshot.active_sessions > 0 && snapshot.status == "clean" {
        snapshot.status = "draining".into();
    }

    let env = DaemonEnv {
        project_root: state.project_root.clone(),
        daemon_bin: ta_lifecycle::locate_sibling_binary("ta-daemon"),
        port: state.daemon_config.server.port,
        snapshot,
    };
    let guard = RestartGuard::for_project(&state.project_root);
    let outcome = match tokio::task::spawn_blocking(move || run_update_check(&env, &guard, mode))
        .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "self-update: check task panicked; nothing was restarted");
            return;
        }
    };

    let message = outcome.message();
    let changed = *last_logged != message;
    match (outcome.level(), changed) {
        (Level::Warn, true) => tracing::warn!("self-update: {message}"),
        (Level::Info, true) => tracing::info!("self-update: {message}"),
        (_, false) => tracing::debug!("self-update: {message}"),
    }
    *last_logged = message;
    publish(outcome.report(mode, now()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_snapshot_is_clean_for_an_empty_project() {
        let d = tempfile::tempdir().unwrap();
        let goals = d.path().join("goals");
        std::fs::create_dir_all(&goals).unwrap();
        let snap = core_snapshot(&goals, d.path(), 0);
        assert!(snap.is_idle(), "{snap:?}");
        assert_eq!(snap.status, "clean");
    }

    #[test]
    fn core_snapshot_counts_inflight_wake_launches() {
        let d = tempfile::tempdir().unwrap();
        let goals = d.path().join("goals");
        std::fs::create_dir_all(&goals).unwrap();
        let counter = ta_lifecycle::InflightCounter::new();
        let guard = counter.begin();
        let busy = core_snapshot(&goals, d.path(), counter.count());
        assert_eq!(busy.inflight_launches, 1);
        assert_eq!(busy.status, "draining");
        drop(guard);
        assert!(core_snapshot(&goals, d.path(), counter.count()).is_idle());
    }

    #[test]
    fn core_snapshot_treats_a_live_apply_lock_as_busy_and_a_stale_one_as_idle() {
        let d = tempfile::tempdir().unwrap();
        let goals = d.path().join("goals");
        std::fs::create_dir_all(&goals).unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        let lock = d.path().join(".ta").join("apply.lock");
        // This test process is alive, so a lock naming it is a live apply.
        std::fs::write(
            &lock,
            format!(r#"{{"pid":{},"draft_id":"d1"}}"#, std::process::id()),
        )
        .unwrap();
        let busy = core_snapshot(&goals, d.path(), 0);
        assert!(busy.applying);
        assert!(busy.blockers().iter().any(|b| b.contains("apply.lock")));
        // pid 0 never names a live process.
        std::fs::write(&lock, r#"{"pid":0,"draft_id":"d1"}"#).unwrap();
        assert!(!core_snapshot(&goals, d.path(), 0).applying);
    }

    #[test]
    fn only_recent_running_sessions_count() {
        use crate::api::agent::{AgentSession, SessionStatus};
        let mk = |status, age_secs: i64| AgentSession {
            session_id: "s".into(),
            agent: "a".into(),
            status,
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now() - chrono::Duration::seconds(age_secs),
            prompt_count: 0,
        };
        let sessions = vec![
            mk(SessionStatus::Running, 10),
            mk(SessionStatus::Running, 3600),
            mk(SessionStatus::Stopped, 10),
            mk(SessionStatus::Idle, 10),
        ];
        assert_eq!(recently_active_sessions(&sessions), 1);
    }

    #[test]
    fn restart_helper_uses_the_drain_aware_command_not_force() {
        let args = restart_helper_args(7733);
        assert_eq!(args, ["daemon", "restart", "--port", "7733"]);
        assert!(!args.iter().any(|a| a == "--force"));
    }

    #[test]
    fn running_identity_matches_the_build_stamps() {
        let id = running_identity();
        assert_eq!(id.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn published_report_round_trips() {
        let r = ta_lifecycle::UpdateOutcome::Disabled.report(AutoUpdateMode::Never, 5);
        publish(r.clone());
        assert_eq!(current_report(), Some(r));
    }
}
