// history.rs — Goal history ledger (v0.9.8.1).
//
// When goals are GC'd or completed, a compact summary is appended to
// `.ta/goal-history.jsonl`. This preserves queryable history even after
// goal JSON files are archived or deleted.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use std::cmp::Reverse;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::GoalError;
use crate::goal_run::GoalRun;

/// Compact summary of a goal for the history ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalHistoryEntry {
    pub id: Uuid,
    pub title: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub agent: String,
    pub created: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<DateTime<Utc>>,
    /// Duration from created to updated_at in minutes.
    pub duration_mins: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub artifact_count: usize,
    #[serde(default)]
    pub lines_changed: usize,
}

impl GoalHistoryEntry {
    /// Create a history entry from a GoalRun.
    pub fn from_goal(goal: &GoalRun) -> Self {
        let duration = goal
            .updated_at
            .signed_duration_since(goal.created_at)
            .num_minutes();
        Self {
            id: goal.goal_run_id,
            title: goal.title.clone(),
            state: goal.state.to_string(),
            phase: goal.plan_phase.clone(),
            agent: goal.agent_id.clone(),
            created: goal.created_at,
            completed: if goal.state.to_string() == "applied"
                || goal.state.to_string() == "completed"
            {
                Some(goal.updated_at)
            } else {
                None
            },
            duration_mins: duration,
            draft_id: goal.pr_package_id.map(|id| id.to_string()),
            artifact_count: 0,
            lines_changed: 0,
        }
    }
}

/// Append-only history ledger stored as JSONL.
pub struct GoalHistoryLedger {
    path: PathBuf,
}

impl GoalHistoryLedger {
    /// Open or create a history ledger at the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// For a standard .ta project layout.
    pub fn for_project(project_root: &Path) -> Self {
        Self::new(project_root.join(".ta/goal-history.jsonl"))
    }

    /// Append a history entry.
    pub fn append(&self, entry: &GoalHistoryEntry) -> Result<(), GoalError> {
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

        let json = serde_json::to_string(entry)?;
        writeln!(file, "{}", json).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Read all entries, optionally filtered.
    pub fn read(&self, filter: &HistoryFilter) -> Result<Vec<GoalHistoryEntry>, GoalError> {
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
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<GoalHistoryEntry>(line) {
                if filter.matches(&entry) {
                    entries.push(entry);
                }
            }
        }

        // Most recent first.
        entries.sort_by_key(|e| Reverse(e.created));

        // Apply limit.
        if let Some(limit) = filter.limit {
            entries.truncate(limit);
        }

        Ok(entries)
    }
}

/// Filter for querying the history ledger.
#[derive(Debug, Default)]
pub struct HistoryFilter {
    pub phase: Option<String>,
    pub agent: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

impl HistoryFilter {
    fn matches(&self, entry: &GoalHistoryEntry) -> bool {
        if let Some(ref phase) = self.phase {
            if entry.phase.as_deref() != Some(phase.as_str()) {
                return false;
            }
        }
        if let Some(ref agent) = self.agent {
            if entry.agent != *agent {
                return false;
            }
        }
        if let Some(since) = self.since {
            if entry.created < since {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_entry(title: &str, phase: Option<&str>) -> GoalHistoryEntry {
        GoalHistoryEntry {
            id: Uuid::new_v4(),
            title: title.to_string(),
            state: "applied".to_string(),
            phase: phase.map(|s| s.to_string()),
            agent: "claude-code".to_string(),
            created: Utc::now(),
            completed: Some(Utc::now()),
            duration_mins: 42,
            draft_id: Some(Uuid::new_v4().to_string()),
            artifact_count: 15,
            lines_changed: 487,
        }
    }

    #[test]
    fn append_and_read_round_trip() {
        let dir = tempdir().unwrap();
        let ledger = GoalHistoryLedger::new(dir.path().join("history.jsonl"));

        let entry = make_entry("Test goal", Some("v0.9.8.1"));
        ledger.append(&entry).unwrap();

        let entries = ledger.read(&HistoryFilter::default()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Test goal");
        assert_eq!(entries[0].phase, Some("v0.9.8.1".to_string()));
    }

    #[test]
    fn filter_by_phase() {
        let dir = tempdir().unwrap();
        let ledger = GoalHistoryLedger::new(dir.path().join("history.jsonl"));

        ledger.append(&make_entry("A", Some("v0.9.8"))).unwrap();
        ledger.append(&make_entry("B", Some("v0.9.8.1"))).unwrap();
        ledger.append(&make_entry("C", None)).unwrap();

        let filter = HistoryFilter {
            phase: Some("v0.9.8.1".to_string()),
            ..Default::default()
        };
        let entries = ledger.read(&filter).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "B");
    }

    #[test]
    fn filter_by_agent() {
        let dir = tempdir().unwrap();
        let ledger = GoalHistoryLedger::new(dir.path().join("history.jsonl"));

        let mut entry = make_entry("A", None);
        entry.agent = "codex".to_string();
        ledger.append(&entry).unwrap();
        ledger.append(&make_entry("B", None)).unwrap();

        let filter = HistoryFilter {
            agent: Some("codex".to_string()),
            ..Default::default()
        };
        let entries = ledger.read(&filter).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "A");
    }

    #[test]
    fn limit_results() {
        let dir = tempdir().unwrap();
        let ledger = GoalHistoryLedger::new(dir.path().join("history.jsonl"));

        for i in 0..10 {
            ledger
                .append(&make_entry(&format!("Goal {}", i), None))
                .unwrap();
        }

        let filter = HistoryFilter {
            limit: Some(3),
            ..Default::default()
        };
        let entries = ledger.read(&filter).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn empty_ledger_returns_empty() {
        let dir = tempdir().unwrap();
        let ledger = GoalHistoryLedger::new(dir.path().join("nonexistent.jsonl"));
        let entries = ledger.read(&HistoryFilter::default()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn serialization_round_trip() {
        let entry = make_entry("Test", Some("v0.9.8.1"));
        let json = serde_json::to_string(&entry).unwrap();
        let restored: GoalHistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry.id, restored.id);
        assert_eq!(entry.title, restored.title);
    }
}
