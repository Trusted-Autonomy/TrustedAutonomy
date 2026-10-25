// daemon.rs — Daemon lifecycle management (`ta daemon` subcommand).
//
// Provides start/stop/restart/status/log as first-class CLI verbs so users
// don't need wrapper scripts or knowledge of the `ta-daemon` binary.
//
// Shared helpers (`ensure_running`, `start`, `stop`, `restart`) are used by
// `shell.rs` and `version_guard.rs` to eliminate duplicated daemon spawn logic.

use clap::Subcommand;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::daemon_ownership::{self, DaemonProbe, HttpProbe, PortHolder};
use ta_mcp_gateway::daemon_port;
use ta_workspace::{
    DaemonAction, DaemonReason, DaemonReport, DaemonState as WsDaemonState,
    DAEMON_REPORT_SCHEMA_VERSION,
};

// ─── macOS code signing (self-healing, best-effort) ─────────────────────────
//
// install_local.sh signs ta/ta-daemon with a stable local identity so
// macOS Keychain's "Always Allow" grant (keyed to the signing identity, not
// the file path) survives rebuilds -- but that signing only happens when
// install_local.sh itself runs. A normal dev loop (`cargo build` then
// `ta daemon restart` directly) produces a fresh, unsigned
// `target/debug/ta-daemon` every time, so we re-sign immediately before every
// spawn, whichever command built it.
//
// The signing rules live in `ta_lifecycle::codesign` so the daemon's
// auto-restart and this CLI share one implementation: only the named local
// identity is used, never ad-hoc (`-`), never a new Keychain identity, always
// the stable identifier, bounded by a short timeout. When the identity is
// absent (every end user's machine) nothing is touched.
fn ensure_stable_codesign(binary_path: &Path, project_root: &Path) {
    let identity = ta_workspace::local_dev::codesign_identity(project_root);
    let outcome = ta_lifecycle::ensure_stable_codesign(
        &ta_lifecycle::SystemCodesign,
        binary_path,
        &identity,
        ta_lifecycle::DAEMON_IDENTIFIER,
    );
    // Silent for interactive use (end users have no dev certificate); an
    // automatic restart started by the daemon says what happened in its log.
    if std::env::var_os("TA_DAEMON_SELF_UPDATE").is_some() {
        eprintln!("{}", outcome.log_line(binary_path));
    }
}

/// `ta daemon` subcommands.
#[derive(Debug, Subcommand)]
pub enum DaemonCommands {
    /// Start the daemon in the background.
    Start {
        /// Run in the foreground (for debugging/containers) instead of daemonizing.
        #[arg(long)]
        foreground: bool,
        /// Daemon API port (overrides `[server] port` in daemon.toml;
        /// default 7700). The web dashboard is served by the same listener
        /// at /ui, so this never binds a second port.
        #[arg(long)]
        port: Option<u16>,
        /// Print one JSON document (state, pid, port, health) instead of text.
        /// Progress messages stay on stderr. Not valid with --foreground.
        #[arg(long)]
        json: bool,
    },
    /// Stop the running daemon gracefully.
    Stop,
    /// Restart the daemon (stop + start). Handles version mismatches.
    ///
    /// By default, waits for active goal runs and agent sessions to finish
    /// before restarting (graceful drain). Use --force to skip the drain and
    /// force-kill immediately.
    Restart {
        /// Override the daemon HTTP port on restart.
        #[arg(long)]
        port: Option<u16>,
        /// Skip graceful drain and force-kill the daemon immediately (SIGKILL).
        ///
        /// Use only when the daemon is unresponsive or you need to restart urgently.
        /// Active goal runs will be interrupted.
        #[arg(long)]
        force: bool,
        /// Print one JSON document (state, new and previous pid, port, health)
        /// instead of text. Progress messages stay on stderr.
        #[arg(long)]
        json: bool,
    },
    /// Show daemon status: PID, port, version, uptime, project root, active goals.
    ///
    /// With `--json`, prints one document (state, pid, port, url, pid and log
    /// files, reasons) on stdout for scripts and installers.
    Status {
        /// Print a JSON document instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Tail the daemon log file.
    Log {
        /// Number of lines to show (default: 50).
        #[arg(default_value = "50")]
        lines: usize,
        /// Follow the log in real time (like `tail -f`).
        #[arg(long, short)]
        follow: bool,
        /// Filter by log level (error, warn, info, debug, trace).
        #[arg(long)]
        level: Option<String>,
        /// Filter by component name (substring match in log lines).
        #[arg(long)]
        component: Option<String>,
        /// Filter by goal ID (substring match).
        #[arg(long)]
        goal: Option<String>,
    },
    /// Self-check: API responsive, event system, plugin status, disk space, goal process liveness.
    Health,
    /// Generate a platform launch unit (macOS LaunchAgent or Linux systemd service) so the
    /// daemon starts automatically on login/boot.  Prints the generated file and its install
    /// path; does NOT write the file without --apply.
    Install {
        /// Write and load the launch unit instead of just printing it.
        #[arg(long)]
        apply: bool,
    },
    /// Show the effective daemon configuration (port, agent framework, etc.).
    ///
    /// Reads daemon.toml and shows the resolved values that `ta run` will use,
    /// including the effective default agent framework.
    Config,
    /// Compare the running daemon's build with this `ta` binary and say what to
    /// do. `install_local.sh` runs it after installing; it never restarts a
    /// daemon unless `--restart` is given (a restart can re-trigger the macOS
    /// Keychain prompt).
    CheckBuild {
        /// Restart the daemon when it is on a different build.
        #[arg(long)]
        restart: bool,
    },
}

/// True when the command prints a JSON document on stdout (`start|restart|status --json`).
pub fn prints_json(command: &DaemonCommands) -> bool {
    matches!(
        command,
        DaemonCommands::Start { json: true, .. }
            | DaemonCommands::Restart { json: true, .. }
            | DaemonCommands::Status { json: true }
    )
}

/// Execute a `ta daemon` subcommand.
pub fn execute(command: &DaemonCommands, project_root: &Path) -> anyhow::Result<()> {
    match command {
        DaemonCommands::Start {
            foreground,
            port,
            json,
        } => cmd_start(project_root, *foreground, *port, *json),
        DaemonCommands::Stop => cmd_stop(project_root),
        DaemonCommands::Restart { port, force, json } => {
            cmd_restart(project_root, *port, *force, *json)
        }
        DaemonCommands::Status { json } => cmd_status(project_root, *json),
        DaemonCommands::Log {
            lines,
            follow,
            level,
            component,
            goal,
        } => cmd_log(
            project_root,
            *lines,
            *follow,
            level.as_deref(),
            component.as_deref(),
            goal.as_deref(),
        ),
        DaemonCommands::Health => cmd_health(project_root),
        DaemonCommands::Install { apply } => cmd_install(project_root, *apply),
        DaemonCommands::Config => cmd_config_show(project_root),
        DaemonCommands::CheckBuild { restart } => cmd_check_build(project_root, *restart),
    }
}

/// What `ta daemon check-build` found.
#[derive(Debug, PartialEq, Eq)]
enum BuildCheck {
    /// The daemon runs this build.
    Current,
    /// The daemon runs another build; the text says what to do.
    Stale(String),
}

/// Compare the running daemon's build with the installed one and word the next
/// step. `mode` is the daemon's own `[daemon] auto_update` value from its status
/// report; `interval_secs` is `update_check_interval_secs`.
fn build_check(
    daemon: &ta_lifecycle::BuildIdentity,
    installed: &ta_lifecycle::BuildIdentity,
    mode: &str,
    interval_secs: u64,
) -> BuildCheck {
    if matches!(
        ta_lifecycle::compare_builds(daemon, installed),
        ta_lifecycle::BuildComparison::Same
    ) {
        return BuildCheck::Current;
    }
    let text = if mode == "when_idle" {
        format!(
            "daemon is still {} (installed: {}); [daemon] auto_update = \"when_idle\" will restart it on its own when idle, within about {}s. To switch now: ta daemon restart",
            daemon.describe(),
            installed.describe(),
            interval_secs
        )
    } else {
        format!(
            "daemon is still {} (installed: {}); run `ta daemon restart` to switch (macOS may prompt for Keychain access once)",
            daemon.describe(),
            installed.describe()
        )
    };
    BuildCheck::Stale(text)
}

/// `[daemon] update_check_interval_secs` from `.ta/daemon.toml`, default 300.
fn update_check_interval_secs(project_root: &Path) -> u64 {
    std::fs::read_to_string(project_root.join(".ta").join("daemon.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("daemon")?
                .get("update_check_interval_secs")?
                .as_integer()
        })
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(300)
}

fn cmd_check_build(project_root: &Path, restart_now: bool) -> anyhow::Result<()> {
    let base_url = resolve_daemon_url(project_root, None);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let body = match client.get(format!("{base_url}/api/status")).send() {
        Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().ok(),
        _ => None,
    };
    let Some(body) = body else {
        println!(
            "daemon: none answering at {base_url}; the next command that needs it starts the installed build ({}).",
            installed_build().describe()
        );
        return Ok(());
    };
    let Some(daemon) = ta_lifecycle::BuildIdentity::from_status_json(&body) else {
        println!(
            "daemon: answered at {base_url} but did not report its version; run `ta daemon status`, then `ta daemon restart` to be sure it runs the installed build."
        );
        return Ok(());
    };
    let installed = installed_build();
    let mode = body["update"]["mode"].as_str().unwrap_or("ask");
    match build_check(
        &daemon,
        &installed,
        mode,
        update_check_interval_secs(project_root),
    ) {
        BuildCheck::Current => {
            println!(
                "daemon: running the installed build ({}).",
                daemon.describe()
            );
            Ok(())
        }
        BuildCheck::Stale(text) if !restart_now => {
            println!("daemon: {text}");
            Ok(())
        }
        BuildCheck::Stale(_) => {
            println!(
                "daemon: restarting {} -> {} (--restart)...",
                daemon.describe(),
                installed.describe()
            );
            let pid = restart(project_root, None).map_err(|e| {
                anyhow::anyhow!(
                    "could not restart the daemon at {base_url}: {e}. Run `ta daemon restart` yourself, or `ta daemon log` for details."
                )
            })?;
            println!("daemon: restarted (pid {pid}); `ta daemon status` shows the new build.");
            Ok(())
        }
    }
}

fn installed_build() -> ta_lifecycle::BuildIdentity {
    ta_lifecycle::BuildIdentity::new(env!("CARGO_PKG_VERSION"), Some(env!("TA_GIT_HASH")))
}

// ─── Shared helpers (used by shell.rs, version_guard.rs, etc.) ───────────────

/// PID-file path: `.ta/daemon.pid`.
fn pid_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("daemon.pid")
}

/// Log-file path: `.ta/daemon.log`.
fn log_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("daemon.log")
}

/// Read the PID from `.ta/daemon.pid` (any format a TA binary has written,
/// via the shared `ta_mcp_gateway::daemon_pid` reader). Returns `None` if
/// the file is missing or has no valid `pid=<N>` line.
pub fn read_pid(project_root: &Path) -> Option<u32> {
    ta_mcp_gateway::daemon_pid::read_pid_file(project_root)
        .ok()
        .flatten()
        .and_then(|p| p.pid)
}

/// Read the port from `.ta/daemon.pid`: the daemon's `bind=host:port` line
/// or the legacy `port=` line. Returns `None` if absent.
fn read_pid_port(project_root: &Path) -> Option<u16> {
    ta_mcp_gateway::daemon_pid::read_pid_file(project_root)
        .ok()
        .flatten()
        .and_then(|p| p.port)
}

/// Write the PID file in the single shared format (pid, bind, port, log).
/// The daemon rewrites it in the same format once it has bound its port.
fn write_pid_file(project_root: &Path, pid: u32, port: u16) -> anyhow::Result<()> {
    let (bind, _) = daemon_toml_bind_port(project_root);
    ta_mcp_gateway::daemon_pid::write_pid_file(
        project_root,
        pid,
        &bind,
        port,
        &log_path(project_root),
    )?;
    Ok(())
}

/// Remove the PID file if it exists.
fn remove_pid_file(project_root: &Path) {
    let _ = std::fs::remove_file(pid_path(project_root));
}

/// Remove a leftover `.ta/daemon.json` (its daemon crashed or exited, or it was
/// copied from another project) and say so. A live daemon's record is never
/// touched.
fn clean_stale_daemon_record(project_root: &Path) {
    if let Some(reason) = ta_mcp_gateway::daemon_json::clean_stale(project_root) {
        eprintln!("Removed stale .ta/daemon.json: {reason}");
    }
}

/// Check whether a process with the given PID is alive.
pub fn is_process_alive(pid: u32) -> bool {
    ta_lifecycle::process_is_alive(pid)
}

/// `[server] bind` and `[server] port` from `.ta/daemon.toml`. A project
/// without an explicit port gets its derived per-project port (see
/// `ta_mcp_gateway::daemon_port`), never the shared 7700.
fn daemon_toml_bind_port(project_root: &Path) -> (String, u16) {
    let bind = daemon_port::read_server_table(project_root)
        .ok()
        .and_then(|(b, _)| b)
        .unwrap_or_else(|| "127.0.0.1".to_string());
    (bind, daemon_port::default_port_lenient(project_root))
}

/// Host and port to reach `project_root`'s daemon on, with optional port
/// override. Order: `port_override`, then the live daemon's `.ta/daemon.json`,
/// then its `.ta/daemon.pid` (the port it actually bound), then `[server] port`
/// in `.ta/daemon.toml`, then the project's derived port. A pid file that
/// exists but carries no readable port is reported loudly on stderr rather
/// than silently ignored.
fn resolve_daemon_addr(project_root: &Path, port_override: Option<u16>) -> (String, u16) {
    let (bind, toml_port) = daemon_toml_bind_port(project_root);

    if let Some(p) = port_override {
        return (bind, p);
    }
    // `.ta/daemon.json`, while its daemon is running and it names this project,
    // is the one authoritative record of where the daemon is.
    match ta_mcp_gateway::daemon_json::read(project_root) {
        ta_mcp_gateway::daemon_json::Lookup::Live(rec) => return (rec.host, rec.port),
        ta_mcp_gateway::daemon_json::Lookup::Ignored { reason } => {
            tracing::debug!(reason = %reason, "ignoring .ta/daemon.json; falling back to daemon.pid");
        }
        ta_mcp_gateway::daemon_json::Lookup::Absent => {}
    }

    match ta_mcp_gateway::daemon_pid::read_pid_file(project_root) {
        Ok(Some(pf)) => match pf.port {
            Some(p) => {
                let host = pf.host.unwrap_or(bind);
                return (host, p);
            }
            None => {
                eprintln!(
                    "[warn] {} has no usable port line (expected `bind=<host>:<port>` or \
                     `port=<port>`). Using port {} from .ta/daemon.toml or the project default \
                     instead. Run `ta daemon restart` to rewrite the pid file.",
                    pid_path(project_root).display(),
                    toml_port
                );
                tracing::warn!(
                    path = %pid_path(project_root).display(),
                    fallback_port = toml_port,
                    "daemon.pid has no usable port"
                );
            }
        },
        Ok(None) => {}
        Err(e) => {
            eprintln!(
                "[warn] Could not read {}: {}. Using port {} from .ta/daemon.toml or the \
                 project default.",
                pid_path(project_root).display(),
                e,
                toml_port
            );
        }
    }

    (bind, toml_port)
}

/// Resolve the daemon URL for `project_root`, with optional port override.
/// See [`resolve_daemon_addr`] for the order. (Whiteboard tools, which send
/// session tokens, go further and refuse outright: see
/// `ta_mcp_gateway::daemon_pid::resolve_daemon_endpoint`.)
pub fn resolve_daemon_url(project_root: &Path, port_override: Option<u16>) -> String {
    let (host, port) = resolve_daemon_addr(project_root, port_override);
    format!("http://{}:{}", host, port)
}

/// The port `project_root`'s daemon is (or will be) reachable on: the live
/// pid file, else `[server] port`, else the project's derived port.
pub fn resolve_daemon_port(project_root: &Path) -> u16 {
    resolve_daemon_addr(project_root, None).1
}

/// Error unless whatever answers on this project's daemon port is this
/// project's own daemon (or nothing answers). For commands that are about to
/// talk to "the daemon" and must not talk to another project's.
pub fn ensure_own_daemon_on_port(project_root: &Path) -> anyhow::Result<()> {
    ensure_port_is_ours(
        "use the daemon on this port",
        project_root,
        None,
        &HttpProbe,
    )
    .map(|_| ())
}

/// `(pid, port)` from this project's pid file when that pid is alive, used to
/// recognise an older daemon that does not report its project.
fn live_local_pid_file(project_root: &Path) -> Option<(u32, Option<u16>)> {
    let pid = read_pid(project_root)?;
    is_process_alive(pid).then(|| (pid, read_pid_port(project_root)))
}

/// Refuse (with an error naming both projects) unless the daemon port is free
/// or held by this project's own daemon. `action` completes "Refusing to ...".
fn ensure_port_is_ours(
    action: &str,
    project_root: &Path,
    port_override: Option<u16>,
    probe: &dyn DaemonProbe,
) -> anyhow::Result<PortHolder> {
    let (host, port) = resolve_daemon_addr(project_root, port_override);
    let configured = daemon_toml_bind_port(project_root).1;
    daemon_ownership::guard(
        action,
        project_root,
        &format!("http://{}:{}", host, port),
        port,
        configured,
        probe,
        live_local_pid_file(project_root),
    )
}

/// Arguments `ta daemon start` passes to `ta-daemon`. `--port` sets the API
/// port (`[server] port` override); it is never forwarded as `--web-port`,
/// which used to make the API and the legacy web UI both bind the same
/// port and crash the daemon with "Address already in use".
fn daemon_spawn_args(
    project_root: &Path,
    port_override: Option<u16>,
    foreground: bool,
) -> Vec<String> {
    let mut args = vec!["--api".to_string()];
    if foreground {
        args.push("--foreground".to_string());
    }
    args.push("--project-root".to_string());
    args.push(project_root.display().to_string());
    if let Some(port) = port_override {
        args.push("--port".to_string());
        args.push(port.to_string());
    }
    args
}

/// The port a daemon start will use, after every safety check.
struct StartPlan {
    port: u16,
}

/// Choose the port for a daemon start and refuse unless it is usable: an
/// explicit `--port` or `[server] port` is used as is, otherwise a free port
/// derived from the project id. Whatever already answers on that port must
/// not be another project's daemon (or anything else), and a derived choice
/// is recorded in `.ta/daemon.toml` so every client resolves the same port.
fn plan_start(
    project_root: &Path,
    port_override: Option<u16>,
    probe: &dyn DaemonProbe,
) -> anyhow::Result<StartPlan> {
    let (bind, _) = daemon_toml_bind_port(project_root);
    let choice = match port_override {
        Some(p) => daemon_port::StartPort {
            port: p,
            explicit: true,
            derived: p,
        },
        None => {
            daemon_port::choose_start_port(project_root, &|p| daemon_port::port_is_free(&bind, p))
                .map_err(|e| anyhow::anyhow!("{e}"))?
        }
    };
    let port = choice.port;
    let configured = daemon_toml_bind_port(project_root).1;

    // Whatever already answers on that port must not be mistaken for ours.
    let base_url = format!("http://{}:{}", bind, port);
    let holder = daemon_ownership::identify_holder(
        project_root,
        &base_url,
        port,
        probe,
        live_local_pid_file(project_root),
    );
    if let Some(msg) =
        daemon_ownership::refusal("start the daemon", project_root, port, configured, &holder)
    {
        return Err(anyhow::anyhow!(msg));
    }
    if let PortHolder::ThisProject { pid, .. } = holder {
        return Err(anyhow::anyhow!(
            "A daemon for this project ({}) is already responding at {}{} (no live pid file \
             matched). Use `ta daemon stop` to shut it down, or `ta daemon restart` to replace it.",
            project_root.display(),
            base_url,
            pid.map(|p| format!(", pid {p}")).unwrap_or_default()
        ));
    }
    // A derived port was verified free a moment ago by `choose_start_port`;
    // only an explicit port still needs this check.
    if choice.explicit && !daemon_port::port_is_free(&bind, port) {
        return Err(anyhow::anyhow!(
            "Cannot start the daemon for {}: port {} on {} is in use by a process that does not \
             answer like a TA daemon. Find it with `lsof -i :{}` (macOS, Linux) or \
             `netstat -ano | findstr :{}` (Windows), stop it, or set `[server] port = <free port>` \
             in {}.",
            project_root.display(),
            port,
            bind,
            port,
            port,
            ta_mcp_gateway::daemon_pid::daemon_toml_path(project_root).display()
        ));
    }

    // A derived port is recorded before the daemon starts so every client
    // (CLI, MCP gateway, editor plugins) resolves the same value.
    if !choice.explicit {
        match daemon_port::record_port(project_root, port) {
            Ok(path) => eprintln!(
                "  No [server] port is set for this project: chose port {} (derived from the \
                 project id, range {}-{}) and recorded it in {}.",
                port,
                daemon_port::PORT_RANGE_START,
                daemon_port::PORT_RANGE_END,
                path.display()
            ),
            Err(e) => {
                tracing::warn!(port, error = %e, "could not record the chosen daemon port");
                eprintln!("[warn] {e}");
            }
        }
    }
    Ok(StartPlan { port })
}

/// Start the daemon in the background. Returns the child PID on success.
///
/// If this project's daemon is already running, returns an error. If the port
/// is held by another project's daemon (or anything else), refuses with a
/// message naming both projects. A project without an explicit port gets a
/// free per-project port, recorded in `.ta/daemon.toml`. Stale PID files are
/// cleaned up automatically.
pub fn start(project_root: &Path, port_override: Option<u16>) -> anyhow::Result<u32> {
    start_with_binary(
        project_root,
        port_override,
        &super::version_guard::find_daemon_binary,
        &HttpProbe,
    )
}

/// [`start`] with the daemon binary lookup and the ownership probe injected.
/// Unit tests pass a lookup that fails, so a test can never launch a real
/// `ta-daemon`. On Windows the test runner's `PATH` includes `target\debug`,
/// where the normal lookup finds a real daemon that the test would then leave
/// running.
fn start_with_binary(
    project_root: &Path,
    port_override: Option<u16>,
    find_daemon_binary: &dyn Fn() -> anyhow::Result<std::path::PathBuf>,
    probe: &dyn DaemonProbe,
) -> anyhow::Result<u32> {
    // Check for existing PID file.
    if let Some(pid) = read_pid(project_root) {
        if is_process_alive(pid) {
            return Err(anyhow::anyhow!(
                "Daemon already running for {} (pid {}). Use `ta daemon stop` first, or \
                 `ta daemon restart` to replace it.",
                project_root.display(),
                pid
            ));
        }
        // Stale PID file — remove it.
        remove_pid_file(project_root);
    }
    if let ta_mcp_gateway::daemon_json::Lookup::Live(rec) =
        ta_mcp_gateway::daemon_json::read(project_root)
    {
        return Err(anyhow::anyhow!(
            "Daemon already running for {} (pid {}, {}; recorded in .ta/daemon.json). Use \
             `ta daemon stop` first, or `ta daemon restart` to replace it.",
            project_root.display(),
            rec.pid,
            rec.base_url()
        ));
    }
    clean_stale_daemon_record(project_root);

    let StartPlan { port, .. } = plan_start(project_root, port_override, probe)?;
    let port_override = Some(port);

    let daemon_bin = find_daemon_binary()?;
    ensure_stable_codesign(&daemon_bin, project_root);

    // Ensure .ta directory exists.
    let ta_dir = project_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;

    let log = log_path(project_root);
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| anyhow::anyhow!("Cannot open daemon log {}: {}", log.display(), e))?;

    let stderr_log = log_file
        .try_clone()
        .map_err(|e| anyhow::anyhow!("Cannot clone log file handle: {}", e))?;

    let mut cmd = Command::new(&daemon_bin);
    cmd.args(daemon_spawn_args(project_root, port_override, false))
        .stdout(log_file)
        .stderr(stderr_log);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Cannot spawn {}: {}", daemon_bin.display(), e))?;

    let pid = child.id();

    write_pid_file(project_root, pid, port)?;

    // Wait briefly to confirm the daemon didn't crash immediately (e.g., port in use).
    std::thread::sleep(std::time::Duration::from_millis(300));
    if let Some(exit_status) = child.try_wait().unwrap_or(None) {
        remove_pid_file(project_root);
        return Err(anyhow::anyhow!(
            "Daemon exited immediately (exit code: {}). Check the log:\n  {}",
            exit_status.code().unwrap_or(-1),
            log_path(project_root).display()
        ));
    }

    Ok(pid)
}

/// Stop the running daemon. Sends POST /api/shutdown, waits up to 5s for exit,
/// then cleans up the PID file.
///
/// Refuses, without sending anything, when the daemon answering on this
/// project's port belongs to a different project.
pub fn stop(project_root: &Path) -> anyhow::Result<()> {
    stop_with(project_root, &HttpProbe)
}

/// [`stop`] with the ownership probe injected.
fn stop_with(project_root: &Path, probe: &dyn DaemonProbe) -> anyhow::Result<()> {
    ensure_port_is_ours("stop the daemon", project_root, None, probe)?;
    let base_url = resolve_daemon_url(project_root, None);
    let shutdown_url = format!("{}/api/shutdown", base_url);

    // Try graceful shutdown via HTTP.
    // The shutdown endpoint requires X-TA-Admin-Confirm: shutdown (v0.17.0.9)
    // to prevent accidental shutdown by agent subprocesses. Shutdown is a gated
    // route: the CLI presents the human credential from `ta login`.
    // Use a 10s timeout: the daemon is known alive (drain check already passed),
    // so 3s was too short — a busy daemon handling drain cleanup could miss it.
    let client = super::human_auth::blocking_client_builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let resp = client
        .post(&shutdown_url)
        .header("X-TA-Admin-Confirm", "shutdown")
        .send();

    let http_sent = match &resp {
        Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
            eprintln!(
                "Warning: shutdown rate-limited (tried too recently). \
                 Falling back to SIGTERM."
            );
            false // fall through to PID kill
        }
        Ok(r) if r.status() == reqwest::StatusCode::FORBIDDEN => {
            // Shutdown is a gated route: it needs the human credential.
            eprintln!(
                "Warning: the daemon refused the shutdown request (403): run `ta login` so \
                 the CLI can present a human credential. Falling back to SIGTERM."
            );
            false // fall through to PID kill
        }
        Ok(_) => true,
        Err(e) if e.is_timeout() => {
            // Daemon is alive (drain check passed) but took > 10s to ack shutdown.
            // Send SIGTERM — the daemon's signal handler triggers clean shutdown.
            eprintln!(
                "Warning: daemon did not acknowledge HTTP shutdown within 10s \
                 — sending SIGTERM to allow clean exit. Use --force to skip waiting."
            );
            false
        }
        Err(_) => false, // connection error: daemon not reachable, fall through to PID
    };

    if !http_sent {
        // HTTP shutdown didn't succeed — try to kill by PID.
        if let Some(pid) = read_pid(project_root) {
            if is_process_alive(pid) {
                #[cfg(unix)]
                {
                    let _ = Command::new("kill").arg(pid.to_string()).output();
                }
            } else {
                eprintln!("Daemon not running (stale PID file).");
                remove_pid_file(project_root);
                clean_stale_daemon_record(project_root);
                return Ok(());
            }
        } else {
            return Err(anyhow::anyhow!(
                "Cannot reach daemon at {} and no PID file found. \
                 The daemon may not be running.",
                base_url
            ));
        }
    }

    // Wait for the process to exit (up to 5s).
    let pid = read_pid(project_root);
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        match pid {
            Some(p) if is_process_alive(p) => continue,
            _ => break,
        }
    }

    // Final check.
    if let Some(p) = pid {
        if is_process_alive(p) {
            return Err(anyhow::anyhow!(
                "Daemon (pid {}) did not exit within 5 seconds. \
                 Force kill it: kill -9 {}",
                p,
                p
            ));
        }
    }

    remove_pid_file(project_root);
    clean_stale_daemon_record(project_root);
    Ok(())
}

/// Restart the daemon: stop the old one (if running), then start a new one.
///
/// Stops the daemon gracefully (HTTP shutdown). "Not running" errors are ignored
/// so this can be called when the daemon may or may not be live.
pub fn restart(project_root: &Path, port_override: Option<u16>) -> anyhow::Result<u32> {
    restart_with(project_root, port_override, &HttpProbe, &|| {
        start(project_root, port_override)
    })
}

/// [`restart`] with the ownership probe and the start step injected.
fn restart_with(
    project_root: &Path,
    port_override: Option<u16>,
    probe: &dyn DaemonProbe,
    start_step: &dyn Fn() -> anyhow::Result<u32>,
) -> anyhow::Result<u32> {
    // A port held by another project is a refusal, not "was not running":
    // check before stopping and before the stop error filter below.
    ensure_port_is_ours("restart the daemon", project_root, port_override, probe)?;

    // Stop the daemon, ignoring errors that simply mean it was not running.
    match stop_with(project_root, probe) {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            // Only suppress "not running" variants — propagate real errors.
            let is_not_running = msg.contains("not running")
                || msg.contains("no PID file")
                || msg.contains("Cannot reach daemon");
            if !is_not_running {
                return Err(e);
            }
        }
    }

    // Brief pause to let the port be released.
    std::thread::sleep(std::time::Duration::from_millis(300));

    start_step()
}

/// Force-kill the daemon (SIGKILL) and start a fresh one.
///
/// Sends SIGKILL to the daemon process (if a PID is known), waits 1s for exit,
/// then starts a new daemon. Skips the graceful drain — active goal runs are interrupted.
pub fn force_restart(project_root: &Path, port_override: Option<u16>) -> anyhow::Result<u32> {
    // --force skips the drain, never the ownership check.
    ensure_port_is_ours(
        "force-restart the daemon",
        project_root,
        port_override,
        &HttpProbe,
    )?;
    if let Some(pid) = read_pid(project_root) {
        if is_process_alive(pid) {
            eprintln!("Force-killing daemon (pid {})...", pid);
            #[cfg(unix)]
            {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
                std::thread::sleep(std::time::Duration::from_secs(1));
                if is_process_alive(pid) {
                    return Err(anyhow::anyhow!(
                        "Daemon (pid {}) did not exit after SIGKILL. \
                         Check system resources or try manually: kill -9 {}",
                        pid,
                        pid
                    ));
                }
            }
            #[cfg(not(unix))]
            {
                // Non-POSIX: fall back to graceful stop.
                let _ = stop(project_root);
            }
        }
    } else {
        // No PID file — try HTTP shutdown (may already be down).
        let _ = stop(project_root);
    }
    remove_pid_file(project_root);
    std::thread::sleep(std::time::Duration::from_millis(300));
    start(project_root, port_override)
}

/// Poll `/api/drain/status` until the daemon reports no active work.
///
/// Prints progress on every poll cycle. Returns `Ok(())` as soon as the daemon
/// is clean or stops responding (already exited is also fine).
///
/// Times out after `DRAIN_TIMEOUT_SECS` and returns an error explaining what is
/// still blocking so the user knows whether to wait or use `--force`. The
/// polling loop itself is `ta_lifecycle::wait_for_idle`, shared with the
/// daemon's automatic self-update and the VT poller.
const DRAIN_TIMEOUT_SECS: u64 = 30;
const DRAIN_POLL_SECS: u64 = 2;

/// `/api/drain/status` over HTTP, as a `ta_lifecycle::DrainSource`.
struct HttpDrainSource {
    client: reqwest::blocking::Client,
    url: String,
}

impl ta_lifecycle::DrainSource for HttpDrainSource {
    fn drain_status(&self) -> Result<Option<ta_lifecycle::DrainSnapshot>, String> {
        match self.client.get(&self.url).send() {
            Ok(resp) if resp.status().is_success() => resp
                .json::<serde_json::Value>()
                .map(|j| ta_lifecycle::DrainSnapshot::from_json(&j))
                .map_err(|e| format!("unreadable drain status from {}: {e}", self.url)),
            // Daemon stopped responding: already exited or unreachable.
            _ => Ok(None),
        }
    }
}

fn drain_and_poll(project_root: &Path, port_override: Option<u16>) -> anyhow::Result<()> {
    let base_url = resolve_daemon_url(project_root, port_override);
    let source = HttpDrainSource {
        client: match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return Ok(()), // Cannot build client: proceed without drain check.
        },
        url: format!("{}/api/drain/status", base_url),
    };
    let opts = ta_lifecycle::DrainWaitOptions {
        timeout: std::time::Duration::from_secs(DRAIN_TIMEOUT_SECS),
        poll_interval: std::time::Duration::from_secs(DRAIN_POLL_SECS),
    };
    let root = project_root.display().to_string();
    eprintln!(
        "Checking for active work in the daemon for project {} ({}) before restart...",
        root, base_url
    );
    ta_lifecycle::wait_for_idle(
        &source,
        &ta_lifecycle::SystemClock,
        &opts,
        &mut |snap, remaining| {
            eprintln!("{}", waiting_message(&root, snap, remaining));
        },
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "{e}\n  Project: {root}\n  Daemon: {base_url}\n  Wait for the work above to finish, \
             or interrupt it with `ta daemon restart --force`."
        )
    })?;
    eprintln!("  No active work: restarting.");
    Ok(())
}

/// One progress line while a restart waits for work to drain: names the
/// project, each running goal (id and title) and how to look at it.
fn waiting_message(
    project_root: &str,
    snap: &ta_lifecycle::DrainSnapshot,
    remaining: u64,
) -> String {
    format!(
        "  Waiting in project {}: {} ({}s remaining before timeout). \
         Use `ta daemon restart --force` to interrupt it.",
        project_root,
        snap.describe_blockers().join("; "),
        remaining
    )
}

/// Ensure the daemon is running. If it's already responding, return Ok.
/// If not, start it and wait for it to become healthy.
///
/// This is the shared entry point used by `ta shell`, `ta run`, `ta dev`, etc.
pub fn ensure_running(project_root: &Path) -> anyhow::Result<()> {
    let base_url = resolve_daemon_url(project_root, None);

    // Quick health check.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    // Use /health (not /api/status) — /api/status scans all drafts/goals and can
    // take 4+ seconds on large workspaces, reliably exceeding the 2s timeout.
    let status_url = format!("{}/health", base_url);
    let reachable = client.get(&status_url).send().is_ok();

    if reachable {
        // Something answers. Using another project's daemon would run this
        // project's commands against the wrong project, so say so.
        ensure_port_is_ours(
            "use the daemon on this port",
            project_root,
            None,
            &HttpProbe,
        )?;
        wait_out_startup(&client, &base_url, project_root)?;
        return Ok(());
    }

    // Not reachable — start it.
    eprintln!("Daemon not reachable at {} — starting...", base_url);
    let pid = start(project_root, None)?;
    let port = read_pid_port(project_root).unwrap_or_else(|| daemon_toml_bind_port(project_root).1);

    eprintln!(
        "  Started daemon (pid {}), port {}, log: {}",
        pid,
        port,
        log_path(project_root).display()
    );

    // Wait for it to become healthy (up to 10s).
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if client.get(&status_url).send().is_ok() {
            wait_out_startup(&client, &base_url, project_root)?;
            eprintln!("  Daemon is ready.");
            return Ok(());
        }
    }

    Err(anyhow::anyhow!(
        "Daemon started (pid {}) but did not become healthy within 10 seconds. \
         Check logs: {}",
        pid,
        log_path(project_root).display()
    ))
}

// ─── Startup state (keychain approval, whiteboard link) ──────────────────────

/// The daemon's `status` field: `ok`, `starting` or `degraded`. A daemon from
/// before the field existed has none and counts as `ok`.
pub(crate) fn daemon_state(json: &serde_json::Value) -> &str {
    json["status"].as_str().unwrap_or("ok")
}

/// Printable lines for the reasons a daemon is `starting` or `degraded`, each
/// with its next step. Empty when the daemon reports `ok`.
pub(crate) fn startup_report_lines(json: &serde_json::Value) -> Vec<String> {
    let state = daemon_state(json);
    if state == "ok" {
        return Vec::new();
    }
    let mut lines = vec![format!("  State:          {state}")];
    let reasons = json["reasons"].as_array().cloned().unwrap_or_default();
    if reasons.is_empty() {
        lines.push("    (the daemon gave no reason; see `ta daemon log`)".to_string());
    }
    for r in &reasons {
        lines.push(format!(
            "    - {}: {}",
            r["component"].as_str().unwrap_or("daemon"),
            r["reason"].as_str().unwrap_or("no reason given")
        ));
        if let Some(next) = r["next_step"].as_str() {
            lines.push(format!("      Next step: {next}"));
        }
    }
    lines
}

/// Poll `/health` until the daemon answers (or `attempts` run out) and return
/// the last body. A `starting` daemon is returned as is, with its reasons.
fn poll_health(
    client: &reqwest::blocking::Client,
    base_url: &str,
    attempts: u32,
    keep_waiting_while_starting: bool,
) -> Option<serde_json::Value> {
    let url = format!("{}/health", base_url);
    let mut last = None;
    for _ in 0..attempts {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(body) = client
            .get(&url)
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(|r| r.json::<serde_json::Value>().ok())
        {
            let starting = daemon_state(&body) == "starting";
            last = Some(body);
            if !(starting && keep_waiting_while_starting) {
                break;
            }
        }
    }
    last
}

/// Print the health line after a start or restart: `ok`, or the state with its
/// reasons and next steps.
fn print_health_after_start(last: Option<&serde_json::Value>, base_url: &str) {
    match last {
        Some(body) if daemon_state(body) == "ok" => {
            println!("  Health: ok (listening on {})", base_url)
        }
        Some(body) => {
            println!(
                "  Health: {} (listening on {})",
                daemon_state(body),
                base_url
            );
            for line in startup_report_lines(body).into_iter().skip(1) {
                println!("{line}");
            }
            println!("  Check progress with: ta daemon status");
        }
        None => println!(
            "  Health: starting (not yet responding on {}; check the log: ta daemon log)",
            base_url
        ),
    }
}

/// A daemon that answers but reports `starting` is waiting for something (most
/// often a Keychain approval). Say what, with the next step, and wait for it
/// up to the daemon's own keychain timeout plus a margin, instead of letting
/// the caller's next request fail with a 503.
fn wait_out_startup(
    client: &reqwest::blocking::Client,
    base_url: &str,
    project_root: &Path,
) -> anyhow::Result<()> {
    const MAX_WAIT_SECS: u64 = 75;
    let url = format!("{}/health", base_url);
    let fetch = || {
        client
            .get(&url)
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(|r| r.json::<serde_json::Value>().ok())
    };
    let Some(first) = fetch().filter(|b| daemon_state(b) == "starting") else {
        return Ok(());
    };
    eprintln!("  The daemon is still starting:");
    for line in startup_report_lines(&first).into_iter().skip(1) {
        eprintln!("{line}");
    }
    eprintln!("  Waiting up to {MAX_WAIT_SECS}s for it to finish...");
    let started = std::time::Instant::now();
    while started.elapsed().as_secs() < MAX_WAIT_SECS {
        std::thread::sleep(std::time::Duration::from_secs(1));
        match fetch() {
            Some(b) if daemon_state(&b) != "starting" => {
                if daemon_state(&b) == "degraded" {
                    eprintln!("  The daemon started DEGRADED:");
                    for line in startup_report_lines(&b).into_iter().skip(1) {
                        eprintln!("{line}");
                    }
                }
                return Ok(());
            }
            Some(_) => {}
            None => break,
        }
    }
    Err(anyhow::anyhow!(
        "The daemon at {base_url} is still starting after {}s (project {}). Check what it is \
         waiting for with `ta daemon status`; if a macOS Keychain dialog is open, click \
         Always Allow. Log: {}",
        started.elapsed().as_secs(),
        project_root.display(),
        log_path(project_root).display()
    ))
}

// ─── CLI command implementations ─────────────────────────────────────────────

fn cmd_start(
    project_root: &Path,
    foreground: bool,
    port: Option<u16>,
    json: bool,
) -> anyhow::Result<()> {
    if foreground {
        if json {
            anyhow::bail!(
                "`ta daemon start --foreground --json` is not supported: a foreground daemon \
                 holds this terminal until it exits, so there is no point at which to print a \
                 report. Drop --foreground to start in the background and get the report, or \
                 drop --json."
            );
        }
        // Foreground mode: exec the daemon binary directly, after the same
        // ownership and port checks as a background start.
        let StartPlan { port: chosen, .. } = plan_start(project_root, port, &HttpProbe)?;
        let daemon_bin = super::version_guard::find_daemon_binary()?;
        let mut cmd = Command::new(&daemon_bin);
        cmd.args(daemon_spawn_args(project_root, Some(chosen), true));

        println!(
            "Starting daemon in foreground: {} --api --project-root {}",
            daemon_bin.display(),
            project_root.display()
        );

        let status = cmd
            .status()
            .map_err(|e| anyhow::anyhow!("Cannot exec {}: {}", daemon_bin.display(), e))?;

        if !status.success() {
            return Err(anyhow::anyhow!(
                "Daemon exited with status {}",
                status.code().unwrap_or(-1)
            ));
        }
        return Ok(());
    }

    let pid = start(project_root, port)?;
    let effective_port =
        read_pid_port(project_root).unwrap_or_else(|| daemon_toml_bind_port(project_root).1);

    if !json {
        println!("Daemon started.");
        println!("  PID:  {}", pid);
        println!("  Port: {}", effective_port);
        println!("  Log:  {}", log_path(project_root).display());
        println!();
        println!("Use `ta daemon status` to check health, `ta daemon stop` to shut down.");
    }

    // Wait briefly for startup and report health. A daemon waiting for a
    // Keychain approval answers `starting` with the reason; show it.
    let base_url = resolve_daemon_url(project_root, port);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;
    let last = poll_health(&client, &base_url, 10, true);
    if json {
        return print_report(&launch_report(
            project_root,
            DaemonAction::Start,
            port,
            pid,
            None,
            last.as_ref(),
        ));
    }
    print_health_after_start(last.as_ref(), &base_url);
    Ok(())
}

fn cmd_stop(project_root: &Path) -> anyhow::Result<()> {
    let pid = read_pid(project_root);
    stop(project_root)?;

    if let Some(p) = pid {
        println!("Daemon stopped (was pid {}).", p);
    } else {
        println!("Daemon stopped.");
    }

    Ok(())
}

fn cmd_restart(
    project_root: &Path,
    port: Option<u16>,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let old_pid = read_pid(project_root);

    // Refuse before announcing or draining anything when the port belongs to
    // another project.
    ensure_port_is_ours("restart the daemon", project_root, port, &HttpProbe)?;
    let (_, target_port) = resolve_daemon_addr(project_root, port);
    eprintln!(
        "Restarting the daemon for project {} (port {}{}).",
        project_root.display(),
        target_port,
        old_pid.map(|p| format!(", pid {p}")).unwrap_or_default()
    );

    let new_pid = if force {
        // --force: skip drain and SIGKILL immediately.
        eprintln!("Force restart requested — skipping graceful drain.");
        force_restart(project_root, port)?
    } else {
        // Graceful restart: check if daemon is responding, drain if so.
        let base_url = resolve_daemon_url(project_root, port);
        let health_url = format!("{}/health", base_url);
        let reachable = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .ok()
            .and_then(|c| c.get(&health_url).send().ok())
            .is_some();

        if reachable {
            drain_and_poll(project_root, port)?;
        }
        restart(project_root, port)?
    };

    let effective_port =
        read_pid_port(project_root).unwrap_or_else(|| daemon_toml_bind_port(project_root).1);

    if !json {
        if let Some(old) = old_pid {
            println!("Daemon restarted (was pid {}, now pid {}).", old, new_pid);
        } else {
            println!("Daemon started (pid {}).", new_pid);
        }

        println!("  Port: {}", effective_port);
        println!("  Log:  {}", log_path(project_root).display());
    }

    // Wait for health. A daemon waiting for a Keychain approval answers
    // `starting` with the reason; show it with the next step.
    let base_url = resolve_daemon_url(project_root, port);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;
    let last = poll_health(&client, &base_url, 20, true);
    if json {
        return print_report(&launch_report(
            project_root,
            DaemonAction::Restart,
            port,
            new_pid,
            old_pid,
            last.as_ref(),
        ));
    }
    print_health_after_start(last.as_ref(), &base_url);
    Ok(())
}

/// What `ta daemon status` found. The probe does the checking (and removes a
/// stale pid file); `print_status` renders it as text and `status_report`
/// as the `--json` document.
enum StatusProbe {
    /// The pid file names a live process and `/api/status` answered.
    Running {
        pid: u32,
        body: serde_json::Value,
        /// Open MCP/shell agent sessions (from `/api/drain/status`).
        open_sessions: u64,
    },
    /// The pid file names a live process but nothing answers on the URL.
    Unresponsive { pid: u32 },
    /// The pid file named a dead process; it was removed.
    StalePid { pid: u32 },
    /// No pid file, and the port answers with another project's daemon.
    OtherProject {
        reported_root: String,
        pid: Option<u64>,
    },
    /// No pid file, but this project's daemon answers.
    External { body: serde_json::Value },
    /// No daemon for this project.
    NotRunning,
}

fn probe_status(project_root: &Path, base_url: &str) -> anyhow::Result<StatusProbe> {
    let pid = read_pid(project_root);
    match pid {
        Some(p) if is_process_alive(p) => {
            // Daemon PID is alive — query for details.
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()?;
            let status_url = format!("{}/api/status", base_url);
            match client.get(&status_url).send() {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json()?;
                    // Open agent sessions (MCP/shell connections).
                    let drain_url = format!("{}/api/drain/status", base_url);
                    let open_sessions = client
                        .get(&drain_url)
                        .send()
                        .ok()
                        .and_then(|dr| dr.json::<serde_json::Value>().ok())
                        .and_then(|dj| dj["active_sessions"].as_u64())
                        .unwrap_or(0);
                    Ok(StatusProbe::Running {
                        pid: p,
                        body,
                        open_sessions,
                    })
                }
                _ => Ok(StatusProbe::Unresponsive { pid: p }),
            }
        }
        Some(p) => {
            // PID exists but process is not alive — stale.
            remove_pid_file(project_root);
            Ok(StatusProbe::StalePid { pid: p })
        }
        None => {
            // No PID file. Check if daemon is reachable anyway (started externally).
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()?;
            let status_url = format!("{}/api/status", base_url);
            if let Ok(resp) = client.get(&status_url).send() {
                if resp.status().is_success() {
                    let body: serde_json::Value = resp.json()?;
                    if let Some(other) = body["project_root"]
                        .as_str()
                        .filter(|r| !daemon_port::same_project_root(project_root, r))
                    {
                        return Ok(StatusProbe::OtherProject {
                            reported_root: other.to_string(),
                            pid: body["pid"].as_u64(),
                        });
                    }
                    return Ok(StatusProbe::External { body });
                }
            }
            Ok(StatusProbe::NotRunning)
        }
    }
}

fn cmd_status(project_root: &Path, json: bool) -> anyhow::Result<()> {
    let base_url = resolve_daemon_url(project_root, None);
    let probe = probe_status(project_root, &base_url)?;
    if json {
        return print_report(&status_report(project_root, &probe));
    }
    print_status(project_root, &base_url, &probe);
    Ok(())
}

fn print_status(project_root: &Path, base_url: &str, probe: &StatusProbe) {
    match probe {
        StatusProbe::Running {
            pid: p,
            body: json,
            open_sessions,
        } => {
            let version = json["version"].as_str().unwrap_or("?");
            let project = json["project"].as_str().unwrap_or("?");
            let active_agents = json["active_agents"].as_array();
            let active_count = active_agents.map(|a| a.len()).unwrap_or(0);
            let active_goals = json["active_goals"].as_u64().unwrap_or(0);
            let pending_drafts = json["pending_drafts"].as_u64().unwrap_or(0);

            if daemon_state(json) == "starting" {
                println!("Daemon is running but still starting.");
                println!("  PID:            {}", p);
                println!("  URL:            {}", base_url);
                println!("  Version:        {}", version);
                for line in startup_report_lines(json) {
                    println!("{line}");
                }
                println!("  Log:            {}", log_path(project_root).display());
                return;
            }
            println!("Daemon is running.");
            println!("  PID:            {}", p);
            println!("  URL:            {}", base_url);
            println!("  Version:        {}", version);
            println!("  Project:        {}", project);
            println!("  Project root:   {}", project_root.display());
            if let Some(reported) = json["project_root"].as_str() {
                if !daemon_port::same_project_root(project_root, reported) {
                    println!(
                        "  WARNING:        the daemon on {} serves {}, not this project. \
                         Run `ta daemon status` there; `ta daemon stop` and `restart` \
                         here refuse to touch it.",
                        base_url, reported
                    );
                }
            }
            println!("  Active goals:   {}", active_goals);
            println!("  Active agents:  {}", active_count);
            println!("  Pending drafts: {}", pending_drafts);
            println!("  Log:            {}", log_path(project_root).display());
            for line in startup_report_lines(json) {
                println!("{line}");
            }
            if let Some(line) = super::status::outstanding_update_message(json) {
                println!("  Update:         {}", line);
            }

            // Show per-goal details when there are active goals.
            if let Some(agents) = active_agents {
                if !agents.is_empty() {
                    println!();
                    println!("  Active goal runs:");
                    for a in agents {
                        let title = a["title"].as_str().unwrap_or("?");
                        let state = a["state"].as_str().unwrap_or("?");
                        let secs = a["running_secs"].as_i64().unwrap_or(0);
                        let goal_id = a["goal_id"].as_str().unwrap_or("?");
                        let pid_str = a["agent_pid"]
                            .as_u64()
                            .map(|p| format!(" pid={}", p))
                            .unwrap_or_default();
                        println!(
                            "    [{}] {} — {} ({}s{})",
                            &goal_id[..8.min(goal_id.len())],
                            title,
                            state,
                            secs,
                            pid_str,
                        );
                    }
                }
            }

            if *open_sessions > 0 {
                println!();
                println!("  Open agent sessions (MCP/shell): {}", open_sessions);
            }
        }
        StatusProbe::Unresponsive { pid: p } => {
            println!(
                "Daemon process is alive (pid {}) but not responding on {}.",
                p, base_url
            );
            println!("  It may still be starting up. Check logs:");
            println!("    ta daemon log");
            println!("  Or force kill and restart:");
            println!("    kill {} && ta daemon start", p);
        }
        StatusProbe::StalePid { pid: p } => {
            println!(
                "Daemon is not running (stale PID file for pid {} removed).",
                p
            );
            println!("  Start it with: ta daemon start");
        }
        StatusProbe::OtherProject {
            reported_root: other,
            pid,
        } => {
            println!(
                "No daemon is running for this project ({}).",
                project_root.display()
            );
            println!(
                "  {} answers with the daemon for {}{}, a different project.",
                base_url,
                other,
                pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
            );
            println!(
                "  Manage that daemon with `ta daemon status` from {}.",
                other
            );
            println!("  Start this project's own daemon with: ta daemon start");
        }
        StatusProbe::External { body: json } => {
            let version = json["version"].as_str().unwrap_or("?");
            println!("Daemon is running (started externally, no PID file).");
            println!("  URL:     {}", base_url);
            println!("  Version: {}", version);
            for line in startup_report_lines(json) {
                println!("{line}");
            }
        }
        StatusProbe::NotRunning => {
            println!("Daemon is not running.");
            println!("  Start it with: ta daemon start");
        }
    }
}

// ─── Machine-readable reports (`--json`) ─────────────────────────────────────

/// A report with the pid/port contract paths filled in and nothing else known.
/// `port_override` is the `--port` the command was given.
fn blank_report(
    project_root: &Path,
    action: DaemonAction,
    port_override: Option<u16>,
) -> DaemonReport {
    let (host, port) = resolve_daemon_addr(project_root, port_override);
    DaemonReport {
        schema_version: DAEMON_REPORT_SCHEMA_VERSION,
        action,
        state: WsDaemonState::NotRunning,
        running: false,
        pid: None,
        previous_pid: None,
        port: Some(port),
        url: Some(format!("http://{host}:{port}")),
        host: Some(host),
        pid_file: pid_path(project_root).display().to_string(),
        log_file: log_path(project_root).display().to_string(),
        project_root: project_root.display().to_string(),
        version: None,
        reported_project_root: None,
        active_goals: None,
        active_agents: None,
        pending_drafts: None,
        reasons: Vec::new(),
        stale_pid_removed: false,
        detail: None,
    }
}

/// The daemon's own health word as a report state: `ok` is running, `starting`
/// stays starting, anything else is degraded.
fn state_of_body(body: &serde_json::Value) -> WsDaemonState {
    match daemon_state(body) {
        "ok" => WsDaemonState::Running,
        "starting" => WsDaemonState::Starting,
        _ => WsDaemonState::Degraded,
    }
}

/// Copy what an answering daemon reports (version, counts, reasons) into `r`.
fn fill_from_body(r: &mut DaemonReport, body: &serde_json::Value) {
    r.version = body["version"].as_str().map(str::to_string);
    r.active_goals = body["active_goals"].as_u64();
    r.active_agents = body["active_agents"].as_array().map(|a| a.len() as u64);
    r.pending_drafts = body["pending_drafts"].as_u64();
    r.reasons = body["reasons"]
        .as_array()
        .map(|reasons| {
            reasons
                .iter()
                .map(|x| DaemonReason {
                    component: x["component"].as_str().unwrap_or("daemon").to_string(),
                    reason: x["reason"]
                        .as_str()
                        .unwrap_or("no reason given")
                        .to_string(),
                    next_step: x["next_step"].as_str().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    if daemon_state(body) != "ok" && r.reasons.is_empty() {
        r.reasons.push(DaemonReason {
            component: "daemon".to_string(),
            reason: "the daemon gave no reason".to_string(),
            next_step: Some("Check the daemon log: ta daemon log".to_string()),
        });
    }
}

/// The `ta daemon status --json` document for what the probe found.
fn status_report(project_root: &Path, probe: &StatusProbe) -> DaemonReport {
    let mut r = blank_report(project_root, DaemonAction::Status, None);
    let url = r.url.clone().unwrap_or_default();
    match probe {
        StatusProbe::Running { pid, body, .. } => {
            fill_from_body(&mut r, body);
            r.pid = Some(*pid);
            r.state = state_of_body(body);
            r.running = true;
            if let Some(reported) = body["project_root"].as_str() {
                r.reported_project_root = Some(reported.to_string());
                if !daemon_port::same_project_root(project_root, reported) {
                    r.detail = Some(format!(
                        "The daemon on {url} serves {reported}, not this project. Run `ta daemon \
                         status` there; `ta daemon stop` and `restart` here refuse to touch it."
                    ));
                }
            }
        }
        StatusProbe::Unresponsive { pid } => {
            r.state = WsDaemonState::Unresponsive;
            r.pid = Some(*pid);
            r.detail = Some(format!(
                "Process {pid} is alive but nothing answers on {url}. It may still be starting: \
                 check `ta daemon log`, or force it with `kill {pid} && ta daemon start`."
            ));
        }
        StatusProbe::StalePid { pid } => {
            r.state = WsDaemonState::StalePid;
            r.stale_pid_removed = true;
            r.detail = Some(format!(
                "{} named pid {pid}, which is not running; the stale file was removed. Start the \
                 daemon with: ta daemon start",
                r.pid_file
            ));
        }
        StatusProbe::OtherProject { reported_root, pid } => {
            r.state = WsDaemonState::OtherProject;
            r.reported_project_root = Some(reported_root.clone());
            r.detail = Some(format!(
                "No daemon is running for this project. {url} answers with the daemon for \
                 {reported_root}{}, a different project. Manage it with `ta daemon status` from \
                 there; start this project's own daemon with: ta daemon start",
                pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
            ));
        }
        StatusProbe::External { body } => {
            fill_from_body(&mut r, body);
            r.state = WsDaemonState::External;
            r.running = true;
            r.reported_project_root = body["project_root"].as_str().map(str::to_string);
            r.detail = Some(format!(
                "A daemon for this project answers on {url} but wrote no {}; it was started \
                 outside `ta daemon start`.",
                r.pid_file
            ));
        }
        StatusProbe::NotRunning => {
            r.detail = Some("No daemon is running. Start it with: ta daemon start".to_string());
        }
    }
    r
}

/// The `start`/`restart --json` document: the pid `start()` returned, and what
/// `/health` said afterwards (`None` when nothing answered yet).
fn launch_report(
    project_root: &Path,
    action: DaemonAction,
    port_override: Option<u16>,
    pid: u32,
    previous_pid: Option<u32>,
    health: Option<&serde_json::Value>,
) -> DaemonReport {
    let mut r = blank_report(project_root, action, port_override);
    r.pid = Some(pid);
    r.previous_pid = previous_pid;
    r.running = true;
    match health {
        Some(body) => {
            fill_from_body(&mut r, body);
            r.state = state_of_body(body);
            if r.state != WsDaemonState::Running {
                r.detail = Some("Check progress with: ta daemon status".to_string());
            }
        }
        None => {
            r.state = WsDaemonState::Starting;
            r.detail = Some(format!(
                "The daemon (pid {pid}) has not answered on {} yet. Check the log: ta daemon \
                 log, then ta daemon status.",
                r.url.clone().unwrap_or_default()
            ));
        }
    }
    r
}

/// Print one report as the command's only stdout.
fn print_report(report: &DaemonReport) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}

fn cmd_log(
    project_root: &Path,
    lines: usize,
    follow: bool,
    level: Option<&str>,
    component: Option<&str>,
    goal: Option<&str>,
) -> anyhow::Result<()> {
    let log = log_path(project_root);

    if !log.exists() {
        return Err(anyhow::anyhow!(
            "No daemon log found at {}. Start the daemon first: ta daemon start",
            log.display()
        ));
    }

    let has_filters = level.is_some() || component.is_some() || goal.is_some();

    if follow {
        // Live tail — open the file, seek to the last N lines, then poll for new content.
        let file = std::fs::File::open(&log)
            .map_err(|e| anyhow::anyhow!("Cannot open {}: {}", log.display(), e))?;
        let reader = std::io::BufReader::new(&file);
        let all_lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

        // Apply filters, then take last N.
        let filtered: Vec<&str> = if has_filters {
            all_lines
                .iter()
                .filter(|l| matches_log_filters(l, level, component, goal))
                .map(String::as_str)
                .collect()
        } else {
            all_lines.iter().map(String::as_str).collect()
        };

        let start = filtered.len().saturating_sub(lines);
        for line in &filtered[start..] {
            println!("{}", line);
        }

        // Now follow using polling.
        let mut last_len = std::fs::metadata(&log)?.len();
        let stdout = std::io::stdout();

        if has_filters {
            println!(
                "--- following {} with filters (Ctrl-C to stop) ---",
                log.display()
            );
        } else {
            println!("--- following {} (Ctrl-C to stop) ---", log.display());
        }

        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let current_len = match std::fs::metadata(&log) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            if current_len > last_len {
                let mut f = std::fs::File::open(&log)?;
                std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(last_len))?;
                let reader = std::io::BufReader::new(f);
                let mut handle = stdout.lock();
                for l in reader.lines().map_while(Result::ok) {
                    if !has_filters || matches_log_filters(&l, level, component, goal) {
                        writeln!(handle, "{}", l)?;
                    }
                }
                last_len = current_len;
            } else if current_len < last_len {
                // Log was truncated/rotated — reset.
                last_len = 0;
            }
        }
    } else {
        // Simple tail — read last N lines.
        let content = std::fs::read_to_string(&log)
            .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", log.display(), e))?;

        let all_lines: Vec<&str> = content.lines().collect();

        // Apply filters first, then take last N.
        let filtered: Vec<&str> = if has_filters {
            all_lines
                .iter()
                .filter(|l| matches_log_filters(l, level, component, goal))
                .copied()
                .collect()
        } else {
            all_lines.clone()
        };

        let start = filtered.len().saturating_sub(lines);
        for line in &filtered[start..] {
            println!("{}", line);
        }

        let filter_note = if has_filters { " (filtered)" } else { "" };
        println!(
            "\n({} of {} lines shown{}. Use `--follow` for live tail.)",
            filtered.len() - start,
            all_lines.len(),
            filter_note,
        );
    }

    Ok(())
}

/// Check if a log line matches the given filters.
///
/// Daemon logs use tracing's default format:
///   `2026-03-15T10:30:00.123Z  INFO component: message goal_id=abc123`
///
/// Filters match by case-insensitive substring:
/// - `level`: matches the level token (ERROR, WARN, INFO, DEBUG, TRACE)
/// - `component`: matches anywhere in the line (component name, module path)
/// - `goal`: matches goal IDs anywhere in the line
fn matches_log_filters(
    line: &str,
    level: Option<&str>,
    component: Option<&str>,
    goal: Option<&str>,
) -> bool {
    let line_lower = line.to_lowercase();

    if let Some(lvl) = level {
        let lvl_lower = lvl.to_lowercase();
        // Match common tracing log level tokens.
        if !line_lower.contains(&lvl_lower) {
            return false;
        }
    }

    if let Some(comp) = component {
        if !line_lower.contains(&comp.to_lowercase()) {
            return false;
        }
    }

    if let Some(goal_id) = goal {
        if !line_lower.contains(&goal_id.to_lowercase()) {
            return false;
        }
    }

    true
}

fn cmd_health(project_root: &Path) -> anyhow::Result<()> {
    println!("Daemon Health Check");
    println!("═══════════════════════════════════════════");

    let mut pass = 0u32;
    let mut warn = 0u32;
    let mut fail = 0u32;

    // 1. PID and process
    print!("  Process... ");
    match read_pid(project_root) {
        Some(pid) if is_process_alive(pid) => {
            println!("ok (pid {})", pid);
            pass += 1;
        }
        Some(pid) => {
            println!("DEAD (pid {} not alive, stale PID file)", pid);
            fail += 1;
        }
        None => {
            println!("no PID file");
            fail += 1;
        }
    }

    // 2. API responsiveness
    print!("  API... ");
    let base_url = resolve_daemon_url(project_root, None);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;

    let start = std::time::Instant::now();
    match client.get(format!("{}/api/status", base_url)).send() {
        Ok(resp) if resp.status().is_success() => {
            let latency = start.elapsed();
            println!("ok ({}ms latency)", latency.as_millis());
            pass += 1;
        }
        Ok(resp) => {
            println!("ERROR (status {})", resp.status());
            fail += 1;
        }
        Err(e) => {
            println!("UNREACHABLE ({})", e);
            fail += 1;
        }
    };

    // 3. Event system (check events directory and recent activity)
    print!("  Event system... ");
    let events_dir = project_root.join(".ta/events");
    let events_file = events_dir.join("events.jsonl");
    if events_file.exists() {
        let meta = std::fs::metadata(&events_file);
        match meta {
            Ok(m) => {
                let size = m.len();
                println!("ok ({} events file, {} bytes)", events_file.display(), size);
                pass += 1;
            }
            Err(_) => {
                println!("warning (cannot read events file)");
                warn += 1;
            }
        }
    } else if events_dir.exists() {
        println!("ok (events directory exists, no events yet)");
        pass += 1;
    } else {
        println!("warning (events directory missing — will be created on first event)");
        warn += 1;
    }

    // 4. Plugin status
    print!("  Plugins... ");
    let plugins = ta_changeset::plugin::discover_plugins(project_root);
    if plugins.is_empty() {
        println!("none installed");
        pass += 1;
    } else {
        let mut plugin_ok = true;
        for p in &plugins {
            // Check if command binary exists for stdio plugins
            if p.manifest.protocol == ta_changeset::plugin::PluginProtocol::JsonStdio {
                if let Some(ref cmd) = p.manifest.command {
                    let bin_path = p.plugin_dir.join(cmd);
                    if !bin_path.exists() {
                        println!(
                            "WARNING ({} binary missing: {})",
                            p.manifest.name,
                            bin_path.display()
                        );
                        warn += 1;
                        plugin_ok = false;
                    }
                }
            }
        }
        if plugin_ok {
            println!("ok ({} plugin(s) installed)", plugins.len());
            pass += 1;
        }
    }

    // 5. Disk space
    print!("  Disk space... ");
    let output = std::process::Command::new("df")
        .args(["-k", &project_root.display().to_string()])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = stdout.lines().nth(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 4 {
                    if let Ok(kb) = fields[3].parse::<u64>() {
                        let bytes = kb * 1024;
                        let threshold = 2 * 1024 * 1024 * 1024u64; // 2 GB
                        if bytes >= threshold {
                            println!("ok ({:.1} GB available)", bytes as f64 / 1_073_741_824.0);
                            pass += 1;
                        } else {
                            println!(
                                "LOW ({:.1} GB available, recommend >2 GB)",
                                bytes as f64 / 1_073_741_824.0
                            );
                            warn += 1;
                        }
                    } else {
                        println!("unknown");
                        warn += 1;
                    }
                } else {
                    println!("unknown");
                    warn += 1;
                }
            } else {
                println!("unknown");
                warn += 1;
            }
        }
        _ => {
            println!("unknown");
            warn += 1;
        }
    }

    // 6. Goal process liveness
    print!("  Goal processes... ");
    let goals_dir = project_root.join(".ta/goals");
    if goals_dir.exists() {
        match ta_goal::GoalRunStore::new(&goals_dir) {
            Ok(store) => {
                let goals = store.list().unwrap_or_default();
                let running: Vec<_> = goals
                    .iter()
                    .filter(|g| g.state == ta_goal::GoalRunState::Running)
                    .collect();

                if running.is_empty() {
                    println!("ok (no active goals)");
                    pass += 1;
                } else {
                    let zombies: Vec<_> = running
                        .iter()
                        .filter(|g| {
                            g.agent_pid
                                .map(|pid| !is_process_alive(pid))
                                .unwrap_or(false)
                        })
                        .collect();

                    if zombies.is_empty() {
                        println!("ok ({} running, all alive)", running.len());
                        pass += 1;
                    } else {
                        println!(
                            "WARNING ({} zombie(s) of {} running)",
                            zombies.len(),
                            running.len()
                        );
                        for z in &zombies {
                            println!(
                                "    {} \"{}\" (pid {} dead)",
                                &z.goal_run_id.to_string()[..8],
                                z.title,
                                z.agent_pid.unwrap_or(0)
                            );
                        }
                        warn += 1;
                    }
                }
            }
            Err(_) => {
                println!("ok (no goal store)");
                pass += 1;
            }
        }
    } else {
        println!("ok (no goals directory)");
        pass += 1;
    }

    // 7. Log file
    print!("  Log file... ");
    let log = log_path(project_root);
    if log.exists() {
        let meta = std::fs::metadata(&log)?;
        let size = meta.len();
        if size > 100 * 1024 * 1024 {
            // 100 MB
            println!("WARNING ({}MB — consider rotation)", size / 1_048_576);
            warn += 1;
        } else {
            println!("ok ({:.1} MB)", size as f64 / 1_048_576.0);
            pass += 1;
        }
    } else {
        println!("not found (daemon may not have been started yet)");
        warn += 1;
    }

    // Summary
    println!();
    if fail == 0 && warn == 0 {
        println!("All checks passed ({}/{}). Daemon is healthy.", pass, pass);
    } else {
        println!("{} passed, {} warnings, {} failures", pass, warn, fail);
    }

    if fail > 0 {
        Err(anyhow::anyhow!("{} daemon health check(s) failed", fail))
    } else {
        Ok(())
    }
}

/// Generate (and optionally install) the platform launch unit for auto-start.
///
/// macOS: `~/Library/LaunchAgents/com.trustedautonomy.daemon.plist`
///   Loaded via `launchctl load` when `--apply` is set.
///
/// Linux: `~/.config/systemd/user/ta-daemon.service`
///   Enabled via `systemctl --user enable --now` when `--apply` is set.
///
/// Windows: not yet supported (prints a warning).
fn cmd_install(project_root: &Path, apply: bool) -> anyhow::Result<()> {
    let daemon_bin = super::version_guard::find_daemon_binary().unwrap_or_else(|_| {
        // Fallback: assume installed in PATH as ta-daemon.
        std::path::PathBuf::from("ta-daemon")
    });

    #[cfg(target_os = "macos")]
    {
        let label = "com.trustedautonomy.daemon";
        let plist_path = dirs_home()
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label));

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>--api</string>
        <string>--project-root</string>
        <string>{root}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{root}/.ta/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{root}/.ta/daemon.log</string>
</dict>
</plist>
"#,
            label = label,
            bin = daemon_bin.display(),
            root = project_root.display(),
        );

        println!("LaunchAgent plist:");
        println!("{}", plist);
        println!("Install path: {}", plist_path.display());

        if apply {
            if let Some(parent) = plist_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&plist_path, &plist).map_err(|e| {
                anyhow::anyhow!("Cannot write plist to {}: {}", plist_path.display(), e)
            })?;
            println!("Written to {}", plist_path.display());

            let status = Command::new("launchctl")
                .args(["load", "-w", &plist_path.to_string_lossy()])
                .status()
                .map_err(|e| anyhow::anyhow!("Cannot run launchctl: {}", e))?;

            if status.success() {
                println!("LaunchAgent loaded. Daemon will start automatically on login.");
                println!("To remove: launchctl unload -w {}", plist_path.display());
            } else {
                return Err(anyhow::anyhow!(
                    "launchctl load failed (exit {}). Check the plist path and try manually:\n  launchctl load -w {}",
                    status.code().unwrap_or(-1),
                    plist_path.display()
                ));
            }
        } else {
            println!("Run `ta daemon install --apply` to write and load this LaunchAgent.");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let service_path = dirs_home().join(".config/systemd/user/ta-daemon.service");

        let service = format!(
            r#"[Unit]
Description=Trusted Autonomy daemon
After=network.target

[Service]
ExecStart={bin} --api --project-root {root}
Restart=on-failure
StandardOutput=append:{root}/.ta/daemon.log
StandardError=append:{root}/.ta/daemon.log

[Install]
WantedBy=default.target
"#,
            bin = daemon_bin.display(),
            root = project_root.display(),
        );

        println!("systemd user service unit:");
        println!("{}", service);
        println!("Install path: {}", service_path.display());

        if apply {
            if let Some(parent) = service_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&service_path, &service).map_err(|e| {
                anyhow::anyhow!(
                    "Cannot write service file to {}: {}",
                    service_path.display(),
                    e
                )
            })?;
            println!("Written to {}", service_path.display());

            // Reload systemd and enable the service.
            let reload = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status()
                .map_err(|e| anyhow::anyhow!("Cannot run systemctl: {}", e))?;
            if !reload.success() {
                return Err(anyhow::anyhow!("systemctl --user daemon-reload failed"));
            }

            let enable = Command::new("systemctl")
                .args(["--user", "enable", "--now", "ta-daemon.service"])
                .status()
                .map_err(|e| anyhow::anyhow!("Cannot run systemctl: {}", e))?;

            if enable.success() {
                println!("systemd service enabled and started.");
                println!("To remove: systemctl --user disable --now ta-daemon.service");
            } else {
                return Err(anyhow::anyhow!(
                    "systemctl enable failed (exit {}). Try:\n  systemctl --user enable --now ta-daemon.service",
                    enable.code().unwrap_or(-1),
                ));
            }
        } else {
            println!("Run `ta daemon install --apply` to write and enable this service unit.");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Suppress unused-variable warnings: project_root and apply are
        // used only in the macOS/Linux cfg branches above.
        let _ = (project_root, apply);
        println!(
            "ta daemon install is not yet supported on this platform.\n\
             On Windows, add ta-daemon to Task Scheduler manually.\n\
             Binary path: {}",
            daemon_bin.display()
        );
    }

    Ok(())
}

/// Returns the current user's home directory.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

// ── ta daemon config show (v0.17.0.8) ────────────────────────────

fn cmd_config_show(project_root: &Path) -> anyhow::Result<()> {
    let daemon_toml = project_root.join(".ta").join("daemon.toml");
    let base_url = resolve_daemon_url(project_root, None);

    // Resolve the effective agent that `ta run` will use without --agent.
    let effective_agent = super::run::resolve_effective_agent(None, None, project_root);

    println!("Daemon configuration");
    println!("  Config file:    {}", daemon_toml.display());
    println!(
        "  Config exists:  {}",
        if daemon_toml.exists() {
            "yes"
        } else {
            "no (using defaults)"
        }
    );
    println!("  Daemon URL:     {}", base_url);
    println!();
    println!("Effective agent (what `ta run` uses without --agent):");
    println!("  Effective agent: {}", effective_agent);

    if daemon_toml.exists() {
        // Show which source provided the agent.
        let content = std::fs::read_to_string(&daemon_toml).unwrap_or_default();
        if content.contains("default =") {
            println!("  Source:          daemon.toml [agent].default");
        } else if content.contains("default_framework") {
            println!("  Source:          daemon.toml [agent].default_framework (legacy)");
        } else {
            println!("  Source:          built-in default");
        }
    } else {
        println!("  Source:          built-in default (no daemon.toml)");
    }

    println!();
    println!("To change the default agent, add to .ta/daemon.toml:");
    println!("  [agent]");
    println!("  default = \"codex\"  # or: claude-code, claude-flow, ...");

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::daemon_ownership::test_support::{CannedProbe, FakeDaemon};
    use crate::commands::daemon_ownership::ProbeAnswer;

    fn project_with_port(port: Option<u16>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        if let Some(p) = port {
            std::fs::write(
                d.path().join(".ta/daemon.toml"),
                format!("[server]\nport = {p}\n"),
            )
            .unwrap();
        }
        d
    }

    fn no_binary() -> anyhow::Result<std::path::PathBuf> {
        Err(anyhow::anyhow!("ta-daemon lookup disabled in this test"))
    }

    /// Project A's daemon (a real local listener) holds the port that project
    /// B is configured for, as in the live incident.
    fn two_projects_one_port() -> (tempfile::TempDir, tempfile::TempDir, FakeDaemon) {
        let a = project_with_port(None);
        let canon_a = std::fs::canonicalize(a.path()).unwrap();
        let fake = FakeDaemon::spawn(
            Some(&canon_a.display().to_string()),
            31337,
            &[("c5ee0b0a-0000-0000-0000-000000000000", "Owner's long goal")],
        );
        let b = project_with_port(Some(fake.port));
        (a, b, fake)
    }

    fn assert_refusal_names_everything(err: &str, a: &Path, b: &Path, port: u16) {
        assert!(err.contains(&format!("port {port}")), "{err}");
        // The other project's root is reported by its daemon in canonical form (on
        // Windows `\\?\C:\Users\runneradmin\...`, not the 8.3 `RUNNER~1` spelling the
        // temp dir hands out), so compare against the canonical spelling.
        let canon_a = std::fs::canonicalize(a).unwrap();
        assert!(err.contains(&canon_a.display().to_string()), "{err}");
        assert!(err.contains(&b.display().to_string()), "{err}");
        assert!(err.contains("pid 31337"), "{err}");
        assert!(err.contains("ta daemon status"), "{err}");
        assert!(err.contains("Nothing was stopped"), "{err}");
    }

    #[test]
    fn second_project_on_a_busy_port_refuses_to_stop_the_first_projects_daemon() {
        let (a, b, fake) = two_projects_one_port();
        let err = stop_with(b.path(), &HttpProbe).unwrap_err().to_string();
        assert_refusal_names_everything(&err, a.path(), b.path(), fake.port);
        assert!(
            !fake.saw("POST"),
            "no write request may reach the other daemon"
        );
        assert!(!fake.saw("shutdown"));
    }

    #[test]
    fn second_project_on_a_busy_port_refuses_to_restart_the_first_projects_daemon() {
        let (a, b, fake) = two_projects_one_port();
        let started = std::cell::Cell::new(false);
        let err = restart_with(b.path(), None, &HttpProbe, &|| {
            started.set(true);
            Ok(1)
        })
        .unwrap_err()
        .to_string();
        assert_refusal_names_everything(&err, a.path(), b.path(), fake.port);
        assert!(!started.get(), "the start step must not run");
        assert!(!fake.saw("shutdown") && !fake.saw("POST"));

        // The CLI commands refuse the same way, before draining anything.
        let err = cmd_restart(b.path(), None, false, false)
            .unwrap_err()
            .to_string();
        assert_refusal_names_everything(&err, a.path(), b.path(), fake.port);
        let err = cmd_restart(b.path(), None, true, false)
            .unwrap_err()
            .to_string();
        assert_refusal_names_everything(&err, a.path(), b.path(), fake.port);
        let err = cmd_stop(b.path()).unwrap_err().to_string();
        assert_refusal_names_everything(&err, a.path(), b.path(), fake.port);
        assert!(
            !fake.saw("drain"),
            "must not even poll the other daemon's goals"
        );
        assert!(!fake.saw("POST") && !fake.saw("shutdown"));
    }

    #[test]
    fn second_project_on_a_busy_port_refuses_to_start_with_the_same_clarity() {
        let (a, b, fake) = two_projects_one_port();
        let err = start_with_binary(b.path(), None, &no_binary, &HttpProbe)
            .unwrap_err()
            .to_string();
        assert_refusal_names_everything(&err, a.path(), b.path(), fake.port);
        // An explicit --port that lands on the other daemon refuses too.
        let c = project_with_port(None);
        let err = start_with_binary(c.path(), Some(fake.port), &no_binary, &HttpProbe)
            .unwrap_err()
            .to_string();
        assert_refusal_names_everything(&err, a.path(), c.path(), fake.port);
        assert!(!fake.saw("POST"));
    }

    #[test]
    fn a_projects_own_daemon_on_its_port_is_not_refused() {
        let a = project_with_port(None);
        let canon_a = std::fs::canonicalize(a.path()).unwrap();
        let fake = FakeDaemon::spawn(Some(&canon_a.display().to_string()), 1, &[]);
        std::fs::write(
            a.path().join(".ta/daemon.toml"),
            format!("[server]\nport = {}\n", fake.port),
        )
        .unwrap();
        let holder = ensure_port_is_ours("stop the daemon", a.path(), None, &HttpProbe).unwrap();
        assert!(
            matches!(holder, PortHolder::ThisProject { .. }),
            "{holder:?}"
        );
        // start() says it is already running rather than starting a second one.
        let err = start_with_binary(a.path(), None, &no_binary, &HttpProbe)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already responding"), "{err}");
    }

    #[test]
    fn start_picks_a_free_derived_port_and_records_it() {
        let d = project_with_port(None);
        let _ = start_with_binary(
            d.path(),
            None,
            &no_binary,
            &CannedProbe(ProbeAnswer::NoAnswer),
        )
        .unwrap_err();
        let recorded = daemon_port::explicit_server_port(d.path())
            .unwrap()
            .expect("the chosen port is recorded in .ta/daemon.toml");
        assert!((7700..=7799).contains(&recorded), "{recorded}");
        // Every client resolves the same port from then on.
        assert_eq!(
            resolve_daemon_url(d.path(), None),
            format!("http://127.0.0.1:{recorded}")
        );
        assert_eq!(daemon_port::default_port_for(d.path()).unwrap(), recorded);
        // Starting again keeps the recorded choice.
        let _ = start_with_binary(
            d.path(),
            None,
            &no_binary,
            &CannedProbe(ProbeAnswer::NoAnswer),
        )
        .unwrap_err();
        assert_eq!(
            daemon_port::explicit_server_port(d.path()).unwrap(),
            Some(recorded)
        );
    }

    #[test]
    fn busy_derived_port_is_skipped_not_shared() {
        let d = project_with_port(None);
        let derived = daemon_port::default_port_for(d.path()).unwrap();
        // Hold the derived port with a plain listener (any process).
        let hold = std::net::TcpListener::bind(("127.0.0.1", derived));
        let _ = start_with_binary(
            d.path(),
            None,
            &no_binary,
            &CannedProbe(ProbeAnswer::NoAnswer),
        )
        .unwrap_err();
        let recorded = daemon_port::explicit_server_port(d.path())
            .unwrap()
            .unwrap();
        if hold.is_ok() {
            assert_ne!(recorded, derived, "must not pick the busy port");
        }
    }

    #[test]
    fn explicit_port_held_by_a_non_daemon_process_fails_with_the_fix() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let d = project_with_port(Some(port));
        let err = start_with_binary(
            d.path(),
            None,
            &no_binary,
            &CannedProbe(ProbeAnswer::NoAnswer),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(&format!("port {port}")), "{err}");
        assert!(err.contains("lsof -i"), "{err}");
        assert!(err.contains("[server] port"), "{err}");
        assert!(err.contains(&d.path().display().to_string()), "{err}");
        // An explicit port is never rewritten.
        assert_eq!(
            daemon_port::explicit_server_port(d.path()).unwrap(),
            Some(port)
        );
    }

    /// Every client resolves the daemon port through `resolve_daemon_url` /
    /// `ta_mcp_gateway::daemon_port`. A literal 7700 in command code is the
    /// shared-default bug coming back.
    #[test]
    fn no_cli_command_hardcodes_the_shared_default_port() {
        fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    rust_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        rust_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        assert!(files.len() > 20, "found only {} source files", files.len());
        let mut offenders = Vec::new();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with("tests.rs") {
                continue;
            }
            // Normalize CRLF so a Windows checkout scans the same text.
            let text = std::fs::read_to_string(&f).unwrap().replace("\r\n", "\n");
            let production = text.split("\n#[cfg(test)]\nmod ").next().unwrap();
            for (n, line) in production.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("7700") {
                    offenders.push(format!("{}:{}: {}", f.display(), n + 1, code));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "hardcoded 7700 in command code (resolve through daemon::resolve_daemon_url or \
             ta_mcp_gateway::daemon_port instead):\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn waiting_message_names_goal_id_title_project_and_how_to_look() {
        let a = project_with_port(None);
        let canon = std::fs::canonicalize(a.path()).unwrap();
        let fake = FakeDaemon::spawn(
            Some(&canon.display().to_string()),
            9,
            &[("c5ee0b0a-0000-0000-0000-000000000000", "Owner's long goal")],
        );
        let source = HttpDrainSource {
            client: reqwest::blocking::Client::new(),
            url: format!("{}/api/drain/status", fake.url()),
        };
        use ta_lifecycle::DrainSource;
        let snap = source.drain_status().unwrap().unwrap();
        let line = waiting_message(&canon.display().to_string(), &snap, 28);
        assert!(line.contains(&canon.display().to_string()), "{line}");
        assert!(line.contains("c5ee0b0a"), "{line}");
        assert!(line.contains("Owner's long goal"), "{line}");
        assert!(line.contains("ta goal status c5ee0b0a"), "{line}");
        assert!(line.contains("28s remaining"), "{line}");
        assert!(line.contains("--force"), "{line}");
        assert!(!line.contains("Waiting: 1 running goal ("), "{line}");
    }

    /// Public-install safety: when the named local signing identity does not
    /// exist (every end user's machine), `ensure_stable_codesign` must leave
    /// the binary byte-for-byte untouched. No ad-hoc re-signing.
    #[cfg(target_os = "macos")]
    #[test]
    fn ensure_stable_codesign_leaves_binary_untouched_without_the_local_identity() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-ta");
        std::fs::copy("/usr/bin/true", &bin).unwrap();
        let before = std::fs::read(&bin).unwrap();
        std::env::set_var("TA_CODESIGN_IDENTITY", "ta-test-nonexistent-identity-xyz");
        ensure_stable_codesign(&bin, dir.path());
        std::env::remove_var("TA_CODESIGN_IDENTITY");
        assert_eq!(
            std::fs::read(&bin).unwrap(),
            before,
            "binary was modified even though the signing identity does not exist"
        );
    }

    #[test]
    fn pid_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".ta")).unwrap();

        assert!(read_pid(project).is_none());

        write_pid_file(project, 12345, 7700).unwrap();
        assert_eq!(read_pid(project), Some(12345));
        assert_eq!(read_pid_port(project), Some(7700));

        remove_pid_file(project);
        assert!(read_pid(project).is_none());
    }

    #[test]
    fn start_forwards_port_as_the_api_port_not_web_port() {
        // Regression: `--port N` used to be forwarded as `--web-port N`, so
        // with `[server] port = N` in daemon.toml the API and the legacy web
        // UI both bound N and the daemon died with "Address already in use".
        let dir = tempfile::tempdir().unwrap();
        let args = daemon_spawn_args(dir.path(), Some(7710), false);
        assert!(!args.iter().any(|a| a == "--web-port"), "{args:?}");
        let i = args.iter().position(|a| a == "--port").expect("--port");
        assert_eq!(args[i + 1], "7710");
        let fg = daemon_spawn_args(dir.path(), Some(7710), true);
        assert!(!fg.iter().any(|a| a == "--web-port"), "{fg:?}");
        assert!(fg.iter().any(|a| a == "--foreground"));
    }

    #[test]
    fn resolve_daemon_url_reads_the_daemons_own_pid_format() {
        // The daemon writes `pid=` + `bind=host:port`. The old reader only
        // understood `port=` and fell back to 7700, another project's daemon.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/daemon.pid"),
            "pid=31751\nbind=127.0.0.1:7710\n",
        )
        .unwrap();
        assert_eq!(read_pid_port(dir.path()), Some(7710));
        assert_eq!(
            resolve_daemon_url(dir.path(), None),
            "http://127.0.0.1:7710"
        );
    }

    #[test]
    fn resolve_daemon_url_default_is_the_projects_derived_port_not_7700() {
        let dir = tempfile::tempdir().unwrap();
        let url = resolve_daemon_url(dir.path(), None);
        let port = daemon_port::default_port_for(dir.path()).unwrap();
        assert_eq!(url, format!("http://127.0.0.1:{port}"));
        assert!((7700..=7799).contains(&port));
    }

    #[test]
    fn resolve_daemon_url_explicit_7700_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/daemon.toml"),
            "[server]\nport = 7700\n",
        )
        .unwrap();
        assert_eq!(
            resolve_daemon_url(dir.path(), None),
            "http://127.0.0.1:7700"
        );
    }

    #[test]
    fn resolve_daemon_url_with_port_override() {
        let dir = tempfile::tempdir().unwrap();
        let url = resolve_daemon_url(dir.path(), Some(9900));
        assert_eq!(url, "http://127.0.0.1:9900");
    }

    #[test]
    fn resolve_daemon_url_follows_a_live_daemon_json_over_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(pid_path(dir.path()), "pid=1\nbind=127.0.0.1:7710\n").unwrap();
        // This test process is the "daemon": its pid is certainly running.
        ta_mcp_gateway::daemon_json::write(dir.path(), "127.0.0.1", 7733, std::process::id())
            .unwrap();
        assert_eq!(
            resolve_daemon_url(dir.path(), None),
            "http://127.0.0.1:7733"
        );
        // An explicit --port still wins.
        assert_eq!(
            resolve_daemon_url(dir.path(), Some(9900)),
            "http://127.0.0.1:9900"
        );
    }

    #[test]
    fn a_crashed_daemons_daemon_json_is_ignored_and_cleaned_by_start_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(pid_path(dir.path()), "pid=1\nbind=127.0.0.1:7710\n").unwrap();
        // pid 2147483646 is not running: the daemon crashed and left its record.
        ta_mcp_gateway::daemon_json::write(dir.path(), "127.0.0.1", 7733, 2_147_483_646).unwrap();
        assert_eq!(
            resolve_daemon_url(dir.path(), None),
            "http://127.0.0.1:7710",
            "a stale record must not redirect the CLI"
        );
        clean_stale_daemon_record(dir.path());
        assert!(!ta_mcp_gateway::daemon_json::path(dir.path()).exists());
    }

    #[test]
    fn resolve_daemon_url_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("daemon.toml"),
            "[server]\nbind = \"0.0.0.0\"\nport = 8800\n",
        )
        .unwrap();

        let url = resolve_daemon_url(dir.path(), None);
        assert_eq!(url, "http://0.0.0.0:8800");
    }

    #[test]
    fn resolve_daemon_url_config_with_override() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("daemon.toml"),
            "[server]\nbind = \"0.0.0.0\"\nport = 8800\n",
        )
        .unwrap();

        // Port override takes precedence over config.
        let url = resolve_daemon_url(dir.path(), Some(9999));
        assert_eq!(url, "http://0.0.0.0:9999");
    }

    #[test]
    fn start_rejects_when_alive_pid_exists() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".ta")).unwrap();

        // Write a PID file with our own PID (guaranteed alive).
        let my_pid = std::process::id();
        write_pid_file(project, my_pid, 7700).unwrap();

        let result = start(project, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("already running"),
            "Error should mention 'already running': {}",
            err
        );

        // Clean up.
        remove_pid_file(project);
    }

    #[test]
    fn start_cleans_stale_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join(".ta")).unwrap();

        // Write a PID file with a PID that's very unlikely to be alive.
        write_pid_file(project, 99999999, 7700).unwrap();

        // The lookup always fails, so no real ta-daemon is ever launched
        // (it would outlive the test). start() must still clean up the stale
        // PID file before it reaches the binary lookup.
        let result = start_with_binary(
            project,
            None,
            &|| Err(anyhow::anyhow!("ta-daemon lookup disabled in this test")),
            &CannedProbe(ProbeAnswer::NoAnswer),
        );
        let err = result.expect_err("start must fail when no daemon binary is found");
        let msg = err.to_string();
        assert!(msg.contains("lookup disabled"), "{msg}");
        assert!(
            !pid_path(project).exists() || read_pid(project) != Some(99999999),
            "Stale PID file should have been removed"
        );
    }

    #[test]
    fn cmd_log_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_log(dir.path(), 50, false, None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No daemon log"));
    }

    #[test]
    fn cmd_log_tail_lines() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        let log = ta_dir.join("daemon.log");
        let mut content = String::new();
        for i in 1..=100 {
            content.push_str(&format!("line {}\n", i));
        }
        std::fs::write(&log, &content).unwrap();

        // cmd_log prints to stdout; we just verify it doesn't error.
        assert!(cmd_log(dir.path(), 10, false, None, None, None).is_ok());
    }

    #[test]
    fn build_check_names_the_exact_next_command_for_a_stale_daemon() {
        use ta_lifecycle::BuildIdentity as B;
        let old = B::new("0.17.12-alpha.5", Some("aaa1111"));
        let new = B::new("0.17.12-alpha.5", Some("bbb2222"));
        match build_check(&old, &new, "ask", 300) {
            BuildCheck::Stale(t) => {
                assert!(t.contains("still 0.17.12-alpha.5 (aaa1111)"), "{t}");
                assert!(t.contains("ta daemon restart"), "{t}");
                assert!(t.contains("Keychain"), "{t}");
            }
            other => panic!("{other:?}"),
        }
        match build_check(&old, &new, "when_idle", 120) {
            BuildCheck::Stale(t) => {
                assert!(t.contains("on its own when idle, within about 120s"), "{t}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(build_check(&new, &new, "ask", 300), BuildCheck::Current);
    }

    #[test]
    fn update_check_interval_reads_daemon_toml_and_defaults_to_300() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(update_check_interval_secs(dir.path()), 300);
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/daemon.toml"),
            "[daemon]\nupdate_check_interval_secs = 60\n",
        )
        .unwrap();
        assert_eq!(update_check_interval_secs(dir.path()), 60);
    }

    #[test]
    fn cmd_status_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        // No PID file, no daemon running — should report "not running".
        assert!(cmd_status(dir.path(), false).is_ok());
    }

    #[test]
    fn is_process_alive_current() {
        let my_pid = std::process::id();
        assert!(is_process_alive(my_pid));
    }

    #[test]
    fn is_process_alive_nonexistent() {
        // PID 99999999 is very unlikely to be alive.
        assert!(!is_process_alive(99999999));
    }

    #[test]
    fn cmd_health_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        // No daemon running — health should report failures but not panic.
        let result = cmd_health(dir.path());
        // May return error (fail count > 0) or ok with warnings — either is fine.
        let _ = result;
    }

    fn starting_body() -> serde_json::Value {
        serde_json::json!({
            "status": "starting",
            "version": "0.0.0",
            "reasons": [{
                "component": "keychain",
                "reason": "waiting for keychain approval for audit-log signing key (12s of 60s)",
                "next_step": "Click Always Allow in the macOS Keychain dialog"
            }]
        })
    }

    #[test]
    fn an_ok_or_pre_upgrade_daemon_prints_no_startup_lines() {
        assert!(startup_report_lines(&serde_json::json!({"status": "ok"})).is_empty());
        // A daemon from before the field existed has no status at all.
        assert!(startup_report_lines(&serde_json::json!({"version": "0.1"})).is_empty());
        assert_eq!(daemon_state(&serde_json::json!({})), "ok");
    }

    #[test]
    fn a_starting_daemon_prints_its_reason_and_the_next_step() {
        let lines = startup_report_lines(&starting_body()).join("\n");
        assert!(lines.contains("State:          starting"), "{lines}");
        assert!(
            lines.contains("keychain: waiting for keychain approval"),
            "{lines}"
        );
        assert!(lines.contains("Next step: Click Always Allow"), "{lines}");
    }

    #[test]
    fn a_degraded_daemon_without_reasons_still_points_at_the_log() {
        let lines = startup_report_lines(&serde_json::json!({"status": "degraded", "reasons": []}))
            .join("\n");
        assert!(
            lines.contains("degraded") && lines.contains("ta daemon log"),
            "{lines}"
        );
    }

    // ── `--json` reports ────────────────────────────────────────────────────

    fn body(status: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut b = serde_json::json!({
            "status": status,
            "version": "1.2.3",
            "project_root": "",
            "active_goals": 2,
            "active_agents": [{"goal_id": "abc"}, {"goal_id": "def"}, {"goal_id": "ghi"}],
            "pending_drafts": 4,
        });
        if let (Some(m), Some(e)) = (b.as_object_mut(), extra.as_object()) {
            m.extend(e.clone());
        }
        b
    }

    #[test]
    fn json_flags_are_parsed_and_detected() {
        assert!(prints_json(&DaemonCommands::Status { json: true }));
        assert!(!prints_json(&DaemonCommands::Status { json: false }));
        assert!(prints_json(&DaemonCommands::Restart {
            port: None,
            force: false,
            json: true
        }));
        assert!(prints_json(&DaemonCommands::Start {
            foreground: false,
            port: None,
            json: true
        }));
        assert!(!prints_json(&DaemonCommands::Stop));
    }

    #[test]
    fn status_report_of_a_healthy_daemon_carries_the_pid_contract() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let probe = StatusProbe::Running {
            pid: 4242,
            body: body(
                "ok",
                serde_json::json!({"project_root": root.display().to_string()}),
            ),
            open_sessions: 1,
        };
        let r = status_report(&root, &probe);
        assert_eq!(r.action, DaemonAction::Status);
        assert_eq!(r.state, WsDaemonState::Running);
        assert!(r.running);
        assert_eq!(r.pid, Some(4242));
        assert_eq!(r.version.as_deref(), Some("1.2.3"));
        assert_eq!(
            (r.active_goals, r.active_agents, r.pending_drafts),
            (Some(2), Some(3), Some(4))
        );
        let pid_file = r.pid_file.replace('\\', "/");
        let log_file = r.log_file.replace('\\', "/");
        assert!(pid_file.ends_with(".ta/daemon.pid"), "{}", r.pid_file);
        assert!(log_file.ends_with(".ta/daemon.log"), "{}", r.log_file);
        assert_eq!(r.project_root, root.display().to_string());
        let url = r.url.clone().unwrap();
        assert_eq!(
            url,
            format!("http://{}:{}", r.host.unwrap(), r.port.unwrap())
        );
        assert!(r.detail.is_none(), "{:?}", r.detail);
        assert_eq!(r.schema_version, DAEMON_REPORT_SCHEMA_VERSION);
    }

    #[test]
    fn status_report_names_why_a_daemon_is_starting_or_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let reasons = serde_json::json!({"reasons": [
            {"component": "keychain", "reason": "waiting for approval", "next_step": "click Always Allow"}
        ]});
        let starting = StatusProbe::Running {
            pid: 1,
            body: body("starting", reasons.clone()),
            open_sessions: 0,
        };
        let r = status_report(dir.path(), &starting);
        assert_eq!(r.state, WsDaemonState::Starting);
        assert!(r.running);
        assert_eq!(r.reasons.len(), 1);
        assert_eq!(r.reasons[0].component, "keychain");
        assert_eq!(
            r.reasons[0].next_step.as_deref(),
            Some("click Always Allow")
        );

        // A degraded daemon that gives no reason still says where to look.
        let degraded = StatusProbe::External {
            body: body("degraded", serde_json::json!({})),
        };
        let r = status_report(dir.path(), &degraded);
        assert_eq!(r.state, WsDaemonState::External);
        assert!(r.running);
        assert_eq!(r.reasons.len(), 1);
        assert!(r.reasons[0]
            .next_step
            .as_deref()
            .unwrap()
            .contains("ta daemon log"));
    }

    #[test]
    fn status_report_of_every_not_running_case_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (
                StatusProbe::NotRunning,
                WsDaemonState::NotRunning,
                "ta daemon start",
            ),
            (
                StatusProbe::StalePid { pid: 77 },
                WsDaemonState::StalePid,
                "ta daemon start",
            ),
            (
                StatusProbe::Unresponsive { pid: 88 },
                WsDaemonState::Unresponsive,
                "ta daemon log",
            ),
            (
                StatusProbe::OtherProject {
                    reported_root: "/elsewhere".to_string(),
                    pid: Some(5),
                },
                WsDaemonState::OtherProject,
                "/elsewhere",
            ),
        ];
        for (probe, state, needle) in cases {
            let r = status_report(dir.path(), &probe);
            assert_eq!(r.state, state);
            assert!(!r.running, "{state:?}");
            let detail = r.detail.clone().unwrap();
            assert!(detail.contains(needle), "{state:?}: {detail}");
        }
        let stale = status_report(dir.path(), &StatusProbe::StalePid { pid: 77 });
        assert!(stale.stale_pid_removed && stale.pid.is_none());
        let unresponsive = status_report(dir.path(), &StatusProbe::Unresponsive { pid: 88 });
        assert_eq!(unresponsive.pid, Some(88));
    }

    #[test]
    fn status_report_flags_a_daemon_serving_another_project() {
        let dir = tempfile::tempdir().unwrap();
        let probe = StatusProbe::Running {
            pid: 9,
            body: body(
                "ok",
                serde_json::json!({"project_root": "/some/other/project"}),
            ),
            open_sessions: 0,
        };
        let r = status_report(dir.path(), &probe);
        assert_eq!(
            r.reported_project_root.as_deref(),
            Some("/some/other/project")
        );
        assert!(r.detail.unwrap().contains("not this project"));
    }

    #[test]
    fn probing_a_stale_pid_file_removes_it_and_reports_it_in_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        write_pid_file(&root, 99_999_999, 7799).unwrap();
        let url = resolve_daemon_url(&root, None);
        let probe = probe_status(&root, &url).unwrap();
        assert!(matches!(probe, StatusProbe::StalePid { pid: 99_999_999 }));
        assert!(
            !pid_path(&root).exists(),
            "the stale pid file must be removed"
        );
        let r = status_report(&root, &probe);
        assert_eq!(r.state, WsDaemonState::StalePid);
        assert!(r.stale_pid_removed);
    }

    #[test]
    fn launch_report_covers_health_ok_starting_and_silent() {
        let dir = tempfile::tempdir().unwrap();
        let ok = launch_report(
            dir.path(),
            DaemonAction::Restart,
            Some(7788),
            200,
            Some(100),
            Some(&body("ok", serde_json::json!({}))),
        );
        assert_eq!(ok.action, DaemonAction::Restart);
        assert_eq!(ok.state, WsDaemonState::Running);
        assert_eq!((ok.pid, ok.previous_pid), (Some(200), Some(100)));
        assert_eq!(ok.port, Some(7788), "--port wins");
        assert!(ok.running && ok.detail.is_none());

        let starting = launch_report(
            dir.path(),
            DaemonAction::Start,
            None,
            300,
            None,
            Some(&body(
                "starting",
                serde_json::json!({"reasons": [{"component": "keychain", "reason": "waiting"}]}),
            )),
        );
        assert_eq!(starting.state, WsDaemonState::Starting);
        assert_eq!(starting.previous_pid, None);
        assert!(starting.detail.unwrap().contains("ta daemon status"));

        let silent = launch_report(dir.path(), DaemonAction::Start, None, 400, None, None);
        assert_eq!(silent.state, WsDaemonState::Starting);
        assert_eq!(silent.pid, Some(400));
        assert!(silent.running);
        assert!(silent.detail.unwrap().contains("ta daemon log"));
    }

    #[test]
    fn reports_serialize_with_snake_case_states_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let r = status_report(dir.path(), &StatusProbe::StalePid { pid: 1 });
        let text = serde_json::to_string(&r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["state"], "stale_pid");
        assert_eq!(v["action"], "status");
        assert_eq!(v["schema_version"], DAEMON_REPORT_SCHEMA_VERSION);
        assert!(v["pid"].is_null() && v["version"].is_null());
        assert_eq!(serde_json::from_str::<DaemonReport>(&text).unwrap(), r);
    }

    #[test]
    fn foreground_json_is_refused_with_the_fix_before_anything_starts() {
        let dir = tempfile::tempdir().unwrap();
        let err = cmd_start(dir.path(), true, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--foreground --json"), "{err}");
        assert!(err.contains("Drop --foreground"), "{err}");
        assert!(!pid_path(dir.path()).exists());
    }

    #[test]
    fn status_json_on_an_empty_project_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cmd_status(dir.path(), true).is_ok());
    }
}
