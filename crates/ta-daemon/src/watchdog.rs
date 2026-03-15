// watchdog.rs — Daemon watchdog loop for goal process liveness (v0.11.2.4).
//
// Spawned as a background tokio task at daemon startup. Each cycle:
//   1. Checks goal process liveness for all `running` goals
//   2. Detects stale questions (awaiting_input too long)
//   3. Emits health.check events when issues are found
//
// Lightweight: no disk I/O unless issues are detected.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use ta_events::schema::{HealthIssue, SessionEvent};
use ta_events::store::{EventStore, FsEventStore};
use ta_events::EventEnvelope;
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use uuid::Uuid;

/// Watchdog configuration extracted from daemon.toml `[operations]`.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How often the watchdog runs (seconds). 0 = disabled.
    pub interval_secs: u32,
    /// Seconds to wait before transitioning a dead-process goal.
    pub zombie_transition_delay_secs: u64,
    /// Seconds before a pending question is flagged as stale.
    pub stale_question_threshold_secs: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            interval_secs: 30,
            zombie_transition_delay_secs: 60,
            stale_question_threshold_secs: 3600,
        }
    }
}

impl WatchdogConfig {
    /// Build from daemon OperationsConfig.
    pub fn from_operations(ops: &crate::config::OperationsConfig) -> Self {
        Self {
            interval_secs: ops.watchdog_interval_secs,
            zombie_transition_delay_secs: ops.zombie_transition_delay_secs,
            stale_question_threshold_secs: ops.stale_question_threshold_secs,
        }
    }
}

/// Start the watchdog loop as a background task.
///
/// The task runs until `shutdown` is notified.
pub async fn run_watchdog(
    project_root: std::path::PathBuf,
    config: WatchdogConfig,
    shutdown: std::sync::Arc<tokio::sync::Notify>,
) {
    if config.interval_secs == 0 {
        tracing::info!("Watchdog disabled (watchdog_interval_secs = 0)");
        return;
    }

    let interval = Duration::from_secs(config.interval_secs as u64);
    tracing::info!(
        interval_secs = config.interval_secs,
        zombie_delay = config.zombie_transition_delay_secs,
        stale_threshold = config.stale_question_threshold_secs,
        "Watchdog started"
    );

    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                watchdog_cycle(&project_root, &config);
            }
            _ = shutdown.notified() => {
                tracing::info!("Watchdog shutting down");
                return;
            }
        }
    }
}

/// One watchdog check cycle. Synchronous — runs quickly and infrequently.
fn watchdog_cycle(project_root: &Path, config: &WatchdogConfig) {
    let goals_dir = project_root.join(".ta").join("goals");
    let events_dir = project_root.join(".ta").join("events");

    let store = match GoalRunStore::new(&goals_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Watchdog: cannot open goal store: {}", e);
            return;
        }
    };

    let goals = match store.list() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!("Watchdog: cannot list goals: {}", e);
            return;
        }
    };

    let now = Utc::now();
    let mut issues: Vec<HealthIssue> = Vec::new();
    let mut goals_checked: usize = 0;
    let event_store = FsEventStore::new(&events_dir);

    for goal in &goals {
        match &goal.state {
            GoalRunState::Running => {
                goals_checked += 1;
                check_running_goal(goal, config, &store, &event_store, &now, &mut issues);
            }
            GoalRunState::AwaitingInput {
                interaction_id,
                question_preview,
            } => {
                goals_checked += 1;
                check_stale_question(
                    goal,
                    config,
                    *interaction_id,
                    question_preview,
                    &event_store,
                    &now,
                    &mut issues,
                );
            }
            // Terminal/inactive states — skip.
            _ => {}
        }
    }

    // Emit health.check event only if issues were found (avoid log noise).
    if !issues.is_empty() {
        tracing::warn!(
            goals_checked = goals_checked,
            issue_count = issues.len(),
            "Watchdog found issues"
        );
        let event = SessionEvent::HealthCheck {
            goals_checked,
            issues,
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Watchdog: failed to persist health.check event: {}", e);
        }
    } else {
        tracing::debug!(goals_checked = goals_checked, "Watchdog cycle: all healthy");
    }
}

/// Check a running goal's agent process liveness.
fn check_running_goal(
    goal: &GoalRun,
    config: &WatchdogConfig,
    store: &GoalRunStore,
    event_store: &FsEventStore,
    now: &chrono::DateTime<Utc>,
    issues: &mut Vec<HealthIssue>,
) {
    let pid = match goal.agent_pid {
        Some(pid) => pid,
        None => {
            // Legacy goal or spawn failure — no PID to check.
            // Only flag if it's been running long enough to be suspicious.
            let age_secs = (*now - goal.updated_at).num_seconds().unsigned_abs();
            if age_secs > config.zombie_transition_delay_secs {
                issues.push(HealthIssue {
                    kind: "unknown_pid".to_string(),
                    goal_id: Some(goal.goal_run_id),
                    detail: format!(
                        "Goal '{}' is running but has no agent PID (age: {}s)",
                        goal.display_tag(),
                        age_secs
                    ),
                });
            }
            return;
        }
    };

    if is_process_alive(pid) {
        return; // All good.
    }

    // Process is dead. Check if it's been dead long enough to transition.
    let age_secs = (*now - goal.updated_at).num_seconds().unsigned_abs();
    if age_secs < config.zombie_transition_delay_secs {
        // Too soon — might be a brief restart. Wait for next cycle.
        tracing::debug!(
            goal_id = %goal.goal_run_id,
            pid = pid,
            age_secs = age_secs,
            "Process dead but within delay window"
        );
        return;
    }

    let detail = format!(
        "Agent process (PID {}) for goal '{}' exited without updating state (last update: {}s ago)",
        pid,
        goal.display_tag(),
        age_secs,
    );

    tracing::warn!(
        goal_id = %goal.goal_run_id,
        pid = pid,
        "Zombie goal detected: {}",
        detail
    );

    issues.push(HealthIssue {
        kind: "zombie_goal".to_string(),
        goal_id: Some(goal.goal_run_id),
        detail: detail.clone(),
    });

    // Emit goal.process_exited event.
    let elapsed = (*now - goal.created_at).num_seconds().unsigned_abs();
    let event = SessionEvent::GoalProcessExited {
        goal_id: goal.goal_run_id,
        pid,
        exit_code: None, // We can't recover exit code after the process is gone.
        elapsed_secs: elapsed,
        detail,
    };
    if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
        tracing::warn!("Watchdog: failed to persist GoalProcessExited event: {}", e);
    }

    // Transition the goal to Failed.
    let reason = format!(
        "Agent process (PID {}) exited without updating goal state. \
         Detected by daemon watchdog after {}s.",
        pid, age_secs
    );
    if let Err(e) = store.transition(goal.goal_run_id, GoalRunState::Failed { reason }) {
        tracing::warn!(
            goal_id = %goal.goal_run_id,
            "Watchdog: failed to transition zombie goal to failed: {}",
            e
        );
    } else {
        tracing::info!(
            goal_id = %goal.goal_run_id,
            "Watchdog: transitioned zombie goal to failed"
        );
    }
}

/// Check if a pending question is stale.
fn check_stale_question(
    goal: &GoalRun,
    config: &WatchdogConfig,
    interaction_id: Uuid,
    question_preview: &str,
    event_store: &FsEventStore,
    now: &chrono::DateTime<Utc>,
    issues: &mut Vec<HealthIssue>,
) {
    let pending_secs = (*now - goal.updated_at).num_seconds().unsigned_abs();

    if pending_secs < config.stale_question_threshold_secs {
        return;
    }

    let detail = format!(
        "Goal '{}' has a question pending for {}s: \"{}\"",
        goal.display_tag(),
        pending_secs,
        truncate_preview(question_preview, 60),
    );

    tracing::warn!(
        goal_id = %goal.goal_run_id,
        interaction_id = %interaction_id,
        pending_secs = pending_secs,
        "Stale question detected"
    );

    issues.push(HealthIssue {
        kind: "stale_question".to_string(),
        goal_id: Some(goal.goal_run_id),
        detail,
    });

    // Emit question.stale event.
    let event = SessionEvent::QuestionStale {
        goal_id: goal.goal_run_id,
        interaction_id,
        question_preview: truncate_preview(question_preview, 100),
        pending_secs,
    };
    if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
        tracing::warn!("Watchdog: failed to persist QuestionStale event: {}", e);
    }
}

/// Check whether a process with the given PID is alive.
///
/// Platform-specific: uses `kill(pid, 0)` on Unix, `tasklist` on Windows.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks process existence without sending a signal.
        // Returns 0 if the process exists and we have permission to send it signals.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
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

/// Truncate a string for display in events/logs.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Compute process health label for a goal (used by CLI and API).
pub fn process_health_label(goal: &GoalRun) -> &'static str {
    match &goal.state {
        GoalRunState::Running => match goal.agent_pid {
            Some(pid) => {
                if is_process_alive(pid) {
                    "alive"
                } else {
                    "dead"
                }
            }
            None => "unknown",
        },
        GoalRunState::AwaitingInput { .. } => match goal.agent_pid {
            Some(pid) => {
                if is_process_alive(pid) {
                    "alive"
                } else {
                    "dead"
                }
            }
            None => "unknown",
        },
        // Terminal states — no process to check.
        _ => "—",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn truncate_preview_short() {
        assert_eq!(truncate_preview("hello", 10), "hello");
    }

    #[test]
    fn truncate_preview_exact() {
        assert_eq!(truncate_preview("hello", 5), "hello");
    }

    #[test]
    fn truncate_preview_long() {
        let result = truncate_preview("hello world, this is a test", 15);
        assert!(result.len() <= 15);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn process_health_label_terminal_state() {
        let goal = GoalRun::new(
            "Test",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        // Default state is Created — terminal-ish, shows "—"
        assert_eq!(process_health_label(&goal), "—");
    }

    #[test]
    fn process_health_label_running_no_pid() {
        let mut goal = GoalRun::new(
            "Test",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        goal.state = GoalRunState::Running;
        goal.agent_pid = None;
        assert_eq!(process_health_label(&goal), "unknown");
    }

    #[test]
    fn process_health_label_running_with_current_pid() {
        let mut goal = GoalRun::new(
            "Test",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        goal.state = GoalRunState::Running;
        goal.agent_pid = Some(std::process::id()); // Our own PID is definitely alive
        assert_eq!(process_health_label(&goal), "alive");
    }

    #[test]
    fn process_health_label_running_with_dead_pid() {
        let mut goal = GoalRun::new(
            "Test",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        goal.state = GoalRunState::Running;
        goal.agent_pid = Some(99999999); // Very unlikely to be alive
        assert_eq!(process_health_label(&goal), "dead");
    }

    #[test]
    fn is_process_alive_current() {
        assert!(is_process_alive(std::process::id()));
    }

    #[test]
    fn is_process_alive_nonexistent() {
        assert!(!is_process_alive(99999999));
    }

    #[test]
    fn watchdog_config_default() {
        let cfg = WatchdogConfig::default();
        assert_eq!(cfg.interval_secs, 30);
        assert_eq!(cfg.zombie_transition_delay_secs, 60);
        assert_eq!(cfg.stale_question_threshold_secs, 3600);
    }

    #[test]
    fn watchdog_cycle_no_goals() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".ta/goals")).unwrap();
        std::fs::create_dir_all(project.join(".ta/events")).unwrap();

        let config = WatchdogConfig::default();
        // Should not panic with no goals.
        watchdog_cycle(project, &config);
    }

    #[test]
    fn watchdog_cycle_healthy_goal() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let goals_dir = project.join(".ta/goals");
        let events_dir = project.join(".ta/events");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::create_dir_all(&events_dir).unwrap();

        let store = GoalRunStore::new(&goals_dir).unwrap();
        let mut goal = GoalRun::new(
            "Healthy",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        goal.state = GoalRunState::Running;
        goal.agent_pid = Some(std::process::id()); // Alive
        store.save(&goal).unwrap();

        let config = WatchdogConfig::default();
        watchdog_cycle(project, &config);

        // No health.check events should be emitted for healthy goals.
        let event_store = FsEventStore::new(&events_dir);
        let events = event_store
            .query(&ta_events::store::EventQueryFilter::default())
            .unwrap_or_default();
        assert!(events.is_empty(), "No events expected for healthy goal");
    }

    #[test]
    fn watchdog_cycle_detects_zombie() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let goals_dir = project.join(".ta/goals");
        let events_dir = project.join(".ta/events");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::create_dir_all(&events_dir).unwrap();

        let store = GoalRunStore::new(&goals_dir).unwrap();
        let mut goal = GoalRun::new(
            "Zombie",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        goal.state = GoalRunState::Running;
        // Dead PID — set updated_at to long ago so zombie delay is exceeded.
        goal.agent_pid = Some(99999999);
        goal.updated_at = Utc::now() - chrono::Duration::seconds(120);
        store.save(&goal).unwrap();

        let config = WatchdogConfig {
            zombie_transition_delay_secs: 60,
            ..Default::default()
        };
        watchdog_cycle(project, &config);

        // Goal should now be in Failed state.
        let updated = store.get(goal.goal_run_id).unwrap().unwrap();
        assert!(
            matches!(updated.state, GoalRunState::Failed { .. }),
            "Expected Failed state, got: {:?}",
            updated.state
        );

        // Should have emitted events.
        let event_store = FsEventStore::new(&events_dir);
        let events = event_store
            .query(&ta_events::store::EventQueryFilter::default())
            .unwrap_or_default();
        assert!(
            events.len() >= 2,
            "Expected at least 2 events (GoalProcessExited + HealthCheck)"
        );
    }

    #[test]
    fn watchdog_cycle_zombie_within_delay_window() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let goals_dir = project.join(".ta/goals");
        let events_dir = project.join(".ta/events");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::create_dir_all(&events_dir).unwrap();

        let store = GoalRunStore::new(&goals_dir).unwrap();
        let mut goal = GoalRun::new(
            "RecentDeath",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        goal.state = GoalRunState::Running;
        // Dead PID but updated_at is very recent — within delay window.
        goal.agent_pid = Some(99999999);
        goal.updated_at = Utc::now();
        store.save(&goal).unwrap();

        let config = WatchdogConfig {
            zombie_transition_delay_secs: 120,
            ..Default::default()
        };
        watchdog_cycle(project, &config);

        // Goal should still be Running (within delay window).
        let updated = store.get(goal.goal_run_id).unwrap().unwrap();
        assert_eq!(updated.state, GoalRunState::Running);
    }

    #[test]
    fn watchdog_cycle_detects_stale_question() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let goals_dir = project.join(".ta/goals");
        let events_dir = project.join(".ta/events");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::create_dir_all(&events_dir).unwrap();

        let store = GoalRunStore::new(&goals_dir).unwrap();
        let iid = uuid::Uuid::new_v4();
        let mut goal = GoalRun::new(
            "Stale Q",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        goal.state = GoalRunState::AwaitingInput {
            interaction_id: iid,
            question_preview: "Which database?".into(),
        };
        // Set updated_at to 2 hours ago.
        goal.updated_at = Utc::now() - chrono::Duration::seconds(7200);
        store.save(&goal).unwrap();

        let config = WatchdogConfig {
            stale_question_threshold_secs: 3600,
            ..Default::default()
        };
        watchdog_cycle(project, &config);

        // Should have emitted stale question events.
        let event_store = FsEventStore::new(&events_dir);
        let events = event_store
            .query(&ta_events::store::EventQueryFilter::default())
            .unwrap_or_default();
        assert!(
            !events.is_empty(),
            "Expected at least 1 event for stale question"
        );
    }
}
