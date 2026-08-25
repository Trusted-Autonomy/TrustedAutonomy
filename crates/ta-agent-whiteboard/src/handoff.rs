//! Durable handoff messages (v0.17.11.2 item 5) — a broadcastable,
//! peer-to-peer, durable generalization of both the existing single-
//! recipient `AgentAction::Escalate` and Claude Code's own non-durable
//! `SendMessage`. Recipient extends [`RoleRef`] from
//! `crates/ta-session/src/agent_action.rs` rather than inventing a second
//! addressing scheme.
//!
//! Durability comes straight from the transport's stream primitives: a
//! message survives the recipient being offline at send time, and is
//! redelivered until acknowledged (see `transport.rs`'s
//! `stream_read_next`/`stream_ack`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ta_session::RoleRef;

use crate::error::Result;
use crate::transport::WhiteboardTransport;

/// The handoff stream name for a given recipient. Each `RoleRef` gets its
/// own stream+consumer pair so a message addressed to `RoleRef::Agent("x")`
/// never competes for delivery with one addressed to `RoleRef::Role(...)`.
fn stream_for(recipient: &RoleRef) -> String {
    format!("handoff-{}", sanitize(&recipient.to_string()))
}

/// A single fixed consumer name per stream — handoff streams are
/// point-to-point (one logical recipient), not fanned out to multiple
/// independent readers, so one durable consumer per stream is sufficient.
const CONSUMER_NAME: &str = "primary";

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffMessage {
    pub sender: String,
    pub recipient: RoleRef,
    pub payload: String,
    pub created_at: DateTime<Utc>,
}

impl HandoffMessage {
    pub fn new(sender: impl Into<String>, recipient: RoleRef, payload: impl Into<String>) -> Self {
        Self {
            sender: sender.into(),
            recipient,
            payload: payload.into(),
            created_at: Utc::now(),
        }
    }
}

/// A delivered-but-unacknowledged message, with the opaque handle needed to
/// acknowledge it via [`ack_handoff`].
pub struct DeliveredHandoff {
    pub message: HandoffMessage,
    ack_handle: AckHandle,
}

struct AckHandle {
    stream: String,
    msg_id: String,
}

/// Send `message` — durable from the moment this returns, regardless of
/// whether the recipient is currently listening.
pub async fn send_handoff(
    transport: &dyn WhiteboardTransport,
    message: &HandoffMessage,
) -> Result<()> {
    let stream = stream_for(&message.recipient);
    let payload = serde_json::to_vec(message)?;
    transport.stream_append(&stream, payload).await
}

/// Poll for the next unacknowledged handoff addressed to `recipient`.
/// Returns `None` if there's nothing waiting right now — callers poll on
/// their own cadence (e.g. once per tool-loop turn).
pub async fn receive_handoff(
    transport: &dyn WhiteboardTransport,
    recipient: &RoleRef,
) -> Result<Option<DeliveredHandoff>> {
    let stream = stream_for(recipient);
    let Some(envelope) = transport.stream_read_next(&stream, CONSUMER_NAME).await? else {
        return Ok(None);
    };
    let message: HandoffMessage = serde_json::from_slice(&envelope.payload)?;
    Ok(Some(DeliveredHandoff {
        message,
        ack_handle: AckHandle {
            stream,
            msg_id: envelope.msg_id,
        },
    }))
}

/// Acknowledge a delivered handoff, so it is not redelivered.
pub async fn ack_handoff(
    transport: &dyn WhiteboardTransport,
    delivered: DeliveredHandoff,
) -> Result<()> {
    transport
        .stream_ack(
            &delivered.ack_handle.stream,
            CONSUMER_NAME,
            &delivered.ack_handle.msg_id,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_transport::InMemoryTransport;
    use ta_session::TeamRole;

    #[tokio::test]
    async fn handoff_survives_recipient_offline_at_send_time() {
        let t = InMemoryTransport::new();
        let recipient = RoleRef::Agent("agent-2".to_string());
        let msg = HandoffMessage::new("agent-1", recipient.clone(), "heads up, schema changed");
        send_handoff(&t, &msg).await.unwrap();

        // Recipient only starts listening well after the send.
        let delivered = receive_handoff(&t, &recipient).await.unwrap().unwrap();
        assert_eq!(delivered.message.payload, "heads up, schema changed");
    }

    #[tokio::test]
    async fn unacked_handoff_is_redelivered() {
        let t = InMemoryTransport::new();
        let recipient = RoleRef::Agent("agent-2".to_string());
        let msg = HandoffMessage::new("agent-1", recipient.clone(), "payload");
        send_handoff(&t, &msg).await.unwrap();

        let first = receive_handoff(&t, &recipient).await.unwrap().unwrap();
        // Not acked — the same message is still pending.
        let second = receive_handoff(&t, &recipient).await.unwrap().unwrap();
        assert_eq!(second.message.payload, first.message.payload);

        ack_handoff(&t, first).await.unwrap();
        assert!(receive_handoff(&t, &recipient).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn role_addressed_handoff_is_independent_of_agent_addressed() {
        let t = InMemoryTransport::new();
        let role_recipient = RoleRef::Role(TeamRole::reviewer());
        let agent_recipient = RoleRef::Agent("agent-2".to_string());

        send_handoff(
            &t,
            &HandoffMessage::new("agent-1", role_recipient.clone(), "for the reviewer"),
        )
        .await
        .unwrap();

        assert!(receive_handoff(&t, &agent_recipient)
            .await
            .unwrap()
            .is_none());
        let delivered = receive_handoff(&t, &role_recipient).await.unwrap().unwrap();
        assert_eq!(delivered.message.payload, "for the reviewer");
    }

    #[tokio::test]
    async fn receive_handoff_returns_none_when_nothing_pending() {
        let t = InMemoryTransport::new();
        let recipient = RoleRef::Agent("agent-2".to_string());
        assert!(receive_handoff(&t, &recipient).await.unwrap().is_none());
    }
}
