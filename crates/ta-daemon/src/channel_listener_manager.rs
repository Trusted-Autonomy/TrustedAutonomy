// channel_listener_manager.rs — Daemon-managed Discord listener lifecycle (v0.16.4.4).
//
// When `[channels.discord_listener] enabled = true` in daemon.toml, this
// module auto-starts the `ta-channel-discord --listen` process and keeps it
// running. If the process exits (crash, OOM, etc.), it is restarted after
// `restart_delay_secs` up to `max_restarts` times (0 = unlimited).
//
// The listener process inherits the daemon's environment so
// `TA_DISCORD_TOKEN`, `TA_DISCORD_CHANNEL_ID`, `TA_DAEMON_URL`, etc. are
// picked up automatically from the environment where the daemon runs.
//
// Lifecycle:
//   daemon starts → adopt-orphan check → spawn_listener() (if no orphan) → monitor loop → restart on exit
//   daemon stops → drop ChildGuard → SIGTERM/SIGKILL the listener
//
// Crash-loop detection (v0.16.1.8):
//   When the listener exits with a non-zero code on >= restart_fail_threshold
//   consecutive attempts, the manager writes `.ta/discord-crash-state.json`
//   with the last 10 lines of stderr. `ta doctor` reads this file to diagnose
//   and fix the root cause (stale PID file, missing env var, auth failure, etc.).
//
// Adopt-orphan (v0.16.4.4):
//   On daemon restart, a prior listener process may still be alive (its PID is
//   in `.ta/discord-listener.pid`). Without adoption, the daemon would spawn a
//   new listener, the new process would see the PID file, exit non-zero ("already
//   running"), incrementing `consecutive_failures` endlessly. The adopt-orphan
//   path detects this case before spawning: if the recorded PID is alive, the
//   manager skips the spawn, resets the crash counter, and watches the existing
//   process via liveness polling until it exits naturally.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Notify;

use crate::config::DiscordListenerConfig;

/// Start the Discord listener manager task.
///
/// Returns immediately (the manager runs as a background tokio task).
/// Call this once at daemon startup when discord_listener.enabled = true.
pub fn start(project_root: PathBuf, config: DiscordListenerConfig, shutdown: Arc<Notify>) {
    tokio::spawn(async move {
        run_manager(project_root, config, shutdown).await;
    });
}

async fn run_manager(project_root: PathBuf, config: DiscordListenerConfig, shutdown: Arc<Notify>) {
    let binary = resolve_binary(&project_root, &config.binary);
    let max_restarts = config.max_restarts;
    let restart_fail_threshold = config.restart_fail_threshold;
    let delay = Duration::from_secs(config.restart_delay_secs);

    tracing::info!(
        binary = %binary.display(),
        max_restarts,
        restart_delay_secs = config.restart_delay_secs,
        restart_fail_threshold,
        "Discord listener manager starting"
    );

    let mut restarts: u32 = 0;
    let mut consecutive_failures: u32 = 0;

    loop {
        // --- Adopt-orphan check (v0.16.4.4) ---
        // On daemon restart the listener may already be alive from a prior daemon run.
        // If we find a live PID in `.ta/discord-listener.pid`, adopt it instead of
        // spawning — which would cause the listener binary to exit non-zero and
        // trigger an infinite crash loop.
        if let Some(orphan_pid) = try_adopt_orphan(&project_root) {
            tracing::info!(
                pid = orphan_pid,
                channel = "discord",
                "Existing Discord listener adopted — skipping spawn, resetting crash counter"
            );

            // A live orphan means the previous daemon run exited cleanly.
            // Reset the failure counter so stale crash state does not carry over.
            consecutive_failures = 0;
            let crash_path = project_root.join(".ta/discord-crash-state.json");
            if crash_path.exists() {
                let _ = std::fs::remove_file(&crash_path);
            }

            // Watch the adopted process until it exits or the daemon shuts down.
            tokio::select! {
                _ = wait_adopted(orphan_pid) => {
                    tracing::warn!(
                        pid = orphan_pid,
                        "Adopted Discord listener exited. Restarting in {}s.",
                        delay.as_secs()
                    );
                }
                _ = shutdown.notified() => {
                    // Leave the adopted listener running — we did not start it.
                    tracing::info!(
                        pid = orphan_pid,
                        "Daemon shutting down; leaving adopted Discord listener running"
                    );
                    return;
                }
            }

            restarts = restarts.saturating_add(1);
            if max_restarts > 0 && restarts >= max_restarts {
                tracing::error!(
                    restarts,
                    max_restarts,
                    "Discord listener exceeded max restarts. Giving up. \
                     Fix the listener configuration and restart the daemon."
                );
                return;
            }

            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = shutdown.notified() => {
                    tracing::info!("Discord listener manager shutting down during restart delay");
                    return;
                }
            }
            continue;
        }

        // --- Spawn phase ---
        tracing::info!(
            binary = %binary.display(),
            restarts,
            "Spawning Discord listener"
        );

        let mut child = match spawn_listener(&binary) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    binary = %binary.display(),
                    error = %e,
                    "Failed to spawn Discord listener. \
                     Ensure 'ta-channel-discord' is on PATH or in .ta/plugins/channels/discord/. \
                     Retrying in {}s.",
                    delay.as_secs()
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.notified() => {
                        tracing::info!("Discord listener manager shutting down (spawn failed)");
                        return;
                    }
                }
                restarts = restarts.saturating_add(1);
                if max_restarts > 0 && restarts >= max_restarts {
                    tracing::error!(
                        restarts,
                        max_restarts,
                        "Discord listener exceeded max restarts. Giving up."
                    );
                    return;
                }
                continue;
            }
        };

        let pid = child.id().unwrap_or(0);
        tracing::info!(pid, "Discord listener running");

        // Capture stderr concurrently so the child's pipe buffer never fills up.
        // All captured lines are also forwarded to the daemon's own stderr (inherit behavior).
        let stderr_handle = child.stderr.take().map(|stderr| {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut all_lines: Vec<String> = Vec::new();
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim_end().to_string();
                            eprintln!("{}", trimmed); // forward to parent stderr
                            all_lines.push(trimmed);
                            // Keep the in-memory buffer bounded.
                            if all_lines.len() > 100 {
                                all_lines.remove(0);
                            }
                        }
                        Err(_) => break,
                    }
                }
                all_lines
            })
        });

        // Wait for the child to exit or the daemon to shut down.
        let exit_status = tokio::select! {
            status = wait_child(child) => status,
            _ = shutdown.notified() => {
                tracing::info!(pid, "Daemon shutting down — Discord listener will exit via PID file cleanup");
                // The listener handles its own graceful shutdown via ctrl-c / SIGTERM.
                return;
            }
        };

        // Collect the last stderr lines (child exited → pipe closed → reader hits EOF quickly).
        let last_stderr: Vec<String> = if let Some(handle) = stderr_handle {
            match tokio::time::timeout(Duration::from_secs(2), handle).await {
                Ok(Ok(lines)) => {
                    let total = lines.len();
                    lines.into_iter().skip(total.saturating_sub(10)).collect()
                }
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };

        let is_crash = match &exit_status {
            Ok(Some(code)) => *code != 0,
            Ok(None) => false, // killed by signal (graceful shutdown) — not a crash
            Err(_) => true,
        };

        match &exit_status {
            Ok(status) => {
                tracing::warn!(
                    pid,
                    exit_code = ?status,
                    consecutive_failures = if is_crash { consecutive_failures + 1 } else { 0 },
                    "Discord listener exited. Restarting in {}s.",
                    delay.as_secs()
                );
            }
            Err(e) => {
                tracing::warn!(
                    pid,
                    error = %e,
                    "Discord listener wait error. Restarting in {}s.",
                    delay.as_secs()
                );
            }
        }

        if is_crash {
            consecutive_failures = consecutive_failures.saturating_add(1);
            if restart_fail_threshold > 0 && consecutive_failures >= restart_fail_threshold {
                write_crash_state(&project_root, consecutive_failures, &last_stderr);
            }
        } else {
            // Clean or signal exit — reset failure counter and clear stale crash state.
            if consecutive_failures > 0 {
                let crash_path = project_root.join(".ta/discord-crash-state.json");
                let _ = std::fs::remove_file(&crash_path);
            }
            consecutive_failures = 0;
        }

        restarts = restarts.saturating_add(1);
        if max_restarts > 0 && restarts >= max_restarts {
            tracing::error!(
                restarts,
                max_restarts,
                "Discord listener exceeded max restarts. Giving up. \
                 Fix the listener configuration and restart the daemon."
            );
            return;
        }

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.notified() => {
                tracing::info!("Discord listener manager shutting down during restart delay");
                return;
            }
        }
    }
}

/// Write crash state to `.ta/discord-crash-state.json` so `ta doctor` can diagnose the loop.
fn write_crash_state(project_root: &Path, consecutive_failures: u32, last_stderr: &[String]) {
    let ta_dir = project_root.join(".ta");
    let crash_path = ta_dir.join("discord-crash-state.json");
    let state = serde_json::json!({
        "plugin": "ta-channel-discord",
        "consecutive_failures": consecutive_failures,
        "last_stderr": last_stderr,
        "pid_path": ".ta/discord-listener.pid",
        "updated_at": chrono::Utc::now().to_rfc3339()
    });
    match serde_json::to_string_pretty(&state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&crash_path, &json) {
                tracing::warn!(
                    error = %e,
                    path = %crash_path.display(),
                    "Failed to write discord crash state"
                );
            } else {
                tracing::warn!(
                    consecutive_failures,
                    path = %crash_path.display(),
                    "Discord listener crash loop — wrote crash state for ta doctor"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to serialize discord crash state");
        }
    }
}

/// Spawn the Discord listener process, returning a tokio Child handle with stderr piped.
fn spawn_listener(binary: &Path) -> std::io::Result<Child> {
    tokio::process::Command::new(binary)
        .arg("--listen")
        // Inherit the daemon's environment (TA_DISCORD_TOKEN etc. flow through).
        .env_clear()
        .envs(std::env::vars())
        // Detach stdout/stdin; pipe stderr so we can capture crash output.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true) // Drop = SIGKILL on Unix
        .spawn()
}

/// Wait for a child process to exit, returning its exit status.
async fn wait_child(mut child: Child) -> std::io::Result<Option<i32>> {
    let status = child.wait().await?;
    Ok(status.code())
}

/// Check whether `{project_root}/.ta/discord-listener.pid` contains a live PID.
///
/// Returns `Some(pid)` if the PID file exists and the process is alive.
/// If the file exists but the PID is dead (stale), removes the file and returns `None`.
/// Returns `None` if the file does not exist or cannot be parsed.
fn try_adopt_orphan(project_root: &Path) -> Option<u32> {
    let pid_path = project_root.join(".ta").join("discord-listener.pid");
    let contents = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = contents.trim().parse().ok()?;
    if is_pid_alive(pid) {
        Some(pid)
    } else {
        tracing::info!(
            pid,
            path = %pid_path.display(),
            "Removing stale Discord listener PID file (process is dead)"
        );
        let _ = std::fs::remove_file(&pid_path);
        None
    }
}

/// Return true if a process with `pid` is currently alive.
///
/// On Unix, probes with `kill(pid, 0)` via libc — no signal is sent; the call
/// only checks that the process exists and we have permission to signal it.
/// On Windows, uses `tasklist /FI "PID eq <pid>"`.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) is a standard POSIX probe — it sends no signal.
        // Returns 0 if the process exists and we have permission (alive).
        // Returns -1/ESRCH if no such process (dead).
        // Returns -1/EPERM if process exists but we can't signal it (still alive).
        let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if ret == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string()) && !stdout.contains("No tasks")
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

/// Poll until the adopted listener process is no longer alive.
///
/// Since the adopted process is not our child we cannot use `waitpid`.
/// Instead we probe liveness with `kill(pid, 0)` on a 1-second interval.
async fn wait_adopted(pid: u32) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if !is_pid_alive(pid) {
            return;
        }
    }
}

/// Resolve the binary path.
///
/// Priority:
/// 1. Absolute path as-is.
/// 2. `.ta/plugins/channels/<name>/<name>` (project-local installed plugin).
/// 3. Name on PATH (let the OS find it).
fn resolve_binary(project_root: &Path, name: &str) -> PathBuf {
    // If it looks like an absolute path, use it directly.
    let p = Path::new(name);
    if p.is_absolute() {
        return p.to_path_buf();
    }

    // Check project-local plugin installation.
    // Strip the "ta-channel-" prefix if present to get the plugin name.
    let plugin_name = name.strip_prefix("ta-channel-").unwrap_or(name);
    let local = project_root
        .join(".ta")
        .join("plugins")
        .join("channels")
        .join(plugin_name)
        .join(name);
    if local.exists() {
        return local;
    }

    // Fall back to PATH lookup.
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolve_binary_absolute() {
        let root = PathBuf::from("/tmp");
        let result = resolve_binary(&root, "/usr/local/bin/ta-channel-discord");
        assert_eq!(result, PathBuf::from("/usr/local/bin/ta-channel-discord"));
    }

    #[test]
    fn resolve_binary_path_fallback() {
        let root = PathBuf::from("/tmp/nonexistent_project");
        // No .ta/plugins directory exists, so falls back to PATH name.
        let result = resolve_binary(&root, "ta-channel-discord");
        assert_eq!(result, PathBuf::from("ta-channel-discord"));
    }

    #[test]
    fn discord_listener_config_default() {
        let config = crate::config::DiscordListenerConfig::default();
        assert!(!config.enabled); // opt-in
        assert_eq!(config.binary, "ta-channel-discord");
        assert_eq!(config.restart_delay_secs, 10);
        assert_eq!(config.max_restarts, 0); // unlimited
        assert_eq!(config.restart_fail_threshold, 5);
    }

    #[test]
    fn crash_loop_stderr_captured_in_signal() {
        // Verify write_crash_state creates the expected JSON file.
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        let last_stderr = vec![
            "Another Discord listener is already running (PID 10435). Stop it first.".to_string(),
        ];

        write_crash_state(dir.path(), 7, &last_stderr);

        let crash_path = ta_dir.join("discord-crash-state.json");
        assert!(crash_path.exists(), "crash state file should be created");

        let content = std::fs::read_to_string(&crash_path).unwrap();
        let state: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(state["plugin"].as_str().unwrap(), "ta-channel-discord");
        assert_eq!(state["consecutive_failures"].as_u64().unwrap(), 7);
        let captured: Vec<String> = state["last_stderr"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        assert_eq!(captured, last_stderr);
        assert!(state["updated_at"].as_str().is_some());
    }

    #[test]
    fn write_crash_state_no_ta_dir_does_not_panic() {
        // If .ta/ doesn't exist, write_crash_state should log and return without panicking.
        let dir = tempfile::tempdir().unwrap();
        // Do NOT create .ta/ — should fail gracefully.
        write_crash_state(dir.path(), 3, &["some error".to_string()]);
        // No assertion needed; just ensure no panic.
    }

    // --- Adopt-orphan tests (v0.16.4.4) ---

    #[test]
    fn is_pid_alive_self() {
        // The current process is definitely alive.
        assert!(
            is_pid_alive(std::process::id()),
            "current process should report as alive"
        );
    }

    #[test]
    fn is_pid_alive_dead() {
        // PID 999_999_999 is far above any OS's valid PID range and is reliably dead.
        assert!(
            !is_pid_alive(999_999_999),
            "nonexistent PID should report as dead"
        );
    }

    #[test]
    fn try_adopt_orphan_no_pid_file() {
        // No PID file → returns None immediately.
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        assert!(
            try_adopt_orphan(dir.path()).is_none(),
            "no PID file should return None"
        );
    }

    #[test]
    fn try_adopt_orphan_alive_pid_returns_some() {
        // Write our own PID (definitely alive) — should return Some(pid).
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let pid = std::process::id();
        std::fs::write(ta_dir.join("discord-listener.pid"), pid.to_string()).unwrap();

        let result = try_adopt_orphan(dir.path());
        assert_eq!(result, Some(pid), "alive PID should be adopted");
        // PID file should still exist (we did not remove it).
        assert!(
            ta_dir.join("discord-listener.pid").exists(),
            "PID file should remain after adoption"
        );
    }

    #[test]
    fn try_adopt_orphan_dead_pid_removes_file() {
        // Write a definitely-dead PID — should return None and delete the stale file.
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let pid_path = ta_dir.join("discord-listener.pid");
        std::fs::write(&pid_path, "999999999").unwrap();
        assert!(pid_path.exists(), "PID file should exist before the call");

        let result = try_adopt_orphan(dir.path());
        assert!(result.is_none(), "dead PID should not be adopted");
        assert!(
            !pid_path.exists(),
            "stale PID file should be removed after dead-PID check"
        );
    }

    #[test]
    fn try_adopt_orphan_malformed_pid_file() {
        // A PID file containing non-numeric text should return None without panicking.
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("discord-listener.pid"), "not-a-pid\n").unwrap();

        let result = try_adopt_orphan(dir.path());
        assert!(result.is_none(), "malformed PID file should return None");
    }
}
