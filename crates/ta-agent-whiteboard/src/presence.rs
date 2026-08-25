//! Presence records — "what I'm doing right now" (v0.17.11.2 item 2).
//! TTL'd via the transport's bucket-wide expiry (see `transport.rs`'s
//! module doc): a crashed agent's presence disappears automatically once
//! its heartbeat stops, no manual cleanup required — unlike the already-known
//! orphaned-goal-record pattern (`ta draft close` not transitioning its
//! parent goal to a terminal state).
//!
//! Naming borrows "Agent Card"-style vocabulary from Google's A2A protocol
//! (capability + current task + status) without adopting A2A's wire
//! protocol — this transport is NATS, not A2A's HTTP/SSE/JSON-RPC.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::transport::WhiteboardTransport;

/// The KV bucket presence records live in.
pub const PRESENCE_BUCKET: &str = "wb_presence";

/// Default time-to-live for a presence entry if the publishing agent stops
/// heartbeating. Callers should re-publish well inside this window (e.g.
/// every 20s for a 60s TTL) to stay visible.
pub const DEFAULT_PRESENCE_TTL: Duration = Duration::from_secs(60);

/// An "Agent Card"-style activity advertisement: what an agent is doing
/// right now, published to the presence bucket keyed by `agent_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresenceRecord {
    pub agent_id: String,
    pub goal_run_id: String,
    pub source_dir: String,
    /// Current phase/stage label (free text — e.g. a PLAN.md phase ID or a
    /// team-session stage name), if the caller has one.
    #[serde(default)]
    pub phase: Option<String>,
    /// Resources being touched, in `task-graph`'s existing `api_impact`/
    /// file-glob vocabulary — deliberately reused rather than inventing a
    /// second one (see the design doc §6, "borrow vocabulary, not invent").
    #[serde(default)]
    pub resources: Vec<String>,
    pub last_heartbeat: DateTime<Utc>,
}

impl PresenceRecord {
    pub fn new(
        agent_id: impl Into<String>,
        goal_run_id: impl Into<String>,
        source_dir: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            goal_run_id: goal_run_id.into(),
            source_dir: source_dir.into(),
            phase: None,
            resources: Vec::new(),
            last_heartbeat: Utc::now(),
        }
    }

    pub fn with_phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = Some(phase.into());
        self
    }

    pub fn with_resources(mut self, resources: Vec<String>) -> Self {
        self.resources = resources;
        self
    }
}

/// Publish (or refresh) `record` in the presence bucket with `ttl`. Call
/// this on a heartbeat interval well inside `ttl` to stay visible.
pub async fn publish_presence(
    transport: &dyn WhiteboardTransport,
    record: &PresenceRecord,
    ttl: Duration,
) -> Result<()> {
    let mut record = record.clone();
    record.last_heartbeat = Utc::now();
    let payload = serde_json::to_vec(&record)?;
    transport
        .kv_put(PRESENCE_BUCKET, &record.agent_id, payload, Some(ttl))
        .await
}

/// Explicitly withdraw a presence record (e.g. on clean agent shutdown)
/// rather than waiting out the TTL.
pub async fn withdraw_presence(transport: &dyn WhiteboardTransport, agent_id: &str) -> Result<()> {
    transport.kv_delete(PRESENCE_BUCKET, agent_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_transport::InMemoryTransport;

    #[tokio::test]
    async fn publish_then_withdraw_round_trips() {
        let t = InMemoryTransport::new();
        let record = PresenceRecord::new("agent-1", "goal-1", "/repo").with_phase("v1.0");
        publish_presence(&t, &record, DEFAULT_PRESENCE_TTL)
            .await
            .unwrap();

        let raw = t.kv_get(PRESENCE_BUCKET, "agent-1").await.unwrap().unwrap();
        let fetched: PresenceRecord = serde_json::from_slice(&raw).unwrap();
        assert_eq!(fetched.agent_id, "agent-1");
        assert_eq!(fetched.phase.as_deref(), Some("v1.0"));

        withdraw_presence(&t, "agent-1").await.unwrap();
        assert!(t
            .kv_get(PRESENCE_BUCKET, "agent-1")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn presence_expires_without_manual_cleanup() {
        let t = InMemoryTransport::new();
        let record = PresenceRecord::new("crashed-agent", "goal-1", "/repo");
        publish_presence(&t, &record, Duration::from_millis(20))
            .await
            .unwrap();
        assert!(t
            .kv_get(PRESENCE_BUCKET, "crashed-agent")
            .await
            .unwrap()
            .is_some());
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(t
            .kv_get(PRESENCE_BUCKET, "crashed-agent")
            .await
            .unwrap()
            .is_none());
    }
}
