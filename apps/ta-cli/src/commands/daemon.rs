// daemon.rs — Daemon lifecycle management (`ta daemon` subcommand).
//
// Provides start/stop/restart/status/log as first-class CLI verbs so users
// don't need wrapper scripts or knowledge of the `ta-daemon` binary.
//
// Shared helpers (`ensure_running`, `start`, `stop`, `restart`) are used by
// `shell.rs` and `version_guard.rs` to eliminate duplicated daemon spawn logic.

use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Subcommand;

/// `ta daemon` subcommands.
#[derive(Subcommand)]
pub enum DaemonCommands {
    /// Start the daemon in the background.
    Start {
        /// Run in the foreground (for debugging/containers) instead of daemonizing.
        #[arg(long)]
        foreground: bool,
        /// Override the daemon HTTP port (default: from daemon.toml or 7700).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Stop the running daemon gracefully.
    Stop,
    /// Restart the daemon (stop + start). Handles version mismatches.
    Restart {
        /// Override the daemon HTTP port on restart.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Show daemon status: PID, port, version, uptime, project root, active goals.
    Status,
    /// Tail the daemon log file.
    Log {
        /// Number of lines to show (default: 50).
        #[arg(default_value = "50")]
        lines: usize,
        /// Follow the log in real time (like `tail -f`).
        #[arg(long, short)]
        follow: bool,
    },
}

/// Execute a `ta daemon` subcommand.
pub fn execute(command: &DaemonCommands, project_root: &Path) -> anyhow::Result<()> {
    match command {
        DaemonCommands::Start { foreground, port } => cmd_start(project_root, *foreground, *port),
        DaemonCommands::Stop => cmd_stop(project_root),
        DaemonCommands::Restart { port } => cmd_restart(project_root, *port),
        DaemonCommands::Status => cmd_status(project_root),
        DaemonCommands::Log { lines, follow } => cmd_log(project_root, *lines, *follow),
    }
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

/// Read the PID from `.ta/daemon.pid`. Returns `None` if the file is missing or
/// doesn't contain a valid `pid=<N>` line.
pub fn read_pid(project_root: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(pid_path(project_root)).ok()?;
    content
        .lines()
        .find(|l| l.starts_with("pid="))
        .and_then(|l| l.strip_prefix("pid="))
        .and_then(|s| s.parse::<u32>().ok())
}

/// Read the port from `.ta/daemon.pid`. Returns `None` if absent.
fn read_pid_port(project_root: &Path) -> Option<u16> {
    let content = std::fs::read_to_string(pid_path(project_root)).ok()?;
    content
        .lines()
        .find(|l| l.starts_with("port="))
        .and_then(|l| l.strip_prefix("port="))
        .and_then(|s| s.parse::<u16>().ok())
}

/// Write a PID file with pid, port, and log path.
fn write_pid_file(project_root: &Path, pid: u32, port: u16) -> anyhow::Result<()> {
    let content = format!(
        "pid={}\nport={}\nlog={}\n",
        pid,
        port,
        log_path(project_root).display()
    );
    std::fs::write(pid_path(project_root), content)?;
    Ok(())
}

/// Remove the PID file if it exists.
fn remove_pid_file(project_root: &Path) {
    let _ = std::fs::remove_file(pid_path(project_root));
}

/// Check whether a process with the given PID is alive.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        // Use `tasklist /FI "PID eq <pid>"` to check if the process exists.
        // The output contains the PID number if the process is alive.
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                // tasklist prints "INFO: No tasks are running..." when not found,
                // or the process line containing the PID when found.
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

/// Resolve the daemon URL from `.ta/daemon.toml` or default, with optional port
/// override.
pub fn resolve_daemon_url(project_root: &Path, port_override: Option<u16>) -> String {
    let config_path = project_root.join(".ta").join("daemon.toml");
    let mut bind = "127.0.0.1".to_string();
    let mut port: u16 = 7700;

    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = content.parse::<toml::Table>() {
            if let Some(b) = config
                .get("server")
                .and_then(|s| s.get("bind"))
                .and_then(|v| v.as_str())
            {
                bind = b.to_string();
            }
            if let Some(p) = config
                .get("server")
                .and_then(|s| s.get("port"))
                .and_then(|v| v.as_integer())
            {
                port = p as u16;
            }
        }
    }

    if let Some(p) = port_override {
        port = p;
    }

    format!("http://{}:{}", bind, port)
}

/// Start the daemon in the background. Returns the child PID on success.
///
/// If a daemon is already running (live PID), returns an error. Stale PID files
/// are cleaned up automatically.
pub fn start(project_root: &Path, port_override: Option<u16>) -> anyhow::Result<u32> {
    // Check for existing PID file.
    if let Some(pid) = read_pid(project_root) {
        if is_process_alive(pid) {
            return Err(anyhow::anyhow!(
                "Daemon already running (pid {}). Use `ta daemon stop` first, or \
                 `ta daemon restart` to replace it.",
                pid
            ));
        }
        // Stale PID file — remove it.
        remove_pid_file(project_root);
    }

    let daemon_bin = super::version_guard::find_daemon_binary()?;

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
    cmd.arg("--api")
        .arg("--project-root")
        .arg(project_root)
        .stdout(log_file)
        .stderr(stderr_log);

    if let Some(port) = port_override {
        cmd.arg("--web-port").arg(port.to_string());
    }

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Cannot spawn {}: {}", daemon_bin.display(), e))?;

    let pid = child.id();
    let port = port_override.unwrap_or_else(|| {
        // Read from daemon.toml or default.
        let url = resolve_daemon_url(project_root, None);
        url.rsplit(':')
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(7700)
    });

    write_pid_file(project_root, pid, port)?;

    Ok(pid)
}

/// Stop the running daemon. Sends POST /api/shutdown, waits up to 5s for exit,
/// then cleans up the PID file.
pub fn stop(project_root: &Path) -> anyhow::Result<()> {
    let base_url = resolve_daemon_url(project_root, None);
    let shutdown_url = format!("{}/api/shutdown", base_url);

    // Try graceful shutdown via HTTP.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;

    let http_sent = client.post(&shutdown_url).send().is_ok();

    if !http_sent {
        // If HTTP fails, try to kill by PID.
        if let Some(pid) = read_pid(project_root) {
            if is_process_alive(pid) {
                #[cfg(unix)]
                {
                    let _ = Command::new("kill").arg(pid.to_string()).output();
                }
                eprintln!("Sent SIGTERM to daemon (pid {}).", pid);
            } else {
                eprintln!("Daemon not running (stale PID file).");
                remove_pid_file(project_root);
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
    Ok(())
}

/// Restart the daemon: stop the old one (if running), then start a new one.
pub fn restart(project_root: &Path, port_override: Option<u16>) -> anyhow::Result<u32> {
    // Stop (ignore errors if not running).
    let _ = stop(project_root);

    // Brief pause to let the port be released.
    std::thread::sleep(std::time::Duration::from_millis(300));

    start(project_root, port_override)
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

    let status_url = format!("{}/api/status", base_url);
    let reachable = client.get(&status_url).send().is_ok();

    if reachable {
        return Ok(());
    }

    // Not reachable — start it.
    eprintln!("Daemon not reachable at {} — starting...", base_url);
    let pid = start(project_root, None)?;
    let port = read_pid_port(project_root).unwrap_or(7700);

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

// ─── CLI command implementations ─────────────────────────────────────────────

fn cmd_start(project_root: &Path, foreground: bool, port: Option<u16>) -> anyhow::Result<()> {
    if foreground {
        // Foreground mode: exec the daemon binary directly.
        let daemon_bin = super::version_guard::find_daemon_binary()?;
        let mut cmd = Command::new(&daemon_bin);
        cmd.arg("--api")
            .arg("--foreground")
            .arg("--project-root")
            .arg(project_root);

        if let Some(p) = port {
            cmd.arg("--web-port").arg(p.to_string());
        }

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
    let effective_port = read_pid_port(project_root).unwrap_or(7700);

    println!("Daemon started.");
    println!("  PID:  {}", pid);
    println!("  Port: {}", effective_port);
    println!("  Log:  {}", log_path(project_root).display());
    println!();
    println!("Use `ta daemon status` to check health, `ta daemon stop` to shut down.");

    // Wait briefly for startup and report health.
    let base_url = resolve_daemon_url(project_root, port);
    let status_url = format!("{}/api/status", base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if client.get(&status_url).send().is_ok() {
            println!("  Health: ok (listening on {})", base_url);
            return Ok(());
        }
    }

    println!("  Health: starting (not yet responding — check log for details)");
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

fn cmd_restart(project_root: &Path, port: Option<u16>) -> anyhow::Result<()> {
    let old_pid = read_pid(project_root);
    let new_pid = restart(project_root, port)?;
    let effective_port = read_pid_port(project_root).unwrap_or(7700);

    if let Some(old) = old_pid {
        println!("Daemon restarted (was pid {}, now pid {}).", old, new_pid);
    } else {
        println!("Daemon started (pid {}).", new_pid);
    }

    println!("  Port: {}", effective_port);
    println!("  Log:  {}", log_path(project_root).display());

    // Wait for health.
    let base_url = resolve_daemon_url(project_root, port);
    let status_url = format!("{}/api/status", base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if client.get(&status_url).send().is_ok() {
            println!("  Health: ok");
            return Ok(());
        }
    }

    println!("  Health: starting (not yet responding — check log)");
    Ok(())
}

fn cmd_status(project_root: &Path) -> anyhow::Result<()> {
    let pid = read_pid(project_root);
    let base_url = resolve_daemon_url(project_root, None);

    match pid {
        Some(p) if is_process_alive(p) => {
            // Daemon PID is alive — query for details.
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()?;

            let status_url = format!("{}/api/status", base_url);
            match client.get(&status_url).send() {
                Ok(resp) if resp.status().is_success() => {
                    let json: serde_json::Value = resp.json()?;
                    let version = json["version"].as_str().unwrap_or("?");
                    let project = json["project"].as_str().unwrap_or("?");
                    let active_agents = json["active_agents"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0);
                    let pending_drafts = json["pending_drafts"].as_u64().unwrap_or(0);

                    println!("Daemon is running.");
                    println!("  PID:            {}", p);
                    println!("  URL:            {}", base_url);
                    println!("  Version:        {}", version);
                    println!("  Project:        {}", project);
                    println!("  Project root:   {}", project_root.display());
                    println!("  Active agents:  {}", active_agents);
                    println!("  Pending drafts: {}", pending_drafts);
                    println!("  Log:            {}", log_path(project_root).display());
                }
                _ => {
                    println!(
                        "Daemon process is alive (pid {}) but not responding on {}.",
                        p, base_url
                    );
                    println!("  It may still be starting up. Check logs:");
                    println!("    ta daemon log");
                    println!("  Or force kill and restart:");
                    println!("    kill {} && ta daemon start", p);
                }
            }
        }
        Some(p) => {
            // PID exists but process is not alive — stale.
            remove_pid_file(project_root);
            println!(
                "Daemon is not running (stale PID file for pid {} removed).",
                p
            );
            println!("  Start it with: ta daemon start");
        }
        None => {
            // No PID file. Check if daemon is reachable anyway (started externally).
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()?;

            let status_url = format!("{}/api/status", base_url);
            if let Ok(resp) = client.get(&status_url).send() {
                if resp.status().is_success() {
                    let json: serde_json::Value = resp.json()?;
                    let version = json["version"].as_str().unwrap_or("?");
                    println!("Daemon is running (started externally, no PID file).");
                    println!("  URL:     {}", base_url);
                    println!("  Version: {}", version);
                    return Ok(());
                }
            }

            println!("Daemon is not running.");
            println!("  Start it with: ta daemon start");
        }
    }

    Ok(())
}

fn cmd_log(project_root: &Path, lines: usize, follow: bool) -> anyhow::Result<()> {
    let log = log_path(project_root);

    if !log.exists() {
        return Err(anyhow::anyhow!(
            "No daemon log found at {}. Start the daemon first: ta daemon start",
            log.display()
        ));
    }

    if follow {
        // Live tail — open the file, seek to the last N lines, then poll for new content.
        let file = std::fs::File::open(&log)
            .map_err(|e| anyhow::anyhow!("Cannot open {}: {}", log.display(), e))?;
        let reader = std::io::BufReader::new(&file);
        let all_lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

        // Print last N lines first.
        let start = all_lines.len().saturating_sub(lines);
        for line in &all_lines[start..] {
            println!("{}", line);
        }

        // Now follow using polling.
        let mut last_len = std::fs::metadata(&log)?.len();
        let stdout = std::io::stdout();

        println!("--- following {} (Ctrl-C to stop) ---", log.display());

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
                    writeln!(handle, "{}", l)?;
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
        let start = all_lines.len().saturating_sub(lines);

        for line in &all_lines[start..] {
            println!("{}", line);
        }

        println!(
            "\n({} of {} lines shown. Use `--follow` for live tail.)",
            all_lines.len() - start,
            all_lines.len()
        );
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
    fn resolve_daemon_url_default() {
        let dir = tempfile::tempdir().unwrap();
        let url = resolve_daemon_url(dir.path(), None);
        assert_eq!(url, "http://127.0.0.1:7700");
    }

    #[test]
    fn resolve_daemon_url_with_port_override() {
        let dir = tempfile::tempdir().unwrap();
        let url = resolve_daemon_url(dir.path(), Some(9900));
        assert_eq!(url, "http://127.0.0.1:9900");
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

        // start() will fail because ta-daemon binary doesn't exist in test,
        // but it should have cleaned up the stale PID file first.
        let result = start(project, None);
        // We expect either a binary-not-found error or success; the stale PID
        // should have been cleaned up either way.
        if result.is_err() {
            // The stale PID file should be gone.
            assert!(
                !pid_path(project).exists() || read_pid(project) != Some(99999999),
                "Stale PID file should have been removed"
            );
        }
    }

    #[test]
    fn cmd_log_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = cmd_log(dir.path(), 50, false);
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
        assert!(cmd_log(dir.path(), 10, false).is_ok());
    }

    #[test]
    fn cmd_status_no_daemon() {
        let dir = tempfile::tempdir().unwrap();
        // No PID file, no daemon running — should report "not running".
        assert!(cmd_status(dir.path()).is_ok());
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
}
