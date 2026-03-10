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

    // Fetch daemon status.
    let status = rt.block_on(super::shell::fetch_status(client, base_url));

    if status.version.is_empty() || status.version == "?" {
        return VersionGuardResult::Unreachable;
    }

    if status.version == cli_version {
        return VersionGuardResult::Match;
    }

    // Version mismatch detected.
    eprintln!(
        "Daemon version mismatch: daemon v{}, CLI v{}",
        status.version, cli_version
    );

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
    match restart_daemon(client, base_url, project_root, rt) {
        Ok(()) => VersionGuardResult::Restarted,
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

/// Restart the daemon: send shutdown request, wait for exit, spawn new one.
fn restart_daemon(
    client: &reqwest::Client,
    base_url: &str,
    project_root: &Path,
    rt: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    eprintln!("  Shutting down old daemon...");

    // Send graceful shutdown request.
    let shutdown_url = format!("{}/api/shutdown", base_url);
    let _ = rt.block_on(client.post(&shutdown_url).send());

    // Wait for the daemon to actually exit (up to 5 seconds).
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let check_url = format!("{}/api/status", base_url);
        let still_running = rt.block_on(async {
            client
                .get(&check_url)
                .timeout(std::time::Duration::from_secs(1))
                .send()
                .await
                .is_ok()
        });
        if !still_running {
            break;
        }
    }

    // Verify it's actually down.
    let check_url = format!("{}/api/status", base_url);
    let still_running = rt.block_on(async {
        client
            .get(&check_url)
            .timeout(std::time::Duration::from_secs(1))
            .send()
            .await
            .is_ok()
    });
    if still_running {
        return Err(anyhow::anyhow!(
            "Old daemon did not shut down within 5 seconds. \
             Kill it manually: pkill -f 'ta-daemon'"
        ));
    }

    // Find the daemon binary — prefer the one next to our own binary.
    let daemon_bin = find_daemon_binary()?;

    eprintln!("  Starting new daemon: {}", daemon_bin.display());

    // Parse bind/port from the base_url.
    let log_path = project_root.join(".ta").join("daemon.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| anyhow::anyhow!("Cannot open daemon log {}: {}", log_path.display(), e))?;

    let stderr_log = log_file
        .try_clone()
        .map_err(|e| anyhow::anyhow!("Cannot clone log file handle: {}", e))?;

    let child = Command::new(&daemon_bin)
        .arg("--api")
        .arg("--project-root")
        .arg(project_root)
        .stdout(log_file)
        .stderr(stderr_log)
        .spawn()
        .map_err(|e| anyhow::anyhow!("Cannot spawn {}: {}", daemon_bin.display(), e))?;

    eprintln!("  New daemon started (pid {})", child.id());

    // Wait for it to become healthy (up to 10 seconds).
    for i in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let healthy = rt.block_on(async {
            client
                .get(&check_url)
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .is_ok()
        });
        if healthy {
            // Verify the new daemon has the right version.
            let status = rt.block_on(super::shell::fetch_status(client, base_url));
            let cli_version = env!("CARGO_PKG_VERSION");
            if status.version == cli_version {
                eprintln!("  Daemon restarted successfully (v{}).", cli_version);
            } else {
                eprintln!(
                    "  Warning: new daemon is v{}, expected v{} — the installed binary may be outdated.",
                    status.version, cli_version
                );
            }
            return Ok(());
        }
        if i == 9 {
            eprintln!("  Still waiting for new daemon to start...");
        }
    }

    Err(anyhow::anyhow!(
        "New daemon did not become healthy within 10 seconds. \
         Check log: {}",
        log_path.display()
    ))
}

/// Find the `ta-daemon` binary.
///
/// Search order:
///   1. Same directory as the current `ta` binary
///   2. PATH lookup
fn find_daemon_binary() -> anyhow::Result<std::path::PathBuf> {
    // Try sibling of the current executable.
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join("ta-daemon");
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }

    // Try PATH lookup.
    if let Ok(output) = Command::new("which").arg("ta-daemon").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(std::path::PathBuf::from(path));
            }
        }
    }

    Err(anyhow::anyhow!(
        "Cannot find 'ta-daemon' binary. \
         Ensure it is in the same directory as 'ta' or on your PATH."
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
