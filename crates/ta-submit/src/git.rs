//! Git adapter for branch-based workflows with GitHub/GitLab PR creation

use std::path::Path;
use std::process::Command;
use ta_changeset::DraftPackage;
use ta_goal::GoalRun;

use crate::adapter::{
    CommitResult, MergeResult, PushResult, Result, ReviewResult, ReviewStatus, SavedVcsState,
    SourceAdapter, SubmitError, SyncResult,
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
        // Clear TA agent VCS isolation env vars so git operates on the
        // work_dir's own repo, not the staging directory's repo (v0.13.17.3
        // sets GIT_DIR/GIT_WORK_TREE/GIT_CEILING_DIRECTORIES for agents).
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.work_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
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

    /// Generate a safe git branch name from the goal title.
    ///
    /// Sanitization steps (item 28):
    /// 1. Lowercase and map all non-alphanumeric chars to `-`
    /// 2. Collapse consecutive dashes into a single `-`
    /// 3. Trim leading and trailing dashes (fixes titles like `` `ta sync` ``)
    /// 4. Truncate to 50 chars and trim any trailing dashes from truncation
    ///
    /// All characters are passed directly to git as command arguments, not
    /// through shell interpolation, so no shell-escaping is needed.
    fn branch_name(&self, goal: &GoalRun, config: &SubmitConfig) -> String {
        let prefix = &config.git.branch_prefix;

        // Step 1: lowercase + replace non-alphanumeric/dash with dash.
        let raw: String = goal
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
            .collect();

        // Step 2: collapse consecutive dashes.
        let mut collapsed = String::with_capacity(raw.len());
        let mut prev_dash = false;
        for c in raw.chars() {
            if c == '-' {
                if !prev_dash {
                    collapsed.push(c);
                }
                prev_dash = true;
            } else {
                collapsed.push(c);
                prev_dash = false;
            }
        }

        // Step 3: trim leading/trailing dashes.
        let trimmed = collapsed.trim_matches('-');

        // Fallback if trimming produced an empty string (e.g. title was "!!!").
        let slug = if trimmed.is_empty() { "goal" } else { trimmed };

        // Step 4: truncate to 50 chars, then trim any trailing dash from truncation.
        let truncated = if slug.len() > 50 {
            slug[..50].trim_end_matches('-')
        } else {
            slug
        };

        format!("{}{}", prefix, truncated)
    }

    /// Auto-detect whether this is a git repository.
    pub fn detect(project_root: &Path) -> bool {
        project_root.join(".git").exists()
    }

    /// Built-in lock files that are always auto-staged when modified (v0.14.3.7).
    ///
    /// These are deterministic outputs of the build process — when a version
    /// bump or dependency change occurs, the lock file regenerates and must be
    /// committed together with the source change to keep the tree self-consistent.
    pub const BUILTIN_LOCK_FILES: &'static [&'static str] = &[
        "Cargo.lock",
        "package-lock.json",
        "go.sum",
        "Pipfile.lock",
        "poetry.lock",
        "yarn.lock",
        "bun.lockb",
        "flake.lock",
    ];

    /// Auto-stage critical files that should always accompany a draft apply commit.
    ///
    /// Stages each file in `candidates` that (a) exists in the working tree and
    /// (b) is dirty according to `git status --porcelain`. Logs each auto-staged
    /// path to stdout with the `ℹ️  auto-staged` prefix.
    fn auto_stage_critical_files(&self, candidates: &[&str]) {
        for path in candidates {
            let full = self.work_dir.join(path);
            if !full.exists() {
                continue;
            }
            // Check if modified (both unstaged and staged changes).
            let dirty = Command::new("git")
                .args(["status", "--porcelain", path])
                .current_dir(&self.work_dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_CEILING_DIRECTORIES")
                .output()
                .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
                .unwrap_or(false);
            if dirty {
                if let Ok(()) = self.git_cmd(&["add", path]).map(|_| ()) {
                    println!("  ℹ️  auto-staged: {}", path);
                    tracing::info!(path = %path, "auto-staged critical file");
                }
            }
        }
    }

    /// Build the full list of auto-stage candidates from built-in + user config.
    fn auto_stage_candidates(work_dir: &std::path::Path) -> Vec<String> {
        let mut candidates: Vec<String> = Self::BUILTIN_LOCK_FILES
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Always include TA state files.
        candidates.push(".ta/plan_history.jsonl".to_string());
        // Merge user-configured entries from [commit] auto_stage.
        let workflow_path = work_dir.join(".ta/workflow.toml");
        let workflow = crate::config::WorkflowConfig::load_or_default(&workflow_path);
        for entry in workflow.commit.auto_stage {
            if !candidates.contains(&entry) {
                candidates.push(entry);
            }
        }
        candidates
    }

    /// Known-safe artifact patterns that are silently dropped from `git add`
    /// when gitignored (v0.13.17.5). These are TA infrastructure files that
    /// should never reach a commit.
    fn is_known_safe_ignored(path: &str) -> bool {
        // Exact filename matches
        if path == ".mcp.json" || path == "daemon.toml" {
            return true;
        }
        // *.local.toml files anywhere
        if path.ends_with(".local.toml") {
            return true;
        }
        // .ta/ runtime state files
        if let Some(rest) = path.strip_prefix(".ta/") {
            if rest.ends_with(".pid") || rest.ends_with(".lock") || rest == "daemon.toml" {
                return true;
            }
        }
        false
    }

    /// Filter artifact paths using `git check-ignore --stdin` (v0.13.17.5).
    ///
    /// Returns `(to_add, ignored)` where:
    /// - `to_add`: paths not gitignored — pass these to `git add`
    /// - `ignored`: paths that are gitignored, with `known_safe` classified
    fn filter_gitignored_artifacts(
        &self,
        paths: &[String],
    ) -> (Vec<String>, Vec<ta_changeset::IgnoredArtifact>) {
        if paths.is_empty() {
            return (vec![], vec![]);
        }

        // Run `git check-ignore --stdin` — prints only the ignored paths.
        // Clear TA agent VCS isolation env vars so the check uses the work_dir
        // repo, not the staging workspace repo (v0.13.17.3).
        let input = paths.join("\n");
        let output = Command::new("git")
            .args(["check-ignore", "--stdin"])
            .current_dir(&self.work_dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(stdin) = child.stdin.take() {
                    let mut stdin = stdin;
                    let _ = stdin.write_all(input.as_bytes());
                }
                child.wait_with_output()
            });

        let ignored_set: std::collections::HashSet<String> = match output {
            Ok(out) => std::str::from_utf8(&out.stdout)
                .unwrap_or("")
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            Err(_) => {
                // If git check-ignore fails (e.g., not a git repo), assume nothing is ignored.
                tracing::debug!("git check-ignore failed — assuming no artifacts are gitignored");
                std::collections::HashSet::new()
            }
        };

        let mut to_add = Vec::new();
        let mut ignored = Vec::new();

        for path in paths {
            if ignored_set.contains(path.as_str()) {
                let known_safe = Self::is_known_safe_ignored(path);
                if known_safe {
                    tracing::debug!(path = %path, "dropping known-safe gitignored artifact from git add");
                } else {
                    eprintln!(
                        "Warning: artifact '{}' is gitignored — dropping from git add. \
                         Was this intentional?",
                        path
                    );
                }
                ignored.push(ta_changeset::IgnoredArtifact {
                    path: path.clone(),
                    known_safe,
                });
            } else {
                to_add.push(path.clone());
            }
        }

        (to_add, ignored)
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

        // Build list of explicit artifact paths from draft package.
        // Using explicit paths avoids accidentally staging unrelated files.
        // Non-fs URIs (mailto://, drive://, etc.) are excluded — only real
        // filesystem paths are staged.
        // Deduplicate: a follow-up draft or combined parent+child diff can
        // produce the same path more than once in the artifact list.
        let mut seen = std::collections::HashSet::new();
        let artifact_paths: Vec<String> = pr
            .changes
            .artifacts
            .iter()
            .filter_map(|a| {
                a.resource_uri
                    .strip_prefix("fs://workspace/")
                    .map(|p| p.to_string())
            })
            .filter(|p| seen.insert(p.clone()))
            .collect();

        // Filter out gitignored paths before calling git add (v0.13.17.5).
        // Known-safe paths (.mcp.json, *.local.toml, .ta/ runtime files) are
        // silently dropped. Unexpected-ignored paths emit a warning.
        let ignored_artifacts = if artifact_paths.is_empty() {
            vec![]
        } else {
            let (to_add, ignored) = self.filter_gitignored_artifacts(&artifact_paths);
            if to_add.is_empty() {
                // All artifacts were gitignored — complete with warning, not an error.
                if !ignored.is_empty() {
                    let unknown_count = ignored.iter().filter(|a| !a.known_safe).count();
                    if unknown_count > 0 {
                        eprintln!(
                            "Warning: all {} artifact(s) were gitignored — nothing was committed.",
                            ignored.len()
                        );
                    }
                }
                // Still attempt to stage PLAN.md and critical files even when all
                // draft artifacts were gitignored, then check if there's anything to commit.
                if self.work_dir.join("PLAN.md").exists() {
                    let _ = self.git_cmd(&["add", "PLAN.md"]);
                }
                let candidates = Self::auto_stage_candidates(&self.work_dir);
                let candidate_refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
                self.auto_stage_critical_files(&candidate_refs);
                return Ok(CommitResult {
                    commit_id: String::new(),
                    message: "All artifacts were gitignored — nothing was committed.".to_string(),
                    metadata: std::collections::HashMap::new(),
                    ignored_artifacts: ignored,
                });
            } else {
                // Split paths into those that exist on disk (git add) and those
                // that don't (deleted by the agent — git rm --cached).
                // This handles the case where an agent renames or deletes a file:
                // the artifact is still in the draft package but is absent from
                // the working tree after apply copies files from staging.
                let (existing, deleted): (Vec<_>, Vec<_>) = to_add
                    .iter()
                    .partition(|p| self.work_dir.join(p.as_str()).exists());

                if !existing.is_empty() {
                    let mut add_args = vec!["add"];
                    for p in &existing {
                        add_args.push(p.as_str());
                    }
                    self.git_cmd(&add_args)?;
                }

                if !deleted.is_empty() {
                    // --cached: remove from index only (file is already gone from disk).
                    // --ignore-unmatch: don't error if the path was never tracked.
                    let mut rm_args = vec!["rm", "--cached", "--ignore-unmatch"];
                    for p in &deleted {
                        rm_args.push(p.as_str());
                    }
                    tracing::info!(
                        count = deleted.len(),
                        paths = ?deleted,
                        "git rm --cached for deleted artifacts"
                    );
                    self.git_cmd(&rm_args)?;
                }

                // Auto-stage lock files, .ta/plan_history.jsonl, and user-configured
                // files that are modified but were not in the draft artifact list.
                // PLAN.md is always staged if it exists (may have been updated by apply).
                if self.work_dir.join("PLAN.md").exists() {
                    let _ = self.git_cmd(&["add", "PLAN.md"]);
                }
                let candidates = Self::auto_stage_candidates(&self.work_dir);
                let candidate_refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
                self.auto_stage_critical_files(&candidate_refs);
            }
            ignored
        };

        if artifact_paths.is_empty() {
            // Fall back to `git add .` when there are no fs:// artifacts
            // (e.g. all artifacts are external URIs like mailto://).
            self.git_cmd(&["add", "."])?;
        }

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
            ignored_artifacts,
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

        // Use self.config (not SubmitConfig::default()) so target_branch and
        // other git settings from workflow.toml are respected.
        let target_branch = &self.config.git.target_branch;
        let head_branch = self.branch_name(goal, &self.config);

        // Build PR body
        let body = self.build_pr_body(goal, pr, &self.config)?;

        tracing::info!(
            "GitAdapter: creating PR {} → {}",
            head_branch,
            target_branch
        );

        // Idempotency check: if a PR already exists for this branch (e.g., from
        // a prior apply attempt that failed after push), return the existing URL
        // rather than failing with "already exists".
        let existing = Command::new("gh")
            .args([
                "pr",
                "list",
                "--head",
                &head_branch,
                "--state",
                "open",
                "--json",
                "url,number",
                "--limit",
                "1",
            ])
            .current_dir(&self.work_dir)
            .output();
        if let Ok(out) = existing {
            if out.status.success() {
                let json = String::from_utf8_lossy(&out.stdout);
                if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(json.trim()) {
                    if let Some(existing_pr) = prs.into_iter().next() {
                        let url = existing_pr
                            .get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let number = existing_pr
                            .get("number")
                            .and_then(|v| v.as_u64())
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| {
                                url.split('/').next_back().unwrap_or("unknown").to_string()
                            });
                        if !url.is_empty() {
                            tracing::info!(
                                "GitAdapter: PR already exists for branch {}: {}",
                                head_branch,
                                url
                            );
                            // Still attempt auto-merge in case it wasn't enabled before.
                            if self.config.git.auto_merge {
                                let merge_strategy = &self.config.git.merge_strategy;
                                let merge_flag = match merge_strategy.as_str() {
                                    "rebase" => "--rebase",
                                    "merge" => "--merge",
                                    _ => "--squash",
                                };
                                let _ = Command::new("gh")
                                    .args(["pr", "merge", "--auto", merge_flag, &number])
                                    .current_dir(&self.work_dir)
                                    .output();
                            }
                            return Ok(ReviewResult {
                                review_url: url.clone(),
                                review_id: number,
                                message: format!("PR already open (reused): {}", url),
                                metadata: [("pr_url".to_string(), url)].into_iter().collect(),
                            });
                        }
                    }
                }
            }
        }

        // Create PR using gh CLI. Pass --head explicitly so the correct branch
        // is targeted even if the working tree HEAD has drifted (e.g. daemon
        // restart between push and PR creation).
        let output = Command::new("gh")
            .args([
                "pr",
                "create",
                "--head",
                &head_branch,
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

    fn protected_submit_targets(&self) -> Vec<String> {
        // Configured protected branches (from submit config), or the well-known defaults.
        let custom = &self.config.git.protected_branches;
        if !custom.is_empty() {
            return custom.clone();
        }
        vec![
            "main".to_string(),
            "master".to_string(),
            "trunk".to_string(),
            "dev".to_string(),
        ]
    }

    fn verify_not_on_protected_target(&self) -> Result<()> {
        let current = self.current_branch()?;
        let protected = self.protected_submit_targets();
        if protected.iter().any(|b| b == &current) {
            return Err(SubmitError::InvalidState(format!(
                "Refusing to commit: still on protected branch '{}' after prepare(). \
                 This would bypass the feature branch + PR workflow. \
                 Check that the VCS adapter created a feature branch, then \
                 re-run `ta draft apply --submit`.",
                current
            )));
        }
        Ok(())
    }

    fn stage_env(
        &self,
        staging_dir: &Path,
        config: &crate::config::VcsAgentConfig,
    ) -> Result<std::collections::HashMap<String, String>> {
        let mut env = std::collections::HashMap::new();

        // Always set author identity so the agent's git commits are clearly labeled.
        env.insert("GIT_AUTHOR_NAME".to_string(), "TA Agent".to_string());
        env.insert("GIT_COMMITTER_NAME".to_string(), "TA Agent".to_string());
        env.insert("GIT_AUTHOR_EMAIL".to_string(), "ta-agent@local".to_string());
        env.insert(
            "GIT_COMMITTER_EMAIL".to_string(),
            "ta-agent@local".to_string(),
        );

        match config.git_mode.as_str() {
            "none" => {
                // Block all git operations.
                env.insert("GIT_DIR".to_string(), "/dev/null".to_string());
            }
            "inherit-read" => {
                // Allow reading from the parent repo but block writes via ceiling.
                if config.ceiling_always {
                    if let Some(parent) = staging_dir.parent() {
                        env.insert(
                            "GIT_CEILING_DIRECTORIES".to_string(),
                            parent.to_string_lossy().to_string(),
                        );
                    }
                }
            }
            _ => {
                // "isolated" (default): init a fresh git repo in the staging dir.
                // Clear TA agent VCS env vars so git init creates .git in staging_dir,
                // not the workspace repo (GIT_DIR may be set by the outer agent env).
                let git_dir = staging_dir.join(".git");
                if !git_dir.exists() {
                    // Init the repo — try with -b main first, fall back without it
                    // for older git versions.
                    let init_output = std::process::Command::new("git")
                        .args(["init", "-b", "main"])
                        .current_dir(staging_dir)
                        .env_remove("GIT_DIR")
                        .env_remove("GIT_WORK_TREE")
                        .env_remove("GIT_CEILING_DIRECTORIES")
                        .output()
                        .map_err(|e| SubmitError::VcsError(format!("git init failed: {}", e)))?;
                    if !init_output.status.success() {
                        let init2 = std::process::Command::new("git")
                            .args(["init"])
                            .current_dir(staging_dir)
                            .env_remove("GIT_DIR")
                            .env_remove("GIT_WORK_TREE")
                            .env_remove("GIT_CEILING_DIRECTORIES")
                            .output()
                            .map_err(|e| {
                                SubmitError::VcsError(format!("git init failed: {}", e))
                            })?;
                        if !init2.status.success() {
                            let stderr = String::from_utf8_lossy(&init2.stderr);
                            return Err(SubmitError::VcsError(format!(
                                "git init in staging dir failed: {}",
                                stderr
                            )));
                        }
                    }
                    // Configure local identity so commits work without global config.
                    let _ = std::process::Command::new("git")
                        .args(["config", "user.name", "TA Agent"])
                        .current_dir(staging_dir)
                        .env_remove("GIT_DIR")
                        .env_remove("GIT_WORK_TREE")
                        .env_remove("GIT_CEILING_DIRECTORIES")
                        .output();
                    let _ = std::process::Command::new("git")
                        .args(["config", "user.email", "ta-agent@local"])
                        .current_dir(staging_dir)
                        .env_remove("GIT_DIR")
                        .env_remove("GIT_WORK_TREE")
                        .env_remove("GIT_CEILING_DIRECTORIES")
                        .output();

                    if config.init_baseline_commit {
                        // Create a baseline commit so `git diff` has something to compare
                        // against. Use -A to add all files (staging .taignore excludes .ta/).
                        let _ = std::process::Command::new("git")
                            .args(["add", "-A"])
                            .current_dir(staging_dir)
                            .env_remove("GIT_DIR")
                            .env_remove("GIT_WORK_TREE")
                            .env_remove("GIT_CEILING_DIRECTORIES")
                            .output();
                        let _ = std::process::Command::new("git")
                            .args(["commit", "--allow-empty", "-m", "pre-agent baseline"])
                            .current_dir(staging_dir)
                            .env_remove("GIT_DIR")
                            .env_remove("GIT_WORK_TREE")
                            .env_remove("GIT_CEILING_DIRECTORIES")
                            .env("GIT_AUTHOR_NAME", "TA Agent")
                            .env("GIT_AUTHOR_EMAIL", "ta-agent@local")
                            .env("GIT_COMMITTER_NAME", "TA Agent")
                            .env("GIT_COMMITTER_EMAIL", "ta-agent@local")
                            .output();
                    }
                }

                // Pin the agent to the staging repo.
                env.insert("GIT_DIR".to_string(), git_dir.to_string_lossy().to_string());
                env.insert(
                    "GIT_WORK_TREE".to_string(),
                    staging_dir.to_string_lossy().to_string(),
                );
                // Ceiling prevents git from looking outside staging_dir.
                if config.ceiling_always {
                    if let Some(parent) = staging_dir.parent() {
                        env.insert(
                            "GIT_CEILING_DIRECTORIES".to_string(),
                            parent.to_string_lossy().to_string(),
                        );
                    }
                }
            }
        }

        Ok(env)
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

    fn merge_review(&self, review_id: &str) -> Result<MergeResult> {
        if !self.has_gh_cli() {
            return Err(SubmitError::ReviewError(
                "gh CLI not found — install GitHub CLI to merge PRs automatically. \
                 Merge manually at the PR URL, then run `ta sync`."
                    .to_string(),
            ));
        }

        let merge_strategy = &self.config.git.merge_strategy;
        let merge_flag = match merge_strategy.as_str() {
            "rebase" => "--rebase",
            "merge" => "--merge",
            _ => "--squash",
        };

        tracing::info!(
            review_id = %review_id,
            strategy = %merge_strategy,
            "GitAdapter: merging PR"
        );

        let output = Command::new("gh")
            .args(["pr", "merge", review_id, "--auto", merge_flag])
            .current_dir(&self.work_dir)
            .output()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // Check if merged immediately or queued for auto-merge.
            let merged =
                !stdout.contains("auto-merge") && !stdout.is_empty() || stdout.contains("Merged");

            Ok(MergeResult {
                merged,
                merge_commit: None,
                message: if merged {
                    format!("PR #{} merged ({}).", review_id, merge_strategy)
                } else {
                    format!(
                        "Auto-merge enabled for PR #{} — will merge when CI passes.",
                        review_id
                    )
                },
                metadata: [
                    ("review_id".to_string(), review_id.to_string()),
                    ("strategy".to_string(), merge_strategy.clone()),
                ]
                .into_iter()
                .collect(),
            })
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            // "Pull request #N is not mergeable" — auto-merge may still be set.
            if stderr.contains("not mergeable") || stderr.contains("auto-merge") {
                Ok(MergeResult {
                    merged: false,
                    merge_commit: None,
                    message: format!(
                        "PR #{} is not yet mergeable (CI may be pending). \
                         Auto-merge is set — it will merge when checks pass. \
                         Run `ta draft watch <id>` to monitor.",
                        review_id
                    ),
                    metadata: [("review_id".to_string(), review_id.to_string())]
                        .into_iter()
                        .collect(),
                })
            } else {
                Err(SubmitError::ReviewError(format!(
                    "gh pr merge failed for PR #{}: {}",
                    review_id, stderr
                )))
            }
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
        // Clear TA agent VCS isolation env vars so test git operations target
        // the temp dir, not the staging directory's repo.
        let clear_git_env = |cmd: &mut Command| {
            cmd.env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_CEILING_DIRECTORIES");
        };

        let mut cmd = Command::new("git");
        cmd.args(["init"]).current_dir(dir);
        clear_git_env(&mut cmd);
        cmd.output()?;

        let mut cmd = Command::new("git");
        cmd.args(["config", "user.name", "Test User"])
            .current_dir(dir);
        clear_git_env(&mut cmd);
        cmd.output()?;

        let mut cmd = Command::new("git");
        cmd.args(["config", "user.email", "test@example.com"])
            .current_dir(dir);
        clear_git_env(&mut cmd);
        cmd.output()?;

        // Create initial commit
        std::fs::write(dir.join("README.md"), "# Test\n")?;

        let mut cmd = Command::new("git");
        cmd.args(["add", "."]).current_dir(dir);
        clear_git_env(&mut cmd);
        cmd.output()?;

        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", "Initial commit"])
            .current_dir(dir);
        clear_git_env(&mut cmd);
        cmd.output()?;

        Ok(())
    }

    #[test]
    fn test_git_adapter_protected_targets_default() {
        let dir = tempdir().unwrap();
        let adapter = GitAdapter::new(dir.path());
        let targets = adapter.protected_submit_targets();
        assert!(targets.contains(&"main".to_string()));
        assert!(targets.contains(&"master".to_string()));
        assert!(targets.contains(&"trunk".to_string()));
        assert!(targets.contains(&"dev".to_string()));
    }

    #[test]
    fn test_git_adapter_protected_targets_custom() {
        let dir = tempdir().unwrap();
        let config = SubmitConfig {
            git: crate::config::GitConfig {
                protected_branches: vec!["release".to_string(), "staging".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        let adapter = GitAdapter::with_config(dir.path(), config);
        let targets = adapter.protected_submit_targets();
        assert_eq!(targets, vec!["release", "staging"]);
    }

    #[test]
    fn test_verify_not_on_protected_target_feature_branch() {
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

        // Create a feature branch
        let config = SubmitConfig::default();
        adapter.prepare(&goal, &config).unwrap();

        // On a feature branch: verify should pass
        assert!(adapter.verify_not_on_protected_target().is_ok());
    }

    #[test]
    fn test_verify_not_on_protected_target_on_main() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        let adapter = GitAdapter::new(dir.path());

        // On main/master (initial branch after init): verify should fail
        let current = adapter.current_branch().unwrap();
        // Only test if we're on a protected branch
        if ["main", "master", "trunk", "dev"].contains(&current.as_str()) {
            assert!(adapter.verify_not_on_protected_target().is_err());
        }
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
    fn test_branch_name_backtick_title() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = SubmitConfig::default();

        // "`ta sync`" → should become "ta/ta-sync" (no leading/trailing dashes)
        let goal = GoalRun::new(
            "`ta sync`",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let branch = adapter.branch_name(&goal, &config);
        assert!(
            !branch.contains("--"),
            "consecutive dashes should be collapsed: {}",
            branch
        );
        assert!(
            !branch.ends_with('-'),
            "branch should not end with dash: {}",
            branch
        );
        let slug = branch.strip_prefix("ta/").unwrap_or(&branch);
        assert!(
            !slug.starts_with('-'),
            "slug should not start with dash: {}",
            branch
        );
    }

    #[test]
    fn test_branch_name_all_special_chars() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = SubmitConfig::default();

        // All special chars → should fall back to "goal"
        let goal = GoalRun::new(
            "!!! ???",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let branch = adapter.branch_name(&goal, &config);
        assert!(
            branch.ends_with("goal"),
            "fallback should be 'goal': {}",
            branch
        );
    }

    #[test]
    fn test_branch_name_single_quotes_and_spaces() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = SubmitConfig::default();

        // "Fix 'ta run' timeout" → "ta/fix-ta-run-timeout"
        let goal = GoalRun::new(
            "Fix 'ta run' timeout",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let branch = adapter.branch_name(&goal, &config);
        assert!(!branch.contains("--"), "no consecutive dashes: {}", branch);
        assert!(branch.contains("fix"), "should contain 'fix': {}", branch);
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
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .unwrap();

        // Detect the actual default branch name (may be "main" or "master").
        let branch_output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(local_dir.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
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
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "Remote commit"])
            .current_dir(remote_dir.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES")
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

    // ── VCS isolation tests (v0.13.17.3) ─────────────────────────────────────

    #[test]
    fn test_git_none_mode_sets_dev_null() {
        let dir = tempdir().unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = crate::config::VcsAgentConfig {
            git_mode: "none".to_string(),
            ..Default::default()
        };
        let env = adapter.stage_env(dir.path(), &config).unwrap();
        assert_eq!(env.get("GIT_DIR").map(|s| s.as_str()), Some("/dev/null"));
        assert!(!env.contains_key("GIT_WORK_TREE"));
    }

    #[test]
    fn test_git_inherit_read_sets_ceiling() {
        let dir = tempdir().unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = crate::config::VcsAgentConfig {
            git_mode: "inherit-read".to_string(),
            ceiling_always: true,
            ..Default::default()
        };
        let env = adapter.stage_env(dir.path(), &config).unwrap();
        assert!(env.contains_key("GIT_CEILING_DIRECTORIES"));
        let ceiling = env.get("GIT_CEILING_DIRECTORIES").unwrap();
        assert_eq!(ceiling, dir.path().parent().unwrap().to_str().unwrap());
    }

    #[test]
    fn test_git_isolated_inits_repo() {
        let dir = tempdir().unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = crate::config::VcsAgentConfig {
            git_mode: "isolated".to_string(),
            init_baseline_commit: false, // skip commit for speed
            ..Default::default()
        };
        let env = adapter.stage_env(dir.path(), &config).unwrap();
        // A .git directory should now exist in the staging dir.
        assert!(
            dir.path().join(".git").exists(),
            ".git should be created by isolated mode"
        );
        // GIT_DIR should point to the staging .git.
        let git_dir = env.get("GIT_DIR").unwrap();
        assert!(
            git_dir.contains(".git"),
            "GIT_DIR should point to staging .git"
        );
        // GIT_WORK_TREE should be the staging dir.
        let work_tree = env.get("GIT_WORK_TREE").unwrap();
        assert_eq!(work_tree, dir.path().to_str().unwrap());
    }

    #[test]
    fn test_git_isolated_sets_ceiling() {
        let dir = tempdir().unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = crate::config::VcsAgentConfig {
            git_mode: "isolated".to_string(),
            ceiling_always: true,
            init_baseline_commit: false,
            ..Default::default()
        };
        let env = adapter.stage_env(dir.path(), &config).unwrap();
        assert!(
            env.contains_key("GIT_CEILING_DIRECTORIES"),
            "GIT_CEILING_DIRECTORIES should be set in isolated mode"
        );
    }

    #[test]
    fn test_git_ceiling_prevents_upward_traversal() {
        let dir = tempdir().unwrap();
        let adapter = GitAdapter::new(dir.path());
        let config = crate::config::VcsAgentConfig {
            git_mode: "isolated".to_string(),
            ceiling_always: true,
            init_baseline_commit: false,
            ..Default::default()
        };
        let env = adapter.stage_env(dir.path(), &config).unwrap();
        let ceiling = env.get("GIT_CEILING_DIRECTORIES").unwrap();
        // The ceiling must be above the staging dir (its parent), not the staging
        // dir itself — otherwise git could still discover the developer's .git above.
        let staging_path = dir.path().to_str().unwrap();
        assert_ne!(
            ceiling.as_str(),
            staging_path,
            "GIT_CEILING_DIRECTORIES should be parent of staging dir, not staging dir itself"
        );
    }

    #[test]
    fn test_artifact_path_extraction_from_uris() {
        // Verify the logic for extracting fs:// artifact paths used in commit().
        // Non-fs URIs should be excluded so we only add real filesystem paths.
        let uris = [
            "fs://workspace/src/main.rs",
            "fs://workspace/Cargo.toml",
            "mailto://nowhere",         // non-fs, should be excluded
            "fs://workspace/README.md", // fs, should be included
        ];
        let fs_paths: Vec<String> = uris
            .iter()
            .filter_map(|uri| uri.strip_prefix("fs://workspace/").map(|p| p.to_string()))
            .collect();
        assert_eq!(fs_paths.len(), 3);
        assert!(fs_paths.contains(&"src/main.rs".to_string()));
        assert!(fs_paths.contains(&"Cargo.toml".to_string()));
        assert!(fs_paths.contains(&"README.md".to_string()));
        // non-fs URI is filtered out
        assert!(!fs_paths.iter().any(|p| p.contains("mailto")));
    }

    // ── v0.13.17.5: gitignore filtering tests ─────────────────────

    /// test_known_safe_dropped_silently (plan item 9.3):
    /// Known-safe paths (.mcp.json, *.local.toml, .ta/ runtime files) are
    /// classified as known_safe=true by is_known_safe_ignored().
    #[test]
    fn test_known_safe_classification() {
        assert!(GitAdapter::is_known_safe_ignored(".mcp.json"));
        assert!(GitAdapter::is_known_safe_ignored("settings.local.toml"));
        assert!(GitAdapter::is_known_safe_ignored("project.local.toml"));
        assert!(GitAdapter::is_known_safe_ignored(".ta/daemon.toml"));
        assert!(GitAdapter::is_known_safe_ignored(".ta/agent.pid"));
        assert!(GitAdapter::is_known_safe_ignored(".ta/staging.lock"));
        // Non-known-safe paths.
        assert!(!GitAdapter::is_known_safe_ignored("src/main.rs"));
        assert!(!GitAdapter::is_known_safe_ignored("Cargo.toml"));
        assert!(!GitAdapter::is_known_safe_ignored("secret.txt"));
    }

    /// test_filter_gitignored_artifacts — .mcp.json gitignored → known_safe=true (plan item 9.3).
    #[test]
    fn test_known_safe_dropped_silently() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        // Add .mcp.json to .gitignore.
        std::fs::write(dir.path().join(".gitignore"), ".mcp.json\n").unwrap();

        let adapter = GitAdapter::new(dir.path());
        let paths = vec![".mcp.json".to_string(), "README.md".to_string()];
        let (to_add, ignored) = adapter.filter_gitignored_artifacts(&paths);

        assert_eq!(to_add, vec!["README.md".to_string()]);
        assert_eq!(ignored.len(), 1);
        assert_eq!(ignored[0].path, ".mcp.json");
        assert!(
            ignored[0].known_safe,
            ".mcp.json must be classified as known_safe"
        );
    }

    /// test_unexpected_ignored_warns (plan item 9.4):
    /// A source file that happens to be gitignored is classified as known_safe=false.
    #[test]
    fn test_unexpected_ignored() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        // Add a source file to .gitignore (unusual but possible).
        std::fs::write(dir.path().join(".gitignore"), "src/secret.rs\n").unwrap();

        let adapter = GitAdapter::new(dir.path());
        let paths = vec!["src/secret.rs".to_string(), "README.md".to_string()];
        let (to_add, ignored) = adapter.filter_gitignored_artifacts(&paths);

        assert_eq!(to_add, vec!["README.md".to_string()]);
        assert_eq!(ignored.len(), 1);
        assert_eq!(ignored[0].path, "src/secret.rs");
        assert!(
            !ignored[0].known_safe,
            "src/secret.rs must be unexpected-ignored"
        );
    }

    /// test_all_ignored_completes_with_warning (plan item 9.5):
    /// When all artifacts are gitignored, filter returns empty to_add list.
    /// The commit() caller handles this gracefully (no panic, no error).
    #[test]
    fn test_all_ignored_returns_empty_to_add() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        std::fs::write(
            dir.path().join(".gitignore"),
            ".mcp.json\nsettings.local.toml\n",
        )
        .unwrap();

        let adapter = GitAdapter::new(dir.path());
        let paths = vec![".mcp.json".to_string(), "settings.local.toml".to_string()];
        let (to_add, ignored) = adapter.filter_gitignored_artifacts(&paths);

        assert!(to_add.is_empty(), "all paths should be filtered out");
        assert_eq!(ignored.len(), 2);
        assert!(ignored.iter().all(|a| a.known_safe), "both are known-safe");
    }

    // ── v0.14.3.7: lock file auto-staging ────────────────────────

    #[test]
    fn builtin_lock_files_contains_expected_entries() {
        let list = GitAdapter::BUILTIN_LOCK_FILES;
        assert!(list.contains(&"Cargo.lock"));
        assert!(list.contains(&"package-lock.json"));
        assert!(list.contains(&"go.sum"));
        assert!(list.contains(&"poetry.lock"));
        assert!(list.contains(&"yarn.lock"));
        assert!(list.contains(&"bun.lockb"));
        assert!(list.contains(&"flake.lock"));
        assert!(list.contains(&"Pipfile.lock"));
    }

    #[test]
    fn auto_stage_candidates_includes_builtin_and_plan_history() {
        let dir = tempdir().unwrap();
        let candidates = GitAdapter::auto_stage_candidates(dir.path());
        // Built-in lock files must be present.
        assert!(candidates.iter().any(|c| c == "Cargo.lock"));
        assert!(candidates.iter().any(|c| c == "go.sum"));
        // TA state file must be present.
        assert!(candidates.iter().any(|c| c == ".ta/plan_history.jsonl"));
    }

    #[test]
    fn auto_stage_candidates_merges_user_config() {
        let dir = tempdir().unwrap();
        // Create workflow.toml with a custom auto_stage entry.
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[commit]\nauto_stage = [\"docs/generated/api.md\"]\n",
        )
        .unwrap();
        let candidates = GitAdapter::auto_stage_candidates(dir.path());
        assert!(
            candidates.iter().any(|c| c == "docs/generated/api.md"),
            "user-configured entry should be present"
        );
        // Built-in entries must still be present.
        assert!(candidates.iter().any(|c| c == "Cargo.lock"));
    }

    #[test]
    fn auto_stage_candidates_no_duplicates_with_user_config() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        // User lists Cargo.lock, which is already in the built-in list.
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[commit]\nauto_stage = [\"Cargo.lock\"]\n",
        )
        .unwrap();
        let candidates = GitAdapter::auto_stage_candidates(dir.path());
        let cargo_lock_count = candidates
            .iter()
            .filter(|c| c.as_str() == "Cargo.lock")
            .count();
        assert_eq!(cargo_lock_count, 1, "Cargo.lock should appear exactly once");
    }

    /// Run a git command in `dir` without TA env var interference.
    fn git_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(dir);
        cmd.env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_CEILING_DIRECTORIES");
        cmd.output().unwrap()
    }

    #[test]
    fn auto_stage_critical_files_stages_modified_file() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        // Create and commit Cargo.lock initially.
        std::fs::write(dir.path().join("Cargo.lock"), "version = 3\n").unwrap();
        git_in(dir.path(), &["add", "Cargo.lock"]);
        git_in(dir.path(), &["commit", "-m", "add lock"]);

        // Modify Cargo.lock (simulating a version bump regenerating it).
        std::fs::write(dir.path().join("Cargo.lock"), "version = 3\n# updated\n").unwrap();

        let adapter = GitAdapter::new(dir.path());
        adapter.auto_stage_critical_files(&["Cargo.lock"]);

        // Cargo.lock should now be in the index.
        let output = git_in(dir.path(), &["diff", "--cached", "--name-only"]);
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(
            staged.contains("Cargo.lock"),
            "Cargo.lock should be staged after auto_stage_critical_files"
        );
    }

    #[test]
    fn auto_stage_critical_files_skips_unmodified_file() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        // Create and commit Cargo.lock.
        std::fs::write(dir.path().join("Cargo.lock"), "version = 3\n").unwrap();
        git_in(dir.path(), &["add", "Cargo.lock"]);
        git_in(dir.path(), &["commit", "-m", "add lock"]);

        // Do NOT modify Cargo.lock — it should not be staged.
        let adapter = GitAdapter::new(dir.path());
        adapter.auto_stage_critical_files(&["Cargo.lock"]);

        let output = git_in(dir.path(), &["diff", "--cached", "--name-only"]);
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(
            !staged.contains("Cargo.lock"),
            "Cargo.lock should not be staged when unmodified"
        );
    }

    #[test]
    fn auto_stage_critical_files_skips_nonexistent_file() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path()).unwrap();

        // Cargo.lock does not exist — auto_stage_critical_files should not error.
        let adapter = GitAdapter::new(dir.path());
        adapter.auto_stage_critical_files(&["Cargo.lock"]); // must not panic

        let output = git_in(dir.path(), &["diff", "--cached", "--name-only"]);
        let staged = String::from_utf8_lossy(&output.stdout);
        assert!(!staged.contains("Cargo.lock"));
    }
}
