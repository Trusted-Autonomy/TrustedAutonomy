// action_worker.rs: the daemon's durable action queue worker (v0.17.12.3).
//
// At startup the worker (1) finishes or rolls back any `ta draft apply` that
// was interrupted, using the apply journal, and (2) moves any action that was
// mid-send when the daemon last stopped to `needs attention`: its outcome is
// unknown and TA never guesses or resends. Then it drains the queue on a
// timer, forever: compensable actions first, irrecoverable ones last, policy
// re-checked before every attempt, growing retry delays.
//
// The queue and the journals live in the per-user state directory, outside the
// project tree, so an agent cannot write them. See `ta_actions::queue`.
//
// Design: docs/superpowers/specs/2026-10-07-atomic-apply-and-action-queue-design.md.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ta_actions::{
    discover_adapter_actions, recover_all, ActionExecutor, ActionQueue, ActionRegistry,
    ApplyEffects, Clock, PackageStatusEffects, ProjectSendPolicy, QueueWorker, RecoveryOutcome,
    RegistryExecutor, RetryPolicy, RunReport, SendPolicy, SystemClock,
};
use ta_credentials::state::{ProjectState, StateLocator};

/// How often the worker looks for due actions.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What startup did, for the log and for tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StartupReport {
    pub applies_rolled_forward: usize,
    pub applies_rolled_back: usize,
    pub applies_failed: usize,
    pub unknown_outcomes: Vec<String>,
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Safety: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Without a cheap liveness probe, assume alive: recovery then waits
        // for the lock to be removed rather than racing a live apply.
        let _ = pid;
        true
    }
}

/// Opens the project's queue (creating the state directory on first use).
fn open(
    project_root: &Path,
    locator: &StateLocator,
) -> Result<(ProjectState, ActionQueue), String> {
    let ps = ProjectState::open(project_root, locator).map_err(|e| e.to_string())?;
    let queue = ActionQueue::open(ps.action_queue_dir()).map_err(|e| e.to_string())?;
    Ok((ps, queue))
}

/// Startup recovery. Synchronous so it is directly testable.
pub fn startup_with(
    ps: &ProjectState,
    queue: &ActionQueue,
    effects: &dyn ApplyEffects,
    clock: &dyn Clock,
) -> StartupReport {
    let mut report = StartupReport::default();
    for r in recover_all(&ps.apply_journal_dir(), effects, queue, clock) {
        match r.outcome {
            RecoveryOutcome::RolledForward { enqueued } => {
                report.applies_rolled_forward += 1;
                tracing::warn!(draft_id = %r.draft_id, enqueued,
                    "finished an interrupted apply: the draft was Applied, its actions are now queued");
            }
            RecoveryOutcome::RolledBack {
                restored,
                commit_undone,
            } => {
                report.applies_rolled_back += 1;
                tracing::warn!(draft_id = %r.draft_id, restored, commit_undone,
                    "rolled back an interrupted apply: the draft was not Applied; re-run `ta draft apply`");
            }
            RecoveryOutcome::Failed { reason } => {
                report.applies_failed += 1;
                tracing::error!(draft_id = %r.draft_id, reason = %reason,
                    "could not recover an interrupted apply; it will be retried on the next start");
            }
            RecoveryOutcome::Discarded | RecoveryOutcome::InProgress => {}
        }
    }
    match queue.recover_in_flight(clock) {
        Ok(keys) => report.unknown_outcomes = keys,
        Err(e) => tracing::error!(error = %e,
            "could not scan the action queue for in-flight actions; run `ta show actions`"),
    }
    report
}

/// One drain pass with explicit collaborators.
pub fn run_pass_with(
    queue: &ActionQueue,
    clock: &dyn Clock,
    executor: &dyn ActionExecutor,
    policy: &dyn SendPolicy,
) -> Result<RunReport, ta_actions::QueueError> {
    let report = QueueWorker {
        queue,
        clock,
        executor,
        policy,
        retry: RetryPolicy::default(),
    }
    .run_once()?;
    for k in &report.done {
        tracing::info!(key = %k, "outside action sent");
    }
    for k in &report.retrying {
        tracing::warn!(key = %k, "outside action failed and will be retried with a growing delay");
    }
    for k in &report.needs_attention {
        tracing::warn!(key = %k, "outside action needs attention; run `ta show actions`");
    }
    Ok(report)
}

fn registry_for(project_root: &Path) -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    for a in discover_adapter_actions(project_root) {
        registry.register(a);
    }
    registry
}

/// Spawns the worker for the daemon's lifetime.
pub fn start(
    project_root: PathBuf,
    shutdown: Arc<tokio::sync::Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let locator = StateLocator::from_env();
        let root = project_root.clone();
        let loc = locator.clone();
        let startup = tokio::task::spawn_blocking(move || {
            let (ps, queue) = open(&root, &loc)?;
            let effects = PackageStatusEffects {
                pr_packages_dir: root.join(".ta").join("pr_packages"),
                lock_root: root.clone(),
                pid_alive,
            };
            Ok::<_, String>(startup_with(&ps, &queue, &effects, &SystemClock))
        })
        .await;
        match startup {
            Ok(Ok(r)) => tracing::info!(?r, "action queue worker started"),
            Ok(Err(e)) => {
                tracing::error!(error = %e,
                    "the action queue worker could not open the state directory, so queued outside actions will NOT be sent. Check the daemon state directory with `ta show state`, then run `ta daemon restart`");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "action queue startup task failed");
                return;
            }
        }
        loop {
            let root = project_root.clone();
            let loc = locator.clone();
            let pass = tokio::task::spawn_blocking(move || {
                let (_ps, queue) = open(&root, &loc)?;
                let registry = registry_for(&root);
                let executor = RegistryExecutor {
                    registry: &registry,
                };
                let policy = ProjectSendPolicy::new(&root);
                run_pass_with(&queue, &SystemClock, &executor, &policy).map_err(|e| e.to_string())
            })
            .await;
            match pass {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::error!(error = %e, "action queue pass failed; will retry"),
                Err(e) => tracing::error!(error = %e, "action queue pass task failed; will retry"),
            }
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = shutdown.notified() => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ta_actions::{
        ApplyJournal, Idempotency, ManualClock, NewAction, QueueEntry, QueueState, Reversibility,
    };

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-12T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn act(id: &str, class: Reversibility) -> NewAction {
        NewAction {
            draft_id: "d1".into(),
            action_id: id.into(),
            tool_name: "ta_external_action:email".into(),
            parameters: serde_json::json!({}),
            description: id.into(),
            class,
            undo: None,
            idempotency: Idempotency::None,
            undo_of: None,
        }
    }

    struct Fixture {
        _project: tempfile::TempDir,
        _state: tempfile::TempDir,
        ps: ProjectState,
        queue: ActionQueue,
        pr_packages: PathBuf,
        project: PathBuf,
    }

    fn fixture() -> Fixture {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let loc = StateLocator::at(state.path());
        let (ps, queue) = open(project.path(), &loc).unwrap();
        let pr_packages = project.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&pr_packages).unwrap();
        Fixture {
            project: project.path().to_path_buf(),
            _project: project,
            _state: state,
            ps,
            queue,
            pr_packages,
        }
    }

    fn effects(f: &Fixture) -> PackageStatusEffects {
        PackageStatusEffects {
            pr_packages_dir: f.pr_packages.clone(),
            lock_root: f.project.clone(),
            pid_alive: |_| false,
        }
    }

    struct Always(ta_actions::ExecOutcome);
    impl ActionExecutor for Always {
        fn execute(&self, _: &QueueEntry, _: &str) -> ta_actions::ExecOutcome {
            self.0.clone()
        }
    }
    struct Allow;
    impl SendPolicy for Allow {
        fn check(&self, _: &QueueEntry) -> ta_actions::PolicyVerdict {
            ta_actions::PolicyVerdict::Allow
        }
    }

    #[test]
    fn startup_moves_in_flight_actions_to_needs_attention_and_never_resends() {
        let f = fixture();
        let clock = ManualClock::new(t0());
        f.queue
            .enqueue_batch(&[act("a1", Reversibility::Irrecoverable)], &clock)
            .unwrap();
        // The previous daemon recorded intent, then died.
        let sent = Always(ta_actions::ExecOutcome::Done {
            result: serde_json::json!({}),
        });
        // Use the public API only: claim by running a pass whose executor panics
        // is not a crash, so simulate the crash by appending the intent line.
        let line = serde_json::json!({
            "event": "sending", "key": "d1:a1", "attempt": 1, "at": t0()
        });
        let mut text = std::fs::read_to_string(f.queue.journal_path()).unwrap();
        text.push_str(&format!("{line}\n"));
        std::fs::write(f.queue.journal_path(), text).unwrap();
        assert_eq!(
            f.queue.snapshot().unwrap().entries[0].state,
            QueueState::Sending
        );

        let report = startup_with(&f.ps, &f.queue, &effects(&f), &clock);
        assert_eq!(report.unknown_outcomes, vec!["d1:a1".to_string()]);
        let r = run_pass_with(&f.queue, &clock, &sent, &Allow).unwrap();
        assert!(r.done.is_empty(), "an unknown outcome is never resent");
        assert_eq!(
            f.queue.snapshot().unwrap().entries[0].state,
            QueueState::NeedsAttention
        );
    }

    #[test]
    fn startup_finishes_an_apply_that_was_marked_applied_and_rolls_back_one_that_was_not() {
        let f = fixture();
        let clock = ManualClock::new(t0());
        let target = f.project.join("work");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("a.txt"), "old").unwrap();

        // Draft "applied-draft" crashed after the status flip.
        let mut j = ApplyJournal::begin(
            &f.ps.apply_journal_dir(),
            "applied-draft",
            &target,
            vec![NewAction {
                draft_id: "applied-draft".into(),
                ..act("m1", Reversibility::Irrecoverable)
            }],
            None,
        )
        .unwrap();
        j.commit_begun().unwrap();
        std::fs::write(
            f.pr_packages.join("applied-draft.json"),
            r#"{"status":{"status":"applied","applied_at":"2026-10-12T00:00:00Z"}}"#,
        )
        .unwrap();
        drop(j);

        // Draft "pending-draft" crashed after overwriting a.txt but before the flip.
        let mut j = ApplyJournal::begin(
            &f.ps.apply_journal_dir(),
            "pending-draft",
            &target,
            vec![],
            None,
        )
        .unwrap();
        j.commit_begun().unwrap();
        j.record_prior("a.txt").unwrap();
        std::fs::write(target.join("a.txt"), "half applied").unwrap();
        std::fs::write(
            f.pr_packages.join("pending-draft.json"),
            r#"{"status":{"status":"approved","approved_by":"x","approved_at":"2026-10-12T00:00:00Z"}}"#,
        )
        .unwrap();
        drop(j);

        let report = startup_with(&f.ps, &f.queue, &effects(&f), &clock);
        assert_eq!(report.applies_rolled_forward, 1);
        assert_eq!(report.applies_rolled_back, 1);
        assert_eq!(
            std::fs::read_to_string(target.join("a.txt")).unwrap(),
            "old"
        );
        let snap = f.queue.snapshot().unwrap();
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.entries[0].key, "applied-draft:m1");
        assert_eq!(snap.entries[0].state, QueueState::Queued);
    }

    #[test]
    fn a_pass_drains_due_actions_and_stops_at_unknown() {
        let f = fixture();
        let clock = ManualClock::new(t0());
        f.queue
            .enqueue_batch(&[act("a1", Reversibility::Compensable)], &clock)
            .unwrap();
        let r = run_pass_with(
            &f.queue,
            &clock,
            &Always(ta_actions::ExecOutcome::Done {
                result: serde_json::json!({"id": 1}),
            }),
            &Allow,
        )
        .unwrap();
        assert_eq!(r.done, vec!["d1:a1".to_string()]);
        let r = run_pass_with(
            &f.queue,
            &clock,
            &Always(ta_actions::ExecOutcome::Unknown {
                message: "x".into(),
            }),
            &Allow,
        )
        .unwrap();
        assert!(r.done.is_empty() && r.needs_attention.is_empty());
    }

    #[test]
    fn the_queue_and_journals_are_in_the_state_dir_not_the_project() {
        let f = fixture();
        assert!(!f.queue.dir().starts_with(&f.project));
        assert!(!f.ps.apply_journal_dir().starts_with(&f.project));
    }
}
