// daemon_ownership.rs: Which project's daemon is answering on a port?
//
// `ta daemon stop`, `restart` and `start` used to act on whatever answered on
// the configured port. With two projects on one machine (both defaulting to
// 7700) that meant one project's `ta daemon restart` shut down the other
// project's daemon while a goal was still running.
//
// Every lifecycle command now asks the answering daemon which project it
// serves (`project_root` and `pid` in `GET /health` and `GET /api/status`),
// compares canonical paths with the current project, and refuses when they
// differ. There is deliberately no flag that overrides the refusal: stopping
// another project's daemon must be done from that project.

use std::path::Path;

use ta_mcp_gateway::daemon_port;

/// What a probe of a daemon address found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeAnswer {
    /// Nothing is listening (connection refused or unreachable).
    NoAnswer,
    /// A TA daemon answered. `project_root` is `None` for a daemon built
    /// before the status endpoints reported it.
    Daemon {
        project_root: Option<String>,
        pid: Option<u32>,
    },
    /// Something answered but it is not a (readable) TA daemon.
    Other(String),
}

/// Asks an address "who are you?". Injectable so tests can use a real local
/// listener or a canned answer.
pub trait DaemonProbe {
    fn identify(&self, base_url: &str) -> ProbeAnswer;
}

/// The real probe: `GET <base_url>/health`.
pub struct HttpProbe;

impl DaemonProbe for HttpProbe {
    fn identify(&self, base_url: &str) -> ProbeAnswer {
        // The connect phase gets its own, longer limit: refusing a connection to a
        // closed local port can take a second or two on Windows, and that must read as
        // "nothing is listening", not as "something accepted and then stayed silent".
        let client = match reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(8))
            .no_proxy()
            .build()
        {
            Ok(c) => c,
            Err(e) => return ProbeAnswer::Other(format!("cannot build an HTTP client: {e}")),
        };
        let url = format!("{base_url}/health");
        let resp = match client.get(&url).send() {
            Ok(r) => r,
            // Only a timeout AFTER the connection was made means "accepted but silent";
            // a connect failure or connect timeout means nothing is listening.
            Err(e) if e.is_timeout() && !e.is_connect() => {
                return ProbeAnswer::Other(format!(
                    "it accepted the connection but did not answer GET {url} within 8 seconds"
                ))
            }
            Err(_) => return ProbeAnswer::NoAnswer,
        };
        if !resp.status().is_success() {
            return ProbeAnswer::Other(format!("GET {url} returned HTTP {}", resp.status()));
        }
        let json: serde_json::Value = match resp.json() {
            Ok(j) => j,
            Err(_) => {
                return ProbeAnswer::Other(format!(
                    "GET {url} did not return the JSON a TA daemon returns"
                ))
            }
        };
        if json["status"] != "ok" || !json["version"].is_string() {
            return ProbeAnswer::Other(format!(
                "GET {url} did not return the health JSON a TA daemon returns"
            ));
        }
        ProbeAnswer::Daemon {
            project_root: json["project_root"].as_str().map(str::to_string),
            pid: json["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
        }
    }
}

/// Who holds a daemon port, from the point of view of one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortHolder {
    /// Nothing answers.
    Free,
    /// This project's own daemon. `legacy` is true when the daemon is too old
    /// to report its project and was recognised by this project's pid file.
    ThisProject { pid: Option<u32>, legacy: bool },
    /// Another project's daemon.
    OtherProject { root: String, pid: Option<u32> },
    /// Something answers that cannot be matched to this project.
    Unverified { detail: String },
}

/// Pure classification of a probe answer. `local_pid` is the live pid recorded
/// in this project's `.ta/daemon.pid` with its port, used only to recognise an
/// older daemon that does not report its project.
pub fn classify(
    project_root: &Path,
    answer: &ProbeAnswer,
    port: u16,
    local_pid_file: Option<(u32, Option<u16>)>,
) -> PortHolder {
    match answer {
        ProbeAnswer::NoAnswer => PortHolder::Free,
        ProbeAnswer::Other(detail) => PortHolder::Unverified {
            detail: detail.clone(),
        },
        ProbeAnswer::Daemon {
            project_root: Some(root),
            pid,
        } => {
            if daemon_port::same_project_root(project_root, root) {
                PortHolder::ThisProject {
                    pid: *pid,
                    legacy: false,
                }
            } else {
                PortHolder::OtherProject {
                    root: root.clone(),
                    pid: *pid,
                }
            }
        }
        ProbeAnswer::Daemon {
            project_root: None,
            pid,
        } => match local_pid_file {
            Some((local, local_port)) if local_port == Some(port) => PortHolder::ThisProject {
                pid: Some(local),
                legacy: true,
            },
            _ => PortHolder::Unverified {
                detail: format!(
                    "it is a TA daemon from an older build that does not report which project it \
                     serves{}, and this project's pid file does not record it",
                    pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
                ),
            },
        },
    }
}

/// Probe `base_url` and classify the answer for `project_root`.
pub fn identify_holder(
    project_root: &Path,
    base_url: &str,
    port: u16,
    probe: &dyn DaemonProbe,
    local_pid_file: Option<(u32, Option<u16>)>,
) -> PortHolder {
    classify(
        project_root,
        &probe.identify(base_url),
        port,
        local_pid_file,
    )
}

/// The refusal message for a port held by something that is not this
/// project's daemon, or `None` when the holder is free or ours.
///
/// `action` completes "Refusing to ...", for example "restart the daemon".
/// The text always names both projects, the port, the holder's pid and the
/// next step, and never suggests overriding.
pub fn refusal(
    action: &str,
    project_root: &Path,
    port: u16,
    configured_port: u16,
    holder: &PortHolder,
) -> Option<String> {
    let daemon_toml = ta_mcp_gateway::daemon_pid::daemon_toml_path(project_root);
    let fix = format!(
        "To run this project's own daemon, set `[server] port = <a free port>` in {} (ports {}-{} \
         are free to use) or remove the explicit port there so this project gets its own, then \
         run `ta daemon start`.",
        daemon_toml.display(),
        daemon_port::PORT_RANGE_START,
        daemon_port::PORT_RANGE_END
    );
    match holder {
        PortHolder::Free | PortHolder::ThisProject { .. } => None,
        PortHolder::OtherProject { root, pid } => Some(format!(
            "Refusing to {action}: port {port} is held by the daemon for {root}{}, not by this \
             project ({}), whose configured port is {configured_port}. Nothing was stopped or \
             started. To check or stop that daemon, run `ta daemon status` or `ta daemon stop` \
             from {root}. TA never stops another project's daemon from here. {fix}",
            pid.map(|p| format!(" (pid {p})")).unwrap_or_default(),
            project_root.display(),
        )),
        PortHolder::Unverified { detail } => Some(format!(
            "Refusing to {action}: something is answering on port {port} but it cannot be matched \
             to this project ({}): {detail}. Nothing was stopped or started. Find the owner with \
             `lsof -i :{port}` (macOS, Linux) or `netstat -ano | findstr :{port}` (Windows); if \
             it is a TA daemon, run `ta daemon status` from its project. {fix}",
            project_root.display(),
        )),
    }
}

/// Probe and refuse (as an error) unless the port is free or ours.
pub fn guard(
    action: &str,
    project_root: &Path,
    base_url: &str,
    port: u16,
    configured_port: u16,
    probe: &dyn DaemonProbe,
    local_pid_file: Option<(u32, Option<u16>)>,
) -> anyhow::Result<PortHolder> {
    let holder = identify_holder(project_root, base_url, port, probe, local_pid_file);
    if let Some(msg) = refusal(action, project_root, port, configured_port, &holder) {
        tracing::warn!(
            action,
            port,
            project_root = %project_root.display(),
            holder = ?holder,
            "refusing daemon lifecycle action: port is held by another daemon"
        );
        return Err(anyhow::anyhow!(msg));
    }
    Ok(holder)
}

/// A real local HTTP listener that answers like a daemon, for tests that need
/// "another project's daemon holds this port" without launching `ta-daemon`.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    pub struct FakeDaemon {
        pub port: u16,
        /// `"<METHOD> <path>"` for every request received.
        pub requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
    }

    impl FakeDaemon {
        /// `root` is what `/health` reports as `project_root` (`None` mimics
        /// an older daemon). `goals` are `(id, title)` pairs for the drain
        /// endpoint; empty means idle.
        pub fn spawn(root: Option<&str>, pid: u32, goals: &[(&str, &str)]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let health = match root {
                Some(r) => serde_json::json!({
                    "status": "ok", "version": "0.0.0-test", "project_root": r, "pid": pid,
                }),
                None => serde_json::json!({"status": "ok", "version": "0.0.0-test"}),
            }
            .to_string();
            let goals_json: Vec<serde_json::Value> = goals
                .iter()
                .map(|(id, title)| serde_json::json!({"id": id, "title": title}))
                .collect();
            let drain = serde_json::json!({
                "status": if goals.is_empty() { "clean" } else { "draining" },
                "active_goals": goals.len(),
                "active_sessions": 0,
                "inflight_launches": 0,
                "applying": false,
                "goals": goals_json,
                "project_root": root,
            })
            .to_string();
            let (reqs, stop2) = (requests.clone(), stop.clone());
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if stop2.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut conn) = conn else { continue };
                    let mut buf = [0u8; 4096];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let mut parts = head.split_whitespace();
                    let (method, path) = (
                        parts.next().unwrap_or("").to_string(),
                        parts.next().unwrap_or("").to_string(),
                    );
                    reqs.lock().unwrap().push(format!("{method} {path}"));
                    let body = match path.as_str() {
                        "/health" => health.clone(),
                        "/api/status" => health.clone(),
                        "/api/drain/status" => drain.clone(),
                        _ => "{}".to_string(),
                    };
                    let _ = write!(
                        conn,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
            });
            Self {
                port,
                requests,
                stop,
            }
        }

        pub fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        pub fn saw(&self, needle: &str) -> bool {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.contains(needle))
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(("127.0.0.1", self.port));
        }
    }

    /// A canned probe answer, no sockets.
    pub struct CannedProbe(pub super::ProbeAnswer);

    impl super::DaemonProbe for CannedProbe {
        fn identify(&self, _base_url: &str) -> super::ProbeAnswer {
            self.0.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn proj() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        d
    }

    #[test]
    fn same_root_is_this_project_even_through_symlinks_and_trailing_slash() {
        let d = proj();
        let canon = std::fs::canonicalize(d.path()).unwrap();
        let answer = ProbeAnswer::Daemon {
            project_root: Some(format!("{}/", canon.display())),
            pid: Some(5),
        };
        assert_eq!(
            classify(d.path(), &answer, 7700, None),
            PortHolder::ThisProject {
                pid: Some(5),
                legacy: false
            }
        );
    }

    #[test]
    fn different_root_is_another_project() {
        let (a, b) = (proj(), proj());
        let answer = ProbeAnswer::Daemon {
            project_root: Some(a.path().display().to_string()),
            pid: Some(77),
        };
        match classify(b.path(), &answer, 7700, None) {
            PortHolder::OtherProject { root, pid } => {
                assert_eq!(root, a.path().display().to_string());
                assert_eq!(pid, Some(77));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn older_daemon_is_ours_only_when_our_pid_file_records_that_port() {
        let d = proj();
        let legacy = ProbeAnswer::Daemon {
            project_root: None,
            pid: None,
        };
        assert_eq!(
            classify(d.path(), &legacy, 7710, Some((42, Some(7710)))),
            PortHolder::ThisProject {
                pid: Some(42),
                legacy: true
            }
        );
        assert!(matches!(
            classify(d.path(), &legacy, 7710, Some((42, Some(7799)))),
            PortHolder::Unverified { .. }
        ));
        assert!(matches!(
            classify(d.path(), &legacy, 7710, None),
            PortHolder::Unverified { .. }
        ));
    }

    #[test]
    fn refusal_names_both_projects_the_port_the_pid_and_the_next_step() {
        let (a, b) = (proj(), proj());
        let holder = PortHolder::OtherProject {
            root: a.path().display().to_string(),
            pid: Some(4242),
        };
        let msg = refusal("restart the daemon", b.path(), 7700, 7700, &holder).unwrap();
        for needle in [
            "Refusing to restart the daemon",
            "port 7700",
            "pid 4242",
            &a.path().display().to_string(),
            &b.path().display().to_string(),
            "ta daemon status",
            "[server] port",
            "never stops another project's daemon",
        ] {
            assert!(msg.contains(needle), "missing {needle:?} in: {msg}");
        }
        assert!(!msg.contains("--force-other"), "{msg}");
        assert!(refusal("stop", b.path(), 1, 1, &PortHolder::Free).is_none());
    }

    #[test]
    fn real_listener_for_another_project_is_identified_over_http() {
        let (a, b) = (proj(), proj());
        let canon_a = std::fs::canonicalize(a.path()).unwrap();
        let fake = FakeDaemon::spawn(Some(&canon_a.display().to_string()), 999, &[]);
        let holder = identify_holder(b.path(), &fake.url(), fake.port, &HttpProbe, None);
        assert!(
            matches!(holder, PortHolder::OtherProject { pid: Some(999), .. }),
            "{holder:?}"
        );
        let own = identify_holder(a.path(), &fake.url(), fake.port, &HttpProbe, None);
        assert!(matches!(own, PortHolder::ThisProject { .. }), "{own:?}");
    }

    #[test]
    fn nothing_listening_is_free() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let d = proj();
        let holder = identify_holder(
            d.path(),
            &format!("http://127.0.0.1:{port}"),
            port,
            &HttpProbe,
            None,
        );
        assert_eq!(holder, PortHolder::Free);
    }

    #[test]
    fn a_non_ta_listener_is_unverified_not_free() {
        // Answers HTTP 200 with a body that is not daemon health JSON.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut c, _)) = l.accept() {
                let mut b = [0u8; 1024];
                let _ = c.read(&mut b);
                let _ = c.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                );
            }
        });
        let d = proj();
        let holder = identify_holder(
            d.path(),
            &format!("http://127.0.0.1:{port}"),
            port,
            &HttpProbe,
            None,
        );
        assert!(
            matches!(holder, PortHolder::Unverified { .. }),
            "{holder:?}"
        );
    }

    #[test]
    fn canned_probe_drives_guard_without_sockets() {
        let d = proj();
        let probe = CannedProbe(ProbeAnswer::Other("weird".into()));
        let err = guard("stop the daemon", d.path(), "http://x", 1, 1, &probe, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("weird") && err.contains("lsof"), "{err}");
    }
}
