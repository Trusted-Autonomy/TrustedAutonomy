// bus.rs -- In-process event distribution using tokio broadcast channels.
//
// The EventBus is the central hub: publishers call `publish(event)`,
// subscribers call `subscribe(filter)` to get a filtered receiver.

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::EventError;
use crate::schema::{EventEnvelope, SessionEvent};

/// Filter for event subscriptions.
#[derive(Debug, Clone)]
pub enum EventFilter {
    /// Receive all events.
    All,
    /// Receive only events matching these type names.
    ByType(Vec<String>),
    /// Receive only events associated with this goal.
    ByGoal(Uuid),
    /// Receive only events associated with this plan phase.
    ByPhase(String),
}

impl EventFilter {
    /// Check whether an envelope matches this filter.
    pub fn matches(&self, envelope: &EventEnvelope) -> bool {
        match self {
            Self::All => true,
            Self::ByType(types) => types.iter().any(|t| t == &envelope.event_type),
            Self::ByGoal(goal_id) => envelope.payload.goal_id() == Some(*goal_id),
            Self::ByPhase(phase) => envelope.payload.phase() == Some(phase.as_str()),
        }
    }
}

/// In-process event bus backed by tokio broadcast.
pub struct EventBus {
    sender: broadcast::Sender<EventEnvelope>,
}

impl EventBus {
    /// Create a new event bus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Publish an event to all subscribers.
    pub fn publish(&self, event: SessionEvent) -> Result<EventEnvelope, EventError> {
        let envelope = EventEnvelope::new(event);
        // Ignore send errors (no active receivers is fine).
        let _ = self.sender.send(envelope.clone());
        Ok(envelope)
    }

    /// Subscribe to events matching the given filter.
    ///
    /// Returns a `FilteredReceiver` that only yields matching events.
    pub fn subscribe(&self, filter: EventFilter) -> FilteredReceiver {
        FilteredReceiver {
            receiver: self.sender.subscribe(),
            filter,
        }
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}

/// A receiver that filters events based on a subscription filter.
pub struct FilteredReceiver {
    receiver: broadcast::Receiver<EventEnvelope>,
    filter: EventFilter,
}

impl FilteredReceiver {
    /// Receive the next matching event. Blocks until one arrives.
    pub async fn recv(&mut self) -> Result<EventEnvelope, EventError> {
        loop {
            match self.receiver.recv().await {
                Ok(envelope) => {
                    if self.filter.matches(&envelope) {
                        return Ok(envelope);
                    }
                    // Skip non-matching events.
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("event subscriber lagged, missed {} events", n);
                    // Continue receiving.
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(EventError::BusError("event bus closed".into()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_all_matches_everything() {
        let event = SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "test".into(),
            agent_id: "a".into(),
            phase: None,
        };
        let envelope = EventEnvelope::new(event);
        assert!(EventFilter::All.matches(&envelope));
    }

    #[test]
    fn filter_by_type() {
        let event = SessionEvent::DraftApproved {
            goal_id: Uuid::new_v4(),
            draft_id: Uuid::new_v4(),
            approved_by: "h".into(),
        };
        let envelope = EventEnvelope::new(event);
        let filter = EventFilter::ByType(vec!["draft_approved".into()]);
        assert!(filter.matches(&envelope));

        let filter2 = EventFilter::ByType(vec!["goal_started".into()]);
        assert!(!filter2.matches(&envelope));
    }

    #[test]
    fn filter_by_goal() {
        let gid = Uuid::new_v4();
        let event = SessionEvent::DraftBuilt {
            goal_id: gid,
            draft_id: Uuid::new_v4(),
            artifact_count: 3,
        };
        let envelope = EventEnvelope::new(event);
        assert!(EventFilter::ByGoal(gid).matches(&envelope));
        assert!(!EventFilter::ByGoal(Uuid::new_v4()).matches(&envelope));
    }

    #[test]
    fn filter_by_phase() {
        let event = SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "t".into(),
            agent_id: "a".into(),
            phase: Some("v0.8.0".into()),
        };
        let envelope = EventEnvelope::new(event);
        assert!(EventFilter::ByPhase("v0.8.0".into()).matches(&envelope));
        assert!(!EventFilter::ByPhase("v0.9.0".into()).matches(&envelope));
    }

    #[tokio::test]
    async fn bus_publish_and_subscribe() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe(EventFilter::All);

        let event = SessionEvent::MemoryStored {
            key: "test".into(),
            category: None,
            source: "cli".into(),
        };

        let published = bus.publish(event).unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(published.id, received.id);
    }

    #[tokio::test]
    async fn bus_filtered_subscription() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe(EventFilter::ByType(vec!["draft_approved".into()]));

        // Publish a non-matching event first.
        bus.publish(SessionEvent::MemoryStored {
            key: "k".into(),
            category: None,
            source: "cli".into(),
        })
        .unwrap();

        // Publish a matching event.
        let gid = Uuid::new_v4();
        bus.publish(SessionEvent::DraftApproved {
            goal_id: gid,
            draft_id: Uuid::new_v4(),
            approved_by: "h".into(),
        })
        .unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type, "draft_approved");
    }

    #[test]
    fn subscriber_count() {
        let bus = EventBus::new(16);
        assert_eq!(bus.subscriber_count(), 0);

        let _rx1 = bus.subscribe(EventFilter::All);
        assert_eq!(bus.subscriber_count(), 1);

        let _rx2 = bus.subscribe(EventFilter::All);
        assert_eq!(bus.subscriber_count(), 2);
    }
}
