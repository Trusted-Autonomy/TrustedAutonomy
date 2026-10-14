// windows_relay.rs — egress transport for the Windows AppContainer (v0.17.12.9)
//
// An AppContainer process cannot reach host loopback, and TA grants no loopback
// exemption. The per-goal egress proxy listens on host loopback, so the only
// thing that crosses the boundary is a named pipe whose DACL names the goal's
// container SID (see `windows_policy::pipe_sddl`):
//
//   agent --TCP--> 127.0.0.1:PORT (inside the container, served by ta-sandbox-exec)
//         --named pipe (DACL: this container only)--> host relay (outside)
//         --TCP--> egress proxy on host 127.0.0.1:PORT
//
// Both ends use the same port number, so the proxy variables the agent already
// receives (`HTTPS_PROXY=http://127.0.0.1:PORT`) work unchanged. The relays move
// bytes only; hostname policy and the audit log stay in the proxy.
//
// This module is platform-neutral so its logic is tested on every OS. Only the
// pipe open (`open_pipe`) and the host server (in `ta-cli`, on tokio) are
// Windows-specific. Same-container loopback (agent to the helper's listener)
// is the one assumption that needs a Windows run to confirm; the conformance
// test `windows_conformance::loopback_relay_inside_container` does that.
//
// Known limit: when the agent half-closes its TCP write side and keeps reading,
// the pipe is not half-closed (a byte pipe has no half-close), so the relay waits
// for the proxy to finish and close. HTTP and CONNECT tunnels do not rely on it.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

/// Exit status when the relay cannot start (matches the Linux helper).
pub const EXIT_RELAY_FAILED: i32 = 125;
/// Exit status when the agent command cannot be started.
pub const EXIT_COMMAND_NOT_FOUND: i32 = 127;

const HELPER_USAGE: &str =
    "usage: ta-sandbox-exec [--egress-pipe PIPE --egress-port PORT] -- COMMAND [ARGS...]";

/// How long `open_pipe` keeps retrying while every pipe instance is busy.
pub const PIPE_BUSY_WAIT: Duration = Duration::from_secs(5);

/// Arguments of the in-container helper on Windows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelperArgs {
    /// Named pipe the in-container relay forwards each connection to.
    pub egress_pipe: Option<String>,
    /// Loopback port the in-container relay listens on.
    pub egress_port: Option<u16>,
    /// The agent command and its arguments.
    pub command: Vec<String>,
}

impl HelperArgs {
    pub fn to_args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if let (Some(p), Some(port)) = (&self.egress_pipe, self.egress_port) {
            a.push("--egress-pipe".to_string());
            a.push(p.clone());
            a.push("--egress-port".to_string());
            a.push(port.to_string());
        }
        a.push("--".to_string());
        a.extend(self.command.iter().cloned());
        a
    }

    pub fn parse(argv: &[String]) -> Result<Self, String> {
        let mut out = HelperArgs::default();
        let mut it = argv.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--" => {
                    out.command = it.cloned().collect();
                    break;
                }
                "--egress-pipe" => {
                    let v = it.next().ok_or("--egress-pipe needs a pipe name")?;
                    if !crate::windows_policy::is_pipe_path(v) {
                        return Err(format!("--egress-pipe {v:?} is not a \\\\.\\pipe\\ name"));
                    }
                    out.egress_pipe = Some(v.clone());
                }
                "--egress-port" => {
                    let v = it.next().ok_or("--egress-port needs a port")?;
                    out.egress_port = Some(
                        v.parse::<u16>()
                            .map_err(|_| format!("--egress-port {v:?} is not a port number"))?,
                    );
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        if out.command.is_empty() {
            return Err("no agent command after `--`".to_string());
        }
        if out.egress_pipe.is_some() != out.egress_port.is_some() {
            return Err("--egress-pipe and --egress-port must be given together".to_string());
        }
        Ok(out)
    }
}

/// Copy `from` into `to` until EOF, then run `done`.
fn pump<R: Read, W: Write>(mut from: R, mut to: W, done: impl FnOnce()) {
    let _ = io::copy(&mut from, &mut to);
    let _ = to.flush();
    done();
}

/// Connect `tcp` to a pipe-like peer given as separate read and write halves.
/// Returns when the peer's output ends.
pub fn bridge<PR, PW>(tcp: TcpStream, pipe_r: PR, pipe_w: PW)
where
    PR: Read + Send + 'static,
    PW: Write + Send + 'static,
{
    let Ok(tcp_r) = tcp.try_clone() else { return };
    let t = thread::spawn(move || {
        // Agent to proxy. The pipe write half is dropped at EOF; a byte pipe has
        // no half-close, so the peer learns of the end when the other
        // direction finishes.
        pump(tcp_r, pipe_w, || {});
    });
    let w = tcp;
    pump(pipe_r, &w, || {
        let _ = w.shutdown(Shutdown::Both);
    });
    let _ = t.join();
}

/// Open the client end of a named pipe, retrying while every instance is busy
/// (`ERROR_PIPE_BUSY`, 231) up to `PIPE_BUSY_WAIT`.
pub fn open_pipe(name: &str) -> io::Result<std::fs::File> {
    let start = Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
        {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(231) && start.elapsed() < PIPE_BUSY_WAIT => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Inside the container: accept TCP on loopback and hand each connection to
/// `connect`, which returns the pipe halves. Runs until the process exits.
pub fn serve_tcp_with<F, PR, PW>(listener: TcpListener, label: String, connect: F)
where
    F: Fn() -> io::Result<(PR, PW)> + Send + Sync + 'static,
    PR: Read + Send + 'static,
    PW: Write + Send + 'static,
{
    let connect = std::sync::Arc::new(connect);
    for conn in listener.incoming() {
        let Ok(tcp) = conn else { continue };
        let connect = connect.clone();
        let label = label.clone();
        thread::spawn(move || match connect() {
            Ok((r, w)) => bridge(tcp, r, w),
            Err(e) => eprintln!(
                "[sandbox] egress relay cannot reach {label}: {e}. The host relay may have \
                 stopped or the pipe is not open to this container; restart the goal."
            ),
        });
    }
}

/// Inside the container: loopback TCP to the host's named pipe.
pub fn serve_tcp_to_pipe(listener: TcpListener, pipe: &str) {
    let name = pipe.to_string();
    let label = name.clone();
    serve_tcp_with(listener, label, move || {
        let r = open_pipe(&name)?;
        let w = r.try_clone()?;
        Ok((r, w))
    });
}

/// The helper's `main` on Windows: start the relay (if asked), run the agent in
/// this same container token, return its exit code.
pub fn run_helper(argv: &[String]) -> i32 {
    let args = match HelperArgs::parse(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ta-sandbox-exec: {e}\n{HELPER_USAGE}");
            return 2;
        }
    };
    if let (Some(pipe), Some(port)) = (args.egress_pipe.clone(), args.egress_port) {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => {
                thread::spawn(move || serve_tcp_to_pipe(l, &pipe));
            }
            Err(e) => {
                eprintln!(
                    "ta-sandbox-exec: cannot listen on 127.0.0.1:{port} inside the AppContainer: \
                     {e}. The agent was not started because it would have no network path. \
                     Check that loopback is available to the container, or remove the egress \
                     hosts from the sandbox policy."
                );
                return EXIT_RELAY_FAILED;
            }
        }
    }
    let (cmd, rest) = args
        .command
        .split_first()
        .expect("parse guarantees a command");
    match std::process::Command::new(cmd).args(rest).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!(
                "ta-sandbox-exec: cannot start the agent {cmd:?}: {e}. Check that the program \
                 is readable by the AppContainer (toolchain grants are listed in the launch banner)."
            );
            EXIT_COMMAND_NOT_FOUND
        }
    }
}

/// Where the helper binary is found next to `ta` (Windows file name).
pub fn helper_path_hint() -> PathBuf {
    PathBuf::from("ta-sandbox-exec.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn helper_args_round_trip() {
        let a = HelperArgs {
            egress_pipe: Some(r"\\.\pipe\ta-egress-abcd1234-n".into()),
            egress_port: Some(40123),
            command: strings(&["claude.exe", "--print", "hi there"]),
        };
        assert_eq!(HelperArgs::parse(&a.to_args()).unwrap(), a);
    }

    #[test]
    fn helper_args_without_egress() {
        let a = HelperArgs::parse(&strings(&["--", "cmd.exe", "/c", "dir"])).unwrap();
        assert_eq!(a.egress_pipe, None);
        assert_eq!(a.command, strings(&["cmd.exe", "/c", "dir"]));
    }

    #[test]
    fn helper_args_reject_bad_input_with_actionable_messages() {
        assert!(HelperArgs::parse(&strings(&["--",]))
            .unwrap_err()
            .contains("no agent command"));
        assert!(HelperArgs::parse(&strings(&["--bogus", "--", "x"]))
            .unwrap_err()
            .contains("unknown argument"));
        assert!(HelperArgs::parse(&strings(&[
            "--egress-pipe",
            "C:\\x",
            "--egress-port",
            "1",
            "--",
            "x"
        ]))
        .unwrap_err()
        .contains("not a"));
        assert!(
            HelperArgs::parse(&strings(&["--egress-port", "99999", "--", "x"]))
                .unwrap_err()
                .contains("port number")
        );
        assert!(
            HelperArgs::parse(&strings(&["--egress-port", "80", "--", "x"]))
                .unwrap_err()
                .contains("together")
        );
    }

    #[test]
    fn command_arguments_that_look_like_flags_are_not_parsed() {
        let a = HelperArgs::parse(&strings(&["--", "agent", "--egress-port", "1"])).unwrap();
        assert_eq!(a.command, strings(&["agent", "--egress-port", "1"]));
        assert_eq!(a.egress_port, None);
    }

    #[test]
    fn tcp_to_peer_relay_moves_bytes_both_ways() {
        // The "pipe" is a TCP echo server, so the relay logic runs on any OS.
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_addr = echo.local_addr().unwrap();
        thread::spawn(move || {
            for c in echo.incoming().flatten() {
                thread::spawn(move || {
                    let mut c2 = c.try_clone().unwrap();
                    let mut c = c;
                    let mut buf = [0u8; 64];
                    while let Ok(n) = c.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let up: Vec<u8> = buf[..n].iter().map(|b| b.to_ascii_uppercase()).collect();
                        if c2.write_all(&up).is_err() {
                            break;
                        }
                    }
                });
            }
        });

        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let front_addr = front.local_addr().unwrap();
        thread::spawn(move || {
            serve_tcp_with(front, "echo".into(), move || {
                let r = TcpStream::connect(echo_addr)?;
                let w = r.try_clone()?;
                Ok((r, w))
            })
        });

        let mut client = TcpStream::connect(front_addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(b"hello relay").unwrap();
        let mut got = vec![0u8; 11];
        client.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"HELLO RELAY");
    }

    #[test]
    fn unreachable_peer_closes_the_client_connection() {
        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = front.local_addr().unwrap();
        thread::spawn(move || {
            serve_tcp_with(
                front,
                "nowhere".into(),
                || -> io::Result<(TcpStream, TcpStream)> {
                    Err(io::Error::new(io::ErrorKind::NotFound, "no pipe"))
                },
            )
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 8];
        // The connection is dropped without data: EOF or reset, never a hang.
        match client.read(&mut buf) {
            Ok(0) | Err(_) => {}
            Ok(n) => panic!("unexpected {n} bytes"),
        }
    }

    #[test]
    fn opening_a_missing_pipe_fails_fast_without_the_busy_wait() {
        let t = Instant::now();
        assert!(open_pipe(r"\\.\pipe\ta-test-does-not-exist-9c4ee279").is_err());
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn run_helper_reports_usage_errors_with_exit_2() {
        assert_eq!(run_helper(&strings(&["--bogus"])), 2);
        assert_eq!(run_helper(&[]), 2);
    }

    #[test]
    fn run_helper_exit_code_for_a_missing_command() {
        let code = run_helper(&strings(&["--", "ta-no-such-program-9c4ee279"]));
        assert_eq!(code, EXIT_COMMAND_NOT_FOUND);
    }
}
