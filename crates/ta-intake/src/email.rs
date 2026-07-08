//! `inbound-email` trigger type — polls a configured messaging plugin for
//! new inbound messages and normalizes each into a `TriggerEvent`. Shipped
//! as one of the two real, working trigger types required by v0.17.0.12.19.
//!
//! Reuses the existing email connector's fetch capability
//! (`ta_submit::messaging_adapter::ExternalMessagingAdapter::fetch`, the
//! same one `ta workflow run email-manager` already uses) instead of
//! reimplementing IMAP/Gmail/Outlook polling — per the plan item's explicit
//! instruction. The `MessageFetcher` trait is the seam that keeps this
//! injectable for testing, matching the `MessagingOps` pattern already used
//! by `apps/ta-cli/src/commands/email_manager.rs`.

use crate::event::{TriggerError, TriggerEvent, TriggerSource};
use crate::manifest::TriggerManifest;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::path::Path;
use ta_submit::messaging_adapter::ExternalMessagingAdapter;
use ta_submit::messaging_plugin_protocol::FetchedMessage;
use uuid::Uuid;

const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Abstracts "fetch messages since a watermark" so `EmailTriggerSource` is
/// testable without spawning a real messaging plugin subprocess.
pub trait MessageFetcher {
    fn fetch(
        &self,
        since: &str,
        account: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<FetchedMessage>, String>;
}

impl MessageFetcher for ExternalMessagingAdapter {
    fn fetch(
        &self,
        since: &str,
        account: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<FetchedMessage>, String> {
        ExternalMessagingAdapter::fetch(self, since, account, limit).map_err(|e| e.to_string())
    }
}

pub struct EmailTriggerSource<F: MessageFetcher> {
    manifest: TriggerManifest,
    fetcher: F,
}

impl<F: MessageFetcher> EmailTriggerSource<F> {
    pub fn new(manifest: TriggerManifest, fetcher: F) -> Self {
        Self { manifest, fetcher }
    }

    fn account(&self) -> Option<String> {
        self.manifest.get_str("account").map(str::to_string)
    }

    fn limit(&self) -> Option<u32> {
        self.manifest
            .get_u64("max_messages_per_poll")
            .and_then(|v| u32::try_from(v).ok())
    }
}

impl EmailTriggerSource<ExternalMessagingAdapter> {
    /// Build a production `EmailTriggerSource` from the messaging plugin
    /// named by this config's `settings.provider` (e.g. `"gmail"`),
    /// discovered the same way `ta workflow run email-manager` finds it.
    pub fn from_plugin(
        manifest: TriggerManifest,
        project_root: &Path,
    ) -> Result<Self, TriggerError> {
        let provider = manifest
            .get_str("provider")
            .ok_or_else(|| TriggerError::InvalidConfig {
                path: "inbound-email trigger settings".into(),
                reason: "missing required `settings.provider` (e.g. \"gmail\")".into(),
            })?
            .to_string();
        let discovered = ta_submit::messaging_adapter::find_messaging_plugin(
            &provider,
            project_root,
        )
        .ok_or_else(|| TriggerError::InvalidConfig {
            path: "inbound-email trigger settings".into(),
            reason: format!(
                "no messaging plugin named '{provider}' found (checked .ta/plugins/messaging/ and PATH)"
            ),
        })?;
        let fetcher = ExternalMessagingAdapter::new(&discovered.manifest);
        Ok(Self::new(manifest, fetcher))
    }
}

impl<F: MessageFetcher> TriggerSource for EmailTriggerSource<F> {
    fn trigger_type(&self) -> &str {
        "inbound-email"
    }

    fn poll(&self, since: Option<&str>) -> Result<Vec<TriggerEvent>, TriggerError> {
        let since = since.unwrap_or(EPOCH);
        let messages = self
            .fetcher
            .fetch(since, self.account().as_deref(), self.limit())
            .map_err(|reason| TriggerError::PollFailed {
                trigger_type: "inbound-email".into(),
                reason,
            })?;

        Ok(messages
            .into_iter()
            .map(|m| {
                let occurred_at = DateTime::parse_from_rfc3339(&m.received_at)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                TriggerEvent {
                    id: Uuid::new_v4(),
                    trigger_type: "inbound-email".into(),
                    source: m.from.clone(),
                    occurred_at,
                    payload: json!({
                        "message_id": m.id,
                        "from": m.from,
                        "to": m.to,
                        "subject": m.subject,
                        "body_text": m.body_text,
                    }),
                    suggested_goal_title: format!("Handle inbound email: {}", m.subject),
                    dedupe_key: Some(format!("inbound-email:{}", m.id)),
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Dispatch;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct FakeFetcher {
        messages: Mutex<Vec<FetchedMessage>>,
        seen_since: Mutex<Vec<String>>,
    }

    impl MessageFetcher for FakeFetcher {
        fn fetch(
            &self,
            since: &str,
            _account: Option<&str>,
            _limit: Option<u32>,
        ) -> Result<Vec<FetchedMessage>, String> {
            self.seen_since.lock().unwrap().push(since.to_string());
            Ok(self.messages.lock().unwrap().drain(..).collect())
        }
    }

    fn test_message(id: &str, subject: &str) -> FetchedMessage {
        FetchedMessage {
            id: id.into(),
            from: "alice@example.com".into(),
            to: "intake@example.com".into(),
            subject: subject.into(),
            body_text: "hello".into(),
            body_html: String::new(),
            thread_id: String::new(),
            received_at: "2026-07-06T12:00:00Z".into(),
        }
    }

    fn manifest(settings: HashMap<String, toml::Value>) -> TriggerManifest {
        TriggerManifest {
            trigger_type: "inbound-email".into(),
            enabled: true,
            dispatch: Dispatch::Queue,
            description: None,
            settings,
        }
    }

    #[test]
    fn normalizes_fetched_messages_into_events() {
        let fetcher = FakeFetcher {
            messages: Mutex::new(vec![test_message("m1", "Need help with billing")]),
            seen_since: Mutex::new(vec![]),
        };
        let source = EmailTriggerSource::new(manifest(HashMap::new()), fetcher);
        let events = source.poll(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].trigger_type, "inbound-email");
        assert_eq!(events[0].source, "alice@example.com");
        assert_eq!(
            events[0].suggested_goal_title,
            "Handle inbound email: Need help with billing"
        );
        assert_eq!(events[0].dedupe_key, Some("inbound-email:m1".to_string()));
    }

    #[test]
    fn no_watermark_defaults_to_epoch() {
        let fetcher = FakeFetcher {
            messages: Mutex::new(vec![]),
            seen_since: Mutex::new(vec![]),
        };
        let source = EmailTriggerSource::new(manifest(HashMap::new()), fetcher);
        source.poll(None).unwrap();
        assert_eq!(
            source.fetcher.seen_since.lock().unwrap().as_slice(),
            &["1970-01-01T00:00:00Z".to_string()]
        );
    }

    #[test]
    fn watermark_is_forwarded_to_fetcher() {
        let fetcher = FakeFetcher {
            messages: Mutex::new(vec![]),
            seen_since: Mutex::new(vec![]),
        };
        let source = EmailTriggerSource::new(manifest(HashMap::new()), fetcher);
        source.poll(Some("2026-07-01T00:00:00Z")).unwrap();
        assert_eq!(
            source.fetcher.seen_since.lock().unwrap().as_slice(),
            &["2026-07-01T00:00:00Z".to_string()]
        );
    }

    #[test]
    fn fetch_error_is_reported_as_poll_failed() {
        struct FailingFetcher;
        impl MessageFetcher for FailingFetcher {
            fn fetch(
                &self,
                _since: &str,
                _account: Option<&str>,
                _limit: Option<u32>,
            ) -> Result<Vec<FetchedMessage>, String> {
                Err("plugin process exited with status 1".into())
            }
        }
        let source = EmailTriggerSource::new(manifest(HashMap::new()), FailingFetcher);
        let result = source.poll(None);
        assert!(matches!(result, Err(TriggerError::PollFailed { .. })));
    }

    #[test]
    fn empty_inbox_produces_no_events() {
        let fetcher = FakeFetcher {
            messages: Mutex::new(vec![]),
            seen_since: Mutex::new(vec![]),
        };
        let source = EmailTriggerSource::new(manifest(HashMap::new()), fetcher);
        assert!(source.poll(None).unwrap().is_empty());
    }
}
