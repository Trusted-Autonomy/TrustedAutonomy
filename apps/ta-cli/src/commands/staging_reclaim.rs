// staging_reclaim.rs — Disk reclaim for goal staging, without losing work.
//
// Two jobs, both about the disk a goal's staging copy uses:
//
// 1. `reclaim_after_agent`: when the agent exits, remove the build-output
//    directories (`target/`, `node_modules/`, ...) its builds created inside the
//    staging copy. They are excluded from the copy and from every diff, so they
//    are never reviewable work. A fresh staging copy is tens of MB; the agent's
//    own `cargo build` is what grows it to GBs.
//
// 2. `assess_staging`: decide whether a goal's staging directory may be
//    deleted by the garbage collectors. Fails closed: when it is not certain
//    that the staging directory holds no uncollected work, it is kept and the
//    caller is told the exact command that recovers it.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use ta_changeset::draft_package::{DraftPackage, DraftStatus};
use ta_goal::{GoalRun, GoalRunState};
use ta_mcp_gateway::GatewayConfig;
use ta_workspace::{reclaim_build_output, ExcludePatterns};

/// Largest file compared byte for byte when looking for uncollected work. A
/// larger file that has the same size as its source twin is treated as changed
/// (keep the staging directory) rather than read into memory.
const MAX_COMPARE_BYTES: u64 = 64 * 1024 * 1024;

/// Reason prefix `ta goal recover` writes when the user cancels a goal. The
/// user decided to discard that work, so its staging is reclaimable.
const USER_CANCEL_REASON_PREFIX: &str = "Cancelled by user";

thread_local! {
    static KEEP_BUILD_OUTPUT: Cell<bool> = const { Cell::new(false) };
}

/// Scoped carrier for `ta run --keep-build-output`. `main.rs` holds one for the
/// duration of its `run::execute(...)` call, which keeps the flag out of that
/// function's long, widely-shared parameter list. Thread-local and restored on
/// drop, like `CliChatModeGuard`.
pub struct KeepBuildOutputGuard {
    previous: bool,
}

impl KeepBuildOutputGuard {
    pub fn set(keep: bool) -> Self {
        let previous = KEEP_BUILD_OUTPUT.with(|c| c.replace(keep));
        Self { previous }
    }
}

impl Drop for KeepBuildOutputGuard {
    fn drop(&mut self) {
        KEEP_BUILD_OUTPUT.with(|c| c.set(self.previous));
    }
}

/// Whether `--keep-build-output` was passed for the `execute` call on this thread.
pub(crate) fn keep_build_output_requested() -> bool {
    KEEP_BUILD_OUTPUT.with(|c| c.get())
}

/// What `reclaim_after_agent` did, for callers that want to report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReclaimOutcome {
    /// `[staging] reclaim_build_output = false` in workflow.toml.
    DisabledByConfig,
    /// `--keep-build-output` was passed.
    KeptByFlag,
    /// Reclaim ran (possibly finding nothing).
    Reclaimed {
        dirs_removed: usize,
        bytes_freed: u64,
    },
}

/// Remove build output from a goal's staging copy. Call after the agent exits
/// and after any step that needs the build (verification gates), before the
/// draft is built. Every decision is logged with the goal id and what to do.
pub fn reclaim_after_agent(
    config: &GatewayConfig,
    goal_id: uuid::Uuid,
    staging_path: &Path,
    source_dir: &Path,
) -> ReclaimOutcome {
    let wf = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    if !wf.staging.reclaim_build_output {
        tracing::info!(
            goal_id = %goal_id,
            staging = %staging_path.display(),
            decision = "keep_build_output",
            reason = "[staging] reclaim_build_output = false",
            "Build output left in staging (set reclaim_build_output = true in .ta/workflow.toml to reclaim it)"
        );
        return ReclaimOutcome::DisabledByConfig;
    }
    if keep_build_output_requested() {
        tracing::info!(
            goal_id = %goal_id,
            staging = %staging_path.display(),
            decision = "keep_build_output",
            reason = "--keep-build-output",
            "Build output left in staging for debugging (run `ta gc` later to free it)"
        );
        println!(
            "Keeping build output in staging (--keep-build-output): {}",
            staging_path.display()
        );
        return ReclaimOutcome::KeptByFlag;
    }

    let excludes = ExcludePatterns::load(source_dir);
    let stats = reclaim_build_output(staging_path, &excludes);

    for (dir, error) in &stats.failures {
        tracing::warn!(
            goal_id = %goal_id,
            dir = %dir,
            error = %error,
            "Could not remove build output directory from staging; remove it by hand or run `ta gc`"
        );
    }
    tracing::info!(
        goal_id = %goal_id,
        staging = %staging_path.display(),
        dirs_removed = stats.dirs_removed.len(),
        bytes_freed = stats.bytes_freed,
        failures = stats.failures.len(),
        decision = "reclaim_build_output",
        "Reclaimed build output from staging"
    );
    if !stats.dirs_removed.is_empty() {
        println!(
            "Reclaimed {} of build output ({} director{}) from staging for goal {}.",
            format_bytes(stats.bytes_freed),
            stats.dirs_removed.len(),
            if stats.dirs_removed.len() == 1 {
                "y"
            } else {
                "ies"
            },
            &goal_id.to_string()[..8],
        );
    }
    ReclaimOutcome::Reclaimed {
        dirs_removed: stats.dirs_removed.len(),
        bytes_freed: stats.bytes_freed,
    }
}

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
    #[cfg(test)]
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
    format!("ta goal recover {}", &goal.goal_run_id.to_string()[..8])
}

/// Decide whether `goal`'s staging directory may be deleted. Pure with respect
/// to the goal: reads the staging directory, the source project and the draft
/// packages, and changes nothing.
///
/// Applied, completed, merged and closed goals are reclaimable as before. A
/// failed goal is reclaimable only when it is certain that nothing in staging is
/// uncollected; every other case keeps the directory.
pub fn assess_staging(config: &GatewayConfig, goal: &GoalRun) -> StagingVerdict {
    match &goal.state {
        GoalRunState::Applied
        | GoalRunState::Completed
        | GoalRunState::Merged
        | GoalRunState::Closed { .. } => StagingVerdict::Reclaimable {
            why: format!("goal is {}", goal.state),
        },
        GoalRunState::Failed { reason } => assess_failed(config, goal, reason),
        other => StagingVerdict::InUse {
            why: format!("goal is {}, not a finished goal", other),
        },
    }
}

fn assess_failed(config: &GatewayConfig, goal: &GoalRun, reason: &str) -> StagingVerdict {
    let recover = recover_command(goal);

    // A `ta run` that is still alive owns this goal: the watchdog may have marked
    // it failed while the post-agent review or draft build is still going.
    if let Some(owner) = goal.owner_pid {
        if super::daemon::is_process_alive(owner) {
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
        if super::daemon::is_process_alive(pid) {
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
    match unresolved_draft_for(config, goal) {
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
        .unwrap_or_else(|| config.workspace_root.clone());
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
fn unresolved_draft_for(
    config: &GatewayConfig,
    goal: &GoalRun,
) -> std::io::Result<Option<(String, String)>> {
    let dir = &config.pr_packages_dir;
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
                || ta_workspace::path_safety::is_infrastructure_component(&name)
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

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn gateway(root: &Path) -> GatewayConfig {
        GatewayConfig::for_project(root)
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
    fn reclaim_removes_only_build_dirs_and_never_source_files() {
        let src = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&src.path().join(".taignore"), "scratch/\n");
        write(&staging.path().join("src/lib.rs"), "fn a() {}");
        write(&staging.path().join("Cargo.toml"), "[package]");
        write(&staging.path().join("target/debug/big.bin"), "0123456789");
        write(&staging.path().join("crates/x/node_modules/p/i.js"), "x");
        write(&staging.path().join("scratch/tmp.txt"), "tmp");
        write(&staging.path().join(".ta/state.json"), "{}");
        write(
            &staging.path().join("src/target.rs"),
            "not a dir named target",
        );

        let ex = ExcludePatterns::load(src.path());
        let stats = reclaim_build_output(staging.path(), &ex);

        assert!(!staging.path().join("target").exists());
        assert!(!staging.path().join("crates/x/node_modules").exists());
        assert!(!staging.path().join("scratch").exists());
        assert!(staging.path().join("src/lib.rs").exists());
        assert!(staging.path().join("Cargo.toml").exists());
        assert!(staging.path().join("src/target.rs").exists());
        assert!(staging.path().join(".ta/state.json").exists());
        assert_eq!(stats.dirs_removed.len(), 3);
        assert_eq!(stats.bytes_freed, 10 + 1 + 3);
        assert!(stats.failures.is_empty());
    }

    #[test]
    fn reclaim_leaves_files_matching_extension_patterns() {
        let staging = tempdir().unwrap();
        write(&staging.path().join("a.pyc"), "bytecode");
        let stats = reclaim_build_output(staging.path(), &ExcludePatterns::defaults());
        assert!(staging.path().join("a.pyc").exists());
        assert_eq!(stats, ta_workspace::ReclaimStats::default());
    }

    #[cfg(unix)]
    #[test]
    fn reclaim_does_not_follow_or_remove_symlinks() {
        let elsewhere = tempdir().unwrap();
        write(&elsewhere.path().join("keep.txt"), "precious");
        let staging = tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), staging.path().join("target")).unwrap();
        let stats = reclaim_build_output(staging.path(), &ExcludePatterns::defaults());
        assert!(elsewhere.path().join("keep.txt").exists());
        assert!(stats.dirs_removed.is_empty());
    }

    #[test]
    fn reclaim_after_agent_honors_config_and_flag() {
        let root = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&staging.path().join("target/a"), "12345");
        let cfg = gateway(root.path());
        let id = uuid::Uuid::new_v4();

        {
            let _g = KeepBuildOutputGuard::set(true);
            assert_eq!(
                reclaim_after_agent(&cfg, id, staging.path(), root.path()),
                ReclaimOutcome::KeptByFlag
            );
            assert!(staging.path().join("target/a").exists());
        }

        write(
            &root.path().join(".ta/workflow.toml"),
            "[staging]\nreclaim_build_output = false\n",
        );
        assert_eq!(
            reclaim_after_agent(&cfg, id, staging.path(), root.path()),
            ReclaimOutcome::DisabledByConfig
        );
        assert!(staging.path().join("target/a").exists());

        std::fs::remove_file(root.path().join(".ta/workflow.toml")).unwrap();
        assert_eq!(
            reclaim_after_agent(&cfg, id, staging.path(), root.path()),
            ReclaimOutcome::Reclaimed {
                dirs_removed: 1,
                bytes_freed: 5
            }
        );
        assert!(!staging.path().join("target").exists());
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

        let v = assess_staging(&gateway(root.path()), &goal);
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
        assert!(assess_staging(&gateway(root.path()), &goal).is_reclaimable());
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
        assert!(assess_staging(&gateway(root.path()), &applied).is_reclaimable());

        std::fs::remove_file(staging.path().join("extra.rs")).unwrap();
        let failed = failed_goal(root.path(), staging.path(), src.path(), "r");
        assert!(assess_staging(&gateway(root.path()), &failed).is_reclaimable());
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
        assert!(assess_staging(&gateway(root.path()), &goal).is_reclaimable());
    }

    #[test]
    fn running_goal_is_in_use() {
        let root = tempdir().unwrap();
        let staging = tempdir().unwrap();
        let mut goal = failed_goal(root.path(), staging.path(), root.path(), "r");
        goal.state = GoalRunState::Running;
        assert!(matches!(
            assess_staging(&gateway(root.path()), &goal),
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
        let v = assess_staging(&gateway(root.path()), &goal);
        assert!(matches!(v, StagingVerdict::InUse { .. }), "{:?}", v);
    }

    #[test]
    fn missing_source_is_unsure_and_kept() {
        let root = tempdir().unwrap();
        let staging = tempdir().unwrap();
        write(&staging.path().join("new.rs"), "work");
        let missing = root.path().join("does-not-exist");
        let goal = failed_goal(root.path(), staging.path(), &missing, "watchdog");
        let v = assess_staging(&gateway(root.path()), &goal);
        assert!(matches!(v, StagingVerdict::Unsure { .. }), "{:?}", v);
        assert!(!v.is_reclaimable());
    }

    #[test]
    fn nonexistent_staging_dir_is_reclaimable() {
        let root = tempdir().unwrap();
        let gone = root.path().join("gone");
        let goal = failed_goal(root.path(), &gone, root.path(), "watchdog");
        assert!(assess_staging(&gateway(root.path()), &goal).is_reclaimable());
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
    }
}
