// swarm.rs — Parallel agent swarm workflow (v0.13.7).
//
// Implements the `swarm` built-in workflow:
//   - Decomposes a goal into independent sub-goals, each with its own staging directory.
//   - Runs sub-goals concurrently (or sequentially for the initial implementation).
//   - Validates each sub-goal with per-agent gates.
//   - An optional integration step merges all sub-goal outputs.
//   - Persists state to `.ta/swarm-workflow-<id>.json` for progress tracking.
//
// Usage (via `ta run --workflow swarm --sub-goals "goal1" "goal2"`):
//   Sub-goal 1 (separate staging) → Sub-goal 2 (separate staging) →
//   Integration agent (merges) → single draft.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Sub-goal specification ──────────────────────────────────────────────────

/// A sub-goal in a swarm workflow.
///
/// Each sub-goal runs as an independent agent in its own staging directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGoalSpec {
    /// Short title for this sub-goal (used as the agent's goal title).
    pub title: String,
    /// Optional objective override. If None, the parent objective is used.
    #[serde(default)]
    pub objective: Option<String>,
    /// Optional plan phase ID (e.g., "v0.13.7.1").
    #[serde(default)]
    pub phase: Option<String>,
    /// Sub-goal titles that must complete (pass) before this sub-goal starts.
    ///
    /// Example: `depends_on: ["compile", "lint"]` — this sub-goal waits until
    /// both "compile" and "lint" sub-goals have passed.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl SubGoalSpec {
    /// Create a simple sub-goal with just a title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            objective: None,
            phase: None,
            depends_on: Vec::new(),
        }
    }

    /// Create a sub-goal for a specific plan phase.
    pub fn for_phase(phase: impl Into<String>) -> Self {
        let phase = phase.into();
        Self {
            title: format!("Implement {}", phase),
            objective: None,
            phase: Some(phase),
            depends_on: Vec::new(),
        }
    }

    /// Declare dependency titles — sub-goals that must pass before this one starts.
    pub fn with_deps(mut self, deps: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.depends_on = deps.into_iter().map(Into::into).collect();
        self
    }
}

// ── Sub-goal execution state ────────────────────────────────────────────────

/// Execution state of a single sub-goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SubGoalStatus {
    /// Not yet started.
    Pending,
    /// Agent is currently running.
    Running {
        goal_id: String,
        staging_path: PathBuf,
    },
    /// Agent completed and all per-agent gates passed.
    Passed {
        goal_id: String,
        staging_path: PathBuf,
    },
    /// Agent completed but per-agent gate failed.
    GateFailed {
        goal_id: String,
        staging_path: PathBuf,
        failed_gate: String,
        error: String,
    },
    /// Agent returned a non-zero exit code.
    AgentFailed { error: String },
    /// This sub-goal was skipped (e.g., because a dependency failed).
    Skipped { reason: String },
}

impl SubGoalStatus {
    /// Returns the goal_id if available.
    pub fn goal_id(&self) -> Option<&str> {
        match self {
            SubGoalStatus::Running { goal_id, .. }
            | SubGoalStatus::Passed { goal_id, .. }
            | SubGoalStatus::GateFailed { goal_id, .. } => Some(goal_id.as_str()),
            _ => None,
        }
    }

    /// Returns the staging path if available.
    pub fn staging_path(&self) -> Option<&Path> {
        match self {
            SubGoalStatus::Running { staging_path, .. }
            | SubGoalStatus::Passed { staging_path, .. }
            | SubGoalStatus::GateFailed { staging_path, .. } => Some(staging_path.as_path()),
            _ => None,
        }
    }

    pub fn is_passed(&self) -> bool {
        matches!(self, SubGoalStatus::Passed { .. })
    }

    pub fn is_failed(&self) -> bool {
        matches!(
            self,
            SubGoalStatus::AgentFailed { .. } | SubGoalStatus::GateFailed { .. }
        )
    }

    pub fn is_complete(&self) -> bool {
        matches!(
            self,
            SubGoalStatus::Passed { .. }
                | SubGoalStatus::AgentFailed { .. }
                | SubGoalStatus::GateFailed { .. }
                | SubGoalStatus::Skipped { .. }
        )
    }
}

// ── Swarm workflow state ────────────────────────────────────────────────────

/// Persisted state for a swarm workflow run.
///
/// Stored in `.ta/swarm-workflow-<id>.json`. Tracks sub-goal progress,
/// integration status, and allows resuming partial swarms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmState {
    /// Unique identifier for this swarm run.
    pub workflow_id: String,
    /// Parent goal title (the macro goal decomposed into sub-goals).
    pub parent_title: String,
    /// Sub-goal specifications (ordered).
    pub sub_goals: Vec<SubGoalSpec>,
    /// Per-sub-goal execution states (parallel to `sub_goals`).
    pub sub_goal_states: Vec<SubGoalStatus>,
    /// Whether to run an integration agent after all sub-goals complete.
    pub run_integration: bool,
    /// Goal ID of the integration agent, once started.
    #[serde(default)]
    pub integration_goal_id: Option<String>,
    /// Whether the integration step passed.
    #[serde(default)]
    pub integration_passed: bool,
    /// Gate command(s) to run after each sub-goal (empty = no gate).
    #[serde(default)]
    pub per_agent_gates: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SwarmState {
    /// Create a new swarm state.
    pub fn new(
        workflow_id: &str,
        parent_title: &str,
        sub_goals: Vec<SubGoalSpec>,
        run_integration: bool,
    ) -> Self {
        let n = sub_goals.len();
        Self {
            workflow_id: workflow_id.to_string(),
            parent_title: parent_title.to_string(),
            sub_goals,
            sub_goal_states: vec![SubGoalStatus::Pending; n],
            run_integration,
            integration_goal_id: None,
            integration_passed: false,
            per_agent_gates: Vec::new(),
            started_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Persist swarm state to disk.
    pub fn save(&mut self, dir: &Path) -> std::io::Result<()> {
        self.updated_at = Utc::now();
        let path = dir.join(format!("swarm-workflow-{}.json", self.workflow_id));
        let json =
            serde_json::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Load a saved swarm state from disk.
    pub fn load(dir: &Path, workflow_id: &str) -> Option<Self> {
        let path = dir.join(format!("swarm-workflow-{}.json", workflow_id));
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Find the latest swarm workflow state file, if any.
    pub fn load_latest(dir: &Path) -> Option<Self> {
        let entries = std::fs::read_dir(dir).ok()?;
        let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_name()?.to_str()?.to_string();
                if name.starts_with("swarm-workflow-") && name.ends_with(".json") {
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

    /// How many sub-goals have completed (passed, failed, or skipped).
    pub fn completed_count(&self) -> usize {
        self.sub_goal_states
            .iter()
            .filter(|s| s.is_complete())
            .count()
    }

    /// How many sub-goals passed.
    pub fn passed_count(&self) -> usize {
        self.sub_goal_states
            .iter()
            .filter(|s| s.is_passed())
            .count()
    }

    /// How many sub-goals failed.
    pub fn failed_count(&self) -> usize {
        self.sub_goal_states
            .iter()
            .filter(|s| s.is_failed())
            .count()
    }

    /// Returns true when all sub-goals are complete (regardless of pass/fail).
    pub fn all_complete(&self) -> bool {
        self.sub_goal_states.iter().all(|s| s.is_complete())
    }

    /// Collect the staging paths of all passed sub-goals.
    pub fn passed_staging_paths(&self) -> Vec<&Path> {
        self.sub_goal_states
            .iter()
            .filter_map(|s| {
                if let SubGoalStatus::Passed { staging_path, .. } = s {
                    Some(staging_path.as_path())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Print a progress summary to stdout.
    pub fn print_summary(&self) {
        println!(
            "Swarm {}: {}/{} sub-goals complete, {}/{} passed",
            self.workflow_id,
            self.completed_count(),
            self.sub_goals.len(),
            self.passed_count(),
            self.sub_goals.len(),
        );
        for (i, (spec, status)) in self.sub_goals.iter().zip(&self.sub_goal_states).enumerate() {
            let indicator = match status {
                SubGoalStatus::Pending => "⏳",
                SubGoalStatus::Running { .. } => "🔄",
                SubGoalStatus::Passed { .. } => "✅",
                SubGoalStatus::GateFailed { .. } | SubGoalStatus::AgentFailed { .. } => "❌",
                SubGoalStatus::Skipped { .. } => "⏭",
            };
            let deps = if spec.depends_on.is_empty() {
                String::new()
            } else {
                format!(" [after: {}]", spec.depends_on.join(", "))
            };
            println!(
                "  [{}/{}] {} {}{}",
                i + 1,
                self.sub_goals.len(),
                indicator,
                spec.title,
                deps
            );
        }
    }

    // ── Dependency graph scheduling (v0.13.16) ──────────────────────────────

    /// Return the indices of sub-goals that are ready to start.
    ///
    /// A sub-goal is ready when:
    ///   1. Its current status is `Pending`.
    ///   2. All sub-goals listed in `depends_on` have status `Passed`.
    ///
    /// If any dependency has failed (AgentFailed/GateFailed), the sub-goal
    /// should be skipped — call `mark_dependency_failed_skips()` first.
    pub fn ready_indices(&self) -> Vec<usize> {
        self.sub_goals
            .iter()
            .enumerate()
            .filter_map(|(i, spec)| {
                if !matches!(self.sub_goal_states[i], SubGoalStatus::Pending) {
                    return None;
                }
                // Check all declared dependencies are passed.
                let all_deps_passed = spec.depends_on.iter().all(|dep_title| {
                    self.sub_goals
                        .iter()
                        .enumerate()
                        .find(|(_, s)| s.title == *dep_title)
                        .map(|(j, _)| self.sub_goal_states[j].is_passed())
                        .unwrap_or(true) // unknown dep → treat as passed (no-op)
                });
                if all_deps_passed {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Mark any `Pending` sub-goal as `Skipped` if one of its dependencies has failed.
    ///
    /// Call this after any sub-goal transitions to a failed state to propagate
    /// the skip to its dependents.
    pub fn mark_dependency_failed_skips(&mut self) {
        let n = self.sub_goals.len();
        for i in 0..n {
            if !matches!(self.sub_goal_states[i], SubGoalStatus::Pending) {
                continue;
            }
            let dep_failed = self.sub_goals[i].depends_on.iter().any(|dep_title| {
                self.sub_goals
                    .iter()
                    .enumerate()
                    .find(|(_, s)| s.title == *dep_title)
                    .map(|(j, _)| self.sub_goal_states[j].is_failed())
                    .unwrap_or(false)
            });
            if dep_failed {
                self.sub_goal_states[i] = SubGoalStatus::Skipped {
                    reason: "dependency failed".to_string(),
                };
            }
        }
    }

    /// Validate the dependency graph for cycles and unknown titles.
    ///
    /// Returns `Err` with a human-readable description if any problem is found.
    pub fn validate_dependencies(&self) -> Result<(), String> {
        let titles: std::collections::HashSet<&str> =
            self.sub_goals.iter().map(|s| s.title.as_str()).collect();

        // Check for unknown dependency titles.
        for spec in &self.sub_goals {
            for dep in &spec.depends_on {
                if !titles.contains(dep.as_str()) {
                    return Err(format!(
                        "Sub-goal '{}' declares unknown dependency '{}'. \
                         Available sub-goals: {}",
                        spec.title,
                        dep,
                        self.sub_goals
                            .iter()
                            .map(|s| s.title.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }

        // Check for cycles using DFS.
        let n = self.sub_goals.len();
        // Build adjacency: index → indices of direct dependencies.
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, spec) in self.sub_goals.iter().enumerate() {
            for dep in &spec.depends_on {
                if let Some(j) = self.sub_goals.iter().position(|s| s.title == *dep) {
                    adj[i].push(j);
                }
            }
        }

        // 0=unvisited, 1=in-stack, 2=done
        let mut state = vec![0u8; n];
        fn has_cycle(node: usize, adj: &[Vec<usize>], state: &mut Vec<u8>) -> bool {
            state[node] = 1;
            for &next in &adj[node] {
                if state[next] == 1 || (state[next] == 0 && has_cycle(next, adj, state)) {
                    return true;
                }
            }
            state[node] = 2;
            false
        }

        for i in 0..n {
            if state[i] == 0 && has_cycle(i, &adj, &mut state) {
                return Err(format!(
                    "Cycle detected in sub-goal dependencies involving '{}'.",
                    self.sub_goals[i].title
                ));
            }
        }

        Ok(())
    }
}

// ── Integration config ──────────────────────────────────────────────────────

/// Configuration for the integration agent that merges swarm outputs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntegrationConfig {
    /// Prompt prefix given to the integration agent.
    #[serde(default)]
    pub prompt: String,
    /// Whether to require all sub-goals to pass before running integration.
    /// If false, integration runs even if some sub-goals failed.
    #[serde(default = "default_true")]
    pub require_all_passed: bool,
}

fn default_true() -> bool {
    true
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sub_goal_spec_new() {
        let spec = SubGoalSpec::new("Add feature X");
        assert_eq!(spec.title, "Add feature X");
        assert!(spec.objective.is_none());
        assert!(spec.phase.is_none());
    }

    #[test]
    fn sub_goal_spec_for_phase() {
        let spec = SubGoalSpec::for_phase("v0.13.7.1");
        assert!(spec.title.contains("v0.13.7.1"));
        assert_eq!(spec.phase.as_deref(), Some("v0.13.7.1"));
    }

    #[test]
    fn sub_goal_status_accessors() {
        let passed = SubGoalStatus::Passed {
            goal_id: "g1".to_string(),
            staging_path: PathBuf::from("/tmp/staging"),
        };
        assert!(passed.is_passed());
        assert!(!passed.is_failed());
        assert!(passed.is_complete());
        assert_eq!(passed.goal_id(), Some("g1"));
        assert_eq!(passed.staging_path(), Some(Path::new("/tmp/staging")));

        let pending = SubGoalStatus::Pending;
        assert!(!pending.is_passed());
        assert!(!pending.is_failed());
        assert!(!pending.is_complete());
        assert!(pending.goal_id().is_none());
    }

    #[test]
    fn swarm_state_counters() {
        let sub_goals = vec![
            SubGoalSpec::new("goal1"),
            SubGoalSpec::new("goal2"),
            SubGoalSpec::new("goal3"),
        ];
        let mut state = SwarmState::new("sw-1", "Parent", sub_goals, false);

        state.sub_goal_states[0] = SubGoalStatus::Passed {
            goal_id: "g1".to_string(),
            staging_path: PathBuf::from("/tmp/s1"),
        };
        state.sub_goal_states[1] = SubGoalStatus::AgentFailed {
            error: "exit 1".to_string(),
        };

        assert_eq!(state.completed_count(), 2);
        assert_eq!(state.passed_count(), 1);
        assert_eq!(state.failed_count(), 1);
        assert!(!state.all_complete());
    }

    #[test]
    fn swarm_state_all_complete() {
        let sub_goals = vec![SubGoalSpec::new("g1"), SubGoalSpec::new("g2")];
        let mut state = SwarmState::new("sw-2", "Parent", sub_goals, false);
        state.sub_goal_states[0] = SubGoalStatus::Passed {
            goal_id: "g1".to_string(),
            staging_path: PathBuf::from("/tmp/s1"),
        };
        state.sub_goal_states[1] = SubGoalStatus::Skipped {
            reason: "dependency failed".to_string(),
        };
        assert!(state.all_complete());
    }

    #[test]
    fn swarm_state_save_and_load() {
        let dir = tempdir().unwrap();
        let sub_goals = vec![SubGoalSpec::new("g1"), SubGoalSpec::new("g2")];
        let mut state = SwarmState::new("sw-3", "My swarm", sub_goals, true);
        state.sub_goal_states[0] = SubGoalStatus::Passed {
            goal_id: "goal-1".to_string(),
            staging_path: PathBuf::from("/tmp/stg1"),
        };
        state.save(dir.path()).unwrap();

        let loaded = SwarmState::load(dir.path(), "sw-3").unwrap();
        assert_eq!(loaded.workflow_id, "sw-3");
        assert_eq!(loaded.parent_title, "My swarm");
        assert_eq!(loaded.sub_goals.len(), 2);
        assert_eq!(loaded.passed_count(), 1);
    }

    #[test]
    fn swarm_state_passed_staging_paths() {
        let sub_goals = vec![SubGoalSpec::new("g1"), SubGoalSpec::new("g2")];
        let mut state = SwarmState::new("sw-4", "Parent", sub_goals, false);
        state.sub_goal_states[0] = SubGoalStatus::Passed {
            goal_id: "g1".to_string(),
            staging_path: PathBuf::from("/tmp/staging1"),
        };
        state.sub_goal_states[1] = SubGoalStatus::AgentFailed {
            error: "fail".to_string(),
        };

        let paths = state.passed_staging_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], Path::new("/tmp/staging1"));
    }

    // ── Dependency graph tests (v0.13.16) ────────────────────────────────────

    #[test]
    fn sub_goal_spec_with_deps() {
        let spec = SubGoalSpec::new("tests").with_deps(["compile", "lint"]);
        assert_eq!(spec.depends_on, vec!["compile", "lint"]);
    }

    #[test]
    fn ready_indices_no_deps() {
        // With no dependencies, all Pending sub-goals are ready immediately.
        let sub_goals = vec![
            SubGoalSpec::new("a"),
            SubGoalSpec::new("b"),
            SubGoalSpec::new("c"),
        ];
        let state = SwarmState::new("sw-dep-1", "Parent", sub_goals, false);
        let ready = state.ready_indices();
        assert_eq!(ready, vec![0, 1, 2]);
    }

    #[test]
    fn ready_indices_respects_pending_dep() {
        // "b" depends on "a". "a" is still Pending, so "b" is not ready.
        let sub_goals = vec![
            SubGoalSpec::new("a"),
            SubGoalSpec::new("b").with_deps(["a"]),
        ];
        let state = SwarmState::new("sw-dep-2", "Parent", sub_goals, false);
        let ready = state.ready_indices();
        assert_eq!(ready, vec![0]); // Only "a" is ready.
    }

    #[test]
    fn ready_indices_after_dep_passes() {
        // "b" depends on "a". Once "a" passes, "b" becomes ready.
        let sub_goals = vec![
            SubGoalSpec::new("a"),
            SubGoalSpec::new("b").with_deps(["a"]),
        ];
        let mut state = SwarmState::new("sw-dep-3", "Parent", sub_goals, false);
        state.sub_goal_states[0] = SubGoalStatus::Passed {
            goal_id: "g-a".to_string(),
            staging_path: PathBuf::from("/tmp/stg-a"),
        };
        let ready = state.ready_indices();
        assert_eq!(ready, vec![1]); // "b" is now ready.
    }

    #[test]
    fn mark_dependency_failed_skips_propagates() {
        // "b" depends on "a". After "a" fails, "b" should be skipped.
        let sub_goals = vec![
            SubGoalSpec::new("a"),
            SubGoalSpec::new("b").with_deps(["a"]),
        ];
        let mut state = SwarmState::new("sw-dep-4", "Parent", sub_goals, false);
        state.sub_goal_states[0] = SubGoalStatus::AgentFailed {
            error: "exit 1".to_string(),
        };
        state.mark_dependency_failed_skips();
        assert!(matches!(
            state.sub_goal_states[1],
            SubGoalStatus::Skipped { .. }
        ));
    }

    #[test]
    fn validate_dependencies_unknown_dep_errors() {
        let sub_goals = vec![
            SubGoalSpec::new("a"),
            SubGoalSpec::new("b").with_deps(["nonexistent"]),
        ];
        let state = SwarmState::new("sw-val-1", "Parent", sub_goals, false);
        let result = state.validate_dependencies();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown dependency"));
    }

    #[test]
    fn validate_dependencies_cycle_detected() {
        // a → b → a is a cycle.
        let sub_goals = vec![
            SubGoalSpec::new("a").with_deps(["b"]),
            SubGoalSpec::new("b").with_deps(["a"]),
        ];
        let state = SwarmState::new("sw-val-2", "Parent", sub_goals, false);
        let result = state.validate_dependencies();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cycle"));
    }

    #[test]
    fn validate_dependencies_valid_dag_ok() {
        // a → b → c (valid linear chain).
        let sub_goals = vec![
            SubGoalSpec::new("a"),
            SubGoalSpec::new("b").with_deps(["a"]),
            SubGoalSpec::new("c").with_deps(["b"]),
        ];
        let state = SwarmState::new("sw-val-3", "Parent", sub_goals, false);
        assert!(state.validate_dependencies().is_ok());
    }

    #[test]
    fn swarm_state_serializes_with_deps() {
        let dir = tempdir().unwrap();
        let sub_goals = vec![
            SubGoalSpec::new("a"),
            SubGoalSpec::new("b").with_deps(["a"]),
        ];
        let mut state = SwarmState::new("sw-ser-1", "Parent", sub_goals, false);
        state.save(dir.path()).unwrap();
        let loaded = SwarmState::load(dir.path(), "sw-ser-1").unwrap();
        assert_eq!(loaded.sub_goals[1].depends_on, vec!["a"]);
    }
}
