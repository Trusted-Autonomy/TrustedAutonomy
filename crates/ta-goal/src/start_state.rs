//! Goal start state capture (v0.17.12.17).
//!
//! Records which commit a goal started from so the goal can be replayed
//! against the same code. Git only: other VCS kinds record nothing, and the
//! caller says so. Only repository facts are recorded (commit SHA, branch,
//! whether tracked files had uncommitted changes), never environment values.

use std::path::Path;
use std::process::Command;

use crate::goal_run::GoalRun;

/// What was true of the source project when the goal started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartState {
    /// Full SHA of `HEAD`; `None` for a repo with no commit yet.
    pub commit: Option<String>,
    /// Checked-out branch; `None` for a detached `HEAD`.
    pub branch: Option<String>,
    /// Tracked files had uncommitted changes (untracked files are ignored:
    /// they do not change what `commit` checks out, and `.ta/` is routinely
    /// untracked).
    pub dirty: bool,
}

/// Run `git <args>` in `root`. `Ok(None)` when git exits non-zero (for
/// example `rev-parse HEAD` in a repo with no commits).
fn git(root: &Path, args: &[&str]) -> anyhow::Result<Option<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // An inherited GIT_DIR / GIT_WORK_TREE / GIT_INDEX_FILE (set inside
        // an isolated agent environment) would redirect git to a different
        // repository than `root`.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "could not run `git {}` in {} to record the goal's start state: {}. \
                 Install git or put it on PATH; the goal can still run without a recorded start state.",
                args.join(" "),
                root.display(),
                e
            )
        })?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Capture the git start state of `root`.
///
/// `Ok(None)` when `root` is not inside a git work tree. `Err` only when git
/// itself cannot be run.
pub fn capture_git_start_state(root: &Path) -> anyhow::Result<Option<StartState>> {
    if git(root, &["rev-parse", "--is-inside-work-tree"])?.as_deref() != Some("true") {
        return Ok(None);
    }
    let commit = git(root, &["rev-parse", "--verify", "HEAD"])?.filter(|s| !s.is_empty());
    let branch =
        git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?.filter(|s| !s.is_empty());
    let dirty = git(root, &["status", "--porcelain", "--untracked-files=no"])?
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    Ok(Some(StartState {
        commit,
        branch,
        dirty,
    }))
}

/// Record `state` on `goal`.
pub fn apply(goal: &mut GoalRun, state: StartState) {
    goal.start_commit = state.commit;
    goal.start_branch = state.branch;
    goal.start_dirty = state.dirty;
}

/// Plain line shown by `ta goal status` when the goal cannot be replayed exactly.
pub const DIRTY_NOTICE: &str =
    "this goal started from uncommitted changes and cannot be replayed exactly";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn run_git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {:?} failed", args);
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "one").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn clean_repo_records_commit_and_branch() {
        let dir = repo();
        let s = capture_git_start_state(dir.path()).unwrap().unwrap();
        assert_eq!(s.commit.as_deref().map(str::len), Some(40));
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(!s.dirty);
    }

    #[test]
    fn modified_tracked_file_is_dirty_but_untracked_is_not() {
        let dir = repo();
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        assert!(!capture_git_start_state(dir.path()).unwrap().unwrap().dirty);
        std::fs::write(dir.path().join("a.txt"), "two").unwrap();
        assert!(capture_git_start_state(dir.path()).unwrap().unwrap().dirty);
    }

    #[test]
    fn detached_head_has_no_branch() {
        let dir = repo();
        run_git(dir.path(), &["checkout", "-q", "--detach"]);
        let s = capture_git_start_state(dir.path()).unwrap().unwrap();
        assert!(s.commit.is_some());
        assert_eq!(s.branch, None);
    }

    #[test]
    fn non_git_directory_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(capture_git_start_state(dir.path()).unwrap(), None);
    }

    #[test]
    fn apply_sets_fields_and_old_records_load() {
        let dir = repo();
        let mut g = GoalRun::new("t", "o", "a", PathBuf::from("/w"), PathBuf::from("/s"));
        // A record written before these fields existed still loads.
        let old = serde_json::to_string(&g).unwrap();
        assert!(!old.contains("start_commit"));
        let loaded: GoalRun = serde_json::from_str(&old).unwrap();
        assert_eq!(loaded.start_commit, None);
        assert!(!loaded.start_dirty);

        apply(
            &mut g,
            capture_git_start_state(dir.path()).unwrap().unwrap(),
        );
        let back: GoalRun = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(back.start_commit, g.start_commit);
        assert_eq!(back.start_branch.as_deref(), Some("main"));
    }

    #[test]
    fn captured_state_holds_no_environment_values() {
        let dir = repo();
        std::env::set_var("TA_START_STATE_SECRET_PROBE", "s3cr3t-value");
        let mut g = GoalRun::new("t", "o", "a", PathBuf::from("/w"), PathBuf::from("/s"));
        apply(
            &mut g,
            capture_git_start_state(dir.path()).unwrap().unwrap(),
        );
        let json = serde_json::to_string(&g).unwrap();
        std::env::remove_var("TA_START_STATE_SECRET_PROBE");
        assert!(!json.contains("s3cr3t-value"));
    }
}
