// graph/nodes/decision_reviewer.rs — `DecisionReviewerNode`.
//
// Wraps `ta_ask::ask()` as one scored `ReviewerVote`, filling the
// consensus-panel-reviewer and decision-gate-input call sites named in
// docs/superpowers/specs/2026-10-04-local-decision-model-primitive-design.md
// §1's table ("Does this diff look safe to commit?"). Registered opt-in via
// `NodeRegistry::register_decision_reviewer`, not `with_builtins()` -- see
// that method's doc comment for why.

use crate::graph::types::{GraphContext, GraphError, ReviewInput, ReviewerNode, ReviewerVote};

/// Asks a decision-model backend "does this diff look safe to commit?",
/// framed with the changed-paths/lines-changed/agent-id already carried on
/// `ReviewInput`, and converts the yes/no answer plus its calibrated
/// confidence into a `ReviewerVote`. `score` is the confidence in the
/// direction of the answer actually given: a confident "yes" scores near
/// 1.0, a confident "no" scores near 0.0 (i.e. `1.0 - confidence`), so
/// `WeightedDecisionNode`'s existing threshold logic can treat this vote
/// identically to any other reviewer's without new special-casing.
pub struct DecisionReviewerNode {
    backend: std::sync::Arc<dyn ta_ask::DecisionBackend>,
}

impl DecisionReviewerNode {
    pub fn new(backend: std::sync::Arc<dyn ta_ask::DecisionBackend>) -> Self {
        Self { backend }
    }
}

impl ReviewerNode for DecisionReviewerNode {
    fn review(&self, input: &ReviewInput, _ctx: &GraphContext) -> Result<ReviewerVote, GraphError> {
        let context = format!(
            "agent: {}\nchanged paths ({}): {}\nlines changed: {}",
            input.agent_id,
            input.changed_paths.len(),
            input.changed_paths.join(", "),
            input.lines_changed,
        );

        let response = ta_ask::ask(
            self.backend.as_ref(),
            "Does this diff look safe to commit?",
            context,
            ta_ask::DecisionSchema::YesNo,
        )
        .map_err(|e| GraphError::NodeExecution {
            node_id: "decision".to_string(),
            message: format!("ta-ask backend call failed: {}", e),
        })?;

        let is_safe = matches!(response.result, ta_ask::DecisionResult::Bool(true));
        let finding = format!(
            "decision_reviewer: model={} confidence={:.2} answer={}",
            response.model_id, response.confidence, is_safe
        );

        Ok(ReviewerVote {
            role: "decision".to_string(),
            score: if is_safe {
                response.confidence
            } else {
                1.0 - response.confidence
            },
            findings: vec![finding],
            timed_out: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_ask::{DecisionError, DecisionRequest, DecisionResponse, DecisionResult};

    struct YesBackend;
    impl ta_ask::DecisionBackend for YesBackend {
        fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
            Ok(DecisionResponse {
                result: DecisionResult::Bool(true),
                confidence: 0.92,
                model_id: "test-yes".to_string(),
                latency_ms: 1,
            })
        }
    }

    struct NoBackend;
    impl ta_ask::DecisionBackend for NoBackend {
        fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
            Ok(DecisionResponse {
                result: DecisionResult::Bool(false),
                confidence: 0.81,
                model_id: "test-no".to_string(),
                latency_ms: 1,
            })
        }
    }

    struct ErrorBackend;
    impl ta_ask::DecisionBackend for ErrorBackend {
        fn decide(&self, _req: &DecisionRequest) -> Result<DecisionResponse, DecisionError> {
            Err(DecisionError::MalformedResponse("boom".to_string()))
        }
    }

    fn input() -> crate::graph::types::ReviewInput {
        crate::graph::types::ReviewInput {
            changed_paths: vec!["src/main.rs".to_string()],
            lines_changed: 5,
            agent_id: "claude-code".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn confident_yes_scores_high() {
        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let node = DecisionReviewerNode::new(std::sync::Arc::new(YesBackend));
        let vote = node.review(&input(), &ctx).unwrap();
        assert_eq!(vote.role, "decision");
        assert_eq!(vote.score, 0.92);
    }

    #[test]
    fn confident_no_scores_low() {
        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let node = DecisionReviewerNode::new(std::sync::Arc::new(NoBackend));
        let vote = node.review(&input(), &ctx).unwrap();
        assert_eq!(vote.score, 1.0 - 0.81);
    }

    #[test]
    fn backend_error_produces_a_clear_graph_error() {
        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let node = DecisionReviewerNode::new(std::sync::Arc::new(ErrorBackend));
        let err = node.review(&input(), &ctx).unwrap_err();
        match err {
            crate::graph::types::GraphError::NodeExecution { node_id, message } => {
                assert_eq!(node_id, "decision");
                assert!(message.contains("boom"));
            }
            other => panic!("expected GraphError::NodeExecution, got {:?}", other),
        }
    }
}
