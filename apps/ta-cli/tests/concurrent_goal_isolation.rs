// concurrent_goal_isolation.rs — v0.17.10.2 item 5: regression tests for the
// concurrent-goal staging isolation bug.
//
// Reproduces (at the primitive level) the scenario from the live incident:
// several goals launched concurrently against the same `source` directory.
// Verifies the fix's three layers hold under real concurrency:
//   1. `SourceStageLock` serializes staging-create so overlays never race.
//   2. Every resulting overlay is a real, goal-scoped `.ta/staging/<goal_id>`
//      directory — `verify_staging_isolation` passes for each.
//   3. Agent-authored `.ta-decisions.json` files stay independent per goal —
//      no last-writer-wins collision in the shared source tree.

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ta_mcp_gateway::GatewayConfig;
use ta_workspace::{verify_staging_isolation, ExcludePatterns, OverlayWorkspace};
use tempfile::TempDir;

// `ta-cli` is a binary-only crate (no `lib.rs`), so integration tests can't
// `use` its internal modules as an external crate. Instead, pull the source
// lock implementation in directly via `include!` — it only depends on
// `libc`/`chrono`/`serde_json`, all already dependencies of this package, so
// this exercises the exact same code the production `ta run` path uses.
#[allow(dead_code)]
mod source_lock_under_test {
    include!("../src/commands/source_lock.rs");
}
use source_lock_under_test::SourceStageLock;

/// Simulates N concurrent `ta_goal_start` calls against the same source
/// directory: each "goal" acquires the source-stage lock, stages an overlay,
/// writes its own `.ta-decisions.json`, then releases the lock — exactly the
/// sequence `run.rs::execute()` performs around overlay creation.
#[test]
fn concurrent_goal_starts_against_same_source_stay_isolated() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("README.md"), "# Shared project\n").unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

    let source_dir = project.path().canonicalize().unwrap();
    let config = GatewayConfig::for_project(&source_dir);
    fs::create_dir_all(&config.staging_dir).unwrap();

    const GOAL_COUNT: usize = 6;
    // Real goal IDs are UUIDs — `verify_staging_isolation` requires the
    // staging directory's final path component to parse as one.
    let goal_ids: Vec<String> = (0..GOAL_COUNT)
        .map(|i| format!("{i:08}-0000-0000-0000-000000000000"))
        .collect();

    // Records max concurrent lock holders, mirroring the source_lock unit
    // test but exercised alongside real overlay creation this time.
    let concurrent_holders = Arc::new(Mutex::new(0usize));
    let max_concurrent = Arc::new(Mutex::new(0usize));

    let staging_dirs: Vec<std::path::PathBuf> = std::thread::scope(|scope| {
        let handles: Vec<_> = goal_ids
            .iter()
            .map(|goal_id| {
                let source_dir = source_dir.clone();
                let staging_root = config.staging_dir.clone();
                let concurrent_holders = concurrent_holders.clone();
                let max_concurrent = max_concurrent.clone();
                scope.spawn(move || {
                    // Step 1: serialize staging-create against the shared source
                    // (item 3).
                    let _lock = SourceStageLock::acquire_blocking(
                        &source_dir,
                        goal_id,
                        Duration::from_secs(10),
                    )
                    .expect("lock acquisition must not fail");

                    {
                        let mut held = concurrent_holders.lock().unwrap();
                        *held += 1;
                        let mut max = max_concurrent.lock().unwrap();
                        *max = (*max).max(*held);
                    }
                    // Give other threads a chance to race in if the lock were
                    // not actually exclusive.
                    std::thread::sleep(Duration::from_millis(15));
                    {
                        let mut held = concurrent_holders.lock().unwrap();
                        *held -= 1;
                    }

                    // Step 2: create the overlay (item 1's fixed launch path
                    // always resolves `source_dir` explicitly, independent of
                    // any process's ambient CWD).
                    let excludes = ExcludePatterns::load(&source_dir);
                    let overlay =
                        OverlayWorkspace::create(goal_id, &source_dir, &staging_root, excludes)
                            .expect("overlay creation must succeed");

                    // Step 3: hard runtime guard (item 2) — every overlay this
                    // path produces must be verifiably isolated before any
                    // agent would be allowed to launch into it.
                    verify_staging_isolation(overlay.staging_dir(), &source_dir, goal_id)
                        .expect("freshly created overlay must pass the isolation guard");

                    // Step 4: simulate the agent writing its decision log —
                    // must land in this goal's own staging dir, never in the
                    // shared source tree (item 4).
                    fs::write(
                        overlay.staging_dir().join(".ta-decisions.json"),
                        format!(r#"[{{"decision":"decision from {goal_id}"}}]"#),
                    )
                    .unwrap();

                    overlay.staging_dir().to_path_buf()
                })
            })
            .collect();

        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // The lock must have been genuinely exclusive.
    assert_eq!(
        *max_concurrent.lock().unwrap(),
        1,
        "source-stage lock must serialize concurrent staging-create calls"
    );

    // 1. Every goal produced a distinct staging directory.
    let unique: std::collections::HashSet<_> = staging_dirs.iter().collect();
    assert_eq!(
        unique.len(),
        GOAL_COUNT,
        "each concurrent goal must get its own staging directory, got: {:?}",
        staging_dirs
    );

    // 2. No cross-visible content: each staging dir's decisions file names
    // only its own goal, never another goal's.
    for (goal_id, staging_dir) in goal_ids.iter().zip(staging_dirs.iter()) {
        let content = fs::read_to_string(staging_dir.join(".ta-decisions.json")).unwrap();
        assert!(
            content.contains(goal_id),
            "staging dir {} must contain its own goal's decision, got: {}",
            staging_dir.display(),
            content
        );
        for other_id in &goal_ids {
            if other_id != goal_id {
                assert!(
                    !content.contains(other_id.as_str()),
                    "staging dir for {} must not contain another goal's decision ({}), got: {}",
                    goal_id,
                    other_id,
                    content
                );
            }
        }
    }

    // 3. The shared source tree itself must never have received a
    // `.ta-decisions.json` — this is the exact last-writer-wins collision
    // reported in the live incident.
    assert!(
        !source_dir.join(".ta-decisions.json").exists(),
        ".ta-decisions.json must never leak into the shared source directory"
    );
}

/// A staging path that has degenerated to equal `source_dir` (the exact
/// failure mode from the live incident) must fail the hard runtime guard
/// fast, with a clear, actionable error — never silently proceed.
#[test]
fn degenerate_staging_path_equal_to_source_fails_fast() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("README.md"), "# Project\n").unwrap();
    let source_dir = project.path().canonicalize().unwrap();

    let result = verify_staging_isolation(&source_dir, &source_dir, "some-goal-id");

    let err = result.expect_err("staging_path == source_dir must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("identical to the source directory"),
        "error must clearly explain the isolation violation for an actionable fix, got: {}",
        msg
    );
}
