// watchdog.rs — Daemon watchdog loop for goal process liveness (v0.11.2.4).
//
// Spawned as a background tokio task at daemon startup. Each cycle:
//   1. Checks goal process liveness for all `running` goals
//   2. Detects stale questions (awaiting_input too long)
//   3. Emits health.check events when issues are found
//   4. Detects sleep/wake transitions and suppresses false heartbeat alerts (v0.13.1.1)
//   5. Checks API connectivity after wake and emits connection lost/restored events (v0.13.1.1)
//
// Lightweight: no disk I/O unless issues are detected.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use reqwest;

use crate::power_manager::SharedPowerManager;
use chrono::Utc;
use ta_audit::{AuditDisposition, AuditEntry, GoalAuditLedger};
use ta_events::schema::{HealthIssue, SessionEvent};
use ta_events::store::{EventStore, FsEventStore};
use ta_events::EventEnvelope;
use ta_goal::operations::{ActionSeverity, CorrectiveAction, OperationsLog};
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use uuid::Uuid;

/// Watchdog configuration extracted from daemon.toml `[operations]` and `[power]`.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How often the watchdog runs (seconds). 0 = disabled.
    pub interval_secs: u32,
    /// Seconds to wait before transitioning a dead-process goal.
    pub zombie_transition_delay_secs: u64,
    /// Seconds before a pending question is flagged as stale.
    pub stale_question_threshold_secs: u64,
    /// Seconds to extend heartbeat deadlines after a detected wake (v0.13.1.1).
    pub wake_grace_secs: u64,
    /// URL to ping for connectivity check after waking from sleep (v0.13.1.1).
    pub connectivity_check_url: String,
    /// Maximum seconds a goal may spend in `Finalizing` state before being
    /// declared stuck and transitioned to `Failed` (v0.13.14).
    ///
    /// Default: 300s (5 min) — enough for diff + draft creation on any workspace size.
    pub finalize_timeout_secs: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            interval_secs: 30,
            zombie_transition_delay_secs: 60,
            stale_question_threshold_secs: 3600,
            wake_grace_secs: 60,
            connectivity_check_url: "https://api.anthropic.com".to_string(),
            finalize_timeout_secs: 300,
        }
    }
}

impl WatchdogConfig {
    /// Build from daemon config (operations + power sections).
    pub fn from_config(
        ops: &crate::config::OperationsConfig,
        power: &crate::config::PowerConfig,
    ) -> Self {
        Self {
            interval_secs: ops.watchdog_interval_secs,
            zombie_transition_delay_secs: ops.zombie_transition_delay_secs,
            stale_question_threshold_secs: ops.stale_question_threshold_secs,
            wake_grace_secs: power.wake_grace_secs,
            connectivity_check_url: power.connectivity_check_url.clone(),
            finalize_timeout_secs: ops.finalize_timeout_secs,
        }
    }

    /// Build from daemon OperationsConfig only (uses power defaults).
    pub fn from_operations(ops: &crate::config::OperationsConfig) -> Self {
        Self {
            interval_secs: ops.watchdog_interval_secs,
            zombie_transition_delay_secs: ops.zombie_transition_delay_secs,
            stale_question_threshold_secs: ops.stale_question_threshold_secs,
            wake_grace_secs: 60,
            connectivity_check_url: "https://api.anthropic.com".to_string(),
            finalize_timeout_secs: ops.finalize_timeout_secs,
        }
    }
}

/// Inter-cycle state for the watchdog (sleep/wake detection, connectivity tracking).
///
/// Held in a `Mutex` and passed between `run_watchdog` async ticks and the
/// synchronous `watchdog_cycle` calls.
#[derive(Debug)]
pub struct WatchdogState {
    /// Monotonic clock reading at the end of the previous cycle.
    prev_monotonic: Instant,
    /// Wall-clock reading at the end of the previous cycle.
    prev_wall: SystemTime,
    /// Wall-clock time of the last detected wake event (`None` if never woken).
    last_wake_wall: Option<SystemTime>,
    /// Whether the API was reachable the last time we checked (post-wake only).
    api_was_reachable: Option<bool>,
}

impl Default for WatchdogState {
    fn default() -> Self {
        Self {
            prev_monotonic: Instant::now(),
            prev_wall: SystemTime::now(),
            last_wake_wall: None,
            api_was_reachable: None,
        }
    }
}

/// Start the watchdog loop as a background task.
///
/// The task runs until `shutdown` is notified.
pub async fn run_watchdog(
    project_root: std::path::PathBuf,
    config: WatchdogConfig,
    power_manager: Option<SharedPowerManager>,
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
        wake_grace_secs = config.wake_grace_secs,
        "Watchdog started"
    );

    let state = Arc::new(Mutex::new(WatchdogState::default()));

    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                let mut st = state.lock().unwrap();
                watchdog_cycle(&project_root, &config, &mut st, power_manager.as_deref());
            }
            _ = shutdown.notified() => {
                tracing::info!("Watchdog shutting down");
                return;
            }
        }
    }
}

/// Check available disk space on the project root filesystem.
/// Returns available bytes, or None if we can't determine it.
fn available_disk_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        let path_cstr = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
        let mut stats: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
        let ret = unsafe { libc::statvfs(path_cstr.as_ptr(), stats.as_mut_ptr()) };
        if ret == 0 {
            let stats = unsafe { stats.assume_init() };
            // f_bavail = blocks available to unprivileged user, f_frsize = block size
            // f_bavail is u32 on macOS but u64 on Linux; u64::from() widens
            // safely on macOS and is a no-op on Linux — allow the lint rather
            // than a cfg branch just for a widening conversion.
            #[allow(clippy::useless_conversion)]
            Some(u64::from(stats.f_bavail) * stats.f_frsize)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// One watchdog check cycle. Synchronous — runs quickly and infrequently.
fn watchdog_cycle(
    project_root: &Path,
    config: &WatchdogConfig,
    state: &mut WatchdogState,
    power_manager: Option<&crate::power_manager::PowerManager>,
) {
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

    // ── Sleep/wake detection (v0.13.1.1) ─────────────────────────────────────
    // Compare wall-clock elapsed vs monotonic elapsed since the last cycle.
    // A wall-clock delta significantly larger than the monotonic delta means
    // the system slept (monotonic clocks pause during sleep; wall clocks don't).
    let current_monotonic = Instant::now();
    let current_wall = SystemTime::now();
    let monotonic_elapsed_secs = current_monotonic
        .saturating_duration_since(state.prev_monotonic)
        .as_secs();
    let wall_elapsed_secs = current_wall
        .duration_since(state.prev_wall)
        .unwrap_or_default()
        .as_secs();

    // If wall-clock advanced by more than 1.5× the interval + monotonic delta,
    // a sleep occurred. The slack (interval + 30s) prevents false positives from
    // slow cycles or scheduling jitter.
    let sleep_slack_secs = config.interval_secs as u64 + 30;
    let detected_sleep = wall_elapsed_secs > monotonic_elapsed_secs + sleep_slack_secs
        && wall_elapsed_secs > sleep_slack_secs;

    if detected_sleep {
        let slept_for_secs = wall_elapsed_secs.saturating_sub(monotonic_elapsed_secs);
        tracing::info!(
            slept_for_secs = slept_for_secs,
            "Sleep/wake detected — extending heartbeat grace period"
        );
        state.last_wake_wall = Some(current_wall);

        // Emit SystemWoke event.
        let woke_event = SessionEvent::SystemWoke { slept_for_secs };
        if let Err(e) = event_store.append(&EventEnvelope::new(woke_event)) {
            tracing::warn!("Watchdog: failed to persist SystemWoke event: {}", e);
        }

        // Post-wake connectivity check.
        check_api_connectivity(config, state, &event_store);
    } else if state.last_wake_wall.is_some() && state.api_was_reachable == Some(false) {
        // We are within the grace window and last check showed API unreachable —
        // re-check to see if connectivity was restored.
        check_api_connectivity(config, state, &event_store);
    }

    // Update state for next cycle.
    state.prev_monotonic = current_monotonic;
    state.prev_wall = current_wall;

    // Determine if we are within the wake grace window (suppress heartbeat alerts).
    let in_wake_grace = if let Some(wake_wall) = state.last_wake_wall {
        current_wall
            .duration_since(wake_wall)
            .map(|d| d.as_secs() < config.wake_grace_secs)
            .unwrap_or(false)
    } else {
        false
    };

    // Check disk space and emit corrective action if low.
    let low_disk_threshold_bytes: u64 = 2 * 1024 * 1024 * 1024; // 2 GB
    if let Some(avail) = available_disk_bytes(project_root) {
        if avail < low_disk_threshold_bytes {
            let avail_gb = avail as f64 / 1_073_741_824.0;
            tracing::warn!(
                available_gb = format!("{:.2}", avail_gb),
                "Disk space low — emitting corrective action"
            );
            let action = CorrectiveAction::new(
                format!("Low disk space: {:.1} GB available", avail_gb),
                ActionSeverity::Critical,
                format!(
                    "Available disk on project root is {:.1} GB, below 2 GB threshold. \
                     Stale staging directories may be consuming significant space.",
                    avail_gb
                ),
                "Run `ta gc` to clean stale staging directories and reclaim disk space.",
                "clean_applied_staging",
            )
            .set_auto_healable();
            let ops_log = OperationsLog::for_project(project_root);
            if let Err(e) = ops_log.append(&action) {
                tracing::warn!(
                    "Watchdog: failed to write disk-space corrective action: {}",
                    e
                );
            }
        }
    }

    for goal in &goals {
        match &goal.state {
            GoalRunState::Running => {
                goals_checked += 1;
                check_running_goal(
                    goal,
                    config,
                    &store,
                    &event_store,
                    &now,
                    &mut issues,
                    in_wake_grace,
                    project_root,
                );
            }
            GoalRunState::Finalizing {
                finalize_started_at,
                run_pid,
                ..
            } => {
                // v0.13.14/v0.13.17: Skip Finalizing goals unless timeout exceeded
                // and the ta run process is no longer alive.
                goals_checked += 1;
                check_finalizing_goal(
                    goal,
                    *finalize_started_at,
                    *run_pid,
                    config,
                    &store,
                    &now,
                    &mut issues,
                    project_root,
                );
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
            // v0.14.7.2: DraftPending — agent wrote work_complete but build hasn't started.
            // Emit a health issue if the goal has been pending too long.
            GoalRunState::DraftPending { pending_since, .. } => {
                goals_checked += 1;
                let pending_secs = (now - *pending_since).num_seconds().unsigned_abs();
                // Give a generous grace period (5 min) before flagging.
                if pending_secs > 300 {
                    let detail = format!(
                        "Goal '{}' has been in draft_pending state for {}s. \
                         Run: ta draft build --goal {}",
                        goal.display_tag(),
                        pending_secs,
                        &goal.goal_run_id.to_string()[..8],
                    );
                    tracing::warn!(
                        goal_id = %goal.goal_run_id,
                        pending_secs = pending_secs,
                        "Draft pending too long — draft build may not have started"
                    );
                    issues.push(HealthIssue {
                        kind: "draft_pending_timeout".to_string(),
                        goal_id: Some(goal.goal_run_id),
                        detail,
                    });
                }
            }
            // Terminal/inactive states — skip.
            _ => {}
        }
    }

    // Update power assertion based on running/finalizing goal count (v0.13.1.1).
    if let Some(pm) = power_manager {
        let active_count = goals
            .iter()
            .filter(|g| {
                matches!(
                    g.state,
                    GoalRunState::Running
                        | GoalRunState::Finalizing { .. }
                        | GoalRunState::DraftPending { .. }
                )
            })
            .count();
        pm.update(active_count);
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

/// Perform an HTTP connectivity check to the configured URL.
///
/// Emits `ApiConnectionLost` or `ApiConnectionRestored` as the state
/// transitions. Uses a short timeout so the watchdog cycle isn't delayed.
fn check_api_connectivity(
    config: &WatchdogConfig,
    state: &mut WatchdogState,
    event_store: &FsEventStore,
) {
    let url = &config.connectivity_check_url;
    let reachable = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
        .and_then(|c| c.head(url.as_str()).send().ok())
        .is_some();

    let was_reachable = state.api_was_reachable;
    state.api_was_reachable = Some(reachable);

    match (was_reachable, reachable) {
        (None | Some(true), false) => {
            tracing::warn!(url = %url, "API connection lost after wake");
            let ev = SessionEvent::ApiConnectionLost {
                checked_url: url.clone(),
            };
            if let Err(e) = event_store.append(&EventEnvelope::new(ev)) {
                tracing::warn!("Watchdog: failed to persist ApiConnectionLost: {}", e);
            }
        }
        (Some(false), true) => {
            tracing::info!(url = %url, "API connection restored after wake");
            let ev = SessionEvent::ApiConnectionRestored {
                checked_url: url.clone(),
            };
            if let Err(e) = event_store.append(&EventEnvelope::new(ev)) {
                tracing::warn!("Watchdog: failed to persist ApiConnectionRestored: {}", e);
            }
        }
        _ => {} // No state change.
    }
}

/// Write a watchdog-crash audit entry to goal-audit.jsonl (v0.14.7.2).
///
/// Called whenever the watchdog transitions a goal to `Failed` (zombie or finalize
/// timeout). Records the goal ID, detected PID (or none), detection timestamp,
/// watchdog reason, and recovery command so the audit trail reflects process crashes.
fn write_watchdog_audit_entry(
    project_root: &Path,
    goal: &GoalRun,
    detected_pid: Option<u32>,
    watchdog_reason: &str,
) {
    let ledger_path = GoalAuditLedger::path_for(project_root);
    match GoalAuditLedger::open(&ledger_path) {
        Ok(mut ledger) => {
            let now = chrono::Utc::now();
            let total = now.signed_duration_since(goal.created_at).num_seconds();
            let pid_note = match detected_pid {
                Some(p) => format!("pid={}", p),
                None => "no PID".to_string(),
            };
            let recovery_cmd = format!("ta goal recover {}", &goal.goal_run_id.to_string()[..8]);
            let mut entry = AuditEntry {
                goal_id: goal.goal_run_id,
                title: goal.title.clone(),
                objective: None,
                disposition: AuditDisposition::Crashed,
                phase: goal.plan_phase.clone(),
                agent: goal.agent_id.clone(),
                created_at: goal.created_at,
                pr_ready_at: None,
                recorded_at: now,
                build_seconds: total,
                review_seconds: 0,
                total_seconds: total,
                draft_id: goal.pr_package_id,
                ai_summary: None,
                reviewer: None,
                denial_reason: None,
                cancel_reason: Some(format!(
                    "watchdog: {} ({}). Recovery: {}",
                    watchdog_reason, pid_note, recovery_cmd
                )),
                artifact_count: 0,
                lines_changed: 0,
                artifacts: Vec::new(),
                policy_result: None,
                parent_goal_id: goal.parent_goal_id,
                previous_hash: None,
            };
            if let Err(e) = ledger.append(&mut entry) {
                tracing::warn!(
                    goal_id = %goal.goal_run_id,
                    "Watchdog: failed to write crash audit entry: {}",
                    e
                );
            } else {
                tracing::info!(
                    goal_id = %goal.goal_run_id,
                    reason = watchdog_reason,
                    "Watchdog: wrote crash audit entry to goal-audit.jsonl"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                goal_id = %goal.goal_run_id,
                "Watchdog: cannot open goal audit ledger: {}",
                e
            );
        }
    }
}

///
/// `in_wake_grace` suppresses all liveness alerts immediately after a detected
/// sleep/wake cycle, preventing false alarms before the agent resumes.
#[allow(clippy::too_many_arguments)]
fn check_running_goal(
    goal: &GoalRun,
    config: &WatchdogConfig,
    store: &GoalRunStore,
    event_store: &FsEventStore,
    now: &chrono::DateTime<Utc>,
    issues: &mut Vec<HealthIssue>,
    in_wake_grace: bool,
    project_root: &Path,
) {
    // During wake grace period, suppress all liveness alerts.
    if in_wake_grace {
        tracing::debug!(
            goal_id = %goal.goal_run_id,
            "Skipping liveness check — within wake grace window"
        );
        return;
    }

    let pid = match goal.agent_pid {
        Some(pid) => pid,
        None => {
            // Legacy goal or spawn failure — no PID to check.
            let age_secs = (*now - goal.updated_at).num_seconds().unsigned_abs();
            if age_secs > config.zombie_transition_delay_secs {
                issues.push(HealthIssue {
                    kind: "unknown_pid".to_string(),
                    goal_id: Some(goal.goal_run_id),
                    detail: format!(
                        "Goal '{}' is running but has no agent PID (age: {}s). \
                         Run: ta goal recover {}",
                        goal.display_tag(),
                        age_secs,
                        &goal.goal_run_id.to_string()[..8],
                    ),
                });
            }
            return;
        }
    };

    if is_process_alive(pid) {
        // Process is alive. Check for stale condition only if heartbeats are expected.
        if goal.heartbeat_required {
            let age_secs = (*now - goal.updated_at).num_seconds().unsigned_abs();
            if age_secs > config.stale_question_threshold_secs {
                tracing::warn!(
                    goal_id = %goal.goal_run_id,
                    pid = pid,
                    age_secs = age_secs,
                    "Stale goal detected — agent alive but no heartbeat received"
                );
                issues.push(HealthIssue {
                    kind: "stale_goal".to_string(),
                    goal_id: Some(goal.goal_run_id),
                    detail: format!(
                        "Goal '{}' (PID {}) alive but no heartbeat for {}s (threshold: {}s). \
                         Agent is running but not sending heartbeats.",
                        goal.display_tag(),
                        pid,
                        age_secs,
                        config.stale_question_threshold_secs,
                    ),
                });
                // Stale: warn only, no state transition.
            }
        }
        return;
    }

    // Process is dead.
    // If the agent exited cleanly, run.rs should have already transitioned the goal to
    // `Finalizing` before we get here. A goal in `Running` with a dead PID means either:
    //   a) The process was killed unexpectedly (SIGKILL, crash)
    //   b) The run.rs exit handler crashed before writing `Finalizing`
    // In both cases, treat as zombie.

    let age_secs = (*now - goal.updated_at).num_seconds().unsigned_abs();
    if age_secs < config.zombie_transition_delay_secs {
        // Too soon — might be a brief restart or race with exit handler. Wait next cycle.
        tracing::debug!(
            goal_id = %goal.goal_run_id,
            pid = pid,
            age_secs = age_secs,
            "Process dead but within delay window — waiting for exit handler"
        );
        return;
    }

    let detail = format!(
        "Agent process (PID {}) for goal '{}' is gone without updating state \
         (last update: {}s ago). Run: ta goal recover {}",
        pid,
        goal.display_tag(),
        age_secs,
        &goal.goal_run_id.to_string()[..8],
    );

    tracing::warn!(
        goal_id = %goal.goal_run_id,
        pid = pid,
        age_secs = age_secs,
        prev_state = %goal.state,
        "Watchdog: goal state transition running -> failed (zombie_goal)"
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
        exit_code: None, // Can't recover exit code from a long-dead process.
        elapsed_secs: elapsed,
        detail,
    };
    if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
        tracing::warn!("Watchdog: failed to persist GoalProcessExited event: {}", e);
    }

    // v0.14.7.2: Write audit record before transitioning to Failed.
    write_watchdog_audit_entry(
        project_root,
        goal,
        Some(pid),
        &format!("zombie_goal: PID {} dead after {}s", pid, age_secs),
    );

    // Transition to Failed.
    let reason = format!(
        "Agent process (PID {}) exited without updating goal state. \
         Detected by daemon watchdog after {}s. \
         Run `ta goal recover {}` to restore state if a draft was created.",
        pid,
        age_secs,
        &goal.goal_run_id.to_string()[..8],
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
            prev_state = "running",
            new_state = "failed",
            reason = "zombie_goal",
            "Watchdog: goal state transition"
        );
    }
}

/// Check a goal in `Finalizing` state (v0.13.14).
///
/// `Finalizing` means the agent exited cleanly and draft creation is in progress.
/// The watchdog should leave it alone unless the timeout is exceeded (indicating
/// the draft-build process was interrupted).
#[allow(clippy::too_many_arguments)]
fn check_finalizing_goal(
    goal: &GoalRun,
    finalize_started_at: chrono::DateTime<Utc>,
    run_pid: Option<u32>,
    config: &WatchdogConfig,
    store: &GoalRunStore,
    now: &chrono::DateTime<Utc>,
    issues: &mut Vec<HealthIssue>,
    project_root: &Path,
) {
    // If the ta run process is still alive, it is actively building the draft.
    // Never fire the timeout while the builder process is running — only catch
    // the case where ta run died mid-build and the state is stuck (v0.13.17).
    if let Some(pid) = run_pid {
        if is_process_alive(pid) {
            tracing::debug!(
                goal_id = %goal.goal_run_id,
                run_pid = pid,
                "Goal in Finalizing — ta run process is alive, skipping timeout check"
            );
            return;
        }
    }

    let elapsed_secs = (*now - finalize_started_at).num_seconds().unsigned_abs();

    if elapsed_secs < config.finalize_timeout_secs {
        // Within timeout — draft creation is still in progress. Do nothing.
        tracing::debug!(
            goal_id = %goal.goal_run_id,
            elapsed_secs = elapsed_secs,
            timeout_secs = config.finalize_timeout_secs,
            "Goal in Finalizing state — draft creation in progress"
        );
        return;
    }

    // v0.13.17.2: Emit structured context about which operation timed out.
    let pid_context = match run_pid {
        Some(pid) => format!(
            "run_pid={} ({})",
            pid,
            if is_process_alive(pid) {
                "alive"
            } else {
                "dead"
            }
        ),
        None => "run_pid=none (no builder PID recorded)".to_string(),
    };

    // Read current progress note to know which operation was in progress.
    let progress_note = store
        .get(goal.goal_run_id)
        .ok()
        .flatten()
        .and_then(|g| g.progress_note.clone())
        .unwrap_or_else(|| "(no progress note)".to_string());

    // Timeout exceeded — draft creation was interrupted or very slow.
    let detail = format!(
        "Goal '{}' has been in Finalizing state for {}s (timeout: {}s, {}). \
         Last operation: '{}'. Draft creation may have been interrupted. \
         Run: ta goal recover {}",
        goal.display_tag(),
        elapsed_secs,
        config.finalize_timeout_secs,
        pid_context,
        progress_note,
        &goal.goal_run_id.to_string()[..8],
    );

    tracing::warn!(
        goal_id = %goal.goal_run_id,
        elapsed_secs = elapsed_secs,
        timeout_secs = config.finalize_timeout_secs,
        run_pid = ?run_pid,
        last_operation = %progress_note,
        prev_state = "finalizing",
        new_state = "failed",
        reason = "finalize_timeout",
        "Watchdog: goal state transition finalizing -> failed (timeout)"
    );

    issues.push(HealthIssue {
        kind: "finalize_timeout".to_string(),
        goal_id: Some(goal.goal_run_id),
        detail: detail.clone(),
    });

    let reason = format!(
        "Finalizing timed out after {}s (timeout: {}s, {}). Last operation: '{}'. \
         Run: ta goal recover {}",
        elapsed_secs,
        config.finalize_timeout_secs,
        pid_context,
        progress_note,
        &goal.goal_run_id.to_string()[..8],
    );

    // v0.14.7.2: Write audit record before transitioning to Failed.
    write_watchdog_audit_entry(
        project_root,
        goal,
        run_pid,
        &format!(
            "finalize_timeout: stuck {}s (timeout {}s), last_op='{}'",
            elapsed_secs, config.finalize_timeout_secs, progress_note
        ),
    );

    if let Err(e) = store.transition(goal.goal_run_id, GoalRunState::Failed { reason }) {
        tracing::warn!(
            goal_id = %goal.goal_run_id,
            "Watchdog: failed to transition stuck-finalizing goal to failed: {}",
            e
        );
    } else {
        tracing::info!(
            goal_id = %goal.goal_run_id,
            prev_state = "finalizing",
            new_state = "failed",
            reason = "finalize_timeout",
            "Watchdog: goal state transition"
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
/// Platform-specific: uses `kill(pid, 0)` on Unix, `OpenProcess` on Windows.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks process existence without sending a signal.
        // Returns 0 if the process exists and we have permission to send it signals.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        // OpenProcess(SYNCHRONIZE, FALSE, pid) is an O(1) kernel call — no subprocess.
        // A non-null non-invalid handle means the process exists; CloseHandle releases it.
        // ERROR_ACCESS_DENIED (5) means the process exists but we lack permission.
        //
        // Previously used `tasklist.exe` which is notoriously slow (1–3 s per call)
        // and was blocking the Tokio runtime thread during the watchdog cycle, which
        // delayed the HTTP server binding and caused "active but not ready" on startup.
        //
        // Raw extern declarations avoid needing a windows-sys crate dependency.
        #[allow(non_upper_case_globals)]
        const SYNCHRONIZE: u32 = 0x00100000;
        #[allow(non_upper_case_globals)]
        const ERROR_ACCESS_DENIED: u32 = 5;
        extern "system" {
            fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;
            fn CloseHandle(hObject: isize) -> i32;
            fn GetLastError() -> u32;
        }
        unsafe {
            let handle = OpenProcess(SYNCHRONIZE, 0, pid);
            if handle == 0 || handle == -1isize {
                // NULL or INVALID_HANDLE_VALUE — check error code.
                GetLastError() == ERROR_ACCESS_DENIED
            } else {
                CloseHandle(handle);
                true
            }
        }
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
        GoalRunState::Running | GoalRunState::Finalizing { .. } => match goal.agent_pid {
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
        // Terminal/finalizing states — process expected to be gone.
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
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);
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
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);

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
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);

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
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);

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
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);

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

    #[test]
    fn watchdog_state_detects_sleep_via_wall_vs_monotonic() {
        // Simulate a state where the wall clock advanced much more than the monotonic.
        // We craft a WatchdogState with prev_wall set far in the past.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".ta/goals")).unwrap();
        std::fs::create_dir_all(project.join(".ta/events")).unwrap();

        let config = WatchdogConfig::default();
        let mut state = WatchdogState {
            // Set wall clock to 5 minutes ago to simulate a 5-minute sleep.
            prev_wall: SystemTime::now() - Duration::from_secs(300),
            // Monotonic started 30 seconds ago (interval time).
            prev_monotonic: Instant::now() - Duration::from_secs(30),
            last_wake_wall: None,
            api_was_reachable: None,
        };

        watchdog_cycle(project, &config, &mut state, None);

        // After cycle, last_wake_wall should be set (sleep detected).
        assert!(
            state.last_wake_wall.is_some(),
            "Should have detected sleep and set last_wake_wall"
        );

        // Events dir should have a SystemWoke event.
        let events_dir = project.join(".ta/events");
        let event_store = FsEventStore::new(&events_dir);
        let events = event_store
            .query(&ta_events::store::EventQueryFilter::default())
            .unwrap_or_default();
        let has_system_woke = events.iter().any(|e| e.event_type == "system_woke");
        assert!(has_system_woke, "Expected system_woke event");
    }

    #[test]
    fn watchdog_state_no_false_sleep_on_normal_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".ta/goals")).unwrap();
        std::fs::create_dir_all(project.join(".ta/events")).unwrap();

        let config = WatchdogConfig::default();
        // Normal state: both prev_monotonic and prev_wall are ~30s ago.
        let mut state = WatchdogState {
            prev_wall: SystemTime::now() - Duration::from_secs(31),
            prev_monotonic: Instant::now() - Duration::from_secs(31),
            last_wake_wall: None,
            api_was_reachable: None,
        };

        watchdog_cycle(project, &config, &mut state, None);

        // No sleep detected — last_wake_wall stays None.
        assert!(
            state.last_wake_wall.is_none(),
            "Should not detect sleep on normal cycle"
        );
    }

    #[test]
    fn watchdog_skips_finalizing_within_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let goals_dir = project.join(".ta/goals");
        let events_dir = project.join(".ta/events");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::create_dir_all(&events_dir).unwrap();

        let store = GoalRunStore::new(&goals_dir).unwrap();
        let mut goal = GoalRun::new(
            "Finalizing",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        // Finalizing started 30 seconds ago — well within 1800s timeout.
        goal.state = ta_goal::GoalRunState::Finalizing {
            exit_code: 0,
            finalize_started_at: Utc::now() - chrono::Duration::seconds(30),
            run_pid: None,
        };
        store.save(&goal).unwrap();

        let config = WatchdogConfig::default();
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);

        // Goal should still be in Finalizing state (not transitioned to failed).
        let updated = store.get(goal.goal_run_id).unwrap().unwrap();
        assert!(
            matches!(updated.state, ta_goal::GoalRunState::Finalizing { .. }),
            "Expected Finalizing, got: {:?}",
            updated.state
        );
    }

    #[test]
    fn watchdog_finalizing_timeout_transitions_to_failed() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let goals_dir = project.join(".ta/goals");
        let events_dir = project.join(".ta/events");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::create_dir_all(&events_dir).unwrap();

        let store = GoalRunStore::new(&goals_dir).unwrap();
        let mut goal = GoalRun::new(
            "StuckFinalizing",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        // Finalizing started 2000 seconds ago — exceeds default 1800s timeout.
        // run_pid = None means no live process to defer to.
        goal.state = ta_goal::GoalRunState::Finalizing {
            exit_code: 0,
            finalize_started_at: Utc::now() - chrono::Duration::seconds(2000),
            run_pid: None,
        };
        store.save(&goal).unwrap();

        let config = WatchdogConfig {
            finalize_timeout_secs: 300,
            ..Default::default()
        };
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);

        // Goal should now be Failed.
        let updated = store.get(goal.goal_run_id).unwrap().unwrap();
        assert!(
            matches!(updated.state, ta_goal::GoalRunState::Failed { .. }),
            "Expected Failed after finalize timeout, got: {:?}",
            updated.state
        );
    }

    #[test]
    fn watchdog_stale_no_action_when_heartbeat_not_required() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let goals_dir = project.join(".ta/goals");
        let events_dir = project.join(".ta/events");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::create_dir_all(&events_dir).unwrap();

        let store = GoalRunStore::new(&goals_dir).unwrap();
        let mut goal = GoalRun::new(
            "LongRunning",
            "obj",
            "agent",
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        );
        // Running with our own PID (alive) — last update 2 hours ago.
        goal.state = ta_goal::GoalRunState::Running;
        goal.agent_pid = Some(std::process::id());
        goal.heartbeat_required = false;
        goal.updated_at = Utc::now() - chrono::Duration::seconds(7200);
        store.save(&goal).unwrap();

        let config = WatchdogConfig {
            stale_question_threshold_secs: 3600,
            ..Default::default()
        };
        let mut state = WatchdogState::default();
        watchdog_cycle(project, &config, &mut state, None);

        // Goal should still be Running (stale check disabled when heartbeat_required=false).
        let updated = store.get(goal.goal_run_id).unwrap().unwrap();
        assert_eq!(updated.state, ta_goal::GoalRunState::Running);
    }

    #[test]
    fn wake_grace_in_effect_suppresses_nothing_directly() {
        // Verify the in_wake_grace flag is computed correctly from state.
        let now = SystemTime::now();
        let config = WatchdogConfig {
            wake_grace_secs: 60,
            ..Default::default()
        };

        // Wake happened 10 seconds ago — still within grace window.
        let state_recent_wake = WatchdogState {
            last_wake_wall: Some(now - Duration::from_secs(10)),
            prev_wall: now - Duration::from_secs(31),
            prev_monotonic: Instant::now() - Duration::from_secs(31),
            api_was_reachable: None,
        };
        let in_grace = state_recent_wake
            .last_wake_wall
            .and_then(|w| now.duration_since(w).ok())
            .map(|d| d.as_secs() < config.wake_grace_secs)
            .unwrap_or(false);
        assert!(in_grace, "Should be in grace window");

        // Wake happened 90 seconds ago — outside grace window.
        let state_old_wake = WatchdogState {
            last_wake_wall: Some(now - Duration::from_secs(90)),
            prev_wall: now - Duration::from_secs(31),
            prev_monotonic: Instant::now() - Duration::from_secs(31),
            api_was_reachable: None,
        };
        let in_grace_old = state_old_wake
            .last_wake_wall
            .and_then(|w| now.duration_since(w).ok())
            .map(|d| d.as_secs() < config.wake_grace_secs)
            .unwrap_or(false);
        assert!(!in_grace_old, "Should not be in grace window after 90s");
    }
}
