// build.rs — Embed VCS revision into the ta-daemon binary.
//
// Sets TA_GIT_HASH at compile time so the daemon can report its build SHA
// for version guard checks. Uses the same detection logic as ta-cli/build.rs:
//   1. TA_REVISION env var (set by CI or VCS adapter)
//   2. git rev-parse --short HEAD (if in a git repo)
//   3. "unknown"

use std::process::Command;

fn main() {
    let revision = std::env::var("TA_REVISION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(detect_git_revision);

    println!("cargo:rustc-env=TA_GIT_HASH={}", revision);

    // Re-run if git HEAD changes (new commits).
    if std::path::Path::new("../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
        println!("cargo:rerun-if-changed=../../.git/refs/");
    }
    println!("cargo:rerun-if-env-changed=TA_REVISION");
}

fn detect_git_revision() -> String {
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let git_hash = match git_hash {
        Some(h) => h,
        None => return "unknown".to_string(),
    };

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    if dirty {
        format!("{}-dirty", git_hash)
    } else {
        git_hash
    }
}
