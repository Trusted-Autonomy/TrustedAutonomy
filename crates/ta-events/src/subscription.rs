// subscription.rs -- Persistent named subscriptions to event streams.
//
// A Subscription is a named, durable binding between an event filter and an
// action. Unlike the in-process EventFilter (which is ephemeral), subscriptions
// survive daemon restarts and resume from a cursor so no events are missed.
//
// Storage: `.ta/subscriptions.json` (one JSON array, written atomically).
// Cursor: each subscription tracks the timestamp of the last event it processed
// so replay can pick up exactly where it left off after a restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::EventError;
use crate::schema::EventEnvelope;

// ---------------------------------------------------------------------------
// SubscriptionFilter
// ---------------------------------------------------------------------------

/// Criteria for matching events to a subscription.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubscriptionFilter {
    /// Match every event.
    All,
    /// Match events whose type is in the list (at least one must match).
    ByTypes { types: Vec<String> },
    /// Match events associated with a specific goal.
    ByGoal { goal_id: Uuid },
    /// Match events associated with a specific plan phase.
    ByPhase { phase: String },
    /// Require ALL inner filters to match (logical AND).
    And { filters: Vec<SubscriptionFilter> },
}

impl SubscriptionFilter {
    /// Return true if the envelope matches this filter.
    pub fn matches(&self, envelope: &EventEnvelope) -> bool {
        match self {
            Self::All => true,
            Self::ByTypes { types } => types.iter().any(|t| t == &envelope.event_type),
            Self::ByGoal { goal_id } => envelope.payload.goal_id() == Some(*goal_id),
            Self::ByPhase { phase } => envelope.payload.phase() == Some(phase.as_str()),
            Self::And { filters } => filters.iter().all(|f| f.matches(envelope)),
        }
    }
}

// ---------------------------------------------------------------------------
// SubscriptionAction
// ---------------------------------------------------------------------------

/// What to do when a matching event arrives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubscriptionAction {
    /// Write a log entry to `.ta/subscription-dispatch.log`.
    Log,
    /// Start the named workflow, passing the event as a variable map.
    RunWorkflow {
        workflow: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        params: HashMap<String, String>,
    },
    /// Deliver the event to the listed channels (Slack, email, …).
    Notify {
        channels: Vec<String>,
        /// Optional Handlebars-style template (`{{event_type}} fired`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        template: Option<String>,
    },
    /// POST the event envelope as JSON to an HTTP endpoint.
    Webhook {
        url: String,
        /// Extra request headers (e.g. `Authorization`).
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        headers: HashMap<String, String>,
        /// HMAC-SHA256 secret for request signing (`X-TA-Signature`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
    },
}

impl SubscriptionAction {
    /// Short human-readable description for CLI display.
    pub fn describe(&self) -> String {
        match self {
            Self::Log => "log".to_string(),
            Self::RunWorkflow { workflow, .. } => format!("workflow:{}", workflow),
            Self::Notify { channels, .. } => format!("notify:{}", channels.join(",")),
            Self::Webhook { url, .. } => {
                let preview: String = url.chars().take(40).collect();
                format!("webhook:{}", preview)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Subscription
// ---------------------------------------------------------------------------

/// A named, persistent subscription binding an event filter to an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    /// Stable identifier (used in `ta events subscriptions remove <id>`).
    pub id: Uuid,
    /// Human-readable name (must be unique within the project).
    pub name: String,
    /// Description of what this subscription does (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Event matching criteria.
    pub filter: SubscriptionFilter,
    /// What to do when a matching event arrives.
    pub action: SubscriptionAction,
    /// Timestamp of the last event this subscription has processed.
    ///
    /// On the next dispatch pass, only events strictly after this cursor
    /// are delivered. `None` means the subscription has never dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<DateTime<Utc>>,
    /// When this subscription was created.
    pub created_at: DateTime<Utc>,
    /// Whether the subscription is active. Disabled subscriptions are stored
    /// but skipped during dispatch.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Subscription {
    /// Create a new enabled subscription.
    pub fn new(
        name: impl Into<String>,
        filter: SubscriptionFilter,
        action: SubscriptionAction,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            description: None,
            filter,
            action,
            cursor: None,
            created_at: Utc::now(),
            enabled: true,
        }
    }

    /// Return true if this subscription matches the envelope and is enabled.
    pub fn matches(&self, envelope: &EventEnvelope) -> bool {
        self.enabled && self.filter.matches(envelope)
    }
}

// ---------------------------------------------------------------------------
// SubscriptionStore
// ---------------------------------------------------------------------------

/// Persistent store for named subscriptions, backed by `.ta/subscriptions.json`.
pub struct SubscriptionStore {
    path: PathBuf,
}

impl SubscriptionStore {
    /// Create a store rooted at the given `.ta` directory.
    pub fn new(ta_dir: impl AsRef<Path>) -> Self {
        Self {
            path: ta_dir.as_ref().join("subscriptions.json"),
        }
    }

    /// Load all subscriptions from disk. Returns an empty list if the file
    /// does not exist.
    pub fn load(&self) -> Result<Vec<Subscription>, EventError> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(&self.path)?;
        let subs: Vec<Subscription> = serde_json::from_str(&content)?;
        Ok(subs)
    }

    /// Persist the entire subscription list atomically (write to temp, rename).
    pub fn save(&self, subs: &[Subscription]) -> Result<(), EventError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(subs)?;
        // Atomic write via temp file in the same directory.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Add a new subscription. Returns an error if a subscription with the
    /// same name already exists.
    pub fn add(&self, sub: Subscription) -> Result<(), EventError> {
        let mut subs = self.load()?;
        if subs.iter().any(|s| s.name == sub.name) {
            return Err(EventError::SubscriptionAlreadyExists(sub.name));
        }
        subs.push(sub);
        self.save(&subs)
    }

    /// Remove a subscription by ID. Returns `true` if it was found and removed.
    pub fn remove(&self, id: Uuid) -> Result<bool, EventError> {
        let mut subs = self.load()?;
        let before = subs.len();
        subs.retain(|s| s.id != id);
        let removed = subs.len() < before;
        if removed {
            self.save(&subs)?;
        }
        Ok(removed)
    }

    /// Retrieve a subscription by ID.
    pub fn get(&self, id: Uuid) -> Result<Option<Subscription>, EventError> {
        Ok(self.load()?.into_iter().find(|s| s.id == id))
    }

    /// Retrieve a subscription by name.
    pub fn get_by_name(&self, name: &str) -> Result<Option<Subscription>, EventError> {
        Ok(self.load()?.into_iter().find(|s| s.name == name))
    }

    /// Update the cursor for a subscription (called after successful dispatch).
    pub fn update_cursor(&self, id: Uuid, cursor: DateTime<Utc>) -> Result<(), EventError> {
        let mut subs = self.load()?;
        if let Some(s) = subs.iter_mut().find(|s| s.id == id) {
            s.cursor = Some(cursor);
            self.save(&subs)?;
            Ok(())
        } else {
            Err(EventError::SubscriptionNotFound(id))
        }
    }

    /// Enable or disable a subscription.
    pub fn set_enabled(&self, id: Uuid, enabled: bool) -> Result<(), EventError> {
        let mut subs = self.load()?;
        if let Some(s) = subs.iter_mut().find(|s| s.id == id) {
            s.enabled = enabled;
            self.save(&subs)?;
            Ok(())
        } else {
            Err(EventError::SubscriptionNotFound(id))
        }
    }

    /// Return all subscriptions.
    pub fn list(&self) -> Result<Vec<Subscription>, EventError> {
        self.load()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SessionEvent;
    use tempfile::tempdir;

    fn make_envelope(event_type: &str) -> EventEnvelope {
        let event = match event_type {
            "goal_started" => SessionEvent::GoalStarted {
                goal_id: Uuid::new_v4(),
                title: "t".into(),
                agent_id: "a".into(),
                phase: None,
            },
            _ => SessionEvent::MemoryStored {
                key: "k".into(),
                category: None,
                source: "cli".into(),
            },
        };
        EventEnvelope::new(event)
    }

    #[test]
    fn filter_all_matches() {
        let f = SubscriptionFilter::All;
        assert!(f.matches(&make_envelope("goal_started")));
        assert!(f.matches(&make_envelope("memory_stored")));
    }

    #[test]
    fn filter_by_types_matches() {
        let f = SubscriptionFilter::ByTypes {
            types: vec!["goal_started".into()],
        };
        assert!(f.matches(&make_envelope("goal_started")));
        assert!(!f.matches(&make_envelope("memory_stored")));
    }

    #[test]
    fn filter_by_phase_matches() {
        let event = SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "t".into(),
            agent_id: "a".into(),
            phase: Some("v0.15.19.1".into()),
        };
        let envelope = EventEnvelope::new(event);
        let f = SubscriptionFilter::ByPhase {
            phase: "v0.15.19.1".into(),
        };
        assert!(f.matches(&envelope));
        let f2 = SubscriptionFilter::ByPhase {
            phase: "v0.99.0".into(),
        };
        assert!(!f2.matches(&envelope));
    }

    #[test]
    fn filter_and_requires_all() {
        let goal_id = Uuid::new_v4();
        let event = SessionEvent::GoalStarted {
            goal_id,
            title: "t".into(),
            agent_id: "a".into(),
            phase: None,
        };
        let envelope = EventEnvelope::new(event);

        let f = SubscriptionFilter::And {
            filters: vec![
                SubscriptionFilter::ByTypes {
                    types: vec!["goal_started".into()],
                },
                SubscriptionFilter::ByGoal { goal_id },
            ],
        };
        assert!(f.matches(&envelope));

        // Wrong goal ID — And should fail.
        let f2 = SubscriptionFilter::And {
            filters: vec![
                SubscriptionFilter::ByTypes {
                    types: vec!["goal_started".into()],
                },
                SubscriptionFilter::ByGoal {
                    goal_id: Uuid::new_v4(),
                },
            ],
        };
        assert!(!f2.matches(&envelope));
    }

    #[test]
    fn disabled_subscription_does_not_match() {
        let mut sub = Subscription::new(
            "disabled-test",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        sub.enabled = false;
        assert!(!sub.matches(&make_envelope("goal_started")));
    }

    #[test]
    fn store_add_and_list() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        assert!(store.list().unwrap().is_empty());

        let sub = Subscription::new(
            "watch-goals",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        store.add(sub.clone()).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "watch-goals");
    }

    #[test]
    fn store_duplicate_name_rejected() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub1 = Subscription::new("dup", SubscriptionFilter::All, SubscriptionAction::Log);
        let sub2 = Subscription::new("dup", SubscriptionFilter::All, SubscriptionAction::Log);

        store.add(sub1).unwrap();
        assert!(store.add(sub2).is_err());
    }

    #[test]
    fn store_remove() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new(
            "to-remove",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        let id = sub.id;
        store.add(sub).unwrap();

        assert_eq!(store.list().unwrap().len(), 1);
        let removed = store.remove(id).unwrap();
        assert!(removed);
        assert!(store.list().unwrap().is_empty());

        // Removing again returns false.
        let removed_again = store.remove(id).unwrap();
        assert!(!removed_again);
    }

    #[test]
    fn store_update_cursor() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new(
            "cursor-test",
            SubscriptionFilter::All,
            SubscriptionAction::Log,
        );
        let id = sub.id;
        store.add(sub).unwrap();

        let ts = Utc::now();
        store.update_cursor(id, ts).unwrap();

        let loaded = store.get(id).unwrap().unwrap();
        assert_eq!(loaded.cursor.unwrap(), ts);
    }

    #[test]
    fn store_get_by_name() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new("find-me", SubscriptionFilter::All, SubscriptionAction::Log);
        store.add(sub).unwrap();

        let found = store.get_by_name("find-me").unwrap();
        assert!(found.is_some());

        let not_found = store.get_by_name("missing").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn store_set_enabled() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new("toggle", SubscriptionFilter::All, SubscriptionAction::Log);
        let id = sub.id;
        store.add(sub).unwrap();

        store.set_enabled(id, false).unwrap();
        let loaded = store.get(id).unwrap().unwrap();
        assert!(!loaded.enabled);

        store.set_enabled(id, true).unwrap();
        let loaded = store.get(id).unwrap().unwrap();
        assert!(loaded.enabled);
    }

    #[test]
    fn action_describe() {
        assert_eq!(SubscriptionAction::Log.describe(), "log");
        assert_eq!(
            SubscriptionAction::RunWorkflow {
                workflow: "ci-fix".into(),
                params: HashMap::new(),
            }
            .describe(),
            "workflow:ci-fix"
        );
        assert_eq!(
            SubscriptionAction::Notify {
                channels: vec!["slack".into(), "email".into()],
                template: None,
            }
            .describe(),
            "notify:slack,email"
        );
    }

    #[test]
    fn store_persists_across_reload() {
        let dir = tempdir().unwrap();
        let store = SubscriptionStore::new(dir.path());

        let sub = Subscription::new(
            "persist-test",
            SubscriptionFilter::ByTypes {
                types: vec!["goal_started".into()],
            },
            SubscriptionAction::Notify {
                channels: vec!["slack".into()],
                template: None,
            },
        );
        store.add(sub).unwrap();

        // Re-open the store from same path.
        let store2 = SubscriptionStore::new(dir.path());
        let list = store2.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "persist-test");
    }
}
