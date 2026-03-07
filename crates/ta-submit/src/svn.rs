//! SVN adapter stub — untested, contributed by AI.
//!
//! This adapter provides basic SVN integration for projects using Subversion.
//! It is **untested** and needs validation by an SVN user before production use.
//!
//! Key differences from Git:
//! - SVN commit is immediately remote (no local-only commits)
//! - No built-in branch-based code review workflow
//! - `push()` is a no-op since `commit()` already sends to the server

use std::path::Path;
use std::process::Command;
use ta_changeset::DraftPackage;
use ta_goal::GoalRun;

use crate::adapter::{CommitResult, PushResult, Result, ReviewResult, SubmitAdapter, SubmitError};
use crate::config::SubmitConfig;

/// SVN adapter implementing Subversion workflow.
///
/// **Status: UNTESTED** — needs validation by an SVN user.
pub struct SvnAdapter {
    work_dir: std::path::PathBuf,
}

impl SvnAdapter {
    pub fn new(work_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
        }
    }

    fn svn_cmd(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("svn")
            .args(args)
            .current_dir(&self.work_dir)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SubmitError::VcsError(format!(
                "svn {} failed: {}",
                args.join(" "),
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Auto-detect whether this is an SVN working copy.
    pub fn detect(project_root: &Path) -> bool {
        project_root.join(".svn").exists()
    }
}

impl SubmitAdapter for SvnAdapter {
    fn prepare(&self, _goal: &GoalRun, _config: &SubmitConfig) -> Result<()> {
        // SVN doesn't use branches the same way as Git.
        // No-op: the working copy is already pointing at the correct location.
        tracing::debug!("SvnAdapter: prepare() — no-op (SVN working copy)");
        Ok(())
    }

    fn commit(&self, goal: &GoalRun, _pr: &DraftPackage, message: &str) -> Result<CommitResult> {
        tracing::info!("SvnAdapter: committing changes");

        // Add any new (unversioned) files.
        // `svn add` with --force adds unversioned files without erroring on already-tracked ones.
        let _ = self.svn_cmd(&["add", "--force", "."]);

        // Build commit message with goal metadata.
        let commit_msg = format!("{}\n\nGoal-ID: {}", message, goal.goal_run_id);

        // Commit — this sends changes to the remote server immediately.
        let output = self.svn_cmd(&["commit", "-m", &commit_msg])?;

        // Try to extract revision number from commit output.
        // SVN output: "Committed revision 1234."
        let rev = output
            .lines()
            .find(|l| l.contains("Committed revision"))
            .and_then(|l| {
                l.split_whitespace()
                    .find(|w| w.chars().any(|c| c.is_ascii_digit()))
                    .map(|w| w.trim_end_matches('.').to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        Ok(CommitResult {
            commit_id: format!("r{}", rev),
            message: format!("Committed revision {}", rev),
            metadata: [("revision".to_string(), rev)].into_iter().collect(),
        })
    }

    fn push(&self, _goal: &GoalRun) -> Result<PushResult> {
        // SVN commit is already remote — no separate push step.
        tracing::debug!("SvnAdapter: push() — no-op (SVN commit is already remote)");
        Ok(PushResult {
            remote_ref: "svn://committed".to_string(),
            message: "SVN commit is already remote — no push needed".to_string(),
            metadata: Default::default(),
        })
    }

    fn open_review(&self, _goal: &GoalRun, _pr: &DraftPackage) -> Result<ReviewResult> {
        // SVN doesn't have built-in code review.
        tracing::debug!("SvnAdapter: open_review() — no-op (SVN has no built-in review)");
        Ok(ReviewResult {
            review_url: "svn://no-review".to_string(),
            review_id: "none".to_string(),
            message: "SVN has no built-in review workflow. Consider using a code review tool like Crucible or ReviewBoard.".to_string(),
            metadata: Default::default(),
        })
    }

    fn name(&self) -> &str {
        "svn"
    }

    fn exclude_patterns(&self) -> Vec<String> {
        vec![".svn/".to_string()]
    }

    fn revision_id(&self) -> Result<String> {
        // `svn info` outputs "Revision: 1234" among other fields.
        let info = self.svn_cmd(&["info"])?;
        let rev = info
            .lines()
            .find(|l| l.starts_with("Revision:"))
            .and_then(|l| l.split(':').nth(1))
            .map(|r| r.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        Ok(format!("r{}", rev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_svn_adapter_name() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = SvnAdapter::new(dir.path());
        assert_eq!(adapter.name(), "svn");
    }

    #[test]
    fn test_svn_adapter_exclude_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = SvnAdapter::new(dir.path());
        assert_eq!(adapter.exclude_patterns(), vec![".svn/"]);
    }

    #[test]
    fn test_svn_adapter_detect() {
        let dir = tempfile::tempdir().unwrap();

        // No .svn directory — should not detect
        assert!(!SvnAdapter::detect(dir.path()));

        // Create .svn directory — should detect
        std::fs::create_dir(dir.path().join(".svn")).unwrap();
        assert!(SvnAdapter::detect(dir.path()));
    }

    #[test]
    fn test_svn_adapter_push_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = SvnAdapter::new(dir.path());
        let goal = GoalRun::new(
            "Test",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let result = adapter.push(&goal).unwrap();
        assert_eq!(result.remote_ref, "svn://committed");
    }
}
