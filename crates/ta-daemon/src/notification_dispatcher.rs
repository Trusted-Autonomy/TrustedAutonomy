// notification_dispatcher.rs — Event-driven notification dispatch.
//
// Bridges the Notification Rules Engine (ta-events::notification) with the
// ChannelDispatcher to send one-way notifications when lifecycle events fire.
//
// Call `dispatch_event()` from the daemon's event-handling path for every
// EventEnvelope that should be evaluated against notification rules.
//
// Relationship to ChannelDispatcher:
//   - ChannelDispatcher handles interactive questions (deliver_question).
//   - NotificationDispatcher handles one-way event notifications (deliver_notification).
//   - Both share the same pool of ChannelDelivery adapters, accessed through
//     a shared Arc<ChannelDispatcher>.

use std::path::Path;
use std::sync::Arc;

use ta_events::channel::{ChannelNotification, DeliveryResult};
use ta_events::notification::{
    NotificationRulesEngine, NotificationSeverity, NotificationTemplate,
};
use ta_events::schema::EventEnvelope;

use crate::channel_dispatcher::ChannelDispatcher;

/// Dispatches one-way event notifications to channels based on notification rules.
///
/// # Example
///
/// ```no_run
/// let dispatcher = NotificationDispatcher::load(
///     Path::new(".ta/notification-rules.toml"),
///     channel_dispatcher.clone(),
/// );
/// let results = dispatcher.dispatch_event(&event).await;
/// ```
pub struct NotificationDispatcher {
    engine: NotificationRulesEngine,
    /// Shared channel adapters (same pool as the question dispatcher).
    channel_dispatcher: Arc<ChannelDispatcher>,
}

impl NotificationDispatcher {
    /// Create a dispatcher by loading rules from a TOML file.
    ///
    /// Returns a dispatcher with an empty rule set if the file does not exist —
    /// this is the common case when the user hasn't configured notification rules.
    pub fn load(rules_path: &Path, channel_dispatcher: Arc<ChannelDispatcher>) -> Self {
        let engine = NotificationRulesEngine::load(rules_path);
        Self {
            engine,
            channel_dispatcher,
        }
    }

    /// Create a dispatcher from an already-loaded engine.
    pub fn new(
        engine: NotificationRulesEngine,
        channel_dispatcher: Arc<ChannelDispatcher>,
    ) -> Self {
        Self {
            engine,
            channel_dispatcher,
        }
    }

    /// Evaluate the event against all notification rules and dispatch to matching channels.
    ///
    /// Returns the aggregated delivery results (one entry per channel per matched rule).
    /// A single event can trigger multiple rules.
    pub async fn dispatch_event(&self, event: &EventEnvelope) -> Vec<DeliveryResult> {
        let matched = self.engine.matching_rules(event);
        if matched.is_empty() {
            tracing::trace!(
                event_type = %event.event_type,
                "No notification rules matched event"
            );
            return vec![];
        }

        let vars = NotificationRulesEngine::build_template_vars(event);
        let severity = NotificationSeverity::for_event_type(&event.event_type);
        let goal_id = extract_goal_id(event);

        let mut all_results = Vec::new();

        for rule in matched {
            if !self.engine.check_and_record(rule, event) {
                continue;
            }

            let channels = self.engine.resolve_channels(rule);
            if channels.is_empty() {
                tracing::debug!(
                    rule_id = %rule.id,
                    event_type = %event.event_type,
                    "Rule matched but no channels configured; skipping delivery"
                );
                continue;
            }

            let template = rule
                .template
                .as_ref()
                .cloned()
                .unwrap_or_else(|| default_template(event));

            let notification = ChannelNotification {
                event_id: event.id.to_string(),
                event_type: event.event_type.clone(),
                title: template.render_title(&vars),
                body: template.render_body(&vars),
                severity,
                goal_id,
                metadata: vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            };

            tracing::info!(
                rule_id = %rule.id,
                event_type = %event.event_type,
                severity = %severity.as_str(),
                channels = ?channels,
                "Dispatching notification"
            );

            let results = self
                .channel_dispatcher
                .dispatch_notification(&notification, channels)
                .await;

            for result in &results {
                if result.success {
                    tracing::info!(
                        channel = %result.channel,
                        rule_id = %rule.id,
                        event_type = %event.event_type,
                        delivery_id = %result.delivery_id,
                        "Notification delivered"
                    );
                } else {
                    tracing::warn!(
                        channel = %result.channel,
                        rule_id = %rule.id,
                        event_type = %event.event_type,
                        error = ?result.error,
                        "Notification delivery failed"
                    );
                }
            }

            all_results.extend(results);
        }

        all_results
    }

    /// Number of rules loaded into the engine.
    pub fn rule_count(&self) -> usize {
        self.engine.rule_count()
    }
}

/// Build a default template when a rule has none configured.
fn default_template(event: &EventEnvelope) -> NotificationTemplate {
    let (title, body) = match event.event_type.as_str() {
        "goal_failed" => (
            "[TA] Goal failed".into(),
            "Goal `{title}` failed. Check `ta goal status {goal_id}` for details.".into(),
        ),
        "goal_completed" => (
            "[TA] Goal completed".into(),
            "Goal `{title}` completed successfully.".into(),
        ),
        "policy_violation" => (
            "[TA] Policy violation".into(),
            "A policy violation was detected. Review with `ta draft view`.".into(),
        ),
        "build_failed" => (
            "[TA] Build failed".into(),
            "Build failed for goal `{goal_id}`. Check the agent output for details.".into(),
        ),
        "draft_denied" => (
            "[TA] Draft denied".into(),
            "A draft was denied. Use `ta draft list` to see pending drafts.".into(),
        ),
        "agent_needs_input" => (
            "[TA] Agent waiting for input".into(),
            "An agent is waiting for your response. Check `ta shell` or the TA Studio.".into(),
        ),
        "graph_decision_escalated" => (
            "[TA] Review panel escalated a decision".into(),
            "A workflow graph decision did not clear its threshold and was escalated for \
             human review. Run `ta workflow graph-run` output or check the draft directly."
                .into(),
        ),
        _ => (
            "[TA] {event_type}".into(),
            "Event `{event_type}` at {timestamp}.".into(),
        ),
    };
    NotificationTemplate { title, body }
}

/// Extract goal_id from the event payload's JSON representation, if present.
fn extract_goal_id(event: &EventEnvelope) -> Option<uuid::Uuid> {
    let json = serde_json::to_value(&event.payload).ok()?;
    let gid_str = json.get("goal_id")?.as_str()?;
    gid_str.parse().ok()
}

// ─── ChannelDispatcher extension ─────────────────────────────────────────────
// The dispatch_notification method is added to ChannelDispatcher in
// channel_dispatcher.rs. This module calls it through the shared Arc.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ta_events::channel::{ChannelDelivery, ChannelQuestion};
    use ta_events::notification::{NotificationRule, NotificationRulesConfig, RuleCondition};
    use ta_events::schema::{EventEnvelope, SessionEvent};
    use uuid::Uuid;

    /// A mock channel adapter that records notifications.
    struct MockAdapter {
        name: String,
    }

    #[async_trait::async_trait]
    impl ChannelDelivery for MockAdapter {
        fn name(&self) -> &str {
            &self.name
        }

        async fn deliver_question(&self, question: &ChannelQuestion) -> DeliveryResult {
            DeliveryResult {
                channel: self.name.clone(),
                delivery_id: format!("q-{}", question.interaction_id),
                success: true,
                error: None,
            }
        }

        async fn deliver_notification(&self, notification: &ChannelNotification) -> DeliveryResult {
            DeliveryResult {
                channel: self.name.clone(),
                delivery_id: format!("n-{}", notification.event_id),
                success: true,
                error: None,
            }
        }

        async fn validate(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn make_dispatcher_with_mock() -> (NotificationDispatcher, ()) {
        let config = NotificationRulesConfig {
            rules: vec![NotificationRule {
                id: "on-failure".into(),
                name: "Alert on failure".into(),
                enabled: true,
                priority: 10,
                conditions: vec![RuleCondition::EventType {
                    value: "goal_failed".into(),
                }],
                channels: vec!["mock".into()],
                template: None,
                rate_limit: None,
            }],
            suppress_duplicates_secs: None,
            global_channels: vec![],
        };
        let engine = NotificationRulesEngine::new(config);
        let mut cd = ChannelDispatcher::new(vec![]);
        cd.register(Arc::new(MockAdapter {
            name: "mock".into(),
        }));
        let dispatcher = NotificationDispatcher::new(engine, Arc::new(cd));
        (dispatcher, ())
    }

    fn goal_failed_event() -> EventEnvelope {
        EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "exit code 1".into(),
            exit_code: Some(1),
        })
    }

    fn goal_completed_event() -> EventEnvelope {
        EventEnvelope::new(SessionEvent::GoalCompleted {
            goal_id: Uuid::new_v4(),
            title: "Fix bug".into(),
            duration_secs: Some(30),
        })
    }

    #[tokio::test]
    async fn dispatches_matching_rule() {
        let (dispatcher, _) = make_dispatcher_with_mock();
        let results = dispatcher.dispatch_event(&goal_failed_event()).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].channel, "mock");
        assert!(results[0].delivery_id.starts_with("n-"));
    }

    #[tokio::test]
    async fn no_dispatch_for_non_matching_event() {
        let (dispatcher, _) = make_dispatcher_with_mock();
        let results = dispatcher.dispatch_event(&goal_completed_event()).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn dedup_suppresses_second_identical_event() {
        let config = NotificationRulesConfig {
            rules: vec![NotificationRule {
                id: "dedup-rule".into(),
                name: "dedup".into(),
                enabled: true,
                priority: 10,
                conditions: vec![RuleCondition::EventType {
                    value: "goal_failed".into(),
                }],
                channels: vec!["mock".into()],
                template: None,
                rate_limit: None,
            }],
            suppress_duplicates_secs: Some(60),
            global_channels: vec![],
        };
        let engine = NotificationRulesEngine::new(config);
        let mut cd = ChannelDispatcher::new(vec![]);
        cd.register(Arc::new(MockAdapter {
            name: "mock".into(),
        }));
        let dispatcher = NotificationDispatcher::new(engine, Arc::new(cd));

        let event = goal_failed_event();
        let r1 = dispatcher.dispatch_event(&event).await;
        let r2 = dispatcher.dispatch_event(&event).await;

        assert_eq!(r1.len(), 1, "first delivery allowed");
        assert!(r2.is_empty(), "second suppressed by dedup");
    }

    #[test]
    fn rule_count_reflects_loaded_rules() {
        let (dispatcher, _) = make_dispatcher_with_mock();
        assert_eq!(dispatcher.rule_count(), 1);
    }

    #[test]
    fn default_template_goal_failed() {
        let event = goal_failed_event();
        let tmpl = default_template(&event);
        assert!(tmpl.title.contains("failed"));
    }

    #[test]
    fn default_template_unknown_event() {
        let event = EventEnvelope::new(SessionEvent::HealthCheck {
            goals_checked: 0,
            issues: vec![],
        });
        let tmpl = default_template(&event);
        assert!(tmpl.title.contains("{event_type}"));
    }

    #[test]
    fn extract_goal_id_from_goal_failed() {
        let gid = Uuid::new_v4();
        let event = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: gid,
            error: "e".into(),
            exit_code: None,
        });
        assert_eq!(extract_goal_id(&event), Some(gid));
    }

    #[test]
    fn extract_goal_id_from_health_check_is_none() {
        let event = EventEnvelope::new(SessionEvent::HealthCheck {
            goals_checked: 0,
            issues: vec![],
        });
        assert!(extract_goal_id(&event).is_none());
    }
}
