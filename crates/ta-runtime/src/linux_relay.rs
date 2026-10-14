// linux_relay.rs — egress transport for the Linux network namespace (v0.17.12.8)
//
// The bubblewrap sandbox unshares the network namespace, so the agent has no
// route to the per-goal egress proxy that listens on host loopback. The only
// thing that crosses the boundary is a Unix socket in a bind-mounted directory:
//
//   agent --TCP--> 127.0.0.1:PORT (inside netns, served by ta-sandbox-exec)
//         --unix socket (bind-mounted)--> HostRelay (outside)
//         --TCP--> egress proxy on host 127.0.0.1:PORT
//
// Both ends use the same port number, so the proxy environment variables the
// agent already receives (`HTTPS_PROXY=http://127.0.0.1:PORT`) work unchanged.
// The relays move bytes only. Hostname policy and audit stay in the proxy.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Copy `from` into `to` until EOF, then half-close the write side.
fn pump<R: io::Read, W: io::Write>(mut from: R, mut to: W, shutdown: impl FnOnce()) {
    let _ = io::copy(&mut from, &mut to);
    let _ = to.flush();
    shutdown();
}

fn bridge_tcp_unix(tcp: TcpStream, unix: UnixStream) {
    let (tcp_r, unix_r) = match (tcp.try_clone(), unix.try_clone()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return,
    };
    let t = thread::spawn(move || {
        let w = unix;
        pump(tcp_r, &w, || {
            let _ = w.shutdown(std::net::Shutdown::Write);
        });
    });
    let w = tcp;
    pump(unix_r, &w, || {
        let _ = w.shutdown(std::net::Shutdown::Write);
    });
    let _ = t.join();
}

/// Inside the sandbox: accept TCP on loopback, forward each connection to the
/// Unix socket. Runs until the process is killed (it dies with its parent).
pub fn serve_tcp_to_unix(listener: TcpListener, socket: &Path) {
    let socket: PathBuf = socket.to_path_buf();
    for conn in listener.incoming() {
        let Ok(tcp) = conn else { continue };
        let socket = socket.clone();
        thread::spawn(move || match UnixStream::connect(&socket) {
            Ok(unix) => bridge_tcp_unix(tcp, unix),
            Err(e) => {
                eprintln!(
                    "[sandbox] egress relay cannot reach {}: {e}. The host relay may have \
                     stopped; restart the goal.",
                    socket.display()
                );
            }
        });
    }
}

/// Outside the sandbox: listen on a Unix socket and forward each connection to
/// the egress proxy's loopback port. Stops and removes the socket on drop.
pub struct HostRelay {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
}

impl HostRelay {
    /// `dir` is created 0700 and is what the sandbox bind-mounts; the socket is
    /// `dir/egress.sock`.
    pub fn start(dir: &Path, proxy_port: u16) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| {
            format!(
                "cannot create the egress relay directory {}: {e}. Check permissions on the \
                 parent directory or set TMPDIR to a writable location.",
                dir.display()
            )
        })?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {} to its owner (0700): {e}", dir.display()))?;
        let socket = dir.join("egress.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).map_err(|e| {
            format!(
                "cannot bind the egress relay socket {}: {e}. Unix socket paths are limited to \
                 about 100 bytes; set TMPDIR to a shorter directory.",
                socket.display()
            )
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("cannot configure the egress relay socket: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((unix, _)) => {
                        let _ = unix.set_nonblocking(false);
                        thread::spawn(move || {
                            match TcpStream::connect(("127.0.0.1", proxy_port)) {
                                Ok(tcp) => bridge_tcp_unix(tcp, unix),
                                Err(e) => tracing::warn!(
                                    port = proxy_port,
                                    error = %e,
                                    "egress relay cannot reach the egress proxy; the agent's \
                                     outbound request was dropped"
                                ),
                            }
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "egress relay accept failed");
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        });
        Ok(Self { socket, stop })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl Drop for HostRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.socket);
        if let Some(dir) = self.socket.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for c in l.incoming() {
                let mut c = c.unwrap();
                thread::spawn(move || {
                    let mut buf = [0u8; 256];
                    while let Ok(n) = c.read(&mut buf) {
                        if n == 0 || c.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    #[test]
    fn bytes_cross_both_relays_to_the_proxy_and_back() {
        let proxy_port = echo_server();
        let dir = tempfile::tempdir().unwrap();
        let relay = HostRelay::start(&dir.path().join("e"), proxy_port).unwrap();
        let sock = relay.socket_path().to_path_buf();

        let inner = TcpListener::bind("127.0.0.1:0").unwrap();
        let inner_port = inner.local_addr().unwrap().port();
        thread::spawn(move || serve_tcp_to_unix(inner, &sock));

        let mut c = TcpStream::connect(("127.0.0.1", inner_port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(b"CONNECT example.com:443").unwrap();
        let mut got = [0u8; 23];
        c.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"CONNECT example.com:443");
    }

    #[test]
    fn relay_directory_is_owner_only_and_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("egress");
        let relay = HostRelay::start(&d, 9).unwrap();
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(relay.socket_path().exists());
        drop(relay);
        assert!(!d.exists(), "relay directory should be removed on drop");
    }

    #[test]
    fn unreachable_proxy_closes_the_connection_instead_of_hanging() {
        // Port 1 is not listening; the host relay drops the connection.
        let dir = tempfile::tempdir().unwrap();
        let relay = HostRelay::start(&dir.path().join("e"), 1).unwrap();
        let mut u = UnixStream::connect(relay.socket_path()).unwrap();
        u.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(u.read(&mut buf).unwrap_or(0), 0);
    }
}
