// graph/engine.rs — Executes a `GraphDefinition` node-by-node with typed
// data passed along edges (v0.17.7.1).
//
// v1 execution order (workers* -> reviewers* -> decision -> action) does not
// yet block on `[[trigger]]` sources — `ta workflow graph run <name>` is a
// synchronous "run now" entry point for testing/debugging a graph before
// it's wired into an event-driven flow (that wiring is 7.7.2/7.7.3). Trigger
// definitions still parse and validate; `TriggerSource::wait()` exists for a
// future caller (e.g. the daemon) to invoke before calling `run_graph`.

use tracing::info;

use super::registry::NodeRegistry;
use super::schema::GraphDefinition;
use super::types::{
    ActionOutcome, Decision, GraphContext, GraphError, ReviewInput, ReviewerVote, WorkItem,
    WorkResult,
};

/// Everything produced by one `run_graph` call, for callers (CLI output,
/// tests) to inspect — every stage that ran is Observable (per the project's
/// Observability Mandate).
#[derive(Debug, Default)]
pub struct GraphRunOutcome {
    pub work_results: Vec<WorkResult>,
    pub votes: Vec<ReviewerVote>,
    pub decision: Option<Decision>,
    pub action_outcome: Option<ActionOutcome>,
}

/// Run `def` end-to-end against `registry`, threading `ctx` through every
/// node. `work_item` seeds any `[[worker]]` nodes (ignored if the graph has
/// none); `review_input` seeds any `[[reviewer]]` nodes (ignored if the
/// graph has none).
pub fn run_graph(
    def: &GraphDefinition,
    registry: &NodeRegistry,
    work_item: &WorkItem,
    review_input: &ReviewInput,
    ctx: &mut GraphContext,
) -> Result<GraphRunOutcome, GraphError> {
    let mut outcome = GraphRunOutcome::default();

    for worker_def in &def.workers {
        info!(node_id = %worker_def.id, kind = %worker_def.kind, "graph: dispatching worker");
        let worker = registry.build_worker(worker_def)?;
        let result = worker
            .dispatch(work_item, ctx)
            .map_err(|e| wrap(&worker_def.id, e))?;
        ctx.vars
            .insert("draft_id".to_string(), result.draft_id.clone());
        outcome.work_results.push(result);
    }

    for reviewer_def in &def.reviewers {
        info!(node_id = %reviewer_def.id, kind = %reviewer_def.kind, "graph: running reviewer");
        let reviewer = registry.build_reviewer(reviewer_def)?;
        let mut vote = reviewer
            .review(review_input, ctx)
            .map_err(|e| wrap(&reviewer_def.id, e))?;
        // The reviewer *node id* (not its Rust-level role string) is what
        // `[decision].weights` keys against, per spec §3's
        // `weights = { policy_check = 1.0, ... }` example.
        vote.role = reviewer_def.id.clone();
        outcome.votes.push(vote);
    }

    if let Some(decision_def) = &def.decision {
        info!(node_id = %decision_def.id, kind = %decision_def.kind, "graph: running decision");
        let decision_node = registry.build_decision(decision_def)?;
        let inputs: Vec<ReviewerVote> = if decision_def.inputs.is_empty() {
            outcome.votes.clone()
        } else {
            outcome
                .votes
                .iter()
                .filter(|v| decision_def.inputs.contains(&v.role))
                .cloned()
                .collect()
        };
        let decision = decision_node
            .decide(&inputs, ctx)
            .map_err(|e| wrap(&decision_def.id, e))?;
        info!(
            node_id = %decision_def.id,
            score = decision.score,
            proceed = decision.proceed,
            "graph: decision reached"
        );
        outcome.decision = Some(decision);
    }

    if let Some(action_def) = &def.action {
        let decision = outcome.decision.as_ref().ok_or_else(|| {
            GraphError::InvalidDefinition(format!(
                "action '{}' requires a [decision] block but none ran",
                action_def.id
            ))
        })?;
        info!(node_id = %action_def.id, kind = %action_def.kind, "graph: running action");
        let action = registry.build_action(action_def)?;
        let result = action
            .act(decision, ctx)
            .map_err(|e| wrap(&action_def.id, e))?;
        info!(
            node_id = %action_def.id,
            applied = result.applied,
            message = %result.message,
            "graph: action complete"
        );
        outcome.action_outcome = Some(result);
    }

    Ok(outcome)
}

fn wrap(node_id: &str, err: GraphError) -> GraphError {
    match err {
        GraphError::NodeExecution { .. } => err,
        other => GraphError::NodeExecution {
            node_id: node_id.to_string(),
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::schema::GraphDefinition;

    fn ctx() -> (tempfile::TempDir, GraphContext) {
        let dir = tempfile::tempdir().unwrap();
        let context = GraphContext::new(dir.path(), "run-engine-test");
        (dir, context)
    }

    #[test]
    fn policy_reviewer_to_weighted_decision_to_recommend_round_trip() {
        let toml = r#"
[[reviewer]]
id = "policy_check"
kind = "policy"

[decision]
id = "panel_verdict"
kind = "weighted"
threshold = 0.5
inputs = ["policy_check"]

[action]
id = "outcome"
kind = "recommend"
decision = "panel_verdict"
"#;
        let def = GraphDefinition::from_toml_str(toml).unwrap();
        let mut registry = NodeRegistry::new();
        // Explicitly enable auto-approve so the round trip proceeds — the
        // registry's own default (`with_builtins()`'s bare `PolicyDocument`)
        // is deny-by-default per §1.3's Human-in-the-Loop default, exercised
        // separately in `nodes::policy_reviewer`'s tests.
        registry.register_reviewer("policy", |_def| {
            let mut doc = ta_policy::PolicyDocument::default();
            doc.defaults.auto_approve.drafts.enabled = true;
            Ok(
                Box::new(crate::graph::nodes::PolicyReviewer::with_document(doc))
                    as Box<dyn super::super::types::ReviewerNode>,
            )
        });
        registry.register_decision("weighted", |def| {
            Ok(
                Box::new(crate::graph::nodes::WeightedDecisionNode::from_def(def))
                    as Box<dyn super::super::types::DecisionNode>,
            )
        });
        registry.register_action("recommend", |_def| {
            Ok(Box::new(RecordingRecommend) as Box<dyn super::super::types::ActionNode>)
        });
        let (_dir, mut context) = ctx();
        let work_item = WorkItem::default();
        let review_input = ReviewInput {
            changed_paths: vec!["src/main.rs".to_string()],
            lines_changed: 5,
            agent_id: "claude-code".to_string(),
            ..Default::default()
        };

        let outcome = run_graph(&def, &registry, &work_item, &review_input, &mut context).unwrap();

        assert_eq!(outcome.votes.len(), 1);
        assert_eq!(outcome.votes[0].role, "policy_check");
        let decision = outcome.decision.unwrap();
        assert!(decision.proceed);
        let action_outcome = outcome.action_outcome.unwrap();
        assert_eq!(action_outcome.kind, "recommend");
        assert!(!action_outcome.applied, "recommend must never apply");
    }

    #[test]
    fn missing_kind_returns_node_not_found() {
        let toml = r#"
[[reviewer]]
id = "policy_check"
kind = "unregistered_kind"

[decision]
id = "d1"
inputs = ["policy_check"]
"#;
        let def = GraphDefinition::from_toml_str(toml).unwrap();
        let registry = NodeRegistry::with_builtins();
        let (_dir, mut context) = ctx();
        let err = run_graph(
            &def,
            &registry,
            &WorkItem::default(),
            &ReviewInput::default(),
            &mut context,
        )
        .unwrap_err();
        assert!(matches!(err, GraphError::NodeNotFound { .. }));
    }

    struct RecordingRecommend;
    impl super::super::types::ActionNode for RecordingRecommend {
        fn act(
            &self,
            decision: &Decision,
            _ctx: &GraphContext,
        ) -> Result<ActionOutcome, GraphError> {
            Ok(ActionOutcome {
                kind: "recommend".to_string(),
                applied: false,
                message: format!("recommended, score={:.2}", decision.score),
                metadata: Default::default(),
            })
        }
    }
}
