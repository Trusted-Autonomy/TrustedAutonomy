//! Plugin registration configuration for `daemon.toml` (v0.14.4).
//!
//! The `[plugins]` section in `daemon.toml` maps each extension point to a
//! plugin binary. Unset slots use the local default implementation.
//!
//! ```toml
//! [plugins]
//! # transport = "ta-transport-websocket"
//! # auth = "ta-auth-oidc"
//! # workspace = "ta-workspace-s3"
//! # review_queue = "ta-review-jira"
//! # audit_storage = "ta-audit-splunk"
//! ```
//!
//! Plugin resolution order (for each slot):
//! 1. Absolute path (if value starts with `/` or `./`)
//! 2. `.ta/plugins/<slot>/<value>`
//! 3. `~/.config/ta/plugins/<slot>/<value>`
//! 4. `$PATH` lookup

use serde::{Deserialize, Serialize};

/// `[plugins]` section of `daemon.toml`.
///
/// Each field is the name or path of a plugin binary for the corresponding
/// extension point. Omit a field to use TA's built-in local default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginsConfig {
    /// MCP transport plugin (e.g., `"ta-transport-websocket"`).
    ///
    /// Replaces the daemon's built-in stdio/unix/tcp transport with a
    /// custom network transport. The plugin must implement the
    /// `TransportBackend` JSON-stdio protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,

    /// Authentication middleware plugin (e.g., `"ta-auth-oidc"`).
    ///
    /// Authenticates HTTP API requests and MCP connections. Default is
    /// no-op (local single-user, no credentials required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,

    /// Workspace storage plugin (e.g., `"ta-workspace-s3"`).
    ///
    /// Stores staging workspace copies. Default is local filesystem
    /// (`.ta/staging/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// Review queue plugin (e.g., `"ta-review-jira"`).
    ///
    /// Routes drafts to external review systems. Default is local queue
    /// (`.ta/review_queue/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_queue: Option<String>,

    /// Audit storage plugin (e.g., `"ta-audit-splunk"`).
    ///
    /// Stores audit log records. Default is local JSONL file
    /// (`.ta/audit.jsonl`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_storage: Option<String>,
}

impl PluginsConfig {
    /// Returns true if any plugin slot is configured.
    pub fn any_configured(&self) -> bool {
        self.transport.is_some()
            || self.auth.is_some()
            || self.workspace.is_some()
            || self.review_queue.is_some()
            || self.audit_storage.is_some()
    }

    /// Returns the names of all configured plugin slots, for startup logging.
    pub fn configured_slots(&self) -> Vec<&str> {
        let mut slots = Vec::new();
        if self.transport.is_some() {
            slots.push("transport");
        }
        if self.auth.is_some() {
            slots.push("auth");
        }
        if self.workspace.is_some() {
            slots.push("workspace");
        }
        if self.review_queue.is_some() {
            slots.push("review_queue");
        }
        if self.audit_storage.is_some() {
            slots.push("audit_storage");
        }
        slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_plugins() {
        let cfg = PluginsConfig::default();
        assert!(!cfg.any_configured());
        assert!(cfg.configured_slots().is_empty());
    }

    #[test]
    fn configured_slots_reported() {
        let cfg = PluginsConfig {
            auth: Some("ta-auth-oidc".to_string()),
            audit_storage: Some("ta-audit-splunk".to_string()),
            ..Default::default()
        };
        assert!(cfg.any_configured());
        let slots = cfg.configured_slots();
        assert!(slots.contains(&"auth"));
        assert!(slots.contains(&"audit_storage"));
        assert!(!slots.contains(&"transport"));
    }

    #[test]
    fn toml_roundtrip_empty() {
        let cfg = PluginsConfig::default();
        let toml_str = toml::to_string(&cfg).unwrap();
        let parsed: PluginsConfig = toml::from_str(&toml_str).unwrap();
        assert!(!parsed.any_configured());
    }

    #[test]
    fn toml_roundtrip_with_plugin() {
        let toml_str = r#"
            auth = "ta-auth-oidc"
            audit_storage = "ta-audit-splunk"
        "#;
        let cfg: PluginsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.auth.as_deref(), Some("ta-auth-oidc"));
        assert_eq!(cfg.audit_storage.as_deref(), Some("ta-audit-splunk"));
        assert!(cfg.transport.is_none());
    }
}
