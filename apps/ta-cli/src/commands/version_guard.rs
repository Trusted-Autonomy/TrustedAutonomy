// version_guard.rs — Daemon version guard (v0.10.10).
//
// Detects when the running daemon is an older (or newer) version than the CLI
// and offers to restart it. Prevents confusion from stale daemons after upgrades.
//
// Used by `ta shell`, `ta run`, and `ta dev` before connecting to the daemon.

use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

/// Result of a version guard check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionGuardResult {
    /// Versions match — proceed normally.
    Match,
    /// Mismatch detected but user declined restart (or non-interactive).
    /// The status bar should show "(stale)".
    Stale {
        daemon_version: String,
        cli_version: String,
    },
    /// Daemon was restarted with the matching version.
    Restarted,
    /// Daemon is unreachable — caller should handle as usual.
    Unreachable,
}

/// Check the daemon version against the CLI version and optionally restart.
///
/// If `interactive` is true, prompts the user to restart on mismatch.
/// If `interactive` is false (e.g., `--no-version-check`), skips the check entirely.
///
/// Returns `VersionGuardResult` indicating what happened.
pub fn check_daemon_version(
    client: &reqwest::Client,
    base_url: &str,
    project_root: &Path,
    interactive: bool,
    rt: &tokio::runtime::Runtime,
) -> VersionGuardResult {
    let cli_version = env!("CARGO_PKG_VERSION");
    let cli_build_sha = env!("TA_GIT_HASH");

    // Fetch daemon status.
    let status = rt.block_on(super::shell::fetch_status(client, base_url));

    if status.version.is_empty() || status.version == "?" {
        return VersionGuardResult::Unreachable;
    }

    // Compare build SHA first (catches rebuilds within the same semver).
    // Fall back to version string comparison if daemon doesn't report build_sha.
    let sha_match = !status.build_sha.is_empty()
        && status.build_sha != "?"
        && status.build_sha == cli_build_sha;
    let version_match = status.version == cli_version;

    if sha_match || (version_match && (status.build_sha.is_empty() || status.build_sha == "?")) {
        return VersionGuardResult::Match;
    }

    // Mismatch detected — either different version or same version with different build SHA.
    if !version_match {
        eprintln!(
            "Daemon version mismatch: daemon v{}, CLI v{}",
            status.version, cli_version
        );
    } else {
        eprintln!(
            "Daemon build mismatch: daemon {} (v{}), CLI {} (v{})",
            status.build_sha, status.version, cli_build_sha, cli_version
        );
    }

    if !interactive {
        eprintln!("  Proceeding with mismatched daemon (--no-version-check).");
        return VersionGuardResult::Stale {
            daemon_version: status.version,
            cli_version: cli_version.to_string(),
        };
    }

    // Prompt user.
    eprint!("Restart daemon with the new version? [Y/n] ");
    let _ = io::stderr().flush();

    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        // Non-interactive stdin (pipe, etc.) — don't restart.
        eprintln!("  (non-interactive — skipping restart)");
        return VersionGuardResult::Stale {
            daemon_version: status.version,
            cli_version: cli_version.to_string(),
        };
    }

    let answer = answer.trim().to_lowercase();
    if answer == "n" || answer == "no" {
        eprintln!("  Proceeding with stale daemon.");
        return VersionGuardResult::Stale {
            daemon_version: status.version,
            cli_version: cli_version.to_string(),
        };
    }

    // User accepted (default is yes) — restart the daemon.
    match super::daemon::restart(project_root, None) {
        Ok(_pid) => VersionGuardResult::Restarted,
        Err(e) => {
            eprintln!("  Failed to restart daemon: {}", e);
            eprintln!("  Proceeding with stale daemon.");
            VersionGuardResult::Stale {
                daemon_version: status.version,
                cli_version: cli_version.to_string(),
            }
        }
    }
}

/// Find the `ta-daemon` binary.
///
/// Search order:
///   1. Same directory as the current `ta` binary
///   2. PATH lookup
pub fn find_daemon_binary() -> anyhow::Result<std::path::PathBuf> {
    // On Windows the binary is "ta-daemon.exe"; on Unix it is "ta-daemon".
    let bin_name = format!("ta-daemon{}", std::env::consts::EXE_SUFFIX);

    // Try sibling of the current executable first (works for zip/tar installs
    // where both binaries are in the same directory).
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join(&bin_name);
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }

    // Fall back to PATH lookup.  `where` is the Windows equivalent of `which`.
    #[cfg(windows)]
    let which_cmd = "where";
    #[cfg(not(windows))]
    let which_cmd = "which";

    if let Ok(output) = Command::new(which_cmd).arg("ta-daemon").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                return Ok(std::path::PathBuf::from(path));
            }
        }
    }

    Err(anyhow::anyhow!(
        "Cannot find '{}' binary. \
         Ensure it is in the same directory as 'ta' or on your PATH.",
        bin_name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_guard_result_variants() {
        let m = VersionGuardResult::Match;
        assert_eq!(m, VersionGuardResult::Match);

        let s = VersionGuardResult::Stale {
            daemon_version: "0.10.9-alpha".to_string(),
            cli_version: "0.10.10-alpha".to_string(),
        };
        assert!(matches!(s, VersionGuardResult::Stale { .. }));
    }

    #[test]
    fn find_daemon_binary_does_not_panic() {
        // This may succeed or fail depending on the environment,
        // but it should not panic.
        let _ = find_daemon_binary();
    }

    #[test]
    fn stale_result_carries_versions() {
        let result = VersionGuardResult::Stale {
            daemon_version: "0.10.5-alpha".to_string(),
            cli_version: "0.10.10-alpha".to_string(),
        };
        match result {
            VersionGuardResult::Stale {
                daemon_version,
                cli_version,
            } => {
                assert_eq!(daemon_version, "0.10.5-alpha");
                assert_eq!(cli_version, "0.10.10-alpha");
            }
            _ => panic!("expected Stale"),
        }
    }
}
