//! DiscordChannelFactory — ChannelFactory implementation for Discord.
//!
//! Reads config from `.ta/config.yaml` and builds DiscordReviewChannel
//! instances. Reads the bot token from an environment variable (default:
//! `TA_DISCORD_TOKEN`) rather than storing it in config.

use std::path::PathBuf;

use ta_changeset::channel_registry::{ChannelCapabilitySet, ChannelFactory};
use ta_changeset::review_channel::{ReviewChannel, ReviewChannelError};
use ta_changeset::session_channel::{SessionChannel, SessionChannelError};

use crate::channel::DiscordReviewChannel;

/// Factory for creating Discord review channels.
///
/// Config format:
/// ```yaml
/// type: discord
/// token_env: TA_DISCORD_TOKEN        # env var name (default: TA_DISCORD_TOKEN)
/// channel_id: "123456789"            # Discord channel snowflake
/// response_dir: ".ta/discord"        # response exchange dir (default: .ta/discord-responses)
/// allowed_roles: ["reviewer"]        # optional access control
/// allowed_users: ["user#1234"]       # optional access control
/// timeout_secs: 3600                 # optional timeout (default: 3600)
/// poll_interval_secs: 2              # optional poll interval (default: 2)
/// ```
pub struct DiscordChannelFactory;

impl ChannelFactory for DiscordChannelFactory {
    fn channel_type(&self) -> &str {
        "discord"
    }

    fn build_review(
        &self,
        config: &serde_json::Value,
    ) -> Result<Box<dyn ReviewChannel>, ReviewChannelError> {
        // Read token env var name (default: TA_DISCORD_TOKEN).
        let token_env = config
            .get("token_env")
            .and_then(|v| v.as_str())
            .unwrap_or("TA_DISCORD_TOKEN");

        let token = std::env::var(token_env).map_err(|_| {
            ReviewChannelError::Other(format!(
                "environment variable '{}' not set — set it to your Discord bot token. \
                 You can change the variable name with 'token_env' in your channel config.",
                token_env
            ))
        })?;

        // Read channel ID (required).
        let discord_channel_id = config
            .get("channel_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ReviewChannelError::Other(
                    "Discord channel config missing 'channel_id' — add the Discord channel \
                     snowflake ID to your .ta/config.yaml discord channel config"
                        .into(),
                )
            })?
            .to_string();

        // Read response directory (default: .ta/discord-responses).
        let response_dir = config
            .get("response_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".ta/discord-responses"));

        // Read access control lists.
        let allowed_users = config
            .get("allowed_users")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let allowed_roles = config
            .get("allowed_roles")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let mut channel = DiscordReviewChannel::new(
            token,
            discord_channel_id,
            response_dir,
            allowed_users,
            allowed_roles,
        );

        // Optional timeout.
        if let Some(timeout) = config.get("timeout_secs").and_then(|v| v.as_u64()) {
            channel = channel.with_timeout(std::time::Duration::from_secs(timeout));
        }

        // Optional poll interval.
        if let Some(interval) = config.get("poll_interval_secs").and_then(|v| v.as_u64()) {
            channel = channel.with_poll_interval(std::time::Duration::from_secs(interval));
        }

        Ok(Box::new(channel))
    }

    fn build_session(
        &self,
        _config: &serde_json::Value,
    ) -> Result<Box<dyn SessionChannel>, SessionChannelError> {
        Err(SessionChannelError::Other(
            "Discord does not support interactive sessions — use 'terminal' or 'web' \
             for session channels instead. Discord is designed for review interactions \
             (approve/deny) only."
                .into(),
        ))
    }

    fn capabilities(&self) -> ChannelCapabilitySet {
        ChannelCapabilitySet {
            supports_review: true,
            supports_session: false,
            supports_notify: true,
            supports_rich_media: true,
            supports_threads: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_changeset::channel_registry::ChannelFactory;

    #[test]
    fn channel_type_is_discord() {
        assert_eq!(DiscordChannelFactory.channel_type(), "discord");
    }

    #[test]
    fn capabilities_correct() {
        let caps = DiscordChannelFactory.capabilities();
        assert!(caps.supports_review);
        assert!(!caps.supports_session);
        assert!(caps.supports_notify);
        assert!(caps.supports_rich_media);
        assert!(!caps.supports_threads);
    }

    #[test]
    fn build_session_returns_error() {
        let result = DiscordChannelFactory.build_session(&serde_json::json!({}));
        let err = result.err().expect("should fail for session channel");
        assert!(err
            .to_string()
            .contains("does not support interactive sessions"));
    }

    #[test]
    fn build_review_missing_channel_id() {
        // Set a fake token env var for this test.
        std::env::set_var("TA_TEST_DISCORD_TOKEN", "fake-token");
        let config = serde_json::json!({
            "token_env": "TA_TEST_DISCORD_TOKEN"
        });
        let result = DiscordChannelFactory.build_review(&config);
        let err = result.err().expect("should fail with missing channel_id");
        assert!(err.to_string().contains("channel_id"));
        std::env::remove_var("TA_TEST_DISCORD_TOKEN");
    }

    #[test]
    fn build_review_missing_token() {
        let config = serde_json::json!({
            "token_env": "TA_NONEXISTENT_TOKEN_VAR_12345",
            "channel_id": "123456789"
        });
        let result = DiscordChannelFactory.build_review(&config);
        let err = result.err().expect("should fail with missing token");
        assert!(err.to_string().contains("TA_NONEXISTENT_TOKEN_VAR_12345"));
    }

    #[test]
    fn build_review_success_with_all_config() {
        std::env::set_var("TA_TEST_DISCORD_TOKEN_2", "fake-token");
        let config = serde_json::json!({
            "token_env": "TA_TEST_DISCORD_TOKEN_2",
            "channel_id": "123456789012345678",
            "response_dir": "/tmp/ta-test-discord",
            "allowed_users": ["reviewer#1234", "admin#5678"],
            "allowed_roles": ["reviewers", "admins"],
            "timeout_secs": 7200,
            "poll_interval_secs": 5
        });
        let result = DiscordChannelFactory.build_review(&config);
        let channel = result.expect("should build successfully");
        assert_eq!(channel.channel_id(), "discord:123456789012345678");
        std::env::remove_var("TA_TEST_DISCORD_TOKEN_2");
    }

    #[test]
    fn build_review_default_token_env() {
        // When no token_env is specified, it defaults to TA_DISCORD_TOKEN.
        let config = serde_json::json!({
            "channel_id": "123456789"
        });
        let result = DiscordChannelFactory.build_review(&config);
        // Will fail because TA_DISCORD_TOKEN is not set, but check the error message.
        let err = result
            .err()
            .expect("should fail with missing TA_DISCORD_TOKEN");
        assert!(err.to_string().contains("TA_DISCORD_TOKEN"));
    }
}
