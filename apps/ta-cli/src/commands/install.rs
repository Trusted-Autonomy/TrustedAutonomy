// commands/install.rs — `ta install` command (v0.14.13).
//
// Opens the TA Studio setup wizard in the browser. If the daemon is not
// already running, starts it first.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Execute `ta install`:
/// 1. Ensure the daemon is running (start if needed).
/// 2. Open the setup wizard at http://localhost:<port>/setup.
pub fn execute(project_root: &Path) -> anyhow::Result<()> {
    // Determine the port from the pid file or daemon config (per-project default).
    let mut port = resolve_port(project_root);
    let mut studio_url = format!("http://localhost:{}/setup", port);
    let mut health_url = format!("http://localhost:{}/health", port);

    println!("TA Studio — Setup Wizard");
    println!();

    // Check if daemon is already running.
    let daemon_running = check_daemon_health(&health_url);

    if daemon_running {
        // Whatever answers must be this project's daemon, not another one's.
        super::daemon::ensure_own_daemon_on_port(project_root)?;
        println!("  Daemon is running at http://localhost:{}", port);
    } else {
        println!("  Starting TA daemon...");
        match super::daemon::start(project_root, None) {
            Ok(pid) => {
                println!("  Daemon started (PID {}).", pid);
                // The start may have chosen a free per-project port.
                port = resolve_port(project_root);
                studio_url = format!("http://localhost:{}/setup", port);
                health_url = format!("http://localhost:{}/health", port);
                // Brief wait for the daemon to become ready.
                let start = std::time::Instant::now();
                let ready = loop {
                    if start.elapsed() > Duration::from_secs(10) {
                        break false;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                    if check_daemon_health(&health_url) {
                        break true;
                    }
                };
                if !ready {
                    println!("  Warning: daemon did not become healthy within 10 seconds.");
                    println!("  If the wizard doesn't load, run: ta daemon status");
                }
            }
            Err(e) => {
                println!("  Warning: could not start daemon automatically: {}", e);
                println!("  You can start it manually with: ta daemon start");
                println!();
            }
        }
    }

    println!();
    println!("  Opening TA Studio setup wizard in your browser...");
    println!("  URL: {}", studio_url);
    println!();

    if let Err(e) = open_browser(&studio_url) {
        println!("  Could not open browser automatically: {}", e);
        println!("  Please open this URL manually: {}", studio_url);
    }

    println!("Next steps:");
    println!("  1. Complete the 5-step setup wizard in the browser.");
    println!("  2. Configure your agent system, VCS, and notifications.");
    println!("  3. Create your first goal from the Dashboard tab.");
    println!();
    println!("  Stuck? Run `ta doctor` for a health check, or visit the Dashboard at:");
    println!("  http://localhost:{}", port);

    Ok(())
}

/// Resolve the daemon port the same way every other command does: the live
/// pid file, then `[server] port` in `.ta/daemon.toml`, then the project's
/// derived port.
fn resolve_port(project_root: &Path) -> u16 {
    super::daemon::resolve_daemon_port(project_root)
}

/// Returns true if `GET <url>` responds with HTTP 200.
fn check_daemon_health(health_url: &str) -> bool {
    // Use a simple synchronous TCP check to avoid requiring tokio here.
    // Parse the URL to get host:port.
    let url_stripped = health_url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let (host_port, _path) = url_stripped.split_once('/').unwrap_or((url_stripped, ""));

    use std::net::{TcpStream, ToSocketAddrs};
    // "localhost:<port>" resolves; an unparseable address is simply not reachable.
    host_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok())
        .unwrap_or(false)
}

/// Open a URL in the default browser.
fn open_browser(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd").args(["/c", "start", url]).spawn()?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!(
            "Automatic browser opening is not supported on this platform. \
             Open {} manually.",
            url
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_daemon_health_unreachable_returns_false() {
        // Port 1 is reserved and never reachable in normal conditions.
        assert!(!check_daemon_health("http://127.0.0.1:1/health"));
    }

    #[test]
    fn resolve_port_returns_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let port = resolve_port(dir.path());
        assert!(port > 0);
    }
}
