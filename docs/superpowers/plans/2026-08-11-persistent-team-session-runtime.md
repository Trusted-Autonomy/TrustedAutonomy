# Persistent Team Session Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a team a persistent execution context that survives across multiple goal-runs (plan phase v0.17.5.1), by adding a new `ta-daemon` subsystem (`team_session.rs`) that supervises a long-running "team session" — a workflow YAML + `.ta/team.toml` binding — restarting a new `ta run` goal for the next role/stage in sequence, carrying prior roles' findings forward as context, instead of every trigger firing a brand-new goal from zero.

**Architecture:** Mirror the two existing `ta-daemon` supervision precedents directly: `connector_supervisor.rs`'s fault-isolated, file-protocol-driven, backoff/suspend subprocess supervision, and `watchdog.rs`'s background tokio task shape with `tokio::select!`-based shutdown. A `TeamSession`'s persistent state lives at `.ta/team-sessions/<id>/state.json`; supervisor status and pause/resume/stop control signals live as sibling files in the same directory, exactly like `.ta/connectors/<name>/`. Context carry-forward reuses `ta-session::advisor_agent`'s proven pattern of "render prior findings to markdown, write to a file, pass `--objective-file <path>` to the next `ta run`" — the same mechanism `apps/ta-cli/src/commands/intake.rs::execute_routed_goal` already uses to fire a subprocess goal-run. **Key design decision**: workflow-YAML stage/role resolution (which needs the `ta-workflow` crate) happens once in the CLI at `ta team-session start` time and is written into `state.json` as an already-resolved `Vec<TeamSessionStageConfig>` — `ta-daemon` does **not** gain a new `ta-workflow` dependency; it only ever reads the pre-resolved list back out of `state.json`. Per-role agent/security/persona assignment (`.ta/team.toml` via `ta_session::team::TeamConfig`) is re-resolved fresh on every cycle inside the daemon loop (not baked into `state.json`), since `ta-session` is already a `ta-daemon` dependency and re-assignments should take effect on the next cycle without restarting the session.

**Tech Stack:** Rust workspace. `ta-daemon` (tokio, serde, uuid, chrono — all already dependencies). `ta-session::team::TeamConfig` / `ta_session::agent_action::{TeamRole, TeamMember}` / `ta_session::workflow_session::AdvisorSecurity` for role resolution (already a `ta-daemon` dependency). `ta-workflow::WorkflowDefinition` for stage/role parsing (CLI-side only, `apps/ta-cli` already depends on it). Plain synchronous `#[test]` + `tempfile::tempdir()` for all logic tests, matching `connector_supervisor.rs`/`watchdog.rs`/`advisor_agent.rs`'s existing test style (this crate has no `tokio::test` dev-dependency configured and this plan does not add one — async loop code stays a thin wrapper around synchronously-testable pure functions).

## Global Constraints

- Nix-wrapped build/test/lint/fmt only: `./dev cargo build --workspace`, `./dev cargo test --workspace`, `./dev cargo clippy --workspace --all-targets -- -D warnings`, `./dev cargo fmt --all -- --check` — all four must pass before each commit, per project CLAUDE.md.
- Never manually bump `Cargo.toml`/`CLAUDE.md` version — `ta draft apply --phase` does that automatically.
- Backoff/suspend constants must match `connector_supervisor.rs` exactly: 1s→2s→4s...→60s cap, `Suspended` after 5 failures within a 300s (5 min) rolling window.
- Every new literal `.join(".ta").join("<name>")` path segment must be registered in `crates/ta-workspace/src/partitioning.rs`'s `LOCAL_TA_PATHS` or `SHARED_TA_PATHS`, or the CI-enforced test `all_direct_dot_ta_join_literals_are_registered_or_allowlisted` fails the build. `team-sessions/` is local (per-workspace runtime state, like `goals/`/`staging/`), not shared.
- No new `---` horizontal rules inside PLAN.md phase content — only mark existing `[ ]` items `[x]` and add the version-line note.
- Mark PLAN.md item checkboxes `[x]` immediately as each task's code is written and compiles — do not wait until the end.
- Use `tempfile::tempdir()` for all test fixtures needing filesystem access.
- Commit in logical working units after each task, on the current TA staging workspace (no manual git branch/PR steps — TA's overlay-diff flow handles that after this session exits).

---

## File Structure

- **Create** `crates/ta-daemon/src/team_session.rs` — all core types (`TeamSessionConfig`, `TeamSessionStageConfig`, `RoleFinding`, `TeamSessionStatus`, `TeamSessionState`), persistence (load/save state.json), supervisor status file read/write, control-signal file read/write, the pure backoff/suspend `FailureTracker`, the context-markdown renderer/writer, the `ta run` argument builder, the synchronous `run_one_cycle` function, and the async `run_team_session`/`start` entry points. One file — this mirrors `connector_supervisor.rs`'s and `watchdog.rs`'s existing "one file per daemon subsystem" convention; do not split further.
- **Modify** `crates/ta-daemon/src/main.rs` — add `pub mod team_session;` near the existing `pub mod connector_supervisor;`/`pub mod watchdog;` lines, and wire `team_session::start(...)` into both the API-mode and MCP-mode daemon startup blocks, immediately after the existing `watchdog::run_watchdog(...)` spawns.
- **Modify** `crates/ta-workspace/src/partitioning.rs` — add `"team-sessions/"` to `LOCAL_TA_PATHS`.
- **Create** `apps/ta-cli/src/commands/team_session.rs` — `TeamSessionCommands` enum (`Start`/`Pause`/`Resume`/`Stop`/`Status`) and `execute()`, mirroring `apps/ta-cli/src/commands/connector.rs`'s shape. This is also where the workflow-YAML stage/role resolution happens (`ta-workflow::WorkflowDefinition::from_file` + `.stage_order()`), since only `apps/ta-cli` (not `ta-daemon`) depends on `ta-workflow`.
- **Modify** `apps/ta-cli/src/commands/mod.rs` — add `pub mod team_session;`.
- **Modify** `apps/ta-cli/src/main.rs` — add a `Commands::TeamSession { command }` variant (hidden legacy-noun form, matching `Commands::Connector`) and its dispatch arm.
- **Modify** `apps/ta-cli/src/commands/verb.rs` — add a `NounEntry` for `team-session` to `NOUN_TABLE` so `ta create/show/update/remove team-session ...` works as the canonical form alongside the legacy `ta team-session ...`.
- **Modify** `docs/USAGE.md` — new "Persistent Team Sessions" section, placed directly after the existing "Connector Supervision" section (~line 15434), documenting `ta team-session start/pause/resume/stop/status`.
- **Modify** `PLAN.md` — check off v0.17.5.1's 6 items as each is completed.

---

### Task 1: Core state types, persistence, and the CI path-registration guard

**Files:**
- Create: `crates/ta-daemon/src/team_session.rs`
- Modify: `crates/ta-daemon/src/main.rs:39` area (add `pub mod team_session;` — no wiring yet, that's Task 7)
- Modify: `crates/ta-workspace/src/partitioning.rs`
- Test: inline `#[cfg(test)] mod tests` in `team_session.rs`

**Interfaces:**
- Produces: `TeamSessionStatus` enum, `TeamSessionStageConfig`, `RoleFinding`, `TeamSessionConfig`, `TeamSessionState` struct with `state_dir()`, `state_path()`, `load()`, `save()`, `new()` — every later task in this plan builds on these exact names/signatures.

- [ ] **Step 1: Write the failing test**

Add to the bottom of a new `crates/ta-daemon/src/team_session.rs`:

```rust
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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
            TeamSessionStageConfig { name: "analyze".to_string(), roles: vec!["analyst".to_string()] },
            TeamSessionStageConfig { name: "decide".to_string(), roles: vec!["strategist".to_string()] },
            TeamSessionStageConfig { name: "execute".to_string(), roles: vec!["trader".to_string()] },
        ]
    }

    #[test]
    fn state_persists_and_is_readable_across_two_sequential_goal_runs() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path();

        // First goal-run: create session, complete the "analyst" role, save.
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
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
        let reloaded = TeamSessionState::load(project_root, "sess-1").unwrap().unwrap();
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

        let final_state = TeamSessionState::load(project_root, "sess-1").unwrap().unwrap();
        assert_eq!(final_state.findings.len(), 2);
        assert_eq!(final_state.current_stage_index, 2);
    }

    #[test]
    fn load_missing_session_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TeamSessionState::load(dir.path(), "nope").unwrap().is_none());
    }

    #[test]
    fn list_ids_finds_only_dirs_with_state_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = TeamSessionState::new("sess-a".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();
        // A stray directory with no state.json must not be listed.
        std::fs::create_dir_all(dir.path().join(".ta").join("team-sessions").join("not-a-session")).unwrap();

        let ids = TeamSessionState::list_ids(dir.path());
        assert_eq!(ids, vec!["sess-a".to_string()]);
    }
}
```

- [ ] **Step 2: Register `pub mod team_session;` and add the crate dependency**

`team_session.rs` uses `chrono`, `serde`, `serde_json` — all already `ta-daemon` dependencies (see `crates/ta-daemon/Cargo.toml`), no `Cargo.toml` change needed for this task.

In `crates/ta-daemon/src/main.rs`, find the existing module declarations block (around line 39, alongside `pub mod connector_supervisor;` and line 50's `pub mod watchdog;`) and add:

```rust
pub mod team_session;
```

- [ ] **Step 3: Register `.ta/team-sessions/` in the CI path-registration guard**

In `crates/ta-workspace/src/partitioning.rs`, find the `LOCAL_TA_PATHS` array (alongside existing entries like `"staging/"`, `"goals/"`) and add:

```rust
    "team-sessions/",
```

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `./dev cargo test --workspace -p ta-daemon team_session::`
Expected: 3 tests pass (`state_persists_and_is_readable_across_two_sequential_goal_runs`, `load_missing_session_returns_none`, `list_ids_finds_only_dirs_with_state_json`).

- [ ] **Step 5: Run the full workspace build to catch any wiring mistakes**

Run: `./dev cargo build --workspace`
Expected: builds cleanly (the module is registered but unused-outside-tests, so expect no new warnings beyond possibly `dead_code` on not-yet-called pub items — acceptable at this stage since later tasks call them; if `cargo build` errors on dead_code under `-D warnings` in CI, note that `cargo build` alone does not pass `-D warnings`, only `cargo clippy` does in this repo's four-check list, so this step only needs a clean *build*, not lint-clean).

- [ ] **Step 6: Mark PLAN.md progress and commit**

In `PLAN.md`, under `### v0.17.5.1 — Persistent Team Session Runtime`, leave item 1 unchecked for now (the module exists but the full `TeamSession` model/loop isn't complete until Task 7) — do not mark it `[x]` yet; this task only lays the state-persistence foundation.

```bash
git add crates/ta-daemon/src/team_session.rs crates/ta-daemon/src/main.rs crates/ta-workspace/src/partitioning.rs
git commit -m "feat(team-session): add persistent TeamSessionState with state.json round-trip"
```

---

### Task 2: Supervisor status file + pause/resume/stop/restart control-signal files

**Files:**
- Modify: `crates/ta-daemon/src/team_session.rs`

**Interfaces:**
- Consumes: `TeamSessionState::state_dir()` (Task 1).
- Produces: `TeamSessionSupervisorStatus` struct, `write_supervisor_status()`, `read_supervisor_status()`, `signal_pause()`, `signal_resume()`, `signal_stop()`, `signal_restart()`, `consume_signal()`, `has_signal()` — Task 3 (backoff) and Task 7 (loop) call these directly by name.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ta-daemon/src/team_session.rs`, above the existing `#[cfg(test)] mod tests` block's closing brace (keep adding to the same `mod tests`):

```rust
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

pub fn read_supervisor_status(project_root: &Path, id: &str) -> Option<TeamSessionSupervisorStatus> {
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
```

Add these tests inside the existing `mod tests` block:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `./dev cargo test --workspace -p ta-daemon team_session::`
Expected: all tests from Task 1 and Task 2 pass (7 total).

- [ ] **Step 3: Commit**

```bash
git add crates/ta-daemon/src/team_session.rs
git commit -m "feat(team-session): add supervisor status file and pause/resume/stop/restart signals"
```

---

### Task 3: Backoff/suspend `FailureTracker` (pure, testable crash-recovery logic)

**Files:**
- Modify: `crates/ta-daemon/src/team_session.rs`

**Interfaces:**
- Produces: `FailureTracker` struct with `record_failure(&mut self, now: DateTime<Utc>) -> BackoffDecision`, `reset(&mut self)`; `BackoffDecision` enum (`Retry { delay_secs: u64 }`, `Suspend`) — Task 7's `run_one_cycle` consumes this directly.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ta-daemon/src/team_session.rs`, above `mod tests`:

```rust
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
        let delay_secs = 2u64.saturating_pow(restart_count).min(MAX_BACKOFF_SECS).max(1);
        BackoffDecision::Retry { delay_secs }
    }

    /// Clears failure history — called after a successful cycle, or when a
    /// `Suspended` session is explicitly restarted via the restart-signal.
    pub fn reset(&mut self) {
        self.recent_failure_times.clear();
    }
}
```

Add these tests inside `mod tests`:

```rust
    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let mut tracker = FailureTracker::default();
        let base = Utc::now();

        let d1 = tracker.record_failure(base);
        assert!(matches!(d1, BackoffDecision::Retry { delay_secs: 1 }));

        let d2 = tracker.record_failure(base + chrono::Duration::seconds(1));
        assert!(matches!(d2, BackoffDecision::Retry { delay_secs: 2 }));

        let d3 = tracker.record_failure(base + chrono::Duration::seconds(2));
        assert!(matches!(d3, BackoffDecision::Retry { delay_secs: 4 }));

        let d4 = tracker.record_failure(base + chrono::Duration::seconds(3));
        assert!(matches!(d4, BackoffDecision::Suspend));
    }

    #[test]
    fn fifth_failure_within_window_suspends() {
        let mut tracker = FailureTracker::default();
        let base = Utc::now();
        for i in 0..4 {
            let decision = tracker.record_failure(base + chrono::Duration::seconds(i));
            assert!(matches!(decision, BackoffDecision::Retry { .. }), "failure {i} should retry");
        }
        let fifth = tracker.record_failure(base + chrono::Duration::seconds(4));
        assert!(matches!(fifth, BackoffDecision::Suspend), "5th failure in-window must suspend");
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
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `./dev cargo test --workspace -p ta-daemon team_session::`
Expected: all tests pass (11 total).

- [ ] **Step 3: Commit**

```bash
git add crates/ta-daemon/src/team_session.rs
git commit -m "feat(team-session): add FailureTracker with connector_supervisor-matching backoff/suspend"
```

---

### Task 4: Session-scoped context markdown (findings carried forward)

**Files:**
- Modify: `crates/ta-daemon/src/team_session.rs`

**Interfaces:**
- Consumes: `TeamSessionState`, `RoleFinding` (Task 1).
- Produces: `render_session_context(state: &TeamSessionState) -> String`, `write_session_context(project_root: &Path, state: &TeamSessionState, stage_name: &str) -> io::Result<PathBuf>` — Task 6's `build_ta_run_args` takes the returned path directly.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ta-daemon/src/team_session.rs`, above `mod tests`:

```rust
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
```

Add these tests inside `mod tests`:

```rust
    #[test]
    fn render_context_with_no_findings_says_so() {
        let state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        let rendered = render_session_context(&state);
        assert!(rendered.contains("No prior role findings yet"));
        assert!(rendered.contains("trading-desk"));
    }

    #[test]
    fn render_context_lists_findings_in_order() {
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
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
        assert!(analyst_pos < strategist_pos, "findings must render in stage order");
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
            dir.path().join(".ta").join("team-sessions").join("sess-1").join("context-analyze.md")
        );
        assert!(path.exists());
        assert!(std::fs::read_to_string(&path).unwrap().contains("trading-desk"));
    }
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `./dev cargo test --workspace -p ta-daemon team_session::`
Expected: all tests pass (14 total).

- [ ] **Step 3: Commit**

```bash
git add crates/ta-daemon/src/team_session.rs
git commit -m "feat(team-session): render prior findings to markdown context for the next role"
```

---

### Task 5: `ta run` argument builder + role resolution via `.ta/team.toml`

**Files:**
- Modify: `crates/ta-daemon/src/team_session.rs`
- Modify: `crates/ta-daemon/Cargo.toml` (no new dependency needed — `ta-session` is already present; confirm during this task and skip the edit if so)

**Interfaces:**
- Consumes: `TeamSessionState`, `TeamSessionStageConfig` (Task 1); `ta_session::team::TeamConfig`, `ta_session::agent_action::TeamRole` (existing `ta-session` crate, already a `ta-daemon` dependency).
- Produces: `build_ta_run_args(state: &TeamSessionState, stage: &TeamSessionStageConfig, role: &str, team_config: &ta_session::team::TeamConfig, context_path: &Path) -> Vec<String>` — Task 6's `run_one_cycle` calls this directly and passes the result to `std::process::Command::args()`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ta-daemon/src/team_session.rs`, above `mod tests`, and add the two new `use` lines to the top of the file's existing `use` block:

```rust
use ta_session::agent_action::TeamRole;
use ta_session::team::TeamConfig;
```

```rust
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
```

Add these tests inside `mod tests` (add `use ta_session::agent_action::TeamMember;` and `use ta_session::workflow_session::AdvisorSecurity;` to the test module's imports, or fully qualify inline as shown):

```rust
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
```

- [ ] **Step 2: Confirm the `ta-session` dependency and re-exports compile**

Run: `./dev cargo build -p ta-daemon`
Expected: builds cleanly. If it fails on `ta_session::team` or `ta_session::agent_action` not being visible, check `crates/ta-session/src/lib.rs`'s `pub mod team;`/`pub mod agent_action;` declarations are present (they already are per the existing codebase) — no `Cargo.toml` edit should be required since `ta-session = { path = "../ta-session", ... }` is already listed in `crates/ta-daemon/Cargo.toml`.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `./dev cargo test --workspace -p ta-daemon team_session::`
Expected: all tests pass (16 total).

- [ ] **Step 4: Commit**

```bash
git add crates/ta-daemon/src/team_session.rs
git commit -m "feat(team-session): build ta run args from stage/role and team.toml assignment"
```

---

### Task 6: `run_one_cycle` — the synchronous, testable per-cycle driver

**Files:**
- Modify: `crates/ta-daemon/src/team_session.rs`

**Interfaces:**
- Consumes: everything from Tasks 1-5 (`TeamSessionState`, signal fns, `FailureTracker`, `write_session_context`, `build_ta_run_args`).
- Produces: `CycleOutcome` enum (`Advanced`, `Retrying { delay_secs: u64 }`, `Suspended`, `Paused`, `Stopped`, `AllStagesComplete`), `run_one_cycle(project_root: &Path, id: &str, ta_bin: &Path, tracker: FailureTracker) -> io::Result<(CycleOutcome, FailureTracker)>` — Task 7's async `run_team_session` calls this inside `tokio::task::spawn_blocking` and is the only caller outside tests.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ta-daemon/src/team_session.rs`, above `mod tests`:

```rust
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
        write_supervisor_status(project_root, &TeamSessionSupervisorStatus {
            id: id.to_string(),
            status: "stopped".to_string(),
            current_stage: None,
            current_role: None,
            restart_count: state.restart_count,
            last_cycle_at: Some(Utc::now()),
            updated_at: Utc::now(),
        })?;
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
            write_supervisor_status(project_root, &TeamSessionSupervisorStatus {
                id: id.to_string(),
                status: "paused".to_string(),
                current_stage: None,
                current_role: None,
                restart_count: state.restart_count,
                last_cycle_at: Some(Utc::now()),
                updated_at: Utc::now(),
            })?;
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
            write_supervisor_status(project_root, &TeamSessionSupervisorStatus {
                id: id.to_string(),
                status: "active".to_string(),
                current_stage: Some(stage.name),
                current_role: Some(role),
                restart_count: 0,
                last_cycle_at: Some(now),
                updated_at: now,
            })?;
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
            write_supervisor_status(project_root, &TeamSessionSupervisorStatus {
                id: id.to_string(),
                status: status_str,
                current_stage: Some(stage.name),
                current_role: Some(role),
                restart_count: state.restart_count,
                last_cycle_at: Some(now),
                updated_at: now,
            })?;
            Ok((outcome, tracker))
        }
    }
}
```

- [ ] **Step 2: Write the failing tests**

Add inside `mod tests`. These use a fake `ta` binary shell script, the exact same technique `crates/ta-session/src/advisor_agent.rs`'s own subprocess tests already use (a temp script on `PATH`-independent absolute path, executed directly — works on the Unix CI runners this repo targets; skip on Windows via `#[cfg(unix)]` matching the existing `advisor_agent.rs` precedent):

```rust
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
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();

        let (outcome, tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();

        assert_eq!(outcome, CycleOutcome::Advanced);
        let reloaded = TeamSessionState::load(dir.path(), "sess-1").unwrap().unwrap();
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
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.current_stage_index = 2; // last of 3 stages (indices 0,1,2)
        state.save(dir.path()).unwrap();

        let (outcome, _tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();

        assert_eq!(outcome, CycleOutcome::Advanced);
        let reloaded = TeamSessionState::load(dir.path(), "sess-1").unwrap().unwrap();
        assert_eq!(reloaded.current_stage_index, 3);
        // Next cycle should wrap back to stage 0 (3 % 3 == 0) — verified by
        // running a second cycle and checking it targets stage "analyze".
        let (outcome2, _tracker3) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();
        assert_eq!(outcome2, CycleOutcome::Advanced);
        let final_state = TeamSessionState::load(dir.path(), "sess-1").unwrap().unwrap();
        assert_eq!(final_state.findings[1].stage, "analyze");
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_crash_looping_session_reaches_suspended_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\nexit 1\n");
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
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
        let (outcome_after, _t) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, tracker).unwrap();
        assert_eq!(outcome_after, CycleOutcome::Suspended);

        let status = read_supervisor_status(dir.path(), "sess-1").unwrap();
        assert_eq!(status.status, "suspended");
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_status_reflects_real_supervisor_state() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\necho done\nexit 0\n");
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
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
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();
        signal_stop(dir.path(), "sess-1").unwrap();

        let (outcome, _tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();

        assert_eq!(outcome, CycleOutcome::Stopped);
        let reloaded = TeamSessionState::load(dir.path(), "sess-1").unwrap().unwrap();
        assert_eq!(reloaded.status, TeamSessionStatus::Stopped);
    }

    #[cfg(unix)]
    #[test]
    fn run_one_cycle_pause_then_resume_signal() {
        let dir = tempfile::tempdir().unwrap();
        let ta_bin = write_fake_ta_binary(dir.path(), "#!/bin/sh\necho ok\nexit 0\n");
        let mut state = TeamSessionState::new("sess-1".to_string(), sample_config(), sample_stages());
        state.save(dir.path()).unwrap();
        signal_pause(dir.path(), "sess-1").unwrap();

        let (outcome, tracker) =
            run_one_cycle(dir.path(), "sess-1", &ta_bin, FailureTracker::default()).unwrap();
        assert_eq!(outcome, CycleOutcome::Paused);
        let paused_state = TeamSessionState::load(dir.path(), "sess-1").unwrap().unwrap();
        assert_eq!(paused_state.status, TeamSessionStatus::Paused);

        signal_resume(dir.path(), "sess-1").unwrap();
        let (outcome2, _tracker2) = run_one_cycle(dir.path(), "sess-1", &ta_bin, tracker).unwrap();
        assert_eq!(outcome2, CycleOutcome::Advanced);
    }
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `./dev cargo test --workspace -p ta-daemon team_session::`
Expected: all tests pass (23 total). This covers PLAN.md item 5's three required test scenarios directly: sequential goal-run state persistence (Task 1 + this task's `run_one_cycle_advances_on_success`), crash-loop → Suspended (`run_one_cycle_crash_looping_session_reaches_suspended_and_stops`), and status reflecting real supervisor state (`run_one_cycle_status_reflects_real_supervisor_state`).

- [ ] **Step 4: Commit**

```bash
git add crates/ta-daemon/src/team_session.rs
git commit -m "feat(team-session): add run_one_cycle sync driver with pause/stop/suspend handling"
```

---

### Task 7: Async supervised loop + daemon wiring

**Files:**
- Modify: `crates/ta-daemon/src/team_session.rs`
- Modify: `crates/ta-daemon/src/main.rs`

**Interfaces:**
- Consumes: `run_one_cycle`, `CycleOutcome`, `FailureTracker`, `TeamSessionState::list_ids` (Tasks 1-6).
- Produces: `pub fn start(project_root: PathBuf, shutdown: Arc<tokio::sync::Notify>)` — the daemon's `main.rs` calls this exactly once per mode block, matching `connector_supervisor::start(...)`/`watchdog::run_watchdog(...)`'s existing call shape.

- [ ] **Step 1: Add the async loop**

Append to `crates/ta-daemon/src/team_session.rs` (add `use std::sync::Arc;` and `use std::time::Duration;` to the top-level `use` block):

```rust
const IDLE_POLL_SECS: u64 = 5;

/// Runs the supervised loop for one team session until it's `Stopped` or
/// the daemon shuts down. Each cycle's actual work (`run_one_cycle`) is
/// synchronous and runs on a blocking thread via `spawn_blocking`, so an
/// in-flight `ta run` subprocess is not interrupted by shutdown — only the
/// next cycle is skipped, matching `connector_supervisor.rs`'s own
/// "in-flight work finishes, then the loop exits" shutdown behavior.
async fn run_team_session(project_root: PathBuf, id: String, ta_bin: PathBuf, shutdown: Arc<tokio::sync::Notify>) {
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
                (CycleOutcome::Retrying { delay_secs: IDLE_POLL_SECS }, tracker)
            }
            Err(join_err) => {
                tracing::error!(session_id = %id, error = %join_err, "team session cycle task panicked");
                (CycleOutcome::Retrying { delay_secs: IDLE_POLL_SECS }, tracker)
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
    let ta_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ta"));
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
```

- [ ] **Step 2: Wire `team_session::start(...)` into the daemon's startup**

In `crates/ta-daemon/src/main.rs`, find the API-mode block containing the existing `watchdog::run_watchdog(...)` spawn (around line 398) and add, immediately after it:

```rust
    {
        let ts_root = project_root.clone();
        let ts_shutdown = shutdown.clone();
        team_session::start(ts_root, ts_shutdown);
    }
```

Repeat the identical block immediately after the second `watchdog::run_watchdog(...)` spawn in the MCP-mode block (around line 490) — `watchdog` itself is spawned separately (duplicated, not shared) in both blocks per the existing precedent, so `team_session::start` follows the same duplication rather than trying to share one call site across the two modes.

- [ ] **Step 3: Run the full test suite and build**

Run: `./dev cargo test --workspace -p ta-daemon`
Expected: all `ta-daemon` tests pass, including all 23 `team_session::` tests.

Run: `./dev cargo build --workspace`
Expected: builds cleanly, `team_session::start` is now referenced from `main.rs` so no more dead-code concern for that function.

- [ ] **Step 4: Mark PLAN.md items 1, 2, 4, and 5 done**

In `PLAN.md`, under `### v0.17.5.1 — Persistent Team Session Runtime`, check off:
```markdown
1. [x] New `ta-daemon` subsystem module (e.g. `team_session.rs` ...) ...
2. [x] Session state carries context forward between goal-runs within the same session ...
4. [x] Crash recovery: reuse `connector_supervisor.rs`'s backoff/suspend pattern ...
5. [x] Tests: session state persists and is readable across two sequential goal-runs; a crash-looping session reaches Suspended and stops; `status` reflects real supervisor state.
```
Leave item 3 (CLI lifecycle commands) and item 6 (USAGE.md) unchecked — those are Tasks 8 and 9.

- [ ] **Step 5: Commit**

```bash
git add crates/ta-daemon/src/team_session.rs crates/ta-daemon/src/main.rs PLAN.md
git commit -m "feat(team-session): wire supervised loop into daemon startup (API + MCP modes)"
```

---

### Task 8: `ta team-session start/pause/resume/stop/status` CLI

**Files:**
- Create: `apps/ta-cli/src/commands/team_session.rs`
- Modify: `apps/ta-cli/src/commands/mod.rs`
- Modify: `apps/ta-cli/src/main.rs`
- Test: inline `#[cfg(test)] mod tests` in `apps/ta-cli/src/commands/team_session.rs`

**Interfaces:**
- Consumes: `ta_workflow::WorkflowDefinition::from_file`/`.stage_order()`; the daemon's on-disk `state.json`/`supervisor-status.json`/signal-file layout from Tasks 1-2 (re-implemented as plain structs here, same shape, matching `connector.rs`'s existing "CLI has its own copy of the status struct, kept in sync with the daemon's" convention — do **not** add an `apps/ta-cli` → `ta-daemon` crate dependency just for these types).
- Produces: `TeamSessionCommands` enum, `execute(command: &TeamSessionCommands, project_root: &Path) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing tests**

Create `apps/ta-cli/src/commands/team_session.rs`:

```rust
// team_session.rs — `ta team-session` subcommand: start, pause, resume, stop, status.
//
// Mirrors `connector.rs`'s shape: no RPC to the running daemon — writes/reads
// the same `.ta/team-sessions/<id>/` files the daemon's `team_session.rs`
// supervisor loop uses (state.json, supervisor-status.json, and
// pause/resume/stop/restart signal files).

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use ta_workflow::WorkflowDefinition;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TeamSessionStatus {
    Active,
    Paused,
    Suspended,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamSessionStageConfig {
    name: String,
    roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamSessionConfig {
    name: String,
    workflow_path: String,
    team_toml_path: String,
    objective: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamSessionState {
    id: String,
    config: TeamSessionConfig,
    stages: Vec<TeamSessionStageConfig>,
    status: TeamSessionStatus,
    current_stage_index: usize,
    findings: Vec<serde_json::Value>,
    restart_count: u32,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
struct SupervisorStatus {
    id: String,
    status: String,
    current_stage: Option<String>,
    current_role: Option<String>,
    restart_count: u32,
    last_cycle_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn session_dir(project_root: &Path, id: &str) -> std::path::PathBuf {
    project_root.join(".ta").join("team-sessions").join(id)
}

fn state_path(project_root: &Path, id: &str) -> std::path::PathBuf {
    session_dir(project_root, id).join("state.json")
}

fn load_state(project_root: &Path, id: &str) -> Option<TeamSessionState> {
    let raw = std::fs::read_to_string(state_path(project_root, id)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn read_supervisor_status(project_root: &Path, id: &str) -> Option<SupervisorStatus> {
    let path = session_dir(project_root, id).join("supervisor-status.json");
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_signal(project_root: &Path, id: &str, signal: &str) -> std::io::Result<()> {
    let dir = session_dir(project_root, id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(signal), signal)
}

#[derive(Debug, Subcommand)]
pub enum TeamSessionCommands {
    /// Start a new persistent team session bound to a workflow YAML and `.ta/team.toml`.
    ///
    /// Resolves the workflow's stage order once at start time and persists it into
    /// the session's state.json — the daemon's supervisor loop reads that resolved
    /// list, it never re-parses the workflow YAML itself.
    ///
    /// Examples:
    ///   ta team-session start trading-desk --workflow templates/workflows/trading-desk.yaml --objective "Generate income > 2x within 6 months after fees"
    Start {
        /// Session name (also used as its ID).
        name: String,
        /// Path to the workflow YAML declaring roles/stages.
        #[arg(long)]
        workflow: String,
        /// Path to the team.toml binding roles to agents (defaults to `.ta/team.toml`).
        #[arg(long)]
        team_toml: Option<String>,
        /// The session's overall objective, carried into every role's context.
        #[arg(long, default_value = "")]
        objective: String,
    },
    /// Pause a running session — the supervisor stops firing new goal-runs
    /// until `ta team-session resume <name>`.
    Pause {
        name: String,
    },
    /// Resume a paused session.
    Resume {
        name: String,
    },
    /// Stop a session permanently (does not delete its state.json history).
    Stop {
        name: String,
    },
    /// Show live supervisor status for one or all sessions.
    Status {
        /// Session name. Shows all if omitted.
        name: Option<String>,
    },
}

pub fn execute(command: &TeamSessionCommands, project_root: &Path) -> Result<()> {
    match command {
        TeamSessionCommands::Start { name, workflow, team_toml, objective } => {
            start(project_root, name, workflow, team_toml.as_deref(), objective)
        }
        TeamSessionCommands::Pause { name } => {
            write_signal(project_root, name, "pause-signal").context("writing pause-signal")?;
            println!("[team-session] pause requested for '{name}' — takes effect on the supervisor's next poll.");
            Ok(())
        }
        TeamSessionCommands::Resume { name } => {
            write_signal(project_root, name, "resume-signal").context("writing resume-signal")?;
            println!("[team-session] resume requested for '{name}'.");
            Ok(())
        }
        TeamSessionCommands::Stop { name } => {
            write_signal(project_root, name, "stop-signal").context("writing stop-signal")?;
            println!("[team-session] stop requested for '{name}'.");
            Ok(())
        }
        TeamSessionCommands::Status { name } => status(project_root, name.as_deref()),
    }
}

fn start(
    project_root: &Path,
    name: &str,
    workflow_path: &str,
    team_toml: Option<&str>,
    objective: &str,
) -> Result<()> {
    if state_path(project_root, name).exists() {
        bail!(
            "Team session '{name}' already exists at {}. Use `ta team-session status {name}` to check it, or pick a different name.",
            state_path(project_root, name).display()
        );
    }

    let workflow_full_path = project_root.join(workflow_path);
    let definition = WorkflowDefinition::from_file(&workflow_full_path).with_context(|| {
        format!(
            "Failed to load workflow YAML at {} for team session '{name}'. Check the --workflow path is correct and the file is valid workflow YAML.",
            workflow_full_path.display()
        )
    })?;
    let stage_order = definition
        .stage_order()
        .with_context(|| format!("Workflow '{workflow_path}' has a cyclic stage dependency graph — cannot resolve a stage order for team session '{name}'."))?;

    let stages: Vec<TeamSessionStageConfig> = stage_order
        .iter()
        .filter_map(|stage_name| {
            definition
                .stages
                .iter()
                .find(|s| &s.name == stage_name)
                .map(|s| TeamSessionStageConfig {
                    name: s.name.clone(),
                    roles: s.roles.clone(),
                })
        })
        .collect();

    if stages.is_empty() {
        bail!(
            "Workflow '{workflow_path}' declares no stages — team session '{name}' would have nothing to run. Add at least one `stages:` entry to the workflow YAML."
        );
    }

    let now = chrono::Utc::now();
    let state = TeamSessionState {
        id: name.to_string(),
        config: TeamSessionConfig {
            name: name.to_string(),
            workflow_path: workflow_path.to_string(),
            team_toml_path: team_toml.unwrap_or(".ta/team.toml").to_string(),
            objective: objective.to_string(),
        },
        stages,
        status: TeamSessionStatus::Active,
        current_stage_index: 0,
        findings: Vec::new(),
        restart_count: 0,
        created_at: now,
        updated_at: now,
    };

    let dir = session_dir(project_root, name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create session directory {}", dir.display()))?;
    let raw = serde_json::to_string_pretty(&state)?;
    std::fs::write(state_path(project_root, name), raw)
        .with_context(|| format!("Failed to write state.json for team session '{name}'"))?;

    println!(
        "[team-session] started '{name}' with {} stage(s) from '{workflow_path}'. The daemon supervisor picks it up on its next startup or poll cycle. Check progress with `ta team-session status {name}`.",
        state.stages.len()
    );
    Ok(())
}

fn status(project_root: &Path, name: Option<&str>) -> Result<()> {
    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => {
            let dir = project_root.join(".ta").join("team-sessions");
            let mut names = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        names.push(entry.file_name().to_string_lossy().to_string());
                    }
                }
            }
            names.sort();
            names
        }
    };

    if names.is_empty() {
        println!("No team sessions found under .ta/team-sessions/. Start one with `ta team-session start <name> --workflow <path>`.");
        return Ok(());
    }

    for n in &names {
        let Some(state) = load_state(project_root, n) else {
            println!("{n}: state.json missing or unreadable — not a valid team session.");
            continue;
        };
        match read_supervisor_status(project_root, n) {
            Some(sup) => {
                println!(
                    "{n}: {} (stage={:?} role={:?} restarts={} last_cycle={:?})",
                    sup.status, sup.current_stage, sup.current_role, sup.restart_count, sup.last_cycle_at
                );
            }
            None => {
                println!(
                    "{n}: {:?} (persisted state only — supervisor hasn't run a cycle yet; findings={})",
                    state.status,
                    state.findings.len()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_role_workflow(dir: &Path) -> String {
        let path = dir.join("trading-desk.yaml");
        std::fs::write(
            &path,
            r#"
name: trading-desk
roles:
  analyst:
    agent: claude-code
    prompt: "Analyze the market."
  strategist:
    agent: claude-code
    prompt: "Decide a strategy."
stages:
  - name: analyze
    roles: ["analyst"]
  - name: decide
    depends_on: ["analyze"]
    roles: ["strategist"]
"#,
        )
        .unwrap();
        "trading-desk.yaml".to_string()
    }

    #[test]
    fn start_writes_state_json_with_resolved_stage_order() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());

        start(dir.path(), "sess-1", &workflow_path, None, "Make money").unwrap();

        let state = load_state(dir.path(), "sess-1").unwrap();
        assert_eq!(state.stages.len(), 2);
        assert_eq!(state.stages[0].name, "analyze");
        assert_eq!(state.stages[1].name, "decide");
        assert_eq!(state.status, TeamSessionStatus::Active);
    }

    #[test]
    fn start_rejects_a_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());
        start(dir.path(), "sess-1", &workflow_path, None, "").unwrap();

        let result = start(dir.path(), "sess-1", &workflow_path, None, "");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn start_rejects_missing_workflow_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = start(dir.path(), "sess-1", "does-not-exist.yaml", None, "");
        assert!(result.is_err());
    }

    #[test]
    fn pause_resume_stop_write_expected_signal_files() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());
        start(dir.path(), "sess-1", &workflow_path, None, "").unwrap();

        execute(&TeamSessionCommands::Pause { name: "sess-1".to_string() }, dir.path()).unwrap();
        assert!(session_dir(dir.path(), "sess-1").join("pause-signal").exists());

        execute(&TeamSessionCommands::Resume { name: "sess-1".to_string() }, dir.path()).unwrap();
        assert!(session_dir(dir.path(), "sess-1").join("resume-signal").exists());

        execute(&TeamSessionCommands::Stop { name: "sess-1".to_string() }, dir.path()).unwrap();
        assert!(session_dir(dir.path(), "sess-1").join("stop-signal").exists());
    }

    #[test]
    fn status_reports_persisted_state_before_any_supervisor_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());
        start(dir.path(), "sess-1", &workflow_path, None, "").unwrap();

        // No supervisor-status.json written yet (daemon hasn't run a cycle) —
        // must not error, must fall back to persisted state.json.
        status(dir.path(), Some("sess-1")).unwrap();
    }
}
```

- [ ] **Step 2: Register the module**

In `apps/ta-cli/src/commands/mod.rs`, add near the existing `pub mod team;` line:

```rust
pub mod team_session;
```

- [ ] **Step 3: Register the `Commands::TeamSession` variant and dispatch arm**

In `apps/ta-cli/src/main.rs`, find the `Commands::Connector { ... }` variant (around line 1052) and add a sibling variant immediately after it:

```rust
    /// Manage persistent team sessions: a workflow YAML + `.ta/team.toml`
    /// binding that runs continuously, firing one goal-run per role in
    /// sequence and carrying prior roles' findings forward as context.
    #[command(hide = true)]
    TeamSession {
        #[command(subcommand)]
        command: commands::team_session::TeamSessionCommands,
    },
```

Find the `Commands::Connector { command } => { ... }` dispatch arm (around line 1854) and add a sibling arm immediately after it:

```rust
        Commands::TeamSession { command } => {
            if warn_legacy {
                print_deprecation_notice("team-session", command);
            }
            commands::team_session::execute(command, &config.workspace_root)
        }
```

If `print_deprecation_notice`'s second parameter type doesn't accept `&TeamSessionCommands` generically (check its signature at its definition, likely near `Commands::Connector`'s use of it), match the exact bound it requires (it's almost certainly a `Debug`-bounded generic given it's reused across every hidden legacy command in this file, so no change should be needed).

- [ ] **Step 4: Run the tests**

Run: `./dev cargo test --workspace -p ta-cli team_session::`
Expected: all 5 tests pass.

Run: `./dev cargo build --workspace`
Expected: builds cleanly.

- [ ] **Step 5: Mark PLAN.md item 3 done and commit**

```markdown
3. [x] Lifecycle commands: `ta team-session start/pause/stop/status <name>`, mirroring the existing `ta connector` command shape.
```

```bash
git add apps/ta-cli/src/commands/team_session.rs apps/ta-cli/src/commands/mod.rs apps/ta-cli/src/main.rs PLAN.md
git commit -m "feat(team-session): add ta team-session start/pause/resume/stop/status CLI"
```

---

### Task 9: Canonical verb+noun registration, USAGE.md docs, and final PLAN.md cleanup

**Files:**
- Modify: `apps/ta-cli/src/commands/verb.rs`
- Modify: `docs/USAGE.md`
- Modify: `PLAN.md`

**Interfaces:**
- Consumes: `NOUN_TABLE`/`NounEntry` (existing, from the earlier exploration — `apps/ta-cli/src/commands/verb.rs:30-45`).

- [ ] **Step 1: Add the `team-session` `NounEntry`**

In `apps/ta-cli/src/commands/verb.rs`, add a new entry to `NOUN_TABLE` (alongside the existing `connector`/`team` entries):

```rust
    NounEntry {
        keys: &["team-session", "team-sessions"],
        legacy: "team-session",
        verbs: &[
            ("create", "start"),
            ("show", "status"),
            ("remove", "stop"),
        ],
    },
```

`pause`/`resume` are intentionally left unmapped to a verb — none of the 10 canonical verbs (`create/list/show/update/remove/run/approve/deny/apply/check/sync`) has a clean, unambiguous fit for "pause a running session" the way `update` unambiguously means "assign" for `team`. `ta team-session pause/resume <name>` remains the only spelling for those two actions; this matches the existing `verb.rs` doc comment's own rule ("nouns/verbs not listed here have no first-class verb+noun form yet — the legacy noun-first command keeps working unchanged").

- [ ] **Step 2: Write a test confirming the entry resolves**

Find `verb.rs`'s existing tests (likely `#[cfg(test)] mod tests` near the bottom, testing `resolve_noun` or similar for `connector`/`team`) and add a parallel case:

```rust
    #[test]
    fn team_session_noun_resolves_create_to_start() {
        let entry = find_noun_entry("team-session").expect("team-session should be registered");
        let verb_target = entry.verbs.iter().find(|(v, _)| *v == "create");
        assert_eq!(verb_target, Some(&("create", "start")));
    }
```

(Match the actual helper function name used by the existing tests in this file — it was reported as `find_noun_entry` based on the `NOUN_TABLE.iter().find(...)` snippet at `verb.rs:418-421`; if the existing tests call a differently-named public wrapper, use that name instead for consistency.)

- [ ] **Step 3: Run the tests**

Run: `./dev cargo test --workspace -p ta-cli verb::`
Expected: all tests pass including the new `team_session_noun_resolves_create_to_start`.

- [ ] **Step 4: Add the USAGE.md section**

In `docs/USAGE.md`, immediately after the existing `## Connector Supervision` section (before `## Context Compression`), add:

```markdown
## Persistent Team Sessions

A persistent team session gives a team of roles (e.g. analyst, strategist, trader) an
ongoing execution context that survives across many goal-runs, instead of every
trigger starting from zero. It binds a workflow YAML's declared roles/stages to your
`.ta/team.toml` agent assignments, then runs continuously: each role's goal-run
completes, its findings are carried forward as context for the next role, and the
cycle repeats through the workflow's stages indefinitely.

Start one:

```bash
ta team-session start trading-desk \
  --workflow templates/workflows/trading-desk.yaml \
  --objective "Generate income > 2x within 6 months after fees"
```

This resolves the workflow's stage order once and persists it to
`.ta/team-sessions/trading-desk/state.json`. The daemon's supervisor picks up any
non-stopped session on startup (or on its next poll) and begins firing one `ta run`
goal per role in sequence.

Check status:

```bash
ta team-session status trading-desk
# trading-desk: active (stage=Some("decide") role=Some("strategist") restarts=0 last_cycle=...)

ta team-session status   # all sessions
```

Pause and resume without losing state:

```bash
ta team-session pause trading-desk
ta team-session resume trading-desk
```

Stop permanently (state.json history is kept, not deleted):

```bash
ta team-session stop trading-desk
```

**Crash recovery**: if a role's goal-run keeps failing, the supervisor retries with
the same backoff as connector supervision — 1s, 2s, 4s, ... up to a 60s cap. After 5
failures within 5 minutes, the session is marked `suspended` and the supervisor stops
attempting new goal-runs until you clear it:

```bash
# write a restart-signal the same way `ta connector restart` does for connectors
touch .ta/team-sessions/trading-desk/restart-signal
```

**Context carry-forward**: each completed role's stdout summary is appended to the
session's findings list and rendered as markdown context
(`.ta/team-sessions/<name>/context-<stage>.md`) passed to the next role's `ta run
--objective-file`, so a "strategist" role can see what the "analyst" role before it
found, and so on across the whole session's lifetime — not just within one goal-run.
```

- [ ] **Step 5: Mark PLAN.md item 6 done and close out the phase's remaining state**

In `PLAN.md`, check off:
```markdown
6. [x] USAGE.md: how to start/monitor/stop a persistent team session.
```

Confirm all 6 items under `### v0.17.5.1 — Persistent Team Session Runtime` are now `[x]`. Do **not** change the `<!-- status: in_progress -->` marker — per this project's CLAUDE.md, only `ta draft apply` transitions phase status.

- [ ] **Step 6: Run the full four-check verification**

Run, in order:
```bash
./dev cargo build --workspace
./dev cargo test --workspace
./dev cargo clippy --workspace --all-targets -- -D warnings
./dev cargo fmt --all -- --check
```
Expected: all four pass. If `clippy` flags anything in the new `team_session.rs` files (e.g. needless clones from the `spawn_blocking` closures, or the `String`-heavy `build_ta_run_args`), fix in place before proceeding — do not silence with `#[allow(...)]` unless the lint is a known false positive already suppressed elsewhere in this crate for the same reason.

- [ ] **Step 7: Commit**

```bash
git add apps/ta-cli/src/commands/verb.rs docs/USAGE.md PLAN.md
git commit -m "docs(team-session): add verb+noun registration and USAGE.md guide, close out v0.17.5.1"
```

---

## Self-Review Notes

**Spec coverage** (against PLAN.md's 6 items for v0.17.5.1):
1. New `ta-daemon` subsystem module `team_session.rs` modeling `TeamSession` state/loop → Tasks 1, 6, 7.
2. Session state carries context forward via `advisor_agent`-style context injection → Task 4 (`render_session_context`/`write_session_context`) + Task 5/6 (`--objective-file` wiring).
3. Lifecycle commands `ta team-session start/pause/stop/status` → Task 8 (plus `resume`, needed for `pause` to be reversible — not in PLAN's literal list but required for `pause` to be a real control, not a dead end).
4. Crash recovery reusing `connector_supervisor.rs`'s backoff/suspend → Task 3 (`FailureTracker`) + Task 6 (`run_one_cycle`'s Suspend handling) + Task 8's `restart-signal` doc note.
5. Tests for all three named scenarios → Task 1 (state persistence) + Task 6 (crash-loop-to-Suspended, status-reflects-real-state).
6. USAGE.md → Task 9.

**Placeholder scan**: no TBD/TODO/"add appropriate X" strings in any task — every step shows complete code. The one open design question (no clean verb for "pause") is called out explicitly with its resolution (leave unmapped), not left as a gap.

**Type consistency**: `TeamSessionState`/`TeamSessionStatus`/`TeamSessionStageConfig`/`RoleFinding` are defined once in Task 1 and reused with identical field names through Tasks 2-7; the CLI's Task 8 copy intentionally duplicates the shape (matching `connector.rs`'s existing convention of the CLI keeping its own copy of the daemon's status struct) rather than adding a cross-crate dependency — called out explicitly in Task 8's Interfaces block so this isn't mistaken for a naming drift bug.
