// graph/mod.rs — Workflow Graph Engine Core (v0.17.7.1).
//
// A workflow graph is a set of typed nodes connected by typed edges,
// executed node-by-node with typed data on edges. Five node kinds
// (`TriggerSource`, `WorkerNode`, `ReviewerNode`, `DecisionNode`,
// `ActionNode`) cover both halves of a workflow — producing work and
// reviewing/deciding/acting on it. See
// docs/superpowers/specs/2026-07-21-workflow-graph-engine-design.md for the
// full design and docs/TA-CONSTITUTION.md §16 for the governing principles.
//
// Node kinds needing only what `ta-workflow` already depends on ship here
// (`nodes::PolicyReviewer`, `nodes::AdvisorConfidenceReviewer`,
// `nodes::WeightedDecisionNode`, registered by `NodeRegistry::with_builtins()`).
// Node kinds needing `ta-brain`/`ta-goal`/the real draft-apply path
// (`GoalDispatchAction`, `AutoApproveAction`, `RecommendAction`) are
// implemented in `apps/ta-cli`, which already depends on everything — see
// `registry.rs`'s module doc comment for the dependency-cycle reasoning.

pub mod engine;
pub mod nodes;
pub mod registry;
pub mod schema;
pub mod types;

pub use engine::{run_graph, GraphRunOutcome};
pub use registry::NodeRegistry;
pub use schema::{ActionDef, DecisionDef, GraphDefinition, NodeDef};
pub use types::{
    ActionNode, ActionOutcome, Decision, DecisionNode, GraphContext, GraphError, ReviewInput,
    ReviewerNode, ReviewerVote, TriggerPayload, TriggerSource, WorkItem, WorkResult, WorkerNode,
};
