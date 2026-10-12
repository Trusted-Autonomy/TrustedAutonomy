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
    /// Which daemon published this record — unused today (single-daemon
    /// scope), present so a future LAN/VPN multi-daemon transport doesn't
    /// need a schema migration. `None` means "the only daemon there is."
    #[serde(default)]
    pub host_id: Option<String>,
    /// The team session this agent belongs to. Stamped by the daemon from the
    /// verified session token, never taken from the client body (v0.17.12.2).
    /// `None` for records written before the field existed; such a record is
    /// visible to no session-filtered query and expires with its TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_session: Option<String>,
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
            host_id: None,
            team_session: None,
            last_heartbeat: Utc::now(),
        }
    }

    pub fn with_team_session(mut self, team_session: impl Into<String>) -> Self {
        self.team_session = Some(team_session.into());
        self
    }

    pub fn with_phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = Some(phase.into());
        self
    }

    pub fn with_resources(mut self, resources: Vec<String>) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_host_id(mut self, host_id: impl Into<String>) -> Self {
        self.host_id = Some(host_id.into());
        self
    }
}

/// Encode one key component so it contains only `[A-Za-z0-9-]` plus `=XX`
/// hex escapes. The NATS JetStream KV key character set is
/// `[-/_=.a-zA-Z0-9]`; keeping `_` out of every encoded component makes `_`
/// an unambiguous separator, so two different `(team_session, id)` pairs can
/// never produce the same key.
pub fn encode_key_component(component: &str) -> String {
    let mut out = String::with_capacity(component.len());
    for b in component.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("={b:02X}"));
        }
    }
    out
}

/// The one function that derives a session-scoped key from
/// `(team_session, id)`: `<encoded session>_<encoded id>`. Presence keys, task
/// ids and handoff routing all use it, so isolation cannot drift between them.
/// With no session the id is returned unchanged (the pre-isolation key).
pub fn session_scoped_key(team_session: Option<&str>, id: &str) -> String {
    match team_session {
        Some(session) => format!(
            "{}_{}",
            encode_key_component(session),
            encode_key_component(id)
        ),
        None => id.to_string(),
    }
}

/// The presence bucket key for `(team_session, agent_id)`.
pub fn presence_key(team_session: Option<&str>, agent_id: &str) -> String {
    session_scoped_key(team_session, agent_id)
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
    let key = presence_key(record.team_session.as_deref(), &record.agent_id);
    transport
        .kv_put(PRESENCE_BUCKET, &key, payload, Some(ttl))
        .await
}

/// Explicitly withdraw a presence record (e.g. on clean agent shutdown)
/// rather than waiting out the TTL.
pub async fn withdraw_presence(transport: &dyn WhiteboardTransport, agent_id: &str) -> Result<()> {
    withdraw_presence_in(transport, None, agent_id).await
}

/// Withdraw the presence record of `agent_id` inside `team_session`.
pub async fn withdraw_presence_in(
    transport: &dyn WhiteboardTransport,
    team_session: Option<&str>,
    agent_id: &str,
) -> Result<()> {
    transport
        .kv_delete(PRESENCE_BUCKET, &presence_key(team_session, agent_id))
        .await
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

    #[test]
    fn new_presence_record_has_no_host_id_by_default() {
        let record = PresenceRecord::new("agent-1", "goal-1", "/tmp/proj");
        assert_eq!(record.host_id, None);
    }

    #[test]
    fn with_host_id_sets_the_field() {
        let record = PresenceRecord::new("agent-1", "goal-1", "/tmp/proj").with_host_id("daemon-a");
        assert_eq!(record.host_id, Some("daemon-a".to_string()));
    }

    #[test]
    fn presence_record_without_host_id_still_deserializes() {
        // Backward compat: a record written before this field existed.
        let json = r#"{"agent_id":"a","goal_run_id":"g","source_dir":"/tmp","phase":null,"resources":[],"last_heartbeat":"2026-01-01T00:00:00Z"}"#;
        let record: PresenceRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.host_id, None);
    }

    #[test]
    fn key_components_never_collide_across_the_separator() {
        // Without escaping, ("a_", "b") and ("a", "_b") would both be a__b-ish.
        assert_ne!(
            session_scoped_key(Some("a_"), "b"),
            session_scoped_key(Some("a"), "_b")
        );
        assert_ne!(
            session_scoped_key(Some("a_b"), "c"),
            session_scoped_key(Some("a"), "b_c")
        );
        let k = session_scoped_key(Some("sess 1/x"), "agent.1");
        assert!(
            k.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=')),
            "{k}"
        );
        assert_eq!(k.matches('_').count(), 1, "{k}");
    }

    #[test]
    fn same_agent_id_in_two_sessions_gets_two_keys() {
        assert_ne!(
            presence_key(Some("s1"), "agent-1"),
            presence_key(Some("s2"), "agent-1")
        );
        assert_eq!(presence_key(None, "agent-1"), "agent-1");
    }

    #[test]
    fn a_record_without_team_session_deserializes_and_omits_the_field() {
        let old = r#"{"agent_id":"a","goal_run_id":"g","source_dir":"/x","last_heartbeat":"2026-01-01T00:00:00Z"}"#;
        let rec: PresenceRecord = serde_json::from_str(old).unwrap();
        assert_eq!(rec.team_session, None);
        let out = serde_json::to_string(&rec).unwrap();
        assert!(!out.contains("team_session"), "{out}");
    }
}
