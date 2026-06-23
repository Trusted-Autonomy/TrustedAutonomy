// headroom_supervisor.rs — Supervisor for the headroom context compression proxy (v0.17.0.7).
//
// Manages a single `headroom proxy --port <port>` subprocess.  Health is
// verified via `GET http://127.0.0.1:<port>/health` every 10 seconds.
// On failure the process is restarted with exponential backoff
// (1 s → 2 s → … → 60 s cap).  After 5 failures within 5 minutes the
// supervisor suspends until `ta compression enable` writes a restart signal.
//
// ## Binary detection order
//   1. `headroom` on $PATH (via which::which)
//   2. ~/.local/bin/headroom
//   3. ~/.venv/bin/headroom
//
// If the binary is not found and `compression.enabled = true`, the supervisor
// prints an actionable install message and returns without starting — the
// daemon continues to run normally (never hard-fail).
//
// ## Status file
//   .ta/compression/status.json — readable by `ta compression status`

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::config::CompressionConfig;

const HEALTH_POLL_INTERVAL_SECS: u64 = 10;
const HEALTH_CHECK_TIMEOUT_SECS: u64 = 5;
const STARTUP_GRACE_SECS: u64 = 5;
const SUSPEND_FAILURE_COUNT: u32 = 5;
const SUSPEND_WINDOW_SECS: u64 = 300; // 5 minutes
const MAX_BACKOFF_SECS: u64 = 60;
const RESTART_SIGNAL_POLL_SECS: u64 = 5;

// ─── Public surface ───────────────────────────────────────────────────────────

/// Start the headroom supervisor if compression is enabled.
///
/// Returns immediately; the supervisor loop runs in a background task.
/// If headroom is not found or compression is disabled, this is a no-op.
pub fn start(project_root: PathBuf, config: CompressionConfig, shutdown: Arc<Notify>) {
    if !config.enabled {
        tracing::debug!("HeadroomSupervisor: compression disabled — not starting");
        return;
    }

    // headroom_learn must always be false in TA-managed runs.
    if config.headroom_learn {
        tracing::warn!(
            "compression.headroom_learn is always disabled in TA-managed runs \
             to protect CLAUDE.md — ignoring config value"
        );
    }

    let binary = match find_headroom_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "Warning: context compression is enabled but the `headroom` binary \
                 was not found in PATH, ~/.local/bin, or ~/.venv/bin.\n\
                 \n  Install headroom to enable 60–95% context savings:\n\
                 \n      pip install headroom-ai[all]\n\
                 \n  Or disable compression in .ta/daemon.toml:\n\
                 \n      ta compression disable\n"
            );
            tracing::warn!(
                port = config.port,
                "compression.enabled=true but headroom binary not found — \
                 running without context compression"
            );
            return;
        }
    };

    let port = config.port;
    let status_dir = project_root.join(".ta").join("compression");

    tracing::info!(
        binary = %binary.display(),
        port,
        "HeadroomSupervisor: starting context compression proxy"
    );

    tokio::spawn(async move {
        run_supervisor(binary, port, status_dir, shutdown).await;
    });
}

/// Locate the headroom binary.
///
/// Search order: $PATH → ~/.local/bin → ~/.venv/bin.
/// Returns `None` if not found in any location.
pub fn find_headroom_binary() -> Option<PathBuf> {
    // 1. PATH
    if let Ok(p) = which::which("headroom") {
        return Some(p);
    }
    // 2. ~/.local/bin  and  3. ~/.venv/bin
    if let Some(home) = home_dir() {
        let local = home.join(".local").join("bin").join("headroom");
        if local.exists() {
            return Some(local);
        }
        let venv = home.join(".venv").join("bin").join("headroom");
        if venv.exists() {
            return Some(venv);
        }
    }
    None
}

/// Read the current supervisor status from disk.
///
/// Returns `None` if the daemon is not running or has not yet written a status file.
pub fn read_status(project_root: &Path) -> Option<HeadroomStatus> {
    let path = project_root
        .join(".ta")
        .join("compression")
        .join("status.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write a restart-signal file so the supervisor clears Suspended state.
///
/// Called by `ta compression enable` when the daemon is already running.
pub fn signal_restart(project_root: &Path) -> std::io::Result<()> {
    let dir = project_root.join(".ta").join("compression");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("restart-signal"), "restart")
}

// ─── Status record ────────────────────────────────────────────────────────────

/// Written to `.ta/compression/status.json`; read by `ta compression status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadroomStatus {
    /// "running" | "stopped" | "suspended" | "starting"
    pub status: String,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub updated_at: String,
}

// ─── Supervisor loop ──────────────────────────────────────────────────────────

async fn run_supervisor(binary: PathBuf, port: u16, status_dir: PathBuf, shutdown: Arc<Notify>) {
    std::fs::create_dir_all(&status_dir).ok();
    write_status(&status_dir, "starting", None, 0);

    let mut restart_count: u32 = 0;
    let mut suspended = false;
    let mut recent_failure_times: Vec<Instant> = Vec::new();

    loop {
        // ── Suspended: poll for a restart signal ─────────────────────────
        if suspended {
            let signal_path = status_dir.join("restart-signal");
            if signal_path.exists() {
                std::fs::remove_file(&signal_path).ok();
                suspended = false;
                recent_failure_times.clear();
                tracing::info!(
                    "HeadroomSupervisor: restart signal received — clearing suspended state"
                );
                write_status(&status_dir, "starting", None, restart_count);
            } else {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(RESTART_SIGNAL_POLL_SECS)) => {}
                    _ = shutdown.notified() => {
                        write_status(&status_dir, "stopped", None, restart_count);
                        return;
                    }
                }
                continue;
            }
        }

        // ── Spawn ─────────────────────────────────────────────────────────
        write_status(&status_dir, "starting", None, restart_count);

        let mut cmd = tokio::process::Command::new(&binary);
        cmd.args(["proxy", "--port", &port.to_string()])
            // Belt-and-suspenders: never let headroom modify CLAUDE.md.
            .env("HEADROOM_LEARN", "false")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    binary = %binary.display(),
                    error = %e,
                    "HeadroomSupervisor: failed to spawn headroom proxy"
                );
                write_status(&status_dir, "stopped", None, restart_count);
                handle_failure(
                    &status_dir,
                    &mut restart_count,
                    &mut suspended,
                    &mut recent_failure_times,
                    &shutdown,
                )
                .await;
                continue;
            }
        };

        let pid = child.id().unwrap_or(0);
        tracing::info!(port, pid, "HeadroomSupervisor: headroom proxy running");
        write_status(&status_dir, "running", Some(pid), restart_count);

        // Startup grace: give headroom time to bind the port before health-checking.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(STARTUP_GRACE_SECS)) => {}
            _ = shutdown.notified() => {
                let _ = child.kill().await;
                write_status(&status_dir, "stopped", None, restart_count);
                return;
            }
        }

        // ── Monitor ───────────────────────────────────────────────────────
        let health_url = format!("http://127.0.0.1:{}/health", port);
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(HEALTH_CHECK_TIMEOUT_SECS))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "HeadroomSupervisor: failed to build HTTP client; skipping health checks");
                reqwest::Client::new()
            }
        };

        let exit_reason = monitor(&mut child, &client, &health_url, &shutdown).await;
        let _ = child.kill().await;
        write_status(&status_dir, "stopped", None, restart_count);

        match exit_reason {
            ExitReason::Shutdown => {
                tracing::info!("HeadroomSupervisor: stopped (daemon shutdown)");
                return;
            }
            ExitReason::Clean => {
                tracing::info!("HeadroomSupervisor: headroom proxy exited cleanly");
            }
            ExitReason::Crash(code) => {
                tracing::warn!(
                    code = ?code,
                    "HeadroomSupervisor: headroom proxy crashed"
                );
            }
            ExitReason::HealthFailed => {
                tracing::warn!("HeadroomSupervisor: health check failed — restarting");
            }
        }

        handle_failure(
            &status_dir,
            &mut restart_count,
            &mut suspended,
            &mut recent_failure_times,
            &shutdown,
        )
        .await;
    }
}

// ─── Exit reason ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum ExitReason {
    Shutdown,
    Clean,
    Crash(Option<i32>),
    HealthFailed,
}

// ─── Monitor loop ─────────────────────────────────────────────────────────────

async fn monitor(
    child: &mut tokio::process::Child,
    client: &reqwest::Client,
    health_url: &str,
    shutdown: &Arc<Notify>,
) -> ExitReason {
    let poll = Duration::from_secs(HEALTH_POLL_INTERVAL_SECS);
    loop {
        tokio::select! {
            status = child.wait() => {
                let code = status.ok().and_then(|s| s.code());
                return if code == Some(0) {
                    ExitReason::Clean
                } else {
                    ExitReason::Crash(code)
                };
            }
            _ = tokio::time::sleep(poll) => {
                match client.get(health_url).send().await {
                    Ok(r) if r.status().is_success() => {
                        // healthy — continue the loop
                    }
                    Ok(r) => {
                        tracing::warn!(
                            url = health_url,
                            status = %r.status(),
                            "HeadroomSupervisor: health check returned non-success — restarting"
                        );
                        return ExitReason::HealthFailed;
                    }
                    Err(e) => {
                        tracing::warn!(
                            url = health_url,
                            error = %e,
                            "HeadroomSupervisor: health check request failed — restarting"
                        );
                        return ExitReason::HealthFailed;
                    }
                }
            }
            _ = shutdown.notified() => return ExitReason::Shutdown,
        }
    }
}

// ─── Failure + backoff ────────────────────────────────────────────────────────

async fn handle_failure(
    status_dir: &Path,
    restart_count: &mut u32,
    suspended: &mut bool,
    recent_failure_times: &mut Vec<Instant>,
    shutdown: &Arc<Notify>,
) {
    *restart_count = restart_count.saturating_add(1);

    let now = Instant::now();
    recent_failure_times.push(now);
    let window = Duration::from_secs(SUSPEND_WINDOW_SECS);
    recent_failure_times.retain(|t| now.duration_since(*t) < window);

    if recent_failure_times.len() as u32 >= SUSPEND_FAILURE_COUNT {
        tracing::error!(
            failures = recent_failure_times.len(),
            "HeadroomSupervisor: suspended after {} failures in {}s \
             — run `ta compression enable` to resume",
            SUSPEND_FAILURE_COUNT,
            SUSPEND_WINDOW_SECS,
        );
        *suspended = true;
        write_status(status_dir, "suspended", None, *restart_count);
        return;
    }

    let backoff_secs = MAX_BACKOFF_SECS
        .min(2u64.saturating_pow(*restart_count))
        .max(1);

    tracing::info!(
        restart_count = *restart_count,
        backoff_secs,
        "HeadroomSupervisor: will restart in {}s",
        backoff_secs,
    );

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
        _ = shutdown.notified() => {
            write_status(status_dir, "stopped", None, *restart_count);
        }
    }
}

// ─── File helpers ─────────────────────────────────────────────────────────────

fn write_status(dir: &Path, status: &str, pid: Option<u32>, restart_count: u32) {
    let record = HeadroomStatus {
        status: status.to_string(),
        pid,
        restart_count,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&record) {
        let path = dir.join("status.json");
        if let Err(e) = std::fs::write(&path, json) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "HeadroomSupervisor: failed to write status file"
            );
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("HOME") {
        return Some(PathBuf::from(v));
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let status_dir = dir.path().join(".ta").join("compression");
        std::fs::create_dir_all(&status_dir).unwrap();

        write_status(&status_dir, "running", Some(42000), 1);

        let status = read_status(dir.path()).unwrap();
        assert_eq!(status.status, "running");
        assert_eq!(status.pid, Some(42000));
        assert_eq!(status.restart_count, 1);
        assert!(!status.updated_at.is_empty());
    }

    #[test]
    fn status_file_absent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_status(dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn signal_restart_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        signal_restart(dir.path()).unwrap();
        let signal_path = dir
            .path()
            .join(".ta")
            .join("compression")
            .join("restart-signal");
        assert!(signal_path.exists());
    }

    #[test]
    fn find_headroom_binary_returns_none_gracefully() {
        // When headroom is not installed, should return None without panicking.
        // (We can't assert a specific result since it depends on the test environment.)
        let _ = find_headroom_binary();
    }

    #[test]
    fn status_serialization_all_fields() {
        let s = HeadroomStatus {
            status: "suspended".to_string(),
            pid: None,
            restart_count: 7,
            updated_at: "2026-06-22T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: HeadroomStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, "suspended");
        assert_eq!(back.restart_count, 7);
        assert!(back.pid.is_none());
    }
}
