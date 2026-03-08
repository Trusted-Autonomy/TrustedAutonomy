//! # ta-channel-discord
//!
//! Native Discord ReviewChannel implementation for Trusted Autonomy.
//!
//! This crate provides `DiscordChannelFactory` implementing `ChannelFactory`,
//! enabling draft review interactions directly through Discord. It posts rich
//! embeds with Approve/Deny buttons and awaits human decisions via Discord's
//! interaction API.
//!
//! ## Architecture
//!
//! Unlike the `ta-connector-discord` crate (which implements `ChannelDelivery`
//! for daemon-side question delivery), this crate implements `ReviewChannel`
//! for MCP gateway-side draft review:
//!
//! 1. Posts an embed with button components to a Discord channel via REST API
//! 2. Polls a response directory for interaction responses (written by the
//!    daemon's `/api/interactions/:id/respond` endpoint)
//! 3. Returns the human's decision to the MCP tool handler
//!
//! ## Access Control
//!
//! The `allowed_roles` and `allowed_users` config fields restrict who can
//! approve or deny drafts. The daemon interaction handler checks these
//! before writing a response file.
//!
//! ## Config
//!
//! ```yaml
//! channels:
//!   review:
//!     type: discord
//!     token_env: TA_DISCORD_TOKEN
//!     channel_id: "123456789"
//!     allowed_roles: ["reviewer"]
//!     allowed_users: ["user#1234"]
//! ```

mod channel;
mod error;
mod factory;
mod payload;

pub use channel::DiscordReviewChannel;
pub use error::DiscordChannelError;
pub use factory::DiscordChannelFactory;
pub use payload::{build_notification_embed, build_review_embed};

#[cfg(test)]
mod tests {
    use super::*;
    use ta_changeset::channel_registry::ChannelFactory;

    #[test]
    fn factory_channel_type() {
        let factory = DiscordChannelFactory;
        assert_eq!(factory.channel_type(), "discord");
    }

    #[test]
    fn factory_capabilities() {
        let factory = DiscordChannelFactory;
        let caps = factory.capabilities();
        assert!(caps.supports_review);
        assert!(!caps.supports_session);
        assert!(caps.supports_notify);
        assert!(caps.supports_rich_media);
        assert!(!caps.supports_threads);
    }

    #[test]
    fn factory_build_session_returns_error() {
        let factory = DiscordChannelFactory;
        let config = serde_json::json!({});
        let result = factory.build_session(&config);
        assert!(result.is_err());
    }

    #[test]
    fn factory_build_review_missing_token_env() {
        let factory = DiscordChannelFactory;
        let config = serde_json::json!({
            "channel_id": "123456789"
        });
        let result = factory.build_review(&config);
        assert!(result.is_err());
    }

    #[test]
    fn factory_build_review_missing_channel_id() {
        let factory = DiscordChannelFactory;
        let config = serde_json::json!({
            "token_env": "MY_TOKEN"
        });
        let result = factory.build_review(&config);
        assert!(result.is_err());
    }
}
