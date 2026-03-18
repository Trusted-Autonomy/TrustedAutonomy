//! Core SourceAdapter trait and result types
//!
//! The `SourceAdapter` trait (formerly `SubmitAdapter`) is the unified abstraction
//! for VCS operations. It combines submit operations (commit, push, open review)
//! with sync operations (fetch upstream, detect conflicts).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use ta_changeset::DraftPackage;
use ta_goal::GoalRun;
use thiserror::Error;

use crate::config::SubmitConfig;

/// Errors that can occur during source operations
#[derive(Debug, Error)]
pub enum SubmitError {
    #[error("Adapter not configured: {0}")]
    NotConfigured(String),

    #[error("VCS operation failed: {0}")]
    VcsError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Review creation failed: {0}")]
    ReviewError(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Sync failed: {0}")]
    SyncError(String),

    #[error("Sync conflict: {conflicts} file(s) in conflict")]
    SyncConflict { conflicts: usize },
}

pub type Result<T> = std::result::Result<T, SubmitError>;

/// Result of a commit operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitResult {
    /// Commit identifier (hash, changelist number, etc.)
    pub commit_id: String,

    /// Human-readable message
    pub message: String,

    /// Adapter-specific metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Result of a push operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    /// Remote reference (branch name, changelist URL, etc.)
    pub remote_ref: String,

    /// Human-readable message
    pub message: String,

    /// Adapter-specific metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Result of opening a review request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResult {
    /// Review URL (GitHub PR, Perforce review, etc.)
    pub review_url: String,

    /// Review identifier
    pub review_id: String,

    /// Human-readable message
    pub message: String,

    /// Adapter-specific metadata
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// Result of a sync operation — pulling upstream changes into the local workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    /// Whether upstream had new changes that were incorporated.
    pub updated: bool,

    /// Files with merge conflicts (empty if none).
    pub conflicts: Vec<String>,

    /// Number of new upstream commits incorporated.
    pub new_commits: u32,

    /// Human-readable summary of what happened.
    pub message: String,

    /// Adapter-specific metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl SyncResult {
    /// Whether the sync completed without conflicts.
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Opaque saved VCS state for save/restore around apply operations.
///
/// Each adapter stores its own state (e.g., Git saves the current branch name,
/// Perforce saves the current changelist). The state is passed back to
/// `restore_state()` after the apply operation completes.
pub struct SavedVcsState {
    /// Adapter name that created this state (for safety checks).
    pub adapter: String,
    /// Opaque state data — only the creating adapter knows how to interpret this.
    pub data: Box<dyn std::any::Any + Send>,
}

/// Pluggable adapter for source control operations (submit + sync).
///
/// The staging->review->apply loop is VCS-agnostic. This trait allows
/// different implementations for Git, Perforce, SVN, or custom workflows.
///
/// Renamed from `SubmitAdapter` in v0.11.1 to reflect the unified scope
/// (submit + sync). The old name is available as a type alias.
pub trait SourceAdapter: Send + Sync {
    /// Create a working branch/changelist/workspace for this goal
    ///
    /// For Git: creates a feature branch
    /// For Perforce: creates a changelist
    /// For "none": no-op
    fn prepare(&self, goal: &GoalRun, config: &SubmitConfig) -> Result<()>;

    /// Commit the approved changes from staging
    ///
    /// For Git: `git add` + `git commit`
    /// For Perforce: shelve files
    /// For "none": no-op
    fn commit(&self, goal: &GoalRun, pr: &DraftPackage, message: &str) -> Result<CommitResult>;

    /// Push the committed changes
    ///
    /// For Git: `git push`
    /// For Perforce: submit changelist
    /// For "none": no-op
    fn push(&self, goal: &GoalRun) -> Result<PushResult>;

    /// Open a review request
    ///
    /// For Git: create GitHub/GitLab PR via API or `gh pr create`
    /// For Perforce: create Swarm review
    /// For "none": no-op
    fn open_review(&self, goal: &GoalRun, pr: &DraftPackage) -> Result<ReviewResult>;

    /// Sync the local workspace with upstream changes.
    ///
    /// For Git: `git fetch` + merge/rebase/ff per `source.git.sync_strategy`
    /// For SVN: `svn update`
    /// For Perforce: `p4 sync`
    /// For "none": no-op (always returns updated=false)
    ///
    /// Returns a `SyncResult` describing what happened. If conflicts are
    /// detected, `SyncResult.conflicts` is non-empty but the method still
    /// returns `Ok` — the caller decides how to handle conflicts. Only
    /// returns `Err` for infrastructure failures (network, permissions).
    fn sync_upstream(&self) -> Result<SyncResult> {
        Ok(SyncResult {
            updated: false,
            conflicts: vec![],
            new_commits: 0,
            message: "No sync operation (default implementation)".to_string(),
            metadata: HashMap::new(),
        })
    }

    /// Adapter display name (for CLI output)
    fn name(&self) -> &str;

    /// Patterns to exclude from staging copy (VCS metadata dirs, etc.)
    ///
    /// Returns patterns in .taignore format: "dirname/", "*.ext", "name".
    /// These are merged with user .taignore patterns and built-in defaults
    /// during overlay workspace creation and diffing.
    fn exclude_patterns(&self) -> Vec<String> {
        vec![]
    }

    /// Save working state before apply operations.
    ///
    /// Git: saves the current branch name so it can be restored after commit.
    /// Perforce: saves the current changelist context.
    /// Default: no-op (returns None).
    fn save_state(&self) -> Result<Option<SavedVcsState>> {
        Ok(None)
    }

    /// Restore working state after apply operations.
    ///
    /// Git: switches back to the original branch.
    /// Perforce: reverts to saved client state.
    /// Default: no-op.
    fn restore_state(&self, _state: Option<SavedVcsState>) -> Result<()> {
        Ok(())
    }

    /// Get the current revision identifier for the working directory.
    ///
    /// Git: short commit hash (e.g., "abc1234")
    /// SVN: revision number (e.g., "r1234")
    /// Perforce: changelist number (e.g., "@1234")
    /// Default: "unknown"
    fn revision_id(&self) -> Result<String> {
        Ok("unknown".to_string())
    }

    /// Check the status of a review/PR by its review ID (e.g., PR number).
    ///
    /// Git: uses `gh pr view --json state` to check PR status.
    /// Returns the current state as a string: "open", "merged", "closed".
    /// Default: returns None (not supported).
    fn check_review(&self, _review_id: &str) -> Result<Option<ReviewStatus>> {
        Ok(None)
    }

    /// Auto-detect whether this adapter applies to the given project root.
    ///
    /// Git: checks for .git/ directory
    /// SVN: checks for .svn/ directory
    /// Perforce: checks for P4CONFIG env var or .p4config
    fn detect(project_root: &Path) -> bool
    where
        Self: Sized,
    {
        let _ = project_root;
        false
    }

    /// Protected submit targets for this adapter (§15 VCS Submit Invariant).
    ///
    /// Returns the list of refs/branches/paths that agents must never commit
    /// directly to. `prepare()` must create an isolation mechanism (feature
    /// branch, shelved CL, etc.) before `verify_not_on_protected_target()` is
    /// called.
    ///
    /// Default: empty list (no protected targets — applies to adapters that
    /// handle isolation entirely through their `prepare()` implementation).
    fn protected_submit_targets(&self) -> Vec<String> {
        vec![]
    }

    /// Assert the post-`prepare()` invariant: the adapter must not be
    /// positioned to commit directly to a protected target (§15).
    ///
    /// Called immediately after `prepare()` succeeds, before any commit or
    /// push. Hard failure aborts the apply workflow.
    ///
    /// Default implementation: if `protected_submit_targets()` returns a
    /// non-empty list, subclasses should override this to check the current
    /// position. The base implementation is a no-op (safe for adapters whose
    /// `prepare()` guarantees isolation without needing an extra check).
    fn verify_not_on_protected_target(&self) -> Result<()> {
        Ok(())
    }
}

/// Status of a VCS review/PR (v0.11.2.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewStatus {
    /// Current state: "open", "merged", "closed", "draft".
    pub state: String,
    /// Whether CI checks are passing.
    pub checks_passing: Option<bool>,
}

/// Backward-compatible alias: `SubmitAdapter` is the old name for `SourceAdapter`.
///
/// Deprecated in v0.11.1. Use `SourceAdapter` instead.
pub use SourceAdapter as SubmitAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    struct MockAdapter;
    impl SourceAdapter for MockAdapter {
        fn prepare(&self, _: &GoalRun, _: &SubmitConfig) -> Result<()> {
            Ok(())
        }
        fn commit(&self, _: &GoalRun, _: &DraftPackage, _: &str) -> Result<CommitResult> {
            unimplemented!()
        }
        fn push(&self, _: &GoalRun) -> Result<PushResult> {
            unimplemented!()
        }
        fn open_review(&self, _: &GoalRun, _: &DraftPackage) -> Result<ReviewResult> {
            unimplemented!()
        }
        fn name(&self) -> &str {
            "mock"
        }
    }

    #[test]
    fn default_protected_targets_empty() {
        let adapter = MockAdapter;
        assert!(adapter.protected_submit_targets().is_empty());
    }

    #[test]
    fn default_verify_not_on_protected_target_ok() {
        let adapter = MockAdapter;
        assert!(adapter.verify_not_on_protected_target().is_ok());
    }

    #[test]
    fn sync_result_is_clean_when_no_conflicts() {
        let result = SyncResult {
            updated: true,
            conflicts: vec![],
            new_commits: 3,
            message: "ok".to_string(),
            metadata: HashMap::new(),
        };
        assert!(result.is_clean());
    }

    #[test]
    fn sync_result_is_not_clean_with_conflicts() {
        let result = SyncResult {
            updated: true,
            conflicts: vec!["src/main.rs".to_string()],
            new_commits: 3,
            message: "conflict".to_string(),
            metadata: HashMap::new(),
        };
        assert!(!result.is_clean());
    }

    #[test]
    fn sync_result_serialization_roundtrip() {
        let result = SyncResult {
            updated: true,
            conflicts: vec!["a.rs".to_string()],
            new_commits: 5,
            message: "synced".to_string(),
            metadata: [("branch".to_string(), "main".to_string())]
                .into_iter()
                .collect(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let restored: SyncResult = serde_json::from_str(&json).unwrap();
        assert!(restored.updated);
        assert_eq!(restored.conflicts, vec!["a.rs"]);
        assert_eq!(restored.new_commits, 5);
    }
}
