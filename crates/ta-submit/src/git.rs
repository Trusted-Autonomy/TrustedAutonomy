//! Git adapter for branch-based workflows with GitHub/GitLab PR creation

use std::path::Path;
use std::process::Command;
use ta_changeset::DraftPackage;
use ta_goal::GoalRun;

use crate::adapter::{
    CommitResult, PushResult, Result, ReviewResult, ReviewStatus, SavedVcsState, SourceAdapter,
    SubmitError, SyncResult,
};
use crate::config::SubmitConfig;
use crate::config::SyncConfig;

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
    /// Submit configuration (co-author, branch prefix, etc.)
    config: SubmitConfig,
    /// Sync configuration (strategy, remote, branch)
    sync_config: SyncConfig,
}

impl GitAdapter {
    /// Create a new GitAdapter for the given working directory
    pub fn new(work_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
            config: SubmitConfig::default(),
            sync_config: SyncConfig::default(),
        }
    }

    /// Create a new GitAdapter with explicit configuration
    pub fn with_config(work_dir: impl Into<std::path::PathBuf>, config: SubmitConfig) -> Self {
        Self {
            work_dir: work_dir.into(),
            config,
            sync_config: SyncConfig::default(),
        }
    }

    /// Create a new GitAdapter with submit and sync configuration
    pub fn with_full_config(
        work_dir: impl Into<std::path::PathBuf>,
        config: SubmitConfig,
        sync_config: SyncConfig,
    ) -> Self {
        Self {
            work_dir: work_dir.into(),
            config,
            sync_config,
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

impl SourceAdapter for GitAdapter {
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
        let co_author_line = if self.config.co_author.is_empty() {
            String::new()
        } else {
            format!("\n\nCo-Authored-By: {}", self.config.co_author)
        };
        let commit_msg = format!(
            "{}\n\nGoal-ID: {}\nPR-ID: {}{}{}",
            message, goal.goal_run_id, pr.package_id, phase_line, co_author_line
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
        let branch_name = self.branch_name(goal, &self.config);
        let remote = &self.config.git.remote;

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

        // Enable auto-merge if configured (v0.11.2.3).
        if self.config.git.auto_merge && self.has_gh_cli() {
            let merge_strategy = &self.config.git.merge_strategy;
            let merge_flag = match merge_strategy.as_str() {
                "rebase" => "--rebase",
                "merge" => "--merge",
                _ => "--squash",
            };
            let auto_merge_output = Command::new("gh")
                .args(["pr", "merge", "--auto", merge_flag, &pr_number])
                .current_dir(&self.work_dir)
                .output();
            match auto_merge_output {
                Ok(o) if o.status.success() => {
                    tracing::info!("GitAdapter: auto-merge enabled for PR #{}", pr_number);
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    tracing::warn!(
                        "GitAdapter: auto-merge failed for PR #{}: {}",
                        pr_number,
                        stderr
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "GitAdapter: could not enable auto-merge for PR #{}: {}",
                        pr_number,
                        e
                    );
                }
            }
        }

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

    fn sync_upstream(&self) -> Result<SyncResult> {
        let remote = &self.sync_config.remote;
        let branch = &self.sync_config.branch;
        let strategy = &self.sync_config.strategy;

        tracing::info!(
            remote = %remote,
            branch = %branch,
            strategy = %strategy,
            "GitAdapter: syncing upstream"
        );

        // Fetch from remote.
        self.git_cmd(&["fetch", remote])?;

        // Count new commits on remote that we don't have locally.
        let remote_ref = format!("{}/{}", remote, branch);
        let count_output = self
            .git_cmd(&["rev-list", "--count", &format!("HEAD..{}", remote_ref)])
            .unwrap_or_else(|_| "0".to_string());
        let new_commits: u32 = count_output.trim().parse().unwrap_or(0);

        if new_commits == 0 {
            return Ok(SyncResult {
                updated: false,
                conflicts: vec![],
                new_commits: 0,
                message: format!("Already up to date with {}/{}.", remote, branch),
                metadata: [
                    ("remote".to_string(), remote.clone()),
                    ("branch".to_string(), branch.clone()),
                ]
                .into_iter()
                .collect(),
            });
        }

        // Apply the upstream changes using the configured strategy.
        let merge_result = match strategy.as_str() {
            "rebase" => self.git_cmd(&["rebase", &remote_ref]),
            "ff-only" => self.git_cmd(&["merge", "--ff-only", &remote_ref]),
            _ => self.git_cmd(&["merge", &remote_ref]),
        };

        match merge_result {
            Ok(output) => Ok(SyncResult {
                updated: true,
                conflicts: vec![],
                new_commits,
                message: format!(
                    "Synced {} new commit(s) from {}/{} (strategy: {}). {}",
                    new_commits, remote, branch, strategy, output
                ),
                metadata: [
                    ("remote".to_string(), remote.clone()),
                    ("branch".to_string(), branch.clone()),
                    ("strategy".to_string(), strategy.clone()),
                ]
                .into_iter()
                .collect(),
            }),
            Err(e) => {
                // Check for merge conflicts.
                let conflict_output = self
                    .git_cmd(&["diff", "--name-only", "--diff-filter=U"])
                    .unwrap_or_default();
                let conflicts: Vec<String> = conflict_output
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect();

                if conflicts.is_empty() {
                    // Not a conflict — infrastructure failure.
                    Err(SubmitError::SyncError(format!(
                        "Failed to sync {}/{} using strategy '{}': {}",
                        remote, branch, strategy, e
                    )))
                } else {
                    // Conflicts detected — return Ok with conflict info.
                    // The caller decides whether to abort the merge.
                    Ok(SyncResult {
                        updated: true,
                        conflicts: conflicts.clone(),
                        new_commits,
                        message: format!(
                            "Synced {} new commit(s) from {}/{} but {} file(s) have conflicts. \
                             Resolve conflicts manually, then `git add` and `git commit`.",
                            new_commits,
                            remote,
                            branch,
                            conflicts.len()
                        ),
                        metadata: [
                            ("remote".to_string(), remote.clone()),
                            ("branch".to_string(), branch.clone()),
                            ("strategy".to_string(), strategy.clone()),
                        ]
                        .into_iter()
                        .collect(),
                    })
                }
            }
        }
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

    fn check_review(&self, review_id: &str) -> Result<Option<ReviewStatus>> {
        if !self.has_gh_cli() {
            return Ok(None);
        }

        let output = Command::new("gh")
            .args(["pr", "view", review_id, "--json", "state,statusCheckRollup"])
            .current_dir(&self.work_dir)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let json: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
                    SubmitError::VcsError(format!("Failed to parse gh pr view output: {}", e))
                })?;

                let state = json
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_lowercase();

                let checks_passing = json.get("statusCheckRollup").and_then(|v| {
                    v.as_array().map(|checks| {
                        checks.iter().all(|c| {
                            c.get("conclusion").and_then(|v| v.as_str()) == Some("SUCCESS")
                        })
                    })
                });

                Ok(Some(ReviewStatus {
                    state,
                    checks_passing,
                }))
            }
            _ => Ok(None),
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
    fn test_git_adapter_sync_upstream_already_up_to_date() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        let adapter = GitAdapter::new(dir.path());
        // No remote configured, so sync should fail gracefully or show up-to-date.
        // Since there's no remote "origin", fetch will fail.
        let result = adapter.sync_upstream();
        // Without a remote, this will return an error (VCS operation failed).
        assert!(result.is_err());
    }

    #[test]
    fn test_git_adapter_sync_upstream_with_local_remote() {
        // Create a "remote" repo and a "local" clone to test sync.
        let remote_dir = tempdir().unwrap();
        init_git_repo(remote_dir.path()).unwrap();

        // Clone it to create a local repo with origin pointing to remote.
        let local_dir = tempdir().unwrap();
        Command::new("git")
            .args(["clone", &remote_dir.path().to_string_lossy(), "."])
            .current_dir(local_dir.path())
            .output()
            .unwrap();

        // Detect the actual default branch name (may be "main" or "master").
        let branch_output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(local_dir.path())
            .output()
            .unwrap();
        let branch_name = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();

        // Configure the sync adapter with the correct branch.
        let sync_config = crate::config::SyncConfig {
            branch: branch_name,
            ..Default::default()
        };
        let adapter =
            GitAdapter::with_full_config(local_dir.path(), SubmitConfig::default(), sync_config);

        // At this point local is up to date with remote.
        let result = adapter.sync_upstream().unwrap();
        assert!(!result.updated);
        assert_eq!(result.new_commits, 0);
        assert!(result.is_clean());

        // Now add a commit to the remote.
        std::fs::write(remote_dir.path().join("new_file.txt"), "hello\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(remote_dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "Remote commit"])
            .current_dir(remote_dir.path())
            .output()
            .unwrap();

        // Sync should pick up the new commit.
        let result = adapter.sync_upstream().unwrap();
        assert!(result.updated);
        assert_eq!(result.new_commits, 1);
        assert!(result.is_clean());

        // Verify the file is now present locally.
        assert!(local_dir.path().join("new_file.txt").exists());
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
