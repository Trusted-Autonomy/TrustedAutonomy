//! # ta-connector-slack
//!
//! Slack channel delivery adapter for Trusted Autonomy.
//!
//! Posts agent questions as Block Kit messages to a Slack channel.
//! Responses come back via Slack's interaction handler, which calls
//! `POST /api/interactions/:id/respond` on the TA daemon.

use serde::{Deserialize, Serialize};
use ta_events::channel::{ChannelDelivery, ChannelQuestion, DeliveryResult};

/// Slack adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Slack Bot User OAuth Token (xoxb-...).
    pub bot_token: String,
    /// Channel ID to post questions to.
    pub channel_id: String,
}

/// Slack channel delivery adapter.
///
/// Posts questions as Block Kit messages with action buttons for choices.
/// For freeform questions, includes a text prompt.
pub struct SlackAdapter {
    config: SlackConfig,
    client: reqwest::Client,
}

impl SlackAdapter {
    pub fn new(config: SlackConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Build a Slack Block Kit payload for a question.
    fn build_blocks(&self, question: &ChannelQuestion) -> serde_json::Value {
        let mut blocks = vec![
            // Header section with question text.
            serde_json::json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(
                        "*Agent Question* (turn {})\n{}",
                        question.turn, question.question
                    )
                }
            }),
        ];

        // Add context block if present.
        if let Some(ctx) = &question.context {
            blocks.push(serde_json::json!({
                "type": "context",
                "elements": [{
                    "type": "mrkdwn",
                    "text": format!("_Context:_ {}", ctx)
                }]
            }));
        }

        // Add choice buttons for "choice" or "yes_no" response hints.
        match question.response_hint.as_str() {
            "yes_no" => {
                blocks.push(serde_json::json!({
                    "type": "actions",
                    "block_id": format!("ta_respond_{}", question.interaction_id),
                    "elements": [
                        {
                            "type": "button",
                            "text": { "type": "plain_text", "text": "Yes" },
                            "style": "primary",
                            "action_id": "ta_answer_yes",
                            "value": serde_json::json!({
                                "interaction_id": question.interaction_id.to_string(),
                                "answer": "yes",
                                "callback_url": question.callback_url,
                            }).to_string()
                        },
                        {
                            "type": "button",
                            "text": { "type": "plain_text", "text": "No" },
                            "style": "danger",
                            "action_id": "ta_answer_no",
                            "value": serde_json::json!({
                                "interaction_id": question.interaction_id.to_string(),
                                "answer": "no",
                                "callback_url": question.callback_url,
                            }).to_string()
                        }
                    ]
                }));
            }
            "choice" if !question.choices.is_empty() => {
                let elements: Vec<serde_json::Value> = question
                    .choices
                    .iter()
                    .enumerate()
                    .map(|(i, choice)| {
                        serde_json::json!({
                            "type": "button",
                            "text": { "type": "plain_text", "text": choice },
                            "action_id": format!("ta_answer_choice_{}", i),
                            "value": serde_json::json!({
                                "interaction_id": question.interaction_id.to_string(),
                                "answer": choice,
                                "callback_url": question.callback_url,
                            }).to_string()
                        })
                    })
                    .collect();

                blocks.push(serde_json::json!({
                    "type": "actions",
                    "block_id": format!("ta_respond_{}", question.interaction_id),
                    "elements": elements
                }));
            }
            _ => {
                // Freeform: instruct the user to reply in thread.
                blocks.push(serde_json::json!({
                    "type": "context",
                    "elements": [{
                        "type": "mrkdwn",
                        "text": format!(
                            "_Reply in this thread to answer. Interaction ID: `{}`_",
                            question.interaction_id
                        )
                    }]
                }));
            }
        }

        serde_json::json!(blocks)
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for SlackAdapter {
    fn name(&self) -> &str {
        "slack"
    }

    async fn deliver_question(&self, question: &ChannelQuestion) -> DeliveryResult {
        let blocks = self.build_blocks(question);
        let body = serde_json::json!({
            "channel": self.config.channel_id,
            "text": format!("Agent question: {}", question.question),
            "blocks": blocks,
        });

        match self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.config.bot_token)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    if json.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                        let ts = json
                            .get("ts")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        tracing::info!(
                            channel = "slack",
                            interaction_id = %question.interaction_id,
                            message_ts = %ts,
                            "Question delivered to Slack"
                        );
                        DeliveryResult {
                            channel: "slack".into(),
                            delivery_id: ts,
                            success: true,
                            error: None,
                        }
                    } else {
                        let err = json
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown_error")
                            .to_string();
                        tracing::warn!(
                            channel = "slack",
                            interaction_id = %question.interaction_id,
                            error = %err,
                            "Slack API returned error"
                        );
                        DeliveryResult {
                            channel: "slack".into(),
                            delivery_id: String::new(),
                            success: false,
                            error: Some(format!(
                                "Slack API error '{}' posting question {} to channel {}",
                                err, question.interaction_id, self.config.channel_id
                            )),
                        }
                    }
                }
                Err(e) => DeliveryResult {
                    channel: "slack".into(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "Failed to parse Slack API response for question {}: {}",
                        question.interaction_id, e
                    )),
                },
            },
            Err(e) => {
                tracing::error!(
                    channel = "slack",
                    interaction_id = %question.interaction_id,
                    error = %e,
                    "Failed to send question to Slack"
                );
                DeliveryResult {
                    channel: "slack".into(),
                    delivery_id: String::new(),
                    success: false,
                    error: Some(format!(
                        "HTTP request to Slack API failed for question {}: {}",
                        question.interaction_id, e
                    )),
                }
            }
        }
    }

    async fn validate(&self) -> Result<(), String> {
        if self.config.bot_token.is_empty() {
            return Err(
                "Slack bot_token is empty. Set it in .ta/daemon.toml under [channels.slack]".into(),
            );
        }
        if self.config.channel_id.is_empty() {
            return Err(
                "Slack channel_id is empty. Set it in .ta/daemon.toml under [channels.slack]"
                    .into(),
            );
        }
        if !self.config.bot_token.starts_with("xoxb-") {
            return Err(format!(
                "Slack bot_token '{}...' does not start with 'xoxb-'. \
                 Use a Bot User OAuth Token from your Slack app settings.",
                &self.config.bot_token[..self.config.bot_token.len().min(8)]
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_config() -> SlackConfig {
        SlackConfig {
            bot_token: "xoxb-test-token".into(),
            channel_id: "C1234567890".into(),
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
    fn build_blocks_choice() {
        let adapter = SlackAdapter::new(test_config());
        let q = test_question();
        let blocks = adapter.build_blocks(&q);
        let arr = blocks.as_array().unwrap();
        // Header + context + actions = 3 blocks
        assert_eq!(arr.len(), 3);
        // Last block is actions
        assert_eq!(arr[2]["type"], "actions");
        let elements = arr[2]["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2); // PostgreSQL, SQLite
    }

    #[test]
    fn build_blocks_yes_no() {
        let adapter = SlackAdapter::new(test_config());
        let mut q = test_question();
        q.response_hint = "yes_no".into();
        q.choices = vec![];
        let blocks = adapter.build_blocks(&q);
        let arr = blocks.as_array().unwrap();
        let actions = arr.last().unwrap();
        assert_eq!(actions["type"], "actions");
        let elements = actions["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2); // Yes, No
    }

    #[test]
    fn build_blocks_freeform() {
        let adapter = SlackAdapter::new(test_config());
        let mut q = test_question();
        q.response_hint = "freeform".into();
        q.choices = vec![];
        q.context = None;
        let blocks = adapter.build_blocks(&q);
        let arr = blocks.as_array().unwrap();
        // Header + freeform context hint = 2 blocks
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["type"], "context");
    }

    #[test]
    fn validate_empty_token() {
        let adapter = SlackAdapter::new(SlackConfig {
            bot_token: String::new(),
            channel_id: "C123".into(),
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(adapter.validate());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("bot_token is empty"));
    }

    #[test]
    fn validate_bad_token_prefix() {
        let adapter = SlackAdapter::new(SlackConfig {
            bot_token: "xoxp-user-token".into(),
            channel_id: "C123".into(),
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(adapter.validate());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("xoxb-"));
    }

    #[test]
    fn validate_ok() {
        let adapter = SlackAdapter::new(test_config());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(adapter.validate());
        assert!(result.is_ok());
    }

    #[test]
    fn adapter_name() {
        let adapter = SlackAdapter::new(test_config());
        assert_eq!(adapter.name(), "slack");
    }
}
