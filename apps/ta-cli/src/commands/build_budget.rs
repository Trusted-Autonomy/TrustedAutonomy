// build_budget.rs — keeps a goal's build output from filling the disk.
//
// A goal agent that runs `cargo build` / `cargo test` over a large workspace can
// write tens of GB of build output within minutes (debug info and the
// incremental cache are most of it), and a full disk takes everything else on the
// machine down with it. Two guards:
//
// * Lean builds: goal builds run with `CARGO_PROFILE_DEV_DEBUG=0` and
//   `CARGO_INCREMENTAL=0` (`[staging] lean_builds`). `lean_env` computes the
//   variables; `build_cache` hands the same ones to the verification gates so
//   the agent's and the gates' builds share fingerprints.
// * A budget watched while the agent runs (`[staging] goal_build_max_gb`, and the
//   free-disk floor `[staging] min_disk_mb`). `BudgetMonitor` measures the goal's
//   build directory on a timer. On a breach it says so, naming the setting, and
//   with `goal_build_over_budget = "stop"` (the default) ends the agent's process
//   tree. The work done so far is kept and the draft is built from it as after
//   any agent exit.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ta_submit::config::{BuildBudgetAction, StagingConfig};
use ta_workspace::build_cache as cache;

/// How often a running agent's build directory is measured.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Time an agent gets to exit after SIGTERM before it is killed.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// The two variables that keep a goal build small.
pub const LEAN_VARS: [(&str, &str); 2] =
    [("CARGO_PROFILE_DEV_DEBUG", "0"), ("CARGO_INCREMENTAL", "0")];

/// The lean-build variables a goal should get: none when `[staging] lean_builds`
/// is off, and never one the project already set in `[agent_env]`.
pub fn lean_env(
    staging: &StagingConfig,
    already_set: &HashMap<String, String>,
) -> Vec<(String, String)> {
    if !staging.lean_builds {
        return Vec::new();
    }
    LEAN_VARS
        .iter()
        .filter(|(k, _)| !already_set.contains_key(*k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// What the monitor enforces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetSettings {
    /// Most build output the goal may add, in bytes. 0 = no budget.
    pub max_bytes: u64,
    /// Free-disk floor in MB. 0 = no floor.
    pub min_free_mb: u64,
    pub action: BuildBudgetAction,
}

impl BudgetSettings {
    pub fn from_config(staging: &StagingConfig) -> Self {
        let max_bytes = if staging.goal_build_max_gb > 0.0 {
            (staging.goal_build_max_gb * 1_073_741_824.0) as u64
        } else {
            0
        };
        Self {
            max_bytes,
            min_free_mb: staging.min_disk_mb,
            action: staging.goal_build_over_budget,
        }
    }

    fn active(&self) -> bool {
        self.max_bytes > 0 || self.min_free_mb > 0
    }
}

/// Which limit a goal went over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Breach {
    OverBudget { used: u64, budget: u64 },
    LowDisk { free_mb: u64, floor_mb: u64 },
}

impl Breach {
    fn kind(&self) -> u8 {
        match self {
            Breach::OverBudget { .. } => 0,
            Breach::LowDisk { .. } => 1,
        }
    }
}

/// The breach, if any. The goal's own budget is reported before the disk floor.
pub fn check(used: u64, free_mb: Option<u64>, s: &BudgetSettings) -> Option<Breach> {
    if s.max_bytes > 0 && used > s.max_bytes {
        return Some(Breach::OverBudget {
            used,
            budget: s.max_bytes,
        });
    }
    match free_mb {
        Some(free) if s.min_free_mb > 0 && free < s.min_free_mb => Some(Breach::LowDisk {
            free_mb: free,
            floor_mb: s.min_free_mb,
        }),
        _ => None,
    }
}

/// The message for a breach: what happened, what was being attempted, what to do.
pub fn describe(goal_id: &str, dir: &Path, breach: &Breach, action: BuildBudgetAction) -> String {
    let what = match breach {
        Breach::OverBudget { used, budget } => format!(
            "Goal {goal_id} has written {} of build output ({}; budget {}, `[staging] \
             goal_build_max_gb` in .ta/workflow.toml).",
            super::staging_reclaim::format_bytes(*used),
            dir.display(),
            super::staging_reclaim::format_bytes(*budget),
        ),
        Breach::LowDisk { free_mb, floor_mb } => format!(
            "Free disk space is down to {free_mb} MB, below the {floor_mb} MB floor (`[staging] \
             min_disk_mb` in .ta/workflow.toml), while goal {goal_id} builds in {}.",
            dir.display()
        ),
    };
    match action {
        BuildBudgetAction::Stop => format!(
            "{what} Stopping the agent so the disk does not fill up; its work so far is kept and \
             the draft is built from it. To let goals build more, raise the setting, or set \
             `goal_build_over_budget = \"warn\"` to only be warned. Free space with `ta cleanup \
             --only staging`."
        ),
        BuildBudgetAction::Warn => format!(
            "{what} The agent keeps running because `[staging] goal_build_over_budget = \"warn\"`. \
             Free space with `ta cleanup --only staging`."
        ),
    }
}

/// `(pid, ppid)` pairs from `ps -A -o pid=,ppid=` output; unparsable lines are skipped.
pub fn parse_ps_table(out: &str) -> Vec<(u32, u32)> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect()
}

/// `root` followed by every process descended from it, parents before children.
pub fn tree_pids(root: u32, table: &[(u32, u32)]) -> Vec<u32> {
    let mut out = vec![root];
    let mut seen: HashSet<u32> = HashSet::from([root]);
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        for (pid, ppid) in table {
            if *ppid == parent && seen.insert(*pid) {
                out.push(*pid);
            }
        }
        i += 1;
    }
    out
}

#[cfg(unix)]
fn process_table() -> Vec<(u32, u32)> {
    match std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()
    {
        Ok(o) if o.status.success() => parse_ps_table(&String::from_utf8_lossy(&o.stdout)),
        Ok(o) => {
            tracing::warn!(status = ?o.status.code(), "`ps` failed; only the agent's own process is signalled");
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not run `ps`; only the agent's own process is signalled");
            Vec::new()
        }
    }
}

/// SIGTERM the agent and everything it started, then SIGKILL what is still
/// running after the grace period, unless `exited` says the agent is gone (its
/// pid may have been reused by then).
#[cfg(unix)]
fn terminate_tree(pid: u32, exited: &AtomicBool, grace: Duration) {
    let signal = |pids: &[u32], sig: i32| {
        for p in pids {
            // SAFETY: plain kill(2) on a pid number; no memory is involved.
            unsafe {
                libc::kill(*p as libc::pid_t, sig);
            }
        }
    };
    let pids = tree_pids(pid, &process_table());
    signal(&pids, libc::SIGTERM);
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if exited.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if exited.load(Ordering::SeqCst) {
        return;
    }
    // Children started after the first snapshot are included in the second one.
    let mut again = tree_pids(pid, &process_table());
    again.extend(pids);
    signal(&again, libc::SIGKILL);
}

#[cfg(not(unix))]
fn terminate_tree(pid: u32, _exited: &AtomicBool, _grace: Duration) {
    tracing::warn!(
        pid,
        "stopping an over-budget agent is not implemented on this platform; it keeps running. \
         Set `[staging] goal_build_over_budget = \"warn\"` to silence this, and free space with \
         `ta cleanup --only staging`"
    );
}

/// Sleeps `total` in short slices; false as soon as `stop` is set.
fn sleep_unless_stopped(stop: &AtomicBool, total: Duration) -> bool {
    let slice = Duration::from_millis(100).min(total);
    let end = std::time::Instant::now() + total;
    while std::time::Instant::now() < end {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(slice.min(end.saturating_duration_since(std::time::Instant::now())));
    }
    !stop.load(Ordering::SeqCst)
}

/// Measure every `interval` until `stop` is set. `measure` returns the goal's
/// build output size and the free disk in MB. `on_breach` is called once per kind of
/// breach; with `BuildBudgetAction::Stop` the loop ends after the first one.
/// Returns the breach that ended it, if any.
pub(crate) fn monitor_loop(
    stop: &AtomicBool,
    interval: Duration,
    settings: &BudgetSettings,
    mut measure: impl FnMut() -> (u64, Option<u64>),
    mut on_breach: impl FnMut(&Breach),
) -> Option<Breach> {
    let mut reported: HashSet<u8> = HashSet::new();
    while sleep_unless_stopped(stop, interval) {
        let (used, free_mb) = measure();
        let Some(breach) = check(used, free_mb, settings) else {
            continue;
        };
        if reported.insert(breach.kind()) {
            on_breach(&breach);
        }
        if settings.action == BuildBudgetAction::Stop {
            return Some(breach);
        }
    }
    None
}

/// Watches one running agent. Dropping it (or `finish`) stops the watching.
pub struct BudgetMonitor {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    stopped_agent: Arc<Mutex<Option<String>>>,
}

impl BudgetMonitor {
    /// Start watching `build_dir` (growth over `baseline_bytes`) while the agent
    /// `pid` runs. `None` when neither limit is set, or the thread cannot start
    /// (which is reported).
    pub fn start(
        goal_id: String,
        pid: u32,
        build_dir: PathBuf,
        baseline_bytes: u64,
        settings: BudgetSettings,
        interval: Duration,
    ) -> Option<Self> {
        if !settings.active() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped_agent = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_slot = Arc::clone(&stopped_agent);
        let spawned = std::thread::Builder::new()
            .name("ta-build-budget".into())
            .spawn(move || {
                let measure_dir = build_dir.clone();
                let breach = monitor_loop(
                    &thread_stop,
                    interval,
                    &settings,
                    move || {
                        let used = cache::dir_size(&measure_dir).saturating_sub(baseline_bytes);
                        let free = nearest_existing(&measure_dir)
                            .and_then(|p| ta_submit::config::check_disk_space_mb(&p).ok());
                        (used, free)
                    },
                    |breach| {
                        let message = describe(&goal_id, &build_dir, breach, settings.action);
                        tracing::error!(
                            goal_id = %goal_id,
                            dir = %build_dir.display(),
                            breach = ?breach,
                            action = settings.action.as_str(),
                            "goal build is over its disk budget"
                        );
                        eprintln!("[error] {message}");
                    },
                );
                if let Some(breach) = breach {
                    if let Ok(mut slot) = thread_slot.lock() {
                        *slot = Some(describe(&goal_id, &build_dir, &breach, settings.action));
                    }
                    terminate_tree(pid, &thread_stop, KILL_GRACE);
                }
            });
        match spawned {
            Ok(thread) => Some(Self {
                stop,
                thread: Some(thread),
                stopped_agent,
            }),
            Err(e) => {
                tracing::warn!(error = %e, "could not start the build disk budget monitor; this goal's build output is not watched. Free space with `ta cleanup --only staging`");
                None
            }
        }
    }

    /// The agent has exited: stop watching. Returns the breach message when the
    /// monitor ended the agent.
    pub fn finish(mut self) -> Option<String> {
        self.shutdown();
        self.stopped_agent.lock().ok().and_then(|s| s.clone())
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for BudgetMonitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Disk one goal's staging directory holds, for `ta doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingUse {
    pub goal: String,
    pub total_bytes: u64,
    /// Of which the goal's own `target/` build output.
    pub target_bytes: u64,
}

/// Every goal staging directory under `<project>/.ta/staging`, largest first.
pub fn staging_use(project_root: &Path) -> Vec<StagingUse> {
    let Ok(rd) = std::fs::read_dir(project_root.join(".ta").join("staging")) else {
        return Vec::new();
    };
    let mut out: Vec<StagingUse> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| StagingUse {
            goal: e.file_name().to_string_lossy().into_owned(),
            total_bytes: cache::dir_size(&e.path()),
            target_bytes: cache::dir_size(&e.path().join("target")),
        })
        .collect();
    out.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes).then(a.goal.cmp(&b.goal)));
    out
}

/// `ta doctor` lines for the goals' staging directories against
/// `[staging] goal_build_max_gb`, and whether any goal's own `target/` is over it.
pub fn doctor_staging_lines(staging: &StagingConfig, used: &[StagingUse]) -> (Vec<String>, bool) {
    let budget = BudgetSettings::from_config(staging).max_bytes;
    let total: u64 = used.iter().map(|u| u.total_bytes).sum();
    let mut over = false;
    let mut lines = vec![format!(
        "{} goal staging dir(s), {} in all; per-goal build budget {}.",
        used.len(),
        super::staging_reclaim::format_bytes(total),
        if budget > 0 {
            super::staging_reclaim::format_bytes(budget)
        } else {
            "off (`goal_build_max_gb = 0`)".to_string()
        }
    )];
    for u in used.iter().take(10) {
        let flag = if budget > 0 && u.target_bytes > budget {
            over = true;
            "  OVER BUDGET: remove its build output with `ta cleanup --only staging` or `rm -r .ta/staging/<id>/target`"
        } else {
            ""
        };
        lines.push(format!(
            "  {}  {} ({} build output){}",
            &u.goal[..u.goal.len().min(8)],
            super::staging_reclaim::format_bytes(u.total_bytes),
            super::staging_reclaim::format_bytes(u.target_bytes),
            flag
        ));
    }
    if used.len() > 10 {
        lines.push(format!("  ... and {} more", used.len() - 10));
    }
    (lines, over)
}

/// `path`, or the closest ancestor that exists (a build directory may not exist yet).
fn nearest_existing(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|p| p.exists()).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn settings(max_bytes: u64, min_free_mb: u64, action: BuildBudgetAction) -> BudgetSettings {
        BudgetSettings {
            max_bytes,
            min_free_mb,
            action,
        }
    }

    #[test]
    fn lean_env_sets_both_variables_unless_off_or_already_set() {
        let on = StagingConfig::default();
        let env = lean_env(&on, &HashMap::new());
        assert_eq!(
            env,
            vec![
                ("CARGO_PROFILE_DEV_DEBUG".to_string(), "0".to_string()),
                ("CARGO_INCREMENTAL".to_string(), "0".to_string())
            ]
        );
        let mut set = HashMap::new();
        set.insert("CARGO_INCREMENTAL".to_string(), "1".to_string());
        let env = lean_env(&on, &set);
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "CARGO_PROFILE_DEV_DEBUG");
        let off = StagingConfig {
            lean_builds: false,
            ..StagingConfig::default()
        };
        assert!(lean_env(&off, &HashMap::new()).is_empty());
    }

    #[test]
    fn settings_come_from_the_staging_config() {
        let s = BudgetSettings::from_config(&StagingConfig::default());
        assert_eq!(s.max_bytes, 10 * 1_073_741_824);
        assert_eq!(s.min_free_mb, 2048);
        assert_eq!(s.action, BuildBudgetAction::Stop);
        let none = BudgetSettings::from_config(&StagingConfig {
            goal_build_max_gb: 0.0,
            min_disk_mb: 0,
            ..StagingConfig::default()
        });
        assert!(!none.active());
    }

    #[test]
    fn check_reports_the_budget_first_then_the_disk_floor() {
        let s = settings(1000, 500, BuildBudgetAction::Stop);
        assert_eq!(
            check(1000, Some(500), &s),
            None,
            "exactly at the limits is fine"
        );
        assert_eq!(
            check(1001, Some(10_000), &s),
            Some(Breach::OverBudget {
                used: 1001,
                budget: 1000
            })
        );
        assert_eq!(
            check(1, Some(499), &s),
            Some(Breach::LowDisk {
                free_mb: 499,
                floor_mb: 500
            })
        );
        assert!(matches!(
            check(5000, Some(1), &s),
            Some(Breach::OverBudget { .. })
        ));
        // An unknown free-disk figure never trips the floor; a 0 limit is off.
        assert_eq!(check(1, None, &s), None);
        assert_eq!(
            check(u64::MAX, Some(0), &settings(0, 0, BuildBudgetAction::Stop)),
            None
        );
    }

    #[test]
    fn messages_name_the_setting_the_directory_and_the_way_out() {
        let dir = Path::new("/p/.ta/build-cache/k/target");
        let over = Breach::OverBudget {
            used: 12 * 1_073_741_824,
            budget: 10 * 1_073_741_824,
        };
        let stop = describe("g1", dir, &over, BuildBudgetAction::Stop);
        for needle in [
            "g1",
            "12.0 GB",
            "10.0 GB",
            "goal_build_max_gb",
            "/p/.ta/build-cache/k/target",
            "Stopping the agent",
            "goal_build_over_budget",
            "ta cleanup",
        ] {
            assert!(stop.contains(needle), "{needle} missing from: {stop}");
        }
        let warn = describe("g1", dir, &over, BuildBudgetAction::Warn);
        assert!(warn.contains("keeps running") && !warn.contains("Stopping the agent"));
        let low = describe(
            "g1",
            dir,
            &Breach::LowDisk {
                free_mb: 100,
                floor_mb: 2048,
            },
            BuildBudgetAction::Stop,
        );
        assert!(
            low.contains("min_disk_mb") && low.contains("100 MB"),
            "{low}"
        );
    }

    #[test]
    fn ps_output_is_parsed_and_the_tree_is_walked_parents_first() {
        let table = parse_ps_table(
            "  1     0\n 10     1\n 11    10\n 12    11\n 20     1\nbad line\n  7\n",
        );
        assert_eq!(table.len(), 5);
        assert_eq!(tree_pids(10, &table), vec![10, 11, 12]);
        assert_eq!(tree_pids(99, &table), vec![99]);
        // A cycle cannot loop forever.
        assert_eq!(tree_pids(1, &[(1, 2), (2, 1)]), vec![1, 2]);
    }

    #[test]
    fn loop_stops_at_the_first_breach_with_the_stop_action() {
        let stop = AtomicBool::new(false);
        let sizes = [10u64, 20, 5000, 9000];
        let i = AtomicU32::new(0);
        let calls = Mutex::new(Vec::new());
        let ended = monitor_loop(
            &stop,
            Duration::from_millis(1),
            &settings(1000, 0, BuildBudgetAction::Stop),
            || {
                let n = i.fetch_add(1, Ordering::SeqCst) as usize;
                (sizes[n.min(3)], None)
            },
            |b| calls.lock().unwrap().push(*b),
        );
        assert_eq!(
            ended,
            Some(Breach::OverBudget {
                used: 5000,
                budget: 1000
            })
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(
            i.load(Ordering::SeqCst),
            3,
            "no measurement after the breach"
        );
    }

    #[test]
    fn loop_with_the_warn_action_reports_once_and_keeps_watching_until_stopped() {
        let stop = AtomicBool::new(false);
        let polls = AtomicU32::new(0);
        let reports = AtomicU32::new(0);
        let ended = monitor_loop(
            &stop,
            Duration::from_millis(1),
            &settings(100, 0, BuildBudgetAction::Warn),
            || {
                if polls.fetch_add(1, Ordering::SeqCst) >= 5 {
                    stop.store(true, Ordering::SeqCst);
                }
                (10_000, None)
            },
            |_| {
                reports.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(ended, None, "warn never ends the agent");
        assert_eq!(
            reports.load(Ordering::SeqCst),
            1,
            "one warning, not one per poll"
        );
        assert!(polls.load(Ordering::SeqCst) >= 5);
    }

    #[test]
    fn loop_returns_at_once_when_already_stopped() {
        let stop = AtomicBool::new(true);
        let ended = monitor_loop(
            &stop,
            Duration::from_secs(3600),
            &settings(1, 0, BuildBudgetAction::Stop),
            || panic!("must not measure after stop"),
            |_| panic!("must not report"),
        );
        assert_eq!(ended, None);
    }

    #[test]
    fn monitor_without_any_limit_does_not_start() {
        let m = BudgetMonitor::start(
            "g".into(),
            1,
            PathBuf::from("/nonexistent"),
            0,
            settings(0, 0, BuildBudgetAction::Stop),
            Duration::from_millis(1),
        );
        assert!(m.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn over_budget_agent_and_its_children_are_terminated() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        // A fake agent: a shell that starts a child, both sleeping.
        let mut agent = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 60 & sleep 60; wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let target = dir.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("big.rlib"), vec![0u8; 4096]).unwrap();

        let monitor = BudgetMonitor::start(
            "goal-test".into(),
            agent.id(),
            target,
            0,
            settings(1024, 0, BuildBudgetAction::Stop),
            Duration::from_millis(50),
        )
        .unwrap();
        let status = agent.wait().unwrap();
        assert!(
            !status.success(),
            "the agent must have been signalled: {status:?}"
        );
        let message = monitor.finish().expect("the monitor ended the agent");
        assert!(message.contains("goal_build_max_gb"), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn under_budget_agent_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut agent = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 1"])
            .spawn()
            .unwrap();
        let monitor = BudgetMonitor::start(
            "goal-test".into(),
            agent.id(),
            dir.path().to_path_buf(),
            0,
            settings(1_000_000, 0, BuildBudgetAction::Stop),
            Duration::from_millis(50),
        )
        .unwrap();
        assert!(agent.wait().unwrap().success());
        assert!(monitor.finish().is_none());
    }

    #[test]
    fn staging_report_lists_goals_largest_first_and_flags_the_over_budget_one() {
        let p = tempfile::tempdir().unwrap();
        let staging = p.path().join(".ta/staging");
        let small = staging.join("aaaaaaaa-small");
        std::fs::create_dir_all(&small).unwrap();
        std::fs::write(small.join("src.rs"), vec![0u8; 100]).unwrap();
        let big = staging.join("bbbbbbbb-big");
        std::fs::create_dir_all(big.join("target/debug")).unwrap();
        std::fs::write(big.join("target/debug/x.rlib"), vec![0u8; 3000]).unwrap();
        std::fs::write(big.join("src.rs"), vec![0u8; 10]).unwrap();

        let used = staging_use(p.path());
        assert_eq!(used.len(), 2);
        assert_eq!(used[0].goal, "bbbbbbbb-big");
        assert_eq!((used[0].total_bytes, used[0].target_bytes), (3010, 3000));
        assert_eq!(used[1].target_bytes, 0);

        // 1e-6 GB is about 1 KB: the big goal's target/ is over, the small one is not.
        let cfg = StagingConfig {
            goal_build_max_gb: 1e-6,
            ..StagingConfig::default()
        };
        let (lines, over) = doctor_staging_lines(&cfg, &used);
        let text = lines.join("\n");
        assert!(over, "{text}");
        assert!(
            text.contains("OVER BUDGET") && text.contains("bbbbbbbb"),
            "{text}"
        );
        assert_eq!(text.matches("OVER BUDGET").count(), 1, "{text}");

        let off = StagingConfig {
            goal_build_max_gb: 0.0,
            ..StagingConfig::default()
        };
        let (lines, over) = doctor_staging_lines(&off, &used);
        assert!(
            !over && lines[0].contains("goal_build_max_gb = 0"),
            "{lines:?}"
        );
        assert!(staging_use(&p.path().join("nope")).is_empty());
    }
}
