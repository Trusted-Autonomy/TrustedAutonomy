// velocity.rs — Feature Velocity Stats & Outcome Telemetry (v0.13.10).
//
// Tracks per-goal timing and outcome data in `.ta/velocity-stats.json`.
// Append-only: one entry per goal terminal outcome.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::GoalError;
use crate::goal_run::GoalRun;

/// Outcome of a goal run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalOutcome {
    Applied,
    Denied,
    Cancelled,
    Failed,
    Timeout,
}

impl std::fmt::Display for GoalOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoalOutcome::Applied => write!(f, "applied"),
            GoalOutcome::Denied => write!(f, "denied"),
            GoalOutcome::Cancelled => write!(f, "cancelled"),
            GoalOutcome::Failed => write!(f, "failed"),
            GoalOutcome::Timeout => write!(f, "timeout"),
        }
    }
}

/// A single goal's velocity record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VelocityEntry {
    pub goal_id: Uuid,
    pub title: String,
    /// Workflow type: "code", "doc", "qa", etc.
    #[serde(default)]
    pub workflow: String,
    /// Agent backend used.
    pub agent: String,
    /// Optional plan phase link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_phase: Option<String>,
    pub outcome: GoalOutcome,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_ready_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Seconds from start to pr_ready (agent build time).
    pub build_seconds: i64,
    /// Seconds from pr_ready to applied/denied (review time).
    #[serde(default)]
    pub review_seconds: i64,
    /// Total seconds from start to terminal state.
    pub total_seconds: i64,
    /// Whether a human amended any artifact before apply.
    #[serde(default)]
    pub amended: bool,
    /// Number of follow-up goals spawned from this one.
    #[serde(default)]
    pub follow_up_count: u32,
    /// Sum of follow-up goal build_seconds (rework cost).
    #[serde(default)]
    pub rework_seconds: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
}

impl VelocityEntry {
    /// Build a VelocityEntry from a GoalRun at terminal state.
    pub fn from_goal(goal: &GoalRun, outcome: GoalOutcome) -> Self {
        let now = Utc::now();
        let total_seconds = (now - goal.created_at).num_seconds().max(0);

        // pr_ready_at is not stored on GoalRun — use updated_at for applied/denied,
        // and estimate build time as total (review time is unknown without pr_ready_at).
        let build_seconds = match outcome {
            GoalOutcome::Applied | GoalOutcome::Denied => {
                // Best effort: use updated_at as the completion time
                (now - goal.created_at).num_seconds().max(0)
            }
            _ => total_seconds,
        };

        Self {
            goal_id: goal.goal_run_id,
            title: goal.title.clone(),
            workflow: String::new(),
            agent: goal.agent_id.clone(),
            plan_phase: goal.plan_phase.clone(),
            outcome,
            started_at: goal.created_at,
            pr_ready_at: None,
            completed_at: Some(now),
            build_seconds,
            review_seconds: 0,
            total_seconds,
            amended: false,
            follow_up_count: 0,
            rework_seconds: 0,
            denial_reason: None,
            cancel_reason: None,
        }
    }

    /// Set the denial reason (for Denied outcome).
    pub fn with_denial_reason(mut self, reason: impl Into<String>) -> Self {
        self.denial_reason = Some(reason.into());
        self
    }

    /// Set the cancel reason (for Cancelled outcome).
    pub fn with_cancel_reason(mut self, reason: impl Into<String>) -> Self {
        self.cancel_reason = Some(reason.into());
        self
    }

    /// Set the workflow type.
    pub fn with_workflow(mut self, workflow: impl Into<String>) -> Self {
        self.workflow = workflow.into();
        self
    }
}

/// Aggregate statistics over a set of velocity entries.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VelocityAggregate {
    pub total_goals: usize,
    pub applied: usize,
    pub denied: usize,
    pub cancelled: usize,
    pub failed: usize,
    pub avg_build_seconds: i64,
    pub avg_rework_seconds: i64,
    pub p90_build_seconds: i64,
    pub total_rework_seconds: i64,
}

impl VelocityAggregate {
    pub fn from_entries(entries: &[VelocityEntry]) -> Self {
        let total_goals = entries.len();
        let applied = entries
            .iter()
            .filter(|e| e.outcome == GoalOutcome::Applied)
            .count();
        let denied = entries
            .iter()
            .filter(|e| e.outcome == GoalOutcome::Denied)
            .count();
        let cancelled = entries
            .iter()
            .filter(|e| e.outcome == GoalOutcome::Cancelled)
            .count();
        let failed = entries
            .iter()
            .filter(|e| matches!(e.outcome, GoalOutcome::Failed | GoalOutcome::Timeout))
            .count();

        let total_rework_seconds: i64 = entries.iter().map(|e| e.rework_seconds).sum();

        let avg_build_seconds = if total_goals > 0 {
            entries.iter().map(|e| e.build_seconds).sum::<i64>() / total_goals as i64
        } else {
            0
        };

        let avg_rework_seconds = if total_goals > 0 {
            total_rework_seconds / total_goals as i64
        } else {
            0
        };

        let mut build_times: Vec<i64> = entries.iter().map(|e| e.build_seconds).collect();
        build_times.sort_unstable();
        let p90_build_seconds = if build_times.is_empty() {
            0
        } else {
            let idx = ((build_times.len() as f64 * 0.9) as usize).min(build_times.len() - 1);
            build_times[idx]
        };

        Self {
            total_goals,
            applied,
            denied,
            cancelled,
            failed,
            avg_build_seconds,
            avg_rework_seconds,
            p90_build_seconds,
            total_rework_seconds,
        }
    }
}

/// The velocity stats file: `.ta/velocity-stats.jsonl` (one JSON entry per line).
pub struct VelocityStore {
    path: PathBuf,
}

impl VelocityStore {
    /// Create a store backed by the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Convenience constructor: `.ta/velocity-stats.jsonl` relative to project root.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self::new(project_root.as_ref().join(".ta/velocity-stats.jsonl"))
    }

    /// Append an entry to the store.
    pub fn append(&self, entry: &VelocityEntry) -> Result<(), GoalError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| GoalError::IoError {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{}", line).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Load all entries from the store.
    pub fn load_all(&self) -> Result<Vec<VelocityEntry>, GoalError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<VelocityEntry>(trimmed) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Load entries since the given date (UTC).
    pub fn load_since(&self, since: DateTime<Utc>) -> Result<Vec<VelocityEntry>, GoalError> {
        let all = self.load_all()?;
        Ok(all.into_iter().filter(|e| e.started_at >= since).collect())
    }

    /// Load entries filtered by outcome.
    pub fn load_by_outcome(&self, outcome: &GoalOutcome) -> Result<Vec<VelocityEntry>, GoalError> {
        let all = self.load_all()?;
        Ok(all.into_iter().filter(|e| &e.outcome == outcome).collect())
    }

    /// Compute aggregate stats over all stored entries.
    pub fn aggregate(&self) -> Result<VelocityAggregate, GoalError> {
        let all = self.load_all()?;
        Ok(VelocityAggregate::from_entries(&all))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_goal() -> GoalRun {
        GoalRun::new(
            "Test Goal",
            "test objective",
            "claude-code",
            PathBuf::from("/tmp/staging"),
            PathBuf::from("/tmp/store"),
        )
    }

    #[test]
    fn velocity_entry_from_goal() {
        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
        assert_eq!(entry.goal_id, goal.goal_run_id);
        assert_eq!(entry.outcome, GoalOutcome::Applied);
        assert!(entry.total_seconds >= 0);
    }

    #[test]
    fn velocity_store_append_and_load() {
        let dir = tempdir().unwrap();
        let store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));

        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
        store.append(&entry).unwrap();

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].goal_id, goal.goal_run_id);
    }

    #[test]
    fn velocity_store_empty_when_no_file() {
        let dir = tempdir().unwrap();
        let store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));
        let loaded = store.load_all().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn aggregate_calculates_correctly() {
        let dir = tempdir().unwrap();
        let store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));

        for _ in 0..3 {
            let goal = make_goal();
            let mut entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
            entry.build_seconds = 600;
            store.append(&entry).unwrap();
        }
        let goal = make_goal();
        let mut entry = VelocityEntry::from_goal(&goal, GoalOutcome::Failed);
        entry.build_seconds = 300;
        store.append(&entry).unwrap();

        let agg = store.aggregate().unwrap();
        assert_eq!(agg.total_goals, 4);
        assert_eq!(agg.applied, 3);
        assert_eq!(agg.failed, 1);
        assert!(agg.avg_build_seconds > 0);
    }
}
