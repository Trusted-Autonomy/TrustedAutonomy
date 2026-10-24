//! Decide whether a goal's staging directory may be deleted.
//!
//! Every garbage collector that removes staging (`ta gc`, the staging-size cap,
//! the daemon's startup and periodic GC pass) asks [`assess_staging`] first. The
//! answer fails closed: when it is not certain that the staging directory holds
//! no uncollected work, it is kept and the caller is handed the exact command
//! that recovers it.
//!
//! This exists because on 2026-10-16 a goal that the watchdog had wrongly marked
//! `failed` had its finished, uncollected work deleted by a GC pass, so
//! `ta goal recover` had nothing left to recover.

use std::path::{Path, PathBuf};

use ta_changeset::draft_package::{DraftPackage, DraftStatus};
use ta_goal::{GoalRun, GoalRunState};

use crate::overlay::ExcludePatterns;

/// Largest file compared byte for byte when looking for uncollected work. A
/// larger file that has the same size as its source twin is treated as changed
/// (keep the staging directory) rather than read into memory.
const MAX_COMPARE_BYTES: u64 = 64 * 1024 * 1024;

/// Reason prefix `ta goal recover` writes when the user cancels a goal. The
/// user decided to discard that work, so its staging is reclaimable.
const USER_CANCEL_REASON_PREFIX: &str = "Cancelled by user";

/// Whether a goal's staging directory may be deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagingVerdict {
    /// Nothing in staging is uncollected; deleting it loses nothing.
    Reclaimable { why: String },
    /// The goal is still being worked on (running, post-agent, or its owning
    /// `ta run` is alive). Never reclaimed.
    InUse { why: String },
    /// Staging holds work no applied or reviewable draft contains.
    Uncollected {
        changed_files: usize,
        recover_command: String,
        why: String,
    },
    /// Could not be determined. Treated like `Uncollected`: keep.
    Unsure {
        why: String,
        recover_command: String,
    },
}

impl StagingVerdict {
    /// True when deleting the staging directory loses nothing.
    pub fn is_reclaimable(&self) -> bool {
        matches!(self, StagingVerdict::Reclaimable { .. })
    }

    /// One line saying why the directory was kept and what the user can run.
    /// `None` for a reclaimable directory.
    pub fn keep_message(&self) -> Option<String> {
        match self {
            StagingVerdict::Reclaimable { .. } => None,
            StagingVerdict::InUse { why } => Some(format!("kept: {}", why)),
            StagingVerdict::Uncollected {
                why,
                recover_command,
                ..
            } => Some(format!(
                "kept: {}. Recover it with: {}",
                why, recover_command
            )),
            StagingVerdict::Unsure {
                why,
                recover_command,
            } => Some(format!(
                "kept: could not confirm it is empty of work ({}). Inspect it with: {}",
                why, recover_command
            )),
        }
    }
}

/// The command that recovers a goal's staging, using the short id users see.
fn recover_command(goal: &GoalRun) -> String {
    format!("ta goal recover {}", short(&goal.goal_run_id.to_string()))
}

/// Decide whether `goal`'s staging directory may be deleted. Reads the staging
/// directory, the source project and the draft packages in `pr_packages_dir`,
/// and changes nothing. `project_root` is the project the goal belongs to; it
/// stands in for the source directory when the goal does not record one.
///
/// Applied, completed, merged and closed goals are reclaimable. A failed goal is
/// reclaimable only when it is certain that nothing in staging is uncollected;
/// every other case keeps the directory.
pub fn assess_staging(
    pr_packages_dir: &Path,
    project_root: &Path,
    goal: &GoalRun,
) -> StagingVerdict {
    match &goal.state {
        GoalRunState::Applied
        | GoalRunState::Completed
        | GoalRunState::Merged
        | GoalRunState::Closed { .. } => StagingVerdict::Reclaimable {
            why: format!("goal is {}", goal.state),
        },
        GoalRunState::Failed { reason } => {
            assess_failed(pr_packages_dir, project_root, goal, reason)
        }
        other => StagingVerdict::InUse {
            why: format!("goal is {}, not a finished goal", other),
        },
    }
}

fn assess_failed(
    pr_packages_dir: &Path,
    project_root: &Path,
    goal: &GoalRun,
    reason: &str,
) -> StagingVerdict {
    let recover = recover_command(goal);

    // A `ta run` that is still alive owns this goal: the watchdog may have marked
    // it failed while the post-agent review or draft build is still going.
    if let Some(owner) = goal.owner_pid {
        if ta_lifecycle::process_is_alive(owner) {
            return StagingVerdict::InUse {
                why: format!(
                    "its owning `ta run` (PID {}) is still alive{}",
                    owner,
                    goal.post_agent_phase
                        .as_deref()
                        .map(|p| format!(" ({})", p))
                        .unwrap_or_default()
                ),
            };
        }
    }
    if let Some(pid) = goal.agent_pid {
        if ta_lifecycle::process_is_alive(pid) {
            return StagingVerdict::InUse {
                why: format!("its agent process (PID {}) is still alive", pid),
            };
        }
    }

    if reason.starts_with(USER_CANCEL_REASON_PREFIX) {
        return StagingVerdict::Reclaimable {
            why: "the user cancelled this goal".to_string(),
        };
    }

    let staging = &goal.workspace_path;
    if staging.as_os_str().is_empty() || !staging.is_dir() {
        return StagingVerdict::Reclaimable {
            why: "no staging directory".to_string(),
        };
    }

    // A draft that is still reviewable (or approved but not applied) is applied
    // from staging, so the directory is still needed.
    match unresolved_draft_for(pr_packages_dir, goal) {
        Ok(Some((draft_id, status))) => {
            return StagingVerdict::Uncollected {
                changed_files: 0,
                recover_command: format!("ta draft apply {}", short(&draft_id)),
                why: format!(
                    "draft {} for this goal is still {} and is applied from this staging",
                    short(&draft_id),
                    status
                ),
            };
        }
        Ok(None) => {}
        Err(e) => {
            return StagingVerdict::Unsure {
                why: format!("could not read draft packages: {}", e),
                recover_command: recover,
            };
        }
    }

    let source_dir = goal
        .source_dir
        .clone()
        .unwrap_or_else(|| project_root.to_path_buf());
    if !source_dir.is_dir() {
        return StagingVerdict::Unsure {
            why: format!("source project {} not found", source_dir.display()),
            recover_command: recover,
        };
    }

    let excludes = ExcludePatterns::load(&source_dir);
    let changed = match changed_files(
        staging,
        &source_dir,
        goal.start_commit.as_deref(),
        &excludes,
    ) {
        Ok(n) => n,
        Err(e) => {
            return StagingVerdict::Unsure {
                why: format!("could not compare staging with source: {}", e),
                recover_command: recover,
            };
        }
    };
    if changed == 0 {
        // A built draft proves collection; an untouched staging holds nothing.
        return StagingVerdict::Reclaimable {
            why: "staging has no changes against the source".to_string(),
        };
    }
    StagingVerdict::Uncollected {
        changed_files: changed,
        recover_command: recover,
        why: format!("staging holds {} changed file(s) not in any draft", changed),
    }
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// A draft for this goal that has not been applied, denied, superseded or closed.
fn unresolved_draft_for(dir: &Path, goal: &GoalRun) -> std::io::Result<Option<(String, String)>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let goal_id = goal.goal_run_id.to_string();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        // An unreadable or foreign file is not evidence either way; the changed
        // file comparison below still has the last word.
        let Ok(json) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<DraftPackage>(&json) else {
            continue;
        };
        if pkg.goal.goal_id != goal_id {
            continue;
        }
        if matches!(
            pkg.status,
            DraftStatus::Draft | DraftStatus::PendingReview | DraftStatus::Approved { .. }
        ) {
            let status = match pkg.status {
                DraftStatus::Approved { .. } => "approved",
                DraftStatus::PendingReview => "pending review",
                _ => "a draft",
            };
            return Ok(Some((pkg.package_id.to_string(), status.to_string())));
        }
    }
    Ok(None)
}

/// Count files in `staging` that differ from the source project, ignoring
/// excluded and infrastructure paths. A file that differs from the source but
/// equals the start commit's version is the source having moved on, not agent
/// work, and is not counted. Deletions are not counted: a deleted file still
/// exists in the source, so nothing is lost.
fn changed_files(
    staging: &Path,
    source: &Path,
    start_commit: Option<&str>,
    excludes: &ExcludePatterns,
) -> std::io::Result<usize> {
    let mut count = 0usize;
    let mut stack: Vec<PathBuf> = vec![staging.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if excludes.should_exclude(&name)
                || crate::path_safety::is_infrastructure_component(&name)
            {
                continue;
            }
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let rel = path.strip_prefix(staging).unwrap_or(&path);
                if differs_from_source(&path, &source.join(rel), rel, source, start_commit)? {
                    count += 1;
                }
            }
        }
    }
    Ok(count)
}

fn differs_from_source(
    staged: &Path,
    source_file: &Path,
    rel: &Path,
    source_root: &Path,
    start_commit: Option<&str>,
) -> std::io::Result<bool> {
    let staged_len = staged.metadata()?.len();
    let same_as_source = match source_file.metadata() {
        Ok(meta) if meta.is_file() => {
            if meta.len() != staged_len {
                false
            } else if staged_len > MAX_COMPARE_BYTES {
                // Cannot confirm cheaply: report as changed so the directory is kept.
                return Ok(true);
            } else {
                std::fs::read(staged)? == std::fs::read(source_file)?
            }
        }
        _ => false,
    };
    if same_as_source {
        return Ok(false);
    }
    // Differs from the working tree. If it still equals the version at the start
    // commit, the source moved on and the agent did not touch it.
    if let Some(commit) = start_commit {
        if let Some(blob) = git_blob_at(source_root, commit, rel) {
            if blob == std::fs::read(staged)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Contents of `rel` at `commit`, or `None` when git cannot say (not a repo, git
/// missing, path absent at that commit). `None` makes the caller count the file
/// as changed, which keeps the staging directory.
fn git_blob_at(source_root: &Path, commit: &str, rel: &Path) -> Option<Vec<u8>> {
    let rel = rel.to_string_lossy().replace('\\', "/");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(source_root)
        .arg("show")
        .arg(format!("{}:{}", commit, rel))
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn assess(root: &Path, goal: &GoalRun) -> StagingVerdict {
        assess_staging(&root.join(".ta/pr_packages"), root, goal)
    }

    fn failed_goal(root: &Path, staging: &Path, source: &Path, reason: &str) -> GoalRun {
        let mut g = GoalRun::new(
            "t",
            "o",
            "claude-code",
            staging.to_path_buf(),
            root.join(".ta/store"),
        );
        g.source_dir = Some(source.to_path_buf());
        g.state = GoalRunState::Failed {
            reason: reason.to_string(),
        };
        g
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn cap_skips_failed_goal_with_uncollected_work() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&src.path().join("a.rs"), "old");
        write(&staging.path().join("a.rs"), "old");
        write(&staging.path().join("new_feature.rs"), "work");
        let goal = failed_goal(root.path(), staging.path(), src.path(), "watchdog");

        let v = assess(root.path(), &goal);
        match &v {
            StagingVerdict::Uncollected {
                changed_files,
                recover_command,
                ..
            } => {
                assert_eq!(*changed_files, 1);
                assert!(recover_command.starts_with("ta goal recover "));
            }
            other => panic!("expected Uncollected, got {:?}", other),
        }
        assert!(!v.is_reclaimable());
        assert!(v.keep_message().unwrap().contains("ta goal recover"));
    }

    #[test]
    fn huge_target_dir_is_not_uncollected_work() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&src.path().join("a.rs"), "same");
        write(&staging.path().join("a.rs"), "same");
        write(&staging.path().join("target/debug/out"), "artifact");
        let goal = failed_goal(root.path(), staging.path(), src.path(), "watchdog");
        assert!(assess(root.path(), &goal).is_reclaimable());
    }

    #[test]
    fn cap_reclaims_applied_and_unchanged_failed_goals() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&src.path().join("a.rs"), "x");
        write(&staging.path().join("a.rs"), "x");

        let mut applied = failed_goal(root.path(), staging.path(), src.path(), "r");
        applied.state = GoalRunState::Applied;
        write(
            &staging.path().join("extra.rs"),
            "ignored for applied goals",
        );
        assert!(assess(root.path(), &applied).is_reclaimable());

        std::fs::remove_file(staging.path().join("extra.rs")).unwrap();
        let failed = failed_goal(root.path(), staging.path(), src.path(), "r");
        assert!(assess(root.path(), &failed).is_reclaimable());
    }

    #[test]
    fn user_cancelled_goal_is_reclaimable_despite_changes() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&staging.path().join("new.rs"), "work");
        let goal = failed_goal(
            root.path(),
            staging.path(),
            src.path(),
            "Cancelled by user via ta goal recover",
        );
        assert!(assess(root.path(), &goal).is_reclaimable());
    }

    #[test]
    fn running_goal_is_in_use() {
        let root = tempdir().unwrap();
        let staging = tempdir().unwrap();
        let mut goal = failed_goal(root.path(), staging.path(), root.path(), "r");
        goal.state = GoalRunState::Running;
        assert!(matches!(
            assess(root.path(), &goal),
            StagingVerdict::InUse { .. }
        ));
    }

    #[test]
    fn failed_goal_with_live_owner_is_in_use() {
        let root = tempdir().unwrap();
        let src = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&staging.path().join("new.rs"), "work");
        let mut goal = failed_goal(root.path(), staging.path(), src.path(), "watchdog");
        goal.owner_pid = Some(std::process::id());
        goal.post_agent_phase = Some("reviewing".to_string());
        let v = assess(root.path(), &goal);
        assert!(matches!(v, StagingVerdict::InUse { .. }), "{:?}", v);
    }

    #[test]
    fn missing_source_is_unsure_and_kept() {
        let root = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&staging.path().join("new.rs"), "work");
        let missing = root.path().join("does-not-exist");
        let goal = failed_goal(root.path(), staging.path(), &missing, "watchdog");
        let v = assess(root.path(), &goal);
        assert!(matches!(v, StagingVerdict::Unsure { .. }), "{:?}", v);
        assert!(!v.is_reclaimable());
    }

    #[test]
    fn nonexistent_staging_dir_is_reclaimable() {
        let root = tempdir().unwrap();
        let gone = root.path().join("gone");
        let goal = failed_goal(root.path(), &gone, root.path(), "watchdog");
        assert!(assess(root.path(), &goal).is_reclaimable());
    }
}
