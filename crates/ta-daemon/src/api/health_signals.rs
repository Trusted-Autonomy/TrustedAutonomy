// api/health_signals.rs — `GET /health/signals` endpoint (v0.15.30.6).
//
// Computes and caches a set of operational health signals for the project.
// The signal set is refreshed at most every 60 seconds (cache TTL).
//
// Studio polls this endpoint every 30s to drive the ambient alert bar.
// The CLI (`ta status`, `ta run`, `ta draft view`) computes signals
// directly from the filesystem without hitting the daemon.
//
// Signal types (mirrors CLI health_signals.rs logic):
//   - disk_staging    — .ta/staging/ over size threshold
//   - disk_free       — available disk below threshold
//   - stale_pr_ready  — goals in pr_ready state >24h
//   - stale_drafts    — drafts approved/pending >3 days
//   - plugin_crash_loop — plugin restarting >10x in 10 min
//   - daemon_error_rate — error rate in daemon log too high
//   - orphan_staging  — orphaned staging dirs with no goal

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ta_goal::{GoalRunState, GoalRunStore};

use crate::api::AppState;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSeverity {
    Info,
    Warn,
    Crit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSignal {
    pub kind: String,
    pub severity: SignalSeverity,
    pub message: String,
    pub action: String,
}

impl HealthSignal {
    fn new(
        kind: impl Into<String>,
        severity: SignalSeverity,
        message: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            severity,
            message: message.into(),
            action: action.into(),
        }
    }
}

/// Response for `GET /health/signals`.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthSignalsResponse {
    pub signals: Vec<HealthSignal>,
    pub total: usize,
    pub has_crit: bool,
    pub has_warn: bool,
    pub computed_at: DateTime<Utc>,
}

// ── Cache ────────────────────────────────────────────────────────────────────

const CACHE_TTL: Duration = Duration::from_secs(60);

/// Shared signal cache so concurrent requests don't trigger duplicate scans.
#[derive(Default)]
pub struct SignalsCache {
    inner: Mutex<Option<CacheEntry>>,
}

struct CacheEntry {
    signals: Vec<HealthSignal>,
    computed_at: DateTime<Utc>,
    expires_at: Instant,
}

impl SignalsCache {
    pub fn get_or_compute(
        &self,
        project_root: &Path,
        goals_dir: &Path,
        pr_packages_dir: &Path,
    ) -> Vec<HealthSignal> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(ref entry) = *guard {
            if entry.expires_at > Instant::now() {
                return entry.signals.clone();
            }
        }
        let signals = compute_signals(project_root, goals_dir, pr_packages_dir);
        *guard = Some(CacheEntry {
            signals: signals.clone(),
            computed_at: Utc::now(),
            expires_at: Instant::now() + CACHE_TTL,
        });
        signals
    }

    pub fn last_computed_at(&self) -> Option<DateTime<Utc>> {
        self.inner.lock().unwrap().as_ref().map(|e| e.computed_at)
    }
}

// ── HTTP handler ─────────────────────────────────────────────────────────────

/// `GET /health/signals` — Operational health signals for Studio ambient bar.
///
/// Returns the cached signal set (refreshed every 60s). Studio polls this
/// endpoint every 30s to drive the ambient alert bar at the top of the UI.
///
/// # Response
///
/// ```json
/// {
///   "signals": [
///     { "kind": "stale_drafts", "severity": "warn", "message": "...", "action": "..." }
///   ],
///   "total": 1,
///   "has_crit": false,
///   "has_warn": true,
///   "computed_at": "2026-05-18T12:00:00Z"
/// }
/// ```
pub async fn health_signals(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let signals = state.signals_cache.get_or_compute(
        &state.project_root,
        &state.goals_dir,
        &state.pr_packages_dir,
    );

    let has_crit = signals.iter().any(|s| s.severity == SignalSeverity::Crit);
    let has_warn = signals.iter().any(|s| s.severity == SignalSeverity::Warn);
    let computed_at = state
        .signals_cache
        .last_computed_at()
        .unwrap_or_else(Utc::now);
    let total = signals.len();

    Json(HealthSignalsResponse {
        signals,
        total,
        has_crit,
        has_warn,
        computed_at,
    })
    .into_response()
}

// ── Signal computation ───────────────────────────────────────────────────────

fn compute_signals(
    project_root: &Path,
    goals_dir: &Path,
    pr_packages_dir: &Path,
) -> Vec<HealthSignal> {
    let mut signals = Vec::new();

    check_disk_pressure(project_root, &mut signals);
    check_stale_goals(goals_dir, &mut signals);
    check_stale_drafts(pr_packages_dir, &mut signals);
    check_plugin_crash_loops(project_root, &mut signals);
    check_daemon_log_error_rate(project_root, &mut signals);
    check_orphan_staging(project_root, goals_dir, &mut signals);
    check_long_running_goal_liveness(project_root, goals_dir, &mut signals);

    signals.sort_by_key(|s| std::cmp::Reverse(s.severity));
    signals
}

// ── Long-running goal liveness (v0.17.0.12.8) ────────────────────────────────
//
// The daemon's event log only emits `goal_started`/`agent_spawned` — no
// turn-level progress streams to Studio, so a healthy in-flight goal looks
// silent indefinitely once it's been running a while. Rather than building
// full turn-level SSE streaming, periodically check goal health (process
// alive + most recent staging file activity) for goals quiet past a
// threshold, and surface a reassuring "still working" signal instead of
// leaving Studio blank.

const QUIET_THRESHOLD_MINS: i64 = 10;

fn check_long_running_goal_liveness(
    project_root: &Path,
    goals_dir: &Path,
    signals: &mut Vec<HealthSignal>,
) {
    let _ = project_root;
    let store = match GoalRunStore::new(goals_dir) {
        Ok(s) => s,
        Err(_) => return,
    };
    let goals = match store.list() {
        Ok(g) => g,
        Err(_) => return,
    };

    let now = Utc::now();
    let quiet_threshold = chrono::Duration::minutes(QUIET_THRESHOLD_MINS);

    for goal in goals
        .iter()
        .filter(|g| matches!(g.state, GoalRunState::Running))
    {
        let quiet_for = now - goal.updated_at;
        if quiet_for < quiet_threshold {
            continue;
        }

        let process_alive = goal.agent_pid.map(is_pid_alive).unwrap_or(false);
        let activity_desc = match most_recent_mtime(&goal.workspace_path) {
            Some(mtime) => format!(
                "last file activity {} ago",
                format_duration_secs(now.signed_duration_since(mtime).num_seconds())
            ),
            None => "no file activity detected yet".to_string(),
        };

        if process_alive {
            signals.push(HealthSignal::new(
                "goal_quiet_but_alive",
                SignalSeverity::Info,
                format!(
                    "\"{}\" has been running {} with no new events — still working ({}, agent process alive)",
                    goal.title,
                    format_duration_secs(quiet_for.num_seconds()),
                    activity_desc
                ),
                "check `ta status` or Studio's Active tab for details".to_string(),
            ));
        } else {
            signals.push(HealthSignal::new(
                "goal_quiet_and_dead",
                SignalSeverity::Warn,
                format!(
                    "\"{}\" has been running {} with no new events and its agent process is not running — {}",
                    goal.title,
                    format_duration_secs(quiet_for.num_seconds()),
                    activity_desc
                ),
                "the agent may have crashed — check `ta daemon log` and consider re-running the goal".to_string(),
            ));
        }
    }
}

/// Return true if a process with `pid` is currently alive.
///
/// On Unix, probes with `kill(pid, 0)` via libc — no signal is sent; the call
/// only checks that the process exists and we have permission to signal it.
/// On Windows, uses `tasklist /FI "PID eq <pid>"`.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string()) && !stdout.contains("No tasks")
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Find the most recent modification time among files under `path` (recursive,
/// best-effort). Used as a proxy for "staging diff activity" when no
/// turn-level event stream is available. Returns `None` if `path` doesn't
/// exist or contains no files.
fn most_recent_mtime(path: &Path) -> Option<DateTime<Utc>> {
    let mut latest: Option<DateTime<Utc>> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    let ts = DateTime::<Utc>::from(modified);
                    if latest.map(|l| ts > l).unwrap_or(true) {
                        latest = Some(ts);
                    }
                }
            }
        }
    }
    latest
}

/// Format a duration in seconds as a short human string: "45s", "12m", "1h5m".
fn format_duration_secs(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn check_disk_pressure(project_root: &Path, signals: &mut Vec<HealthSignal>) {
    let staging_dir = project_root.join(".ta/staging");
    if staging_dir.exists() {
        let staging_bytes = walkdir_size(&staging_dir);
        let staging_gb = staging_bytes as f64 / 1_073_741_824.0;
        let threshold_gb = 20.0f64;
        if staging_gb >= threshold_gb {
            signals.push(HealthSignal::new(
                "disk_staging",
                SignalSeverity::Warn,
                format!(
                    "Disk pressure: .ta/staging/ is {:.1} GB (threshold: {:.0} GB)",
                    staging_gb, threshold_gb
                ),
                "run `ta doctor --fix` to reclaim space".to_string(),
            ));
        }
    }

    if let Some(mb) = free_disk_mb(project_root) {
        if mb < 2_048 {
            signals.push(HealthSignal::new(
                "disk_free",
                SignalSeverity::Crit,
                format!(
                    "Low disk space: only {} MB free — agent may fail mid-run",
                    mb
                ),
                "free up disk space before starting new goals".to_string(),
            ));
        } else if mb < 5_120 {
            signals.push(HealthSignal::new(
                "disk_free",
                SignalSeverity::Warn,
                format!("Low disk space: {} MB free — consider running `ta gc`", mb),
                "run `ta gc` or `ta doctor --fix` to free space".to_string(),
            ));
        }
    }
}

fn check_stale_goals(goals_dir: &Path, signals: &mut Vec<HealthSignal>) {
    let store = match GoalRunStore::new(goals_dir) {
        Ok(s) => s,
        Err(_) => return,
    };
    let goals = match store.list() {
        Ok(g) => g,
        Err(_) => return,
    };

    let now = Utc::now();
    let stale_hours = 24i64;

    let stale_pr_ready = goals
        .iter()
        .filter(|g| {
            matches!(g.state, GoalRunState::PrReady)
                && (now - g.updated_at).num_hours() >= stale_hours
        })
        .count();

    if stale_pr_ready > 0 {
        signals.push(HealthSignal::new(
            "stale_pr_ready",
            SignalSeverity::Warn,
            format!(
                "{} goal(s) in pr_ready state for {}h+ with no action",
                stale_pr_ready, stale_hours
            ),
            "run `ta draft list` to review pending drafts".to_string(),
        ));
    }
}

fn check_stale_drafts(pr_packages_dir: &Path, signals: &mut Vec<HealthSignal>) {
    use chrono::Duration;
    use ta_changeset::draft_package::DraftStatus;

    if !pr_packages_dir.exists() {
        return;
    }

    let stale_days = 3i64;
    let cutoff = Utc::now() - Duration::days(stale_days);

    let stale_count = std::fs::read_dir(pr_packages_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                .filter(|e| {
                    std::fs::read_to_string(e.path())
                        .ok()
                        .and_then(|c| {
                            serde_json::from_str::<ta_changeset::draft_package::DraftPackage>(&c)
                                .ok()
                        })
                        .map(|pkg| {
                            matches!(
                                pkg.status,
                                DraftStatus::Approved { .. } | DraftStatus::PendingReview
                            ) && pkg.created_at < cutoff
                        })
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);

    if stale_count > 0 {
        signals.push(HealthSignal::new(
            "stale_drafts",
            SignalSeverity::Warn,
            format!(
                "{} draft(s) approved/pending but not applied for {}+ days",
                stale_count, stale_days
            ),
            "run `ta draft list --stale` to review".to_string(),
        ));
    }
}

fn check_plugin_crash_loops(project_root: &Path, signals: &mut Vec<HealthSignal>) {
    let log_path = project_root.join(".ta/daemon.log");
    if !log_path.exists() {
        return;
    }
    let content = match std::fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let now = Utc::now();
    let window = chrono::Duration::minutes(10);
    let crash_patterns = [
        "restarting plugin",
        "plugin crashed",
        "restart attempt",
        "channel plugin",
        "listener restarting",
    ];

    let recent_crashes = content
        .lines()
        .rev()
        .take(5000)
        .filter(|line| {
            let is_recent = parse_log_timestamp(line)
                .map(|ts| (now - ts).abs() < window)
                .unwrap_or(false);
            is_recent && {
                let lower = line.to_lowercase();
                crash_patterns.iter().any(|p| lower.contains(p))
            }
        })
        .count();

    if recent_crashes > 10 {
        signals.push(HealthSignal::new(
            "plugin_crash_loop",
            SignalSeverity::Warn,
            format!(
                "Plugin crash loop: {}x restarts in last 10 min",
                recent_crashes
            ),
            "run `ta daemon log` for details; `ta daemon restart` if needed".to_string(),
        ));
    }
}

fn check_daemon_log_error_rate(project_root: &Path, signals: &mut Vec<HealthSignal>) {
    let log_path = project_root.join(".ta/daemon.log");
    if !log_path.exists() {
        return;
    }
    let content = match std::fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let now = Utc::now();
    let window = chrono::Duration::minutes(5);
    let mut error_count = 0usize;
    let mut total_recent = 0usize;

    for line in content.lines().rev().take(2000) {
        let is_recent = parse_log_timestamp(line)
            .map(|ts| (now - ts).abs() < window)
            .unwrap_or(false);
        if is_recent {
            total_recent += 1;
            let upper = line.to_uppercase();
            if upper.contains(" ERROR ") || upper.contains(" WARN ") {
                error_count += 1;
            }
        }
    }

    if total_recent > 0 && error_count >= 10 && error_count * 100 / total_recent > 20 {
        signals.push(HealthSignal::new(
            "daemon_error_rate",
            SignalSeverity::Warn,
            format!(
                "Daemon log: {} ERROR/WARN entries in last 5 min",
                error_count
            ),
            "run `ta daemon log` to see details".to_string(),
        ));
    }
}

fn check_orphan_staging(project_root: &Path, goals_dir: &Path, signals: &mut Vec<HealthSignal>) {
    let staging_dir = project_root.join(".ta/staging");
    if !staging_dir.exists() {
        return;
    }

    let store = match GoalRunStore::new(goals_dir) {
        Ok(s) => s,
        Err(_) => return,
    };
    let goals = match store.list() {
        Ok(g) => g,
        Err(_) => return,
    };

    let all_goal_paths: std::collections::HashSet<_> = goals
        .iter()
        .filter(|g| !g.workspace_path.as_os_str().is_empty())
        .map(|g| g.workspace_path.clone())
        .collect();

    let now = Utc::now();
    let mut orphan_count = 0usize;
    let mut orphan_bytes = 0u64;

    if let Ok(entries) = std::fs::read_dir(&staging_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if all_goal_paths
                .iter()
                .any(|p| p.starts_with(&path) || *p == path)
            {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    let age_secs = now
                        .signed_duration_since(DateTime::<Utc>::from(modified))
                        .num_seconds();
                    if age_secs > 86_400 {
                        orphan_count += 1;
                        orphan_bytes += walkdir_size(&path);
                    }
                }
            }
        }
    }

    if orphan_count > 0 {
        signals.push(HealthSignal::new(
            "orphan_staging",
            SignalSeverity::Info,
            format!(
                "{} orphaned staging dir(s) with no associated goal ({})",
                orphan_count,
                format_bytes(orphan_bytes)
            ),
            "run `ta doctor --fix` to remove orphaned staging directories".to_string(),
        ));
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns free disk space in MB for the filesystem containing `path`.
fn free_disk_mb(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return None;
        }
        Some((stat.f_bavail as u64) * (stat.f_frsize as u64) / (1024 * 1024))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn walkdir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += walkdir_size(&entry.path());
                }
            }
        }
    }
    total
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn parse_log_timestamp(line: &str) -> Option<DateTime<Utc>> {
    let prefix = line.get(..40).unwrap_or(line);
    for chunk in prefix.split_whitespace().take(3) {
        if let Ok(ts) = DateTime::parse_from_rfc3339(chunk) {
            return Some(ts.with_timezone(&Utc));
        }
        let chunk_z = format!("{}Z", chunk);
        if let Ok(ts) = DateTime::parse_from_rfc3339(&chunk_z) {
            return Some(ts.with_timezone(&Utc));
        }
        if let Ok(ts) = chrono::NaiveDateTime::parse_from_str(chunk, "%Y-%m-%dT%H:%M:%S%.f") {
            return Some(ts.and_utc());
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn signal_severity_ordering() {
        assert!(SignalSeverity::Crit > SignalSeverity::Warn);
        assert!(SignalSeverity::Warn > SignalSeverity::Info);
    }

    #[test]
    fn compute_signals_empty_dir() {
        let dir = tempdir().unwrap();
        let goals_dir = dir.path().join(".ta/goals");
        let pr_dir = dir.path().join(".ta/pr_packages");
        let signals = compute_signals(dir.path(), &goals_dir, &pr_dir);
        // Should not crash; at most disk_free signal.
        for s in &signals {
            assert_ne!(s.kind, "disk_staging");
            assert_ne!(s.kind, "stale_pr_ready");
        }
    }

    #[test]
    fn signals_cache_returns_cached() {
        let dir = tempdir().unwrap();
        let goals_dir = dir.path().join(".ta/goals");
        let pr_dir = dir.path().join(".ta/pr_packages");
        let cache = SignalsCache::default();
        let s1 = cache.get_or_compute(dir.path(), &goals_dir, &pr_dir);
        let s2 = cache.get_or_compute(dir.path(), &goals_dir, &pr_dir);
        assert_eq!(s1.len(), s2.len());
    }

    #[test]
    fn signals_cache_last_computed_at_none_initially() {
        let cache = SignalsCache::default();
        assert!(cache.last_computed_at().is_none());
    }

    #[test]
    fn format_bytes_gb() {
        assert_eq!(format_bytes(2 * 1_073_741_824), "2.0 GB");
    }

    fn make_goal(dir: &std::path::Path) -> ta_goal::GoalRun {
        ta_goal::GoalRun::new(
            "Long-running goal",
            "objective",
            "claude-code",
            dir.join("workspace"),
            dir.join("store"),
        )
    }

    #[test]
    fn format_duration_secs_under_minute() {
        assert_eq!(format_duration_secs(45), "45s");
    }

    #[test]
    fn format_duration_secs_minutes() {
        assert_eq!(format_duration_secs(125), "2m");
    }

    #[test]
    fn format_duration_secs_hours() {
        assert_eq!(format_duration_secs(3725), "1h2m");
    }

    #[test]
    fn long_running_goal_with_alive_process_signals_info() {
        let dir = tempdir().unwrap();
        let goals_dir = dir.path().join(".ta/goals");
        let store = ta_goal::GoalRunStore::new(&goals_dir).unwrap();

        let mut goal = make_goal(dir.path());
        goal.state = GoalRunState::Running;
        goal.updated_at = Utc::now() - chrono::Duration::minutes(15);
        goal.agent_pid = Some(std::process::id());
        std::fs::create_dir_all(&goal.workspace_path).unwrap();
        store.save(&goal).unwrap();

        let mut signals = Vec::new();
        check_long_running_goal_liveness(dir.path(), &goals_dir, &mut signals);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].kind, "goal_quiet_but_alive");
        assert_eq!(signals[0].severity, SignalSeverity::Info);
        assert!(signals[0].message.contains("Long-running goal"));
    }

    #[test]
    fn long_running_goal_with_dead_process_signals_warn() {
        let dir = tempdir().unwrap();
        let goals_dir = dir.path().join(".ta/goals");
        let store = ta_goal::GoalRunStore::new(&goals_dir).unwrap();

        let mut goal = make_goal(dir.path());
        goal.state = GoalRunState::Running;
        goal.updated_at = Utc::now() - chrono::Duration::minutes(15);
        // A PID astronomically unlikely to be alive in any test environment.
        goal.agent_pid = Some(2_147_483_647);
        std::fs::create_dir_all(&goal.workspace_path).unwrap();
        store.save(&goal).unwrap();

        let mut signals = Vec::new();
        check_long_running_goal_liveness(dir.path(), &goals_dir, &mut signals);

        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].kind, "goal_quiet_and_dead");
        assert_eq!(signals[0].severity, SignalSeverity::Warn);
    }

    #[test]
    fn recently_updated_goal_produces_no_signal() {
        let dir = tempdir().unwrap();
        let goals_dir = dir.path().join(".ta/goals");
        let store = ta_goal::GoalRunStore::new(&goals_dir).unwrap();

        let mut goal = make_goal(dir.path());
        goal.state = GoalRunState::Running;
        goal.updated_at = Utc::now(); // fresh — under the quiet threshold
        goal.agent_pid = Some(std::process::id());
        std::fs::create_dir_all(&goal.workspace_path).unwrap();
        store.save(&goal).unwrap();

        let mut signals = Vec::new();
        check_long_running_goal_liveness(dir.path(), &goals_dir, &mut signals);

        assert!(signals.is_empty());
    }

    #[test]
    fn non_running_goal_produces_no_signal() {
        let dir = tempdir().unwrap();
        let goals_dir = dir.path().join(".ta/goals");
        let store = ta_goal::GoalRunStore::new(&goals_dir).unwrap();

        let mut goal = make_goal(dir.path());
        goal.state = GoalRunState::PrReady;
        goal.updated_at = Utc::now() - chrono::Duration::minutes(30);
        std::fs::create_dir_all(&goal.workspace_path).unwrap();
        store.save(&goal).unwrap();

        let mut signals = Vec::new();
        check_long_running_goal_liveness(dir.path(), &goals_dir, &mut signals);

        assert!(signals.is_empty());
    }

    #[test]
    fn response_serializes() {
        let resp = HealthSignalsResponse {
            signals: vec![],
            total: 0,
            has_crit: false,
            has_warn: false,
            computed_at: Utc::now(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("signals"));
        assert!(json.contains("has_crit"));
    }
}
