// gateway_socket_bridge.rs — Black-box proof of the out-of-sandbox gateway
// (v0.17.12.6): `ta serve --socket` serves MCP on a Unix socket, and
// `ta mcp-bridge` relays an agent's stdio to it. Runs the real `ta` binary.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct Gateway {
    child: std::process::Child,
}
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_gateway(project: &TempDir, socket: &std::path::Path) -> Gateway {
    let child = Command::new(env!("CARGO_BIN_EXE_ta"))
        .arg("--project-root")
        .arg(project.path())
        .arg("--no-version-check")
        .arg("serve")
        .arg("--socket")
        .arg(socket)
        .current_dir(project.path())
        .env("TA_MCP_CONNECTION_TOKEN", TOKEN)
        .env_remove("TA_PROJECT_ROOT")
        .env_remove("TA_CHAT_MODE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start gateway");
    let start = Instant::now();
    while !socket.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "gateway socket never appeared"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Gateway { child }
}

fn bridge(socket: &std::path::Path, token: &str) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_ta"))
        .arg("--no-version-check")
        .arg("mcp-bridge")
        .arg("--endpoint")
        .arg(socket)
        .env("TA_MCP_CONNECTION_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start bridge")
}

#[test]
fn bridge_relays_an_mcp_initialize_to_the_out_of_sandbox_gateway() {
    let project = TempDir::new().unwrap();
    // Short path: macOS limits socket paths to 104 bytes.
    let sock_dir = tempfile::Builder::new()
        .prefix("ta-t")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = sock_dir.path().join("mcp.sock");
    let _gw = start_gateway(&project, &socket);

    let mut b = bridge(&socket, TOKEN);
    let mut stdin = b.stdin.take().unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    BufReader::new(b.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(
        line.contains("\"result\"") && line.contains("serverInfo"),
        "got: {line}"
    );
    drop(stdin);
    let _ = b.kill();
    let _ = b.wait();
}

#[test]
fn bridge_with_the_wrong_token_is_refused_with_an_actionable_error() {
    let project = TempDir::new().unwrap();
    let sock_dir = tempfile::Builder::new()
        .prefix("ta-t")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = sock_dir.path().join("mcp.sock");
    let _gw = start_gateway(&project, &socket);

    let b = bridge(&socket, "ffffffffffffffffffffffffffffffff");
    let out = b.wait_with_output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("refused the connection"), "stderr: {err}");
}

#[test]
fn serve_socket_without_a_token_fails_with_guidance() {
    let project = TempDir::new().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ta"))
        .arg("--project-root")
        .arg(project.path())
        .arg("--no-version-check")
        .args(["serve", "--socket", "/tmp/ta-never.sock"])
        .env_remove("TA_MCP_CONNECTION_TOKEN")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("TA_MCP_CONNECTION_TOKEN"));
}
