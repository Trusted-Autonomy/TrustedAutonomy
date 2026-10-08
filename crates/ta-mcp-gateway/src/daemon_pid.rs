//! The one shared reader and writer for `.ta/daemon.pid`, and the one place
//! that decides which port a project's daemon API is reachable on.
//!
//! Before this module existed the CLI wrote `pid=/port=/log=`, the daemon
//! then overwrote the same file with `pid=/bind=host:port`, and every reader
//! only looked for a `port=` line. A daemon started on any port other than
//! 7700 therefore looked like "no port recorded", and readers silently fell
//! back to 7700, which on a machine with more than one TA project is a
//! DIFFERENT project's daemon. Whiteboard tool calls then delivered this
//! project's session token to that other daemon.
//!
//! Format written (by both the CLI and the daemon):
//!
//! ```text
//! pid=<PID>
//! bind=<host>:<port>
//! port=<port>
//! log=<path to daemon.log>
//! ```
//!
//! `port=` duplicates the port from `bind=` on purpose, so an older `ta`
//! binary that only understands `port=` keeps resolving the right port.
//! The reader accepts the new format, the CLI's legacy `pid/port/log`
//! format and the daemon's legacy `pid/bind` format.

use std::path::{Path, PathBuf};

/// The port a daemon binds when nothing configures a different one. Only
/// ever used as a resolution result when the project has no daemon
/// configuration and no pid file at all.
pub const DEFAULT_DAEMON_PORT: u16 = 7700;

/// `.ta/daemon.pid` under `project_root`.
pub fn pid_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("daemon.pid")
}

/// `.ta/daemon.log` under `project_root` (where `ta daemon start` sends the
/// daemon's stdout and stderr).
pub fn default_log_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("daemon.log")
}

/// `.ta/daemon.toml` under `project_root`.
pub fn daemon_toml_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("daemon.toml")
}

/// Parsed contents of a pid file. Every field is optional because the file
/// may come from an older binary that wrote a subset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonPidFile {
    pub pid: Option<u32>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub log: Option<PathBuf>,
}

impl DaemonPidFile {
    /// Parse any of the formats described in the module doc. A `bind=`
    /// line wins over a `port=` line when both are present and disagree,
    /// because `bind=` is what the daemon itself records after binding.
    pub fn parse(content: &str) -> Self {
        let mut out = DaemonPidFile::default();
        let mut port_line: Option<u16> = None;
        let mut bind_port: Option<u16> = None;
        for line in content.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("pid=") {
                out.pid = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("port=") {
                port_line = v.trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("bind=") {
                if let Some((host, port)) = split_host_port(v.trim()) {
                    out.host = Some(host);
                    bind_port = Some(port);
                }
            } else if let Some(v) = line.strip_prefix("log=") {
                let v = v.trim();
                if !v.is_empty() {
                    out.log = Some(PathBuf::from(v));
                }
            }
        }
        out.port = bind_port.or(port_line);
        out
    }

    /// Render in the single current format.
    pub fn render(pid: u32, host: &str, port: u16, log: &Path) -> String {
        format!(
            "pid={pid}\nbind={host}:{port}\nport={port}\nlog={}\n",
            log.display()
        )
    }
}

/// Split `host:port`, accepting bracketed IPv6 (`[::1]:7700`). Returns the
/// host without brackets.
fn split_host_port(s: &str) -> Option<(String, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some((host.to_string(), port))
}

/// Write `.ta/daemon.pid` in the current format, creating `.ta/` if needed.
pub fn write_pid_file(
    project_root: &Path,
    pid: u32,
    host: &str,
    port: u16,
    log: &Path,
) -> std::io::Result<()> {
    let path = pid_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, DaemonPidFile::render(pid, host, port, log))
}

/// Read and parse `.ta/daemon.pid`. `Ok(None)` when the file does not exist.
pub fn read_pid_file(project_root: &Path) -> std::io::Result<Option<DaemonPidFile>> {
    match std::fs::read_to_string(pid_path(project_root)) {
        Ok(content) => Ok(Some(DaemonPidFile::parse(&content))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Where a resolved daemon endpoint came from, for error and log messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointSource {
    /// The live daemon's `.ta/daemon.pid`.
    PidFile(PathBuf),
    /// `[server]` in `.ta/daemon.toml` (no pid file present).
    DaemonToml(PathBuf),
    /// Neither file exists: the daemon's built-in default.
    Default,
}

/// A resolved, connectable daemon API endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonEndpoint {
    /// Host to CONNECT to (a wildcard bind such as `0.0.0.0` becomes
    /// `127.0.0.1`).
    pub host: String,
    pub port: u16,
    pub source: EndpointSource,
}

impl DaemonEndpoint {
    pub fn base_url(&self) -> String {
        if self.host.contains(':') {
            format!("http://[{}]:{}", self.host, self.port)
        } else {
            format!("http://{}:{}", self.host, self.port)
        }
    }
}

fn connect_host(bind: &str) -> String {
    match bind.trim() {
        "" | "0.0.0.0" | "::" | "[::]" | "*" => "127.0.0.1".to_string(),
        other => other.to_string(),
    }
}

/// Resolve the daemon API endpoint for `project_root`, failing closed.
///
/// Order:
/// 1. `.ta/daemon.pid` exists: it MUST carry a usable port (`bind=` or
///    `port=`). If it does not, this is an error naming the file, never a
///    guess.
/// 2. Otherwise `.ta/daemon.toml` exists: it must parse, and its
///    `[server] port` (when present) must be a valid port. A daemon.toml with
///    no `[server] port` means the daemon itself binds its built-in default,
///    so that default is the configured port, not a guess.
/// 3. Neither file exists: the built-in default (7700).
pub fn resolve_daemon_endpoint(project_root: &Path) -> Result<DaemonEndpoint, String> {
    let pid = pid_path(project_root);
    match read_pid_file(project_root) {
        Ok(Some(parsed)) => {
            return match parsed.port {
                Some(port) => Ok(DaemonEndpoint {
                    host: connect_host(parsed.host.as_deref().unwrap_or("127.0.0.1")),
                    port,
                    source: EndpointSource::PidFile(pid),
                }),
                None => Err(format!(
                    "The daemon pid file {} has no usable port (expected a `bind=<host>:<port>` \
                     or `port=<port>` line). Refusing to guess a port, because the default 7700 \
                     may belong to a different project's daemon. Fix: run `ta daemon restart` in \
                     {} so the daemon rewrites the file, or delete the stale file if no daemon \
                     is running.",
                    pid.display(),
                    project_root.display()
                )),
            };
        }
        Ok(None) => {}
        Err(e) => {
            return Err(format!(
                "Could not read the daemon pid file {}: {}. Refusing to guess a port. Check the \
                 file's permissions, or run `ta daemon restart` in {}.",
                pid.display(),
                e,
                project_root.display()
            ))
        }
    }

    let toml_path = daemon_toml_path(project_root);
    if toml_path.exists() {
        let content = std::fs::read_to_string(&toml_path).map_err(|e| {
            format!(
                "Could not read {}: {}. Refusing to guess a daemon port. Check the file's \
                 permissions.",
                toml_path.display(),
                e
            )
        })?;
        let table: toml::Table = content.parse().map_err(|e| {
            format!(
                "{} is not valid TOML ({}), so the daemon port cannot be determined. Refusing \
                 to guess a port. Fix the file, then retry.",
                toml_path.display(),
                e
            )
        })?;
        let server = table.get("server");
        let host = server
            .and_then(|s| s.get("bind"))
            .and_then(|v| v.as_str())
            .unwrap_or("127.0.0.1");
        let port = match server.and_then(|s| s.get("port")) {
            None => DEFAULT_DAEMON_PORT,
            Some(v) => v
                .as_integer()
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p != 0)
                .ok_or_else(|| {
                    format!(
                        "{} has `[server] port = {}`, which is not a usable TCP port (1-65535). \
                         Refusing to guess a daemon port. Fix the value, then retry.",
                        toml_path.display(),
                        v
                    )
                })?,
        };
        return Ok(DaemonEndpoint {
            host: connect_host(host),
            port,
            source: EndpointSource::DaemonToml(toml_path),
        });
    }

    Ok(DaemonEndpoint {
        host: "127.0.0.1".to_string(),
        port: DEFAULT_DAEMON_PORT,
        source: EndpointSource::Default,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        dir
    }

    #[test]
    fn daemon_two_line_format_resolves_the_bound_port() {
        // The exact shape found live: the daemon overwrote the CLI's file
        // with only pid= and bind=. Old readers returned None here and fell
        // back to 7700 (another project's daemon).
        let dir = project();
        std::fs::write(pid_path(dir.path()), "pid=4242\nbind=127.0.0.1:7710\n").unwrap();
        let ep = resolve_daemon_endpoint(dir.path()).unwrap();
        assert_eq!(ep.port, 7710);
        assert_eq!(ep.base_url(), "http://127.0.0.1:7710");
        assert!(matches!(ep.source, EndpointSource::PidFile(_)));
    }

    #[test]
    fn legacy_cli_port_format_still_resolves() {
        let dir = project();
        std::fs::write(
            pid_path(dir.path()),
            "pid=1\nport=8899\nlog=/tmp/daemon.log\n",
        )
        .unwrap();
        assert_eq!(resolve_daemon_endpoint(dir.path()).unwrap().port, 8899);
    }

    #[test]
    fn pid_file_without_any_port_is_an_error_not_7700() {
        let dir = project();
        std::fs::write(pid_path(dir.path()), "pid=1\n").unwrap();
        let err = resolve_daemon_endpoint(dir.path()).unwrap_err();
        assert!(err.contains("daemon.pid"), "{err}");
        assert!(err.contains("ta daemon restart"), "{err}");
    }

    #[test]
    fn malformed_daemon_toml_is_an_error_not_7700() {
        let dir = project();
        std::fs::write(daemon_toml_path(dir.path()), "[server\nport = ").unwrap();
        let err = resolve_daemon_endpoint(dir.path()).unwrap_err();
        assert!(err.contains("daemon.toml"), "{err}");
    }

    #[test]
    fn invalid_port_in_daemon_toml_is_an_error() {
        let dir = project();
        std::fs::write(daemon_toml_path(dir.path()), "[server]\nport = 70000\n").unwrap();
        assert!(resolve_daemon_endpoint(dir.path()).is_err());
    }

    #[test]
    fn daemon_toml_port_used_when_no_pid_file() {
        let dir = project();
        std::fs::write(
            daemon_toml_path(dir.path()),
            "[server]\nbind = \"0.0.0.0\"\nport = 7711\n",
        )
        .unwrap();
        let ep = resolve_daemon_endpoint(dir.path()).unwrap();
        assert_eq!(ep.base_url(), "http://127.0.0.1:7711");
    }

    #[test]
    fn default_only_when_no_configuration_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let ep = resolve_daemon_endpoint(dir.path()).unwrap();
        assert_eq!(ep.port, DEFAULT_DAEMON_PORT);
        assert_eq!(ep.source, EndpointSource::Default);
    }

    #[test]
    fn written_file_round_trips_and_keeps_legacy_port_line() {
        let dir = project();
        let log = default_log_path(dir.path());
        write_pid_file(dir.path(), 77, "127.0.0.1", 7712, &log).unwrap();
        let raw = std::fs::read_to_string(pid_path(dir.path())).unwrap();
        assert!(raw.contains("port=7712\n"), "old readers need port=: {raw}");
        let parsed = read_pid_file(dir.path()).unwrap().unwrap();
        assert_eq!(parsed.pid, Some(77));
        assert_eq!(parsed.port, Some(7712));
        assert_eq!(parsed.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.log, Some(log));
    }

    #[test]
    fn bind_wins_over_a_stale_port_line() {
        let parsed = DaemonPidFile::parse("pid=1\nport=7700\nbind=127.0.0.1:7710\n");
        assert_eq!(parsed.port, Some(7710));
    }

    #[test]
    fn ipv6_bind_parses() {
        let parsed = DaemonPidFile::parse("pid=1\nbind=[::1]:7720\n");
        assert_eq!(parsed.host.as_deref(), Some("::1"));
        assert_eq!(parsed.port, Some(7720));
    }
}
