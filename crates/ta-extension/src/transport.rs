//! TransportBackend — plugin trait for network-exposed MCP transports (v0.14.4).
//!
//! The daemon's built-in transports (stdio, Unix socket, TCP/TLS) cover all
//! single-machine and LAN deployments. This trait is the extension point for
//! plugins that add WebSocket, gRPC, WebRTC, or other transports without
//! forking TA.
//!
//! ## Default implementation
//!
//! [`LocalTransportBackend`] is a stub that reports `mode = "local"` and
//! panics if asked to bind — it exists as a placeholder when no transport
//! plugin is registered. The daemon uses its own built-in transport logic
//! (from `ta-daemon::transport`) in this case.
//!
//! ## Plugin registration
//!
//! ```toml
//! [plugins]
//! transport = "ta-transport-websocket"
//! ```
//!
//! The plugin binary must implement the JSON-stdio extension protocol
//! (defined in `docs/PLUGIN-AUTHORING.md`).

use crate::ExtensionError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Configuration passed to a transport plugin at bind time.
///
/// Populated from `[server]` in `daemon.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransportPluginConfig {
    /// Listen address passed to the plugin (e.g. `"0.0.0.0:7800"`).
    pub bind_addr: String,
    /// Optional TLS certificate path.
    pub tls_cert_path: Option<String>,
    /// Optional TLS private key path.
    pub tls_key_path: Option<String>,
    /// Plugin-specific key-value options from `[server.plugin_options]`.
    #[serde(default)]
    pub options: std::collections::HashMap<String, String>,
}

/// Plugin trait for network-exposed MCP transports.
///
/// Implement this trait to add a new transport (WebSocket, gRPC, etc.) to TA.
///
/// The daemon calls:
/// 1. [`bind`](TransportBackend::bind) once at startup to start listening.
/// 2. [`accept`](TransportBackend::accept) repeatedly to get incoming connections.
/// 3. [`shutdown`](TransportBackend::shutdown) on clean exit.
///
/// Each accepted connection is a `(read, write)` byte-stream pair that the
/// daemon passes directly to the MCP server.
#[async_trait]
pub trait TransportBackend: Send + Sync {
    /// Unique name for this transport (e.g., `"websocket"`, `"grpc"`).
    fn name(&self) -> &str;

    /// Start listening. Called once at daemon startup.
    async fn bind(&self, config: &TransportPluginConfig) -> Result<(), ExtensionError>;

    /// Accept the next incoming MCP connection.
    ///
    /// Returns a descriptive peer label (e.g., IP address) for logging.
    /// The actual byte-stream is managed by the plugin; the daemon signals
    /// readiness to proceed by returning from this method.
    ///
    /// Returns `None` when the transport has been shut down cleanly.
    async fn accept(&self) -> Result<Option<String>, ExtensionError>;

    /// Shut down and release all resources.
    async fn shutdown(&self) -> Result<(), ExtensionError>;
}

/// Default (no-op) transport backend used when no plugin is registered.
///
/// The daemon uses its built-in transport instead of going through this trait
/// when `[plugins].transport` is not set in `daemon.toml`. This struct exists
/// as a type-level placeholder.
pub struct LocalTransportBackend;

#[async_trait]
impl TransportBackend for LocalTransportBackend {
    fn name(&self) -> &str {
        "local"
    }

    async fn bind(&self, _config: &TransportPluginConfig) -> Result<(), ExtensionError> {
        Err(ExtensionError::NotSupported(
            "LocalTransportBackend is a placeholder — \
             the daemon uses its built-in transport (stdio/unix/tcp). \
             Register a [plugins].transport plugin to use a custom transport."
                .into(),
        ))
    }

    async fn accept(&self) -> Result<Option<String>, ExtensionError> {
        Err(ExtensionError::NotSupported(
            "LocalTransportBackend.accept() should never be called".into(),
        ))
    }

    async fn shutdown(&self) -> Result<(), ExtensionError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_backend_name() {
        let b = LocalTransportBackend;
        assert_eq!(b.name(), "local");
    }

    #[tokio::test]
    async fn local_backend_bind_returns_not_supported() {
        let b = LocalTransportBackend;
        let result = b.bind(&TransportPluginConfig::default()).await;
        assert!(matches!(result, Err(ExtensionError::NotSupported(_))));
    }

    #[tokio::test]
    async fn local_backend_shutdown_ok() {
        let b = LocalTransportBackend;
        assert!(b.shutdown().await.is_ok());
    }

    #[test]
    fn transport_plugin_config_default() {
        let cfg = TransportPluginConfig::default();
        assert!(cfg.bind_addr.is_empty());
        assert!(cfg.tls_cert_path.is_none());
    }
}
