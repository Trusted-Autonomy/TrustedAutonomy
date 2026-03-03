// review_channel.rs — ReviewChannel trait for pluggable human-agent communication.
//
// Unlike SessionChannel (which streams agent output), ReviewChannel is for
// bidirectional interactions where TA needs human input: draft review, plan
// approval, escalation, etc. Implementations can target any medium — terminal,
// Slack, Discord, email, webhook.
//
// This is the core abstraction for v0.4.1.1 (Runtime Channel Architecture).
// Future adapters (v0.5.3) implement this same trait for non-terminal mediums.

use std::fmt;

use crate::interaction::{
    ChannelCapabilities, InteractionRequest, InteractionResponse, Notification,
};

/// Errors from ReviewChannel operations.
#[derive(Debug, thiserror::Error)]
pub enum ReviewChannelError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("channel closed")]
    ChannelClosed,

    #[error("channel timeout")]
    Timeout,

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("channel error: {0}")]
    Other(String),
}

/// Bidirectional communication channel between agent and human reviewer.
///
/// Implementations handle delivery (terminal, Slack, email, etc.) and
/// response collection. The trait is interaction-agnostic — it carries
/// any TA interaction, not just draft reviews.
///
/// # Blocking Semantics
///
/// `request_interaction` is blocking: the caller (MCP tool handler) waits
/// until the human responds. This is the default for v0.4.1.1. Future phases
/// may add non-blocking modes where the agent continues and checks back later.
pub trait ReviewChannel: Send + Sync {
    /// Send an interaction request to the human and await their response.
    ///
    /// This is a blocking call — the MCP tool handler suspends until the
    /// human provides a decision through whatever medium this channel uses.
    fn request_interaction(
        &self,
        request: &InteractionRequest,
    ) -> Result<InteractionResponse, ReviewChannelError>;

    /// Non-blocking notification to the human.
    ///
    /// Used for status updates, progress reports, and informational messages
    /// that don't require a response.
    fn notify(&self, notification: &Notification) -> Result<(), ReviewChannelError>;

    /// What this channel supports (async responses, rich media, threads, etc.).
    fn capabilities(&self) -> ChannelCapabilities;

    /// Channel identity string for audit trail (e.g., "terminal:tty0", "slack:C04ABC").
    fn channel_id(&self) -> &str;
}

/// Configuration for selecting and configuring a ReviewChannel.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewChannelConfig {
    /// Channel type: "terminal" (default), future: "slack", "discord", "email", "webhook".
    #[serde(default = "default_channel_type")]
    pub channel_type: String,

    /// Whether the agent blocks on approval (default: true).
    #[serde(default = "default_true")]
    pub blocking_mode: bool,

    /// Notification level filter: "debug", "info", "warning", "error".
    #[serde(default = "default_notification_level")]
    pub notification_level: String,

    /// Channel-specific configuration (webhook URL, Slack token, etc.).
    /// Interpretation depends on `channel_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_config: Option<serde_json::Value>,
}

fn default_channel_type() -> String {
    "terminal".to_string()
}

fn default_true() -> bool {
    true
}

fn default_notification_level() -> String {
    "info".to_string()
}

impl Default for ReviewChannelConfig {
    fn default() -> Self {
        Self {
            channel_type: default_channel_type(),
            blocking_mode: true,
            notification_level: default_notification_level(),
            channel_config: None,
        }
    }
}

/// Construct a ReviewChannel from config (v0.5.3).
///
/// This factory function selects the appropriate channel implementation
/// based on `config.channel_type`. Unknown types return an error.
pub fn build_channel(
    config: &ReviewChannelConfig,
) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
    use crate::terminal_channel::{AutoApproveChannel, TerminalChannel};

    match config.channel_type.as_str() {
        "terminal" => Ok(Box::new(TerminalChannel::stdio())),
        "auto-approve" => Ok(Box::new(AutoApproveChannel::new())),
        "webhook" => {
            let channel_cfg = config.channel_config.as_ref().ok_or_else(|| {
                ReviewChannelError::Other(
                    "webhook channel requires channel_config with 'endpoint' field".into(),
                )
            })?;
            let endpoint = channel_cfg
                .get("endpoint")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ReviewChannelError::Other("webhook channel_config missing 'endpoint'".into())
                })?;
            Ok(Box::new(crate::webhook_channel::WebhookChannel::new(
                endpoint,
            )))
        }
        other => Err(ReviewChannelError::Other(format!(
            "unknown channel type: '{}'. Available: terminal, auto-approve, webhook",
            other,
        ))),
    }
}

impl fmt::Display for ReviewChannelConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "channel={}, blocking={}, notify_level={}",
            self.channel_type, self.blocking_mode, self.notification_level
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_channel_config_defaults() {
        let config = ReviewChannelConfig::default();
        assert_eq!(config.channel_type, "terminal");
        assert!(config.blocking_mode);
        assert_eq!(config.notification_level, "info");
    }

    #[test]
    fn review_channel_config_serialization() {
        let config = ReviewChannelConfig {
            channel_type: "slack".into(),
            blocking_mode: false,
            notification_level: "debug".into(),
            channel_config: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: ReviewChannelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.channel_type, "slack");
        assert!(!restored.blocking_mode);
        assert_eq!(restored.notification_level, "debug");
    }

    #[test]
    fn review_channel_config_display() {
        let config = ReviewChannelConfig::default();
        let display = format!("{}", config);
        assert!(display.contains("terminal"));
        assert!(display.contains("blocking=true"));
    }

    #[test]
    fn review_channel_error_display() {
        let err = ReviewChannelError::ChannelClosed;
        assert_eq!(format!("{}", err), "channel closed");

        let err = ReviewChannelError::InvalidResponse("bad json".into());
        assert_eq!(format!("{}", err), "invalid response: bad json");
    }
}
