//! `schedule` trigger type — fires a recurring `TriggerEvent` once at least
//! `interval_secs` has elapsed since the last fire (a watermark, tracked by
//! the caller; `ta-intake` is stateless). Shipped as one of the two real,
//! working trigger types required by v0.17.0.12.19.
//!
//! "Cron-like" per the plan, not full cron-expression parsing: a plain
//! recurring interval covers the real use case (periodic maintenance/check
//! goals) without pulling in a cron grammar dependency. A community trigger
//! type is free to implement real cron parsing against the same
//! `TriggerSource` trait without any change here.

use crate::event::{TriggerError, TriggerEvent, TriggerSource};
use crate::manifest::TriggerManifest;
use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

const DEFAULT_INTERVAL_SECS: u64 = 3600;
const DEFAULT_GOAL_TITLE: &str = "Scheduled trigger fired";

pub struct ScheduleTriggerSource {
    manifest: TriggerManifest,
}

impl ScheduleTriggerSource {
    pub fn new(manifest: TriggerManifest) -> Self {
        Self { manifest }
    }

    fn interval_secs(&self) -> u64 {
        self.manifest
            .get_u64("interval_secs")
            .unwrap_or(DEFAULT_INTERVAL_SECS)
    }

    fn goal_title(&self) -> String {
        self.manifest
            .get_str("goal_title")
            .unwrap_or(DEFAULT_GOAL_TITLE)
            .to_string()
    }
}

impl TriggerSource for ScheduleTriggerSource {
    fn trigger_type(&self) -> &str {
        "schedule"
    }

    fn poll(&self, since: Option<&str>) -> Result<Vec<TriggerEvent>, TriggerError> {
        let interval_secs = self.interval_secs();
        let now = Utc::now();

        let due = match since {
            None => true,
            Some(watermark) => {
                let last = DateTime::parse_from_rfc3339(watermark)
                    .map_err(|e| TriggerError::PollFailed {
                        trigger_type: "schedule".into(),
                        reason: format!("watermark '{watermark}' is not RFC 3339: {e}"),
                    })?
                    .with_timezone(&Utc);
                (now - last).num_seconds() >= interval_secs as i64
            }
        };

        if !due {
            return Ok(vec![]);
        }

        Ok(vec![TriggerEvent {
            id: Uuid::new_v4(),
            trigger_type: "schedule".into(),
            source: "schedule".into(),
            occurred_at: now,
            payload: json!({
                "interval_secs": interval_secs,
                "fired_at": now.to_rfc3339(),
            }),
            suggested_goal_title: self.goal_title(),
            dedupe_key: Some(format!("schedule:{}", now.to_rfc3339())),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Dispatch;
    use std::collections::HashMap;

    fn manifest(settings: HashMap<String, toml::Value>) -> TriggerManifest {
        TriggerManifest {
            trigger_type: "schedule".into(),
            enabled: true,
            dispatch: Dispatch::Direct,
            description: None,
            settings,
        }
    }

    #[test]
    fn fires_on_first_poll_with_no_watermark() {
        let source = ScheduleTriggerSource::new(manifest(HashMap::new()));
        let events = source.poll(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].trigger_type, "schedule");
        assert_eq!(events[0].suggested_goal_title, DEFAULT_GOAL_TITLE);
        assert!(events[0].dedupe_key.is_some());
    }

    #[test]
    fn does_not_fire_before_interval_elapsed() {
        let mut settings = HashMap::new();
        settings.insert("interval_secs".into(), toml::Value::Integer(3600));
        let source = ScheduleTriggerSource::new(manifest(settings));
        let recent = Utc::now().to_rfc3339();
        let events = source.poll(Some(&recent)).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn fires_after_interval_elapsed() {
        let mut settings = HashMap::new();
        settings.insert("interval_secs".into(), toml::Value::Integer(60));
        let source = ScheduleTriggerSource::new(manifest(settings));
        let long_ago = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
        let events = source.poll(Some(&long_ago)).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn custom_goal_title_is_used() {
        let mut settings = HashMap::new();
        settings.insert(
            "goal_title".into(),
            toml::Value::String("Nightly health check".into()),
        );
        let source = ScheduleTriggerSource::new(manifest(settings));
        let events = source.poll(None).unwrap();
        assert_eq!(events[0].suggested_goal_title, "Nightly health check");
    }

    #[test]
    fn malformed_watermark_is_reported() {
        let source = ScheduleTriggerSource::new(manifest(HashMap::new()));
        let result = source.poll(Some("not-a-timestamp"));
        assert!(matches!(result, Err(TriggerError::PollFailed { .. })));
    }
}
