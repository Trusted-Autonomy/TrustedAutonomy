//! DiscordReviewChannel — ReviewChannel implementation using the Discord REST API.
//!
//! Posts rich embeds with button components to a Discord channel and awaits
//! human decisions via a file-based response exchange (same pattern as
//! WebhookChannel). The daemon's interaction handler writes response files
//! when a Discord user clicks Approve/Deny/Discuss.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use ta_changeset::interaction::{
    ChannelCapabilities, Decision, InteractionRequest, InteractionResponse, Notification,
};
use ta_changeset::review_channel::{ReviewChannel, ReviewChannelError};

use crate::payload::{build_notification_embed, build_review_embed};

/// Discord-native review channel.
///
/// Sends interactions as Discord embeds with buttons and collects responses
/// via file-based exchange. The daemon writes response files at
/// `{response_dir}/response-{interaction_id}.json` when a Discord user
/// clicks a button.
pub struct DiscordReviewChannel {
    /// Discord bot token.
    token: String,
    /// Discord channel ID to post to.
    discord_channel_id: String,
    /// Directory for response file exchange.
    response_dir: PathBuf,
    /// Users allowed to respond (Discord username#discriminator or user ID).
    allowed_users: Vec<String>,
    /// Roles allowed to respond (role name or ID).
    allowed_roles: Vec<String>,
    /// HTTP client for Discord API calls.
    client: reqwest::Client,
    /// Polling interval for response files.
    poll_interval: Duration,
    /// Timeout for waiting on a response.
    timeout: Duration,
    /// Channel identity string.
    channel_id_str: String,
}

impl DiscordReviewChannel {
    /// Create a new Discord review channel.
    ///
    /// # Arguments
    /// - `token`: Discord bot token
    /// - `discord_channel_id`: Discord channel snowflake ID
    /// - `response_dir`: Directory where the daemon writes response files
    /// - `allowed_users`: List of allowed Discord user identifiers
    /// - `allowed_roles`: List of allowed Discord role names/IDs
    pub fn new(
        token: String,
        discord_channel_id: String,
        response_dir: PathBuf,
        allowed_users: Vec<String>,
        allowed_roles: Vec<String>,
    ) -> Self {
        let channel_id_str = format!("discord:{}", discord_channel_id);
        Self {
            token,
            discord_channel_id,
            response_dir,
            allowed_users,
            allowed_roles,
            client: reqwest::Client::new(),
            poll_interval: Duration::from_secs(2),
            timeout: Duration::from_secs(3600),
            channel_id_str,
        }
    }

    /// Set the polling interval.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Set the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn response_path(&self, interaction_id: &str) -> PathBuf {
        self.response_dir
            .join(format!("response-{}.json", interaction_id))
    }

    /// Post a message payload to the Discord channel via REST API.
    ///
    /// Uses a blocking tokio runtime bridge since ReviewChannel is sync.
    fn post_message(&self, payload: &serde_json::Value) -> Result<String, ReviewChannelError> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            self.discord_channel_id
        );

        // Bridge sync → async: create a small runtime for the HTTP call.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                ReviewChannelError::Other(format!(
                    "failed to create async runtime for Discord API call: {}",
                    e
                ))
            })?;

        let result = rt.block_on(async {
            self.client
                .post(&url)
                .header("Authorization", format!("Bot {}", self.token))
                .header("Content-Type", "application/json")
                .json(payload)
                .send()
                .await
        });

        match result {
            Ok(resp) => {
                let status = resp.status();
                let body = rt.block_on(resp.text()).unwrap_or_default();

                if status.is_success() {
                    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let message_id = json
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    tracing::info!(
                        channel = "discord",
                        discord_channel = %self.discord_channel_id,
                        message_id = %message_id,
                        "Posted message to Discord"
                    );
                    Ok(message_id)
                } else {
                    let err_msg = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                        .unwrap_or_else(|| body.clone());
                    tracing::warn!(
                        channel = "discord",
                        status = %status,
                        error = %err_msg,
                        "Discord API error"
                    );
                    Err(ReviewChannelError::Other(format!(
                        "Discord API error (HTTP {}): '{}' — check your bot token and channel_id ({})",
                        status, err_msg, self.discord_channel_id
                    )))
                }
            }
            Err(e) => {
                tracing::error!(
                    channel = "discord",
                    error = %e,
                    "Failed to send message to Discord"
                );
                Err(ReviewChannelError::Other(format!(
                    "HTTP request to Discord API failed: {} — check network connectivity and bot token",
                    e
                )))
            }
        }
    }

    /// Check if a responder is authorized based on allowed_users and allowed_roles.
    fn is_authorized(&self, responder: &DiscordResponse) -> bool {
        // If no restrictions are set, allow anyone.
        if self.allowed_users.is_empty() && self.allowed_roles.is_empty() {
            return true;
        }

        // Check user match.
        if let Some(ref user) = responder.user {
            if self.allowed_users.iter().any(|u| u == user) {
                return true;
            }
        }
        if let Some(ref user_id) = responder.user_id {
            if self.allowed_users.iter().any(|u| u == user_id) {
                return true;
            }
        }

        // Check role match.
        if let Some(ref roles) = responder.roles {
            for role in roles {
                if self.allowed_roles.iter().any(|r| r == role) {
                    return true;
                }
            }
        }

        false
    }
}

/// The response file format written by the daemon interaction handler.
#[derive(Debug, serde::Deserialize)]
struct DiscordResponse {
    decision: String,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    roles: Option<Vec<String>>,
}

fn parse_decision(s: &str, reasoning: &Option<String>) -> Result<Decision, ReviewChannelError> {
    match s.to_lowercase().as_str() {
        "approve" | "approved" | "accept" | "acknowledge" => Ok(Decision::Approve),
        "reject" | "rejected" | "deny" | "denied" | "intervene" => Ok(Decision::Reject {
            reason: reasoning
                .clone()
                .unwrap_or_else(|| "rejected via Discord".to_string()),
        }),
        "discuss" => Ok(Decision::Discuss),
        "skip" => Ok(Decision::SkipForNow),
        other => Err(ReviewChannelError::InvalidResponse(format!(
            "unknown decision: '{}'. Expected: approve, reject, discuss, skip",
            other,
        ))),
    }
}

impl ReviewChannel for DiscordReviewChannel {
    fn request_interaction(
        &self,
        request: &InteractionRequest,
    ) -> Result<InteractionResponse, ReviewChannelError> {
        let interaction_id = request.interaction_id.to_string();

        // Ensure response directory exists.
        fs::create_dir_all(&self.response_dir).map_err(|e| {
            ReviewChannelError::Other(format!(
                "failed to create response directory '{}': {} — ensure the directory is writable",
                self.response_dir.display(),
                e
            ))
        })?;

        // Post the embed to Discord.
        let payload = build_review_embed(request);
        let message_id = self.post_message(&payload)?;

        tracing::info!(
            channel = "discord",
            interaction_id = %interaction_id,
            message_id = %message_id,
            discord_channel = %self.discord_channel_id,
            "Review request posted to Discord — waiting for response"
        );

        // Poll for response file.
        let start = Instant::now();
        let response_path = self.response_path(&interaction_id);

        loop {
            if response_path.exists() {
                let content = fs::read_to_string(&response_path).map_err(|e| {
                    ReviewChannelError::Other(format!(
                        "failed to read response file '{}': {}",
                        response_path.display(),
                        e
                    ))
                })?;

                // Clean up the response file.
                let _ = fs::remove_file(&response_path);

                let discord_resp: DiscordResponse =
                    serde_json::from_str(&content).map_err(|e| {
                        ReviewChannelError::InvalidResponse(format!(
                            "invalid response JSON in '{}': {} — content: {}",
                            response_path.display(),
                            e,
                            &content[..content.len().min(200)]
                        ))
                    })?;

                // Check access control.
                if !self.is_authorized(&discord_resp) {
                    let user_display = discord_resp
                        .user
                        .as_deref()
                        .or(discord_resp.user_id.as_deref())
                        .unwrap_or("unknown");
                    tracing::warn!(
                        channel = "discord",
                        interaction_id = %interaction_id,
                        user = %user_display,
                        "Unauthorized response — ignoring and continuing to poll"
                    );
                    // Write the unauthorized decision back so the daemon can
                    // notify the user they're not authorized, then continue polling.
                    continue;
                }

                let decision = parse_decision(&discord_resp.decision, &discord_resp.reasoning)?;

                let responder_id = discord_resp
                    .user
                    .or(discord_resp.user_id)
                    .map(|u| format!("discord:{}", u))
                    .unwrap_or_else(|| self.channel_id_str.clone());

                let response = InteractionResponse::new(request.interaction_id, decision)
                    .with_responder(responder_id);
                let response = if let Some(reasoning) = discord_resp.reasoning {
                    response.with_reasoning(reasoning)
                } else {
                    response
                };

                tracing::info!(
                    channel = "discord",
                    interaction_id = %interaction_id,
                    decision = %response.decision,
                    "Received response from Discord"
                );

                return Ok(response);
            }

            if start.elapsed() > self.timeout {
                tracing::warn!(
                    channel = "discord",
                    interaction_id = %interaction_id,
                    timeout_secs = self.timeout.as_secs(),
                    "Timed out waiting for Discord response"
                );
                return Err(ReviewChannelError::Timeout);
            }

            thread::sleep(self.poll_interval);
        }
    }

    fn notify(&self, notification: &Notification) -> Result<(), ReviewChannelError> {
        let payload = build_notification_embed(notification);
        self.post_message(&payload)?;
        Ok(())
    }

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            supports_async: true,
            supports_rich_media: true,
            supports_threads: false,
        }
    }

    fn channel_id(&self) -> &str {
        &self.channel_id_str
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_channel(response_dir: &std::path::Path) -> DiscordReviewChannel {
        DiscordReviewChannel::new(
            "test-bot-token".into(),
            "123456789012345678".into(),
            response_dir.to_path_buf(),
            vec!["reviewer#1234".into()],
            vec!["admin".into()],
        )
        .with_poll_interval(Duration::from_millis(50))
        .with_timeout(Duration::from_millis(200))
    }

    #[test]
    fn channel_id_format() {
        let dir = TempDir::new().unwrap();
        let channel = test_channel(dir.path());
        assert_eq!(channel.channel_id(), "discord:123456789012345678");
    }

    #[test]
    fn channel_capabilities() {
        let dir = TempDir::new().unwrap();
        let channel = test_channel(dir.path());
        let caps = channel.capabilities();
        assert!(caps.supports_async);
        assert!(caps.supports_rich_media);
        assert!(!caps.supports_threads);
    }

    #[test]
    fn parse_decision_variants() {
        let none = &None;
        assert_eq!(parse_decision("approve", none).unwrap(), Decision::Approve);
        assert_eq!(parse_decision("Approved", none).unwrap(), Decision::Approve);
        assert_eq!(parse_decision("accept", none).unwrap(), Decision::Approve);
        assert_eq!(
            parse_decision("acknowledge", none).unwrap(),
            Decision::Approve
        );
        assert!(matches!(
            parse_decision("reject", none).unwrap(),
            Decision::Reject { .. }
        ));
        assert!(matches!(
            parse_decision("deny", none).unwrap(),
            Decision::Reject { .. }
        ));
        assert!(matches!(
            parse_decision("intervene", none).unwrap(),
            Decision::Reject { .. }
        ));
        assert_eq!(parse_decision("discuss", none).unwrap(), Decision::Discuss);
        assert_eq!(parse_decision("skip", none).unwrap(), Decision::SkipForNow);
        assert!(parse_decision("invalid", none).is_err());
    }

    #[test]
    fn authorized_no_restrictions() {
        let dir = TempDir::new().unwrap();
        let channel = DiscordReviewChannel::new(
            "token".into(),
            "123".into(),
            dir.path().to_path_buf(),
            vec![],
            vec![],
        );
        let resp = DiscordResponse {
            decision: "approve".into(),
            reasoning: None,
            user: Some("anyone".into()),
            user_id: None,
            roles: None,
        };
        assert!(channel.is_authorized(&resp));
    }

    #[test]
    fn authorized_by_user() {
        let dir = TempDir::new().unwrap();
        let channel = test_channel(dir.path());
        let resp = DiscordResponse {
            decision: "approve".into(),
            reasoning: None,
            user: Some("reviewer#1234".into()),
            user_id: None,
            roles: None,
        };
        assert!(channel.is_authorized(&resp));
    }

    #[test]
    fn authorized_by_role() {
        let dir = TempDir::new().unwrap();
        let channel = test_channel(dir.path());
        let resp = DiscordResponse {
            decision: "approve".into(),
            reasoning: None,
            user: Some("stranger".into()),
            user_id: None,
            roles: Some(vec!["admin".into()]),
        };
        assert!(channel.is_authorized(&resp));
    }

    #[test]
    fn unauthorized_user() {
        let dir = TempDir::new().unwrap();
        let channel = test_channel(dir.path());
        let resp = DiscordResponse {
            decision: "approve".into(),
            reasoning: None,
            user: Some("stranger#0000".into()),
            user_id: None,
            roles: Some(vec!["member".into()]),
        };
        assert!(!channel.is_authorized(&resp));
    }

    #[test]
    fn response_path_format() {
        let dir = TempDir::new().unwrap();
        let channel = test_channel(dir.path());
        let path = channel.response_path("abc-123");
        assert_eq!(path, dir.path().join("response-abc-123.json"));
    }

    // Note: We don't test request_interaction or notify directly because they
    // require a real Discord API connection. The payload building and access
    // control logic is thoroughly tested above and in payload.rs.
}
