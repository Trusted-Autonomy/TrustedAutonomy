//! One self-update check: compare, decide, restart. Pure over an injected
//! environment so the daemon, the poller and the tests share the exact logic.

use serde::{Deserialize, Serialize};

use crate::drain::DrainSnapshot;
use crate::guard::{GuardDecision, GuardState, RestartGuard};
use crate::mode::AutoUpdateMode;
use crate::version::{compare_builds, BuildComparison, BuildIdentity};

/// Everything a check needs from the outside world.
pub trait UpdateEnv {
    /// The build that is running right now.
    fn running(&self) -> BuildIdentity;
    /// The build installed next to it (`<sibling> --version`).
    fn installed(&self) -> Result<BuildIdentity, String>;
    /// Current idle data. For the daemon this folds in goals, sessions,
    /// in-flight wake launches and `.ta/apply.lock`.
    fn drain(&self) -> Result<DrainSnapshot, String>;
    fn now_unix(&self) -> u64;
    /// Start the drain-aware restart onto `target`. Returns once the restart
    /// has been handed off.
    fn restart(&self, target: &BuildIdentity) -> Result<(), String>;
    fn can_self_restart(&self) -> bool {
        AutoUpdateMode::platform_can_self_restart()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    Disabled,
    UpToDate {
        running: BuildIdentity,
        /// Set when a previous restart attempt has now converged.
        recovered: Option<GuardState>,
    },
    CheckFailed {
        reason: String,
    },
    /// A newer build is installed and a person must apply it.
    AwaitingApproval {
        running: BuildIdentity,
        target: BuildIdentity,
        /// `when_idle` was configured but this platform cannot self-restart.
        degraded_from_when_idle: bool,
    },
    /// Newer build installed, daemon busy: waiting.
    Pending {
        running: BuildIdentity,
        target: BuildIdentity,
        blockers: Vec<String>,
    },
    Restarted {
        running: BuildIdentity,
        target: BuildIdentity,
        attempt: u32,
    },
    RestartFailed {
        running: BuildIdentity,
        target: BuildIdentity,
        attempt: u32,
        reason: String,
    },
    BackedOff {
        running: BuildIdentity,
        target: BuildIdentity,
        attempts: u32,
        retry_at_unix: u64,
    },
    GaveUp {
        running: BuildIdentity,
        target: BuildIdentity,
        attempts: u32,
        last_from: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

/// What `ta status` and the daemon status API show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    /// `up_to_date`, `disabled`, `check_failed`, `awaiting_approval`,
    /// `pending`, `restarting`, `restart_failed`, `backoff` or `gave_up`.
    pub state: String,
    pub mode: String,
    pub running: String,
    pub target: Option<String>,
    pub waiting_for: Vec<String>,
    /// Single line for `ta status`, e.g. "update pending, waiting for 2 running goals".
    pub message: String,
    pub checked_at_unix: u64,
}

impl UpdateReport {
    /// True when the report describes an update that has not been applied.
    pub fn is_update_outstanding(&self) -> bool {
        !matches!(
            self.state.as_str(),
            "up_to_date" | "disabled" | "check_failed"
        )
    }
}

impl UpdateOutcome {
    pub fn level(&self) -> Level {
        match self {
            Self::CheckFailed { .. }
            | Self::RestartFailed { .. }
            | Self::BackedOff { .. }
            | Self::GaveUp { .. } => Level::Warn,
            _ => Level::Info,
        }
    }

    fn state(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::UpToDate { .. } => "up_to_date",
            Self::CheckFailed { .. } => "check_failed",
            Self::AwaitingApproval { .. } => "awaiting_approval",
            Self::Pending { .. } => "pending",
            Self::Restarted { .. } => "restarting",
            Self::RestartFailed { .. } => "restart_failed",
            Self::BackedOff { .. } => "backoff",
            Self::GaveUp { .. } => "gave_up",
        }
    }

    /// A log line that says what happened, which builds are involved, what
    /// the user can do and which setting changes the behaviour.
    pub fn message(&self) -> String {
        match self {
            Self::Disabled => "daemon self-update check is off ([daemon] auto_update = \"never\"). \
                Set auto_update to \"ask\" or \"when_idle\" in .ta/daemon.toml to be told about, or apply, newer installed builds."
                .into(),
            Self::UpToDate { running, recovered } => match recovered {
                Some(prior) => format!(
                    "daemon update complete: now running {} (was {}) after {} restart attempt(s)",
                    running.describe(), prior.from, prior.attempts
                ),
                None => format!("daemon is up to date: running {}", running.describe()),
            },
            Self::CheckFailed { reason } => format!(
                "daemon self-update check could not run: {reason}. Nothing was restarted. \
                 Reinstall with ./install_local.sh or `ta daemon restart` once the installed ta-daemon runs."
            ),
            Self::AwaitingApproval { running, target, degraded_from_when_idle } => format!(
                "update available: running {}, installed {}. Not restarting automatically{}. \
                 Apply it with `ta daemon restart` (waits for running goals) or answer yes at the \
                 `ta shell` / `ta dev` prompt.",
                running.describe(), target.describe(),
                if *degraded_from_when_idle {
                    " because [daemon] auto_update = \"when_idle\" is inert on this platform (Windows cannot replace a running daemon safely)"
                } else {
                    " ([daemon] auto_update = \"ask\"); set it to \"when_idle\" to restart automatically when idle"
                }
            ),
            Self::Pending { running, target, blockers } => format!(
                "update pending, waiting for {}: running {}, installed {}. It will restart \
                 on its own as soon as the daemon is idle; nothing is interrupted. \
                 Use `ta daemon restart --force` only if you accept interrupting that work.",
                blockers.join(", "), running.describe(), target.describe()
            ),
            Self::Restarted { running, target, attempt } => format!(
                "daemon idle: restarting from {} onto installed {} (attempt {attempt})",
                running.describe(), target.describe()
            ),
            Self::RestartFailed { running, target, attempt, reason } => format!(
                "restart onto {} failed (attempt {attempt}, still running {}): {reason}. \
                 Check .ta/daemon.log, or run `ta daemon restart` yourself.",
                target.describe(), running.describe()
            ),
            Self::BackedOff { target, attempts, retry_at_unix, .. } => format!(
                "installed build {} did not take after {attempts} restart attempt(s); backing off, next try at unix time {retry_at_unix}. \
                 Check that the installed ta-daemon matches this build (`ta-daemon --version`), or run `ta daemon restart`.",
                target.describe()
            ),
            Self::GaveUp { running, target, attempts, last_from } => format!(
                "gave up auto-updating to {} after {attempts} restart attempt(s) (still running {}, last restarted from {last_from}). \
                 The restart is not converging: the new binary reports a different build than `ta-daemon --version` \
                 or crashes on start. Check .ta/daemon.log, reinstall with ./install_local.sh, then `ta daemon restart`. \
                 Delete .ta/daemon-update.json to re-arm, or set [daemon] auto_update = \"never\" to stop trying.",
                target.describe(), running.describe()
            ),
        }
    }

    pub fn report(&self, mode: AutoUpdateMode, now_unix: u64) -> UpdateReport {
        let (running, target, waiting_for) = match self {
            Self::Disabled | Self::CheckFailed { .. } => (String::new(), None, vec![]),
            Self::UpToDate { running, .. } => (running.describe(), None, vec![]),
            Self::Pending {
                running,
                target,
                blockers,
            } => (
                running.describe(),
                Some(target.describe()),
                blockers.clone(),
            ),
            Self::AwaitingApproval {
                running, target, ..
            }
            | Self::Restarted {
                running, target, ..
            }
            | Self::RestartFailed {
                running, target, ..
            }
            | Self::BackedOff {
                running, target, ..
            }
            | Self::GaveUp {
                running, target, ..
            } => (running.describe(), Some(target.describe()), vec![]),
        };
        UpdateReport {
            state: self.state().into(),
            mode: mode.to_string(),
            running,
            target,
            waiting_for,
            message: self.message(),
            checked_at_unix: now_unix,
        }
    }
}

/// Run one check. See the module docs of `ta_lifecycle` for the contract.
pub fn run_update_check(
    env: &dyn UpdateEnv,
    guard: &RestartGuard,
    mode: AutoUpdateMode,
) -> UpdateOutcome {
    if mode == AutoUpdateMode::Never {
        return UpdateOutcome::Disabled;
    }
    let running = env.running();
    let installed = match env.installed() {
        Ok(i) => i,
        Err(reason) => return UpdateOutcome::CheckFailed { reason },
    };
    if compare_builds(&running, &installed) == BuildComparison::Same {
        let recovered = guard.clear();
        return UpdateOutcome::UpToDate { running, recovered };
    }

    let effective = mode.effective_with(env.can_self_restart());
    if effective != AutoUpdateMode::WhenIdle {
        return UpdateOutcome::AwaitingApproval {
            running,
            target: installed,
            degraded_from_when_idle: mode == AutoUpdateMode::WhenIdle,
        };
    }

    let blockers = match env.drain() {
        Ok(snap) => snap.blockers(),
        Err(e) => vec![format!(
            "could not determine whether the daemon is idle ({e})"
        )],
    };
    if !blockers.is_empty() {
        return UpdateOutcome::Pending {
            running,
            target: installed,
            blockers,
        };
    }

    let now = env.now_unix();
    let attempt = match guard.evaluate(&installed, now) {
        GuardDecision::Proceed { attempt } => attempt,
        GuardDecision::Backoff {
            attempts,
            retry_at_unix,
        } => {
            return UpdateOutcome::BackedOff {
                running,
                target: installed,
                attempts,
                retry_at_unix,
            }
        }
        GuardDecision::GaveUp {
            attempts,
            last_from,
        } => {
            return UpdateOutcome::GaveUp {
                running,
                target: installed,
                attempts,
                last_from,
            }
        }
    };
    if let Err(reason) = guard.record_attempt(&installed, &running, attempt, now) {
        return UpdateOutcome::RestartFailed {
            running,
            target: installed,
            attempt,
            reason,
        };
    }
    match env.restart(&installed) {
        Ok(()) => UpdateOutcome::Restarted {
            running,
            target: installed,
            attempt,
        },
        Err(reason) => UpdateOutcome::RestartFailed {
            running,
            target: installed,
            attempt,
            reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Simulated environment: scripted builds, idle source and clock.
    struct Sim {
        running: BuildIdentity,
        installed: RefCell<Result<BuildIdentity, String>>,
        drain: RefCell<Result<DrainSnapshot, String>>,
        now: Cell<u64>,
        restarts: RefCell<Vec<BuildIdentity>>,
        restart_result: RefCell<Result<(), String>>,
        can_restart: bool,
    }

    fn id(h: &str) -> BuildIdentity {
        BuildIdentity::new("1.0.0", Some(h))
    }

    fn sim(running: &str, installed: &str, drain: DrainSnapshot) -> Sim {
        Sim {
            running: id(running),
            installed: RefCell::new(Ok(id(installed))),
            drain: RefCell::new(Ok(drain)),
            now: Cell::new(10_000),
            restarts: RefCell::new(vec![]),
            restart_result: RefCell::new(Ok(())),
            can_restart: true,
        }
    }

    impl UpdateEnv for Sim {
        fn running(&self) -> BuildIdentity {
            self.running.clone()
        }
        fn installed(&self) -> Result<BuildIdentity, String> {
            self.installed.borrow().clone()
        }
        fn drain(&self) -> Result<DrainSnapshot, String> {
            self.drain.borrow().clone()
        }
        fn now_unix(&self) -> u64 {
            self.now.get()
        }
        fn restart(&self, t: &BuildIdentity) -> Result<(), String> {
            self.restarts.borrow_mut().push(t.clone());
            self.restart_result.borrow().clone()
        }
        fn can_self_restart(&self) -> bool {
            self.can_restart
        }
    }

    fn guard() -> (tempfile::TempDir, RestartGuard) {
        let d = tempfile::tempdir().unwrap();
        let g = RestartGuard::for_project(d.path());
        (d, g)
    }

    fn busy_goals(n: u64) -> DrainSnapshot {
        DrainSnapshot {
            status: "draining".into(),
            active_goals: n,
            ..Default::default()
        }
    }

    #[test]
    fn stale_and_idle_restarts_onto_the_installed_build() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(
            matches!(out, UpdateOutcome::Restarted { attempt: 1, .. }),
            "{out:?}"
        );
        assert_eq!(*s.restarts.borrow(), vec![id("bbb")]);
        assert!(out.message().contains("1.0.0 (aaa)") && out.message().contains("1.0.0 (bbb)"));
    }

    #[test]
    fn stale_and_busy_waits_and_reports_pending_without_restarting() {
        let (_d, g) = guard();
        let s = sim(
            "aaa",
            "bbb",
            DrainSnapshot {
                status: "draining".into(),
                active_goals: 2,
                inflight_launches: 1,
                ..Default::default()
            },
        );
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(
            s.restarts.borrow().is_empty(),
            "must never interrupt running work"
        );
        let report = out.report(AutoUpdateMode::WhenIdle, 10_000);
        assert_eq!(report.state, "pending");
        assert!(report.is_update_outstanding());
        assert_eq!(
            report.waiting_for,
            vec![
                "2 running goals".to_string(),
                "1 in-flight wake launch".to_string()
            ]
        );
        assert!(
            report
                .message
                .starts_with("update pending, waiting for 2 running goals"),
            "{}",
            report.message
        );

        // Once the work finishes, the next check restarts.
        *s.drain.borrow_mut() = Ok(DrainSnapshot::clean());
        assert!(matches!(
            run_update_check(&s, &g, AutoUpdateMode::WhenIdle),
            UpdateOutcome::Restarted { .. }
        ));
    }

    #[test]
    fn unknown_idle_state_is_treated_as_busy() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        *s.drain.borrow_mut() = Err("connection refused".into());
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(matches!(out, UpdateOutcome::Pending { .. }));
        assert!(s.restarts.borrow().is_empty());
    }

    #[test]
    fn same_hash_does_nothing() {
        let (_d, g) = guard();
        let s = sim("aaa", "aaa", busy_goals(0));
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(matches!(
            out,
            UpdateOutcome::UpToDate {
                recovered: None,
                ..
            }
        ));
        assert!(s.restarts.borrow().is_empty());
        assert!(!out
            .report(AutoUpdateMode::WhenIdle, 1)
            .is_update_outstanding());
    }

    #[test]
    fn restart_loop_is_capped_with_backoff_then_gives_up() {
        let (_d, g) = guard();
        // The restarted daemon still reports "aaa" while the installed build is "bbb".
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        let mut restarts_seen = 0;
        let mut backed_off = false;
        let mut gave_up = false;
        for _ in 0..10 {
            match run_update_check(&s, &g, AutoUpdateMode::WhenIdle) {
                UpdateOutcome::Restarted { .. } => restarts_seen += 1,
                UpdateOutcome::GaveUp { attempts, .. } => {
                    assert_eq!(attempts, 3);
                    gave_up = true;
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
            // An immediate re-check must back off, not restart again.
            s.now.set(s.now.get() + 5);
            if matches!(
                run_update_check(&s, &g, AutoUpdateMode::WhenIdle),
                UpdateOutcome::BackedOff { .. }
            ) {
                backed_off = true;
            }
            s.now.set(s.now.get() + 1300);
        }
        assert!(gave_up && backed_off);
        assert_eq!(restarts_seen, 3, "stops after the cap");
        assert_eq!(s.restarts.borrow().len(), 3);
        // Even much later it stays given up.
        s.now.set(s.now.get() + 1_000_000);
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(matches!(out, UpdateOutcome::GaveUp { .. }));
        assert!(
            out.message().contains("daemon-update.json") && out.message().contains("auto_update")
        );
        assert_eq!(out.level(), Level::Warn);
    }

    #[test]
    fn backoff_blocks_an_immediate_second_attempt() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        assert!(matches!(
            run_update_check(&s, &g, AutoUpdateMode::WhenIdle),
            UpdateOutcome::Restarted { .. }
        ));
        s.now.set(s.now.get() + 10);
        assert!(matches!(
            run_update_check(&s, &g, AutoUpdateMode::WhenIdle),
            UpdateOutcome::BackedOff { attempts: 1, .. }
        ));
        assert_eq!(s.restarts.borrow().len(), 1);
    }

    #[test]
    fn convergence_clears_the_guard_and_says_so() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        // After the restart the process now runs "bbb".
        let after = sim("bbb", "bbb", DrainSnapshot::clean());
        let out = run_update_check(&after, &g, AutoUpdateMode::WhenIdle);
        match &out {
            UpdateOutcome::UpToDate {
                recovered: Some(p), ..
            } => assert_eq!(p.attempts, 1),
            other => panic!("{other:?}"),
        }
        assert!(out.message().contains("update complete"));
        // A later, different update starts from attempt 1 again.
        let next = sim("bbb", "ccc", DrainSnapshot::clean());
        assert!(matches!(
            run_update_check(&next, &g, AutoUpdateMode::WhenIdle),
            UpdateOutcome::Restarted { attempt: 1, .. }
        ));
    }

    #[test]
    fn failed_restart_is_reported_and_counts_toward_the_cap() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        *s.restart_result.borrow_mut() = Err("cannot spawn ta".into());
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(matches!(
            out,
            UpdateOutcome::RestartFailed { attempt: 1, .. }
        ));
        assert!(out.message().contains("cannot spawn ta") && out.message().contains("daemon.log"));
        assert_eq!(g.load().attempts, 1);
    }

    #[test]
    fn ask_never_restarts_and_never_disables_reporting() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        let out = run_update_check(&s, &g, AutoUpdateMode::Ask);
        assert!(matches!(
            out,
            UpdateOutcome::AwaitingApproval {
                degraded_from_when_idle: false,
                ..
            }
        ));
        assert!(s.restarts.borrow().is_empty());
        assert!(out.message().contains("ta daemon restart") && out.message().contains("when_idle"));
    }

    #[test]
    fn never_does_not_even_look() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        assert_eq!(
            run_update_check(&s, &g, AutoUpdateMode::Never),
            UpdateOutcome::Disabled
        );
        assert!(s.restarts.borrow().is_empty());
    }

    #[test]
    fn when_idle_is_inert_where_the_platform_cannot_self_restart() {
        let (_d, g) = guard();
        let mut s = sim("aaa", "bbb", DrainSnapshot::clean());
        s.can_restart = false;
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(matches!(
            out,
            UpdateOutcome::AwaitingApproval {
                degraded_from_when_idle: true,
                ..
            }
        ));
        assert!(s.restarts.borrow().is_empty());
        assert!(out.message().contains("inert"));
    }

    #[test]
    fn check_failure_leaves_everything_alone() {
        let (_d, g) = guard();
        let s = sim("aaa", "bbb", DrainSnapshot::clean());
        *s.installed.borrow_mut() = Err("`ta-daemon --version` exited with 1".into());
        let out = run_update_check(&s, &g, AutoUpdateMode::WhenIdle);
        assert!(matches!(out, UpdateOutcome::CheckFailed { .. }));
        assert!(s.restarts.borrow().is_empty());
    }
}
