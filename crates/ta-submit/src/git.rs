//! Git adapter for branch-based workflows with GitHub/GitLab PR creation

use std::path::Path;
use std::process::Command;
use ta_changeset::DraftPackage;
use ta_goal::GoalRun;

use crate::adapter::{
    CommitResult, PushResult, Result, ReviewResult, SavedVcsState, SubmitAdapter, SubmitError,
};
use crate::config::SubmitConfig;

/// Git adapter implementing branch-based workflow
///
/// Features:
/// - Automatic feature branch creation
/// - Git commit with PR metadata
/// - Push to remote
/// - GitHub/GitLab PR creation via gh/glab CLI
pub struct GitAdapter {
    /// Working directory for git operations
    work_dir: std::path::PathBuf,
}

impl GitAdapter {
    /// Create a new GitAdapter for the given working directory
    pub fn new(work_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
        }
    }

    /// Run a git command in the working directory
    fn git_cmd(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.work_dir)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SubmitError::VcsError(format!(
                "git {} failed: {}",
                args.join(" "),
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Check if gh CLI is available
    fn has_gh_cli(&self) -> bool {
        Command::new("gh")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get current branch name
    pub fn current_branch(&self) -> Result<String> {
        self.git_cmd(&["rev-parse", "--abbrev-ref", "HEAD"])
    }

    /// Generate branch name from goal
    fn branch_name(&self, goal: &GoalRun, config: &SubmitConfig) -> String {
        let prefix = &config.git.branch_prefix;
        let sanitized = goal
            .title
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();

        // Truncate to reasonable length
        let sanitized = if sanitized.len() > 50 {
            &sanitized[..50]
        } else {
            &sanitized
        };

        format!("{}{}", prefix, sanitized)
    }

    /// Auto-detect whether this is a git repository.
    pub fn detect(project_root: &Path) -> bool {
        project_root.join(".git").exists()
    }
}

impl SubmitAdapter for GitAdapter {
    fn prepare(&self, goal: &GoalRun, config: &SubmitConfig) -> Result<()> {
        let branch_name = self.branch_name(goal, config);

        tracing::info!("GitAdapter: creating branch {}", branch_name);

        // Check if branch already exists
        let branches = self.git_cmd(&["branch", "--list", &branch_name])?;
        if branches.is_empty() {
            // Create new branch
            self.git_cmd(&["checkout", "-b", &branch_name])?;
        } else {
            // Switch to existing branch
            self.git_cmd(&["checkout", &branch_name])?;
        }

        Ok(())
    }

    fn commit(&self, goal: &GoalRun, pr: &DraftPackage, message: &str) -> Result<CommitResult> {
        tracing::info!("GitAdapter: committing changes");

        // Add all changes (staging workspace is already filtered by .taignore)
        self.git_cmd(&["add", "."])?;

        // Check if there are changes to commit
        let status = self.git_cmd(&["status", "--porcelain"])?;
        if status.trim().is_empty() {
            return Err(SubmitError::InvalidState(
                "No changes to commit".to_string(),
            ));
        }

        // Append metadata trailers to the caller-provided message.
        let phase_line = goal
            .plan_phase
            .as_ref()
            .map(|p| format!("\nPhase: {}", p))
            .unwrap_or_default();
        let commit_msg = format!(
            "{}\n\nGoal-ID: {}\nPR-ID: {}{}\n\nCo-Authored-By: Trusted Autonomy <ta@trustedautonomy.dev>",
            message, goal.goal_run_id, pr.package_id, phase_line
        );

        // Commit
        self.git_cmd(&["commit", "-m", &commit_msg])?;

        // Get commit hash
        let commit_id = self.git_cmd(&["rev-parse", "HEAD"])?;

        Ok(CommitResult {
            commit_id: commit_id.clone(),
            message: format!("Committed as {}", &commit_id[..8]),
            metadata: [("full_hash".to_string(), commit_id)].into_iter().collect(),
        })
    }

    fn push(&self, goal: &GoalRun) -> Result<PushResult> {
        let config = SubmitConfig::default(); // TODO: pass config through
        let branch_name = self.branch_name(goal, &config);
        let remote = &config.git.remote;

        tracing::info!("GitAdapter: pushing branch {} to {}", branch_name, remote);

        // Push with --set-upstream
        self.git_cmd(&["push", "-u", remote, &branch_name])?;

        Ok(PushResult {
            remote_ref: format!("{}/{}", remote, branch_name),
            message: format!("Pushed to {}/{}", remote, branch_name),
            metadata: [
                ("branch".to_string(), branch_name),
                ("remote".to_string(), remote.clone()),
            ]
            .into_iter()
            .collect(),
        })
    }

    fn open_review(&self, goal: &GoalRun, pr: &DraftPackage) -> Result<ReviewResult> {
        if !self.has_gh_cli() {
            return Err(SubmitError::ReviewError(
                "gh CLI not found - install GitHub CLI to create PRs".to_string(),
            ));
        }

        let config = SubmitConfig::default(); // TODO: pass config through
        let target_branch = &config.git.target_branch;

        // Build PR body
        let body = self.build_pr_body(goal, pr, &config)?;

        tracing::info!("GitAdapter: creating PR to {}", target_branch);

        // Create PR using gh CLI
        let output = Command::new("gh")
            .args([
                "pr",
                "create",
                "--base",
                target_branch,
                "--title",
                &goal.title,
                "--body",
                &body,
            ])
            .current_dir(&self.work_dir)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SubmitError::ReviewError(format!(
                "gh pr create failed: {}",
                stderr
            )));
        }

        let pr_url = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Extract PR number from URL (e.g., https://github.com/owner/repo/pull/123)
        let pr_number = pr_url
            .split('/')
            .next_back()
            .unwrap_or("unknown")
            .to_string();

        Ok(ReviewResult {
            review_url: pr_url.clone(),
            review_id: pr_number,
            message: format!("Created PR: {}", pr_url),
            metadata: [("pr_url".to_string(), pr_url)].into_iter().collect(),
        })
    }

    fn name(&self) -> &str {
        "git"
    }

    fn exclude_patterns(&self) -> Vec<String> {
        vec![".git/".to_string()]
    }

    fn save_state(&self) -> Result<Option<SavedVcsState>> {
        let branch = self.current_branch()?;
        tracing::debug!(branch = %branch, "GitAdapter: saved branch state");
        Ok(Some(SavedVcsState {
            adapter: "git".to_string(),
            data: Box::new(branch),
        }))
    }

    fn restore_state(&self, state: Option<SavedVcsState>) -> Result<()> {
        let state = match state {
            Some(s) => s,
            None => return Ok(()),
        };

        if state.adapter != "git" {
            return Err(SubmitError::InvalidState(format!(
                "Cannot restore state from adapter '{}' in GitAdapter",
                state.adapter
            )));
        }

        let original_branch = state
            .data
            .downcast::<String>()
            .map_err(|_| SubmitError::InvalidState("Invalid saved state type".to_string()))?;

        let current = self.current_branch()?;
        if current != *original_branch {
            match self.git_cmd(&["checkout", &original_branch]) {
                Ok(_) => {
                    tracing::info!(
                        branch = %original_branch,
                        "GitAdapter: restored to original branch"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        branch = %original_branch,
                        current = %current,
                        error = %e,
                        "GitAdapter: could not restore branch. Run: git checkout {}",
                        original_branch
                    );
                }
            }
        }
        Ok(())
    }

    fn revision_id(&self) -> Result<String> {
        let hash = self.git_cmd(&["rev-parse", "--short", "HEAD"])?;

        // Check for uncommitted changes
        let status = self.git_cmd(&["status", "--porcelain"])?;
        if status.is_empty() {
            Ok(hash)
        } else {
            Ok(format!("{}-dirty", hash))
        }
    }
}

impl GitAdapter {
    /// Build PR body from template or default format.
    ///
    /// Template resolution order:
    /// 1. Explicit `config.git.pr_template` path
    /// 2. `.ta/pr-template.md` in the working directory
    /// 3. Built-in default format with per-artifact detail
    fn build_pr_body(
        &self,
        goal: &GoalRun,
        pr: &DraftPackage,
        config: &SubmitConfig,
    ) -> Result<String> {
        // Try explicit config path first.
        if let Some(template_path) = &config.git.pr_template {
            if template_path.exists() {
                let template = std::fs::read_to_string(template_path)?;
                return Ok(self.substitute_template(&template, goal, pr));
            }
        }

        // Try .ta/pr-template.md in the working directory.
        let convention_path = self.work_dir.join(".ta/pr-template.md");
        if convention_path.exists() {
            if let Ok(template) = std::fs::read_to_string(&convention_path) {
                return Ok(self.substitute_template(&template, goal, pr));
            }
        }

        // Default PR body with per-artifact detail (matches ta draft view medium).
        let artifact_detail = Self::format_artifacts_detail(pr);
        Ok(format!(
            "## Summary\n\n\
             {}\n\n\
             **Why**: {}\n\n\
             **Impact**: {}\n\n\
             ## Changes ({} artifacts)\n\n\
             {}\n\n\
             ## Goal Context\n\n\
             - **Goal ID**: `{}`\n\
             - **PR ID**: `{}`\n\
             {}\n\n\
             ---\n\n\
             Generated by [Trusted Autonomy](https://github.com/trustedautonomy/ta)",
            pr.summary.what_changed,
            pr.summary.why,
            pr.summary.impact,
            pr.changes.artifacts.len(),
            artifact_detail,
            goal.goal_run_id,
            pr.package_id,
            goal.plan_phase
                .as_ref()
                .map(|p| format!("- **Plan Phase**: `{}`", p))
                .unwrap_or_default()
        ))
    }

    /// Format artifacts with summaries and explanations for PR body (markdown).
    fn format_artifacts_detail(pr: &DraftPackage) -> String {
        pr.changes
            .artifacts
            .iter()
            .map(|a| {
                let change_icon = match a.change_type {
                    ta_changeset::draft_package::ChangeType::Add => "+",
                    ta_changeset::draft_package::ChangeType::Modify => "~",
                    ta_changeset::draft_package::ChangeType::Delete => "-",
                    ta_changeset::draft_package::ChangeType::Rename => ">",
                };
                let summary = a
                    .explanation_tiers
                    .as_ref()
                    .map(|t| t.summary.as_str())
                    .or(a.rationale.as_deref())
                    .unwrap_or("");

                let mut line = if summary.is_empty() {
                    format!("- `{change_icon}` `{}`", a.resource_uri)
                } else {
                    format!("- `{change_icon}` `{}` — {}", a.resource_uri, summary)
                };

                // Add explanation as sub-bullet if present and different from summary.
                if let Some(tiers) = &a.explanation_tiers {
                    if !tiers.explanation.is_empty() && tiers.explanation != tiers.summary {
                        line.push_str(&format!("\n  - {}", tiers.explanation));
                    }
                }

                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Substitute template variables.
    ///
    /// Available variables:
    ///   {title}          -- goal title
    ///   {summary}        -- what changed (from change_summary.json)
    ///   {why}            -- why it changed
    ///   {impact}         -- impact assessment
    ///   {objective}      -- full goal objective text
    ///   {artifact_count} -- number of files changed
    ///   {artifacts}      -- per-artifact detail with summaries and explanations
    ///   {goal_id}        -- goal UUID
    ///   {pr_id}          -- PR package UUID
    ///   {plan_phase}     -- plan phase (or "N/A")
    fn substitute_template(&self, template: &str, goal: &GoalRun, pr: &DraftPackage) -> String {
        let artifact_lines = Self::format_artifacts_detail(pr);

        template
            .replace("{summary}", &pr.summary.what_changed)
            .replace("{why}", &pr.summary.why)
            .replace("{impact}", &pr.summary.impact)
            .replace("{goal_id}", &goal.goal_run_id.to_string())
            .replace("{pr_id}", &pr.package_id.to_string())
            .replace("{title}", &goal.title)
            .replace("{objective}", &goal.objective)
            .replace("{plan_phase}", goal.plan_phase.as_deref().unwrap_or("N/A"))
            .replace("{artifact_count}", &pr.changes.artifacts.len().to_string())
            .replace("{artifacts}", &artifact_lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn init_git_repo(dir: &Path) -> Result<()> {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()?;
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()?;
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()?;

        // Create initial commit
        std::fs::write(dir.join("README.md"), "# Test\n")?;
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()?;
        Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(dir)
            .output()?;

        Ok(())
    }

    #[test]
    fn test_git_adapter_branch_name() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        let adapter = GitAdapter::new(dir.path());
        let goal = GoalRun::new(
            "Add New Feature",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );

        let config = SubmitConfig::default();
        let branch = adapter.branch_name(&goal, &config);

        assert!(branch.starts_with("ta/"));
        assert!(branch.contains("add-new-feature"));
    }

    #[test]
    fn test_git_adapter_prepare() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        let adapter = GitAdapter::new(dir.path());
        let goal = GoalRun::new(
            "Test Goal",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );

        let config = SubmitConfig::default();
        assert!(adapter.prepare(&goal, &config).is_ok());

        // Verify we're on the new branch
        let current = adapter.current_branch().unwrap();
        assert!(current.starts_with("ta/"));
    }

    #[test]
    fn test_git_adapter_exclude_patterns() {
        let dir = tempdir().unwrap();
        let adapter = GitAdapter::new(dir.path());
        let patterns = adapter.exclude_patterns();
        assert_eq!(patterns, vec![".git/"]);
    }

    #[test]
    fn test_git_adapter_detect() {
        let dir = tempdir().unwrap();

        // No .git directory — should not detect
        assert!(!GitAdapter::detect(dir.path()));

        // Create .git directory — should detect
        init_git_repo(dir.path()).unwrap();
        assert!(GitAdapter::detect(dir.path()));
    }

    #[test]
    fn test_git_adapter_save_restore_state() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        let adapter = GitAdapter::new(dir.path());

        // Save state on main/master
        let original_branch = adapter.current_branch().unwrap();
        let state = adapter.save_state().unwrap();
        assert!(state.is_some());

        // Create and switch to a new branch
        let goal = GoalRun::new(
            "Test Goal",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let config = SubmitConfig::default();
        adapter.prepare(&goal, &config).unwrap();

        // Verify we're on a different branch
        let current = adapter.current_branch().unwrap();
        assert_ne!(current, original_branch);

        // Restore state
        adapter.restore_state(state).unwrap();
        let restored = adapter.current_branch().unwrap();
        assert_eq!(restored, original_branch);
    }

    #[test]
    fn test_git_adapter_revision_id() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        let adapter = GitAdapter::new(dir.path());
        let rev = adapter.revision_id().unwrap();

        // Should be a short hash (7+ chars)
        assert!(!rev.is_empty());
        assert_ne!(rev, "unknown");
    }
}
