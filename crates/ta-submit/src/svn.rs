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
use ta_goal::CommitContext;

use crate::adapter::{
    CommitResult, PushResult, Result, ReviewResult, SourceAdapter, SubmitError, SyncResult,
};
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

/// Result of [`ensure_ta_ignore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvnIgnoreOutcome {
    /// Patterns added to `svn:ignore` on `.ta` (empty when already complete).
    pub added: Vec<String>,
    /// True when the property was actually set (false for a dry run or when
    /// nothing was missing).
    pub applied: bool,
}

/// Output of one `svn` invocation: `(success, stdout, stderr)`.
pub type SvnRun = std::io::Result<(bool, String, String)>;

fn run_real_svn(project_root: &Path, args: &[&str]) -> SvnRun {
    let out = Command::new("svn")
        .args(args)
        .current_dir(project_root)
        .output()?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

/// Add TA's local-state patterns to the `svn:ignore` property of `.ta`.
///
/// Subversion has no ignore file and `svn:ignore` has no comment syntax, so
/// there is no marker block to own. The only change TA ever makes is
/// appending patterns that are missing; existing lines (the project's own
/// ignores) are never removed, reordered or rewritten, and no tracked file
/// content is touched. The property change itself is a working-copy
/// modification the user commits like any other.
///
/// Requires `.ta` to be under version control (`svn add --depth=empty .ta`).
pub fn ensure_ta_ignore(project_root: &Path, dry_run: bool) -> Result<SvnIgnoreOutcome> {
    ensure_ta_ignore_with(&|args| run_real_svn(project_root, args), dry_run)
}

/// [`ensure_ta_ignore`] with an injectable `svn` runner (for tests).
pub fn ensure_ta_ignore_with(
    run: &dyn Fn(&[&str]) -> SvnRun,
    dry_run: bool,
) -> Result<SvnIgnoreOutcome> {
    let missing_svn = |e: std::io::Error| {
        SubmitError::VcsError(format!(
            "could not run `svn` ({e}). Install Subversion and make sure `svn` is on PATH, \
             then re-run `ta setup vcs --vcs svn`"
        ))
    };
    let (ok, current, stderr) = run(&["propget", "svn:ignore", ".ta"]).map_err(missing_svn)?;
    if !ok {
        return Err(SubmitError::VcsError(format!(
            "`svn propget svn:ignore .ta` failed: {}. `.ta` must be under version control \
             before TA can set svn:ignore on it. Run `svn add --depth=empty .ta`, then re-run \
             `ta setup vcs --vcs svn`",
            stderr.trim()
        )));
    }
    let (merged, added) = ta_workspace::merge_svn_ignore(&current);
    if added.is_empty() || dry_run {
        return Ok(SvnIgnoreOutcome {
            added,
            applied: false,
        });
    }
    let (ok, _, stderr) = run(&["propset", "svn:ignore", &merged, ".ta"]).map_err(missing_svn)?;
    if !ok {
        return Err(SubmitError::VcsError(format!(
            "`svn propset svn:ignore .ta` failed: {}. Nothing was changed. Fix the \
             working copy error above and re-run `ta setup vcs --vcs svn`",
            stderr.trim()
        )));
    }
    Ok(SvnIgnoreOutcome {
        added,
        applied: true,
    })
}

impl SourceAdapter for SvnAdapter {
    fn prepare(&self, _ctx: &CommitContext, _config: &SubmitConfig) -> Result<()> {
        // SVN doesn't use branches the same way as Git.
        // No-op: the working copy is already pointing at the correct location.
        tracing::debug!("SvnAdapter: prepare() — no-op (SVN working copy)");
        Ok(())
    }

    fn commit(
        &self,
        ctx: &CommitContext,
        _pr: &DraftPackage,
        message: &str,
    ) -> Result<CommitResult> {
        tracing::info!("SvnAdapter: committing changes");

        // Add any new (unversioned) files.
        // `svn add` with --force adds unversioned files without erroring on already-tracked ones.
        let _ = self.svn_cmd(&["add", "--force", "."]);

        // Build commit message with goal metadata.
        let commit_msg = format!("{}\n\nGoal-ID: {}", message, ctx.goal_run_id);

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
            ignored_artifacts: vec![],
        })
    }

    fn push(&self, _ctx: &CommitContext) -> Result<PushResult> {
        // SVN commit is already remote — no separate push step.
        tracing::debug!("SvnAdapter: push() — no-op (SVN commit is already remote)");
        Ok(PushResult {
            remote_ref: "svn://committed".to_string(),
            message: "SVN commit is already remote — no push needed".to_string(),
            metadata: Default::default(),
        })
    }

    fn open_review(&self, _ctx: &CommitContext, _pr: &DraftPackage) -> Result<ReviewResult> {
        // SVN doesn't have built-in code review.
        tracing::debug!("SvnAdapter: open_review() — no-op (SVN has no built-in review)");
        Ok(ReviewResult {
            review_url: "svn://no-review".to_string(),
            review_id: "none".to_string(),
            message: "SVN has no built-in review workflow. Consider using a code review tool like Crucible or ReviewBoard.".to_string(),
            metadata: Default::default(),
        })
    }

    fn sync_upstream(&self) -> Result<SyncResult> {
        tracing::info!("SvnAdapter: running svn update");

        match self.svn_cmd(&["update"]) {
            Ok(output) => {
                // Try to detect conflicts from "C " prefix lines in svn update output.
                let conflicts: Vec<String> = output
                    .lines()
                    .filter(|l| l.starts_with("C ") || l.starts_with("C\t"))
                    .map(|l| l[2..].trim().to_string())
                    .collect();

                // Try to count updated files from "U " prefix lines.
                let updated_count = output
                    .lines()
                    .filter(|l| l.starts_with("U ") || l.starts_with("A ") || l.starts_with("D "))
                    .count();

                Ok(SyncResult {
                    updated: updated_count > 0 || !conflicts.is_empty(),
                    conflicts,
                    new_commits: updated_count as u32,
                    message: format!(
                        "svn update completed. {}",
                        output.lines().last().unwrap_or("")
                    ),
                    metadata: Default::default(),
                })
            }
            Err(e) => Err(SubmitError::SyncError(format!("svn update failed: {}", e))),
        }
    }

    fn name(&self) -> &str {
        "svn"
    }

    fn exclude_patterns(&self) -> Vec<String> {
        vec![".svn/".to_string()]
    }

    fn is_path_ignored(&self, project_root: &Path, ta_rel_path: &str) -> Result<bool> {
        // SVN has no VcsBackend variant in ta-workspace; check inline via `svn status --no-ignore`.
        // A line starting with 'I' means the path is ignored by svn:ignore properties.
        let path = format!(".ta/{}", ta_rel_path);
        let out = Command::new("svn")
            .args(["status", "--no-ignore", &path])
            .current_dir(project_root)
            .output()
            .map_err(|e| SubmitError::VcsError(format!("svn status failed: {}", e)))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(stdout.lines().any(|l| l.starts_with('I')))
    }

    fn commit_diff(&self) -> Option<String> {
        match self.svn_cmd(&["diff", "-c", "HEAD"]) {
            Ok(diff) => Some(diff),
            Err(e) => {
                tracing::warn!("SvnAdapter: commit_diff failed ({}); scan skipped", e);
                None
            }
        }
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

    fn is_dirty(&self) -> Result<bool> {
        // svn status: non-empty output means there are local modifications.
        let output = self.svn_cmd(&["status"]);
        match output {
            Ok(s) => Ok(!s.trim().is_empty()),
            Err(_) => Ok(false),
        }
    }

    fn list_tracked_files(&self) -> Result<Vec<std::path::PathBuf>> {
        // svn list --depth infinity: lists all versioned files.
        let output = self.svn_cmd(&["list", "--depth", "infinity"])?;
        let files = output
            .lines()
            .filter(|l| !l.ends_with('/'))
            .map(|l| self.work_dir.join(l))
            .collect();
        Ok(files)
    }

    fn head_sha(&self) -> Option<String> {
        // svn info --show-item revision: returns the current working-copy revision.
        self.svn_cmd(&["info", "--show-item", "revision"])
            .ok()
            .map(|s| format!("r{}", s.trim()))
    }

    fn log_since(&self, ref_: &str) -> Result<Vec<crate::adapter::CommitSummary>> {
        // svn log -r <rev>:HEAD --limit 50: returns commits since a given revision.
        let rev_range = if ref_.is_empty() {
            "1:HEAD".to_string()
        } else {
            format!("{}:HEAD", ref_.trim_start_matches('r'))
        };
        let output = self.svn_cmd(&["log", "-r", &rev_range, "--limit", "50"])?;
        let summaries = output
            .lines()
            .filter(|l| l.starts_with('r') && l.contains(" | "))
            .map(|l| {
                let parts: Vec<&str> = l.splitn(3, " | ").collect();
                crate::adapter::CommitSummary {
                    sha: parts.first().unwrap_or(&l).trim().to_string(),
                    subject: parts.get(2).unwrap_or(&"").trim().to_string(),
                }
            })
            .collect();
        Ok(summaries)
    }

    fn checkout_branch(&self, _branch: &str) -> Result<()> {
        // SVN uses switch (svn switch URL) not checkout for branch changes.
        Err(SubmitError::VcsError(
            "SVN: use `svn switch <url>` to change branches".to_string(),
        ))
    }

    fn create_tag(&self, tag: &str, message: &str) -> Result<()> {
        // SVN tags are copies in the repository.
        let _ = (tag, message);
        Err(SubmitError::VcsError(
            "SVN: create tags via `svn copy trunk tags/<name>` in your repository".to_string(),
        ))
    }

    fn tag_exists(&self, tag: &str) -> Result<bool> {
        let _ = tag;
        Err(SubmitError::VcsError(
            "SVN: check tags via `svn list <repo>/tags`".to_string(),
        ))
    }

    fn push_tag(&self, tag: &str) -> Result<()> {
        let _ = tag;
        Err(SubmitError::VcsError(
            "SVN: tags are copies in the repo; use `svn copy` to create and they're immediately remote".to_string(),
        ))
    }

    fn protected_submit_targets(&self) -> Vec<String> {
        // SVN paths that agents must not commit directly to.
        // Default: /trunk (the conventional integration line).
        vec!["/trunk".to_string()]
    }

    fn verify_not_on_protected_target(&self) -> Result<()> {
        // Check the working copy's URL via `svn info --show-item url`.
        // SVN's `prepare()` is currently a no-op (no branching), so this
        // guard blocks commits to /trunk until proper branch/copy support
        // is added.
        let url_result = self.svn_cmd(&["info", "--show-item", "url"]);
        match url_result {
            Ok(url) => {
                let protected = self.protected_submit_targets();
                for target in &protected {
                    if url.contains(target.as_str()) {
                        return Err(SubmitError::InvalidState(format!(
                            "Refusing to commit: working copy URL '{}' contains protected path \
                             '{}'. SVN branching is not yet supported — use a branch or \
                             feature copy before applying changes to a protected path.",
                            url, target
                        )));
                    }
                }
                Ok(())
            }
            Err(_) => {
                // svn not installed or not an SVN working copy — allow (svn commit
                // would also fail in this case, providing its own error).
                tracing::warn!(
                    "SvnAdapter: could not run `svn info` for protected target check — skipping"
                );
                Ok(())
            }
        }
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
    fn test_svn_adapter_protected_targets() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = SvnAdapter::new(dir.path());
        let targets = adapter.protected_submit_targets();
        assert!(targets.contains(&"/trunk".to_string()));
    }

    /// v0.17.7.2 item 5: a non-CI adapter (SVN has no "CI check" concept)
    /// must stay on `SourceAdapter::check_failures()`'s empty-vec default —
    /// verified against the real `SvnAdapter`, not just a mock, since
    /// `CiFailureTrigger` degrades based on this exact return value.
    #[test]
    fn test_svn_adapter_check_failures_uses_default_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = SvnAdapter::new(dir.path());
        assert_eq!(adapter.check_failures("1234").unwrap(), Vec::new());
    }

    #[test]
    fn test_svn_adapter_verify_degrades_without_svn() {
        // Without svn CLI or a real working copy, verify should degrade gracefully.
        let dir = tempfile::tempdir().unwrap();
        let adapter = SvnAdapter::new(dir.path());
        // Not an SVN working copy, so svn info will fail → should return Ok
        assert!(adapter.verify_not_on_protected_target().is_ok());
    }

    #[test]
    fn test_svn_adapter_push_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = SvnAdapter::new(dir.path());
        let goal = ta_goal::GoalRun::new(
            "Test",
            "Test",
            "test-agent",
            dir.path().to_path_buf(),
            dir.path().join("store"),
        );
        let ctx = ta_goal::CommitContext::from(&goal);
        let result = adapter.push(&ctx).unwrap();
        assert_eq!(result.remote_ref, "svn://committed");
    }

    #[test]
    fn ensure_ta_ignore_appends_only_missing_and_keeps_existing() {
        use std::cell::RefCell;
        let calls: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
        let run = |args: &[&str]| -> SvnRun {
            calls
                .borrow_mut()
                .push(args.iter().map(|a| a.to_string()).collect());
            if args[0] == "propget" {
                Ok((true, "my-own-ignore\nstaging\n".into(), String::new()))
            } else {
                Ok((true, String::new(), String::new()))
            }
        };
        let out = ensure_ta_ignore_with(&run, false).unwrap();
        assert!(out.applied);
        assert!(!out.added.contains(&"staging".to_string()));
        assert!(out.added.contains(&"setup-progress.json".to_string()));
        let calls = calls.borrow();
        let set = calls.iter().find(|c| c[0] == "propset").unwrap();
        assert!(set[2].starts_with("my-own-ignore\nstaging\n"));
        assert_eq!(set[3], ".ta");
    }

    #[test]
    fn ensure_ta_ignore_dry_run_never_sets() {
        let run = |args: &[&str]| -> SvnRun {
            assert_eq!(args[0], "propget", "dry run must not propset");
            Ok((true, String::new(), String::new()))
        };
        let out = ensure_ta_ignore_with(&run, true).unwrap();
        assert!(!out.applied);
        assert!(!out.added.is_empty());
    }

    #[test]
    fn ensure_ta_ignore_complete_property_is_a_noop() {
        let all = ta_workspace::svn_ignore_patterns().join("\n");
        let run = |args: &[&str]| -> SvnRun {
            assert_eq!(args[0], "propget");
            Ok((true, all.clone(), String::new()))
        };
        let out = ensure_ta_ignore_with(&run, false).unwrap();
        assert!(out.added.is_empty() && !out.applied);
    }

    #[test]
    fn ensure_ta_ignore_unversioned_ta_dir_gives_actionable_error() {
        let run = |_: &[&str]| -> SvnRun {
            Ok((
                false,
                String::new(),
                "svn: E200009: not under version control".into(),
            ))
        };
        let err = ensure_ta_ignore_with(&run, false).unwrap_err().to_string();
        assert!(err.contains("svn add --depth=empty .ta"), "{err}");
    }

    #[test]
    fn ensure_ta_ignore_missing_svn_binary_gives_actionable_error() {
        let run = |_: &[&str]| -> SvnRun {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "not found",
            ))
        };
        let err = ensure_ta_ignore_with(&run, false).unwrap_err().to_string();
        assert!(err.contains("Install Subversion"), "{err}");
    }
}
