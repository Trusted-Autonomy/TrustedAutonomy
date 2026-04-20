//! "None" adapter - backwards-compatible fallback with no VCS operations

use ta_changeset::DraftPackage;
use ta_goal::GoalRun;

use crate::adapter::{CommitResult, PushResult, Result, ReviewResult, SourceAdapter};
use crate::config::SubmitConfig;

/// Fallback adapter that performs no VCS operations
///
/// This adapter maintains backwards compatibility with workflows that don't
/// use version control integration. It's selected automatically when no
/// workflow config exists or when adapter = "none".
pub struct NoneAdapter;

impl NoneAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NoneAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceAdapter for NoneAdapter {
    fn prepare(&self, _goal: &GoalRun, _config: &SubmitConfig) -> Result<()> {
        // No-op: no workspace preparation needed
        tracing::debug!("NoneAdapter: prepare() - no-op");
        Ok(())
    }

    fn commit(&self, goal: &GoalRun, _pr: &DraftPackage, message: &str) -> Result<CommitResult> {
        // No-op: no commit operation
        tracing::debug!("NoneAdapter: commit() - no-op");
        Ok(CommitResult {
            commit_id: format!("none-{}", goal.goal_run_id),
            message: message.to_string(),
            metadata: Default::default(),
            ignored_artifacts: vec![],
        })
    }

    fn push(&self, goal: &GoalRun) -> Result<PushResult> {
        // No-op: no push operation
        tracing::debug!("NoneAdapter: push() - no-op");
        Ok(PushResult {
            remote_ref: format!("none-{}", goal.goal_run_id),
            message: "No push operation (none adapter)".to_string(),
            metadata: Default::default(),
        })
    }

    fn open_review(&self, goal: &GoalRun, _pr: &DraftPackage) -> Result<ReviewResult> {
        // No-op: no review creation
        tracing::debug!("NoneAdapter: open_review() - no-op");
        Ok(ReviewResult {
            review_url: format!("none://{}", goal.goal_run_id),
            review_id: format!("none-{}", goal.goal_run_id),
            message: "No review creation (none adapter)".to_string(),
            metadata: Default::default(),
        })
    }

    fn name(&self) -> &str {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_goal::GoalRun;

    fn mock_goal() -> GoalRun {
        GoalRun::new(
            "Test Goal",
            "Test objective",
            "test-agent",
            std::path::PathBuf::from("/tmp/workspace"),
            std::path::PathBuf::from("/tmp/store"),
        )
    }

    #[test]
    fn test_none_adapter_name() {
        let adapter = NoneAdapter::new();
        assert_eq!(adapter.name(), "none");
    }

    #[test]
    fn test_none_adapter_prepare() {
        let adapter = NoneAdapter::new();
        let goal = mock_goal();
        let config = SubmitConfig::default();

        assert!(adapter.prepare(&goal, &config).is_ok());
    }

    #[test]
    fn test_none_adapter_push() {
        let adapter = NoneAdapter::new();
        let goal = mock_goal();

        let result = adapter.push(&goal).unwrap();
        assert!(result.remote_ref.starts_with("none-"));
    }

    #[test]
    fn commit_diff_always_returns_none() {
        let adapter = NoneAdapter::new();
        assert_eq!(adapter.commit_diff(), None);
    }

    // Note: Tests for commit() and open_review() require PRPackage construction
    // which has a complex structure. These will be added as integration tests
    // that use actual PRPackage instances from ta-changeset test utilities.
}
