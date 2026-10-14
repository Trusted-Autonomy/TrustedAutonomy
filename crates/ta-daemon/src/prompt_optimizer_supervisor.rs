// prompt_optimizer_supervisor.rs — Generic prompt-optimizer plugin supervisor (v0.17.0.10).
//
// Manages a single optimizer subprocess driven by `PromptOptimizerPluginConfig`.
// headroom is the built-in default plugin; any HTTP proxy can be substituted
// via `[compression.plugin]` in daemon.toml without changing this code.
//
// ## Lifecycle
//   1. Spawns `plugin.command` with `plugin.args`, merging `plugin.env` into env.
//   2. Polls `plugin.health_endpoint` every 10s; restarts on failure.
//   3. Exponential backoff (1→60 s cap); after 5 failures in 5 min the optimizer is
//      disabled for the session with one user-visible message (flap guard), never
//      restarted forever. `ta compression enable` clears it.
//   4. Writes status to `.ta/compression/status.json`, including restart count and
//      the last failure.
//
// ## No visible windows (v0.17.12.21)
// Every optimizer process is built by `build_command`, which sets `CREATE_NO_WINDOW`
// on Windows so a console child never opens (or repeatedly re-opens) a shell window.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use ta_context_opt::OptimizersConfig;

use crate::config::{CompressionConfig, PromptOptimizerPluginConfig};

const HEALTH_POLL_INTERVAL_SECS: u64 = 10;
const HEALTH_CHECK_TIMEOUT_SECS: u64 = 5;
const STARTUP_GRACE_SECS: u64 = 5;
const SUSPEND_FAILURE_COUNT: u32 = 5;
const SUSPEND_WINDOW_SECS: u64 = 300;
const MAX_BACKOFF_SECS: u64 = 60;
const RESTART_SIGNAL_POLL_SECS: u64 = 5;

/// Win32 `CREATE_NO_WINDOW`: the child gets no console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Process-creation flags applied to every optimizer spawn.
///
/// Only meaningful on Windows; kept as a constant on every platform so a test
/// can assert it without a Windows host.
pub const OPTIMIZER_CREATION_FLAGS: u32 = CREATE_NO_WINDOW;

/// Build the command that runs an optimizer plugin, with stdio detached and
/// the platform's window-suppression flag set. The only place an optimizer
/// process is constructed.
pub fn build_command(plugin: &PromptOptimizerPluginConfig) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&plugin.command);
    cmd.args(&plugin.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for (k, v) in &plugin.env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    cmd.creation_flags(OPTIMIZER_CREATION_FLAGS);
    cmd
}

// ─── Public surface ───────────────────────────────────────────────────────────

/// Start the prompt-optimizer supervisor if compression is enabled.
///
/// Returns immediately; the supervisor loop runs in a background task.
/// If the plugin binary is not found or compression is disabled, this is a no-op.
pub fn start(
    project_root: PathBuf,
    config: CompressionConfig,
    optimizers: OptimizersConfig,
    shutdown: Arc<Notify>,
) {
    // `[optimizers.input] use = "none"|"rtk"` means no proxy-kind optimizer runs.
    let input_forbids_proxy = optimizers.input_wants_proxy() == Some(false);
    if !config.enabled || input_forbids_proxy {
        // Write a "disabled" status file so the CLI always reads an authoritative state
        // and stale "running" entries from before `ta compression disable` are overwritten.
        let status_dir = project_root.join(".ta").join("compression");
        std::fs::create_dir_all(&status_dir).ok();
        write_status(&status_dir, "disabled", None, 0, None);
        tracing::debug!(
            input_slot = ?optimizers.input.use_,
            "PromptOptimizerSupervisor: proxy optimizer not selected — wrote disabled status"
        );
        return;
    }

    let plugin = config.effective_plugin();
    let status_dir = project_root.join(".ta").join("compression");

    // Verify the binary is findable before spawning.
    if which::which(&plugin.command).is_err() {
        eprintln!(
            "Warning: context compression is enabled but the `{cmd}` binary was not found on PATH.\n\
             \n  Check your [compression.plugin] configuration in .ta/daemon.toml:\n\
             \n      [compression.plugin]\n      command = \"{cmd}\"    # must be on PATH\n\
             \n  Or disable compression:\n\
             \n      ta compression disable\n",
            cmd = plugin.command
        );
        tracing::warn!(
            command = %plugin.command,
            plugin = %plugin.name,
            "compression.enabled=true but plugin binary not found — running without compression"
        );
        return;
    }

    tracing::info!(
        plugin = %plugin.name,
        command = %plugin.command,
        proxy_url = %plugin.proxy_base_url,
        health = %plugin.health_endpoint,
        "PromptOptimizerSupervisor: starting compression proxy"
    );

    tokio::spawn(async move {
        run_supervisor(plugin, status_dir, shutdown).await;
    });
}

/// Read the current supervisor status from disk.
pub fn read_status(project_root: &Path) -> Option<OptimizerStatus> {
    let path = project_root
        .join(".ta")
        .join("compression")
        .join("status.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write a restart-signal file so the supervisor clears Suspended state.
pub fn signal_restart(project_root: &Path) -> std::io::Result<()> {
    let dir = project_root.join(".ta").join("compression");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("restart-signal"), "restart")
}

// ─── Status record ────────────────────────────────────────────────────────────

/// Written to `.ta/compression/status.json`; read by `ta compression status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizerStatus {
    /// Schema version: 0 for legacy files (field absent), 1 for files written by v0.17.0.10.1+.
    #[serde(default)]
    pub schema_version: u32,
    pub status: String,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub updated_at: String,
    /// Name of the active plugin (e.g. "headroom", "custom-proxy").
    /// None in legacy files (before v0.17.0.10.1) and when compression is disabled.
    #[serde(default)]
    pub plugin_name: Option<String>,
    /// Why the optimizer last stopped (spawn error, exit code, failed health
    /// check). Cleared when the supervisor starts with the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    /// RFC 3339 time of `last_failure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<String>,
    /// One-line, user-facing explanation when the flap guard disabled the optimizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

// ─── Flap guard ───────────────────────────────────────────────────────────────

/// Counts failures in a sliding window; trips after `SUSPEND_FAILURE_COUNT`.
#[derive(Debug, Default)]
pub struct FlapGuard {
    recent: Vec<Instant>,
}

impl FlapGuard {
    /// Record a failure at `now`; true when the optimizer must be disabled.
    pub fn record(&mut self, now: Instant) -> bool {
        self.recent.push(now);
        let window = Duration::from_secs(SUSPEND_WINDOW_SECS);
        self.recent.retain(|t| now.duration_since(*t) < window);
        self.recent.len() as u32 >= SUSPEND_FAILURE_COUNT
    }

    pub fn failures(&self) -> usize {
        self.recent.len()
    }

    pub fn clear(&mut self) {
        self.recent.clear();
    }
}

// ─── Supervisor loop ──────────────────────────────────────────────────────────

async fn run_supervisor(
    plugin: PromptOptimizerPluginConfig,
    status_dir: PathBuf,
    shutdown: Arc<Notify>,
) {
    std::fs::create_dir_all(&status_dir).ok();
    // A new supervisor run starts with a clean failure history, matching its
    // restart count of zero.
    std::fs::remove_file(status_dir.join("status.json")).ok();
    write_status(&status_dir, "starting", None, 0, Some(&plugin.name));

    let mut restart_count: u32 = 0;
    let mut suspended = false;
    let mut flap_guard = FlapGuard::default();

    loop {
        // ── Suspended: poll for a restart signal ─────────────────────────
        if suspended {
            let signal_path = status_dir.join("restart-signal");
            if signal_path.exists() {
                std::fs::remove_file(&signal_path).ok();
                suspended = false;
                flap_guard.clear();
                tracing::info!(
                    plugin = %plugin.name,
                    "PromptOptimizerSupervisor: restart signal received — clearing suspended state"
                );
                write_status(
                    &status_dir,
                    "starting",
                    None,
                    restart_count,
                    Some(&plugin.name),
                );
            } else {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(RESTART_SIGNAL_POLL_SECS)) => {}
                    _ = shutdown.notified() => {
                        write_status(&status_dir, "stopped", None, restart_count, Some(&plugin.name));
                        return;
                    }
                }
                continue;
            }
        }

        // ── Spawn ─────────────────────────────────────────────────────────
        write_status(
            &status_dir,
            "starting",
            None,
            restart_count,
            Some(&plugin.name),
        );

        let mut cmd = build_command(&plugin);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(
                    command = %plugin.command,
                    plugin = %plugin.name,
                    error = %e,
                    "PromptOptimizerSupervisor: failed to spawn plugin process"
                );
                write_status(
                    &status_dir,
                    "stopped",
                    None,
                    restart_count,
                    Some(&plugin.name),
                );
                handle_failure(
                    &status_dir,
                    &plugin.name,
                    &format!("could not spawn `{}`: {e}", plugin.command),
                    &mut restart_count,
                    &mut suspended,
                    &mut flap_guard,
                    &shutdown,
                )
                .await;
                continue;
            }
        };

        let pid = child.id().unwrap_or(0);
        tracing::info!(
            plugin = %plugin.name,
            command = %plugin.command,
            pid,
            proxy_url = %plugin.proxy_base_url,
            "PromptOptimizerSupervisor: plugin running"
        );
        write_status(
            &status_dir,
            "running",
            Some(pid),
            restart_count,
            Some(&plugin.name),
        );

        // Startup grace: give the plugin time to bind the port.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(STARTUP_GRACE_SECS)) => {}
            _ = shutdown.notified() => {
                let _ = child.kill().await;
                write_status(&status_dir, "stopped", None, restart_count, Some(&plugin.name));
                return;
            }
        }

        // ── Monitor ───────────────────────────────────────────────────────
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(HEALTH_CHECK_TIMEOUT_SECS))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "PromptOptimizerSupervisor: failed to build HTTP client; skipping health checks"
                );
                reqwest::Client::new()
            }
        };

        let exit_reason = monitor(&mut child, &client, &plugin.health_endpoint, &shutdown).await;
        let _ = child.kill().await;
        write_status(
            &status_dir,
            "stopped",
            None,
            restart_count,
            Some(&plugin.name),
        );

        let failure_reason = exit_reason.describe(&plugin.health_endpoint);
        match exit_reason {
            ExitReason::Shutdown => {
                tracing::info!(
                    plugin = %plugin.name,
                    "PromptOptimizerSupervisor: stopped (daemon shutdown)"
                );
                return;
            }
            ExitReason::Clean => {
                tracing::info!(
                    plugin = %plugin.name,
                    "PromptOptimizerSupervisor: plugin exited cleanly"
                );
            }
            ExitReason::Crash(code) => {
                tracing::warn!(
                    plugin = %plugin.name,
                    code = ?code,
                    "PromptOptimizerSupervisor: plugin crashed"
                );
            }
            ExitReason::HealthFailed => {
                tracing::warn!(
                    plugin = %plugin.name,
                    "PromptOptimizerSupervisor: health check failed — restarting"
                );
            }
        }

        handle_failure(
            &status_dir,
            &plugin.name,
            &failure_reason,
            &mut restart_count,
            &mut suspended,
            &mut flap_guard,
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

impl ExitReason {
    /// One-line reason stored as the status file's `last_failure`.
    fn describe(&self, health_url: &str) -> String {
        match self {
            ExitReason::Shutdown => "daemon shutdown".to_string(),
            ExitReason::Clean => "process exited with code 0 unexpectedly".to_string(),
            ExitReason::Crash(Some(code)) => format!("process exited with code {code}"),
            ExitReason::Crash(None) => "process was terminated by a signal".to_string(),
            ExitReason::HealthFailed => format!("health check failed at {health_url}"),
        }
    }
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
                            "PromptOptimizerSupervisor: health check non-success — restarting"
                        );
                        return ExitReason::HealthFailed;
                    }
                    Err(e) => {
                        tracing::warn!(
                            url = health_url,
                            error = %e,
                            "PromptOptimizerSupervisor: health check failed — restarting"
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
    plugin_name: &str,
    reason: &str,
    restart_count: &mut u32,
    suspended: &mut bool,
    flap_guard: &mut FlapGuard,
    shutdown: &Arc<Notify>,
) {
    *restart_count = restart_count.saturating_add(1);
    record_failure(status_dir, reason);

    if flap_guard.record(Instant::now()) {
        let notice = format!(
            "The {plugin_name} optimizer failed {SUSPEND_FAILURE_COUNT} times in \
             {} minutes (last: {reason}) and is disabled for this session. \
             Agents run without it. Fix the cause, then run `ta compression enable` to resume.",
            SUSPEND_WINDOW_SECS / 60,
        );
        // The one user-visible message. The supervisor will not restart again
        // until a restart signal arrives.
        eprintln!("Warning: {notice}");
        tracing::error!(
            plugin = %plugin_name,
            failures = flap_guard.failures(),
            restart_count = *restart_count,
            last_failure = %reason,
            "PromptOptimizerSupervisor: disabled for the session after repeated failures"
        );
        *suspended = true;
        write_status(
            status_dir,
            "suspended",
            None,
            *restart_count,
            Some(plugin_name),
        );
        set_notice(status_dir, &notice);
        return;
    }

    let backoff_secs = MAX_BACKOFF_SECS
        .min(2u64.saturating_pow(*restart_count))
        .max(1);

    tracing::info!(
        plugin = %plugin_name,
        restart_count = *restart_count,
        backoff_secs,
        last_failure = %reason,
        "PromptOptimizerSupervisor: will restart in {}s",
        backoff_secs,
    );

    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
        _ = shutdown.notified() => {
            write_status(status_dir, "stopped", None, *restart_count, Some(plugin_name));
        }
    }
}

// ─── File helpers ─────────────────────────────────────────────────────────────

fn write_status(
    dir: &Path,
    status: &str,
    pid: Option<u32>,
    restart_count: u32,
    plugin_name: Option<&str>,
) {
    // Keep the failure history across state changes within one supervisor run.
    let previous = read_status_file(dir);
    let notice = if status == "suspended" {
        previous.as_ref().and_then(|p| p.notice.clone())
    } else {
        None
    };
    write_record(
        dir,
        &OptimizerStatus {
            schema_version: 1,
            status: status.to_string(),
            pid,
            restart_count,
            updated_at: chrono::Utc::now().to_rfc3339(),
            plugin_name: plugin_name.map(|s| s.to_string()),
            last_failure: previous.as_ref().and_then(|p| p.last_failure.clone()),
            last_failure_at: previous.as_ref().and_then(|p| p.last_failure_at.clone()),
            notice,
        },
    );
}

/// Store why the optimizer last stopped.
fn record_failure(dir: &Path, reason: &str) {
    if let Some(mut rec) = read_status_file(dir) {
        rec.last_failure = Some(reason.to_string());
        rec.last_failure_at = Some(chrono::Utc::now().to_rfc3339());
        rec.updated_at = chrono::Utc::now().to_rfc3339();
        write_record(dir, &rec);
    }
}

/// Attach the flap-guard message to the current status.
fn set_notice(dir: &Path, notice: &str) {
    if let Some(mut rec) = read_status_file(dir) {
        rec.notice = Some(notice.to_string());
        write_record(dir, &rec);
    }
}

fn read_status_file(dir: &Path) -> Option<OptimizerStatus> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("status.json")).ok()?).ok()
}

fn write_record(dir: &Path, record: &OptimizerStatus) {
    if let Ok(json) = serde_json::to_string_pretty(record) {
        let path = dir.join("status.json");
        if let Err(e) = std::fs::write(&path, json) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "PromptOptimizerSupervisor: failed to write status file"
            );
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressionConfig;

    #[test]
    fn status_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let status_dir = dir.path().join(".ta").join("compression");
        std::fs::create_dir_all(&status_dir).unwrap();

        write_status(&status_dir, "running", Some(42000), 1, Some("headroom"));

        let status = read_status(dir.path()).unwrap();
        assert_eq!(status.status, "running");
        assert_eq!(status.pid, Some(42000));
        assert_eq!(status.restart_count, 1);
        assert_eq!(status.plugin_name, Some("headroom".to_string()));
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
    fn start_writes_disabled_status() {
        let cfg = CompressionConfig {
            enabled: false,
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let shutdown = Arc::new(Notify::new());
        start(
            dir.path().to_path_buf(),
            cfg,
            OptimizersConfig::default(),
            shutdown,
        );
        let status = read_status(dir.path()).unwrap();
        assert_eq!(status.status, "disabled");
        assert_eq!(status.pid, None);
        assert_eq!(status.plugin_name, None);
        assert_eq!(status.schema_version, 1);
    }

    #[test]
    fn disabled_status_overwrites_stale_running_entry() {
        let dir = tempfile::tempdir().unwrap();
        let status_dir = dir.path().join(".ta").join("compression");
        std::fs::create_dir_all(&status_dir).unwrap();
        // Simulate a stale "running" entry left by a previous daemon start.
        write_status(&status_dir, "running", Some(99999), 0, Some("headroom"));
        assert_eq!(read_status(dir.path()).unwrap().status, "running");

        let cfg = CompressionConfig {
            enabled: false,
            ..Default::default()
        };
        let shutdown = Arc::new(Notify::new());
        start(
            dir.path().to_path_buf(),
            cfg,
            OptimizersConfig::default(),
            shutdown,
        );

        let status = read_status(dir.path()).unwrap();
        assert_eq!(status.status, "disabled");
        assert_eq!(status.pid, None);
    }

    #[test]
    fn schema_version_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let status_dir = dir.path().join(".ta").join("compression");
        std::fs::create_dir_all(&status_dir).unwrap();

        write_status(&status_dir, "running", Some(1234), 0, Some("headroom"));

        let status = read_status(dir.path()).unwrap();
        assert_eq!(
            status.schema_version, 1,
            "new writes must emit schema_version 1"
        );
    }

    #[test]
    fn legacy_json_schema_version_defaults_to_zero() {
        // JSON written before v0.17.0.10.1 has no schema_version field.
        let json = r#"{"status":"running","pid":1234,"restart_count":0,"updated_at":"2026-01-01T00:00:00Z"}"#;
        let s: OptimizerStatus = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.schema_version, 0,
            "missing schema_version defaults to 0 for legacy files"
        );
        assert_eq!(s.plugin_name, None, "missing plugin_name defaults to None");
        assert_eq!(s.status, "running");
    }

    #[test]
    fn status_serialization_all_fields() {
        let s = OptimizerStatus {
            schema_version: 1,
            status: "suspended".to_string(),
            pid: None,
            restart_count: 7,
            updated_at: "2026-06-22T00:00:00Z".to_string(),
            plugin_name: Some("my-proxy".to_string()),
            last_failure: None,
            last_failure_at: None,
            notice: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: OptimizerStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, 1);
        assert_eq!(back.status, "suspended");
        assert_eq!(back.restart_count, 7);
        assert!(back.pid.is_none());
        assert_eq!(back.plugin_name, Some("my-proxy".to_string()));
    }

    #[test]
    fn optimizer_creation_flags_include_create_no_window() {
        // Covers the flag value only. Whether a Windows child really opens no console cannot be
        // observed from a unit test (`std::process::Command` does not expose its creation flags),
        // so the real check is the manual Windows smoke test in docs/RELEASING.md, tracked by
        // PLAN v0.17.12.24 item 2. `build_command` is the one place an optimizer is spawned.
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
        assert_eq!(
            OPTIMIZER_CREATION_FLAGS & CREATE_NO_WINDOW,
            CREATE_NO_WINDOW
        );
    }

    #[test]
    fn build_command_carries_args_and_env() {
        let plugin = PromptOptimizerPluginConfig {
            command: "my-opt".to_string(),
            args: vec!["serve".to_string()],
            ..Default::default()
        };
        let cmd = build_command(&plugin);
        let std_cmd = cmd.as_std();
        assert_eq!(std_cmd.get_program(), "my-opt");
        assert_eq!(std_cmd.get_args().collect::<Vec<_>>(), vec!["serve"]);
        assert!(std_cmd
            .get_envs()
            .any(|(k, v)| k == "HEADROOM_LEARN" && v.is_some()));
    }

    #[test]
    fn flap_guard_trips_on_fifth_failure_in_window() {
        let mut g = FlapGuard::default();
        let t0 = Instant::now();
        for i in 0..4 {
            assert!(!g.record(t0 + Duration::from_secs(i * 10)), "failure {i}");
        }
        assert!(g.record(t0 + Duration::from_secs(40)));
    }

    #[test]
    fn flap_guard_forgets_failures_outside_the_window() {
        let mut g = FlapGuard::default();
        let t0 = Instant::now();
        for i in 0..4 {
            g.record(t0 + Duration::from_secs(i));
        }
        // Long after: the earlier four have aged out, so this is failure one.
        assert!(!g.record(t0 + Duration::from_secs(SUSPEND_WINDOW_SECS + 60)));
        assert_eq!(g.failures(), 1);
    }

    #[tokio::test]
    async fn repeated_failures_suspend_once_and_record_last_failure() {
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), "running", Some(1), 0, Some("headroom"));
        let shutdown = Arc::new(Notify::new());
        // Wake any backoff sleep immediately so the test does not wait.
        shutdown.notify_one();
        let (mut count, mut suspended, mut guard) = (0u32, false, FlapGuard::default());
        for i in 0..SUSPEND_FAILURE_COUNT {
            assert!(!suspended, "suspended too early at failure {i}");
            shutdown.notify_one();
            handle_failure(
                dir.path(),
                "headroom",
                "process exited with code 3",
                &mut count,
                &mut suspended,
                &mut guard,
                &shutdown,
            )
            .await;
        }
        assert!(suspended);
        assert_eq!(count, SUSPEND_FAILURE_COUNT);
        let st = read_status_file(dir.path()).unwrap();
        assert_eq!(st.status, "suspended");
        assert_eq!(st.restart_count, SUSPEND_FAILURE_COUNT);
        assert_eq!(
            st.last_failure.as_deref(),
            Some("process exited with code 3")
        );
        assert!(st.last_failure_at.is_some());
        let notice = st.notice.unwrap();
        assert!(notice.contains("ta compression enable"), "{notice}");
    }

    #[test]
    fn failure_history_survives_status_changes_but_notice_does_not() {
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), "running", Some(1), 0, Some("headroom"));
        record_failure(dir.path(), "health check failed at http://x/health");
        set_notice(dir.path(), "disabled");
        write_status(dir.path(), "suspended", None, 5, Some("headroom"));
        assert!(read_status_file(dir.path()).unwrap().notice.is_some());
        write_status(dir.path(), "starting", None, 5, Some("headroom"));
        let st = read_status_file(dir.path()).unwrap();
        assert_eq!(
            st.last_failure.as_deref(),
            Some("health check failed at http://x/health")
        );
        assert!(
            st.notice.is_none(),
            "notice clears once the optimizer restarts"
        );
    }

    #[test]
    fn input_slot_none_writes_disabled_status_even_when_compression_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut opt = OptimizersConfig::default();
        opt.input.use_ = Some("none".to_string());
        start(
            dir.path().to_path_buf(),
            CompressionConfig::default(),
            opt,
            Arc::new(Notify::new()),
        );
        assert_eq!(read_status(dir.path()).unwrap().status, "disabled");
    }

    #[test]
    fn legacy_status_without_failure_fields_still_parses() {
        let json =
            r#"{"schema_version":1,"status":"running","pid":1,"restart_count":2,"updated_at":"x"}"#;
        let s: OptimizerStatus = serde_json::from_str(json).unwrap();
        assert!(s.last_failure.is_none() && s.notice.is_none());
    }
}
