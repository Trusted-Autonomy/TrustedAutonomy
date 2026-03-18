// channel_listener_manager.rs — Daemon-managed Discord listener lifecycle (v0.12.1).
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
//   daemon starts → spawn_listener() → monitor loop → restart on exit
//   daemon stops → drop ChildGuard → SIGTERM/SIGKILL the listener

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

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
    let delay = Duration::from_secs(config.restart_delay_secs);

    tracing::info!(
        binary = %binary.display(),
        max_restarts,
        restart_delay_secs = config.restart_delay_secs,
        "Discord listener manager starting"
    );

    let mut restarts: u32 = 0;

    loop {
        tracing::info!(
            binary = %binary.display(),
            restarts,
            "Spawning Discord listener"
        );

        let child = match spawn_listener(&binary) {
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

        // Wait for the child to exit or the daemon to shut down.
        let exit_status = tokio::select! {
            status = wait_child(child) => status,
            _ = shutdown.notified() => {
                tracing::info!(pid, "Daemon shutting down — Discord listener will exit via PID file cleanup");
                // The listener handles its own graceful shutdown via ctrl-c / SIGTERM.
                // We just return here; the child process will be dropped (OS will reap it).
                return;
            }
        };

        match exit_status {
            Ok(status) => {
                tracing::warn!(
                    pid,
                    exit_code = ?status,
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

/// Spawn the Discord listener process, returning a tokio Child handle.
fn spawn_listener(binary: &Path) -> std::io::Result<Child> {
    tokio::process::Command::new(binary)
        .arg("--listen")
        // Inherit the daemon's environment (TA_DISCORD_TOKEN etc. flow through).
        .env_clear()
        .envs(std::env::vars())
        // Detach stdout/stdin; keep stderr for logging.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true) // Drop = SIGKILL on Unix
        .spawn()
}

/// Wait for a child process to exit, returning its exit status.
async fn wait_child(mut child: Child) -> std::io::Result<Option<i32>> {
    let status = child.wait().await?;
    Ok(status.code())
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
        let config = DiscordListenerConfig::default();
        assert!(!config.enabled); // opt-in
        assert_eq!(config.binary, "ta-channel-discord");
        assert_eq!(config.restart_delay_secs, 10);
        assert_eq!(config.max_restarts, 0); // unlimited
    }
}
