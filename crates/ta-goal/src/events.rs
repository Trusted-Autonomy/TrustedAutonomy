// events.rs — Event model and notification dispatch.
//
// TA emits events at key lifecycle points. Notification sinks (log files,
// webhook scripts, Discord, email) subscribe to these events.
//
// This aligns with the Plugin Architecture guidance:
// - Plugins observe and advise, they cannot bypass policy
// - Events use stable types that plugins can depend on
// - The dispatcher is synchronous for now; async dispatch is a future enhancement
//
// Core event hooks from the architecture:
//   Goal lifecycle: on_goal_created, on_goal_configured, on_goal_started, etc.
//   Execution: on_changeset_created, on_pr_generated, on_pr_approved, on_pr_denied
//   Audit: on_policy_violation, on_anomaly_detected

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::GoalError;
use crate::goal_run::GoalRunState;

/// Events emitted by TA at key lifecycle points.
///
/// These are the stable event types that notification sinks and future
/// plugins can subscribe to. The enum variants map to the event hooks
/// defined in the plugin architecture guidance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum TaEvent {
    /// A new GoalRun was created.
    GoalCreated {
        goal_run_id: Uuid,
        title: String,
        agent_id: String,
        timestamp: DateTime<Utc>,
    },

    /// A GoalRun changed state.
    GoalStateChanged {
        goal_run_id: Uuid,
        from_state: String,
        to_state: String,
        timestamp: DateTime<Utc>,
    },

    /// A PR package is ready for review.
    PrReady {
        goal_run_id: Uuid,
        pr_package_id: Uuid,
        summary: String,
        timestamp: DateTime<Utc>,
    },

    /// A PR package was approved.
    PrApproved {
        goal_run_id: Uuid,
        pr_package_id: Uuid,
        approved_by: String,
        timestamp: DateTime<Utc>,
    },

    /// A PR package was denied.
    PrDenied {
        goal_run_id: Uuid,
        pr_package_id: Uuid,
        reason: String,
        denied_by: String,
        timestamp: DateTime<Utc>,
    },

    /// Approved changes were applied to the target.
    ChangesApplied {
        goal_run_id: Uuid,
        files: Vec<String>,
        target_dir: String,
        timestamp: DateTime<Utc>,
    },

    /// A ChangeSet was created (file staged).
    ChangesetCreated {
        goal_run_id: Uuid,
        changeset_id: Uuid,
        target_uri: String,
        timestamp: DateTime<Utc>,
    },

    /// An interactive session was started (v0.3.1.2).
    SessionStarted {
        goal_run_id: Uuid,
        session_id: Uuid,
        channel_id: String,
        agent_id: String,
        timestamp: DateTime<Utc>,
    },

    /// An interactive session state changed (v0.3.1.2).
    SessionStateChanged {
        session_id: Uuid,
        from_state: String,
        to_state: String,
        timestamp: DateTime<Utc>,
    },

    /// Human sent a message in an interactive session (v0.3.1.2).
    SessionMessage {
        session_id: Uuid,
        sender: String,
        content_preview: String,
        timestamp: DateTime<Utc>,
    },

    /// Agent proposed a plan update within a macro goal session (v0.4.1).
    /// Held for human approval — the update is not applied automatically.
    PlanUpdateProposed {
        goal_run_id: Uuid,
        phase: String,
        status_note: String,
        timestamp: DateTime<Utc>,
    },

    /// A session was paused by the human (v0.6.0).
    SessionPaused {
        session_id: Uuid,
        timestamp: DateTime<Utc>,
    },

    /// A paused session was resumed (v0.6.0).
    SessionResumed {
        session_id: Uuid,
        timestamp: DateTime<Utc>,
    },

    /// A session was aborted by the human (v0.6.0).
    SessionAborted {
        session_id: Uuid,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// A draft was built from workspace diff (v0.6.0).
    DraftBuilt {
        session_id: Uuid,
        draft_id: Uuid,
        artifact_count: usize,
        timestamp: DateTime<Utc>,
    },

    /// Human made a review decision on a draft (v0.6.0).
    ReviewDecision {
        session_id: Uuid,
        draft_id: Uuid,
        approved: bool,
        feedback: Option<String>,
        timestamp: DateTime<Utc>,
    },

    /// Session entered a new iteration after rejection (v0.6.0).
    SessionIteration {
        session_id: Uuid,
        iteration: u32,
        timestamp: DateTime<Utc>,
    },

    /// A goal failed due to agent exit, crash, or workspace setup error (v0.9.4).
    GoalFailed {
        goal_run_id: Uuid,
        error: String,
        exit_code: Option<i32>,
        timestamp: DateTime<Utc>,
    },

    /// An agent session started working on a goal or as orchestrator (v0.9.6).
    AgentSessionStarted {
        agent_id: String,
        agent_type: String,
        goal_run_id: Option<Uuid>,
        caller_mode: String,
        timestamp: DateTime<Utc>,
    },

    /// An agent session ended (v0.9.6).
    AgentSessionEnded {
        agent_id: String,
        goal_run_id: Option<Uuid>,
        timestamp: DateTime<Utc>,
    },
}

impl TaEvent {
    /// Get the event type name as a string.
    pub fn event_type(&self) -> &str {
        match self {
            TaEvent::GoalCreated { .. } => "goal_created",
            TaEvent::GoalStateChanged { .. } => "goal_state_changed",
            TaEvent::PrReady { .. } => "pr_ready",
            TaEvent::PrApproved { .. } => "pr_approved",
            TaEvent::PrDenied { .. } => "pr_denied",
            TaEvent::ChangesApplied { .. } => "changes_applied",
            TaEvent::ChangesetCreated { .. } => "changeset_created",
            TaEvent::SessionStarted { .. } => "session_started",
            TaEvent::SessionStateChanged { .. } => "session_state_changed",
            TaEvent::SessionMessage { .. } => "session_message",
            TaEvent::PlanUpdateProposed { .. } => "plan_update_proposed",
            TaEvent::SessionPaused { .. } => "session_paused",
            TaEvent::SessionResumed { .. } => "session_resumed",
            TaEvent::SessionAborted { .. } => "session_aborted",
            TaEvent::DraftBuilt { .. } => "draft_built",
            TaEvent::ReviewDecision { .. } => "review_decision",
            TaEvent::SessionIteration { .. } => "session_iteration",
            TaEvent::GoalFailed { .. } => "goal_failed",
            TaEvent::AgentSessionStarted { .. } => "agent_session_started",
            TaEvent::AgentSessionEnded { .. } => "agent_session_ended",
        }
    }

    /// Helper to create a GoalCreated event.
    pub fn goal_created(goal_run_id: Uuid, title: &str, agent_id: &str) -> Self {
        TaEvent::GoalCreated {
            goal_run_id,
            title: title.to_string(),
            agent_id: agent_id.to_string(),
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a GoalStateChanged event.
    pub fn goal_state_changed(goal_run_id: Uuid, from: &GoalRunState, to: &GoalRunState) -> Self {
        TaEvent::GoalStateChanged {
            goal_run_id,
            from_state: from.to_string(),
            to_state: to.to_string(),
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a SessionPaused event (v0.6.0).
    pub fn session_paused(session_id: Uuid) -> Self {
        TaEvent::SessionPaused {
            session_id,
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a SessionResumed event (v0.6.0).
    pub fn session_resumed(session_id: Uuid) -> Self {
        TaEvent::SessionResumed {
            session_id,
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a SessionAborted event (v0.6.0).
    pub fn session_aborted(session_id: Uuid, reason: &str) -> Self {
        TaEvent::SessionAborted {
            session_id,
            reason: reason.to_string(),
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a DraftBuilt event (v0.6.0).
    pub fn draft_built(session_id: Uuid, draft_id: Uuid, artifact_count: usize) -> Self {
        TaEvent::DraftBuilt {
            session_id,
            draft_id,
            artifact_count,
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a ReviewDecision event (v0.6.0).
    pub fn review_decision(
        session_id: Uuid,
        draft_id: Uuid,
        approved: bool,
        feedback: Option<&str>,
    ) -> Self {
        TaEvent::ReviewDecision {
            session_id,
            draft_id,
            approved,
            feedback: feedback.map(|s| s.to_string()),
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a SessionIteration event (v0.6.0).
    pub fn session_iteration(session_id: Uuid, iteration: u32) -> Self {
        TaEvent::SessionIteration {
            session_id,
            iteration,
            timestamp: Utc::now(),
        }
    }

    /// Helper to create a GoalFailed event (v0.9.4).
    pub fn goal_failed(goal_run_id: Uuid, error: &str, exit_code: Option<i32>) -> Self {
        TaEvent::GoalFailed {
            goal_run_id,
            error: error.to_string(),
            exit_code,
            timestamp: Utc::now(),
        }
    }

    /// Helper to create an AgentSessionStarted event (v0.9.6).
    pub fn agent_session_started(
        agent_id: &str,
        agent_type: &str,
        goal_run_id: Option<Uuid>,
        caller_mode: &str,
    ) -> Self {
        TaEvent::AgentSessionStarted {
            agent_id: agent_id.to_string(),
            agent_type: agent_type.to_string(),
            goal_run_id,
            caller_mode: caller_mode.to_string(),
            timestamp: Utc::now(),
        }
    }

    /// Helper to create an AgentSessionEnded event (v0.9.6).
    pub fn agent_session_ended(agent_id: &str, goal_run_id: Option<Uuid>) -> Self {
        TaEvent::AgentSessionEnded {
            agent_id: agent_id.to_string(),
            goal_run_id,
            timestamp: Utc::now(),
        }
    }
}

/// Trait for receiving TA events.
///
/// Implementations decide what to do with each event: log to a file,
/// call a webhook, send a Discord message, etc.
///
/// This is the foundation of the plugin architecture. In Phase 3+,
/// this trait will be extended with filtering (subscribe to specific
/// event types) and async dispatch.
pub trait NotificationSink: Send {
    /// Handle an event. Errors are logged but don't stop the system.
    fn send(&self, event: &TaEvent) -> Result<(), GoalError>;
}

/// Logs events as JSONL to a file (always-on sink).
pub struct LogSink {
    path: PathBuf,
}

impl LogSink {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl NotificationSink for LogSink {
    fn send(&self, event: &TaEvent) -> Result<(), GoalError> {
        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| GoalError::IoError {
                path: parent.display().to_string(),
                source,
            })?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;

        let json = serde_json::to_string(event)?;
        writeln!(file, "{}", json).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;

        Ok(())
    }
}

/// Dispatches events to multiple sinks.
///
/// Errors from individual sinks are logged (via tracing) but don't
/// prevent other sinks from receiving the event.
pub struct EventDispatcher {
    sinks: Vec<Box<dyn NotificationSink>>,
}

impl EventDispatcher {
    /// Create a new dispatcher with no sinks.
    pub fn new() -> Self {
        Self { sinks: Vec::new() }
    }

    /// Add a notification sink.
    pub fn add_sink(&mut self, sink: Box<dyn NotificationSink>) {
        self.sinks.push(sink);
    }

    /// Dispatch an event to all sinks.
    pub fn dispatch(&self, event: &TaEvent) {
        for sink in &self.sinks {
            if let Err(e) = sink.send(event) {
                tracing::warn!("notification sink error: {}", e);
            }
        }
    }
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn event_serialization_round_trip() {
        let event = TaEvent::goal_created(Uuid::new_v4(), "Test Goal", "agent-1");
        let json = serde_json::to_string(&event).unwrap();
        let restored: TaEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(event.event_type(), restored.event_type());
        assert!(json.contains("\"goal_created\""));
    }

    #[test]
    fn log_sink_appends_to_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let sink = LogSink::new(&path);

        let event1 = TaEvent::goal_created(Uuid::new_v4(), "Goal 1", "agent-1");
        let event2 = TaEvent::goal_created(Uuid::new_v4(), "Goal 2", "agent-2");

        sink.send(&event1).unwrap();
        sink.send(&event2).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn dispatcher_sends_to_all_sinks() {
        let dir = tempdir().unwrap();
        let path1 = dir.path().join("sink1.jsonl");
        let path2 = dir.path().join("sink2.jsonl");

        let mut dispatcher = EventDispatcher::new();
        dispatcher.add_sink(Box::new(LogSink::new(&path1)));
        dispatcher.add_sink(Box::new(LogSink::new(&path2)));

        let event = TaEvent::goal_created(Uuid::new_v4(), "Test", "agent-1");
        dispatcher.dispatch(&event);

        // Both sinks should have received the event.
        assert!(fs::read_to_string(&path1).unwrap().contains("goal_created"));
        assert!(fs::read_to_string(&path2).unwrap().contains("goal_created"));
    }

    #[test]
    fn event_type_names() {
        let id = Uuid::new_v4();
        assert_eq!(
            TaEvent::goal_created(id, "x", "y").event_type(),
            "goal_created"
        );
        assert_eq!(
            TaEvent::goal_state_changed(id, &GoalRunState::Created, &GoalRunState::Configured)
                .event_type(),
            "goal_state_changed"
        );
    }

    #[test]
    fn session_event_types_v060() {
        let sid = Uuid::new_v4();
        let did = Uuid::new_v4();

        assert_eq!(TaEvent::session_paused(sid).event_type(), "session_paused");
        assert_eq!(
            TaEvent::session_resumed(sid).event_type(),
            "session_resumed"
        );
        assert_eq!(
            TaEvent::session_aborted(sid, "user cancelled").event_type(),
            "session_aborted"
        );
        assert_eq!(
            TaEvent::draft_built(sid, did, 5).event_type(),
            "draft_built"
        );
        assert_eq!(
            TaEvent::review_decision(sid, did, true, None).event_type(),
            "review_decision"
        );
        assert_eq!(
            TaEvent::session_iteration(sid, 2).event_type(),
            "session_iteration"
        );
    }

    #[test]
    fn session_event_serialization_v060() {
        let sid = Uuid::new_v4();
        let did = Uuid::new_v4();

        let events = vec![
            TaEvent::session_paused(sid),
            TaEvent::session_resumed(sid),
            TaEvent::session_aborted(sid, "cancelled"),
            TaEvent::draft_built(sid, did, 3),
            TaEvent::review_decision(sid, did, false, Some("needs work")),
            TaEvent::session_iteration(sid, 1),
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let restored: TaEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event.event_type(), restored.event_type());
        }
    }

    #[test]
    fn goal_failed_event_v094() {
        let gid = Uuid::new_v4();
        let event = TaEvent::goal_failed(gid, "agent exited with code 1", Some(1));
        assert_eq!(event.event_type(), "goal_failed");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("goal_failed"));
        assert!(json.contains("agent exited with code 1"));
        assert!(json.contains("\"exit_code\":1"));

        let restored: TaEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.event_type(), "goal_failed");
    }

    #[test]
    fn goal_failed_no_exit_code() {
        let gid = Uuid::new_v4();
        let event = TaEvent::goal_failed(gid, "workspace setup failed", None);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"exit_code\":null"));
    }

    #[test]
    fn agent_session_events_v096() {
        let gid = Uuid::new_v4();
        let started = TaEvent::agent_session_started("agent-1", "claude-code", Some(gid), "normal");
        assert_eq!(started.event_type(), "agent_session_started");
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains("agent_session_started"));
        assert!(json.contains("claude-code"));
        let restored: TaEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.event_type(), "agent_session_started");

        let ended = TaEvent::agent_session_ended("agent-1", Some(gid));
        assert_eq!(ended.event_type(), "agent_session_ended");
        let json2 = serde_json::to_string(&ended).unwrap();
        let restored2: TaEvent = serde_json::from_str(&json2).unwrap();
        assert_eq!(restored2.event_type(), "agent_session_ended");
    }

    #[test]
    fn agent_session_no_goal() {
        let started = TaEvent::agent_session_started("orch-1", "claude-code", None, "orchestrator");
        let json = serde_json::to_string(&started).unwrap();
        assert!(json.contains("\"goal_run_id\":null"));
        assert!(json.contains("orchestrator"));
    }

    #[test]
    fn review_decision_with_feedback() {
        let sid = Uuid::new_v4();
        let did = Uuid::new_v4();
        let event = TaEvent::review_decision(sid, did, false, Some("Fix the auth module"));
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Fix the auth module"));
        assert!(json.contains("\"approved\":false"));
    }
}
