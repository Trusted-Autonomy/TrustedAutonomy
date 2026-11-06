//! Reports worker goals that could not start.
//!
//! The Wayfinder poller launches a worker goal with `ta run --origin cos
//! --provenance-file`, as a child of the poller, not of the daemon, so the wake
//! path never sees it fail. `ta run` records why in the goal (`launch_failure`,
//! state `failed`); this watcher notices and publishes the same `launch_failed`
//! outcome the wake path publishes, keyed by the goal's BARE candidate id (the
//! `#nonce` is deliberately never given to a worker, so it is not on the goal).
//!
//! Each goal is reported once: `.ta/launch-failed-published/<goal id>` marks it.
//! Only goals with a provenance candidate id are reported here. A Chief-of-Staff
//! goal woken by the daemon is reported by the wake path, which has the full id.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ta_agent_whiteboard::WhiteboardTransport;
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};

/// How often the goals directory is scanned.
const SCAN_INTERVAL: Duration = Duration::from_secs(10);
/// A failed goal older than this is not reported (it predates this daemon run
/// by long enough that the poller has moved on).
const MAX_AGE_HOURS: i64 = 24;
/// The stamped team session for outcomes that belong to no wake listener.
const WATCH_SESSION: &str = "daemon";

fn published_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("launch-failed-published")
}

fn marker_path(project_root: &Path, goal: &GoalRun) -> PathBuf {
    published_dir(project_root).join(goal.goal_run_id.to_string())
}

/// True when `goal` is a failed worker goal whose launch failure has not been reported.
fn needs_report(project_root: &Path, goal: &GoalRun, now: chrono::DateTime<chrono::Utc>) -> bool {
    matches!(goal.state, GoalRunState::Failed { .. })
        && goal.launch_failure.is_some()
        && goal
            .request_provenance
            .as_ref()
            .is_some_and(|p| p.ids.candidate_id.is_some())
        && now - goal.updated_at < chrono::Duration::hours(MAX_AGE_HOURS)
        && !marker_path(project_root, goal).exists()
}

/// One pass: publish every unreported failed worker goal. Returns how many.
pub async fn scan_once(
    transport: &dyn WhiteboardTransport,
    project_root: &Path,
    goals_dir: &Path,
) -> usize {
    let store = match GoalRunStore::new(goals_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(dir = %goals_dir.display(), error = %e, "goal_watch: cannot open the goal store, launch failures are not being reported. Check that .ta/goals is readable");
            return 0;
        }
    };
    let goals = match store.list() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(dir = %goals_dir.display(), error = %e, "goal_watch: cannot list goals");
            return 0;
        }
    };
    let now = chrono::Utc::now();
    let mut reported = 0;
    for goal in goals.iter().filter(|g| needs_report(project_root, g, now)) {
        let (Some(failure), Some(candidate)) = (&goal.launch_failure, goal.bare_candidate_id())
        else {
            continue;
        };
        // Mark first: a crash between publishing and marking would report twice;
        // a crash the other way round would never report. publish_or_queue itself
        // queues on a transport failure, so "marked" means "handed to the outbox".
        let dir = published_dir(project_root);
        let marked = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(marker_path(project_root, goal), &candidate));
        if let Err(e) = marked {
            tracing::error!(goal_id = %goal.goal_run_id, error = %e, "goal_watch: could not record that this goal's launch failure was reported; skipping it so it is not reported on every pass. Check that .ta/ is writable");
            continue;
        }
        crate::launch_failed::publish_or_queue(
            transport,
            project_root,
            WATCH_SESSION,
            Some(&goal.goal_run_id.to_string()),
            &candidate,
            failure,
        )
        .await;
        reported += 1;
    }
    reported
}

/// Start the watcher for this project when the whiteboard transport is enabled.
pub fn start(app_state: &Arc<crate::api::AppState>, shutdown: Arc<tokio::sync::Notify>) {
    let Some(transport) = app_state.whiteboard_transport.clone() else {
        tracing::debug!(
            "goal_watch: [whiteboard] disabled for this project, not watching for launch failures"
        );
        return;
    };
    let project_root = app_state.project_root.clone();
    let goals_dir = app_state.goals_dir.clone();
    tokio::spawn(async move {
        loop {
            crate::launch_failed::flush_outbox(transport.as_ref(), &project_root).await;
            scan_once(transport.as_ref(), &project_root, &goals_dir).await;
            tokio::select! {
                _ = tokio::time::sleep(SCAN_INTERVAL) => {}
                _ = shutdown.notified() => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_agent_whiteboard::InMemoryTransport;
    use ta_goal::{LaunchFailure, LaunchFailureCode};

    const OUT: &str = crate::api::whiteboard::EXTERNAL_OUTCOME_STREAM;

    fn failed_goal(candidate: Option<&str>, failure: bool, fail_it: bool) -> GoalRun {
        let mut g = GoalRun::new(
            "Fix the login page",
            "o",
            "claude-code",
            PathBuf::from("/w"),
            PathBuf::from("/s"),
        );
        g.request_provenance = Some(
            serde_json::from_value(serde_json::json!({
                "requester": {"id": "wayfinder", "channel": "wayfinder"},
                "ids": {"candidate_id": candidate},
            }))
            .unwrap(),
        );
        g.transition(GoalRunState::Configured).unwrap();
        g.transition(GoalRunState::Running).unwrap();
        if fail_it {
            g.transition(GoalRunState::Failed {
                reason: "the agent never started".into(),
            })
            .unwrap();
        }
        if failure {
            g.launch_failure = Some(LaunchFailure {
                code: LaunchFailureCode::NoLogin,
                detail: "the agent said it is not logged in".into(),
                fix: "Log in from Wayfinder Settings > Agent login, or run: ta agent-login login claude-code".into(),
            });
        }
        g
    }

    async fn drain(t: &InMemoryTransport) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Some(m) = t.stream_read_next(OUT, "poller").await.unwrap() {
            out.push(serde_json::from_slice(&m.payload).unwrap());
            t.stream_ack(OUT, "poller", &m.msg_id).await.unwrap();
        }
        out
    }

    #[tokio::test]
    async fn a_failed_worker_goal_is_reported_once_with_its_bare_candidate_id() {
        let tmp = tempfile::tempdir().unwrap();
        let goals_dir = tmp.path().join("goals");
        let store = GoalRunStore::new(&goals_dir).unwrap();
        store
            .save(&failed_goal(Some("wayfinder-task:T-7"), true, true))
            .unwrap();
        let t = InMemoryTransport::new();

        assert_eq!(scan_once(&t, tmp.path(), &goals_dir).await, 1);
        let out = drain(&t).await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["candidate_id"], "wayfinder-task:T-7");
        assert_eq!(out[0]["outcome"], "launch_failed");
        assert_eq!(out[0]["reason_code"], "no_login");
        assert!(out[0]["fix"]
            .as_str()
            .unwrap()
            .contains("ta agent-login login"));

        // Not again, on this pass or the next.
        assert_eq!(scan_once(&t, tmp.path(), &goals_dir).await, 0);
        assert!(drain(&t).await.is_empty());
    }

    #[tokio::test]
    async fn healthy_and_unrelated_goals_publish_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let goals_dir = tmp.path().join("goals");
        let store = GoalRunStore::new(&goals_dir).unwrap();
        // Running normally.
        store
            .save(&failed_goal(Some("wayfinder-task:T-1"), false, false))
            .unwrap();
        // Failed for another reason (no launch classification).
        store
            .save(&failed_goal(Some("wayfinder-task:T-2"), false, true))
            .unwrap();
        // Failed to launch, but not a Wayfinder candidate.
        store.save(&failed_goal(None, true, true)).unwrap();
        let t = InMemoryTransport::new();
        assert_eq!(scan_once(&t, tmp.path(), &goals_dir).await, 0);
        assert!(drain(&t).await.is_empty());
    }
}
