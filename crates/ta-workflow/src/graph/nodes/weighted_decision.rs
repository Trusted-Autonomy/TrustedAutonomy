// graph/nodes/weighted_decision.rs — `WeightedDecisionNode` (v0.17.7.1).
//
// A thin wrapper over `ta-workflow::consensus::run_consensus` — the
// difference from today's `governed_workflow.rs::stage_consensus` is that
// algorithm/threshold/weights come from the graph's TOML config, not
// hardcoded literals (`threshold=0.75`/`ConsensusAlgorithm::Raft`/empty
// weights at governed_workflow.rs:2494-2498).

use std::collections::HashMap;
use std::str::FromStr;

use crate::consensus::{run_consensus, ConsensusAlgorithm, ConsensusInput};
use crate::graph::schema::DecisionDef;
use crate::graph::types::{Decision, DecisionNode, GraphContext, GraphError, ReviewerVote};

pub struct WeightedDecisionNode {
    pub algorithm: ConsensusAlgorithm,
    pub threshold: f64,
    pub weights: HashMap<String, f64>,
    pub require_all: bool,
}

impl WeightedDecisionNode {
    pub fn from_def(def: &DecisionDef) -> Self {
        let algorithm = def
            .algorithm
            .as_deref()
            .and_then(|s| ConsensusAlgorithm::from_str(s).ok())
            .unwrap_or_default();
        Self {
            algorithm,
            threshold: def.threshold,
            weights: def.weights.clone(),
            require_all: def.require_all,
        }
    }
}

impl DecisionNode for WeightedDecisionNode {
    fn decide(&self, votes: &[ReviewerVote], ctx: &GraphContext) -> Result<Decision, GraphError> {
        let input = ConsensusInput {
            votes: votes.to_vec(),
            weights: self.weights.clone(),
            threshold: self.threshold,
            algorithm: self.algorithm.clone(),
            run_id: Some(ctx.run_id.clone()),
            run_dir: Some(ctx.run_dir.clone()),
            require_all: self.require_all,
            override_reason: None,
            audit_sink: Some(ctx.workspace_root.join(".ta").join("audit.jsonl")),
        };
        Ok(run_consensus(&input)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote(role: &str, score: f64) -> ReviewerVote {
        ReviewerVote {
            role: role.to_string(),
            score,
            findings: vec![],
            timed_out: false,
        }
    }

    #[test]
    fn threshold_and_weights_come_from_def_not_hardcoded() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = GraphContext {
            workspace_root: dir.path().to_path_buf(),
            run_id: "run-1".to_string(),
            run_dir: dir.path().to_path_buf(),
            vars: Default::default(),
        };
        let mut weights = HashMap::new();
        weights.insert("security".to_string(), 2.0);
        let def = DecisionDef {
            id: "d1".into(),
            kind: "weighted".into(),
            algorithm: Some("weighted".into()),
            threshold: 0.9,
            inputs: vec!["architect".into(), "security".into()],
            weights,
            require_all: false,
        };
        let node = WeightedDecisionNode::from_def(&def);
        let votes = vec![vote("architect", 0.9), vote("security", 0.5)];
        let result = node.decide(&votes, &ctx).unwrap();
        // weighted average = (0.9*1.0 + 0.5*2.0) / 3.0 = 1.9/3.0 = 0.6333
        assert!((result.score - 0.6333).abs() < 1e-3);
        assert!(!result.proceed, "0.63 < threshold 0.9");
    }

    #[test]
    fn low_threshold_proceeds_with_same_votes() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = GraphContext {
            workspace_root: dir.path().to_path_buf(),
            run_id: "run-2".to_string(),
            run_dir: dir.path().to_path_buf(),
            vars: Default::default(),
        };
        let def = DecisionDef {
            id: "d1".into(),
            kind: "weighted".into(),
            algorithm: None,
            threshold: 0.5,
            inputs: vec![],
            weights: HashMap::new(),
            require_all: false,
        };
        let node = WeightedDecisionNode::from_def(&def);
        let votes = vec![vote("policy", 0.8)];
        let result = node.decide(&votes, &ctx).unwrap();
        assert!(result.proceed);
    }
}
