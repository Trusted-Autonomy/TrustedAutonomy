// dispatcher.rs -- Subscription dispatcher: match events to subscriptions and
// determine which actions to fire.
//
// The dispatcher is stateless from the caller's perspective: it reads the
// subscription list from the store, evaluates the envelope against each
// active subscription, and returns a list of `DispatchRecord`s. Updating
// cursors and executing actions are the caller's responsibility — keeping
// them separate makes the dispatcher easily testable.
//
// Replay: `dispatch_replay` evaluates all events in the store that are
// strictly after each subscription's cursor, so subscriptions resume from
// exactly where they left off after a daemon restart.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::EventError;
use crate::schema::EventEnvelope;
use crate::store::{EventQueryFilter, EventStore};
use crate::subscription::{SubscriptionAction, SubscriptionStore};

// ---------------------------------------------------------------------------
// DispatchRecord
// ---------------------------------------------------------------------------

/// Describes a single event→subscription match ready for execution.
///
/// The caller receives a list of these and decides how to act on each one
/// (execute the workflow, fire the webhook, etc.).
#[derive(Debug, Clone)]
pub struct DispatchRecord {
    /// Subscription that matched.
    pub subscription_id: Uuid,
    /// Human-readable subscription name.
    pub subscription_name: String,
    /// The action that should be executed.
    pub action: SubscriptionAction,
    /// ID of the matching event envelope.
    pub event_id: Uuid,
    /// Type string of the matching event.
    pub event_type: String,
    /// Timestamp of the matching event (use this to update the cursor).
    pub event_timestamp: DateTime<Utc>,
    /// Full envelope, available for webhook payloads or template rendering.
    pub envelope: EventEnvelope,
}

// ---------------------------------------------------------------------------
// SubscriptionDispatcher
// ---------------------------------------------------------------------------

/// Evaluates subscriptions against events and returns dispatch records.
pub struct SubscriptionDispatcher {
    store: SubscriptionStore,
}

impl SubscriptionDispatcher {
    /// Create a dispatcher backed by the given subscription store.
    pub fn new(store: SubscriptionStore) -> Self {
        Self { store }
    }

    /// Evaluate a single event envelope against all active subscriptions.
    ///
    /// Returns one `DispatchRecord` per matching subscription. Does NOT
    /// update cursors — callers should do that after successful execution.
    pub fn dispatch(&self, envelope: &EventEnvelope) -> Result<Vec<DispatchRecord>, EventError> {
        let subs = self.store.load()?;
        let records: Vec<DispatchRecord> = subs
            .into_iter()
            .filter(|s| s.matches(envelope))
            .map(|s| DispatchRecord {
                subscription_id: s.id,
                subscription_name: s.name,
                action: s.action,
                event_id: envelope.id,
                event_type: envelope.event_type.clone(),
                event_timestamp: envelope.timestamp,
                envelope: envelope.clone(),
            })
            .collect();
        Ok(records)
    }

    /// Replay all events from the event store that each subscription has not
    /// yet seen (i.e., events with timestamp > subscription.cursor).
    ///
    /// Returns a flat list of dispatch records, grouped by subscription then
    /// by event (chronological within each subscription).
    pub fn dispatch_replay(
        &self,
        event_store: &dyn EventStore,
    ) -> Result<Vec<DispatchRecord>, EventError> {
        let subs = self.store.load()?;
        let mut all_records: Vec<DispatchRecord> = Vec::new();

        for sub in &subs {
            if !sub.enabled {
                continue;
            }

            let filter = EventQueryFilter {
                since: sub.cursor,
                ..Default::default()
            };

            let events = event_store.query(&filter)?;
            for envelope in &events {
                if sub.matches(envelope) {
                    all_records.push(DispatchRecord {
                        subscription_id: sub.id,
                        subscription_name: sub.name.clone(),
                        action: sub.action.clone(),
                        event_id: envelope.id,
                        event_type: envelope.event_type.clone(),
                        event_timestamp: envelope.timestamp,
                        envelope: envelope.clone(),
                    });
                }
            }
        }

        Ok(all_records)
    }

    /// Advance the cursor for a subscription after a successful dispatch.
    pub fn advance_cursor(
        &self,
        subscription_id: Uuid,
        timestamp: DateTime<Utc>,
    ) -> Result<(), EventError> {
        self.store.update_cursor(subscription_id, timestamp)
    }

    /// Return a reference to the underlying subscription store.
    pub fn subscription_store(&self) -> &SubscriptionStore {
        &self.store
    }
}

/// Build a human-readable summary of dispatch records for CLI or log output.
pub fn format_dispatch_summary(records: &[DispatchRecord]) -> String {
    if records.is_empty() {
        return "No matching subscriptions.".to_string();
    }
    let mut lines = Vec::new();
    for r in records {
        lines.push(format!(
            "  [{}] {} -> {}",
            r.event_type,
            r.subscription_name,
            r.action.describe()
        ));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SessionEvent;
    use crate::store::FsEventStore;
    use crate::subscription::{Subscription, SubscriptionFilter};
    use tempfile::tempdir;

    fn goal_started_envelope(goal_id: Uuid) -> EventEnvelope {
        EventEnvelope::new(SessionEvent::GoalStarted {
            goal_id,
            title: "test goal".into(),
            agent_id: "agent-1".into(),
            phase: None,
        })
    }

    fn memory_stored_envelope() -> EventEnvelope {
        EventEnvelope::new(SessionEvent::MemoryStored {
            key: "k".into(),
            category: None,
            source: "cli".into(),
        })
    }

    #[test]
    fn dispatch_all_filter_matches_any_event() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new(
            "watch-all",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        store.add(sub).unwrap();

        let dispatcher = SubscriptionDispatcher::new(SubscriptionStore::new(dir.path()));
        let records = dispatcher
            .dispatch(&goal_started_envelope(Uuid::new_v4()))
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].subscription_name, "watch-all");
    }

    #[test]
    fn dispatch_type_filter_excludes_non_matching() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new(
            "watch-goals",
            SubscriptionFilter::ByTypes {
                types: vec!["goal_started".into()],
            },
            SubscriptionAction::Log,
        );
        store.add(sub).unwrap();

        let dispatcher = SubscriptionDispatcher::new(SubscriptionStore::new(dir.path()));

        // Matching event.
        let records = dispatcher
            .dispatch(&goal_started_envelope(Uuid::new_v4()))
            .unwrap();
        assert_eq!(records.len(), 1);

        // Non-matching event.
        let records = dispatcher.dispatch(&memory_stored_envelope()).unwrap();
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn dispatch_disabled_subscription_skipped() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let mut sub =
            Subscription::new("disabled", SubscriptionFilter::All, SubscriptionAction::Log);
        sub.enabled = false;
        store.add(sub).unwrap();

        let dispatcher = SubscriptionDispatcher::new(SubscriptionStore::new(dir.path()));
        let records = dispatcher
            .dispatch(&goal_started_envelope(Uuid::new_v4()))
            .unwrap();

        assert_eq!(records.len(), 0);
    }

    #[test]
    fn dispatch_multiple_subscriptions() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        store
            .add(Subscription::new(
                "sub-a",
                SubscriptionFilter::All,
                SubscriptionAction::Log,
            ))
            .unwrap();
        store
            .add(Subscription::new(
                "sub-b",
                SubscriptionFilter::ByTypes {
                    types: vec!["goal_started".into()],
                },
                SubscriptionAction::Log,
            ))
            .unwrap();

        let dispatcher = SubscriptionDispatcher::new(SubscriptionStore::new(dir.path()));
        let records = dispatcher
            .dispatch(&goal_started_envelope(Uuid::new_v4()))
            .unwrap();

        assert_eq!(records.len(), 2);
    }

    #[test]
    fn dispatch_record_contains_correct_fields() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new(
            "check-fields",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        let sub_id = sub.id;
        store.add(sub).unwrap();

        let envelope = goal_started_envelope(Uuid::new_v4());
        let env_id = envelope.id;

        let dispatcher = SubscriptionDispatcher::new(SubscriptionStore::new(dir.path()));
        let records = dispatcher.dispatch(&envelope).unwrap();

        assert_eq!(records[0].subscription_id, sub_id);
        assert_eq!(records[0].event_id, env_id);
        assert_eq!(records[0].event_type, "goal_started");
    }

    #[test]
    fn dispatch_replay_from_cursor() {
        let dir = tempdir().unwrap();
        let sub_store = SubscriptionStore::new(dir.path().join(".ta"));
        let ev_store = FsEventStore::new(dir.path().join(".ta").join("events"));

        // Create subscription without cursor.
        let mut sub = Subscription::new(
            "replay-test",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        let sub_id = sub.id;

        // Store an "already seen" event in the event store and set it as cursor.
        let past_envelope = memory_stored_envelope();
        ev_store.append(&past_envelope).unwrap();
        sub.cursor = Some(past_envelope.timestamp);

        sub_store.add(sub).unwrap();

        // Store a newer event.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let new_envelope = goal_started_envelope(Uuid::new_v4());
        ev_store.append(&new_envelope).unwrap();
        let new_id = new_envelope.id;

        let dispatcher =
            SubscriptionDispatcher::new(SubscriptionStore::new(dir.path().join(".ta")));
        let records = dispatcher.dispatch_replay(&ev_store).unwrap();

        // Only the newer event should be replayed.
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event_id, new_id);
        assert_eq!(records[0].subscription_name, "replay-test");

        // Advance cursor.
        dispatcher
            .advance_cursor(sub_id, records[0].event_timestamp)
            .unwrap();

        // Replay again — nothing new.
        let records2 = dispatcher.dispatch_replay(&ev_store).unwrap();
        assert_eq!(records2.len(), 0);
    }

    #[test]
    fn dispatch_replay_no_cursor_processes_all() {
        let dir = tempdir().unwrap();
        let sub_store = SubscriptionStore::new(dir.path().join(".ta"));
        let ev_store = FsEventStore::new(dir.path().join(".ta").join("events"));

        let sub = Subscription::new(
            "no-cursor",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        sub_store.add(sub).unwrap();

        ev_store.append(&memory_stored_envelope()).unwrap();
        ev_store.append(&memory_stored_envelope()).unwrap();

        let dispatcher =
            SubscriptionDispatcher::new(SubscriptionStore::new(dir.path().join(".ta")));
        let records = dispatcher.dispatch_replay(&ev_store).unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn format_dispatch_summary_empty() {
        assert_eq!(format_dispatch_summary(&[]), "No matching subscriptions.");
    }

    #[test]
    fn format_dispatch_summary_has_records() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());
        let sub = Subscription::new("s1", SubscriptionFilter::All, SubscriptionAction::Log);
        store.add(sub).unwrap();

        let dispatcher = SubscriptionDispatcher::new(SubscriptionStore::new(dir.path()));
        let records = dispatcher
            .dispatch(&goal_started_envelope(Uuid::new_v4()))
            .unwrap();
        let summary = format_dispatch_summary(&records);
        assert!(summary.contains("goal_started"));
        assert!(summary.contains("s1"));
        assert!(summary.contains("log"));
    }
}
