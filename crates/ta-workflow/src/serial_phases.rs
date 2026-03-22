// serial_phases.rs — Serial phase chains workflow (v0.13.7).
//
// Implements the `serial-phases` built-in workflow:
//   - Runs each phase as a follow-up goal in the same staging directory.
//   - Evaluates configurable gates (build, test, clippy, custom) after each phase.
//   - Pauses with actionable error on gate failure, allowing the user to fix and resume.
//   - Persists workflow state to `.ta/serial-workflow-<id>.json` for resume support.
//
// Usage (via `ta run --workflow serial-phases --phases v0.13.7.1,v0.13.7.2`):
//   Phase 1 runs → gates evaluated → Phase 2 as follow-up → gates → final summary.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Gate types ──────────────────────────────────────────────────────────────

/// A gate command evaluated after each phase step.
///
/// Gates are run in the staging directory. All must pass (exit 0) before the
/// next phase begins. On failure the workflow pauses, the step is marked
/// `GateFailed`, and the user is told how to resume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WorkflowGate {
    /// Run `cargo build --workspace` in the staging directory.
    Build,
    /// Run `cargo test --workspace` in the staging directory.
    Test,
    /// Run `cargo clippy --workspace --all-targets -- -D warnings`.
    Clippy,
    /// Arbitrary shell command run in the staging directory.
    Custom { command: String },
}

impl WorkflowGate {
    /// Shell command string for this gate.
    pub fn command_str(&self) -> String {
        match self {
            WorkflowGate::Build => "cargo build --workspace".to_string(),
            WorkflowGate::Test => "cargo test --workspace".to_string(),
            WorkflowGate::Clippy => {
                "cargo clippy --workspace --all-targets -- -D warnings".to_string()
            }
            WorkflowGate::Custom { command } => command.clone(),
        }
    }

    /// Short descriptive name for display.
    pub fn name(&self) -> &str {
        match self {
            WorkflowGate::Build => "build",
            WorkflowGate::Test => "test",
            WorkflowGate::Clippy => "clippy",
            WorkflowGate::Custom { .. } => "custom",
        }
    }

    /// Parse a gate from a string identifier.
    ///
    /// Accepts: "build", "test", "clippy", or any other string as a custom command.
    pub fn parse(s: &str) -> Self {
        match s {
            "build" => WorkflowGate::Build,
            "test" => WorkflowGate::Test,
            "clippy" => WorkflowGate::Clippy,
            cmd => WorkflowGate::Custom {
                command: cmd.to_string(),
            },
        }
    }
}

// ── Step state ──────────────────────────────────────────────────────────────

/// Execution state of a single phase step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepState {
    /// Not yet started.
    Pending,
    /// Agent is running for this phase.
    Running { goal_id: String },
    /// Agent completed and all gates passed.
    Passed { goal_id: String },
    /// Agent completed but a gate failed.
    GateFailed {
        goal_id: String,
        failed_gate: String,
        error: String,
    },
    /// Agent itself returned a non-zero exit code.
    AgentFailed { error: String },
}

impl StepState {
    /// Returns the goal_id if this step has one.
    pub fn goal_id(&self) -> Option<&str> {
        match self {
            StepState::Running { goal_id }
            | StepState::Passed { goal_id }
            | StepState::GateFailed { goal_id, .. } => Some(goal_id.as_str()),
            _ => None,
        }
    }

    /// Returns true if this step completed successfully.
    pub fn is_passed(&self) -> bool {
        matches!(self, StepState::Passed { .. })
    }

    /// Returns true if this step failed in any way.
    pub fn is_failed(&self) -> bool {
        matches!(
            self,
            StepState::GateFailed { .. } | StepState::AgentFailed { .. }
        )
    }
}

// ── Workflow state ──────────────────────────────────────────────────────────

/// Persisted state for a serial-phases workflow run.
///
/// Stored in `.ta/serial-workflow-<id>.json`. Used to resume after gate failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialPhasesState {
    /// Unique identifier for this workflow run.
    pub workflow_id: String,
    /// Ordered list of phase IDs to execute.
    pub phases: Vec<String>,
    /// Per-step execution states (parallel to `phases`).
    pub steps: Vec<StepState>,
    /// Index of the current step (0-based).
    pub current_step: usize,
    /// Staging directory path (set after the first phase creates it).
    pub staging_path: Option<PathBuf>,
    /// Goal ID of the most recently completed step (used as follow-up for next step).
    pub last_goal_id: Option<String>,
    /// Gate names that were configured for this workflow run.
    pub gates: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SerialPhasesState {
    /// Create a new workflow state for the given phases.
    pub fn new(workflow_id: &str, phases: Vec<String>, gates: Vec<String>) -> Self {
        let n = phases.len();
        Self {
            workflow_id: workflow_id.to_string(),
            phases,
            steps: vec![StepState::Pending; n],
            current_step: 0,
            staging_path: None,
            last_goal_id: None,
            gates,
            started_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Persist workflow state to disk.
    pub fn save(&mut self, dir: &Path) -> std::io::Result<()> {
        self.updated_at = Utc::now();
        let path = dir.join(format!("serial-workflow-{}.json", self.workflow_id));
        let json =
            serde_json::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Load a saved workflow state from disk.
    pub fn load(dir: &Path, workflow_id: &str) -> Option<Self> {
        let path = dir.join(format!("serial-workflow-{}.json", workflow_id));
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Find the latest serial-workflow state file, if any.
    pub fn load_latest(dir: &Path) -> Option<Self> {
        let entries = std::fs::read_dir(dir).ok()?;
        let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_name()?.to_str()?.to_string();
                if name.starts_with("serial-workflow-") && name.ends_with(".json") {
                    let mtime = p.metadata().ok()?.modified().ok()?;
                    Some((mtime, p))
                } else {
                    None
                }
            })
            .collect();
        candidates.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        let (_, path) = candidates.first()?;
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Returns the number of phases that have been passed.
    pub fn passed_count(&self) -> usize {
        self.steps.iter().filter(|s| s.is_passed()).count()
    }

    /// Returns the first failed step index, if any.
    pub fn failed_step(&self) -> Option<usize> {
        self.steps.iter().position(|s| s.is_failed())
    }

    /// Returns the next step to run (first Pending after all Passed steps).
    pub fn next_pending_step(&self) -> Option<usize> {
        self.steps
            .iter()
            .position(|s| matches!(s, StepState::Pending))
    }
}

// ── Gate evaluation ─────────────────────────────────────────────────────────

/// Result of evaluating a single gate command.
#[derive(Debug)]
pub struct GateResult {
    pub gate: WorkflowGate,
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub output: String,
    pub elapsed_secs: f64,
}

/// Run a single gate command in the given staging directory.
///
/// Returns Ok(GateResult) with `passed=false` if the command exits non-zero.
/// Returns Err if the command could not be spawned.
pub fn run_gate(gate: &WorkflowGate, staging_dir: &Path) -> Result<GateResult, std::io::Error> {
    let cmd_str = gate.command_str();
    let start = std::time::Instant::now();

    // Split command string into program + args for cross-platform compatibility.
    let mut parts = shlex::split(&cmd_str).unwrap_or_else(|| vec![cmd_str.clone()]);
    if parts.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty gate command",
        ));
    }
    let program = parts.remove(0);

    let output = std::process::Command::new(&program)
        .args(&parts)
        .current_dir(staging_dir)
        .output()?;

    let elapsed = start.elapsed().as_secs_f64();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(GateResult {
        gate: gate.clone(),
        passed: output.status.success(),
        exit_code: output.status.code(),
        output: combined,
        elapsed_secs: elapsed,
    })
}

/// Evaluate all gates in sequence. Returns Ok(()) if all pass, Err with detail on first failure.
pub fn evaluate_gates(
    gates: &[WorkflowGate],
    staging_dir: &Path,
    quiet: bool,
) -> Result<(), GateFailure> {
    for gate in gates {
        if !quiet {
            print!("  Gate [{}]: {} ... ", gate.name(), gate.command_str());
        }
        match run_gate(gate, staging_dir) {
            Ok(result) => {
                if result.passed {
                    if !quiet {
                        println!("PASS ({:.1}s)", result.elapsed_secs);
                    }
                } else {
                    if !quiet {
                        println!(
                            "FAIL (exit {}, {:.1}s)",
                            result.exit_code.unwrap_or(-1),
                            result.elapsed_secs
                        );
                        // Print last 2000 chars of output.
                        let out = if result.output.len() > 2000 {
                            format!("...{}", &result.output[result.output.len() - 2000..])
                        } else {
                            result.output.clone()
                        };
                        if !out.trim().is_empty() {
                            println!("{}", out);
                        }
                    }
                    return Err(GateFailure {
                        gate_name: gate.name().to_string(),
                        command: gate.command_str(),
                        exit_code: result.exit_code,
                        output: result.output,
                    });
                }
            }
            Err(e) => {
                if !quiet {
                    println!("ERROR: {}", e);
                }
                return Err(GateFailure {
                    gate_name: gate.name().to_string(),
                    command: gate.command_str(),
                    exit_code: None,
                    output: e.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Details about a gate failure.
#[derive(Debug)]
pub struct GateFailure {
    pub gate_name: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub output: String,
}

impl std::fmt::Display for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gate '{}' ({}) failed with exit code {:?}",
            self.gate_name, self.command, self.exit_code
        )
    }
}

// ── shlex helper (inline, no extra dep) ────────────────────────────────────

mod shlex {
    /// Minimally split a shell-like command string into tokens.
    ///
    /// Handles double-quoted strings and basic escapes. Returns None only if
    /// the input is empty; otherwise always returns at least one token.
    pub fn split(s: &str) -> Option<Vec<String>> {
        if s.trim().is_empty() {
            return None;
        }
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut in_quote = false;
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => in_quote = !in_quote,
                '\\' if in_quote => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ' ' | '\t' if !in_quote => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(c),
            }
        }
        if !current.is_empty() {
            tokens.push(current);
        }
        Some(tokens)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn workflow_gate_command_strings() {
        assert_eq!(WorkflowGate::Build.command_str(), "cargo build --workspace");
        assert_eq!(WorkflowGate::Test.command_str(), "cargo test --workspace");
        assert!(WorkflowGate::Clippy.command_str().contains("clippy"));
        let custom = WorkflowGate::Custom {
            command: "make check".to_string(),
        };
        assert_eq!(custom.command_str(), "make check");
    }

    #[test]
    fn workflow_gate_parse() {
        assert_eq!(WorkflowGate::parse("build"), WorkflowGate::Build);
        assert_eq!(WorkflowGate::parse("test"), WorkflowGate::Test);
        assert_eq!(WorkflowGate::parse("clippy"), WorkflowGate::Clippy);
        assert_eq!(
            WorkflowGate::parse("make check"),
            WorkflowGate::Custom {
                command: "make check".to_string()
            }
        );
    }

    #[test]
    fn workflow_gate_names() {
        assert_eq!(WorkflowGate::Build.name(), "build");
        assert_eq!(WorkflowGate::Test.name(), "test");
        assert_eq!(WorkflowGate::Clippy.name(), "clippy");
        assert_eq!(
            WorkflowGate::Custom {
                command: "x".to_string()
            }
            .name(),
            "custom"
        );
    }

    #[test]
    fn step_state_is_passed_and_failed() {
        let passed = StepState::Passed {
            goal_id: "abc".to_string(),
        };
        assert!(passed.is_passed());
        assert!(!passed.is_failed());

        let failed = StepState::GateFailed {
            goal_id: "abc".to_string(),
            failed_gate: "build".to_string(),
            error: "fail".to_string(),
        };
        assert!(!failed.is_passed());
        assert!(failed.is_failed());

        let agent_fail = StepState::AgentFailed {
            error: "exit 1".to_string(),
        };
        assert!(agent_fail.is_failed());
    }

    #[test]
    fn step_state_goal_id() {
        let s = StepState::Passed {
            goal_id: "gid-1".to_string(),
        };
        assert_eq!(s.goal_id(), Some("gid-1"));
        assert_eq!(StepState::Pending.goal_id(), None);
    }

    #[test]
    fn serial_phases_state_save_and_load() {
        let dir = tempdir().unwrap();
        let mut state = SerialPhasesState::new(
            "wf-1",
            vec!["v0.13.7.1".to_string(), "v0.13.7.2".to_string()],
            vec!["build".to_string(), "test".to_string()],
        );
        state.steps[0] = StepState::Passed {
            goal_id: "goal-abc".to_string(),
        };
        state.last_goal_id = Some("goal-abc".to_string());
        state.current_step = 1;

        state.save(dir.path()).unwrap();

        let loaded = SerialPhasesState::load(dir.path(), "wf-1").unwrap();
        assert_eq!(loaded.workflow_id, "wf-1");
        assert_eq!(loaded.phases.len(), 2);
        assert_eq!(loaded.current_step, 1);
        assert!(loaded.steps[0].is_passed());
        assert_eq!(loaded.last_goal_id.as_deref(), Some("goal-abc"));
    }

    #[test]
    fn serial_phases_state_load_latest() {
        let dir = tempdir().unwrap();

        let mut s1 = SerialPhasesState::new("wf-1", vec!["p1".to_string()], vec![]);
        s1.save(dir.path()).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut s2 = SerialPhasesState::new("wf-2", vec!["p2".to_string()], vec![]);
        s2.save(dir.path()).unwrap();

        let latest = SerialPhasesState::load_latest(dir.path()).unwrap();
        assert_eq!(latest.workflow_id, "wf-2");
    }

    #[test]
    fn serial_phases_state_passed_count_and_failed_step() {
        let mut state = SerialPhasesState::new(
            "wf-3",
            vec!["p1".to_string(), "p2".to_string(), "p3".to_string()],
            vec![],
        );
        state.steps[0] = StepState::Passed {
            goal_id: "g1".to_string(),
        };
        state.steps[1] = StepState::GateFailed {
            goal_id: "g2".to_string(),
            failed_gate: "build".to_string(),
            error: "fail".to_string(),
        };

        assert_eq!(state.passed_count(), 1);
        assert_eq!(state.failed_step(), Some(1));
        assert_eq!(state.next_pending_step(), Some(2));
    }

    #[test]
    fn run_gate_passing_command() {
        let dir = tempdir().unwrap();
        let gate = WorkflowGate::Custom {
            command: "echo hello".to_string(),
        };
        let result = run_gate(&gate, dir.path()).unwrap();
        assert!(result.passed);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn run_gate_failing_command() {
        let dir = tempdir().unwrap();
        // Use a command that exits non-zero.
        let gate = WorkflowGate::Custom {
            command: "false".to_string(),
        };
        let result = run_gate(&gate, dir.path()).unwrap();
        assert!(!result.passed);
    }

    #[test]
    fn evaluate_gates_all_pass() {
        let dir = tempdir().unwrap();
        let gates = vec![
            WorkflowGate::Custom {
                command: "echo gate1".to_string(),
            },
            WorkflowGate::Custom {
                command: "echo gate2".to_string(),
            },
        ];
        assert!(evaluate_gates(&gates, dir.path(), true).is_ok());
    }

    #[test]
    fn evaluate_gates_first_failure_stops() {
        let dir = tempdir().unwrap();
        let gates = vec![
            WorkflowGate::Custom {
                command: "false".to_string(),
            },
            WorkflowGate::Custom {
                command: "echo should_not_run".to_string(),
            },
        ];
        let result = evaluate_gates(&gates, dir.path(), true);
        assert!(result.is_err());
        let failure = result.unwrap_err();
        assert_eq!(failure.gate_name, "custom");
    }

    #[test]
    fn shlex_split_basic() {
        let tokens = shlex::split("cargo build --workspace").unwrap();
        assert_eq!(tokens, ["cargo", "build", "--workspace"]);
    }

    #[test]
    fn shlex_split_quoted() {
        let tokens = shlex::split("cargo clippy -- -D \"some warning\"").unwrap();
        assert_eq!(tokens[0], "cargo");
        assert_eq!(tokens[3], "-D");
        assert_eq!(tokens[4], "some warning");
    }
}
