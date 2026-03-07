// channel.rs — Channel delivery trait for routing questions to external interfaces.
//
// When an agent calls ta_ask_human, the question can be delivered to one or
// more external channels (Slack, Discord, email, etc.) in addition to or
// instead of the local `ta shell` interface. Each channel implements this
// trait and is registered with the daemon's channel dispatcher.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A question to be delivered through an external channel.
///
/// This is a simplified view of PendingQuestion, containing only the fields
/// that channel adapters need for rendering and delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelQuestion {
    /// Unique interaction ID — used to correlate responses.
    pub interaction_id: Uuid,
    /// Goal this question belongs to.
    pub goal_id: Uuid,
    /// The question text.
    pub question: String,
    /// Optional context about what the agent was doing.
    pub context: Option<String>,
    /// Expected response shape: "freeform", "yes_no", "choice".
    pub response_hint: String,
    /// Suggested choices when response_hint is "choice".
    pub choices: Vec<String>,
    /// Turn number in the conversation (1-based).
    pub turn: u32,
    /// The daemon's base URL for posting responses.
    /// Channels should POST to `{callback_url}/api/interactions/{interaction_id}/respond`.
    pub callback_url: String,
}

/// Result of a channel delivery attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryResult {
    /// Channel name that handled the delivery.
    pub channel: String,
    /// Channel-specific delivery ID (e.g., Slack message timestamp, Discord message ID).
    pub delivery_id: String,
    /// Whether the delivery was successful.
    pub success: bool,
    /// Error message if delivery failed.
    pub error: Option<String>,
}

/// Trait for channel adapters that deliver questions to external interfaces.
///
/// Each channel implementation renders the question in its native format
/// (Slack Block Kit, Discord embeds, email HTML) and provides a mechanism
/// for the human to respond, which then calls back to the daemon's
/// `POST /api/interactions/:id/respond` endpoint.
#[async_trait::async_trait]
pub trait ChannelDelivery: Send + Sync {
    /// Human-readable name of the channel (e.g., "slack", "discord", "email").
    fn name(&self) -> &str;

    /// Deliver a question to the channel.
    ///
    /// Returns a `DeliveryResult` with the channel-specific delivery ID.
    /// The channel is responsible for rendering the question appropriately
    /// and setting up a response mechanism.
    async fn deliver_question(&self, question: &ChannelQuestion) -> DeliveryResult;

    /// Validate that the channel's configuration is correct and the
    /// channel is reachable.
    async fn validate(&self) -> Result<(), String>;
}

/// Channel routing configuration — which channels to deliver to for a given event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelRouting {
    /// Explicit list of channel names to deliver to.
    /// Empty means use the daemon's default channel list.
    #[serde(default)]
    pub channels: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_question_serialization() {
        let q = ChannelQuestion {
            interaction_id: Uuid::new_v4(),
            goal_id: Uuid::new_v4(),
            question: "Which database should I use?".into(),
            context: Some("Setting up the backend".into()),
            response_hint: "choice".into(),
            choices: vec!["PostgreSQL".into(), "SQLite".into()],
            turn: 1,
            callback_url: "http://localhost:7700".into(),
        };
        let json = serde_json::to_string(&q).unwrap();
        let restored: ChannelQuestion = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.question, q.question);
        assert_eq!(restored.choices.len(), 2);
    }

    #[test]
    fn delivery_result_success() {
        let r = DeliveryResult {
            channel: "slack".into(),
            delivery_id: "1234567890.123456".into(),
            success: true,
            error: None,
        };
        assert!(r.success);
        assert!(r.error.is_none());
    }

    #[test]
    fn delivery_result_failure() {
        let r = DeliveryResult {
            channel: "discord".into(),
            delivery_id: String::new(),
            success: false,
            error: Some("Bot token invalid".into()),
        };
        assert!(!r.success);
        assert!(r.error.is_some());
    }

    #[test]
    fn channel_routing_default_is_empty() {
        let routing = ChannelRouting::default();
        assert!(routing.channels.is_empty());
    }

    #[test]
    fn channel_routing_serialization() {
        let routing = ChannelRouting {
            channels: vec!["slack".into(), "email".into()],
        };
        let json = serde_json::to_string(&routing).unwrap();
        let restored: ChannelRouting = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.channels.len(), 2);
    }
}
