// graph/registry.rs — Node-kind registry for the graph engine (v0.17.7.1).
//
// Maps a TOML `kind` string to a constructor for the matching trait object.
// Built-in kinds that only need what `ta-workflow` already depends on
// (`ta-policy`, `ta-decision`, the local `consensus` module) are registered
// by `NodeRegistry::with_builtins()`. Kinds needing heavier crates
// (`ta-brain`, `ta-goal` — `GoalDispatchAction`; the real draft-apply path —
// `AutoApproveAction`/`RecommendAction`) are registered by the caller
// (`apps/ta-cli`), which already depends on all of them — this keeps
// `ta-workflow` free of a dependency cycle (`ta-brain` already depends on
// `ta-workflow` for its template-matching signal).

use std::collections::HashMap;

use super::schema::{ActionDef, DecisionDef, NodeDef};
use super::types::{ActionNode, DecisionNode, GraphError, ReviewerNode, TriggerSource, WorkerNode};

type TriggerFactory =
    Box<dyn Fn(&NodeDef) -> Result<Box<dyn TriggerSource>, GraphError> + Send + Sync>;
type WorkerFactory = Box<dyn Fn(&NodeDef) -> Result<Box<dyn WorkerNode>, GraphError> + Send + Sync>;
type ReviewerFactory =
    Box<dyn Fn(&NodeDef) -> Result<Box<dyn ReviewerNode>, GraphError> + Send + Sync>;
type DecisionFactory =
    Box<dyn Fn(&DecisionDef) -> Result<Box<dyn DecisionNode>, GraphError> + Send + Sync>;
type ActionFactory =
    Box<dyn Fn(&ActionDef) -> Result<Box<dyn ActionNode>, GraphError> + Send + Sync>;

/// Registry of node-kind constructors, keyed by the TOML `kind` string.
#[derive(Default)]
pub struct NodeRegistry {
    triggers: HashMap<String, TriggerFactory>,
    workers: HashMap<String, WorkerFactory>,
    reviewers: HashMap<String, ReviewerFactory>,
    decisions: HashMap<String, DecisionFactory>,
    actions: HashMap<String, ActionFactory>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The three node kinds fully expressible with `ta-workflow`'s own
    /// dependency set: `policy` (`PolicyReviewer`), `advisor_confidence`
    /// (`AdvisorConfidenceReviewer`), and `weighted` (`WeightedDecisionNode`)
    /// — enough to express today's existing single-reviewer approval flow as
    /// a graph (v0.17.7.1's proof-of-abstraction scope).
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register_reviewer("policy", |_def| {
            Ok(Box::new(super::nodes::PolicyReviewer::default()) as Box<dyn ReviewerNode>)
        });
        registry.register_reviewer("advisor_confidence", |_def| {
            Ok(Box::new(super::nodes::AdvisorConfidenceReviewer::default())
                as Box<dyn ReviewerNode>)
        });
        registry.register_decision("weighted", |def| {
            Ok(Box::new(super::nodes::WeightedDecisionNode::from_def(def))
                as Box<dyn DecisionNode>)
        });
        registry
    }

    pub fn register_trigger(
        &mut self,
        kind: impl Into<String>,
        factory: impl Fn(&NodeDef) -> Result<Box<dyn TriggerSource>, GraphError> + Send + Sync + 'static,
    ) {
        self.triggers.insert(kind.into(), Box::new(factory));
    }

    pub fn register_worker(
        &mut self,
        kind: impl Into<String>,
        factory: impl Fn(&NodeDef) -> Result<Box<dyn WorkerNode>, GraphError> + Send + Sync + 'static,
    ) {
        self.workers.insert(kind.into(), Box::new(factory));
    }

    pub fn register_reviewer(
        &mut self,
        kind: impl Into<String>,
        factory: impl Fn(&NodeDef) -> Result<Box<dyn ReviewerNode>, GraphError> + Send + Sync + 'static,
    ) {
        self.reviewers.insert(kind.into(), Box::new(factory));
    }

    /// Registers `"decision"` → `DecisionReviewerNode`, backed by `backend`.
    /// Deliberately NOT part of `with_builtins()` -- unlike `advisor_confidence`/
    /// `policy`, this needs a live `typed_ask::DecisionBackend` instance (typically
    /// a `DeciderBackend` managing a real subprocess), which a caller must
    /// construct and own; `ta-workflow` itself has no opinion on which backend
    /// or how it's configured. Call this explicitly, after `with_builtins()`,
    /// once a backend is available. Takes an `Arc` (not `Box`) because the
    /// underlying `Fn` factory closure may be invoked more than once (once per
    /// graph run resolving this node kind) and `DecisionBackend` isn't `Clone`
    /// (a `DeciderBackend` owns an unclonable child-process handle) -- `Arc`
    /// lets each invocation cheaply clone a shared reference to the same
    /// backend instance instead.
    pub fn register_decision_reviewer(
        &mut self,
        backend: std::sync::Arc<dyn typed_ask::DecisionBackend>,
    ) {
        self.register_reviewer("decision", move |_def| {
            Ok(
                Box::new(super::nodes::DecisionReviewerNode::new(backend.clone()))
                    as Box<dyn ReviewerNode>,
            )
        });
    }

    pub fn register_decision(
        &mut self,
        kind: impl Into<String>,
        factory: impl Fn(&DecisionDef) -> Result<Box<dyn DecisionNode>, GraphError>
            + Send
            + Sync
            + 'static,
    ) {
        self.decisions.insert(kind.into(), Box::new(factory));
    }

    pub fn register_action(
        &mut self,
        kind: impl Into<String>,
        factory: impl Fn(&ActionDef) -> Result<Box<dyn ActionNode>, GraphError> + Send + Sync + 'static,
    ) {
        self.actions.insert(kind.into(), Box::new(factory));
    }

    pub fn build_trigger(&self, def: &NodeDef) -> Result<Box<dyn TriggerSource>, GraphError> {
        let factory = self
            .triggers
            .get(&def.kind)
            .ok_or_else(|| GraphError::NodeNotFound {
                category: "trigger",
                kind: def.kind.clone(),
            })?;
        factory(def)
    }

    pub fn build_worker(&self, def: &NodeDef) -> Result<Box<dyn WorkerNode>, GraphError> {
        let factory = self
            .workers
            .get(&def.kind)
            .ok_or_else(|| GraphError::NodeNotFound {
                category: "worker",
                kind: def.kind.clone(),
            })?;
        factory(def)
    }

    pub fn build_reviewer(&self, def: &NodeDef) -> Result<Box<dyn ReviewerNode>, GraphError> {
        let factory = self
            .reviewers
            .get(&def.kind)
            .ok_or_else(|| GraphError::NodeNotFound {
                category: "reviewer",
                kind: def.kind.clone(),
            })?;
        factory(def)
    }

    pub fn build_decision(&self, def: &DecisionDef) -> Result<Box<dyn DecisionNode>, GraphError> {
        let factory = self
            .decisions
            .get(&def.kind)
            .ok_or_else(|| GraphError::NodeNotFound {
                category: "decision",
                kind: def.kind.clone(),
            })?;
        factory(def)
    }

    pub fn build_action(&self, def: &ActionDef) -> Result<Box<dyn ActionNode>, GraphError> {
        let factory = self
            .actions
            .get(&def.kind)
            .ok_or_else(|| GraphError::NodeNotFound {
                category: "action",
                kind: def.kind.clone(),
            })?;
        factory(def)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_builtins_registers_policy_advisor_and_weighted() {
        let registry = NodeRegistry::with_builtins();
        let reviewer_def = NodeDef {
            id: "r1".into(),
            kind: "policy".into(),
            params: Default::default(),
        };
        assert!(registry.build_reviewer(&reviewer_def).is_ok());

        let advisor_def = NodeDef {
            id: "r2".into(),
            kind: "advisor_confidence".into(),
            params: Default::default(),
        };
        assert!(registry.build_reviewer(&advisor_def).is_ok());

        let decision_def = DecisionDef {
            id: "d1".into(),
            kind: "weighted".into(),
            algorithm: None,
            threshold: 0.75,
            inputs: vec![],
            weights: Default::default(),
            require_all: false,
        };
        assert!(registry.build_decision(&decision_def).is_ok());
    }

    #[test]
    fn unknown_kind_returns_node_not_found() {
        let registry = NodeRegistry::with_builtins();
        let def = NodeDef {
            id: "r1".into(),
            kind: "does_not_exist".into(),
            params: Default::default(),
        };
        let err = registry.build_reviewer(&def).map(|_| ()).unwrap_err();
        assert!(matches!(
            err,
            GraphError::NodeNotFound {
                category: "reviewer",
                ..
            }
        ));
    }

    #[test]
    fn caller_can_register_additional_action_kinds() {
        struct Noop;
        impl ActionNode for Noop {
            fn act(
                &self,
                _decision: &super::super::types::Decision,
                _ctx: &super::super::types::GraphContext,
            ) -> Result<super::super::types::ActionOutcome, GraphError> {
                Ok(super::super::types::ActionOutcome {
                    kind: "noop".into(),
                    applied: false,
                    message: "noop".into(),
                    metadata: Default::default(),
                })
            }
        }
        let mut registry = NodeRegistry::with_builtins();
        registry.register_action("noop", |_def| Ok(Box::new(Noop) as Box<dyn ActionNode>));
        let def = ActionDef {
            id: "a1".into(),
            kind: "noop".into(),
            decision: None,
            params: Default::default(),
        };
        assert!(registry.build_action(&def).is_ok());
    }

    #[test]
    fn register_decision_reviewer_adds_the_decision_kind() {
        struct AlwaysYes;
        impl typed_ask::DecisionBackend for AlwaysYes {
            fn decide(
                &self,
                _req: &typed_ask::DecisionRequest,
            ) -> Result<typed_ask::DecisionResponse, typed_ask::DecisionError> {
                Ok(typed_ask::DecisionResponse {
                    result: typed_ask::DecisionResult::Bool(true),
                    confidence: 1.0,
                    model_id: "always-yes".to_string(),
                    latency_ms: 0,
                })
            }
        }

        let mut registry = NodeRegistry::with_builtins();
        registry.register_decision_reviewer(std::sync::Arc::new(AlwaysYes));

        let reviewer_def = NodeDef {
            id: "decision".into(),
            kind: "decision".into(),
            params: Default::default(),
        };
        let reviewer = registry
            .build_reviewer(&reviewer_def)
            .expect("factory should succeed");

        let ctx = crate::graph::types::GraphContext::new("/tmp", "run-1");
        let vote = reviewer
            .review(&crate::graph::types::ReviewInput::default(), &ctx)
            .unwrap();
        assert_eq!(vote.role, "decision");
        assert_eq!(vote.score, 1.0);
    }
}
