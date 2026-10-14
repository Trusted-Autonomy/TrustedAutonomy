// mediation.rs — The out-of-sandbox MCP gateway and its in-sandbox bridge
// (v0.17.12.6).
//
// A sandboxed agent cannot run `ta serve` itself: the gateway reads `.ta/`,
// the control-plane secrets and the daemon, all of which the box denies. So
// `ta run` starts `ta serve --socket <path>` OUTSIDE the sandbox, in the
// staging cwd with the launcher's environment, and the agent reaches it
// through `ta mcp-bridge --endpoint <path>`, a stdio-to-socket relay.
//
// Wire protocol, one connection per MCP session or credential lookup:
//   1. Client sends `Bearer <token>\n` (MCP) or `Bearer <token> credential\n`
//      followed by `host=<host>\n` (credential lookup).
//   2. Server answers `OK\n` and speaks MCP, or answers a credential reply
//      (see `ta_credential_broker::encode_endpoint_reply`), or `Unauthorized\n`.
//
// The socket lives in a short temp dir (macOS limits socket paths to 104
// bytes), mode 0700 for the dir and 0600 for the socket. The token is a
// per-goal random value passed through the environment, never written to a
// file. Windows named pipes are not implemented: on Windows the capability is
// reported missing and the launch follows the degraded-mode table.

use std::path::{Path, PathBuf};

/// Environment variable carrying the per-goal connection token.
pub const ENV_TOKEN: &str = "TA_MCP_CONNECTION_TOKEN";

/// A fresh unguessable per-goal token.
pub fn new_connection_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// What a handshake line asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Handshake {
    Mcp,
    Credential,
    Rejected,
}

/// Parse and check the first line against the expected token.
pub fn parse_handshake(line: &str, expected: &str) -> Handshake {
    let Some(rest) = line.trim().strip_prefix("Bearer ") else {
        return Handshake::Rejected;
    };
    let mut parts = rest.split_whitespace();
    let token = parts.next().unwrap_or("");
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Handshake::Rejected;
    }
    match parts.next() {
        None => Handshake::Mcp,
        Some(p) if p == ta_credential_broker::CREDENTIAL_PURPOSE => Handshake::Credential,
        Some(_) => Handshake::Rejected,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `ta serve --socket`: accept loop, a fresh gateway server per connection.
#[cfg(unix)]
pub fn serve_socket(
    project_root: &Path,
    socket: &Path,
    credential_root: Option<&Path>,
) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    let token = std::env::var(ENV_TOKEN).map_err(|_| {
        anyhow::anyhow!(
            "ta serve --socket: {ENV_TOKEN} is not set. The launcher (`ta run`) sets it; \
             do not start a socket gateway by hand without a token."
        )
    })?;
    if token.len() < 16 {
        anyhow::bail!("ta serve --socket: {ENV_TOKEN} is too short to be a connection token");
    }
    // Fail early (and visibly in the gateway log) if the server cannot be built.
    super::serve::build_server(project_root)?;
    let credential_root = credential_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project_root.to_path_buf());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket).map_err(|e| {
            anyhow::anyhow!("ta serve --socket: cannot bind {}: {e}", socket.display())
        })?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        }
        tracing::info!(socket = %socket.display(), "gateway listening");
        let project_root = project_root.to_path_buf();
        loop {
            let (stream, _) = listener.accept().await?;
            let token = token.clone();
            let project_root = project_root.clone();
            let credential_root = credential_root.clone();
            tokio::spawn(async move {
                let (rd, mut wr) = stream.into_split();
                let mut rd = BufReader::new(rd);
                let mut line = String::new();
                if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                match parse_handshake(&line, &token) {
                    Handshake::Rejected => {
                        let _ = wr.write_all(b"Unauthorized\n").await;
                        tracing::warn!("gateway connection rejected: bad handshake");
                    }
                    Handshake::Credential => {
                        let mut req = String::new();
                        let _ = rd.read_line(&mut req).await;
                        let host = req.trim().strip_prefix("host=").unwrap_or("").to_string();
                        let reply = tokio::task::spawn_blocking(move || {
                            let host = host.split(':').next().unwrap_or("").to_string();
                            let r = ta_credential_broker::resolve_for_host(
                                &credential_root,
                                &host,
                                true,
                            );
                            ta_credential_broker::encode_endpoint_reply(&r)
                        })
                        .await
                        .unwrap_or_else(|e| format!("ERR credential lookup panicked: {e}\n"));
                        let _ = wr.write_all(reply.as_bytes()).await;
                    }
                    Handshake::Mcp => {
                        if wr.write_all(b"OK\n").await.is_err() || wr.flush().await.is_err() {
                            return;
                        }
                        match super::serve::build_server(&project_root) {
                            Ok(server) => match server.serve((rd, wr)).await {
                                Ok(svc) => {
                                    let _ = svc.waiting().await;
                                }
                                Err(e) => tracing::warn!(error = %e, "MCP session failed"),
                            },
                            Err(e) => tracing::error!(error = %e, "cannot build gateway server"),
                        }
                    }
                }
            });
        }
    })
}

#[cfg(not(unix))]
pub fn serve_socket(_: &Path, _: &Path, _: Option<&Path>) -> anyhow::Result<()> {
    anyhow::bail!(
        "ta serve --socket needs Unix domain sockets, which this platform build does not \
         provide. Windows named-pipe support is not yet shipped; sandboxed launches report \
         mediation.gateway_outside as missing here."
    )
}

/// `ta mcp-bridge --endpoint`: relay stdio to the gateway socket.
#[cfg(unix)]
pub fn bridge(endpoint: &Path) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let token = std::env::var(ENV_TOKEN).map_err(|_| {
        anyhow::anyhow!(
            "ta mcp-bridge: {ENV_TOKEN} is not set in the agent environment. The bridge is \
             started by an agent that `ta run` launched; the token is passed there automatically."
        )
    })?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let stream = tokio::net::UnixStream::connect(endpoint)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "ta mcp-bridge: cannot connect to the goal's gateway at {}: {e}. The gateway \
                 may have exited; re-run the goal.",
                    endpoint.display()
                )
            })?;
        let (rd, mut wr) = stream.into_split();
        wr.write_all(format!("Bearer {token}\n").as_bytes()).await?;
        let mut rd = BufReader::new(rd);
        let mut ack = String::new();
        rd.read_line(&mut ack).await?;
        if ack.trim() != "OK" {
            anyhow::bail!(
                "ta mcp-bridge: the gateway at {} refused the connection ({}). The token does \
                 not match this goal's gateway.",
                endpoint.display(),
                ack.trim()
            );
        }
        let to_server = async {
            let mut stdin = tokio::io::stdin();
            let _ = tokio::io::copy(&mut stdin, &mut wr).await;
            let _ = wr.shutdown().await;
        };
        let to_client = async {
            let mut stdout = tokio::io::stdout();
            let _ = tokio::io::copy(&mut rd, &mut stdout).await;
            let _ = stdout.flush().await;
        };
        tokio::select! { _ = to_server => {}, _ = to_client => {} }
        Ok(())
    })
}

#[cfg(not(unix))]
pub fn bridge(_: &Path) -> anyhow::Result<()> {
    anyhow::bail!("ta mcp-bridge needs Unix domain sockets; not available on this platform build")
}

/// A running out-of-sandbox gateway. Kills the process and removes the
/// socket directory on drop.
pub struct GatewayGuard {
    child: std::process::Child,
    dir: PathBuf,
    pub socket: PathBuf,
    pub token: String,
}

impl Drop for GatewayGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Start `ta serve --socket` outside the sandbox and wait for the socket.
///
/// `gateway_env` holds the extra variables the gateway needs that the agent
/// env carries (chat-mode settings); the launcher's own environment is inherited.
#[cfg(unix)]
pub fn spawn_gateway(
    staging: &Path,
    credential_root: &Path,
    gateway_env: &std::collections::HashMap<String, String>,
    wait: std::time::Duration,
) -> anyhow::Result<GatewayGuard> {
    use std::os::unix::fs::PermissionsExt;
    let token = new_connection_token();
    let dir = std::env::temp_dir().join(format!(
        "ta-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let socket = dir.join("mcp.sock");
    let log = dir.join("gateway.log");
    let ta_bin = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("cannot locate the running ta binary: {e}"))?;
    let log_file = std::fs::File::create(&log)?;
    let mut cmd = std::process::Command::new(&ta_bin);
    cmd.arg("serve")
        .arg("--socket")
        .arg(&socket)
        .arg("--credential-root")
        .arg(credential_root)
        .current_dir(staging)
        .envs(gateway_env)
        .env("TA_IS_STAGING", "1")
        .env(ENV_TOKEN, &token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log_file);
    let child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "could not start the out-of-sandbox gateway ({} serve --socket {}): {e}",
            ta_bin.display(),
            socket.display()
        )
    })?;
    let mut guard = GatewayGuard {
        child,
        dir: dir.clone(),
        socket: socket.clone(),
        token,
    };
    let start = std::time::Instant::now();
    loop {
        if socket.exists() {
            return Ok(guard);
        }
        if let Ok(Some(status)) = guard.child.try_wait() {
            let tail = std::fs::read_to_string(&log).unwrap_or_default();
            anyhow::bail!(
                "the out-of-sandbox gateway exited early ({status}) before opening {}. \
                 Gateway log ({}): {}",
                socket.display(),
                log.display(),
                tail.lines().last().unwrap_or("(empty)")
            );
        }
        if start.elapsed() > wait {
            anyhow::bail!(
                "the out-of-sandbox gateway did not open {} within {}s. Gateway log: {}",
                socket.display(),
                wait.as_secs(),
                log.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(not(unix))]
pub fn spawn_gateway(
    _: &Path,
    _: &Path,
    _: &std::collections::HashMap<String, String>,
    _: std::time::Duration,
) -> anyhow::Result<GatewayGuard> {
    anyhow::bail!("the out-of-sandbox gateway needs Unix domain sockets (not on this platform)")
}

/// Rewrite an agent MCP config so the `ta` server runs `ta mcp-bridge`
/// against `endpoint`. Other servers are kept as-is. The token stays an
/// environment placeholder so it never lands in a file.
pub fn bridged_mcp_config(
    base: Option<&serde_json::Value>,
    ta_binary: &str,
    endpoint: &Path,
) -> serde_json::Value {
    let mut config = base
        .cloned()
        .filter(|v| v.get("mcpServers").is_some())
        .unwrap_or_else(|| serde_json::json!({ "mcpServers": {} }));
    config["mcpServers"]["ta"] = serde_json::json!({
        "command": ta_binary,
        "args": ["mcp-bridge", "--endpoint", endpoint.display().to_string()],
        "env": { ENV_TOKEN: format!("${{{ENV_TOKEN}}}") }
    });
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_checks_token_and_purpose() {
        assert_eq!(parse_handshake("Bearer abc\n", "abc"), Handshake::Mcp);
        assert_eq!(
            parse_handshake("Bearer abc credential\n", "abc"),
            Handshake::Credential
        );
        assert_eq!(parse_handshake("Bearer abd\n", "abc"), Handshake::Rejected);
        assert_eq!(
            parse_handshake("Bearer abc other\n", "abc"),
            Handshake::Rejected
        );
        assert_eq!(parse_handshake("abc\n", "abc"), Handshake::Rejected);
        assert_eq!(parse_handshake("", "abc"), Handshake::Rejected);
    }

    #[test]
    fn tokens_are_long_and_unique() {
        let (a, b) = (new_connection_token(), new_connection_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    #[test]
    fn bridged_config_replaces_only_ta_and_holds_no_token() {
        let base = serde_json::json!({"mcpServers": {
            "ta": {"command": "ta", "args": ["serve"]},
            "meridian-analytics": {"command": "meridian", "args": ["serve"]}
        }});
        let out = bridged_mcp_config(Some(&base), "/bin/ta", Path::new("/tmp/ta-1/mcp.sock"));
        assert_eq!(out["mcpServers"]["ta"]["args"][0], "mcp-bridge");
        assert_eq!(out["mcpServers"]["ta"]["args"][2], "/tmp/ta-1/mcp.sock");
        assert_eq!(
            out["mcpServers"]["meridian-analytics"]["command"],
            "meridian"
        );
        assert_eq!(
            out["mcpServers"]["ta"]["env"][ENV_TOKEN],
            "${TA_MCP_CONNECTION_TOKEN}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn credential_reply_over_a_socket_reaches_the_client() {
        use std::io::{BufRead, BufReader, Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut rd = BufReader::new(stream.try_clone().unwrap());
            let mut hs = String::new();
            rd.read_line(&mut hs).unwrap();
            assert_eq!(
                parse_handshake(&hs, "tok-tok-tok-tok-tok"),
                Handshake::Credential
            );
            let mut req = String::new();
            rd.read_line(&mut req).unwrap();
            assert_eq!(req.trim(), "host=github.com");
            let mut s = stream;
            s.write_all(b"OK\nsecret=s3\ncredential_name=gh\nconnector_id=github\n")
                .unwrap();
            let mut rest = String::new();
            let _ = rd.read_to_string(&mut rest);
        });
        let got =
            ta_credential_broker::resolve_via_endpoint(&sock, "tok-tok-tok-tok-tok", "github.com")
                .unwrap()
                .unwrap();
        assert_eq!(got.secret, "s3");
        server.join().unwrap();
    }
}
