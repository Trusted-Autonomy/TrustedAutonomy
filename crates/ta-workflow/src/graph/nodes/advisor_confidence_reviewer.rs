// graph/nodes/advisor_confidence_reviewer.rs — `AdvisorConfidenceReviewer`
// (v0.17.7.1).
//
// Wraps the existing `ta_decision::gate::decide()` confidence/risk gate as
// one scored `ReviewerVote`, per the reuse inventory in
// docs/superpowers/specs/2026-07-21-workflow-graph-engine-design.md §5.

use ta_decision::{decide, DecisionInput, DecisionThresholds};

use crate::graph::types::{GraphContext, GraphError, ReviewInput, ReviewerNode, ReviewerVote};

/// Runs `ta_decision::gate::decide()` against the caller-supplied
/// `DecisionThresholds` (defaults to `DecisionThresholds::default()` when
/// built via `NodeRegistry::with_builtins()`).
#[derive(Default)]
pub struct AdvisorConfidenceReviewer {
    thresholds: DecisionThresholds,
}

impl AdvisorConfidenceReviewer {
    pub fn with_thresholds(thresholds: DecisionThresholds) -> Self {
        Self { thresholds }
    }
}

impl ReviewerNode for AdvisorConfidenceReviewer {
    fn review(&self, input: &ReviewInput, _ctx: &GraphContext) -> Result<ReviewerVote, GraphError> {
        let decision_input = DecisionInput {
            verdict: input.verdict,
            risk_score: input.risk_score,
            confidence: input.confidence,
        };
        let gate_decision = decide(&decision_input, &self.thresholds);
        let score = if gate_decision.is_auto_approvable() {
            1.0
        } else {
            0.0
        };
        let finding = format!(
            "advisor_confidence: gate_decision={gate_decision:?} risk_score={} confidence={:.2}",
            input.risk_score, input.confidence
        );
        Ok(ReviewerVote {
            role: "advisor_confidence".to_string(),
            score,
            findings: vec![finding],
            timed_out: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_decision::Verdict;

    fn input(risk_score: u32, confidence: f64) -> ReviewInput {
        ReviewInput {
            verdict: Verdict::Pass,
            risk_score,
            confidence,
            ..Default::default()
        }
    }

    #[test]
    fn commit_decision_scores_one() {
        let ctx = GraphContext::new("/tmp", "run-1");
        let reviewer = AdvisorConfidenceReviewer::default();
        let vote = reviewer.review(&input(10, 0.95), &ctx).unwrap();
        assert_eq!(vote.score, 1.0);
        assert_eq!(vote.role, "advisor_confidence");
    }

    #[test]
    fn escalate_decision_scores_zero() {
        let ctx = GraphContext::new("/tmp", "run-1");
        let reviewer = AdvisorConfidenceReviewer::default();
        let vote = reviewer.review(&input(10, 0.1), &ctx).unwrap();
        assert_eq!(vote.score, 0.0);
    }

    #[test]
    fn custom_thresholds_change_the_outcome() {
        let ctx = GraphContext::new("/tmp", "run-1");
        let lenient = DecisionThresholds {
            min_confidence: 0.05,
            max_risk_score: 100,
            escalate_risk_score: 101,
        };
        let reviewer = AdvisorConfidenceReviewer::with_thresholds(lenient);
        let vote = reviewer.review(&input(90, 0.1), &ctx).unwrap();
        assert_eq!(vote.score, 1.0, "lenient thresholds should now commit");
    }
}
