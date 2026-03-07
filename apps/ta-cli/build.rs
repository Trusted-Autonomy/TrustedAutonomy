// build.rs — Embed VCS and build metadata into the ta binary.
//
// Sets these env vars at compile time:
//   TA_GIT_HASH     — short VCS revision (e.g., "abc1234", "r1234"), or "unknown"
//   TA_BUILD_DATE   — build date in YYYY-MM-DD format
//
// Resolution order for revision ID:
//   1. TA_REVISION env var (set by CI or adapter)
//   2. git rev-parse --short HEAD (if in a git repo)
//   3. "unknown"

use std::process::Command;

fn main() {
    // Get the VCS revision. Check for an explicit override first (adapter-agnostic),
    // then fall back to git detection.
    let revision = std::env::var("TA_REVISION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(detect_git_revision);

    // Build date.
    let build_date = chrono_free_date();

    println!("cargo:rustc-env=TA_GIT_HASH={}", revision);
    println!("cargo:rustc-env=TA_BUILD_DATE={}", build_date);

    // Re-run if git HEAD changes (new commits) — only if .git exists.
    if std::path::Path::new("../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
        println!("cargo:rerun-if-changed=../../.git/refs/");
    }

    // Re-run if the override env var changes.
    println!("cargo:rerun-if-env-changed=TA_REVISION");
}

/// Detect git revision, returning "unknown" if not in a git repo.
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

    // Check for uncommitted changes.
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

/// Get current date as YYYY-MM-DD without pulling in chrono.
fn chrono_free_date() -> String {
    // Try Unix `date` first (macOS/Linux), then Windows `powershell`.
    if let Some(date) = Command::new("date")
        .args(["+%Y-%m-%d"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        return date;
    }

    // Windows fallback: use PowerShell.
    if let Some(date) = Command::new("powershell")
        .args(["-NoProfile", "-Command", "Get-Date -Format yyyy-MM-dd"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        return date;
    }

    "unknown".to_string()
}
