// channel_dispatcher.rs — Routes questions to external channel adapters.
//
// The ChannelDispatcher holds registered channel adapters and dispatches
// questions to the appropriate channels based on routing hints from the
// AgentNeedsInput event or the daemon's default channel list.

use std::collections::HashMap;
use std::sync::Arc;

use ta_events::channel::{ChannelDelivery, ChannelQuestion, DeliveryResult};
use uuid::Uuid;

use crate::config::ChannelsConfig;

/// Manages channel adapters and dispatches questions to them.
pub struct ChannelDispatcher {
    adapters: HashMap<String, Arc<dyn ChannelDelivery>>,
    default_channels: Vec<String>,
}

impl ChannelDispatcher {
    /// Create a new dispatcher with no adapters.
    pub fn new(default_channels: Vec<String>) -> Self {
        Self {
            adapters: HashMap::new(),
            default_channels,
        }
    }

    /// Build a dispatcher from daemon channel configuration.
    ///
    /// Registers adapters for each configured channel (Slack, Discord, Email).
    /// Only channels with complete configuration are registered.
    pub fn from_config(config: &ChannelsConfig) -> Self {
        let mut dispatcher = Self::new(config.default_channels.clone());

        if let Some(ref slack_cfg) = config.slack {
            let adapter = ta_connector_slack::SlackAdapter::new(ta_connector_slack::SlackConfig {
                bot_token: slack_cfg.bot_token.clone(),
                channel_id: slack_cfg.channel_id.clone(),
            });
            dispatcher.register(Arc::new(adapter));
            tracing::info!("Registered Slack channel adapter");
        }

        if let Some(ref discord_cfg) = config.discord {
            let adapter =
                ta_connector_discord::DiscordAdapter::new(ta_connector_discord::DiscordConfig {
                    bot_token: discord_cfg.bot_token.clone(),
                    channel_id: discord_cfg.channel_id.clone(),
                });
            dispatcher.register(Arc::new(adapter));
            tracing::info!("Registered Discord channel adapter");
        }

        if let Some(ref email_cfg) = config.email {
            let adapter = ta_connector_email::EmailAdapter::new(ta_connector_email::EmailConfig {
                send_endpoint: email_cfg.send_endpoint.clone(),
                api_key: email_cfg.api_key.clone(),
                from_address: email_cfg.from_address.clone(),
                to_address: email_cfg.to_address.clone(),
            });
            dispatcher.register(Arc::new(adapter));
            tracing::info!("Registered Email channel adapter");
        }

        dispatcher
    }

    /// Register a channel adapter.
    pub fn register(&mut self, adapter: Arc<dyn ChannelDelivery>) {
        self.adapters.insert(adapter.name().to_string(), adapter);
    }

    /// Get the list of registered channel names.
    pub fn registered_channels(&self) -> Vec<String> {
        self.adapters.keys().cloned().collect()
    }

    /// Dispatch a question to channels.
    ///
    /// If `channel_hints` is non-empty, delivers to those specific channels.
    /// Otherwise, delivers to the default channels from daemon config.
    /// Returns a `DeliveryResult` for each attempted delivery.
    pub async fn dispatch(
        &self,
        question: &ChannelQuestion,
        channel_hints: &[String],
    ) -> Vec<DeliveryResult> {
        let target_channels = if channel_hints.is_empty() {
            &self.default_channels
        } else {
            channel_hints
        };

        if target_channels.is_empty() {
            tracing::debug!(
                interaction_id = %question.interaction_id,
                "No channels configured for question delivery; question is available via HTTP API only"
            );
            return vec![];
        }

        let mut results = Vec::new();

        for channel_name in target_channels {
            match self.adapters.get(channel_name) {
                Some(adapter) => {
                    let result = adapter.deliver_question(question).await;
                    if result.success {
                        tracing::info!(
                            channel = %channel_name,
                            interaction_id = %question.interaction_id,
                            delivery_id = %result.delivery_id,
                            "Question delivered to channel"
                        );
                    } else {
                        tracing::warn!(
                            channel = %channel_name,
                            interaction_id = %question.interaction_id,
                            error = ?result.error,
                            "Failed to deliver question to channel"
                        );
                    }
                    results.push(result);
                }
                None => {
                    tracing::warn!(
                        channel = %channel_name,
                        interaction_id = %question.interaction_id,
                        registered = ?self.registered_channels(),
                        "Channel '{}' is not registered; skipping delivery. \
                         Configure it in .ta/daemon.toml under [channels.{}]",
                        channel_name,
                        channel_name
                    );
                    results.push(DeliveryResult {
                        channel: channel_name.clone(),
                        delivery_id: String::new(),
                        success: false,
                        error: Some(format!(
                            "Channel '{}' is not registered. Configure it in .ta/daemon.toml \
                             under [channels.{}]. Registered channels: {:?}",
                            channel_name,
                            channel_name,
                            self.registered_channels()
                        )),
                    });
                }
            }
        }

        results
    }

    /// Build a ChannelQuestion from event data.
    #[allow(clippy::too_many_arguments)]
    pub fn build_question(
        goal_id: Uuid,
        interaction_id: Uuid,
        question: &str,
        context: Option<&str>,
        response_hint: &str,
        choices: &[String],
        turn: u32,
        callback_url: &str,
    ) -> ChannelQuestion {
        ChannelQuestion {
            interaction_id,
            goal_id,
            question: question.to_string(),
            context: context.map(|s| s.to_string()),
            response_hint: response_hint.to_string(),
            choices: choices.to_vec(),
            turn,
            callback_url: callback_url.to_string(),
        }
    }

    /// Validate all registered adapters' configurations.
    pub async fn validate_all(&self) -> Vec<(String, Result<(), String>)> {
        let mut results = Vec::new();
        for (name, adapter) in &self.adapters {
            let result = adapter.validate().await;
            results.push((name.clone(), result));
        }
        results
    }

    /// Check if any channels are configured.
    pub fn has_channels(&self) -> bool {
        !self.adapters.is_empty()
    }

    /// Number of registered channel adapters.
    pub fn adapter_count(&self) -> usize {
        self.adapters.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_events::channel::ChannelQuestion;

    /// A test adapter that always succeeds.
    struct MockAdapter {
        name: String,
    }

    #[async_trait::async_trait]
    impl ChannelDelivery for MockAdapter {
        fn name(&self) -> &str {
            &self.name
        }

        async fn deliver_question(&self, question: &ChannelQuestion) -> DeliveryResult {
            DeliveryResult {
                channel: self.name.clone(),
                delivery_id: format!("mock-{}", question.interaction_id),
                success: true,
                error: None,
            }
        }

        async fn validate(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// A test adapter that always fails.
    struct FailAdapter;

    #[async_trait::async_trait]
    impl ChannelDelivery for FailAdapter {
        fn name(&self) -> &str {
            "fail"
        }

        async fn deliver_question(&self, _question: &ChannelQuestion) -> DeliveryResult {
            DeliveryResult {
                channel: "fail".into(),
                delivery_id: String::new(),
                success: false,
                error: Some("intentional failure".into()),
            }
        }

        async fn validate(&self) -> Result<(), String> {
            Err("fail adapter".into())
        }
    }

    fn test_question() -> ChannelQuestion {
        ChannelQuestion {
            interaction_id: Uuid::new_v4(),
            goal_id: Uuid::new_v4(),
            question: "Which DB?".into(),
            context: None,
            response_hint: "freeform".into(),
            choices: vec![],
            turn: 1,
            callback_url: "http://localhost:7700".into(),
        }
    }

    #[tokio::test]
    async fn dispatch_to_registered_channel() {
        let mut dispatcher = ChannelDispatcher::new(vec!["test".into()]);
        dispatcher.register(Arc::new(MockAdapter {
            name: "test".into(),
        }));

        let q = test_question();
        let results = dispatcher.dispatch(&q, &[]).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].channel, "test");
    }

    #[tokio::test]
    async fn dispatch_to_unknown_channel() {
        let dispatcher = ChannelDispatcher::new(vec![]);

        let q = test_question();
        let results = dispatcher.dispatch(&q, &["nonexistent".into()]).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert!(results[0]
            .error
            .as_ref()
            .unwrap()
            .contains("not registered"));
    }

    #[tokio::test]
    async fn dispatch_no_channels() {
        let dispatcher = ChannelDispatcher::new(vec![]);

        let q = test_question();
        let results = dispatcher.dispatch(&q, &[]).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn dispatch_uses_hints_over_defaults() {
        let mut dispatcher = ChannelDispatcher::new(vec!["default".into()]);
        dispatcher.register(Arc::new(MockAdapter {
            name: "default".into(),
        }));
        dispatcher.register(Arc::new(MockAdapter {
            name: "specific".into(),
        }));

        let q = test_question();
        let results = dispatcher.dispatch(&q, &["specific".into()]).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].channel, "specific");
    }

    #[tokio::test]
    async fn dispatch_multiple_channels() {
        let mut dispatcher = ChannelDispatcher::new(vec!["a".into(), "b".into()]);
        dispatcher.register(Arc::new(MockAdapter { name: "a".into() }));
        dispatcher.register(Arc::new(MockAdapter { name: "b".into() }));

        let q = test_question();
        let results = dispatcher.dispatch(&q, &[]).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.success));
    }

    #[tokio::test]
    async fn validate_all_reports_errors() {
        let mut dispatcher = ChannelDispatcher::new(vec![]);
        dispatcher.register(Arc::new(MockAdapter {
            name: "good".into(),
        }));
        dispatcher.register(Arc::new(FailAdapter));

        let results = dispatcher.validate_all().await;
        assert_eq!(results.len(), 2);
        let ok_count = results.iter().filter(|(_, r)| r.is_ok()).count();
        let err_count = results.iter().filter(|(_, r)| r.is_err()).count();
        assert_eq!(ok_count, 1);
        assert_eq!(err_count, 1);
    }

    #[tokio::test]
    async fn from_config_empty() {
        let config = ChannelsConfig::default();
        let dispatcher = ChannelDispatcher::from_config(&config);
        assert!(!dispatcher.has_channels());
        assert_eq!(dispatcher.adapter_count(), 0);
    }

    #[test]
    fn build_question_helper() {
        let gid = Uuid::new_v4();
        let iid = Uuid::new_v4();
        let q = ChannelDispatcher::build_question(
            gid,
            iid,
            "What?",
            Some("context"),
            "freeform",
            &[],
            1,
            "http://localhost:7700",
        );
        assert_eq!(q.goal_id, gid);
        assert_eq!(q.interaction_id, iid);
        assert_eq!(q.question, "What?");
    }

    #[test]
    fn registered_channels_list() {
        let mut dispatcher = ChannelDispatcher::new(vec![]);
        dispatcher.register(Arc::new(MockAdapter {
            name: "slack".into(),
        }));
        dispatcher.register(Arc::new(MockAdapter {
            name: "discord".into(),
        }));

        let channels = dispatcher.registered_channels();
        assert_eq!(channels.len(), 2);
        assert!(channels.contains(&"slack".to_string()));
        assert!(channels.contains(&"discord".to_string()));
    }
}
