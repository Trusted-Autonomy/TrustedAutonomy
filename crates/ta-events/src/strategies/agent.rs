// agent.rs -- Agent response strategy: build context for launching a goal from an event.
//
// This module does NOT import ta-goal or launch processes. It builds the
// context (prompt, event payload, attempt info) that the daemon uses to
// call `ta run` or launch a GoalRun.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::EventError;
use crate::router::RoutingDecision;
use crate::schema::EventEnvelope;

/// Context for launching an agent goal in response to an event.
///
/// The daemon inspects this struct to determine how to invoke `ta run`
/// or create a GoalRun with the appropriate context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponseContext {
    /// Agent name to use (e.g., "claude-code").
    pub agent_name: String,
    /// Prompt to inject as the agent's objective.
    pub prompt: String,
    /// The event type that triggered this response.
    pub event_type: String,
    /// Full event payload serialized as JSON for context injection.
    pub event_payload_json: String,
    /// Goal ID from the triggering event (if any).
    pub goal_id: Option<Uuid>,
    /// Plan phase from the triggering event (if any).
    pub phase: Option<String>,
    /// Whether the agent's output requires human approval.
    pub require_approval: bool,
    /// Current attempt number (0-based).
    pub attempt_number: u32,
    /// Maximum attempts before giving up.
    pub max_attempts: u32,
}

/// Build an `AgentResponseContext` from a routing decision and event envelope.
///
/// Returns an error if the decision lacks required agent fields (agent name).
pub fn build_agent_context(
    decision: &RoutingDecision,
    envelope: &EventEnvelope,
) -> Result<AgentResponseContext, EventError> {
    let agent_name = decision.agent.clone().ok_or_else(|| {
        EventError::RoutingConfig(format!(
            "agent strategy for event '{}' is missing the 'agent' field — \
             specify which agent to use in event-routing.yaml",
            decision.event_type
        ))
    })?;

    let prompt = decision.prompt.clone().unwrap_or_else(|| {
        format!(
            "An event of type '{}' occurred. Review the event context and take appropriate action.",
            decision.event_type
        )
    });

    let event_payload_json =
        serde_json::to_string_pretty(&envelope.payload).unwrap_or_else(|_| "{}".to_string());

    Ok(AgentResponseContext {
        agent_name,
        prompt,
        event_type: decision.event_type.clone(),
        event_payload_json,
        goal_id: envelope.payload.goal_id(),
        phase: envelope.payload.phase().map(|s| s.to_string()),
        require_approval: decision.require_approval,
        attempt_number: decision.attempt_number,
        max_attempts: decision.max_attempts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{EventRouter, Responder, ResponseStrategy, RoutingConfig};
    use crate::schema::SessionEvent;

    fn make_router_with_agent() -> EventRouter {
        let config = RoutingConfig {
            responders: vec![Responder {
                event: "goal_failed".into(),
                strategy: ResponseStrategy::Agent,
                filter: None,
                agent: Some("claude-code".into()),
                prompt: Some("Diagnose and fix the failure.".into()),
                require_approval: Some(true),
                escalate_after: None,
                max_attempts: None,
                workflow: None,
                channels: vec![],
            }],
            ..Default::default()
        };
        EventRouter::new(config).unwrap()
    }

    #[test]
    fn build_context_from_goal_failed() {
        let router = make_router_with_agent();
        let goal_id = Uuid::new_v4();
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id,
            error: "cargo test failed".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);
        let ctx = build_agent_context(&decision, &envelope).unwrap();

        assert_eq!(ctx.agent_name, "claude-code");
        assert_eq!(ctx.prompt, "Diagnose and fix the failure.");
        assert_eq!(ctx.goal_id, Some(goal_id));
        assert!(ctx.require_approval);
        assert_eq!(ctx.event_type, "goal_failed");
    }

    #[test]
    fn build_context_includes_event_json() {
        let router = make_router_with_agent();
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "build error: missing import".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);
        let ctx = build_agent_context(&decision, &envelope).unwrap();

        assert!(ctx.event_payload_json.contains("missing import"));
        assert!(ctx.event_payload_json.contains("goal_failed"));
    }

    #[test]
    fn build_context_respects_attempt_number() {
        let mut router = make_router_with_agent();
        let goal_id = Uuid::new_v4();

        router.record_attempt("goal_failed", Some(goal_id));
        router.record_attempt("goal_failed", Some(goal_id));

        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id,
            error: "err".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);
        let ctx = build_agent_context(&decision, &envelope).unwrap();

        assert_eq!(ctx.attempt_number, 2);
        assert_eq!(ctx.max_attempts, 3);
    }

    #[test]
    fn build_context_missing_agent_name_errors() {
        let config = RoutingConfig {
            responders: vec![Responder {
                event: "goal_failed".into(),
                strategy: ResponseStrategy::Agent,
                filter: None,
                agent: None, // missing!
                prompt: None,
                require_approval: None,
                escalate_after: None,
                max_attempts: None,
                workflow: None,
                channels: vec![],
            }],
            ..Default::default()
        };
        let router = EventRouter::new(config).unwrap();
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "err".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);
        let result = build_agent_context(&decision, &envelope);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("agent"));
    }
}
