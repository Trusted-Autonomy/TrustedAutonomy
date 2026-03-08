// multi_channel.rs — Multi-channel routing for ReviewChannel (v0.10.0).
//
// MultiReviewChannel dispatches review requests to N inner channels.
// Configurable strategy: `first_response` (default — first Ok wins) or
// `quorum` (require N approvals before returning).

use crate::interaction::{
    ChannelCapabilities, InteractionRequest, InteractionResponse, Notification,
};
use crate::review_channel::{ReviewChannel, ReviewChannelError};

/// Dispatch strategy for multi-channel review.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiChannelStrategy {
    /// First channel to respond wins (default).
    #[default]
    FirstResponse,
    /// Require `quorum_size` approvals before returning.
    Quorum { quorum_size: usize },
}

/// A ReviewChannel that dispatches to multiple inner channels.
///
/// For `request_interaction`, channels are tried sequentially — the first
/// successful response is returned. For `notify`, all channels receive the
/// notification (fan-out). Failures on individual channels are logged but
/// don't prevent delivery to remaining channels.
pub struct MultiReviewChannel {
    channels: Vec<Box<dyn ReviewChannel>>,
    strategy: MultiChannelStrategy,
}

impl MultiReviewChannel {
    /// Create a new multi-channel from inner channels.
    ///
    /// # Panics
    /// Panics if `channels` is empty — use a single channel directly instead.
    pub fn new(channels: Vec<Box<dyn ReviewChannel>>, strategy: MultiChannelStrategy) -> Self {
        assert!(
            !channels.is_empty(),
            "MultiReviewChannel requires at least one inner channel"
        );
        Self { channels, strategy }
    }

    /// Wrap a single channel (no-op wrapper for uniform handling).
    pub fn single(channel: Box<dyn ReviewChannel>) -> Self {
        Self {
            channels: vec![channel],
            strategy: MultiChannelStrategy::FirstResponse,
        }
    }

    /// Number of inner channels.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// Whether this multi-channel is empty (should never be true).
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// The configured dispatch strategy.
    pub fn strategy(&self) -> &MultiChannelStrategy {
        &self.strategy
    }

    /// Channel IDs of all inner channels.
    pub fn inner_channel_ids(&self) -> Vec<&str> {
        self.channels.iter().map(|c| c.channel_id()).collect()
    }
}

impl ReviewChannel for MultiReviewChannel {
    fn request_interaction(
        &self,
        request: &InteractionRequest,
    ) -> Result<InteractionResponse, ReviewChannelError> {
        match &self.strategy {
            MultiChannelStrategy::FirstResponse => {
                let mut last_err = None;
                for channel in &self.channels {
                    match channel.request_interaction(request) {
                        Ok(response) => {
                            tracing::info!(
                                channel_id = channel.channel_id(),
                                interaction_id = %request.interaction_id,
                                "multi-channel: got response from channel"
                            );
                            return Ok(response);
                        }
                        Err(e) => {
                            tracing::warn!(
                                channel_id = channel.channel_id(),
                                error = %e,
                                "multi-channel: channel failed, trying next"
                            );
                            last_err = Some(e);
                        }
                    }
                }
                Err(last_err
                    .unwrap_or_else(|| ReviewChannelError::Other("no channels available".into())))
            }
            MultiChannelStrategy::Quorum { quorum_size } => {
                let mut approvals = 0usize;
                let mut last_response = None;
                let mut errors = Vec::new();

                for channel in &self.channels {
                    match channel.request_interaction(request) {
                        Ok(response) => {
                            approvals += 1;
                            last_response = Some(response);
                            if approvals >= *quorum_size {
                                tracing::info!(
                                    approvals,
                                    quorum_size,
                                    "multi-channel: quorum reached"
                                );
                                return Ok(last_response.unwrap());
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                channel_id = channel.channel_id(),
                                error = %e,
                                "multi-channel: channel failed in quorum"
                            );
                            errors.push(e);
                        }
                    }
                }

                // Not enough approvals.
                if let Some(response) = last_response {
                    tracing::warn!(
                        approvals,
                        quorum_size,
                        "multi-channel: quorum not reached, returning best response"
                    );
                    Ok(response)
                } else {
                    Err(errors.into_iter().next().unwrap_or_else(|| {
                        ReviewChannelError::Other(format!(
                            "quorum not reached: needed {quorum_size} approvals, got {approvals}"
                        ))
                    }))
                }
            }
        }
    }

    fn notify(&self, notification: &Notification) -> Result<(), ReviewChannelError> {
        let mut last_err = None;
        let mut delivered = 0usize;

        for channel in &self.channels {
            match channel.notify(notification) {
                Ok(()) => delivered += 1,
                Err(e) => {
                    tracing::warn!(
                        channel_id = channel.channel_id(),
                        error = %e,
                        "multi-channel: notification delivery failed on channel"
                    );
                    last_err = Some(e);
                }
            }
        }

        if delivered > 0 {
            Ok(())
        } else {
            Err(last_err.unwrap_or_else(|| {
                ReviewChannelError::Other("no channels delivered notification".into())
            }))
        }
    }

    fn capabilities(&self) -> ChannelCapabilities {
        // Merge capabilities: if any channel supports a capability, the
        // multi-channel reports it.
        let mut caps = ChannelCapabilities::default();
        for channel in &self.channels {
            let c = channel.capabilities();
            caps.supports_async = caps.supports_async || c.supports_async;
            caps.supports_rich_media = caps.supports_rich_media || c.supports_rich_media;
            caps.supports_threads = caps.supports_threads || c.supports_threads;
        }
        caps
    }

    fn channel_id(&self) -> &str {
        "multi-channel"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::{InteractionKind, Urgency};
    use crate::terminal_channel::AutoApproveChannel;

    fn test_request() -> InteractionRequest {
        InteractionRequest::new(
            InteractionKind::DraftReview,
            serde_json::json!({"draft_id": "test"}),
            Urgency::Blocking,
        )
    }

    #[test]
    fn single_channel_passthrough() {
        let ch = MultiReviewChannel::single(Box::new(AutoApproveChannel::new()));
        assert_eq!(ch.len(), 1);
        assert_eq!(ch.channel_id(), "multi-channel");
        let resp = ch.request_interaction(&test_request());
        assert!(resp.is_ok());
    }

    #[test]
    fn multi_channel_first_response() {
        let channels: Vec<Box<dyn ReviewChannel>> = vec![
            Box::new(AutoApproveChannel::new()),
            Box::new(AutoApproveChannel::new()),
        ];
        let ch = MultiReviewChannel::new(channels, MultiChannelStrategy::FirstResponse);
        assert_eq!(ch.len(), 2);
        let resp = ch.request_interaction(&test_request());
        assert!(resp.is_ok());
    }

    #[test]
    fn multi_channel_quorum() {
        let channels: Vec<Box<dyn ReviewChannel>> = vec![
            Box::new(AutoApproveChannel::new()),
            Box::new(AutoApproveChannel::new()),
            Box::new(AutoApproveChannel::new()),
        ];
        let ch = MultiReviewChannel::new(channels, MultiChannelStrategy::Quorum { quorum_size: 2 });
        let resp = ch.request_interaction(&test_request());
        assert!(resp.is_ok());
    }

    #[test]
    fn notify_fans_out() {
        let channels: Vec<Box<dyn ReviewChannel>> = vec![
            Box::new(AutoApproveChannel::new()),
            Box::new(AutoApproveChannel::new()),
        ];
        let ch = MultiReviewChannel::new(channels, MultiChannelStrategy::FirstResponse);
        let notif = Notification {
            notification_id: uuid::Uuid::new_v4(),
            level: crate::interaction::NotificationLevel::Info,
            message: "test notification".into(),
            created_at: chrono::Utc::now(),
            goal_id: None,
        };
        assert!(ch.notify(&notif).is_ok());
    }

    #[test]
    fn inner_channel_ids() {
        let channels: Vec<Box<dyn ReviewChannel>> = vec![
            Box::new(AutoApproveChannel::new()),
            Box::new(AutoApproveChannel::new()),
        ];
        let ch = MultiReviewChannel::new(channels, MultiChannelStrategy::FirstResponse);
        let ids = ch.inner_channel_ids();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn capabilities_merge() {
        let channels: Vec<Box<dyn ReviewChannel>> = vec![Box::new(AutoApproveChannel::new())];
        let ch = MultiReviewChannel::new(channels, MultiChannelStrategy::FirstResponse);
        let caps = ch.capabilities();
        // AutoApproveChannel has default capabilities
        assert!(!caps.supports_rich_media);
    }

    #[test]
    #[should_panic(expected = "requires at least one")]
    fn empty_channels_panic() {
        let _ch = MultiReviewChannel::new(vec![], MultiChannelStrategy::FirstResponse);
    }

    #[test]
    fn strategy_accessor() {
        let ch = MultiReviewChannel::single(Box::new(AutoApproveChannel::new()));
        assert_eq!(ch.strategy(), &MultiChannelStrategy::FirstResponse);
    }
}
