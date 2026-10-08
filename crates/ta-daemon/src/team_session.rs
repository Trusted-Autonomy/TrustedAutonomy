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
use ta_policy::business_budget::BudgetGuardrails;
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
    /// The bound workflow's business-metric budget guardrail, resolved by
    /// the CLI from `WorkflowDefinition.budget` at `start` time (v0.17.5.2)
    /// — `ta-daemon` never parses the workflow YAML itself, mirroring how
    /// `stages` is already pre-resolved rather than re-derived here.
    #[serde(default)]
    pub budget: Option<BudgetGuardrails>,
    /// Each role's `prompt:` text from the workflow YAML, keyed by role
    /// name and resolved once by the CLI from `WorkflowDefinition.roles` at
    /// `start` time, same "CLI parses the YAML, daemon never does" split
    /// as `stages`/`budget` above. Without this, a role's own instructions
    /// (e.g. trading-desk.yaml's "You are a trader...") never reached the
    /// agent at all; only the session-level `objective` and prior findings
    /// did (found during Phase 1 live testing of ta-virtual-team, 2026-09).
    /// `#[serde(default)]` so state.json files written before this field
    /// existed still load.
    #[serde(default)]
    pub role_prompts: std::collections::HashMap<String, String>,
    /// Biscuit-backed grant scoped to `whiteboard:team_session:<name>`,
    /// minted at `start()` time when `[whiteboard] enabled = true`. `None`
    /// when whiteboard coordination is off for this project. Threaded into
    /// each role's launch so agent processes can call the new
    /// `ta_whiteboard_*` MCP tools.
    #[serde(default)]
    pub whiteboard_token: Option<String>,
    /// When `whiteboard_token` expires (v0.17.11.12) — `token_refresh.rs`'s
    /// periodic task re-mints the token and updates both fields together
    /// well before this passes, so a long-running session never actually
    /// hits it. `None` for a session with no whiteboard token, or one
    /// started before this field existed (`#[serde(default)]`) — the
    /// refresh task treats a missing expiry on a *present* token as
    /// "refresh it now" rather than "never expires", so an old session
    /// self-heals into having a real expiry on its next refresh check
    /// instead of silently never being refreshed.
    #[serde(default)]
    pub whiteboard_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSessionState {
    pub id: String,
    pub config: TeamSessionConfig,
    pub stages: Vec<TeamSessionStageConfig>,
    /// Roles launched on demand by `wake_listener.rs` when a message
    /// arrives on one of their registered keys, instead of taking a turn in
    /// `stages`' round-robin rotation (v0.17.11.10). A sibling list, not a
    /// flag on `TeamSessionStageConfig` -- keeps `stages` unambiguously
    /// "the rotation" and avoids conflating the two membership questions.
    /// `#[serde(default)]` so state.json files written before this field
    /// existed still load.
    #[serde(default)]
    pub wake_on_demand_listeners: Vec<crate::wake_listener::WakeListenerConfig>,
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
            wake_on_demand_listeners: Vec::new(),
            status: TeamSessionStatus::Active,
            current_stage_index: 0,
            findings: Vec::new(),
            restart_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_wake_on_demand_listeners(
        mut self,
        listeners: Vec<crate::wake_listener::WakeListenerConfig>,
    ) -> Self {
        self.wake_on_demand_listeners = listeners;
        self
    }

    pub fn state_dir(project_root: &Path, id: &str) -> PathBuf {
        project_root.join(".ta").join("team-sessions").join(id)
    }

    pub fn state_path(project_root: &Path, id: &str) -> PathBuf {
        Self::state_dir(project_root, id).join("state.json")
    }

    /// Path to this session's business-metric budget ledger (v0.17.5.2) —
    /// the same file `.ta_human_verify`'s `budget.ledger_path` param should
    /// point at, so a role's budgeted actions accumulate into the session's
    /// own running total.
    pub fn budget_ledger_path(project_root: &Path, id: &str) -> PathBuf {
        Self::state_dir(project_root, id).join("budget-ledger.jsonl")
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
pub fn render_session_context(project_root: &Path, state: &TeamSessionState, role: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Team session: {}\n\n", state.config.name));
    out.push_str(&format!("**Objective:** {}\n\n", state.config.objective));

    if let Some(prompt) = state.config.role_prompts.get(role) {
        if !prompt.trim().is_empty() {
            out.push_str(&format!("## Your role: {role}\n\n{prompt}\n\n"));
        }
    }

    if let Some(budget) = &state.config.budget {
        let ledger_path = TeamSessionState::budget_ledger_path(project_root, &state.id);
        let spent = ta_policy::business_budget::ledger_running_total(&ledger_path);
        let pct = if budget.total > 0.0 {
            spent / budget.total * 100.0
        } else {
            0.0
        };
        out.push_str(&format!(
            "**Budget ({metric}):** {spent:.2} / {total:.2} spent ({pct:.1}%)",
            metric = budget.metric,
            total = budget.total,
        ));
        if let Some(cap) = budget.per_action_max_pct {
            out.push_str(&format!(", hard per-action cap {cap:.1}%"));
        }
        if let Some(soft) = budget.soft_threshold_pct {
            out.push_str(&format!(", soft escalation threshold {soft:.1}%"));
        }
        out.push_str(".\n\n");
        out.push_str(&format!(
            "When performing a budgeted action, call `ta_human_verify` with a `budget` \
             param: `{{\"metric\": \"{metric}\", \"total\": {total}, \
             \"per_action_max_pct\": {cap:?}, \"soft_threshold_pct\": {soft:?}}}`, the \
             action's amount, and `ledger_path: \".ta/team-sessions/{id}/budget-ledger.jsonl\"`.\n\n",
            metric = budget.metric,
            total = budget.total,
            cap = budget.per_action_max_pct,
            soft = budget.soft_threshold_pct,
            id = state.id,
        ));
    }

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
    role: &str,
) -> io::Result<PathBuf> {
    let dir = TeamSessionState::state_dir(project_root, &state.id);
    std::fs::create_dir_all(&dir)?;
    let path = session_context_path(project_root, &state.id, stage_name);
    std::fs::write(&path, render_session_context(project_root, state, role))?;
    Ok(path)
}

/// Builds the `ta run` argument list for firing a role's goal-run,
/// mirroring `apps/ta-cli/src/commands/intake.rs::execute_routed_goal`'s
/// command construction. Returns a plain `Vec<String>` (not a `Command`)
/// so the argument logic is unit-testable without spawning a process.
///
/// `label` is a free-text title component (a rotation stage's name, or
/// e.g. `"wake-on-demand"` for `wake_listener.rs`'s launches) -- this
/// function only needs a string for the title, not a full stage struct, so
/// it's callable outside `team_session.rs`'s rotation state machine too
/// (v0.17.11.10).
///
/// Origin and chat mode (H7/H9, red-team CR-01): when the role's persona
/// (`.ta/personas/<name>.toml`, loaded from `project_root`) declares
/// `chat_mode = true`, or the role or persona declares an `origin`, the
/// arguments carry `--chat-mode` and/or `--origin <value>` (see
/// `ta_goal::origin::resolve_launch_origin`). An origin that is never
/// auto-approved (`cos`, `chat`) always comes with `--chat-mode`. Fails
/// closed: a persona that cannot be loaded, or an invalid or conflicting
/// origin, returns an error and the role is not launched, because the
/// daemon could otherwise launch a CoS with a full tool surface.
pub fn build_ta_run_args(
    project_root: &Path,
    state: &TeamSessionState,
    label: &str,
    role: &str,
    team_config: &TeamConfig,
    context_path: &Path,
    workflow_tag: Option<&str>,
) -> Result<Vec<String>, String> {
    let title = format!("{}: {} ({})", state.config.name, label, role);
    let mut args = vec![
        "run".to_string(),
        title,
        "--headless".to_string(),
        "--objective-file".to_string(),
        context_path.to_string_lossy().to_string(),
        "--team".to_string(),
        role.to_string(),
        // v0.17.11.8: lets `ta run` deliver `state.config.whiteboard_token`
        // into this role's staging workspace (see apps/ta-cli/src/commands/
        // run.rs's `write_whiteboard_session_file`) so the agent's
        // `ta_whiteboard_*` MCP tools can authenticate.
        "--team-session-id".to_string(),
        state.id.clone(),
    ];

    if let Some(member) = team_config.find_by_role(&TeamRole::new(role)) {
        args.push("--security".to_string());
        args.push(member.security.to_string());
        if let Some(persona) = &member.persona {
            args.push("--persona".to_string());
            args.push(persona.clone());
        }
        let (persona_chat_mode, persona_origin) = match &member.persona {
            Some(name) => {
                let p = ta_goal::PersonaConfig::load(project_root, name).map_err(|e| {
                    format!(
                        "cannot launch role '{}': its persona '{}' could not be loaded to \
                         check chat mode and origin ({}). Fix .ta/personas/{}.toml or the \
                         role's persona in .ta/team.toml.",
                        role, name, e, name
                    )
                })?;
                (p.capabilities.chat_mode, p.capabilities.origin)
            }
            None => (false, None),
        };
        let launch = ta_goal::origin::resolve_launch_origin(
            persona_chat_mode,
            persona_origin.as_deref(),
            member.origin.as_deref(),
        )
        .map_err(|e| {
            format!(
                "cannot launch role '{}': {}. Check the role in .ta/team.toml and its persona.",
                role, e
            )
        })?;
        if launch.chat_mode {
            args.push("--chat-mode".to_string());
        }
        if let Some(origin) = launch.origin {
            args.push("--origin".to_string());
            args.push(origin);
        }
        // `team.toml`'s `member.agent_id`/`model_tier` name a MODEL (e.g.
        // "claude-opus-5"), not a framework -- `ta run`'s `--agent` flag
        // means framework (claude-code, codex, a custom manifest) and
        // resolves the value against `AgentFrameworkManifest`. Passing a
        // model id there always failed to resolve, silently fell back to
        // the hardcoded "claude-code" default, and discarded the
        // originally-requested model entirely -- every team-session/
        // wake-on-demand launch silently ran whatever model `claude`'s own
        // local default happened to be, never what team.toml configured.
        // Found live, 2026-10-01, investigating a dogfood test failure.
        //
        // Fix: route the resolved model through `--model` (forwarded to the
        // underlying agent binary once the framework is already chosen --
        // see run.rs's `execute()`), and leave `--agent` unset so `ta run`'s
        // own framework-resolution chain (persona binding → workflow.toml →
        // daemon.toml → "claude-code") decides the framework, same as any
        // other goal. This also fixes a second latent bug for free: passing
        // the model id into `--agent` previously won tier 1 of that
        // resolution chain and silently overrode any persona-level
        // framework binding at tier 2.
        let resolved = team_config.resolve_agent_id(member);
        if resolved.eq_ignore_ascii_case("auto") {
            // `agent_id = "auto"` (`ta team assign <role> auto`) is a
            // documented sentinel, not a real model name: it hands the
            // choice to the supervisor's recommendation via `ta run`'s
            // dedicated `--agent auto` tier (see resolve_effective_agent_full
            // / recommend_agent in run.rs), which --model has no equivalent
            // for. Routing it through --model instead (as the general case
            // below does) would forward a literal `--model auto` to the
            // `claude` binary, silently breaking supervisor auto-pick for
            // any team member assigned "auto" -- found in code review of
            // this very fix, 2026-10-02.
            args.push("--agent".to_string());
            args.push("auto".to_string());
        } else {
            args.push("--model".to_string());
            args.push(resolved.to_string());
        }
    }
    // A role with no `.ta/team.toml` assignment yet falls through to
    // `ta run`'s own default resolution chain (workflow.toml, daemon.toml,
    // "claude-code") rather than failing the cycle outright.

    // Generic cost-classification tag (v0.17.x cost-experiment framework).
    // Note: the CLI flag is `--workflow-tag`, not `--workflow` -- `ta run`
    // already has a `--workflow` flag selecting the *execution engine*
    // (single-agent/serial-phases/swarm, see apps/ta-cli/src/commands/
    // run.rs's `WorkflowKind`), so this opaque classification tag needed a
    // distinct name to avoid colliding with that existing, load-bearing
    // flag.
    if let Some(tag) = workflow_tag {
        args.push("--workflow-tag".to_string());
        args.push(tag.to_string());
    }

    Ok(args)
}

// ─── macOS code signing (self-healing, best-effort) ─────────────────────────
//
// Both this module and `wake_listener.rs` spawn `ta_bin` (the main `ta`
// binary, via `build_ta_run_args`' output) directly, once per rotation
// cycle or wake-on-demand poll -- a much higher-frequency spawn than the
// daemon's own one-time startup. `apps/ta-cli/src/commands/daemon.rs` has
// the matching fix for `ta-daemon` itself (`ensure_stable_codesign`); this
// is the same fix, duplicated rather than shared across the `ta-cli`/
// `ta-daemon` crate boundary, for whichever binary `ta_bin` resolves to
// here (`ta`, not `ta-daemon`). See that file's module comment for the
// full rationale: a bare `cargo build` produces a fresh, unsigned binary
// every time, so without this, every single poll cycle could get a fresh
// Keychain prompt, not just every rebuild.
//
// Best-effort and bounded: never blocks a cycle on a signing failure or a
// slow/locked Keychain -- each attempt is capped at a short timeout.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_stable_codesign(binary_path: &Path) {
    const IDENTIFIER: &str = "com.trustedautonomy.ta";
    const CODESIGN_TIMEOUT: Duration = Duration::from_secs(5);

    let identity = std::env::var("TA_CODESIGN_IDENTITY")
        .unwrap_or_else(|_| "Trusted Autonomy Local Dev".to_string());

    if run_codesign_with_timeout(binary_path, &identity, IDENTIFIER, CODESIGN_TIMEOUT) {
        return;
    }
    let _ = run_codesign_with_timeout(binary_path, "-", IDENTIFIER, CODESIGN_TIMEOUT);
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn ensure_stable_codesign(_binary_path: &Path) {}

#[cfg(target_os = "macos")]
fn run_codesign_with_timeout(
    binary_path: &Path,
    identity: &str,
    identifier: &str,
    timeout: Duration,
) -> bool {
    let mut child = match std::process::Command::new("codesign")
        .arg("--force")
        .arg("--sign")
        .arg(identity)
        .arg("--identifier")
        .arg(identifier)
        .arg(binary_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
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
            tracing::info!(
                session_id = %id,
                "team session restarted via `ta team-session restart` -- failure tracker reset, \
                 rotation resuming"
            );
        } else {
            // Deliberately debug, not silent: a Suspended session with no
            // restart signal polls forever doing nothing else, and this is
            // the only place that fact is ever observable short of
            // `ta team-session status` -- found live, 2026-10-02, when a
            // user watching `RUST_LOG=debug` across a daemon restart saw
            // zero team_session/supervisor log lines at all for a
            // Suspended session and couldn't tell whether the supervisor
            // was alive, polling, or never started.
            tracing::debug!(
                session_id = %id,
                "team session is Suspended, no restart-signal present -- waiting \
                 (`ta team-session restart {id}` to clear it)"
            );
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
    let context_path = write_session_context(project_root, &state, &stage.name, &role)?;
    // Round-robin rotation launches are not yet classified with a workflow
    // tag -- only wake-on-demand listeners (`wake_listener.rs`) carry one
    // today, via their own `workflow_tag` config.
    let args = build_ta_run_args(
        project_root,
        &state,
        &stage.name,
        &role,
        &team_config,
        &context_path,
        None,
    )
    .map_err(|e| {
        tracing::error!(session_id = %id, role = %role, error = %e, "refusing to launch team role");
        io::Error::other(e)
    })?;

    ensure_stable_codesign(ta_bin);
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
        other => {
            // Found live, 2026-10-02: this branch used to discard the
            // failed subprocess's exit status and stdout/stderr (or the
            // spawn error, if `ta_bin` couldn't even be launched) entirely,
            // just incrementing restart_count -- so when a session hit
            // Suspended after 5 failures, there was no way, anywhere
            // (daemon.log included), to find out what the actual goal-run
            // failure even was. Log it at warn, truncated to a reasonable
            // preview -- a full agent transcript can be large, and this is
            // a diagnostic breadcrumb, not the artifact of record (that's
            // whatever `ta run` itself already wrote to .ta/goals/).
            const PREVIEW_LEN: usize = 2000;
            match &other {
                Ok(out) => {
                    let stdout_preview = String::from_utf8_lossy(&out.stdout);
                    let stderr_preview = String::from_utf8_lossy(&out.stderr);
                    tracing::warn!(
                        session_id = %id,
                        role = %role,
                        stage = %stage.name,
                        exit_code = ?out.status.code(),
                        stdout = %crate::watchdog::truncate_preview(&stdout_preview, PREVIEW_LEN),
                        stderr = %crate::watchdog::truncate_preview(&stderr_preview, PREVIEW_LEN),
                        "team session goal-run failed"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %id,
                        role = %role,
                        stage = %stage.name,
                        error = %e,
                        ta_bin = %ta_bin.display(),
                        "team session goal-run failed to spawn"
                    );
                }
            }
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
/// How often the discovery loop (below) re-scans for team sessions created
/// after this daemon process itself started. Matches `watchdog.rs`'s own
/// `interval_secs` convention/default (30s) for consistency -- short enough
/// that `ta team-session start` run against an already-running daemon gets
/// picked up promptly, not "on next restart" as it silently required before
/// this fix (found live, 2026-10-02: a session created after daemon startup
/// never got a supervisor spawned for it at all -- not stuck, never started
/// -- because `start()`'s one-time enumeration at process-launch was the
/// *only* place new sessions were ever discovered).
const SESSION_DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);

/// Spawns `run_team_session` for `id` if its persisted status isn't already
/// `Stopped`. Shared by `start()`'s initial one-time scan and the ongoing
/// discovery loop so both apply the identical "skip Stopped" rule.
fn spawn_if_not_stopped(
    project_root: &Path,
    id: &str,
    ta_bin: &Path,
    shutdown: &Arc<tokio::sync::Notify>,
) -> Option<tokio::task::JoinHandle<()>> {
    let Ok(Some(state)) = TeamSessionState::load(project_root, id) else {
        return None;
    };
    if state.status == TeamSessionStatus::Stopped {
        return None;
    }
    let pr = project_root.to_path_buf();
    let sid = id.to_string();
    let bin = ta_bin.to_path_buf();
    let sd = shutdown.clone();
    Some(tokio::spawn(async move {
        run_team_session(pr, sid, bin, sd).await;
    }))
}

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
    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    for id in TeamSessionState::list_ids(&project_root) {
        known.insert(id.clone());
        if let Some(h) = spawn_if_not_stopped(&project_root, &id, &ta_bin, &shutdown) {
            handles.push(h);
        }
    }

    // Ongoing discovery: re-scan every SESSION_DISCOVERY_INTERVAL for
    // session IDs not seen before, and spawn a supervisor for each one --
    // the fix for the gap described in SESSION_DISCOVERY_INTERVAL's doc
    // comment above. Already-known IDs are skipped outright (even a
    // Stopped-then-restarted one keeps its original supervisor task, which
    // already polls for exactly that transition -- see run_team_session's
    // Suspended/Paused handling), so this never double-spawns.
    {
        let pr = project_root.clone();
        let bin = ta_bin.clone();
        let sd = shutdown.clone();
        handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(SESSION_DISCOVERY_INTERVAL) => {}
                    _ = sd.notified() => return,
                }
                for id in TeamSessionState::list_ids(&pr) {
                    if known.contains(&id) {
                        continue;
                    }
                    tracing::info!(
                        session = %id,
                        "team_session: discovered session created after daemon startup, \
                         spawning its supervisor now"
                    );
                    known.insert(id.clone());
                    spawn_if_not_stopped(&pr, &id, &bin, &sd);
                }
            }
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
            budget: None,
            role_prompts: std::collections::HashMap::new(),
            whiteboard_token: None,
            whiteboard_token_expires_at: None,
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
    fn list_ids_sees_a_session_created_after_an_earlier_snapshot() {
        // Validates the premise `start()`'s discovery loop relies on: a
        // session created on disk after an earlier `list_ids()` call shows
        // up in a later one. Doesn't spawn the real loop (which launches a
        // real `ta` subprocess via `run_team_session` -- not something to
        // exercise in a unit test), just the on-disk discovery primitive
        // the fix for the "daemon never picks up a session created after
        // its own startup" bug (found live, 2026-10-02) depends on.
        let dir = tempfile::tempdir().unwrap();
        let mut first =
            TeamSessionState::new("sess-a".to_string(), sample_config(), sample_stages());
        first.save(dir.path()).unwrap();

        let snapshot_one: std::collections::HashSet<String> =
            TeamSessionState::list_ids(dir.path()).into_iter().collect();
        assert_eq!(snapshot_one, ["sess-a".to_string()].into());

        // Simulates `ta team-session start` creating a new session while
        // the daemon (holding `snapshot_one`) is already running.
        let mut second =
            TeamSessionState::new("sess-b".to_string(), sample_config(), sample_stages());
        second.save(dir.path()).unwrap();

        let snapshot_two: std::collections::HashSet<String> =
            TeamSessionState::list_ids(dir.path()).into_iter().collect();
        let newly_discovered: Vec<&String> = snapshot_two.difference(&snapshot_one).collect();
        assert_eq!(newly_discovered, vec![&"sess-b".to_string()]);
    }

    #[test]
    fn spawn_if_not_stopped_skips_a_stopped_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut state =
            TeamSessionState::new("sess-a".to_string(), sample_config(), sample_stages());
        state.status = TeamSessionStatus::Stopped;
        state.save(dir.path()).unwrap();

        let shutdown = Arc::new(tokio::sync::Notify::new());
        let handle =
            spawn_if_not_stopped(dir.path(), "sess-a", Path::new("/usr/bin/true"), &shutdown);
        assert!(
            handle.is_none(),
            "a Stopped session must never get a supervisor spawned for it, \
             whether at daemon startup or by the ongoing discovery loop"
        );
    }

    #[test]
    fn spawn_if_not_stopped_skips_a_nonexistent_session() {
        let dir = tempfile::tempdir().unwrap();
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let handle = spawn_if_not_stopped(
            dir.path(),
            "no-such-session",
            Path::new("/usr/bin/true"),
            &shutdown,
        );
        assert!(handle.is_none());
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
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let rendered = render_session_context(dir.path(), &state, "analyst");
        assert!(rendered.contains("No prior role findings yet"));
        assert!(rendered.contains("trading-desk"));
    }

    #[test]
    fn render_context_includes_budget_and_running_ledger_total() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = sample_config();
        config.budget = Some(BudgetGuardrails {
            metric: "usd".to_string(),
            total: 1000.0,
            per_action_max_pct: Some(10.0),
            soft_threshold_pct: Some(80.0),
            objective: Some("generate income > 2x within 6 months after fees".to_string()),
        });
        let state = TeamSessionState::new("sess-1".to_string(), config, sample_stages());

        let ledger_path = TeamSessionState::budget_ledger_path(dir.path(), "sess-1");
        ta_policy::business_budget::record_ledger_spend(&ledger_path, "buy AAPL", 250.0).unwrap();

        let rendered = render_session_context(dir.path(), &state, "analyst");
        assert!(rendered.contains("Budget (usd)"), "got: {rendered}");
        assert!(rendered.contains("250.00 / 1000.00"), "got: {rendered}");
        assert!(rendered.contains("25.0%"), "got: {rendered}");
        assert!(rendered.contains("budget-ledger.jsonl"), "got: {rendered}");
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
        let dir = tempfile::tempdir().unwrap();
        let rendered = render_session_context(dir.path(), &state, "trader");
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
    fn render_context_includes_the_firing_roles_own_prompt() {
        // Regression test (Phase 1 live testing of ta-virtual-team, 2026-09):
        // a role's `prompt:` text from the workflow YAML was parsed by
        // `ta-workflow`'s `RoleDefinition` but never reached the agent:
        // only the session-level `objective` and prior findings did. A live
        // team-session run silently ignored a role's own instructions.
        let mut config = sample_config();
        config.role_prompts.insert(
            "analyst".to_string(),
            "You are a market analyst. Review market data and report findings.".to_string(),
        );
        config.role_prompts.insert(
            "trader".to_string(),
            "You are a trader. Execute the strategist's decisions.".to_string(),
        );
        let state = TeamSessionState::new("sess-1".to_string(), config, sample_stages());
        let dir = tempfile::tempdir().unwrap();

        let analyst_context = render_session_context(dir.path(), &state, "analyst");
        assert!(
            analyst_context.contains("You are a market analyst."),
            "got: {analyst_context}"
        );
        assert!(
            !analyst_context.contains("Execute the strategist's decisions."),
            "analyst's context must not contain the trader's prompt"
        );

        let trader_context = render_session_context(dir.path(), &state, "trader");
        assert!(
            trader_context.contains("You are a trader."),
            "got: {trader_context}"
        );
        assert!(
            !trader_context.contains("market analyst"),
            "trader's context must not contain the analyst's prompt"
        );
    }

    #[test]
    fn render_context_omits_role_section_when_no_prompt_configured() {
        // A role with no `prompt:` in the workflow YAML (or an older
        // state.json predating this field) must not render an empty
        // "## Your role" heading.
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let dir = tempfile::tempdir().unwrap();
        let rendered = render_session_context(dir.path(), &state, "analyst");
        assert!(!rendered.contains("## Your role"), "got: {rendered}");
    }

    #[test]
    fn write_session_context_creates_the_expected_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let path = write_session_context(dir.path(), &state, "analyze", "analyst").unwrap();
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
    fn build_args_with_assigned_role_includes_security_persona_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "analyst").unwrap();

        let mut team_config = TeamConfig::default();
        team_config.assign(
            TeamRole::new("analyst"),
            "claude-sonnet-4-6".to_string(),
            ta_session::workflow_session::AdvisorSecurity::Auto,
            Some("careful-analyst".to_string()),
        );
        write_test_persona(dir.path(), "careful-analyst", "");

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "analyst",
            &team_config,
            &context_path,
            None,
        )
        .unwrap();

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
        // Model id goes through --model, not --agent: --agent means
        // "framework" to `ta run` (claude-code/codex/a manifest), not
        // "which model" -- passing a model id there always failed to
        // resolve and silently fell back to a hardcoded default (the bug
        // this test now guards against regressing).
        assert!(!args.contains(&"--agent".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn build_args_model_tier_overrides_agent_id_when_resolvable() {
        // Proves model_tier actually affects which model launches a role
        // (v0.17.11.6), not just that the field round-trips through TOML.
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "analyst").unwrap();

        let mut team_config = TeamConfig::default();
        team_config.assign(
            TeamRole::new("analyst"),
            "claude-sonnet-4-6".to_string(),
            ta_session::workflow_session::AdvisorSecurity::Auto,
            None,
        );
        team_config
            .model_tiers
            .insert("highest".to_string(), "claude-opus-5".to_string());
        team_config.members[0].model_tier = Some("highest".to_string());

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "analyst",
            &team_config,
            &context_path,
            None,
        )
        .unwrap();

        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"claude-opus-5".to_string()));
        assert!(!args.contains(&"claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn build_args_agent_id_literal_auto_routes_through_agent_flag_not_model() {
        // `ta team assign <role> auto` (documented in USAGE.md's "agent =
        // auto -- supervisor auto-pick") is a sentinel, not a real model
        // name -- it must keep going through `--agent auto` (which `ta
        // run` gives dedicated supervisor-recommendation handling), not
        // `--model auto`, which would forward a literal, meaningless
        // "auto" straight to the `claude` binary. Found in code review of
        // the --model fix itself, 2026-10-02.
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "analyst").unwrap();

        let mut team_config = TeamConfig::default();
        team_config.assign(
            TeamRole::new("analyst"),
            "auto".to_string(),
            ta_session::workflow_session::AdvisorSecurity::Auto,
            None,
        );

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "analyst",
            &team_config,
            &context_path,
            None,
        )
        .unwrap();

        assert!(args.contains(&"--agent".to_string()));
        assert!(args.contains(&"auto".to_string()));
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn build_args_model_tier_falls_back_to_agent_id_when_tier_unresolvable() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "analyst").unwrap();

        let mut team_config = TeamConfig::default();
        team_config.assign(
            TeamRole::new("analyst"),
            "claude-sonnet-4-6".to_string(),
            ta_session::workflow_session::AdvisorSecurity::Auto,
            None,
        );
        // model_tier set, but not declared in [model_tiers] — should not
        // block launching the role, just fall back to agent_id.
        team_config.members[0].model_tier = Some("nonexistent-tier".to_string());

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "analyst",
            &team_config,
            &context_path,
            None,
        )
        .unwrap();

        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn build_args_with_unassigned_role_omits_security_and_persona() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "analyst").unwrap();

        let team_config = TeamConfig::default(); // no members assigned

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "analyst",
            &team_config,
            &context_path,
            None,
        )
        .unwrap();

        assert!(!args.contains(&"--security".to_string()));
        assert!(!args.contains(&"--persona".to_string()));
        assert!(!args.contains(&"--agent".to_string()));
        assert!(!args.contains(&"--model".to_string()));
        // Still fires the goal — just without an assignment-derived override.
        assert!(args.contains(&"--team".to_string()));
    }

    #[test]
    fn build_args_always_includes_team_session_id() {
        // v0.17.11.8: every team-session role launch passes its own
        // session id so `ta run` can look up `whiteboard_token` from the
        // real project root and deliver it into the role's staging copy.
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-42".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "analyst").unwrap();
        let team_config = TeamConfig::default();

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "analyst",
            &team_config,
            &context_path,
            None,
        )
        .unwrap();

        let flag_idx = args
            .iter()
            .position(|a| a == "--team-session-id")
            .expect("--team-session-id flag missing");
        assert_eq!(args[flag_idx + 1], "sess-42");
    }

    #[test]
    fn build_ta_run_args_appends_workflow_tag_flag_when_listener_has_a_tag() {
        // Generic cost-classification tag (v0.17.x cost-experiment
        // framework): `--workflow-tag`, not `--workflow` -- the latter is
        // already `ta run`'s execution-engine selector.
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "specialist").unwrap();
        let team_config = TeamConfig::default();

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "specialist",
            &team_config,
            &context_path,
            Some("brain-maintenance"),
        )
        .unwrap();

        let flag_pos = args
            .iter()
            .position(|a| a == "--workflow-tag")
            .expect("--workflow-tag flag present");
        assert_eq!(args[flag_pos + 1], "brain-maintenance");
    }

    #[test]
    fn build_ta_run_args_omits_workflow_tag_flag_when_no_tag() {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let stage = &state.stages[0];
        let context_path =
            write_session_context(dir.path(), &state, &stage.name, "researcher").unwrap();
        let team_config = TeamConfig::default();

        let args = build_ta_run_args(
            dir.path(),
            &state,
            &stage.name,
            "researcher",
            &team_config,
            &context_path,
            None,
        )
        .unwrap();

        assert!(!args.contains(&"--workflow-tag".to_string()));
    }

    // ── CR-01: chat mode and origin for CoS-style launches ────────────────

    fn write_test_persona(project: &Path, name: &str, capabilities_toml: &str) {
        let dir = project.join(".ta").join("personas");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.toml", name)),
            format!(
                "[persona]\nname = \"{}\"\n\n[capabilities]\n{}\n",
                name, capabilities_toml
            ),
        )
        .unwrap();
    }

    /// Builds args for `role` assigned to `persona` (and optional role
    /// origin), in a temp project.
    fn args_for_role(
        persona: Option<(&str, &str)>,
        role_origin: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let context_path =
            write_session_context(dir.path(), &state, "intake", "chief-of-staff").unwrap();
        if let Some((name, caps)) = persona {
            write_test_persona(dir.path(), name, caps);
        }
        let mut team_config = TeamConfig::default();
        team_config.assign(
            TeamRole::new("chief-of-staff"),
            "claude-opus-5".to_string(),
            ta_session::workflow_session::AdvisorSecurity::Auto,
            persona.map(|(n, _)| n.to_string()),
        );
        team_config.members[0].origin = role_origin.map(str::to_string);
        build_ta_run_args(
            dir.path(),
            &state,
            "intake",
            "chief-of-staff",
            &team_config,
            &context_path,
            None,
        )
    }

    fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .map(|i| args[i + 1].as_str())
    }

    #[test]
    fn chat_mode_persona_with_cos_origin_launches_with_chat_mode_and_origin_cos() {
        let args = args_for_role(
            Some(("chief-of-staff", "chat_mode = true\norigin = \"cos\"")),
            None,
        )
        .unwrap();
        assert!(args.contains(&"--chat-mode".to_string()), "{:?}", args);
        assert_eq!(flag_value(&args, "--origin"), Some("cos"));
        assert_eq!(flag_value(&args, "--persona"), Some("chief-of-staff"));
    }

    #[test]
    fn chat_mode_persona_without_origin_gets_origin_chat() {
        let args = args_for_role(Some(("chief-of-staff", "chat_mode = true")), None).unwrap();
        assert!(args.contains(&"--chat-mode".to_string()));
        assert_eq!(flag_value(&args, "--origin"), Some("chat"));
    }

    #[test]
    fn cos_role_origin_forces_chat_mode_even_for_a_plain_persona() {
        let args = args_for_role(Some(("chief-of-staff", "")), Some("cos")).unwrap();
        assert!(args.contains(&"--chat-mode".to_string()), "{:?}", args);
        assert_eq!(flag_value(&args, "--origin"), Some("cos"));
    }

    #[test]
    fn cos_role_origin_with_no_persona_still_gets_chat_mode() {
        let args = args_for_role(None, Some("cos")).unwrap();
        assert!(args.contains(&"--chat-mode".to_string()));
        assert_eq!(flag_value(&args, "--origin"), Some("cos"));
    }

    #[test]
    fn non_chat_persona_gets_neither_chat_mode_nor_origin() {
        let args =
            args_for_role(Some(("implementer", "allowed_tools = [\"Bash(*)\"]")), None).unwrap();
        assert!(!args.contains(&"--chat-mode".to_string()));
        assert!(!args.contains(&"--origin".to_string()));
    }

    #[test]
    fn unloadable_persona_or_bad_origin_refuses_to_build_args() {
        // Persona named in team.toml but missing on disk.
        let dir = tempfile::tempdir().unwrap();
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let context_path = write_session_context(dir.path(), &state, "intake", "cos").unwrap();
        let mut team_config = TeamConfig::default();
        team_config.assign(
            TeamRole::new("cos"),
            "claude-opus-5".to_string(),
            ta_session::workflow_session::AdvisorSecurity::Auto,
            Some("missing-persona".to_string()),
        );
        let err = build_ta_run_args(
            dir.path(),
            &state,
            "intake",
            "cos",
            &team_config,
            &context_path,
            None,
        )
        .unwrap_err();
        assert!(err.contains("missing-persona"), "{}", err);

        // Invalid origin, conflicting origins, chat mode with an
        // auto-approvable origin.
        assert!(args_for_role(Some(("chief-of-staff", "origin = \"COS\"")), None).is_err());
        assert!(args_for_role(
            Some(("chief-of-staff", "chat_mode = true\norigin = \"cos\"")),
            Some("chat")
        )
        .is_err());
        assert!(args_for_role(
            Some(("chief-of-staff", "chat_mode = true\norigin = \"poller\"")),
            None
        )
        .is_err());
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

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_suspended_then_restart_signal() {
        // `resume-signal` only clears Paused (see the test above);
        // Suspended (reached via the backoff/crash-recovery path, not a
        // human pause) needs restart-signal, written by `ta team-session
        // restart`, not `resume`. Found live, 2026-10-02: there was no CLI
        // command for this at all before -- USAGE.md told users to
        // manually `touch .ta/team-sessions/<name>/restart-signal`.
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

        // A resume-signal must NOT clear Suspended -- confirms the two
        // statuses really do require distinct signals, not just that
        // restart-signal happens to work.
        signal_resume(dir.path(), "sess-1").unwrap();
        let (outcome_resume_attempt, tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, tracker).unwrap();
        assert_eq!(
            outcome_resume_attempt,
            CycleOutcome::Suspended,
            "resume-signal must be a no-op against a Suspended session"
        );

        signal_restart(dir.path(), "sess-1").unwrap();
        let new_ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\necho ok\nexit 0\n");
        let (outcome_after_restart, _tracker) =
            run_one_cycle(dir.path(), "sess-1", &new_ta_bin, tracker).unwrap();
        assert_eq!(outcome_after_restart, CycleOutcome::Advanced);

        let restarted_state = TeamSessionState::load(dir.path(), "sess-1")
            .unwrap()
            .unwrap();
        assert_eq!(restarted_state.status, TeamSessionStatus::Active);
    }
}
