// team_session.rs — Persistent team session supervision (v0.17.5.1).
//
// A `TeamSession` binds a workflow YAML (parsed CLI-side into an ordered
// stage/role list) to a `.ta/team.toml` team, and supervises a long-running
// loop that fires one `ta run` goal per role in sequence, carrying prior
// roles' findings forward as context — mirrors `connector_supervisor.rs`'s
// fault-isolated, file-protocol-driven, backoff/suspend subprocess
// supervision model, applied to a new subject (a team session) instead of a
// connector process.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ta_session::agent_action::TeamRole;
use ta_session::team::TeamConfig;

/// Lifecycle status of a `TeamSession`, persisted in `state.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamSessionStatus {
    Active,
    Paused,
    Suspended,
    Stopped,
}

/// One stage of the bound workflow, pre-resolved by the CLI at `start` time
/// from `WorkflowDefinition::stage_order()` — `ta-daemon` never parses the
/// workflow YAML itself (see plan Architecture note).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSessionStageConfig {
    pub name: String,
    pub roles: Vec<String>,
}

/// A completed role goal-run's carried-forward context, in stage order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleFinding {
    pub stage: String,
    pub role: String,
    pub completed_at: DateTime<Utc>,
    /// Trimmed tail of the goal-run's stdout — the finding a later role's
    /// context should see. Kept as free text; no structured schema is
    /// imposed on what a role "found".
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSessionConfig {
    pub name: String,
    pub workflow_path: String,
    pub team_toml_path: String,
    pub objective: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSessionState {
    pub id: String,
    pub config: TeamSessionConfig,
    pub stages: Vec<TeamSessionStageConfig>,
    pub status: TeamSessionStatus,
    pub current_stage_index: usize,
    pub findings: Vec<RoleFinding>,
    pub restart_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TeamSessionState {
    pub fn new(id: String, config: TeamSessionConfig, stages: Vec<TeamSessionStageConfig>) -> Self {
        let now = Utc::now();
        Self {
            id,
            config,
            stages,
            status: TeamSessionStatus::Active,
            current_stage_index: 0,
            findings: Vec::new(),
            restart_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn state_dir(project_root: &Path, id: &str) -> PathBuf {
        project_root.join(".ta").join("team-sessions").join(id)
    }

    pub fn state_path(project_root: &Path, id: &str) -> PathBuf {
        Self::state_dir(project_root, id).join("state.json")
    }

    /// Loads `state.json` for `id`, or `Ok(None)` if the session doesn't exist.
    pub fn load(project_root: &Path, id: &str) -> io::Result<Option<Self>> {
        let path = Self::state_path(project_root, id);
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let state: TeamSessionState = serde_json::from_str(&raw)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Persists this state to `.ta/team-sessions/<id>/state.json`, creating
    /// the directory if needed. Updates `updated_at` before writing.
    pub fn save(&mut self, project_root: &Path) -> io::Result<()> {
        self.updated_at = Utc::now();
        let dir = Self::state_dir(project_root, &self.id);
        std::fs::create_dir_all(&dir)?;
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(Self::state_path(project_root, &self.id), raw)
    }

    /// Lists all session IDs with a `state.json` under `.ta/team-sessions/`.
    pub fn list_ids(project_root: &Path) -> Vec<String> {
        let dir = project_root.join(".ta").join("team-sessions");
        let mut ids = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if Self::state_path(project_root, &name).exists() {
                        ids.push(name);
                    }
                }
            }
        }
        ids.sort();
        ids
    }
}

/// Live supervisor status, written by the running loop and read by the CLI —
/// mirrors `ConnectorSupervisorStatus` in `connector_supervisor.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSessionSupervisorStatus {
    pub id: String,
    pub status: String, // "active" | "paused" | "suspended" | "stopped"
    pub current_stage: Option<String>,
    pub current_role: Option<String>,
    pub restart_count: u32,
    pub last_cycle_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

fn supervisor_status_path(project_root: &Path, id: &str) -> PathBuf {
    TeamSessionState::state_dir(project_root, id).join("supervisor-status.json")
}

pub fn write_supervisor_status(
    project_root: &Path,
    status: &TeamSessionSupervisorStatus,
) -> io::Result<()> {
    let dir = TeamSessionState::state_dir(project_root, &status.id);
    std::fs::create_dir_all(&dir)?;
    let raw = serde_json::to_string_pretty(status)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(supervisor_status_path(project_root, &status.id), raw)
}

pub fn read_supervisor_status(
    project_root: &Path,
    id: &str,
) -> Option<TeamSessionSupervisorStatus> {
    let raw = std::fs::read_to_string(supervisor_status_path(project_root, id)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Names of the control-signal files a session's directory may contain.
/// CLI commands write these; the supervised loop consumes (deletes) them.
const SIGNAL_PAUSE: &str = "pause-signal";
const SIGNAL_RESUME: &str = "resume-signal";
const SIGNAL_STOP: &str = "stop-signal";
const SIGNAL_RESTART: &str = "restart-signal";

fn signal_path(project_root: &Path, id: &str, signal: &str) -> PathBuf {
    TeamSessionState::state_dir(project_root, id).join(signal)
}

fn write_signal(project_root: &Path, id: &str, signal: &str) -> io::Result<()> {
    let dir = TeamSessionState::state_dir(project_root, id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(signal_path(project_root, id, signal), signal)
}

pub fn signal_pause(project_root: &Path, id: &str) -> io::Result<()> {
    write_signal(project_root, id, SIGNAL_PAUSE)
}

pub fn signal_resume(project_root: &Path, id: &str) -> io::Result<()> {
    write_signal(project_root, id, SIGNAL_RESUME)
}

pub fn signal_stop(project_root: &Path, id: &str) -> io::Result<()> {
    write_signal(project_root, id, SIGNAL_STOP)
}

/// Clears a `Suspended` session so the supervised loop resumes retrying —
/// same semantics as `connector_supervisor.rs`'s restart-signal.
pub fn signal_restart(project_root: &Path, id: &str) -> io::Result<()> {
    write_signal(project_root, id, SIGNAL_RESTART)
}

pub fn has_signal(project_root: &Path, id: &str, signal: &str) -> bool {
    signal_path(project_root, id, signal).exists()
}

/// Deletes a signal file after the loop has acted on it, so it isn't
/// reprocessed on the next cycle.
pub fn consume_signal(project_root: &Path, id: &str, signal: &str) -> io::Result<()> {
    let path = signal_path(project_root, id, signal);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Same backoff/suspend constants as `connector_supervisor.rs`, reused
/// verbatim per PLAN.md item 4 ("reuse `connector_supervisor.rs`'s
/// backoff/suspend pattern").
const MAX_BACKOFF_SECS: u64 = 60;
const SUSPEND_FAILURE_COUNT: u32 = 5;
const SUSPEND_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone, Copy)]
pub enum BackoffDecision {
    Retry { delay_secs: u64 },
    Suspend,
}

/// Tracks recent goal-run failures for one team session, in-memory only —
/// same lifetime as `connector_supervisor.rs`'s `recent_failure_times: Vec<Instant>`
/// (lost on daemon restart; acceptable, matches existing precedent).
#[derive(Debug, Clone, Default)]
pub struct FailureTracker {
    recent_failure_times: Vec<DateTime<Utc>>,
}

impl FailureTracker {
    /// Records a failure at `now` and returns whether the caller should
    /// retry with a backoff delay or suspend the session.
    pub fn record_failure(&mut self, now: DateTime<Utc>) -> BackoffDecision {
        self.recent_failure_times
            .retain(|t| (now - *t).num_seconds() < SUSPEND_WINDOW_SECS);
        self.recent_failure_times.push(now);

        if self.recent_failure_times.len() as u32 >= SUSPEND_FAILURE_COUNT {
            return BackoffDecision::Suspend;
        }

        let restart_count = self.recent_failure_times.len() as u32 - 1;
        let delay_secs = 2u64
            .saturating_pow(restart_count)
            .clamp(1, MAX_BACKOFF_SECS);
        BackoffDecision::Retry { delay_secs }
    }

    /// Clears failure history — called after a successful cycle, or when a
    /// `Suspended` session is explicitly restarted via the restart-signal.
    pub fn reset(&mut self) {
        self.recent_failure_times.clear();
    }
}

/// Renders prior roles' findings as markdown context for the next role's
/// goal-run — same "prior findings become the next goal's objective
/// context" shape as `ta_session::advisor_agent::build_advisor_context`,
/// applied to a team session's own findings instead of a draft/phase
/// summary.
pub fn render_session_context(state: &TeamSessionState) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Team session: {}\n\n", state.config.name));
    out.push_str(&format!("**Objective:** {}\n\n", state.config.objective));

    if state.findings.is_empty() {
        out.push_str("No prior role findings yet — this is the session's first goal-run.\n");
        return out;
    }

    out.push_str("## Prior role findings\n\n");
    for finding in &state.findings {
        out.push_str(&format!(
            "### {} ({}) — completed {}\n\n{}\n\n",
            finding.role,
            finding.stage,
            finding.completed_at.to_rfc3339(),
            finding.summary,
        ));
    }
    out
}

fn session_context_path(project_root: &Path, id: &str, stage_name: &str) -> PathBuf {
    TeamSessionState::state_dir(project_root, id).join(format!("context-{stage_name}.md"))
}

/// Writes the rendered context to `.ta/team-sessions/<id>/context-<stage>.md`
/// and returns the path, for use as `ta run --objective-file <path>`.
pub fn write_session_context(
    project_root: &Path,
    state: &TeamSessionState,
    stage_name: &str,
) -> io::Result<PathBuf> {
    let dir = TeamSessionState::state_dir(project_root, &state.id);
    std::fs::create_dir_all(&dir)?;
    let path = session_context_path(project_root, &state.id, stage_name);
    std::fs::write(&path, render_session_context(state))?;
    Ok(path)
}

/// Builds the `ta run` argument list for firing the next role's goal-run,
/// mirroring `apps/ta-cli/src/commands/intake.rs::execute_routed_goal`'s
/// command construction. Returns a plain `Vec<String>` (not a `Command`)
/// so the argument logic is unit-testable without spawning a process.
pub fn build_ta_run_args(
    state: &TeamSessionState,
    stage: &TeamSessionStageConfig,
    role: &str,
    team_config: &TeamConfig,
    context_path: &Path,
) -> Vec<String> {
    let title = format!("{}: {} ({})", state.config.name, stage.name, role);
    let mut args = vec![
        "run".to_string(),
        title,
        "--headless".to_string(),
        "--objective-file".to_string(),
        context_path.to_string_lossy().to_string(),
        "--team".to_string(),
        role.to_string(),
    ];

    if let Some(member) = team_config.find_by_role(&TeamRole::new(role)) {
        args.push("--security".to_string());
        args.push(member.security.to_string());
        if let Some(persona) = &member.persona {
            args.push("--persona".to_string());
            args.push(persona.clone());
        }
        args.push("--agent".to_string());
        args.push(member.agent_id.clone());
    }
    // A role with no `.ta/team.toml` assignment yet falls through to
    // `ta run`'s own default resolution chain (workflow.toml, daemon.toml,
    // "claude-code") rather than failing the cycle outright.

    args
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleOutcome {
    /// The current role's goal-run succeeded; state advanced to the next
    /// role/stage (wrapping to stage 0 after the last stage — a team
    /// session runs continuously, not one cycle-through-and-done).
    Advanced,
    /// The goal-run failed; retry after `delay_secs`.
    Retrying { delay_secs: u64 },
    /// 5 failures within 5 minutes — stop attempting new goal-runs until a
    /// restart-signal is written.
    Suspended,
    /// A pause-signal was consumed; no goal-run was attempted this cycle.
    Paused,
    /// A stop-signal was consumed; the session is now `Stopped` and the
    /// caller should stop scheduling further cycles for this id.
    Stopped,
}

/// Runs exactly one supervised cycle for team session `id`: checks control
/// signals, otherwise resolves the next role and fires its `ta run`
/// goal synchronously (blocking on subprocess completion). Kept
/// synchronous and side-effect-explicit (loads/saves state itself,
/// returns rather than mutates the tracker) so it can be unit-tested with
/// a fake `ta` binary the same way `advisor_agent.rs`'s own subprocess
/// tests do, without any `tokio::test` infra.
pub fn run_one_cycle(
    project_root: &Path,
    id: &str,
    ta_bin: &Path,
    mut tracker: FailureTracker,
) -> io::Result<(CycleOutcome, FailureTracker)> {
    let mut state = match TeamSessionState::load(project_root, id)? {
        Some(s) => s,
        None => return Ok((CycleOutcome::Stopped, tracker)),
    };

    if has_signal(project_root, id, SIGNAL_STOP) {
        consume_signal(project_root, id, SIGNAL_STOP)?;
        state.status = TeamSessionStatus::Stopped;
        state.save(project_root)?;
        write_supervisor_status(
            project_root,
            &TeamSessionSupervisorStatus {
                id: id.to_string(),
                status: "stopped".to_string(),
                current_stage: None,
                current_role: None,
                restart_count: state.restart_count,
                last_cycle_at: Some(Utc::now()),
                updated_at: Utc::now(),
            },
        )?;
        return Ok((CycleOutcome::Stopped, tracker));
    }

    if state.status == TeamSessionStatus::Suspended {
        if has_signal(project_root, id, SIGNAL_RESTART) {
            consume_signal(project_root, id, SIGNAL_RESTART)?;
            tracker.reset();
            state.status = TeamSessionStatus::Active;
            state.save(project_root)?;
        } else {
            return Ok((CycleOutcome::Suspended, tracker));
        }
    }

    if has_signal(project_root, id, SIGNAL_PAUSE) {
        consume_signal(project_root, id, SIGNAL_PAUSE)?;
        state.status = TeamSessionStatus::Paused;
        state.save(project_root)?;
    }
    if state.status == TeamSessionStatus::Paused {
        if has_signal(project_root, id, SIGNAL_RESUME) {
            consume_signal(project_root, id, SIGNAL_RESUME)?;
            state.status = TeamSessionStatus::Active;
            state.save(project_root)?;
        } else {
            write_supervisor_status(
                project_root,
                &TeamSessionSupervisorStatus {
                    id: id.to_string(),
                    status: "paused".to_string(),
                    current_stage: None,
                    current_role: None,
                    restart_count: state.restart_count,
                    last_cycle_at: Some(Utc::now()),
                    updated_at: Utc::now(),
                },
            )?;
            return Ok((CycleOutcome::Paused, tracker));
        }
    }

    if state.stages.is_empty() {
        return Ok((CycleOutcome::Stopped, tracker));
    }
    let stage_index = state.current_stage_index % state.stages.len();
    let stage = state.stages[stage_index].clone();
    let role = stage
        .roles
        .first()
        .cloned()
        .unwrap_or_else(|| "implementer".to_string());

    let team_config = TeamConfig::load(project_root).unwrap_or_default();
    let context_path = write_session_context(project_root, &state, &stage.name)?;
    let args = build_ta_run_args(&state, &stage, &role, &team_config, &context_path);

    let output = std::process::Command::new(ta_bin)
        .args(&args)
        .current_dir(project_root)
        .output();

    let now = Utc::now();
    match output {
        Ok(out) if out.status.success() => {
            let summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
            state.findings.push(RoleFinding {
                stage: stage.name.clone(),
                role: role.clone(),
                completed_at: now,
                summary: if summary.is_empty() {
                    format!("Role '{role}' completed with no stdout output.")
                } else {
                    summary
                },
            });
            state.current_stage_index = stage_index + 1;
            state.restart_count = 0;
            state.status = TeamSessionStatus::Active;
            state.save(project_root)?;
            tracker.reset();
            write_supervisor_status(
                project_root,
                &TeamSessionSupervisorStatus {
                    id: id.to_string(),
                    status: "active".to_string(),
                    current_stage: Some(stage.name),
                    current_role: Some(role),
                    restart_count: 0,
                    last_cycle_at: Some(now),
                    updated_at: now,
                },
            )?;
            Ok((CycleOutcome::Advanced, tracker))
        }
        _ => {
            state.restart_count += 1;
            state.save(project_root)?;
            let decision = tracker.record_failure(now);
            let (status_str, outcome) = match decision {
                BackoffDecision::Retry { delay_secs } => {
                    ("active".to_string(), CycleOutcome::Retrying { delay_secs })
                }
                BackoffDecision::Suspend => {
                    state.status = TeamSessionStatus::Suspended;
                    state.save(project_root)?;
                    ("suspended".to_string(), CycleOutcome::Suspended)
                }
            };
            write_supervisor_status(
                project_root,
                &TeamSessionSupervisorStatus {
                    id: id.to_string(),
                    status: status_str,
                    current_stage: Some(stage.name),
                    current_role: Some(role),
                    restart_count: state.restart_count,
                    last_cycle_at: Some(now),
                    updated_at: now,
                },
            )?;
            Ok((outcome, tracker))
        }
    }
}

const IDLE_POLL_SECS: u64 = 5;

/// Runs the supervised loop for one team session until it's `Stopped` or
/// the daemon shuts down. Each cycle's actual work (`run_one_cycle`) is
/// synchronous and runs on a blocking thread via `spawn_blocking`, so an
/// in-flight `ta run` subprocess is not interrupted by shutdown — only the
/// next cycle is skipped, matching `connector_supervisor.rs`'s own
/// "in-flight work finishes, then the loop exits" shutdown behavior.
async fn run_team_session(
    project_root: PathBuf,
    id: String,
    ta_bin: PathBuf,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let mut tracker = FailureTracker::default();
    loop {
        let pr = project_root.clone();
        let sid = id.clone();
        let bin = ta_bin.clone();
        let cur_tracker = tracker.clone();

        let cycle_result = tokio::select! {
            r = tokio::task::spawn_blocking(move || run_one_cycle(&pr, &sid, &bin, cur_tracker)) => r,
            _ = shutdown.notified() => return,
        };

        let (outcome, next_tracker) = match cycle_result {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                tracing::error!(session_id = %id, error = %e, "team session cycle I/O error");
                (
                    CycleOutcome::Retrying {
                        delay_secs: IDLE_POLL_SECS,
                    },
                    tracker,
                )
            }
            Err(join_err) => {
                tracing::error!(session_id = %id, error = %join_err, "team session cycle task panicked");
                (
                    CycleOutcome::Retrying {
                        delay_secs: IDLE_POLL_SECS,
                    },
                    tracker,
                )
            }
        };
        tracker = next_tracker;

        let sleep_secs = match outcome {
            CycleOutcome::Stopped => return,
            CycleOutcome::Advanced => 0,
            CycleOutcome::Retrying { delay_secs } => delay_secs,
            CycleOutcome::Suspended => IDLE_POLL_SECS, // poll for a restart-signal
            CycleOutcome::Paused => IDLE_POLL_SECS,    // poll for a resume/stop-signal
        };

        if sleep_secs > 0 {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {}
                _ = shutdown.notified() => return,
            }
        }
    }
}

/// Discovers all team sessions under `.ta/team-sessions/` and spawns one
/// supervised loop per session whose persisted status isn't already
/// `Stopped` — mirrors `connector_supervisor::start`'s "read config, spawn
/// one task per entry" shape. Returns the join handles for introspection
/// (tests / graceful-shutdown awaiting), matching `connector_supervisor`'s
/// `AllQueues` return-for-introspection precedent.
pub fn start(
    project_root: PathBuf,
    shutdown: Arc<tokio::sync::Notify>,
) -> Vec<tokio::task::JoinHandle<()>> {
    // `std::env::current_exe()` would resolve to this process's own binary
    // (`ta-daemon`), not the `ta` CLI `run_one_cycle` needs to spawn — reuse
    // `web.rs`'s existing sibling-binary lookup (adjacent-to-daemon, falling
    // back to bare "ta" resolved via PATH) rather than reinventing it.
    let ta_bin = PathBuf::from(crate::web::find_ta_binary_web());
    let mut handles = Vec::new();
    for id in TeamSessionState::list_ids(&project_root) {
        let Ok(Some(state)) = TeamSessionState::load(&project_root, &id) else {
            continue;
        };
        if state.status == TeamSessionStatus::Stopped {
            continue;
        }
        let pr = project_root.clone();
        let bin = ta_bin.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            run_team_session(pr, id, bin, sd).await;
        }));
    }
    handles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> TeamSessionConfig {
        TeamSessionConfig {
            name: "trading-desk".to_string(),
            workflow_path: "templates/workflows/trading-desk.yaml".to_string(),
            team_toml_path: ".ta/team.toml".to_string(),
            objective: "Generate income > 2x within 6 months after fees".to_string(),
        }
    }

    fn sample_stages() -> Vec<TeamSessionStageConfig> {
        vec![
            TeamSessionStageConfig {
                name: "analyze".to_string(),
                roles: vec!["analyst".to_string()],
            },
            TeamSessionStageConfig {
                name: "decide".to_string(),
                roles: vec!["strategist".to_string()],
            },
            TeamSessionStageConfig {
                name: "execute".to_string(),
                roles: vec!["trader".to_string()],
            },
        ]
    }

    #[test]
    fn state_persists_and_is_readable_across_two_sequential_goal_runs() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path();

        // First goal-run: create session, complete the "analyst" role, save.
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.findings.push(RoleFinding {
            stage: "analyze".to_string(),
            role: "analyst".to_string(),
            completed_at: Utc::now(),
            summary: "Market conditions favor a conservative allocation.".to_string(),
        });
        state.current_stage_index = 1;
        state.save(project_root).unwrap();

        // Second goal-run: a fresh load (simulating a new supervisor cycle)
        // must see the first goal-run's finding and stage progress.
        let reloaded = TeamSessionState::load(project_root, "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.findings.len(), 1);
        assert_eq!(reloaded.findings[0].role, "analyst");
        assert_eq!(reloaded.current_stage_index, 1);

        // Complete the "strategist" role in this second cycle, save again.
        let mut reloaded = reloaded;
        reloaded.findings.push(RoleFinding {
            stage: "decide".to_string(),
            role: "strategist".to_string(),
            completed_at: Utc::now(),
            summary: "Decided to open a small long position.".to_string(),
        });
        reloaded.current_stage_index = 2;
        reloaded.save(project_root).unwrap();

        let final_state = TeamSessionState::load(project_root, "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(final_state.findings.len(), 2);
        assert_eq!(final_state.current_stage_index, 2);
    }

    #[test]
    fn load_missing_session_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TeamSessionState::load(dir.path(), "nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn list_ids_finds_only_dirs_with_state_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut state =
            TeamSessionState::new("sess-a".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();
        // A stray directory with no state.json must not be listed.
        std::fs::create_dir_all(
            dir.path()
                .join(".ta")
                .join("team-sessions")
                .join("not-a-session"),
        )
        .unwrap();

        let ids = TeamSessionState::list_ids(dir.path());
        assert_eq!(ids, vec!["sess-a".to_string()]);
    }

    #[test]
    fn supervisor_status_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let status = TeamSessionSupervisorStatus {
            id: "sess-1".to_string(),
            status: "active".to_string(),
            current_stage: Some("analyze".to_string()),
            current_role: Some("analyst".to_string()),
            restart_count: 2,
            last_cycle_at: Some(Utc::now()),
            updated_at: Utc::now(),
        };
        write_supervisor_status(dir.path(), &status).unwrap();
        let read_back = read_supervisor_status(dir.path(), "sess-1").unwrap();
        assert_eq!(read_back.status, "active");
        assert_eq!(read_back.restart_count, 2);
        assert_eq!(read_back.current_role.as_deref(), Some("analyst"));
    }

    #[test]
    fn read_supervisor_status_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_supervisor_status(dir.path(), "nope").is_none());
    }

    #[test]
    fn signal_files_write_check_and_consume() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_signal(dir.path(), "sess-1", SIGNAL_PAUSE));

        signal_pause(dir.path(), "sess-1").unwrap();
        assert!(has_signal(dir.path(), "sess-1", SIGNAL_PAUSE));

        consume_signal(dir.path(), "sess-1", SIGNAL_PAUSE).unwrap();
        assert!(!has_signal(dir.path(), "sess-1", SIGNAL_PAUSE));

        // Consuming an already-absent signal is a no-op, not an error.
        consume_signal(dir.path(), "sess-1", SIGNAL_PAUSE).unwrap();
    }

    #[test]
    fn all_four_signal_kinds_are_distinct_files() {
        let dir = tempfile::tempdir().unwrap();
        signal_stop(dir.path(), "sess-1").unwrap();
        signal_restart(dir.path(), "sess-1").unwrap();
        assert!(has_signal(dir.path(), "sess-1", SIGNAL_STOP));
        assert!(has_signal(dir.path(), "sess-1", SIGNAL_RESTART));
        assert!(!has_signal(dir.path(), "sess-1", SIGNAL_PAUSE));
        assert!(!has_signal(dir.path(), "sess-1", SIGNAL_RESUME));
    }

    #[test]
    fn backoff_doubles_before_the_suspend_threshold() {
        let mut tracker = FailureTracker::default();
        let base = Utc::now();

        // Suspend triggers on the 5th failure in-window (SUSPEND_FAILURE_COUNT),
        // so only the first 4 failures ever produce a Retry decision.
        let d1 = tracker.record_failure(base);
        assert!(matches!(d1, BackoffDecision::Retry { delay_secs: 1 }));

        let d2 = tracker.record_failure(base + chrono::Duration::seconds(1));
        assert!(matches!(d2, BackoffDecision::Retry { delay_secs: 2 }));

        let d3 = tracker.record_failure(base + chrono::Duration::seconds(2));
        assert!(matches!(d3, BackoffDecision::Retry { delay_secs: 4 }));

        let d4 = tracker.record_failure(base + chrono::Duration::seconds(3));
        assert!(matches!(d4, BackoffDecision::Retry { delay_secs: 8 }));

        let d5 = tracker.record_failure(base + chrono::Duration::seconds(4));
        assert!(matches!(d5, BackoffDecision::Suspend));
    }

    #[test]
    fn fifth_failure_within_window_suspends() {
        let mut tracker = FailureTracker::default();
        let base = Utc::now();
        for i in 0..4 {
            let decision = tracker.record_failure(base + chrono::Duration::seconds(i));
            assert!(
                matches!(decision, BackoffDecision::Retry { .. }),
                "failure {i} should retry"
            );
        }
        let fifth = tracker.record_failure(base + chrono::Duration::seconds(4));
        assert!(
            matches!(fifth, BackoffDecision::Suspend),
            "5th failure in-window must suspend"
        );
    }

    #[test]
    fn failures_outside_the_five_minute_window_do_not_accumulate() {
        let mut tracker = FailureTracker::default();
        let base = Utc::now();
        // 4 failures, then a 5th more than 300s later — the first 4 should
        // have aged out, so this is effectively a fresh first failure.
        for i in 0..4 {
            tracker.record_failure(base + chrono::Duration::seconds(i));
        }
        let later = tracker.record_failure(base + chrono::Duration::seconds(400));
        assert!(matches!(later, BackoffDecision::Retry { delay_secs: 1 }));
    }

    #[test]
    fn reset_clears_history() {
        let mut tracker = FailureTracker::default();
        let base = Utc::now();
        tracker.record_failure(base);
        tracker.record_failure(base + chrono::Duration::seconds(1));
        tracker.reset();
        let decision = tracker.record_failure(base + chrono::Duration::seconds(2));
        assert!(matches!(decision, BackoffDecision::Retry { delay_secs: 1 }));
    }

    #[test]
    fn render_context_with_no_findings_says_so() {
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let rendered = render_session_context(&state);
        assert!(rendered.contains("No prior role findings yet"));
        assert!(rendered.contains("trading-desk"));
    }

    #[test]
    fn render_context_lists_findings_in_order() {
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.findings.push(RoleFinding {
            stage: "analyze".to_string(),
            role: "analyst".to_string(),
            completed_at: Utc::now(),
            summary: "Bullish on tech.".to_string(),
        });
        state.findings.push(RoleFinding {
            stage: "decide".to_string(),
            role: "strategist".to_string(),
            completed_at: Utc::now(),
            summary: "Open a long position.".to_string(),
        });
        let rendered = render_session_context(&state);
        let analyst_pos = rendered.find("analyst").unwrap();
        let strategist_pos = rendered.find("strategist").unwrap();
        assert!(
            analyst_pos < strategist_pos,
            "findings must render in stage order"
        );
        assert!(rendered.contains("Bullish on tech."));
        assert!(rendered.contains("Open a long position."));
    }

    #[test]
    fn write_session_context_creates_the_expected_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let path = write_session_context(dir.path(), &state, "analyze").unwrap();
        assert_eq!(
            path,
            dir.path()
                .join(".ta")
                .join("team-sessions")
                .join("sess-1")
                .join("context-analyze.md")
        );
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("trading-desk"));
    }

    #[test]
    fn build_args_with_assigned_role_includes_security_persona_and_agent() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path = write_session_context(dir.path(), &state, &stage.name).unwrap();

        let mut team_config = TeamConfig::default();
        team_config.assign(
            TeamRole::new("analyst"),
            "claude-sonnet-4-6".to_string(),
            ta_session::workflow_session::AdvisorSecurity::Auto,
            Some("careful-analyst".to_string()),
        );

        let args = build_ta_run_args(&state, stage, "analyst", &team_config, &context_path);

        assert_eq!(args[0], "run");
        assert!(args.contains(&"--headless".to_string()));
        assert!(args.contains(&"--objective-file".to_string()));
        assert!(args.contains(&context_path.to_string_lossy().to_string()));
        assert!(args.contains(&"--team".to_string()));
        assert!(args.contains(&"analyst".to_string()));
        assert!(args.contains(&"--security".to_string()));
        assert!(args.contains(&"auto".to_string()));
        assert!(args.contains(&"--persona".to_string()));
        assert!(args.contains(&"careful-analyst".to_string()));
        assert!(args.contains(&"--agent".to_string()));
        assert!(args.contains(&"claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn build_args_with_unassigned_role_omits_security_and_persona() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path = write_session_context(dir.path(), &state, &stage.name).unwrap();

        let team_config = TeamConfig::default(); // no members assigned

        let args = build_ta_run_args(&state, stage, "analyst", &team_config, &context_path);

        assert!(!args.contains(&"--security".to_string()));
        assert!(!args.contains(&"--persona".to_string()));
        assert!(!args.contains(&"--agent".to_string()));
        // Still fires the goal — just without an assignment-derived override.
        assert!(args.contains(&"--team".to_string()));
    }

    #[cfg(unix)]
    fn write_fake_ta_binary(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-ta");
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_advances_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(
            dir.path(),
            "#!/bin/sh\necho 'analyst finding: bullish on tech'\nexit 0\n",
        );
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();

        let (outcome, tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();

        assert_eq!(outcome, CycleOutcome::Advanced);
        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.findings.len(), 1);
        assert!(reloaded.findings[0].summary.contains("bullish on tech"));
        assert_eq!(reloaded.current_stage_index, 1);
        let (_outcome2, _tracker2) = (outcome, tracker); // silence unused if not asserted further
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_stage_index_wraps_after_last_stage() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\necho ok\nexit 0\n");
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.current_stage_index = 2; // last of 3 stages (indices 0,1,2)
        state.save(dir.path()).unwrap();

        let (outcome, _tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();

        assert_eq!(outcome, CycleOutcome::Advanced);
        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.current_stage_index, 3);
        // Next cycle should wrap back to stage 0 (3 % 3 == 0) — verified by
        // running a second cycle and checking it targets stage "analyze".
        let (outcome2, _tracker3) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();
        assert_eq!(outcome2, CycleOutcome::Advanced);
        let final_state = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(final_state.findings[1].stage, "analyze");
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_crash_looping_session_reaches_suspended_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\nexit 1\n");
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();

        let mut tracker = FailureTracker::default();
        let mut last_outcome = CycleOutcome::Advanced;
        for _ in 0..5 {
            let (outcome, next_tracker) =
                run_one_cycle(dir.path(), "sess-1", &ta_bin, tracker).unwrap();
            tracker = next_tracker;
            last_outcome = outcome;
        }
        assert_eq!(last_outcome, CycleOutcome::Suspended);

        // Further cycles must not attempt new goal-runs — status stays
        // Suspended and no new failure is recorded.
        let (outcome_after, _t) = run_one_cycle(dir.path(), "sess-1", &ta_bin, tracker).unwrap();
        assert_eq!(outcome_after, CycleOutcome::Suspended);

        let status = read_supervisor_status(dir.path(), "sess-1").unwrap();
        assert_eq!(status.status, "suspended");
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_status_reflects_real_supervisor_state() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\necho done\nexit 0\n");
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();

        run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();

        let status = read_supervisor_status(dir.path(), "sess-1").unwrap();
        assert_eq!(status.status, "active");
        assert_eq!(status.current_role.as_deref(), Some("analyst"));
        assert_eq!(status.restart_count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_stop_signal_stops_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\nexit 0\n");
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();
        signal_stop(dir.path(), "sess-1").unwrap();

        let (outcome, _tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();

        assert_eq!(outcome, CycleOutcome::Stopped);
        let reloaded = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.status, TeamSessionStatus::Stopped);
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_pause_then_resume_signal() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\necho ok\nexit 0\n");
        let mut state =
            TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();
        signal_pause(dir.path(), "sess-1").unwrap();

        let (outcome, tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();
        assert_eq!(outcome, CycleOutcome::Paused);
        let paused_state = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(paused_state.status, TeamSessionStatus::Paused);

        signal_resume(dir.path(), "sess-1").unwrap();
        let (outcome2, _tracker2) = run_one_cycle(dir.path(), "sess-1", &ta_bin, tracker).unwrap();
        assert_eq!(outcome2, CycleOutcome::Advanced);
    }
}
