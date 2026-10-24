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
use std::path::Path;

use ta_goal::GoalRun;
use ta_mcp_gateway::GatewayConfig;
use ta_workspace::{reclaim_build_output, ExcludePatterns};

pub use ta_workspace::StagingVerdict;

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

/// Decide whether `goal`'s staging directory may be deleted. The decision itself
/// lives in `ta_workspace::assess_staging`, shared with the daemon's GC pass so
/// that no collector can delete staging another one would keep.
pub fn assess_staging(config: &GatewayConfig, goal: &GoalRun) -> StagingVerdict {
    ta_workspace::assess_staging(&config.pr_packages_dir, &config.workspace_root, goal)
}

pub(crate) fn format_bytes(bytes: u64) -> String {
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
    fn format_bytes_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
    }
}
