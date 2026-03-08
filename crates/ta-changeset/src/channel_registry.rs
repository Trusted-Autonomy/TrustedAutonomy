// channel_registry.rs — Pluggable IO channel system (v0.7.0).
//
// All channels (CLI, web, Slack, Discord, email) are equal: they share the
// same ChannelFactory trait and register in the ChannelRegistry at startup.
// Channel routing config (`.ta/config.yaml`) determines which channel handles
// which concern (review, notify, session, escalation).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::review_channel::{ReviewChannel, ReviewChannelError};
use crate::session_channel::{SessionChannel, SessionChannelError};

/// What a channel implementation can do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelCapabilitySet {
    /// Can this channel deliver review interactions (approve/deny)?
    pub supports_review: bool,
    /// Can this channel stream session events (agent output, etc.)?
    pub supports_session: bool,
    /// Can this channel deliver notifications?
    pub supports_notify: bool,
    /// Does this channel support rich media (images, code blocks, buttons)?
    pub supports_rich_media: bool,
    /// Does this channel support threaded conversations?
    pub supports_threads: bool,
}

impl Default for ChannelCapabilitySet {
    fn default() -> Self {
        Self {
            supports_review: true,
            supports_session: true,
            supports_notify: true,
            supports_rich_media: false,
            supports_threads: false,
        }
    }
}

/// Factory trait for creating channel instances.
///
/// Each channel plugin (terminal, Slack, Discord, email, webhook) implements
/// this trait. The registry holds factories, and the routing config decides
/// which factory handles which concern.
pub trait ChannelFactory: Send + Sync {
    /// Channel type name (e.g., "terminal", "slack", "discord", "email", "webhook").
    fn channel_type(&self) -> &str;

    /// Create a ReviewChannel for human review interactions.
    fn build_review(
        &self,
        config: &serde_json::Value,
    ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError>;

    /// Create a SessionChannel for agent-human streaming.
    fn build_session(
        &self,
        config: &serde_json::Value,
    ) -> Result<Box<dyn SessionChannel>, SessionChannelError>;

    /// What this channel type supports.
    fn capabilities(&self) -> ChannelCapabilitySet;
}

/// Registry of channel factories, keyed by channel type name.
pub struct ChannelRegistry {
    factories: HashMap<String, Box<dyn ChannelFactory>>,
}

impl ChannelRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a channel factory.
    pub fn register(&mut self, factory: Box<dyn ChannelFactory>) {
        let name = factory.channel_type().to_string();
        self.factories.insert(name, factory);
    }

    /// Get a factory by channel type.
    pub fn get(&self, channel_type: &str) -> Option<&dyn ChannelFactory> {
        self.factories.get(channel_type).map(|f| f.as_ref())
    }

    /// List all registered channel types.
    pub fn channel_types(&self) -> Vec<&str> {
        self.factories.keys().map(|k| k.as_str()).collect()
    }

    /// Check if a channel type is registered.
    pub fn has_channel(&self, channel_type: &str) -> bool {
        self.factories.contains_key(channel_type)
    }

    /// Number of registered channel factories.
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    /// Build a ReviewChannel from routing config.
    pub fn build_review_from_config(
        &self,
        route: &ChannelRouteConfig,
    ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
        let factory = self.get(&route.channel_type).ok_or_else(|| {
            ReviewChannelError::Other(format!(
                "unknown channel type: '{}'. Registered: {:?}",
                route.channel_type,
                self.channel_types()
            ))
        })?;
        factory.build_review(&route.config)
    }

    /// Build a ReviewChannel from a ReviewRouteConfig (single or multi).
    ///
    /// If the config specifies multiple channels, returns a `MultiReviewChannel`
    /// wrapping all of them. If single, returns the channel directly.
    pub fn build_review_from_route(
        &self,
        route: &ReviewRouteConfig,
        strategy: &crate::multi_channel::MultiChannelStrategy,
    ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
        let configs = route.configs();
        if configs.len() == 1 {
            return self.build_review_from_config(configs[0]);
        }
        let mut channels: Vec<Box<dyn ReviewChannel>> = Vec::with_capacity(configs.len());
        for config in configs {
            channels.push(self.build_review_from_config(config)?);
        }
        Ok(Box::new(crate::multi_channel::MultiReviewChannel::new(
            channels,
            strategy.clone(),
        )))
    }

    /// Build a SessionChannel from routing config.
    pub fn build_session_from_config(
        &self,
        route: &ChannelRouteConfig,
    ) -> Result<Box<dyn SessionChannel>, SessionChannelError> {
        let factory = self.get(&route.channel_type).ok_or_else(|| {
            SessionChannelError::Other(format!(
                "unknown channel type: '{}'. Registered: {:?}",
                route.channel_type,
                self.channel_types()
            ))
        })?;
        factory.build_session(&route.config)
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// A single channel routing entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelRouteConfig {
    /// Channel type (must match a registered ChannelFactory).
    #[serde(rename = "type")]
    pub channel_type: String,
    /// Channel-specific config (Slack channel, email address, etc.).
    #[serde(flatten)]
    pub config: serde_json::Value,
}

impl Default for ChannelRouteConfig {
    fn default() -> Self {
        Self {
            channel_type: "terminal".to_string(),
            config: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Notification routing entry (supports multiple targets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyRouteConfig {
    /// Channel type.
    #[serde(rename = "type")]
    pub channel_type: String,
    /// Minimum notification level to deliver: "debug", "info", "warning", "error".
    #[serde(default = "default_notify_level")]
    pub level: String,
    /// Channel-specific config.
    #[serde(flatten)]
    pub config: serde_json::Value,
}

fn default_notify_level() -> String {
    "info".to_string()
}

/// One-or-many channel route config (v0.10.0 multi-channel routing).
///
/// Accepts either a single channel object or an array of channels:
/// ```yaml
/// review: { type: terminal }          # single
/// review:                              # multiple
///   - { type: terminal }
///   - { type: webhook, endpoint: /tmp/review }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReviewRouteConfig {
    /// A single channel (backward-compatible default).
    Single(ChannelRouteConfig),
    /// Multiple channels — dispatched via MultiReviewChannel.
    Multiple(Vec<ChannelRouteConfig>),
}

impl Default for ReviewRouteConfig {
    fn default() -> Self {
        Self::Single(ChannelRouteConfig::default())
    }
}

impl ReviewRouteConfig {
    /// Return the list of channel configs (always at least one).
    pub fn configs(&self) -> Vec<&ChannelRouteConfig> {
        match self {
            Self::Single(c) => vec![c],
            Self::Multiple(cs) => cs.iter().collect(),
        }
    }

    /// True if this specifies more than one channel.
    pub fn is_multi(&self) -> bool {
        matches!(self, Self::Multiple(cs) if cs.len() > 1)
    }
}

/// One-or-many escalation route config (v0.10.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EscalationRouteConfig {
    /// A single escalation channel.
    Single(ChannelRouteConfig),
    /// Multiple escalation channels.
    Multiple(Vec<ChannelRouteConfig>),
}

impl EscalationRouteConfig {
    /// Return the list of channel configs.
    pub fn configs(&self) -> Vec<&ChannelRouteConfig> {
        match self {
            Self::Single(c) => vec![c],
            Self::Multiple(cs) => cs.iter().collect(),
        }
    }
}

/// Top-level channel routing configuration.
///
/// Loaded from `.ta/config.yaml`:
/// ```yaml
/// channels:
///   review: { type: terminal }                  # single channel
///   review:                                      # multi-channel (v0.10.0)
///     - { type: terminal }
///     - { type: webhook, endpoint: /tmp/review }
///   notify:
///     - { type: terminal }
///     - { type: slack, channel: "#reviews", level: warning }
///   session: { type: terminal }
///   escalation: { type: email, to: "mgr@co.com" }
///   strategy: first_response                     # or "quorum"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelRoutingConfig {
    /// Channel(s) for review interactions (draft approve/deny).
    /// Supports a single channel or array of channels (v0.10.0).
    #[serde(default)]
    pub review: ReviewRouteConfig,
    /// Channels for notifications (can be multiple).
    #[serde(default)]
    pub notify: Vec<NotifyRouteConfig>,
    /// Channel for interactive sessions.
    #[serde(default)]
    pub session: ChannelRouteConfig,
    /// Channel(s) for escalation (high-priority or supervisor review).
    /// Supports a single channel or array of channels (v0.10.0).
    #[serde(default)]
    pub escalation: Option<EscalationRouteConfig>,
    /// Default agent to assign when requests come in through a channel.
    #[serde(default)]
    pub default_agent: Option<String>,
    /// Default workflow to use for channel-initiated goals.
    #[serde(default)]
    pub default_workflow: Option<String>,
    /// Multi-channel dispatch strategy (v0.10.0): "first_response" (default) or "quorum".
    #[serde(default)]
    pub strategy: Option<String>,
}

// ChannelRoutingConfig derives Default since all fields have Default implementations.

/// Wrapper for `.ta/config.yaml` — the channels section lives here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaConfig {
    /// Channel routing configuration.
    #[serde(default)]
    pub channels: ChannelRoutingConfig,
}

/// Load `.ta/config.yaml` from project root.
pub fn load_config(project_root: &Path) -> TaConfig {
    let config_path = project_root.join(".ta").join("config.yaml");
    if !config_path.exists() {
        return TaConfig::default();
    }
    match std::fs::read_to_string(&config_path) {
        Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
        Err(_) => TaConfig::default(),
    }
}

/// Built-in terminal channel factory.
///
/// Always available — provides CLI-based review and session channels.
pub struct TerminalChannelFactory;

impl ChannelFactory for TerminalChannelFactory {
    fn channel_type(&self) -> &str {
        "terminal"
    }

    fn build_review(
        &self,
        _config: &serde_json::Value,
    ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
        Ok(Box::new(crate::terminal_channel::TerminalChannel::stdio()))
    }

    fn build_session(
        &self,
        _config: &serde_json::Value,
    ) -> Result<Box<dyn SessionChannel>, SessionChannelError> {
        Ok(Box::new(
            crate::terminal_channel::TerminalSessionChannel::new(),
        ))
    }

    fn capabilities(&self) -> ChannelCapabilitySet {
        ChannelCapabilitySet {
            supports_review: true,
            supports_session: true,
            supports_notify: true,
            supports_rich_media: false,
            supports_threads: false,
        }
    }
}

/// Built-in auto-approve channel factory (for testing/CI).
pub struct AutoApproveChannelFactory;

impl ChannelFactory for AutoApproveChannelFactory {
    fn channel_type(&self) -> &str {
        "auto-approve"
    }

    fn build_review(
        &self,
        _config: &serde_json::Value,
    ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
        Ok(Box::new(crate::terminal_channel::AutoApproveChannel::new()))
    }

    fn build_session(
        &self,
        _config: &serde_json::Value,
    ) -> Result<Box<dyn SessionChannel>, SessionChannelError> {
        // Auto-approve doesn't have meaningful session interaction.
        Ok(Box::new(
            crate::terminal_channel::TerminalSessionChannel::new(),
        ))
    }

    fn capabilities(&self) -> ChannelCapabilitySet {
        ChannelCapabilitySet {
            supports_review: true,
            supports_session: false,
            supports_notify: false,
            supports_rich_media: false,
            supports_threads: false,
        }
    }
}

/// Built-in webhook channel factory.
pub struct WebhookChannelFactory;

impl ChannelFactory for WebhookChannelFactory {
    fn channel_type(&self) -> &str {
        "webhook"
    }

    fn build_review(
        &self,
        config: &serde_json::Value,
    ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
        let endpoint = config
            .get("endpoint")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ReviewChannelError::Other("webhook requires 'endpoint' in config".into())
            })?;
        Ok(Box::new(crate::webhook_channel::WebhookChannel::new(
            endpoint,
        )))
    }

    fn build_session(
        &self,
        _config: &serde_json::Value,
    ) -> Result<Box<dyn SessionChannel>, SessionChannelError> {
        // Webhook doesn't support bidirectional sessions.
        Err(SessionChannelError::Other(
            "webhook does not support interactive sessions".into(),
        ))
    }

    fn capabilities(&self) -> ChannelCapabilitySet {
        ChannelCapabilitySet {
            supports_review: true,
            supports_session: false,
            supports_notify: true,
            supports_rich_media: false,
            supports_threads: false,
        }
    }
}

/// Create a ChannelRegistry pre-loaded with all built-in channel factories.
pub fn default_registry() -> ChannelRegistry {
    let mut registry = ChannelRegistry::new();
    registry.register(Box::new(TerminalChannelFactory));
    registry.register(Box::new(AutoApproveChannelFactory));
    registry.register(Box::new(WebhookChannelFactory));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_builtins() {
        let registry = default_registry();
        assert!(registry.has_channel("terminal"));
        assert!(registry.has_channel("auto-approve"));
        assert!(registry.has_channel("webhook"));
        assert!(!registry.has_channel("slack"));
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn build_review_from_config() {
        let registry = default_registry();
        let route = ChannelRouteConfig {
            channel_type: "terminal".into(),
            config: serde_json::json!({}),
        };
        let channel = registry.build_review_from_config(&route);
        assert!(channel.is_ok());
    }

    #[test]
    fn build_review_unknown_type_errors() {
        let registry = default_registry();
        let route = ChannelRouteConfig {
            channel_type: "slack".into(),
            config: serde_json::json!({}),
        };
        let result = registry.build_review_from_config(&route);
        assert!(result.is_err());
    }

    #[test]
    fn channel_routing_config_single_review() {
        let yaml = r#"
review:
  type: terminal
notify:
  - type: terminal
  - type: webhook
    endpoint: "/tmp/notify"
    level: warning
session:
  type: terminal
escalation:
  type: webhook
  endpoint: "/tmp/escalate"
default_agent: claude-code
"#;
        let config: ChannelRoutingConfig = serde_yaml::from_str(yaml).unwrap();
        let review_configs = config.review.configs();
        assert_eq!(review_configs.len(), 1);
        assert_eq!(review_configs[0].channel_type, "terminal");
        assert!(!config.review.is_multi());
        assert_eq!(config.notify.len(), 2);
        assert_eq!(config.notify[1].channel_type, "webhook");
        assert_eq!(config.notify[1].level, "warning");
        assert!(config.escalation.is_some());
        assert_eq!(config.default_agent.as_deref(), Some("claude-code"));
    }

    #[test]
    fn channel_routing_config_multi_review() {
        let yaml = r#"
review:
  - type: terminal
  - type: webhook
    endpoint: "/tmp/review"
session:
  type: terminal
strategy: first_response
"#;
        let config: ChannelRoutingConfig = serde_yaml::from_str(yaml).unwrap();
        let review_configs = config.review.configs();
        assert_eq!(review_configs.len(), 2);
        assert_eq!(review_configs[0].channel_type, "terminal");
        assert_eq!(review_configs[1].channel_type, "webhook");
        assert!(config.review.is_multi());
        assert_eq!(config.strategy.as_deref(), Some("first_response"));
    }

    #[test]
    fn channel_routing_config_multi_escalation() {
        let yaml = r#"
review:
  type: terminal
session:
  type: terminal
escalation:
  - type: webhook
    endpoint: "/tmp/esc1"
  - type: webhook
    endpoint: "/tmp/esc2"
"#;
        let config: ChannelRoutingConfig = serde_yaml::from_str(yaml).unwrap();
        let esc = config.escalation.unwrap();
        assert_eq!(esc.configs().len(), 2);
    }

    #[test]
    fn ta_config_deserialization() {
        let yaml = r#"
channels:
  review:
    type: terminal
  session:
    type: terminal
"#;
        let config: TaConfig = serde_yaml::from_str(yaml).unwrap();
        let review_configs = config.channels.review.configs();
        assert_eq!(review_configs[0].channel_type, "terminal");
    }

    #[test]
    fn default_ta_config() {
        let config = TaConfig::default();
        let review_configs = config.channels.review.configs();
        assert_eq!(review_configs[0].channel_type, "terminal");
        assert!(config.channels.notify.is_empty());
    }

    #[test]
    fn build_multi_review_from_route_single() {
        let registry = default_registry();
        let route = ReviewRouteConfig::Single(ChannelRouteConfig {
            channel_type: "terminal".into(),
            config: serde_json::json!({}),
        });
        let strategy = crate::multi_channel::MultiChannelStrategy::FirstResponse;
        let channel = registry.build_review_from_route(&route, &strategy);
        assert!(channel.is_ok());
    }

    #[test]
    fn build_multi_review_from_route_multiple() {
        let registry = default_registry();
        let route = ReviewRouteConfig::Multiple(vec![
            ChannelRouteConfig {
                channel_type: "auto-approve".into(),
                config: serde_json::json!({}),
            },
            ChannelRouteConfig {
                channel_type: "auto-approve".into(),
                config: serde_json::json!({}),
            },
        ]);
        let strategy = crate::multi_channel::MultiChannelStrategy::FirstResponse;
        let channel = registry.build_review_from_route(&route, &strategy);
        assert!(channel.is_ok());
    }

    #[test]
    fn channel_capability_set_defaults() {
        let caps = ChannelCapabilitySet::default();
        assert!(caps.supports_review);
        assert!(caps.supports_session);
        assert!(caps.supports_notify);
        assert!(!caps.supports_rich_media);
        assert!(!caps.supports_threads);
    }

    #[test]
    fn register_custom_factory() {
        struct MockFactory;
        impl ChannelFactory for MockFactory {
            fn channel_type(&self) -> &str {
                "mock"
            }
            fn build_review(
                &self,
                _config: &serde_json::Value,
            ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
                Ok(Box::new(crate::terminal_channel::AutoApproveChannel::new()))
            }
            fn build_session(
                &self,
                _config: &serde_json::Value,
            ) -> Result<Box<dyn SessionChannel>, SessionChannelError> {
                Err(SessionChannelError::Other("mock".into()))
            }
            fn capabilities(&self) -> ChannelCapabilitySet {
                ChannelCapabilitySet::default()
            }
        }

        let mut registry = default_registry();
        registry.register(Box::new(MockFactory));
        assert!(registry.has_channel("mock"));
        assert_eq!(registry.len(), 4);
    }

    #[test]
    fn load_config_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = load_config(dir.path());
        assert_eq!(config.channels.review.configs()[0].channel_type, "terminal");
    }

    #[test]
    fn webhook_factory_requires_endpoint() {
        let registry = default_registry();
        let route = ChannelRouteConfig {
            channel_type: "webhook".into(),
            config: serde_json::json!({}),
        };
        let result = registry.build_review_from_config(&route);
        assert!(result.is_err());
    }
}
