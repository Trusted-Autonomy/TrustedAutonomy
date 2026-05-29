// health_signals.rs — Compute operational health signals for CLI and daemon (v0.15.30.6).
//
// Health signals are lightweight status indicators that surface actionable issues:
//   - Plugin crash loops (channel plugin restarting repeatedly)
//   - Disk pressure (staging or overall disk over threshold)
//   - Stale goals (failed/pr_ready state with no user action >24h)
//   - Stale staging dirs (no associated active goal >24h)
//   - Stale drafts (approved/pending but not applied for >3 days)
//   - Daemon log error rate (ERROR/WARN density above threshold)
//
// `ta status` shows a Health block when signals are present.
// `ta run` prints warn/crit signals as a pre-flight banner.
// `ta draft view` appends warn/crit signals as a footer.
// `GET /health/signals` returns the signal set as JSON (polled by Studio).

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use ta_goal::{GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSeverity {
    Info,
    Warn,
    Crit,
}

impl std::fmt::Display for SignalSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warn => write!(f, "warn"),
            Self::Crit => write!(f, "crit"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSignal {
    /// Stable signal type key for deduplication.
    pub kind: String,
    pub severity: SignalSeverity,
    /// One-line human-readable message.
    pub message: String,
    /// Suggested remediation action.
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

// ── Signal computation ───────────────────────────────────────────────────────

/// Compute the current set of health signals for the project.
///
/// Signals are sorted by severity descending (crit first). If everything is
/// clean, returns an empty vec — callers should suppress the Health block.
pub fn compute_health_signals(config: &GatewayConfig) -> Vec<HealthSignal> {
    let mut signals = Vec::new();

    check_disk_pressure(config, &mut signals);
    check_stale_goals(config, &mut signals);
    check_stale_staging_dirs(config, &mut signals);
    check_stale_drafts_signal(config, &mut signals);
    check_plugin_crash_loops(config, &mut signals);
    check_daemon_log_error_rate(config, &mut signals);

    // Sort crit → warn → info.
    signals.sort_by_key(|s| std::cmp::Reverse(s.severity));
    signals
}

// ── Individual signal checkers ───────────────────────────────────────────────

fn check_disk_pressure(config: &GatewayConfig, signals: &mut Vec<HealthSignal>) {
    // (a) Staging dir size vs configured threshold.
    let wf = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let workflow_max_gb = wf.staging.staging_max_gb;
    let staging_threshold_gb: f64 = if workflow_max_gb > 0.0 {
        workflow_max_gb
    } else {
        // Fall back to 20 GB (phase spec default).
        20.0
    };

    let staging_dir = config.workspace_root.join(".ta/staging");
    if staging_dir.exists() {
        let staging_bytes = walkdir_size(&staging_dir);
        let staging_gb = staging_bytes as f64 / 1_073_741_824.0;
        if staging_gb >= staging_threshold_gb {
            signals.push(HealthSignal::new(
                "disk_staging",
                SignalSeverity::Warn,
                format!(
                    "Disk pressure: .ta/staging/ is {:.1} GB (threshold: {:.0} GB)",
                    staging_gb, staging_threshold_gb
                ),
                "run `ta doctor --fix` to reclaim space".to_string(),
            ));
        }
    }

    // (b) Overall disk usage.
    if let Ok(mb) = ta_submit::check_disk_space_mb(&config.workspace_root) {
        // Get total disk space to compute percentage.
        let free_gb = mb as f64 / 1024.0;
        // We approximate 90% used as free < 10% of a typical 500 GB disk.
        // Better: use statvfs on unix. For now use a 5 GB free threshold as crit.
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
                format!(
                    "Low disk space: {} MB free — consider running `ta gc` to reclaim space",
                    mb
                ),
                "run `ta gc` or `ta doctor --fix` to free space".to_string(),
            ));
        } else if free_gb < 10.0 {
            // Check if we have a way to estimate total.
            // Keep as info only.
            signals.push(HealthSignal::new(
                "disk_free",
                SignalSeverity::Info,
                format!("Disk space: {:.1} GB free", free_gb),
                "run `ta gc` if you need more space".to_string(),
            ));
        }
    }
}

fn check_stale_goals(config: &GatewayConfig, signals: &mut Vec<HealthSignal>) {
    let store = match GoalRunStore::new(&config.goals_dir) {
        Ok(s) => s,
        Err(_) => return,
    };
    let goals = match store.list() {
        Ok(g) => g,
        Err(_) => return,
    };

    let now = Utc::now();
    let stale_hours = 24i64;

    let stale_failed: Vec<_> = goals
        .iter()
        .filter(|g| {
            matches!(g.state, GoalRunState::Failed { .. })
                && (now - g.updated_at).num_hours() >= stale_hours
        })
        .collect();

    let stale_pr_ready: Vec<_> = goals
        .iter()
        .filter(|g| {
            matches!(g.state, GoalRunState::PrReady)
                && (now - g.updated_at).num_hours() >= stale_hours
        })
        .collect();

    if !stale_failed.is_empty() {
        signals.push(HealthSignal::new(
            "stale_failed_goals",
            SignalSeverity::Info,
            format!(
                "{} failed goal(s) with staging older than {}h",
                stale_failed.len(),
                stale_hours
            ),
            "run `ta gc` to clean up or `ta run --follow-up <id>` to retry".to_string(),
        ));
    }

    if !stale_pr_ready.is_empty() {
        signals.push(HealthSignal::new(
            "stale_pr_ready",
            SignalSeverity::Warn,
            format!(
                "{} goal(s) have been in pr_ready state for {}h+ with no action",
                stale_pr_ready.len(),
                stale_hours
            ),
            "run `ta draft list` to review pending drafts".to_string(),
        ));
    }
}

fn check_stale_staging_dirs(config: &GatewayConfig, signals: &mut Vec<HealthSignal>) {
    let staging_dir = config.workspace_root.join(".ta/staging");
    if !staging_dir.exists() {
        return;
    }

    let store = match GoalRunStore::new(&config.goals_dir) {
        Ok(s) => s,
        Err(_) => return,
    };
    let goals = match store.list() {
        Ok(g) => g,
        Err(_) => return,
    };

    // Build set of active goal workspace paths.
    let active_paths: std::collections::HashSet<_> = goals
        .iter()
        .filter(|g| {
            matches!(
                g.state,
                GoalRunState::Running | GoalRunState::Configured | GoalRunState::PrReady
            )
        })
        .filter(|g| !g.workspace_path.as_os_str().is_empty())
        .map(|g| g.workspace_path.clone())
        .collect();

    // Build set of all goal workspace paths (any state).
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
            // If this dir is in an active goal path, skip.
            if active_paths
                .iter()
                .any(|p| p.starts_with(&path) || *p == path)
            {
                continue;
            }
            // If this dir is associated with any goal (even terminal), skip for orphan check.
            // Orphan = not referenced by any goal at all.
            if all_goal_paths
                .iter()
                .any(|p| p.starts_with(&path) || *p == path)
            {
                continue;
            }

            // Check age via mtime.
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    let age_secs = now
                        .signed_duration_since(chrono::DateTime::<Utc>::from(modified))
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

fn check_stale_drafts_signal(config: &GatewayConfig, signals: &mut Vec<HealthSignal>) {
    use chrono::Duration;
    use ta_changeset::draft_package::DraftStatus;

    if !config.pr_packages_dir.exists() {
        return;
    }

    let stale_days = 3i64;
    let now = Utc::now();
    let cutoff = now - Duration::days(stale_days);

    let stale_count = match std::fs::read_dir(&config.pr_packages_dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .filter(|e| {
                std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|c| {
                        serde_json::from_str::<ta_changeset::draft_package::DraftPackage>(&c).ok()
                    })
                    .map(|pkg| {
                        matches!(
                            pkg.status,
                            DraftStatus::Approved { .. } | DraftStatus::PendingReview
                        ) && pkg.created_at < cutoff
                    })
                    .unwrap_or(false)
            })
            .count(),
        Err(_) => return,
    };

    if stale_count > 0 {
        signals.push(HealthSignal::new(
            "stale_drafts",
            SignalSeverity::Warn,
            format!(
                "{} draft(s) approved/pending but not applied for {}+ days",
                stale_count, stale_days
            ),
            "run `ta draft list --stale` and apply or close them".to_string(),
        ));
    }
}

fn check_plugin_crash_loops(config: &GatewayConfig, signals: &mut Vec<HealthSignal>) {
    // Prefer the structured crash-state file written by channel_listener_manager (v0.16.1.8).
    // It contains the last stderr output and is much more actionable than log scanning.
    let crash_state_path = config.workspace_root.join(".ta/discord-crash-state.json");
    if crash_state_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&crash_state_path) {
            if let Ok(state) = serde_json::from_str::<serde_json::Value>(&content) {
                let consecutive_failures = state["consecutive_failures"].as_u64().unwrap_or(0);
                if consecutive_failures > 0 {
                    let plugin = state["plugin"].as_str().unwrap_or("plugin");
                    let last_stderr: Vec<String> = state["last_stderr"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    // Quick pattern diagnosis for the action hint.
                    let combined = last_stderr.join("\n").to_lowercase();
                    let hint =
                        if combined.contains("already running") || combined.contains("stale pid") {
                            "stale PID file blocking restarts — run `ta doctor --fix` to clear it"
                        } else if combined.contains("not set")
                            || combined.contains("environment variable")
                        {
                            "missing env var — check TA_DISCORD_TOKEN; run `ta doctor` for details"
                        } else {
                            "run `ta doctor` for diagnosis; `ta doctor --fix` to auto-resolve"
                        };

                    signals.push(HealthSignal::new(
                        "plugin_crash_loop",
                        SignalSeverity::Warn,
                        format!(
                            "Plugin crash loop: {} crashed {}x consecutively",
                            plugin, consecutive_failures
                        ),
                        hint.to_string(),
                    ));
                    return; // crash state is authoritative — skip log scanning
                }
            }
        }
    }

    // Fall back to daemon.log scanning when no crash-state file is present.
    let log_path = config.workspace_root.join(".ta/daemon.log");
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

    let mut recent_crashes = 0usize;

    for line in content.lines().rev().take(5000) {
        let is_recent = parse_log_timestamp(line)
            .map(|ts| (now - ts).abs() < window)
            .unwrap_or(false);

        if is_recent {
            let lower = line.to_lowercase();
            if crash_patterns.iter().any(|p| lower.contains(p)) {
                recent_crashes += 1;
            }
        }
    }

    if recent_crashes > 10 {
        signals.push(HealthSignal::new(
            "plugin_crash_loop",
            SignalSeverity::Warn,
            format!(
                "Plugin crash loop detected: {}x restarts in last 10 min",
                recent_crashes
            ),
            "run `ta doctor` for diagnosis; `ta daemon restart` if needed".to_string(),
        ));
    }
}

fn check_daemon_log_error_rate(config: &GatewayConfig, signals: &mut Vec<HealthSignal>) {
    let log_path = config.workspace_root.join(".ta/daemon.log");
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
    let mut top_error = String::new();

    for line in content.lines().rev().take(2000) {
        let is_recent = parse_log_timestamp(line)
            .map(|ts| (now - ts).abs() < window)
            .unwrap_or(false);

        if is_recent {
            total_recent += 1;
            let upper = line.to_uppercase();
            if upper.contains(" ERROR ") || upper.contains(" WARN ") {
                error_count += 1;
                if top_error.is_empty() {
                    top_error = line.chars().take(120).collect();
                }
            }
        }
    }

    // Signal if error rate > 20% of recent lines and at least 10 errors.
    if total_recent > 0 && error_count >= 10 && error_count * 100 / total_recent > 20 {
        signals.push(HealthSignal::new(
            "daemon_error_rate",
            SignalSeverity::Warn,
            format!(
                "Daemon log: {} ERROR/WARN entries in last 5 min ({}% of log)",
                error_count,
                error_count * 100 / total_recent
            ),
            "run `ta daemon log` to see details; consider `ta daemon restart`".to_string(),
        ));
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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

pub fn format_bytes(bytes: u64) -> String {
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

/// Try to parse a timestamp from a log line.
/// Handles formats like: "2026-05-18T12:34:56Z", "2026-05-18 12:34:56",
/// and tracing's default format "2026-05-18T12:34:56.123456Z".
fn parse_log_timestamp(line: &str) -> Option<chrono::DateTime<Utc>> {
    // Look for ISO-8601 style prefix in the first 40 chars.
    let prefix = line.get(..40).unwrap_or(line);
    // Try full RFC3339.
    for chunk in prefix.split_whitespace().take(3) {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(chunk) {
            return Some(ts.with_timezone(&Utc));
        }
        // Try without timezone suffix.
        let chunk_z = format!("{}Z", chunk);
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&chunk_z) {
            return Some(ts.with_timezone(&Utc));
        }
        // Try NaiveDateTime + Z.
        if let Ok(ts) = chrono::NaiveDateTime::parse_from_str(chunk, "%Y-%m-%dT%H:%M:%S%.f") {
            return Some(ts.and_utc());
        }
    }
    None
}

// ── Print helpers (used by CLI commands) ─────────────────────────────────────

/// Print warn/crit signals as a pre-flight banner.
/// Returns true if any crit signals were present.
pub fn print_preflight_banner(signals: &[HealthSignal]) -> bool {
    let relevant: Vec<_> = signals
        .iter()
        .filter(|s| s.severity >= SignalSeverity::Warn)
        .collect();

    if relevant.is_empty() {
        return false;
    }

    for s in &relevant {
        let label = match s.severity {
            SignalSeverity::Crit => "[crit]",
            SignalSeverity::Warn => "[warn]",
            SignalSeverity::Info => "[info]",
        };
        println!("{} {} — {}", label, s.message, s.action);
    }

    relevant.iter().any(|s| s.severity == SignalSeverity::Crit)
}

/// Print health signals as a status block.
/// Shows only non-empty signals. Returns false if nothing to show.
pub fn print_status_block(signals: &[HealthSignal]) -> bool {
    if signals.is_empty() {
        return false;
    }
    println!("│  Health:");
    for s in signals {
        let label = match s.severity {
            SignalSeverity::Crit => "[crit]",
            SignalSeverity::Warn => "[warn]",
            SignalSeverity::Info => "[info]",
        };
        println!("│    {} {}", label, s.message);
        println!("│    → {}", s.action);
    }
    true
}

/// Print warn/crit signals as a footer (for ta draft view).
pub fn print_footer(signals: &[HealthSignal]) {
    let relevant: Vec<_> = signals
        .iter()
        .filter(|s| s.severity >= SignalSeverity::Warn)
        .collect();

    if relevant.is_empty() {
        return;
    }

    println!();
    println!("─── System Health ───");
    for s in &relevant {
        let label = match s.severity {
            SignalSeverity::Crit => "[crit]",
            SignalSeverity::Warn => "[warn]",
            _ => "[info]",
        };
        println!("{} {} — {}", label, s.message, s.action);
    }
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
    fn health_signal_builder() {
        let s = HealthSignal::new("test", SignalSeverity::Warn, "msg", "fix");
        assert_eq!(s.kind, "test");
        assert_eq!(s.severity, SignalSeverity::Warn);
        assert_eq!(s.message, "msg");
        assert_eq!(s.action, "fix");
    }

    #[test]
    fn compute_empty_workspace() {
        let dir = tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // Should not panic on empty workspace.
        let signals = compute_health_signals(&config);
        // No staging dir, no goals → at most a disk free signal.
        for s in &signals {
            assert_ne!(s.kind, "disk_staging");
            assert_ne!(s.kind, "stale_pr_ready");
        }
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(2 * 1_048_576), "2.0 MB");
        assert_eq!(format_bytes(3 * 1_073_741_824), "3.0 GB");
    }

    #[test]
    fn print_preflight_returns_false_on_empty() {
        assert!(!print_preflight_banner(&[]));
    }

    #[test]
    fn print_preflight_returns_true_on_crit() {
        let s = HealthSignal::new("disk_free", SignalSeverity::Crit, "low disk", "free space");
        assert!(print_preflight_banner(&[s]));
    }

    #[test]
    fn print_status_block_returns_false_on_empty() {
        assert!(!print_status_block(&[]));
    }

    #[test]
    fn print_preflight_skips_info() {
        let info = HealthSignal::new("test", SignalSeverity::Info, "info msg", "info action");
        assert!(!print_preflight_banner(&[info]));
    }

    #[test]
    fn signals_sorted_crit_first() {
        let mut signals: Vec<HealthSignal> = [
            HealthSignal::new("a", SignalSeverity::Info, "a", "a"),
            HealthSignal::new("b", SignalSeverity::Crit, "b", "b"),
            HealthSignal::new("c", SignalSeverity::Warn, "c", "c"),
        ]
        .into();
        signals.sort_by_key(|s| std::cmp::Reverse(s.severity));
        assert_eq!(signals[0].severity, SignalSeverity::Crit);
        assert_eq!(signals[1].severity, SignalSeverity::Warn);
        assert_eq!(signals[2].severity, SignalSeverity::Info);
    }

    #[test]
    fn parse_log_timestamp_rfc3339() {
        let line = "2026-05-18T12:34:56Z INFO something happened";
        let ts = parse_log_timestamp(line);
        assert!(ts.is_some());
    }

    #[test]
    fn parse_log_timestamp_invalid() {
        let line = "no timestamp here just text";
        assert!(parse_log_timestamp(line).is_none());
    }
}
