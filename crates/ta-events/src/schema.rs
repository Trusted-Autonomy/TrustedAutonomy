// schema.rs -- Stable SessionEvent schema with versioned event envelope.
//
// This is the public contract for external consumers. Event types are
// tagged for forward compatibility -- new variants can be added without
// breaking existing consumers who ignore unknown types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current schema version. Bumped when backward-incompatible changes are made.
pub const SCHEMA_VERSION: u32 = 1;

/// Wrapper around every event with metadata for persistence and routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Unique event identifier.
    pub id: Uuid,
    /// When the event was created.
    pub timestamp: DateTime<Utc>,
    /// Schema version for backward compatibility.
    pub version: u32,
    /// Stringified event type (e.g., "goal_started").
    pub event_type: String,
    /// The event payload.
    pub payload: SessionEvent,
}

impl EventEnvelope {
    /// Create a new envelope wrapping the given event.
    pub fn new(event: SessionEvent) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            version: SCHEMA_VERSION,
            event_type: event.event_type().to_string(),
            payload: event,
        }
    }
}

/// Stable session event types covering the full TA lifecycle.
///
/// Each variant carries the minimum data needed for external consumers.
/// Uses `#[serde(tag = "type")]` for clean JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A new goal was started.
    GoalStarted {
        goal_id: Uuid,
        title: String,
        agent_id: String,
        phase: Option<String>,
    },

    /// A goal completed successfully.
    GoalCompleted {
        goal_id: Uuid,
        title: String,
        duration_secs: Option<u64>,
    },

    /// A draft package was built from workspace diffs.
    DraftBuilt {
        goal_id: Uuid,
        draft_id: Uuid,
        artifact_count: usize,
    },

    /// A draft was submitted for review.
    DraftSubmitted { goal_id: Uuid, draft_id: Uuid },

    /// A draft was approved.
    DraftApproved {
        goal_id: Uuid,
        draft_id: Uuid,
        approved_by: String,
    },

    /// A draft was denied.
    DraftDenied {
        goal_id: Uuid,
        draft_id: Uuid,
        reason: String,
        denied_by: String,
    },

    /// Approved changes were applied to the source.
    DraftApplied {
        goal_id: Uuid,
        draft_id: Uuid,
        files_count: usize,
    },

    /// A session was paused.
    SessionPaused { session_id: Uuid },

    /// A session was resumed.
    SessionResumed { session_id: Uuid },

    /// A session was aborted.
    SessionAborted { session_id: Uuid, reason: String },

    /// A plan phase was marked complete.
    PlanPhaseCompleted {
        phase_id: String,
        phase_title: String,
    },

    /// A review was requested (human attention needed).
    ReviewRequested {
        goal_id: Uuid,
        draft_id: Uuid,
        summary: String,
    },

    /// A policy violation was detected.
    PolicyViolation {
        goal_id: Option<Uuid>,
        agent_id: String,
        resource_uri: String,
        violation: String,
    },

    /// A memory entry was stored.
    MemoryStored {
        key: String,
        category: Option<String>,
        source: String,
    },
}

impl SessionEvent {
    /// Get the event type name as a static string.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::GoalStarted { .. } => "goal_started",
            Self::GoalCompleted { .. } => "goal_completed",
            Self::DraftBuilt { .. } => "draft_built",
            Self::DraftSubmitted { .. } => "draft_submitted",
            Self::DraftApproved { .. } => "draft_approved",
            Self::DraftDenied { .. } => "draft_denied",
            Self::DraftApplied { .. } => "draft_applied",
            Self::SessionPaused { .. } => "session_paused",
            Self::SessionResumed { .. } => "session_resumed",
            Self::SessionAborted { .. } => "session_aborted",
            Self::PlanPhaseCompleted { .. } => "plan_phase_completed",
            Self::ReviewRequested { .. } => "review_requested",
            Self::PolicyViolation { .. } => "policy_violation",
            Self::MemoryStored { .. } => "memory_stored",
        }
    }

    /// Extract the goal ID from events that carry one.
    pub fn goal_id(&self) -> Option<Uuid> {
        match self {
            Self::GoalStarted { goal_id, .. }
            | Self::GoalCompleted { goal_id, .. }
            | Self::DraftBuilt { goal_id, .. }
            | Self::DraftSubmitted { goal_id, .. }
            | Self::DraftApproved { goal_id, .. }
            | Self::DraftDenied { goal_id, .. }
            | Self::DraftApplied { goal_id, .. }
            | Self::ReviewRequested { goal_id, .. } => Some(*goal_id),
            Self::PolicyViolation { goal_id, .. } => *goal_id,
            _ => None,
        }
    }

    /// Extract the phase from events that carry one.
    pub fn phase(&self) -> Option<&str> {
        match self {
            Self::GoalStarted { phase, .. } => phase.as_deref(),
            Self::PlanPhaseCompleted { phase_id, .. } => Some(phase_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_creation() {
        let event = SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "Test".into(),
            agent_id: "claude-code".into(),
            phase: Some("v0.8.0".into()),
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.version, SCHEMA_VERSION);
        assert_eq!(envelope.event_type, "goal_started");
    }

    #[test]
    fn event_serialization_round_trip() {
        let event = SessionEvent::DraftApproved {
            goal_id: Uuid::new_v4(),
            draft_id: Uuid::new_v4(),
            approved_by: "human".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let restored: SessionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.event_type(), restored.event_type());
    }

    #[test]
    fn envelope_serialization() {
        let event = SessionEvent::PolicyViolation {
            goal_id: Some(Uuid::new_v4()),
            agent_id: "codex".into(),
            resource_uri: "fs://workspace/secrets.env".into(),
            violation: "Attempted to read credential file".into(),
        };
        let envelope = EventEnvelope::new(event);
        let json = serde_json::to_string_pretty(&envelope).unwrap();
        assert!(json.contains("\"policy_violation\""));
        assert!(json.contains("\"version\":"));
    }

    #[test]
    fn all_event_types_have_names() {
        let gid = Uuid::new_v4();
        let did = Uuid::new_v4();
        let sid = Uuid::new_v4();
        let events: Vec<SessionEvent> = vec![
            SessionEvent::GoalStarted {
                goal_id: gid,
                title: "t".into(),
                agent_id: "a".into(),
                phase: None,
            },
            SessionEvent::GoalCompleted {
                goal_id: gid,
                title: "t".into(),
                duration_secs: Some(60),
            },
            SessionEvent::DraftBuilt {
                goal_id: gid,
                draft_id: did,
                artifact_count: 3,
            },
            SessionEvent::DraftSubmitted {
                goal_id: gid,
                draft_id: did,
            },
            SessionEvent::DraftApproved {
                goal_id: gid,
                draft_id: did,
                approved_by: "h".into(),
            },
            SessionEvent::DraftDenied {
                goal_id: gid,
                draft_id: did,
                reason: "r".into(),
                denied_by: "h".into(),
            },
            SessionEvent::DraftApplied {
                goal_id: gid,
                draft_id: did,
                files_count: 5,
            },
            SessionEvent::SessionPaused { session_id: sid },
            SessionEvent::SessionResumed { session_id: sid },
            SessionEvent::SessionAborted {
                session_id: sid,
                reason: "done".into(),
            },
            SessionEvent::PlanPhaseCompleted {
                phase_id: "v0.8.0".into(),
                phase_title: "Events".into(),
            },
            SessionEvent::ReviewRequested {
                goal_id: gid,
                draft_id: did,
                summary: "s".into(),
            },
            SessionEvent::PolicyViolation {
                goal_id: None,
                agent_id: "a".into(),
                resource_uri: "fs://x".into(),
                violation: "v".into(),
            },
            SessionEvent::MemoryStored {
                key: "k".into(),
                category: None,
                source: "cli".into(),
            },
        ];
        for e in &events {
            assert!(!e.event_type().is_empty());
        }
        assert_eq!(events.len(), 14);
    }

    #[test]
    fn goal_id_extraction() {
        let gid = Uuid::new_v4();
        let event = SessionEvent::DraftBuilt {
            goal_id: gid,
            draft_id: Uuid::new_v4(),
            artifact_count: 1,
        };
        assert_eq!(event.goal_id(), Some(gid));

        let paused = SessionEvent::SessionPaused {
            session_id: Uuid::new_v4(),
        };
        assert_eq!(paused.goal_id(), None);
    }

    #[test]
    fn phase_extraction() {
        let event = SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "t".into(),
            agent_id: "a".into(),
            phase: Some("v0.8.0".into()),
        };
        assert_eq!(event.phase(), Some("v0.8.0"));

        let completed = SessionEvent::PlanPhaseCompleted {
            phase_id: "v0.7.5".into(),
            phase_title: "Fixes".into(),
        };
        assert_eq!(completed.phase(), Some("v0.7.5"));
    }
}
