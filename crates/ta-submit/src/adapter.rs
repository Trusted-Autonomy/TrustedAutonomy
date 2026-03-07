//! Core SubmitAdapter trait and result types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use ta_changeset::DraftPackage;
use ta_goal::GoalRun;
use thiserror::Error;

use crate::config::SubmitConfig;

/// Errors that can occur during submit operations
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

/// Pluggable adapter for submitting changes through different VCS workflows
///
/// The staging->review->apply loop is VCS-agnostic. This trait allows
/// different implementations for Git, Perforce, SVN, or custom workflows.
pub trait SubmitAdapter: Send + Sync {
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
}
