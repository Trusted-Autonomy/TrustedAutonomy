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

/// A structured action that any interface can render as an actionable next step.
///
/// Actions are embedded in event envelopes so that non-CLI interfaces (Discord,
/// Slack, webapp, email) can present the same actionable suggestions that the
/// CLI shell currently hardcodes in its renderer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventAction {
    /// Machine-readable verb: "view", "approve", "deny", "tail", "list"
    pub verb: String,
    /// The CLI command to execute (interface-agnostic)
    pub command: String,
    /// Human-readable label for the action
    pub label: String,
}

impl EventAction {
    /// Create a new action.
    pub fn new(
        verb: impl Into<String>,
        command: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            verb: verb.into(),
            command: command.into(),
            label: label.into(),
        }
    }
}

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
    /// Structured actions that any interface can render as next steps.
    /// Most events have an empty list; key lifecycle events populate this.
    #[serde(default)]
    pub actions: Vec<EventAction>,
}

impl EventEnvelope {
    /// Create a new envelope wrapping the given event.
    /// Actions are automatically derived from the event payload.
    pub fn new(event: SessionEvent) -> Self {
        let actions = event.suggested_actions();
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            version: SCHEMA_VERSION,
            event_type: event.event_type().to_string(),
            payload: event,
            actions,
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
        /// Human-readable goal title, for display in channel notifications.
        title: String,
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

    /// A goal failed due to agent exit, crash, or workspace error (v0.9.4).
    GoalFailed {
        goal_id: Uuid,
        error: String,
        exit_code: Option<i32>,
    },

    /// An agent running a goal needs human input to continue.
    AgentNeedsInput {
        goal_id: Uuid,
        interaction_id: Uuid,
        question: String,
        #[serde(default)]
        context: Option<String>,
        #[serde(default = "default_response_hint")]
        response_hint: String,
        #[serde(default)]
        choices: Vec<String>,
        turn: u32,
        #[serde(default)]
        timeout_secs: Option<u64>,
        /// Channel routing hints — which external channels to deliver this
        /// question to. Empty means use daemon defaults.
        #[serde(default)]
        channels: Vec<String>,
    },

    /// A human answered an agent's question.
    AgentQuestionAnswered {
        goal_id: Uuid,
        interaction_id: Uuid,
        responder_id: String,
        turn: u32,
    },

    /// An interactive session started (multiple exchanges expected).
    InteractiveSessionStarted {
        goal_id: Uuid,
        session_type: String,
        channel: String,
    },

    /// An interactive session completed.
    InteractiveSessionCompleted {
        goal_id: Uuid,
        turn_count: u32,
        outcome: String,
    },

    /// A background command (e.g., `ta run`) failed.
    CommandFailed {
        command: String,
        exit_code: i32,
        stderr: String,
    },

    /// Upstream sync completed successfully (v0.11.1).
    SyncCompleted {
        adapter: String,
        new_commits: u32,
        message: String,
    },

    /// Upstream sync detected conflicts (v0.11.1).
    SyncConflict {
        adapter: String,
        conflicts: Vec<String>,
        message: String,
    },

    /// A project build completed successfully (v0.11.2).
    BuildCompleted {
        adapter: String,
        operation: String,
        duration_secs: f64,
        message: String,
    },

    /// A project build or test failed (v0.11.2).
    BuildFailed {
        adapter: String,
        operation: String,
        exit_code: i32,
        duration_secs: f64,
        message: String,
    },

    /// An agent process exited unexpectedly while the goal was still running (v0.11.2.4).
    GoalProcessExited {
        goal_id: Uuid,
        pid: u32,
        exit_code: Option<i32>,
        elapsed_secs: u64,
        detail: String,
    },

    /// A daemon health check found issues (v0.11.2.4).
    /// Only emitted when issues are detected — silent when healthy.
    HealthCheck {
        goals_checked: usize,
        issues: Vec<HealthIssue>,
    },

    /// An agent question has been pending too long (v0.11.2.4).
    QuestionStale {
        goal_id: Uuid,
        interaction_id: Uuid,
        question_preview: String,
        pending_secs: u64,
    },

    /// The host woke from sleep (v0.13.1.1).
    ///
    /// Detected by comparing wall-clock advance vs monotonic advance: a wall-clock
    /// delta significantly larger than the monotonic delta indicates the system slept.
    /// Heartbeat deadlines are extended by `wake_grace_secs` after this event.
    SystemWoke {
        /// Approximate number of seconds the system was asleep.
        slept_for_secs: u64,
    },

    /// Network connectivity to the Claude API was lost after waking from sleep (v0.13.1.1).
    ///
    /// Emitted when the daemon's post-wake connectivity check finds the API unreachable.
    /// The shell shows "Woke from sleep — waiting for network..." until restored.
    ApiConnectionLost {
        /// The URL that was unreachable.
        checked_url: String,
    },

    /// Network connectivity to the Claude API was restored (v0.13.1.1).
    ///
    /// Emitted after `ApiConnectionLost` when the API becomes reachable again.
    ApiConnectionRestored {
        /// The URL that is now reachable.
        checked_url: String,
    },
}

/// A single issue found during a watchdog health check (v0.11.2.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthIssue {
    pub kind: String,
    pub goal_id: Option<Uuid>,
    pub detail: String,
}

fn default_response_hint() -> String {
    "freeform".to_string()
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
            Self::GoalFailed { .. } => "goal_failed",
            Self::AgentNeedsInput { .. } => "agent_needs_input",
            Self::AgentQuestionAnswered { .. } => "agent_question_answered",
            Self::InteractiveSessionStarted { .. } => "interactive_session_started",
            Self::InteractiveSessionCompleted { .. } => "interactive_session_completed",
            Self::CommandFailed { .. } => "command_failed",
            Self::SyncCompleted { .. } => "sync_completed",
            Self::SyncConflict { .. } => "sync_conflict",
            Self::BuildCompleted { .. } => "build_completed",
            Self::BuildFailed { .. } => "build_failed",
            Self::GoalProcessExited { .. } => "goal_process_exited",
            Self::HealthCheck { .. } => "health_check",
            Self::QuestionStale { .. } => "question_stale",
            Self::SystemWoke { .. } => "system_woke",
            Self::ApiConnectionLost { .. } => "api_connection_lost",
            Self::ApiConnectionRestored { .. } => "api_connection_restored",
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
            | Self::ReviewRequested { goal_id, .. }
            | Self::GoalFailed { goal_id, .. }
            | Self::AgentNeedsInput { goal_id, .. }
            | Self::AgentQuestionAnswered { goal_id, .. }
            | Self::InteractiveSessionStarted { goal_id, .. }
            | Self::InteractiveSessionCompleted { goal_id, .. }
            | Self::GoalProcessExited { goal_id, .. }
            | Self::QuestionStale { goal_id, .. } => Some(*goal_id),
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

    /// Return structured actions appropriate for this event type.
    ///
    /// These are suggested next steps an operator or interface can present
    /// to the user. Any interface (CLI, Discord, webapp) can render them
    /// without hardcoding event-specific logic.
    pub fn suggested_actions(&self) -> Vec<EventAction> {
        match self {
            Self::GoalStarted { goal_id, .. } => {
                let short_id = &goal_id.to_string()[..8];
                vec![EventAction::new(
                    "tail",
                    format!("ta shell :tail {}", short_id),
                    format!("Tail live output for goal {}", short_id),
                )]
            }
            Self::GoalCompleted { .. } => {
                vec![EventAction::new("list", "ta draft list", "List all drafts")]
            }
            Self::DraftBuilt { draft_id, .. } => {
                let full_id = draft_id.to_string();
                let short_id = &full_id[..8];
                vec![
                    EventAction::new(
                        "view",
                        format!("ta draft view {}", full_id),
                        format!("View draft {}", short_id),
                    ),
                    EventAction::new(
                        "approve",
                        format!("ta draft approve {}", full_id),
                        format!("Approve draft {}", short_id),
                    ),
                    EventAction::new(
                        "deny",
                        format!("ta draft deny {}", full_id),
                        format!("Deny draft {}", short_id),
                    ),
                ]
            }
            Self::AgentNeedsInput { interaction_id, .. } => {
                let iid = interaction_id.to_string();
                let short_id = &iid[..8];
                vec![EventAction::new(
                    "respond",
                    format!("ta interact respond {}", iid),
                    format!("Respond to agent question {}", short_id),
                )]
            }
            Self::CommandFailed { command, .. } => {
                vec![EventAction::new(
                    "retry",
                    command.clone(),
                    format!("Retry: {}", command),
                )]
            }
            Self::BuildFailed { operation, .. } => {
                vec![EventAction::new(
                    "retry",
                    format!(
                        "ta build{}",
                        if operation == "test" { " --test" } else { "" }
                    ),
                    format!("Retry {} build", operation),
                )]
            }
            Self::GoalProcessExited { goal_id, .. } => {
                let short_id = &goal_id.to_string()[..8];
                vec![
                    EventAction::new(
                        "inspect",
                        format!("ta goal status {}", short_id),
                        format!("Inspect goal {}", short_id),
                    ),
                    EventAction::new(
                        "list",
                        "ta goal list --all".to_string(),
                        "List all goals".to_string(),
                    ),
                ]
            }
            Self::QuestionStale { interaction_id, .. } => {
                let iid = interaction_id.to_string();
                vec![EventAction::new(
                    "respond",
                    format!("ta interact respond {}", iid),
                    "Respond to stale question".to_string(),
                )]
            }
            Self::ApiConnectionLost { .. } => {
                vec![EventAction::new(
                    "status",
                    "ta status --deep",
                    "Check daemon and network status",
                )]
            }
            // All other events have no suggested actions.
            _ => vec![],
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
    fn goal_started_envelope_has_tail_action() {
        let goal_id = Uuid::new_v4();
        let event = SessionEvent::GoalStarted {
            goal_id,
            title: "Fix auth".into(),
            agent_id: "claude-code".into(),
            phase: None,
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.actions.len(), 1);
        assert_eq!(envelope.actions[0].verb, "tail");
        let short_id = &goal_id.to_string()[..8];
        assert!(envelope.actions[0].command.contains(short_id));
    }

    #[test]
    fn draft_built_envelope_has_view_approve_deny_actions() {
        let draft_id = Uuid::new_v4();
        let event = SessionEvent::DraftBuilt {
            goal_id: Uuid::new_v4(),
            draft_id,
            artifact_count: 5,
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.actions.len(), 3);
        let verbs: Vec<&str> = envelope.actions.iter().map(|a| a.verb.as_str()).collect();
        assert!(verbs.contains(&"view"));
        assert!(verbs.contains(&"approve"));
        assert!(verbs.contains(&"deny"));
        let full_id = draft_id.to_string();
        for action in &envelope.actions {
            assert!(action.command.contains(&full_id));
        }
    }

    #[test]
    fn goal_completed_envelope_has_list_action() {
        let event = SessionEvent::GoalCompleted {
            goal_id: Uuid::new_v4(),
            title: "Done".into(),
            duration_secs: Some(90),
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.actions.len(), 1);
        assert_eq!(envelope.actions[0].verb, "list");
        assert!(envelope.actions[0].command.contains("ta draft list"));
    }

    #[test]
    fn other_events_have_no_actions() {
        let event = SessionEvent::MemoryStored {
            key: "k".into(),
            category: None,
            source: "cli".into(),
        };
        let envelope = EventEnvelope::new(event);
        assert!(envelope.actions.is_empty());
    }

    #[test]
    fn envelope_serialization_includes_actions() {
        let event = SessionEvent::DraftBuilt {
            goal_id: Uuid::new_v4(),
            draft_id: Uuid::new_v4(),
            artifact_count: 2,
        };
        let envelope = EventEnvelope::new(event);
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("\"actions\""));
        assert!(json.contains("\"verb\""));
        assert!(json.contains("\"command\""));
        assert!(json.contains("\"label\""));
    }

    #[test]
    fn envelope_deserialization_backwards_compat_no_actions_field() {
        // Old events without an `actions` field should deserialize with empty actions.
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "timestamp": "2026-01-01T00:00:00Z",
            "version": 1,
            "event_type": "memory_stored",
            "payload": {"type": "memory_stored", "key": "k", "category": null, "source": "cli"}
        }"#;
        let envelope: EventEnvelope = serde_json::from_str(json).unwrap();
        assert!(envelope.actions.is_empty());
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
                title: "test goal".into(),
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
            SessionEvent::GoalFailed {
                goal_id: gid,
                error: "agent crashed".into(),
                exit_code: Some(1),
            },
            SessionEvent::AgentNeedsInput {
                goal_id: gid,
                interaction_id: Uuid::new_v4(),
                question: "Which DB?".into(),
                context: None,
                response_hint: "freeform".into(),
                choices: vec![],
                turn: 1,
                timeout_secs: None,
                channels: vec![],
            },
            SessionEvent::AgentQuestionAnswered {
                goal_id: gid,
                interaction_id: Uuid::new_v4(),
                responder_id: "human".into(),
                turn: 1,
            },
            SessionEvent::InteractiveSessionStarted {
                goal_id: gid,
                session_type: "clarification".into(),
                channel: "cli".into(),
            },
            SessionEvent::InteractiveSessionCompleted {
                goal_id: gid,
                turn_count: 3,
                outcome: "completed".into(),
            },
            SessionEvent::SyncCompleted {
                adapter: "git".into(),
                new_commits: 3,
                message: "ok".into(),
            },
            SessionEvent::SyncConflict {
                adapter: "git".into(),
                conflicts: vec!["a.rs".into()],
                message: "conflict".into(),
            },
            SessionEvent::BuildCompleted {
                adapter: "cargo".into(),
                operation: "build".into(),
                duration_secs: 5.0,
                message: "ok".into(),
            },
            SessionEvent::BuildFailed {
                adapter: "cargo".into(),
                operation: "test".into(),
                exit_code: 1,
                duration_secs: 3.0,
                message: "failed".into(),
            },
            SessionEvent::GoalProcessExited {
                goal_id: gid,
                pid: 12345,
                exit_code: Some(1),
                elapsed_secs: 300,
                detail: "process exited".into(),
            },
            SessionEvent::HealthCheck {
                goals_checked: 2,
                issues: vec![HealthIssue {
                    kind: "zombie_goal".into(),
                    goal_id: Some(gid),
                    detail: "process exited 5m ago".into(),
                }],
            },
            SessionEvent::QuestionStale {
                goal_id: gid,
                interaction_id: Uuid::new_v4(),
                question_preview: "Which DB?".into(),
                pending_secs: 7200,
            },
        ];
        for e in &events {
            assert!(!e.event_type().is_empty());
        }
        assert_eq!(events.len(), 26);
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

    // §8 regression tests: DraftApproved, DraftDenied, DraftApplied events must
    // exist with structured goal_id/draft_id fields and correct event_type strings.
    // If any variant is removed or renamed these tests will catch the regression.
    #[test]
    fn draft_approved_event_serializes_with_structured_fields() {
        let goal_id = Uuid::new_v4();
        let draft_id = Uuid::new_v4();
        let event = SessionEvent::DraftApproved {
            goal_id,
            draft_id,
            approved_by: "alice".to_string(),
        };
        assert_eq!(event.event_type(), "draft_approved");
        assert_eq!(event.goal_id(), Some(goal_id));
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"draft_approved\""));
        assert!(json.contains(&goal_id.to_string()));
        assert!(json.contains(&draft_id.to_string()));
        assert!(json.contains("\"alice\""));
    }

    #[test]
    fn draft_denied_event_serializes_with_structured_fields() {
        let goal_id = Uuid::new_v4();
        let draft_id = Uuid::new_v4();
        let event = SessionEvent::DraftDenied {
            goal_id,
            draft_id,
            reason: "needs more tests".to_string(),
            denied_by: "bob".to_string(),
        };
        assert_eq!(event.event_type(), "draft_denied");
        assert_eq!(event.goal_id(), Some(goal_id));
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"draft_denied\""));
        assert!(json.contains("\"needs more tests\""));
        assert!(json.contains("\"bob\""));
    }

    #[test]
    fn draft_applied_event_serializes_with_structured_fields() {
        let goal_id = Uuid::new_v4();
        let draft_id = Uuid::new_v4();
        let event = SessionEvent::DraftApplied {
            goal_id,
            draft_id,
            files_count: 7,
        };
        assert_eq!(event.event_type(), "draft_applied");
        assert_eq!(event.goal_id(), Some(goal_id));
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"draft_applied\""));
        assert!(json.contains("\"files_count\":7"));
    }

    #[test]
    fn agent_needs_input_has_respond_action() {
        let interaction_id = Uuid::new_v4();
        let event = SessionEvent::AgentNeedsInput {
            goal_id: Uuid::new_v4(),
            interaction_id,
            question: "What database?".into(),
            context: None,
            response_hint: "freeform".into(),
            choices: vec![],
            turn: 1,
            timeout_secs: None,
            channels: vec![],
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.actions.len(), 1);
        assert_eq!(envelope.actions[0].verb, "respond");
        assert!(envelope.actions[0]
            .command
            .contains(&interaction_id.to_string()));
    }

    #[test]
    fn system_woke_event_type() {
        let event = SessionEvent::SystemWoke {
            slept_for_secs: 120,
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.event_type, "system_woke");
        assert!(envelope.actions.is_empty());
    }

    #[test]
    fn api_connection_lost_has_status_action() {
        let event = SessionEvent::ApiConnectionLost {
            checked_url: "https://api.anthropic.com".into(),
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.event_type, "api_connection_lost");
        assert_eq!(envelope.actions.len(), 1);
        assert_eq!(envelope.actions[0].verb, "status");
    }

    #[test]
    fn api_connection_restored_event_type() {
        let event = SessionEvent::ApiConnectionRestored {
            checked_url: "https://api.anthropic.com".into(),
        };
        let envelope = EventEnvelope::new(event);
        assert_eq!(envelope.event_type, "api_connection_restored");
        assert!(envelope.actions.is_empty());
    }
}
