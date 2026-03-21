// transport.rs — MCP transport abstraction (v0.13.2).
//
// Provides pluggable transport backends for the TA MCP server:
//
//   - Stdio (default): agent spawned as child process; MCP over stdin/stdout.
//     Backward-compatible with all existing .mcp.json setups.
//
//   - Unix socket: daemon creates a socket file; MCP client connects locally.
//     Faster than TCP for same-machine use. Works across container boundaries
//     when the socket path is mounted into the container.
//
//   - TCP: daemon listens on a configurable address; agent connects over network.
//     Supports optional TLS (encrypted transport) and bearer token authentication.
//     Enables remote agent execution and cluster deployments.
//
// All non-stdio transports support bearer token authentication:
//   The client sends `Bearer <token>\n` as the first line before any MCP traffic.
//   The server verifies the token and closes the connection on mismatch.
//
// Usage in main.rs:
//   transport::serve(server, &daemon_config.transport, &project_root).await?;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rmcp::ServiceExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

use crate::config::{TransportConfig, TransportMode};
use ta_mcp_gateway::TaGatewayServer;

/// Serve the MCP gateway using the transport configured in daemon.toml.
///
/// - `Stdio`: calls `server.serve(rmcp::transport::stdio())` (existing behaviour).
/// - `Unix`: binds a Unix domain socket, accepts one connection, serves MCP.
/// - `Tcp`: binds a TCP listener, accepts one connection (with optional TLS),
///   verifies bearer token, then serves MCP.
///
/// Returns when the MCP session ends (client disconnected or server stopped).
pub async fn serve(
    server: TaGatewayServer,
    config: &TransportConfig,
    project_root: &Path,
) -> Result<()> {
    match config.mode {
        TransportMode::Stdio => serve_stdio(server).await,
        TransportMode::Unix => {
            let socket_path = resolve_socket_path(&config.unix_socket_path, project_root);
            serve_unix(server, &socket_path, config.auth_token.as_deref()).await
        }
        TransportMode::Tcp => {
            serve_tcp(
                server,
                &config.tcp_addr,
                config.auth_token.as_deref(),
                config.tls.as_ref(),
                project_root,
            )
            .await
        }
    }
}

// ---------------------------------------------------------------------------
// Stdio transport
// ---------------------------------------------------------------------------

async fn serve_stdio(server: TaGatewayServer) -> Result<()> {
    info!("MCP transport: stdio");
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP stdio serve error")?;
    service.waiting().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unix socket transport
// ---------------------------------------------------------------------------

#[cfg(unix)]
async fn serve_unix(
    server: TaGatewayServer,
    socket_path: &Path,
    auth_token: Option<&str>,
) -> Result<()> {
    use tokio::net::UnixListener;

    // Remove stale socket file if it exists.
    if socket_path.exists() {
        std::fs::remove_file(socket_path).with_context(|| {
            format!(
                "Failed to remove stale Unix socket: {}",
                socket_path.display()
            )
        })?;
    }

    // Ensure parent directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create socket directory: {}", parent.display()))?;
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("Failed to bind Unix socket: {}", socket_path.display()))?;

    info!(
        path = %socket_path.display(),
        "MCP transport: Unix socket — waiting for connection"
    );

    let (stream, _addr) = listener
        .accept()
        .await
        .context("Unix socket accept failed")?;
    info!("MCP Unix socket: client connected");

    let (rd, wr) = tokio::io::split(stream);
    let (rd, wr) = authenticate_connection(rd, wr, auth_token).await?;

    let service = server
        .serve((rd, wr))
        .await
        .context("MCP Unix socket serve error")?;
    service.waiting().await?;

    // Clean up socket file on exit.
    let _ = std::fs::remove_file(socket_path);

    Ok(())
}

#[cfg(not(unix))]
async fn serve_unix(
    _server: TaGatewayServer,
    _socket_path: &Path,
    _auth_token: Option<&str>,
) -> Result<()> {
    bail!("Unix socket transport is not supported on this platform. Use 'stdio' or 'tcp'.")
}

// ---------------------------------------------------------------------------
// TCP transport
// ---------------------------------------------------------------------------

async fn serve_tcp(
    server: TaGatewayServer,
    tcp_addr: &str,
    auth_token: Option<&str>,
    tls_config: Option<&crate::config::TlsConfig>,
    _project_root: &Path,
) -> Result<()> {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(tcp_addr)
        .await
        .with_context(|| format!("Failed to bind TCP listener on {}", tcp_addr))?;

    let local_addr = listener.local_addr()?;

    if tls_config.is_some() {
        info!(addr = %local_addr, "MCP transport: TCP+TLS — waiting for connection");
    } else {
        info!(addr = %local_addr, "MCP transport: TCP — waiting for connection");
        if auth_token.is_none() {
            warn!(
                "TCP transport has no auth_token configured. \
                 Set [transport].auth_token in daemon.toml to require authentication."
            );
        }
    }

    let (stream, peer_addr) = listener.accept().await.context("TCP accept failed")?;
    info!(%peer_addr, "MCP TCP: client connected");

    if let Some(tls) = tls_config {
        serve_tcp_tls(server, stream, peer_addr, auth_token, tls).await
    } else {
        let (rd, wr) = stream.into_split();
        let (rd, wr) = authenticate_connection(rd, wr, auth_token).await?;
        let service = server
            .serve((rd, wr))
            .await
            .context("MCP TCP serve error")?;
        service.waiting().await?;
        Ok(())
    }
}

async fn serve_tcp_tls(
    server: TaGatewayServer,
    stream: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    auth_token: Option<&str>,
    tls_config: &crate::config::TlsConfig,
) -> Result<()> {
    use rustls_pemfile::{certs, private_key};
    use std::sync::Arc;
    use tokio_rustls::TlsAcceptor;

    // Load certificate chain.
    let cert_data = std::fs::read(&tls_config.cert_path).with_context(|| {
        format!(
            "Failed to read TLS certificate: {}",
            tls_config.cert_path.display()
        )
    })?;
    let mut cert_reader = std::io::BufReader::new(cert_data.as_slice());
    let cert_chain: Vec<_> = certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to parse TLS certificate")?;

    // Load private key.
    let key_data = std::fs::read(&tls_config.key_path).with_context(|| {
        format!(
            "Failed to read TLS private key: {}",
            tls_config.key_path.display()
        )
    })?;
    let mut key_reader = std::io::BufReader::new(key_data.as_slice());
    let private_key = private_key(&mut key_reader)
        .context("Failed to parse TLS private key")?
        .context("No private key found in key file")?;

    // Build TLS server config.
    let tls_server_config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .context("Failed to build TLS server config")?;

    let acceptor = TlsAcceptor::from(Arc::new(tls_server_config));
    let tls_stream = acceptor
        .accept(stream)
        .await
        .with_context(|| format!("TLS handshake failed with {}", peer_addr))?;

    info!(%peer_addr, "TLS handshake complete");

    let (rd, wr) = tokio::io::split(tls_stream);
    let (rd, wr) = authenticate_connection(rd, wr, auth_token).await?;

    let service = server
        .serve((rd, wr))
        .await
        .context("MCP TCP+TLS serve error")?;
    service.waiting().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Bearer token authentication
// ---------------------------------------------------------------------------

/// Perform bearer token authentication on a connection before handing it to rmcp.
///
/// Protocol (for non-stdio transports when `auth_token` is `Some`):
///   1. Client sends: `Bearer <token>\n`
///   2. Server sends: `OK\n` (token matches) or closes connection (mismatch).
///   3. MCP protocol proceeds on the same stream.
///
/// If `auth_token` is `None`, authentication is skipped and the stream is
/// returned as-is (suitable for Unix sockets protected by filesystem permissions).
async fn authenticate_connection<R, W>(
    reader: R,
    mut writer: W,
    auth_token: Option<&str>,
) -> Result<(BufReader<R>, W)>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf_reader = BufReader::new(reader);

    let Some(expected_token) = auth_token else {
        // No auth required — pass through.
        return Ok((buf_reader, writer));
    };

    // Read the first line: expected format `Bearer <token>\n`.
    let mut line = String::new();
    buf_reader
        .read_line(&mut line)
        .await
        .context("Failed to read authentication token from client")?;

    let token = line.trim().strip_prefix("Bearer ").unwrap_or("").trim();

    if token != expected_token {
        // Reject the connection.
        let _ = writer.write_all(b"Unauthorized\n").await;
        let _ = writer.flush().await;
        bail!("MCP client authentication failed: invalid bearer token");
    }

    // Acknowledge successful authentication.
    writer
        .write_all(b"OK\n")
        .await
        .context("Failed to send auth acknowledgment")?;
    writer.flush().await.context("Failed to flush auth ack")?;

    info!("MCP connection authenticated via bearer token");
    Ok((buf_reader, writer))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_socket_path(unix_socket_path: &str, project_root: &Path) -> PathBuf {
    let p = Path::new(unix_socket_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_root.join(p)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_socket_path_absolute() {
        let root = Path::new("/project");
        let result = resolve_socket_path("/tmp/mcp.sock", root);
        assert_eq!(result, PathBuf::from("/tmp/mcp.sock"));
    }

    #[test]
    fn resolve_socket_path_relative() {
        let root = Path::new("/project");
        let result = resolve_socket_path(".ta/mcp.sock", root);
        assert_eq!(result, PathBuf::from("/project/.ta/mcp.sock"));
    }

    #[tokio::test]
    async fn authenticate_connection_no_auth_passthrough() {
        let input = b"MCP data\n";
        let rd = tokio::io::BufReader::new(input.as_slice());
        let wr = tokio::io::sink();
        // No auth token — should succeed without reading any auth line.
        let (mut reader, _wr) = authenticate_connection(rd, wr, None)
            .await
            .expect("auth with no token should pass through");
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        // The reader should still have the original data.
        assert_eq!(line, "MCP data\n");
    }

    #[tokio::test]
    async fn authenticate_connection_valid_token() {
        let input = b"Bearer secret-token\nMCP data\n";
        let rd = tokio::io::BufReader::new(input.as_slice());
        let mut wr_buf = Vec::new();
        let wr = tokio::io::BufWriter::new(&mut wr_buf);
        let result = authenticate_connection(rd, wr, Some("secret-token")).await;
        assert!(result.is_ok(), "valid token should authenticate");
    }

    #[tokio::test]
    async fn authenticate_connection_invalid_token() {
        let input = b"Bearer wrong-token\n";
        let rd = tokio::io::BufReader::new(input.as_slice());
        let wr_buf = Vec::new();
        let wr = tokio::io::BufWriter::new(std::io::Cursor::new(wr_buf));
        let result = authenticate_connection(rd, wr, Some("secret-token")).await;
        assert!(result.is_err(), "invalid token should fail authentication");
    }
}
