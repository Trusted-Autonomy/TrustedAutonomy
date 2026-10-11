//! The `gh` PATH-shadow wrapper must never re-execute itself in an unbounded
//! chain. A wrapper that resolves back to another wrapper (or itself, when
//! the PATH spelling and `current_exe` disagree, as can happen on Windows)
//! once leaked thousands of `gh` processes on CI. These tests drive the real
//! binary: they never reach a real `gh` and spawn at most two processes.

use std::path::{Path, PathBuf};
use std::process::Command;

const SHIM: &str = env!("CARGO_BIN_EXE_gh");
const GH_NAME: &str = if cfg!(windows) { "gh.exe" } else { "gh" };

fn copy_shim_into(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let dest = dir.join(GH_NAME);
    std::fs::copy(SHIM, &dest).unwrap();
    dest
}

#[test]
fn shim_refuses_to_run_when_reentry_marker_is_set() {
    let out = Command::new(SHIM)
        .env("TA_GH_SHIM_ACTIVE", "1")
        .arg("--version")
        .output()
        .expect("run gh shim");
    assert_eq!(out.status.code(), Some(126));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("refusing to run"), "stderr: {stderr}");
}

#[test]
fn chain_of_two_wrappers_ends_at_depth_two() {
    let dir = tempfile::tempdir().unwrap();
    let first = copy_shim_into(&dir.path().join("a"));
    copy_shim_into(&dir.path().join("b"));
    let path = std::env::join_paths([dir.path().join("a"), dir.path().join("b")]).unwrap();

    // `a/gh` excludes its own directory, finds `b/gh` (another wrapper, the
    // only other `gh` on PATH), launches it with the marker set, and `b/gh`
    // refuses. The whole chain is two processes and exits 126.
    let out = Command::new(&first)
        .env("PATH", &path)
        .env_remove("TA_GH_SHIM_ACTIVE")
        .env("TA_PROJECT_ROOT", dir.path())
        .arg("--version")
        .output()
        .expect("run gh shim chain");
    assert_eq!(out.status.code(), Some(126));
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to run"));
}
