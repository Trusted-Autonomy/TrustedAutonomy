//! # ta-connector-discord
//!
//! Discord channel delivery adapter for Trusted Autonomy.
//!
//! Posts agent questions as embeds with button components to a Discord channel.
//! Responses come back via Discord's interaction handler, which calls
//! `POST /api/interactions/:id/respond` on the TA daemon.

use serde::{Deserialize, Serialize};
use ta_events::channel::{ChannelDelivery, ChannelQuestion, DeliveryResult};

/// Discord adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// Discord Bot Token.
    pub bot_token: String,
    /// Channel ID to post questions to.
    pub channel_id: String,
}

/// Discord channel delivery adapter.
///
/// Posts questions as rich embeds with button components for choices.
/// For freeform questions, instructs the user to reply in a thread.
pub struct DiscordAdapter {
    config: DiscordConfig,
    client: reqwest::Client,
}

impl DiscordAdapter {
    pub fn new(config: DiscordConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Build a Discord message payload with embed and components.
    fn build_payload(&self, question: &ChannelQuestion) -> serde_json::Value {
        let mut embed = serde_json::json!({
            "title": format!("Agent Question (turn {})", question.turn),
            "description": &question.question,
            "color": 0x5865F2, // Discord blurple
        });

        if let Some(ctx) = &question.context {
            embed["fields"] = serde_json::json!([{
                "name": "Context",
                "value": ctx,
                "inline": false,
            }]);
        }

        let mut payload = serde_json::json!({
            "embeds": [embed],
        });

        // Add button components for choice/yes_no.
        match question.response_hint.as_str() {
            "yes_no" => {
                payload["components"] = serde_json::json!([{
                    "type": 1, // ACTION_ROW
                    "components": [
                        {
                            "type": 2, // BUTTON
                            "style": 3, // SUCCESS (green)
                            "label": "Yes",
                            "custom_id": format!("ta_{}_{}_yes",
                                question.interaction_id,
                                question.callback_url.replace('/', "_")
                            ),
                        },
                        {
                            "type": 2,
                            "style": 4, // DANGER (red)
                            "label": "No",
                            "custom_id": format!("ta_{}_{}_no",
                                question.interaction_id,
                                question.callback_url.replace('/', "_")
                            ),
                        }
                    ]
                }]);
            }
            "choice" if !question.choices.is_empty() => {
                let buttons: Vec<serde_json::Value> = question
                    .choices
                    .iter()
                    .enumerate()
                    .take(5) // Discord limit: 5 buttons per row
                    .map(|(i, choice)| {
                        serde_json::json!({
                            "type": 2,
                            "style": 1, // PRIMARY (blurple)
                            "label": &choice[..choice.len().min(80)],
                            "custom_id": format!("ta_{}_choice_{}",
                                question.interaction_id, i
                            ),
                        })
                    })
                    .collect();

                payload["components"] = serde_json::json!([{
                    "type": 1,
                    "components": buttons,
                }]);
            }
            _ => {
                // Freeform: add a footer prompting thread reply.
                if let Some(embeds) = payload["embeds"].as_array_mut() {
                    if let Some(embed) = embeds.first_mut() {
                        embed["footer"] = serde_json::json!({
                            "text": format!(
                                "Reply in this thread to answer. ID: {}",
                                question.interaction_id
                            )
                        });
                    }
                }
            }
        }

        payload
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for DiscordAdapter {
    fn name(&self) -> &str {
        "discord"
    }

    async fn deliver_question(&self, question: &ChannelQuestion) -> DeliveryResult {
        let payload = self.build_payload(question);
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            self.config.channel_id
        );

        match self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                match resp.json::<serde_json::Value>().await {
                    Ok(json) => {
                        if status.is_success() {
                            let message_id = json
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            tracing::info!(
                                channel = "discord",
                                interaction_id = %question.interaction_id,
                                message_id = %message_id,
                                "Question delivered to Discord"
                            );
                            DeliveryResult {
                                channel: "discord".into(),
                                delivery_id: message_id,
                                success: true,
                                error: None,
                            }
                        } else {
                            let err_msg = json
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown error")
                                .to_string();
                            tracing::warn!(
                                channel = "discord",
                                interaction_id = %question.interaction_id,
                                status = %status,
                                error = %err_msg,
                                "Discord API returned error"
                            );
                            DeliveryResult {
                                channel: "discord".into(),
                                delivery_id: String::new(),
                                success: false,
                                error: Some(format!(
                                    "Discord API error (HTTP {}): '{}' posting question {} to channel {}",
                                    status, err_msg, question.interaction_id, self.config.channel_id
                                )),
                            }
                        }
                    }
                    Err(e) => DeliveryResult {
                        channel: "discord".into(),
                        delivery_id: String::new(),
                        success: false,
                        error: Some(format!(
                            "Failed to parse Discord API response for question {}: {}",
                            question.interaction_id, e
                        )),
                    },
                }
            }
            Err(e) => {
                tracing::error!(
                    channel = "discord",
                    interaction_id = %question.interaction_id,
                    error = %e,
                    "Failed to send question to Discord"
                );
                DeliveryResult {
                    channel: "discord".into(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "HTTP request to Discord API failed for question {}: {}",
                        question.interaction_id, e
                    )),
                }
            }
        }
    }

    async fn validate(&self) -> Result<(), String> {
        if self.config.bot_token.is_empty() {
            return Err(
                "Discord bot_token is empty. Set it in .ta/daemon.toml under [channels.discord]"
                    .into(),
            );
        }
        if self.config.channel_id.is_empty() {
            return Err(
                "Discord channel_id is empty. Set it in .ta/daemon.toml under [channels.discord]"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_config() -> DiscordConfig {
        DiscordConfig {
            bot_token: "test-bot-token".into(),
            channel_id: "123456789012345678".into(),
        }
    }

    fn test_question() -> ChannelQuestion {
        ChannelQuestion {
            interaction_id: Uuid::new_v4(),
            goal_id: Uuid::new_v4(),
            question: "Which database?".into(),
            context: Some("Setting up backend".into()),
            response_hint: "choice".into(),
            choices: vec!["PostgreSQL".into(), "SQLite".into()],
            turn: 1,
            callback_url: "http://localhost:7700".into(),
        }
    }

    #[test]
    fn build_payload_choice() {
        let adapter = DiscordAdapter::new(test_config());
        let q = test_question();
        let payload = adapter.build_payload(&q);
        assert!(payload.get("embeds").is_some());
        assert!(payload.get("components").is_some());
        let components = payload["components"].as_array().unwrap();
        assert_eq!(components.len(), 1);
        let buttons = components[0]["components"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
    }

    #[test]
    fn build_payload_yes_no() {
        let adapter = DiscordAdapter::new(test_config());
        let mut q = test_question();
        q.response_hint = "yes_no".into();
        q.choices = vec![];
        let payload = adapter.build_payload(&q);
        let buttons = payload["components"][0]["components"].as_array().unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0]["label"], "Yes");
        assert_eq!(buttons[1]["label"], "No");
    }

    #[test]
    fn build_payload_freeform() {
        let adapter = DiscordAdapter::new(test_config());
        let mut q = test_question();
        q.response_hint = "freeform".into();
        q.choices = vec![];
        let payload = adapter.build_payload(&q);
        assert!(payload.get("components").is_none());
        let embed = &payload["embeds"][0];
        assert!(embed.get("footer").is_some());
    }

    #[test]
    fn validate_empty_token() {
        let adapter = DiscordAdapter::new(DiscordConfig {
            bot_token: String::new(),
            channel_id: "123".into(),
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(adapter.validate());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bot_token is empty"));
    }

    #[test]
    fn validate_ok() {
        let adapter = DiscordAdapter::new(test_config());
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(rt.block_on(adapter.validate()).is_ok());
    }

    #[test]
    fn adapter_name() {
        let adapter = DiscordAdapter::new(test_config());
        assert_eq!(adapter.name(), "discord");
    }
}
