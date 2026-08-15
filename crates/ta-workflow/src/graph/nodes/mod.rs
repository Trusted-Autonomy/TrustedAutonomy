// graph/nodes/mod.rs — Built-in node implementations shippable from
// `ta-workflow` itself (v0.17.7.1). `GoalDispatchAction`, `AutoApproveAction`,
// and `RecommendAction` need `ta-brain`/`ta-goal`/the real draft-apply path
// and are therefore implemented one layer up, in `apps/ta-cli`, and
// registered into a `NodeRegistry` alongside these built-ins — see
// `graph/registry.rs`'s module doc comment for why.

mod advisor_confidence_reviewer;
mod policy_reviewer;
mod weighted_decision;

pub use advisor_confidence_reviewer::AdvisorConfidenceReviewer;
pub use policy_reviewer::PolicyReviewer;
pub use weighted_decision::WeightedDecisionNode;
