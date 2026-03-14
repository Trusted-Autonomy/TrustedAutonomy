// workflow.rs -- Workflow response strategy: build context for starting a workflow from an event.
//
// This module does NOT import ta-workflow or execute workflows. It builds the
// context (workflow name, input variables) that the daemon uses to start a
// workflow definition with event data.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::EventError;
use crate::router::RoutingDecision;
use crate::schema::{EventEnvelope, SessionEvent};

/// Context for starting a workflow in response to an event.
///
/// The daemon inspects this struct to find the workflow definition
/// and start it with the given input variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResponseContext {
    /// Workflow name to start (must match a `.ta/workflows/<name>.yaml` file).
    pub workflow_name: String,
    /// The event type that triggered this response.
    pub event_type: String,
    /// Input variables derived from the event, available for template expansion
    /// in workflow stage prompts.
    pub input_variables: HashMap<String, String>,
}

/// Build a `WorkflowResponseContext` from a routing decision and event envelope.
///
/// Returns an error if the decision lacks required workflow fields (workflow name).
pub fn build_workflow_context(
    decision: &RoutingDecision,
    envelope: &EventEnvelope,
) -> Result<WorkflowResponseContext, EventError> {
    let workflow_name = decision.workflow.clone().ok_or_else(|| {
        EventError::RoutingConfig(format!(
            "workflow strategy for event '{}' is missing the 'workflow' field — \
             specify which workflow to start in event-routing.yaml",
            decision.event_type
        ))
    })?;

    let mut vars = HashMap::new();
    vars.insert("event_type".into(), decision.event_type.clone());

    // Extract common fields from the event payload.
    if let Some(goal_id) = envelope.payload.goal_id() {
        vars.insert("goal_id".into(), goal_id.to_string());
    }
    if let Some(phase) = envelope.payload.phase() {
        vars.insert("phase".into(), phase.to_string());
    }

    // Extract error information from failure events.
    match &envelope.payload {
        SessionEvent::GoalFailed { error, .. } => {
            vars.insert("error".into(), error.clone());
        }
        SessionEvent::CommandFailed {
            command, stderr, ..
        } => {
            vars.insert("command".into(), command.clone());
            vars.insert("error".into(), stderr.clone());
        }
        SessionEvent::DraftDenied { reason, .. } => {
            vars.insert("reason".into(), reason.clone());
        }
        SessionEvent::SessionAborted { reason, .. } => {
            vars.insert("reason".into(), reason.clone());
        }
        _ => {}
    }

    // Include full event JSON for workflows that need the complete payload.
    let event_json = serde_json::to_string(&envelope.payload).unwrap_or_else(|_| "{}".to_string());
    vars.insert("event_json".into(), event_json);

    Ok(WorkflowResponseContext {
        workflow_name,
        event_type: decision.event_type.clone(),
        input_variables: vars,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{EventRouter, Responder, ResponseStrategy, RoutingConfig};
    use uuid::Uuid;

    fn make_workflow_router() -> EventRouter {
        let config = RoutingConfig {
            responders: vec![Responder {
                event: "goal_failed".into(),
                strategy: ResponseStrategy::Workflow,
                filter: None,
                agent: None,
                prompt: None,
                require_approval: None,
                escalate_after: None,
                max_attempts: None,
                workflow: Some("retry-pipeline".into()),
                channels: vec![],
            }],
            ..Default::default()
        };
        EventRouter::new(config).unwrap()
    }

    #[test]
    fn build_workflow_context_basic() {
        let router = make_workflow_router();
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "test failure".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);
        let ctx = build_workflow_context(&decision, &envelope).unwrap();

        assert_eq!(ctx.workflow_name, "retry-pipeline");
        assert_eq!(ctx.event_type, "goal_failed");
    }

    #[test]
    fn build_workflow_context_extracts_variables() {
        let router = make_workflow_router();
        let goal_id = Uuid::new_v4();
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id,
            error: "missing import".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);
        let ctx = build_workflow_context(&decision, &envelope).unwrap();

        assert_eq!(
            ctx.input_variables.get("goal_id").unwrap(),
            &goal_id.to_string()
        );
        assert_eq!(ctx.input_variables.get("error").unwrap(), "missing import");
        assert_eq!(
            ctx.input_variables.get("event_type").unwrap(),
            "goal_failed"
        );
    }

    #[test]
    fn build_workflow_context_includes_full_json() {
        let router = make_workflow_router();
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "build error".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);
        let ctx = build_workflow_context(&decision, &envelope).unwrap();

        let event_json = ctx.input_variables.get("event_json").unwrap();
        assert!(event_json.contains("build error"));
        assert!(event_json.contains("goal_failed"));
    }

    #[test]
    fn build_workflow_context_missing_workflow_name_errors() {
        let config = RoutingConfig {
            responders: vec![Responder {
                event: "goal_failed".into(),
                strategy: ResponseStrategy::Workflow,
                filter: None,
                agent: None,
                prompt: None,
                require_approval: None,
                escalate_after: None,
                max_attempts: None,
                workflow: None, // missing!
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
        let result = build_workflow_context(&decision, &envelope);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("workflow"));
    }

    #[test]
    fn build_workflow_context_command_failed() {
        let config = RoutingConfig {
            responders: vec![Responder {
                event: "command_failed".into(),
                strategy: ResponseStrategy::Workflow,
                filter: None,
                agent: None,
                prompt: None,
                require_approval: None,
                escalate_after: None,
                max_attempts: None,
                workflow: Some("fix-pipeline".into()),
                channels: vec![],
            }],
            ..Default::default()
        };
        let router = EventRouter::new(config).unwrap();
        let envelope = EventEnvelope::new(SessionEvent::CommandFailed {
            command: "cargo test".into(),
            exit_code: 1,
            stderr: "test failed: assertion error".into(),
        });
        let decision = router.route(&envelope);
        let ctx = build_workflow_context(&decision, &envelope).unwrap();

        assert_eq!(ctx.input_variables.get("command").unwrap(), "cargo test");
        assert_eq!(
            ctx.input_variables.get("error").unwrap(),
            "test failed: assertion error"
        );
    }
}
