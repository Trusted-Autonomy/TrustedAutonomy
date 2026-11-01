// operations.rs — Corrective action log (v0.13.1).
//
// The daemon watchdog emits CorrectiveAction proposals when it detects issues.
// They are persisted to `.ta/operations.jsonl` and surfaced via `ta operations log`.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::GoalError;

/// Severity of a corrective action proposal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionSeverity {
    Info,
    Warning,
    Critical,
}

/// Status of a corrective action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// Proposed, awaiting approval.
    Proposed,
    /// Approved by human or auto-heal policy.
    Approved { by: String },
    /// Denied by human.
    Denied { reason: String },
    /// Executed successfully.
    Executed { outcome: String },
    /// Execution failed.
    Failed { error: String },
}

/// A structured proposal for a corrective action.
///
/// The watchdog creates these when it detects issues. They are stored to
/// `.ta/operations.jsonl` and surfaced via `ta operations log`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectiveAction {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// One-line description of the issue detected.
    pub issue: String,
    /// Severity of the issue.
    pub severity: ActionSeverity,
    /// Human-readable diagnosis of what caused the issue.
    pub diagnosis: String,
    /// What action is proposed.
    pub proposed_action: String,
    /// Key that identifies this action type in the auto-heal `allowed` list.
    pub action_key: String,
    /// Whether this action can be auto-healed without human approval.
    pub auto_healable: bool,
    /// Current status of this action.
    pub status: ActionStatus,
    /// Goal ID this action is related to (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<Uuid>,
}

impl CorrectiveAction {
    pub fn new(
        issue: impl Into<String>,
        severity: ActionSeverity,
        diagnosis: impl Into<String>,
        proposed_action: impl Into<String>,
        action_key: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            created_at: now,
            updated_at: now,
            issue: issue.into(),
            severity,
            diagnosis: diagnosis.into(),
            proposed_action: proposed_action.into(),
            action_key: action_key.into(),
            auto_healable: false,
            status: ActionStatus::Proposed,
            goal_id: None,
        }
    }

    pub fn with_goal_id(mut self, goal_id: Uuid) -> Self {
        self.goal_id = Some(goal_id);
        self
    }

    pub fn set_auto_healable(mut self) -> Self {
        self.auto_healable = true;
        self
    }
}

/// Append-only log of corrective actions stored as JSONL at `.ta/operations.jsonl`.
pub struct OperationsLog {
    path: PathBuf,
}

impl OperationsLog {
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            path: project_root.join(".ta/operations.jsonl"),
        }
    }

    /// Append a corrective action entry.
    pub fn append(&self, action: &CorrectiveAction) -> Result<(), GoalError> {
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
        let json = serde_json::to_string(action)?;
        writeln!(file, "{}", json).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })
    }

    /// Read all entries, most recent first. Optionally limited.
    ///
    /// The file is append-only: an update to an action (see
    /// [`OperationsLog::resolve_proposed`]) appends a second record with the same
    /// `id`, and the last record for an id is the one returned.
    pub fn read(&self, limit: Option<usize>) -> Result<Vec<CorrectiveAction>, GoalError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut entries: Vec<CorrectiveAction> = Vec::new();
        let mut index: HashMap<Uuid, usize> = HashMap::new();
        for line in reader.lines() {
            let line = line.map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(action) = serde_json::from_str::<CorrectiveAction>(line) {
                match index.get(&action.id) {
                    Some(&i) => entries[i] = action,
                    None => {
                        index.insert(action.id, entries.len());
                        entries.push(action);
                    }
                }
            }
        }
        // Most recent first.
        entries.sort_by_key(|e| Reverse(e.created_at));
        if let Some(limit) = limit {
            entries.truncate(limit);
        }
        Ok(entries)
    }

    /// Mark every still-`Proposed` action accepted by `matches` as executed
    /// with `outcome`, by appending an updated record. Returns how many were
    /// resolved. Used when the condition that raised an action has cleared
    /// (for example disk space recovered) so it stops showing as pending.
    pub fn resolve_proposed(
        &self,
        matches: impl Fn(&CorrectiveAction) -> bool,
        outcome: &str,
    ) -> Result<usize, GoalError> {
        let open: Vec<CorrectiveAction> = self
            .read(None)?
            .into_iter()
            .filter(|a| matches!(a.status, ActionStatus::Proposed) && matches(a))
            .collect();
        for mut action in open.iter().cloned() {
            action.status = ActionStatus::Executed {
                outcome: outcome.to_string(),
            };
            action.updated_at = Utc::now();
            self.append(&action)?;
        }
        Ok(open.len())
    }
}

/// True for the watchdog's low-disk-space alert (critical, issue text
/// "Low disk space: ..."), which `ta status` and the watchdog treat as
/// cleared once free space is back above the threshold.
pub fn is_low_disk_alert(action: &CorrectiveAction) -> bool {
    action.severity == ActionSeverity::Critical && action.issue.starts_with("Low disk space")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn corrective_action_new() {
        let action = CorrectiveAction::new(
            "Goal xyz is zombie",
            ActionSeverity::Warning,
            "Agent process PID 12345 exited",
            "Transition to failed",
            "transition_zombie_to_failed",
        );
        assert!(matches!(action.status, ActionStatus::Proposed));
        assert!(!action.auto_healable);
    }

    #[test]
    fn corrective_action_with_goal_id() {
        let goal_id = Uuid::new_v4();
        let action = CorrectiveAction::new("Test", ActionSeverity::Info, "Diag", "Fix", "noop")
            .with_goal_id(goal_id);
        assert_eq!(action.goal_id, Some(goal_id));
    }

    #[test]
    fn corrective_action_auto_healable() {
        let action = CorrectiveAction::new(
            "Low disk",
            ActionSeverity::Critical,
            "< 2 GB",
            "Remove staging",
            "clean_applied_staging",
        )
        .set_auto_healable();
        assert!(action.auto_healable);
    }

    #[test]
    fn corrective_action_roundtrip() {
        let action = CorrectiveAction::new(
            "Low disk space",
            ActionSeverity::Critical,
            "Available disk < 2 GB",
            "Remove 3 stale staging dirs",
            "clean_applied_staging",
        )
        .set_auto_healable();
        let json = serde_json::to_string(&action).unwrap();
        let restored: CorrectiveAction = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, action.id);
        assert!(restored.auto_healable);
    }

    #[test]
    fn operations_log_append_read() {
        let dir = tempdir().unwrap();
        let log = OperationsLog::for_project(dir.path());

        let action = CorrectiveAction::new(
            "Test issue",
            ActionSeverity::Info,
            "Just testing",
            "Do nothing",
            "noop",
        );
        log.append(&action).unwrap();

        let entries = log.read(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, action.id);
    }

    #[test]
    fn operations_log_limit() {
        let dir = tempdir().unwrap();
        let log = OperationsLog::for_project(dir.path());

        for i in 0..5 {
            let action = CorrectiveAction::new(
                format!("Issue {}", i),
                ActionSeverity::Info,
                "Diagnosis",
                "Action",
                "noop",
            );
            log.append(&action).unwrap();
        }

        let entries = log.read(Some(3)).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn operations_log_empty() {
        let dir = tempdir().unwrap();
        let log = OperationsLog::for_project(dir.path());
        let entries = log.read(None).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn action_status_approved_roundtrip() {
        let mut action = CorrectiveAction::new("X", ActionSeverity::Warning, "D", "P", "key");
        action.status = ActionStatus::Approved {
            by: "auto-heal".to_string(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let restored: CorrectiveAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(restored.status, ActionStatus::Approved { .. }));
    }

    #[test]
    fn resolve_proposed_closes_only_matching_open_actions_and_keeps_one_record_per_id() {
        let dir = tempdir().unwrap();
        let log = OperationsLog::for_project(dir.path());
        let disk = CorrectiveAction::new(
            "Low disk space: 0.2 GB available",
            ActionSeverity::Critical,
            "D",
            "P",
            "clean_applied_staging",
        );
        let zombie = CorrectiveAction::new(
            "Goal xyz is zombie",
            ActionSeverity::Warning,
            "D",
            "P",
            "transition_zombie_to_failed",
        );
        log.append(&disk).unwrap();
        log.append(&zombie).unwrap();

        let n = log
            .resolve_proposed(is_low_disk_alert, "disk recovered")
            .unwrap();
        assert_eq!(n, 1);

        let entries = log.read(None).unwrap();
        assert_eq!(entries.len(), 2, "an update must not duplicate the action");
        let disk_now = entries.iter().find(|e| e.id == disk.id).unwrap();
        assert!(
            matches!(&disk_now.status, ActionStatus::Executed { outcome } if outcome == "disk recovered")
        );
        let zombie_now = entries.iter().find(|e| e.id == zombie.id).unwrap();
        assert!(matches!(zombie_now.status, ActionStatus::Proposed));

        // Nothing left to resolve: a second call is a no-op.
        assert_eq!(log.resolve_proposed(is_low_disk_alert, "again").unwrap(), 0);
    }
}
