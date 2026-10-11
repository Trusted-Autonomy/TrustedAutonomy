// version_guard.rs — Daemon version guard (v0.10.10).
//
// Detects when the running daemon is an older (or newer) version than the CLI
// and offers to restart it. Prevents confusion from stale daemons after upgrades.
//
// Used by `ta shell`, `ta run`, and `ta dev` before connecting to the daemon.

use std::io::{self, Write};
use std::path::Path;

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

    // Compare build SHA first (catches rebuilds within the same semver), falling
    // back to the version string when the daemon does not report a build SHA.
    // The comparison lives in ta-lifecycle so the daemon's own self-update check
    // and the VT poller use exactly the same rules.
    let daemon_id =
        ta_lifecycle::BuildIdentity::new(status.version.clone(), Some(&status.build_sha));
    let cli_id = ta_lifecycle::BuildIdentity::new(cli_version, Some(cli_build_sha));
    let comparison = ta_lifecycle::compare_builds(&daemon_id, &cli_id);

    match comparison {
        ta_lifecycle::BuildComparison::Same => return VersionGuardResult::Match,
        ta_lifecycle::BuildComparison::DiffersByVersion => eprintln!(
            "Daemon version mismatch: daemon v{}, CLI v{}",
            status.version, cli_version
        ),
        ta_lifecycle::BuildComparison::DiffersByHash => eprintln!(
            "Daemon build mismatch: daemon {} (v{}), CLI {} (v{})",
            status.build_sha, status.version, cli_build_sha, cli_version
        ),
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
    ta_lifecycle::locate_sibling_binary("ta-daemon").map_err(|e| anyhow::anyhow!(e))
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
