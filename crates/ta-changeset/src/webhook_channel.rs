// webhook_channel.rs — Webhook-based ReviewChannel implementation (v0.5.3).
//
// Posts InteractionRequest JSON to a configured endpoint and awaits
// an InteractionResponse. For the MVP, this uses a file-based exchange
// pattern: TA writes the request to a file, an external process reads
// it and writes a response file. This avoids adding HTTP client deps
// to the data-model crate.
//
// For production use, integrate with reqwest in a higher-level crate
// or use the external webhook adapter pattern.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use crate::interaction::{
    ChannelCapabilities, Decision, InteractionRequest, InteractionResponse, Notification,
};
use crate::review_channel::{ReviewChannel, ReviewChannelError};

/// File-based webhook channel for external review integrations.
///
/// Exchange pattern:
/// 1. TA writes `{endpoint}/request-{id}.json` with the InteractionRequest
/// 2. External process reads it, decides, writes `{endpoint}/response-{id}.json`
/// 3. TA polls for the response file and parses it
///
/// The `endpoint` is a directory path (for file-based) or URL (for future HTTP).
pub struct WebhookChannel {
    endpoint: PathBuf,
    poll_interval: Duration,
    timeout: Duration,
    channel_id: String,
}

impl WebhookChannel {
    /// Create a new webhook channel with a directory endpoint.
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: PathBuf::from(endpoint),
            poll_interval: Duration::from_secs(2),
            timeout: Duration::from_secs(3600), // 1 hour default
            channel_id: format!("webhook:{}", endpoint),
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

    fn request_path(&self, id: &str) -> PathBuf {
        self.endpoint.join(format!("request-{}.json", id))
    }

    fn response_path(&self, id: &str) -> PathBuf {
        self.endpoint.join(format!("response-{}.json", id))
    }
}

/// The response file format that external integrations write.
#[derive(Debug, serde::Deserialize)]
struct WebhookResponse {
    decision: String,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    responder_id: Option<String>,
}

impl ReviewChannel for WebhookChannel {
    fn request_interaction(
        &self,
        request: &InteractionRequest,
    ) -> Result<InteractionResponse, ReviewChannelError> {
        let id = request.interaction_id.to_string();

        // Ensure endpoint directory exists.
        fs::create_dir_all(&self.endpoint)?;

        // Write the request file.
        let request_json = serde_json::to_string_pretty(request)
            .map_err(|e| ReviewChannelError::Other(format!("serialization error: {}", e)))?;
        fs::write(self.request_path(&id), &request_json)?;

        // Poll for response file.
        let start = Instant::now();
        let response_path = self.response_path(&id);

        loop {
            if response_path.exists() {
                let content = fs::read_to_string(&response_path)?;
                // Clean up files.
                let _ = fs::remove_file(self.request_path(&id));
                let _ = fs::remove_file(&response_path);

                let webhook_resp: WebhookResponse =
                    serde_json::from_str(&content).map_err(|e| {
                        ReviewChannelError::InvalidResponse(format!("invalid response JSON: {}", e))
                    })?;

                let decision = parse_decision(&webhook_resp.decision, &webhook_resp.reasoning)?;
                let mut response = InteractionResponse::new(request.interaction_id, decision);
                if let Some(reasoning) = webhook_resp.reasoning {
                    response = response.with_reasoning(reasoning);
                }
                if let Some(responder) = webhook_resp.responder_id {
                    response = response.with_responder(responder);
                } else {
                    response = response.with_responder(&self.channel_id);
                }

                return Ok(response);
            }

            if start.elapsed() > self.timeout {
                // Clean up request file on timeout.
                let _ = fs::remove_file(self.request_path(&id));
                return Err(ReviewChannelError::Timeout);
            }

            thread::sleep(self.poll_interval);
        }
    }

    fn notify(&self, notification: &Notification) -> Result<(), ReviewChannelError> {
        fs::create_dir_all(&self.endpoint)?;
        let path = self.endpoint.join(format!(
            "notification-{}.json",
            chrono::Utc::now().timestamp_millis()
        ));
        let json = serde_json::to_string_pretty(notification)
            .map_err(|e| ReviewChannelError::Other(format!("serialization error: {}", e)))?;
        fs::write(&path, json)?;
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
        &self.channel_id
    }
}

fn parse_decision(s: &str, reasoning: &Option<String>) -> Result<Decision, ReviewChannelError> {
    match s.to_lowercase().as_str() {
        "approve" | "approved" => Ok(Decision::Approve),
        "reject" | "rejected" | "deny" | "denied" => Ok(Decision::Reject {
            reason: reasoning
                .clone()
                .unwrap_or_else(|| "rejected via webhook".to_string()),
        }),
        "discuss" => Ok(Decision::Discuss),
        other => Err(ReviewChannelError::InvalidResponse(format!(
            "unknown decision: '{}'. Expected: approve, reject, discuss",
            other,
        ))),
    }
}

/// Stub for future Slack integration (v0.5.3).
///
/// Will use Block Kit cards for draft review and button callbacks for
/// approve/reject/discuss. Requires `reqwest` for Slack API calls.
pub struct SlackChannel {
    #[allow(dead_code)]
    channel_id: String,
}

impl SlackChannel {
    pub fn new(_token: &str, _channel: &str) -> Self {
        Self {
            channel_id: "slack:stub".to_string(),
        }
    }
}

/// Stub for future Email integration (v0.5.3).
///
/// Will use SMTP for sending review summaries and IMAP for parsing replies.
pub struct EmailChannel {
    #[allow(dead_code)]
    channel_id: String,
}

impl EmailChannel {
    pub fn new(_smtp_host: &str, _to: &str) -> Self {
        Self {
            channel_id: "email:stub".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::{InteractionKind, Urgency};
    use tempfile::TempDir;
    fn test_request() -> InteractionRequest {
        InteractionRequest::new(
            InteractionKind::DraftReview,
            serde_json::json!({"draft_id": "test-123"}),
            Urgency::Blocking,
        )
    }

    #[test]
    fn webhook_writes_request_file() {
        let dir = TempDir::new().unwrap();
        let channel = WebhookChannel::new(dir.path().to_str().unwrap());

        let request = test_request();
        let id = request.interaction_id.to_string();

        // Write a pre-existing response so we don't block.
        let response_path = dir.path().join(format!("response-{}.json", id));
        fs::write(
            &response_path,
            r#"{"decision": "approve", "reasoning": "looks good"}"#,
        )
        .unwrap();

        let resp = channel.request_interaction(&request).unwrap();
        assert_eq!(resp.decision, Decision::Approve);
        assert_eq!(resp.reasoning.unwrap(), "looks good");
    }

    #[test]
    fn webhook_timeout_on_missing_response() {
        let dir = TempDir::new().unwrap();
        let channel = WebhookChannel::new(dir.path().to_str().unwrap())
            .with_timeout(Duration::from_millis(100))
            .with_poll_interval(Duration::from_millis(20));

        let request = test_request();
        let result = channel.request_interaction(&request);
        assert!(matches!(result, Err(ReviewChannelError::Timeout)));
    }

    #[test]
    fn webhook_reject_decision() {
        let dir = TempDir::new().unwrap();
        let channel = WebhookChannel::new(dir.path().to_str().unwrap());

        let request = test_request();
        let id = request.interaction_id.to_string();

        let response_path = dir.path().join(format!("response-{}.json", id));
        fs::write(
            &response_path,
            r#"{"decision": "reject", "reasoning": "needs work"}"#,
        )
        .unwrap();

        let resp = channel.request_interaction(&request).unwrap();
        assert!(matches!(resp.decision, Decision::Reject { .. }));
    }

    #[test]
    fn webhook_notification_writes_file() {
        let dir = TempDir::new().unwrap();
        let channel = WebhookChannel::new(dir.path().to_str().unwrap());

        let notification = Notification::info("test notification");
        channel.notify(&notification).unwrap();

        let files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("notification-"))
            })
            .collect();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn parse_decision_variants() {
        let none = &None;
        assert_eq!(parse_decision("approve", none).unwrap(), Decision::Approve);
        assert_eq!(parse_decision("Approved", none).unwrap(), Decision::Approve);
        assert!(matches!(
            parse_decision("reject", none).unwrap(),
            Decision::Reject { .. }
        ));
        assert!(matches!(
            parse_decision("denied", none).unwrap(),
            Decision::Reject { .. }
        ));
        assert_eq!(parse_decision("discuss", none).unwrap(), Decision::Discuss);
        assert!(parse_decision("invalid", none).is_err());
    }

    #[test]
    fn build_channel_terminal() {
        use crate::review_channel::{build_channel, ReviewChannelConfig};
        let config = ReviewChannelConfig::default();
        let channel = build_channel(&config).unwrap();
        assert_eq!(channel.channel_id(), "terminal:stdio");
    }

    #[test]
    fn build_channel_auto_approve() {
        use crate::review_channel::{build_channel, ReviewChannelConfig};
        let config = ReviewChannelConfig {
            channel_type: "auto-approve".into(),
            ..Default::default()
        };
        let channel = build_channel(&config).unwrap();
        assert_eq!(channel.channel_id(), "auto-approve");
    }

    #[test]
    fn build_channel_webhook() {
        use crate::review_channel::{build_channel, ReviewChannelConfig};
        let dir = TempDir::new().unwrap();
        let config = ReviewChannelConfig {
            channel_type: "webhook".into(),
            channel_config: Some(serde_json::json!({
                "endpoint": dir.path().to_str().unwrap()
            })),
            ..Default::default()
        };
        let channel = build_channel(&config).unwrap();
        assert!(channel.channel_id().starts_with("webhook:"));
    }

    #[test]
    fn build_channel_unknown_type_errors() {
        use crate::review_channel::{build_channel, ReviewChannelConfig};
        let config = ReviewChannelConfig {
            channel_type: "carrier-pigeon".into(),
            ..Default::default()
        };
        assert!(build_channel(&config).is_err());
    }
}
