// capture.rs — ActionCapture log for the External Action Governance Framework.
//
// Every attempted external action is captured to a JSONL log regardless of
// policy outcome (executed, captured for review, blocked, dry-run). This
// provides a complete, tamper-evident audit trail of what the agent tried.
//
// The log is stored at `.ta/action-log.jsonl` in the workspace root.
// Each line is a JSON-serialized `CapturedAction`.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::policy::ActionPolicy;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("failed to write to action log at {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to read action log at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to serialize action: {0}")]
    Serialize(#[from] serde_json::Error),
}

// ── Outcome ──────────────────────────────────────────────────────────────────

/// What happened to an action request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionOutcome {
    /// The action was executed by a plugin (policy=auto).
    Executed {
        /// Plugin's return value.
        result: serde_json::Value,
    },
    /// The action was captured and added to the draft for human review.
    CapturedForReview,
    /// The action was blocked by policy and not executed.
    Blocked { reason: String },
    /// Dry-run mode: the action was logged but not executed or captured.
    DryRun,
    /// The action was rejected because the rate limit was exceeded.
    RateLimited { limit: u32, current: u32 },
}

impl std::fmt::Display for ActionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionOutcome::Executed { .. } => write!(f, "executed"),
            ActionOutcome::CapturedForReview => write!(f, "captured_for_review"),
            ActionOutcome::Blocked { .. } => write!(f, "blocked"),
            ActionOutcome::DryRun => write!(f, "dry_run"),
            ActionOutcome::RateLimited { .. } => write!(f, "rate_limited"),
        }
    }
}

// ── CapturedAction ────────────────────────────────────────────────────────────

/// A single action attempt recorded in the capture log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedAction {
    /// Unique ID for this capture record.
    pub capture_id: Uuid,

    /// The action type name (e.g., `"email"`, `"api_call"`).
    pub action_type: String,

    /// The full payload provided by the agent.
    pub payload: serde_json::Value,

    /// Goal run this action was requested under (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_run_id: Option<Uuid>,

    /// Human-readable goal title for log readability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_title: Option<String>,

    /// When the action was attempted.
    pub timestamp: DateTime<Utc>,

    /// Which policy was applied.
    pub policy: ActionPolicy,

    /// What happened as a result.
    pub outcome: ActionOutcome,

    /// Whether this was a dry-run request.
    pub dry_run: bool,
}

impl CapturedAction {
    /// Create a new capture record with a fresh UUID and current timestamp.
    pub fn new(
        action_type: impl Into<String>,
        payload: serde_json::Value,
        goal_run_id: Option<Uuid>,
        goal_title: Option<String>,
        policy: ActionPolicy,
        outcome: ActionOutcome,
        dry_run: bool,
    ) -> Self {
        Self {
            capture_id: Uuid::new_v4(),
            action_type: action_type.into(),
            payload,
            goal_run_id,
            goal_title,
            timestamp: Utc::now(),
            policy,
            outcome,
            dry_run,
        }
    }
}

// ── ActionCapture ─────────────────────────────────────────────────────────────

/// Append-only JSONL log for captured external actions.
///
/// Stored at `.ta/action-log.jsonl` in the workspace root.
pub struct ActionCapture {
    log_path: PathBuf,
}

impl ActionCapture {
    /// Create an `ActionCapture` targeting the given workspace `.ta/` directory.
    pub fn new(ta_dir: &Path) -> Self {
        Self {
            log_path: ta_dir.join("action-log.jsonl"),
        }
    }

    /// Append a captured action to the log.
    pub fn append(&self, action: &CapturedAction) -> Result<(), CaptureError> {
        let mut line = serde_json::to_string(action)?;
        line.push('\n');

        if let Some(parent) = self.log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CaptureError::Write {
                path: self.log_path.clone(),
                source: e,
            })?;
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| CaptureError::Write {
                path: self.log_path.clone(),
                source: e,
            })?;

        file.write_all(line.as_bytes())
            .map_err(|e| CaptureError::Write {
                path: self.log_path.clone(),
                source: e,
            })?;

        tracing::debug!(
            capture_id = %action.capture_id,
            action_type = %action.action_type,
            outcome = %action.outcome,
            "action captured"
        );

        Ok(())
    }

    /// Query captured actions, optionally filtered by goal run ID.
    ///
    /// Returns entries in append order (oldest first). Returns an empty vec
    /// if the log does not yet exist.
    pub fn query(&self, goal_run_id: Option<Uuid>) -> Result<Vec<CapturedAction>, CaptureError> {
        if !self.log_path.exists() {
            return Ok(vec![]);
        }

        let content = std::fs::read_to_string(&self.log_path).map_err(|e| CaptureError::Read {
            path: self.log_path.clone(),
            source: e,
        })?;

        let mut results = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<CapturedAction>(line) {
                Ok(entry) => {
                    if let Some(filter_id) = goal_run_id {
                        if entry.goal_run_id != Some(filter_id) {
                            continue;
                        }
                    }
                    results.push(entry);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "skipping malformed action log entry");
                }
            }
        }

        Ok(results)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_capture(dir: &Path) -> ActionCapture {
        ActionCapture::new(dir)
    }

    fn sample_action(goal_id: Option<Uuid>) -> CapturedAction {
        CapturedAction::new(
            "email",
            json!({"to": "alice@example.com", "subject": "Hi", "body": "Hello"}),
            goal_id,
            Some("Test Goal".into()),
            ActionPolicy::Review,
            ActionOutcome::CapturedForReview,
            false,
        )
    }

    #[test]
    fn append_and_query_all() {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());

        let a1 = sample_action(None);
        let a2 = sample_action(None);
        capture.append(&a1).unwrap();
        capture.append(&a2).unwrap();

        let entries = capture.query(None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].capture_id, a1.capture_id);
        assert_eq!(entries[1].capture_id, a2.capture_id);
    }

    #[test]
    fn query_filtered_by_goal() {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());

        let goal_a = Uuid::new_v4();
        let goal_b = Uuid::new_v4();

        capture.append(&sample_action(Some(goal_a))).unwrap();
        capture.append(&sample_action(Some(goal_b))).unwrap();
        capture.append(&sample_action(Some(goal_a))).unwrap();

        let for_a = capture.query(Some(goal_a)).unwrap();
        assert_eq!(for_a.len(), 2);

        let for_b = capture.query(Some(goal_b)).unwrap();
        assert_eq!(for_b.len(), 1);
    }

    #[test]
    fn query_returns_empty_when_no_log() {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());
        let entries = capture.query(None).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn outcome_display_strings() {
        assert_eq!(
            ActionOutcome::CapturedForReview.to_string(),
            "captured_for_review"
        );
        assert_eq!(ActionOutcome::DryRun.to_string(), "dry_run");
        assert_eq!(
            ActionOutcome::Blocked {
                reason: "policy=block".into()
            }
            .to_string(),
            "blocked"
        );
        assert_eq!(
            ActionOutcome::RateLimited {
                limit: 5,
                current: 5
            }
            .to_string(),
            "rate_limited"
        );
    }

    #[test]
    fn blocked_action_is_captured() {
        let dir = tempfile::tempdir().unwrap();
        let capture = make_capture(dir.path());

        let blocked = CapturedAction::new(
            "social_post",
            json!({"platform": "twitter", "content": "hi"}),
            None,
            None,
            ActionPolicy::Block,
            ActionOutcome::Blocked {
                reason: "policy=block".into(),
            },
            false,
        );
        capture.append(&blocked).unwrap();

        let entries = capture.query(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action_type, "social_post");
        assert!(matches!(entries[0].outcome, ActionOutcome::Blocked { .. }));
    }
}
