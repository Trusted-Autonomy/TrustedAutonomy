//! "Is it idle?": the drain snapshot, the apply lock, in-flight counters, and
//! the drain-aware restart orchestration shared by the CLI and other daemons.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::clock::Clock;

/// What `GET /api/drain/status` reports. Every field defaults, so an older
/// daemon that only reports `status`, `active_goals` and `active_sessions`
/// still parses.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DrainSnapshot {
    /// `"clean"` or `"draining"`.
    pub status: String,
    pub active_goals: u64,
    pub active_sessions: u64,
    /// Wake-on-demand (or any other) launches currently running.
    pub inflight_launches: u64,
    /// A `ta draft apply` holds a live `.ta/apply.lock`.
    pub applying: bool,
}

impl DrainSnapshot {
    pub fn clean() -> Self {
        Self {
            status: "clean".into(),
            ..Self::default()
        }
    }

    /// Snapshot for a program that only tracks its own in-flight records.
    pub fn from_inflight(inflight: usize) -> Self {
        Self {
            status: if inflight == 0 { "clean" } else { "draining" }.into(),
            inflight_launches: inflight as u64,
            ..Self::default()
        }
    }

    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }

    /// Human-readable reasons the daemon is not idle (empty when idle).
    pub fn blockers(&self) -> Vec<String> {
        fn plural(n: u64, one: &str, many: &str) -> String {
            format!("{n} {}", if n == 1 { one } else { many })
        }
        let mut out = Vec::new();
        if self.active_goals > 0 {
            out.push(plural(self.active_goals, "running goal", "running goals"));
        }
        if self.active_sessions > 0 {
            out.push(plural(
                self.active_sessions,
                "active agent session",
                "active agent sessions",
            ));
        }
        if self.inflight_launches > 0 {
            out.push(plural(
                self.inflight_launches,
                "in-flight wake launch",
                "in-flight wake launches",
            ));
        }
        if self.applying {
            out.push("a draft apply in progress (.ta/apply.lock)".into());
        }
        if out.is_empty() && self.status == "draining" {
            out.push("the daemon reports it is draining".into());
        }
        out
    }

    pub fn is_idle(&self) -> bool {
        self.blockers().is_empty()
    }
}

/// Counts in-flight work (launches, requests) for a drain snapshot.
/// `const`-constructible so it can live in a `static`.
#[derive(Debug, Default)]
pub struct InflightCounter {
    n: AtomicUsize,
}

/// Decrements its counter on drop, including on panic and early return.
#[derive(Debug)]
pub struct InflightGuard<'a> {
    counter: &'a InflightCounter,
}

impl InflightCounter {
    pub const fn new() -> Self {
        Self {
            n: AtomicUsize::new(0),
        }
    }
    pub fn begin(&self) -> InflightGuard<'_> {
        self.n.fetch_add(1, Ordering::SeqCst);
        InflightGuard { counter: self }
    }
    pub fn count(&self) -> usize {
        self.n.load(Ordering::SeqCst)
    }
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.counter.n.fetch_sub(1, Ordering::SeqCst);
    }
}

/// State of `<project>/.ta/apply.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyLockState {
    Absent,
    /// Held by a live process: an apply is running.
    Live {
        pid: u32,
        draft_id: String,
    },
    /// Left behind by a process that is gone: does not block.
    Stale {
        pid: u32,
    },
    /// Present but unreadable: treated as busy, the conservative choice.
    Unreadable(String),
}

impl ApplyLockState {
    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Live { .. } | Self::Unreadable(_))
    }
}

/// Inspect `.ta/apply.lock`. `alive` decides whether the pid is running.
pub fn apply_lock_state(project_root: &Path, alive: &dyn Fn(u32) -> bool) -> ApplyLockState {
    let path = project_root.join(".ta").join("apply.lock");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ApplyLockState::Absent,
        Err(e) => return ApplyLockState::Unreadable(format!("{}: {e}", path.display())),
    };
    let json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(j) => j,
        Err(e) => return ApplyLockState::Unreadable(format!("{}: {e}", path.display())),
    };
    let pid = json["pid"].as_u64().unwrap_or(0) as u32;
    if pid != 0 && alive(pid) {
        ApplyLockState::Live {
            pid,
            draft_id: json["draft_id"].as_str().unwrap_or("unknown").to_string(),
        }
    } else {
        ApplyLockState::Stale { pid }
    }
}

/// Whether a process with this pid is running (`kill -0` / `tasklist`).
pub fn process_is_alive(pid: u32) -> bool {
    use std::process::Command;
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                out.contains(&pid.to_string()) && !out.contains("No tasks")
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, Command::new("true"));
        false
    }
}

/// Where a drain snapshot comes from. The CLI implements it over HTTP
/// (`/api/drain/status`); the poller can implement it over its own records.
pub trait DrainSource {
    /// `Ok(None)` means the source is gone (daemon already exited): nothing
    /// is left to wait for.
    fn drain_status(&self) -> Result<Option<DrainSnapshot>, String>;
}

#[derive(Debug, Clone)]
pub struct DrainWaitOptions {
    pub timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for DrainWaitOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            poll_interval: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainTimeout {
    pub waited_secs: u64,
    pub blockers: Vec<String>,
}

impl std::fmt::Display for DrainTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Drain timed out after {}s, still waiting on: {}.\n\
             Use `ta daemon restart --force` to interrupt active work.",
            self.waited_secs,
            self.blockers.join(", ")
        )
    }
}

impl std::error::Error for DrainTimeout {}

/// Poll `source` until idle, the source disappears, or the timeout passes.
/// `progress` is called on every busy poll with the snapshot and the seconds
/// left before the timeout. A failing source is treated as "nothing to wait
/// for", matching the CLI's existing behaviour for an unreachable daemon.
pub fn wait_for_idle(
    source: &dyn DrainSource,
    clock: &dyn Clock,
    opts: &DrainWaitOptions,
    progress: &mut dyn FnMut(&DrainSnapshot, u64),
) -> Result<(), DrainTimeout> {
    let start = clock.now_unix();
    let deadline = start + opts.timeout.as_secs();
    loop {
        let snap = match source.drain_status() {
            Ok(Some(s)) => s,
            Ok(None) | Err(_) => return Ok(()),
        };
        if snap.is_idle() {
            return Ok(());
        }
        let now = clock.now_unix();
        if now >= deadline {
            return Err(DrainTimeout {
                waited_secs: opts.timeout.as_secs(),
                blockers: snap.blockers(),
            });
        }
        progress(&snap, deadline - now);
        clock.sleep(opts.poll_interval);
    }
}

/// Drain-aware restart: wait until idle, then run `restart` (stop + start).
/// Never interrupts running work; on timeout the restart is not attempted.
pub fn drain_then_restart<T, E>(
    source: &dyn DrainSource,
    clock: &dyn Clock,
    opts: &DrainWaitOptions,
    progress: &mut dyn FnMut(&DrainSnapshot, u64),
    restart: impl FnOnce() -> Result<T, E>,
) -> Result<T, RestartError<E>> {
    wait_for_idle(source, clock, opts, progress).map_err(RestartError::Drain)?;
    restart().map_err(RestartError::Restart)
}

#[derive(Debug)]
pub enum RestartError<E> {
    Drain(DrainTimeout),
    Restart(E),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use std::cell::RefCell;

    struct Script(RefCell<Vec<Option<DrainSnapshot>>>);
    impl DrainSource for Script {
        fn drain_status(&self) -> Result<Option<DrainSnapshot>, String> {
            let mut q = self.0.borrow_mut();
            Ok(if q.len() > 1 {
                q.remove(0)
            } else {
                q[0].clone()
            })
        }
    }

    fn busy(goals: u64) -> DrainSnapshot {
        DrainSnapshot {
            status: "draining".into(),
            active_goals: goals,
            ..Default::default()
        }
    }

    #[test]
    fn older_daemon_payload_parses_and_reports_blockers() {
        let s = DrainSnapshot::from_json(&serde_json::json!(
            {"status":"draining","active_goals":2,"active_sessions":0}
        ))
        .unwrap();
        assert_eq!(s.blockers(), vec!["2 running goals".to_string()]);
        assert!(DrainSnapshot::clean().is_idle());
    }

    #[test]
    fn every_blocker_kind_is_reported() {
        let s = DrainSnapshot {
            status: "draining".into(),
            active_goals: 1,
            active_sessions: 1,
            inflight_launches: 2,
            applying: true,
        };
        let b = s.blockers().join("; ");
        assert!(b.contains("1 running goal"), "{b}");
        assert!(b.contains("1 active agent session"), "{b}");
        assert!(b.contains("2 in-flight wake launches"), "{b}");
        assert!(b.contains("apply.lock"), "{b}");
    }

    #[test]
    fn inflight_guard_counts_and_releases_even_on_panic() {
        let c = InflightCounter::new();
        {
            let _a = c.begin();
            let _b = c.begin();
            assert_eq!(c.count(), 2);
            assert!(!DrainSnapshot::from_inflight(c.count()).is_idle());
        }
        assert_eq!(c.count(), 0);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = c.begin();
            panic!("boom");
        }));
        assert!(r.is_err());
        assert_eq!(c.count(), 0);
    }

    #[test]
    fn apply_lock_states() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let lock = dir.path().join(".ta").join("apply.lock");
        assert_eq!(
            apply_lock_state(dir.path(), &|_| true),
            ApplyLockState::Absent
        );
        std::fs::write(&lock, r#"{"pid":4242,"draft_id":"d1"}"#).unwrap();
        let live = apply_lock_state(dir.path(), &|p| p == 4242);
        assert_eq!(
            live,
            ApplyLockState::Live {
                pid: 4242,
                draft_id: "d1".into()
            }
        );
        assert!(live.is_blocking());
        let stale = apply_lock_state(dir.path(), &|_| false);
        assert_eq!(stale, ApplyLockState::Stale { pid: 4242 });
        assert!(!stale.is_blocking());
        std::fs::write(&lock, "not json").unwrap();
        assert!(apply_lock_state(dir.path(), &|_| false).is_blocking());
    }

    #[test]
    fn wait_for_idle_returns_when_work_finishes_without_real_sleep() {
        let src = Script(RefCell::new(vec![
            Some(busy(2)),
            Some(busy(1)),
            Some(DrainSnapshot::clean()),
        ]));
        let clock = ManualClock::new(1000);
        let mut seen = Vec::new();
        wait_for_idle(
            &src,
            &clock,
            &DrainWaitOptions::default(),
            &mut |s, left| seen.push((s.active_goals, left)),
        )
        .unwrap();
        assert_eq!(seen.len(), 2);
        assert!(clock.now_unix() >= 1004);
    }

    #[test]
    fn wait_for_idle_times_out_naming_what_is_left() {
        let src = Script(RefCell::new(vec![Some(busy(3))]));
        let clock = ManualClock::new(0);
        let err =
            wait_for_idle(&src, &clock, &DrainWaitOptions::default(), &mut |_, _| {}).unwrap_err();
        assert_eq!(err.blockers, vec!["3 running goals".to_string()]);
        assert!(err.to_string().contains("--force"));
    }

    #[test]
    fn vanished_source_is_not_a_blocker_and_timeout_skips_the_restart() {
        let gone = Script(RefCell::new(vec![None]));
        let clock = ManualClock::new(0);
        let r: Result<u32, RestartError<String>> = drain_then_restart(
            &gone,
            &clock,
            &DrainWaitOptions::default(),
            &mut |_, _| {},
            || Ok(7),
        );
        assert_eq!(r.unwrap(), 7);

        let stuck = Script(RefCell::new(vec![Some(busy(1))]));
        let mut ran = false;
        let r: Result<(), RestartError<String>> = drain_then_restart(
            &stuck,
            &clock,
            &DrainWaitOptions::default(),
            &mut |_, _| {},
            || {
                ran = true;
                Ok(())
            },
        );
        assert!(matches!(r, Err(RestartError::Drain(_))));
        assert!(!ran, "restart must not run while work is still active");
    }
}
