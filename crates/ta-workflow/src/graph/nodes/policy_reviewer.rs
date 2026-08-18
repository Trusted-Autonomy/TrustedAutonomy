// graph/nodes/policy_reviewer.rs — `PolicyReviewer` (v0.17.7.1).
//
// Wraps the existing `ta_policy::auto_approve::should_auto_approve_draft`
// rule evaluation as one scored `ReviewerVote`, per the reuse inventory in
// docs/superpowers/specs/2026-07-21-workflow-graph-engine-design.md §5.

use ta_policy::{
    auto_approve::should_auto_approve_draft, AutoApproveDecision, DraftInfo, PolicyDocument,
};

use crate::graph::types::{GraphContext, GraphError, ReviewInput, ReviewerNode, ReviewerVote};

/// Evaluates a draft against a `PolicyDocument`. Defaults to
/// `PolicyDocument::default()` when constructed via the registry's built-in
/// factory (`NodeRegistry::with_builtins()`); a caller that has already
/// loaded a project's real policy document can build one directly with
/// `PolicyReviewer::with_document(doc)` instead.
#[derive(Default)]
pub struct PolicyReviewer {
    document: PolicyDocument,
}

impl PolicyReviewer {
    pub fn with_document(document: PolicyDocument) -> Self {
        Self { document }
    }
}

impl ReviewerNode for PolicyReviewer {
    fn review(&self, input: &ReviewInput, _ctx: &GraphContext) -> Result<ReviewerVote, GraphError> {
        let draft = DraftInfo {
            changed_paths: input.changed_paths.clone(),
            lines_changed: input.lines_changed,
            plan_phase: input.plan_phase.clone(),
            agent_id: input.agent_id.clone(),
        };
        let decision = should_auto_approve_draft(&draft, &self.document);
        let (score, findings) = match decision {
            AutoApproveDecision::Approved { reasons } => (1.0, reasons),
            AutoApproveDecision::Denied { blockers } => (0.0, blockers),
        };
        Ok(ReviewerVote {
            role: "policy".to_string(),
            score,
            findings,
            timed_out: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(paths: &[&str]) -> ReviewInput {
        ReviewInput {
            changed_paths: paths.iter().map(|s| s.to_string()).collect(),
            lines_changed: 10,
            agent_id: "claude-code".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn defaults_to_denied_until_a_human_enables_auto_approve() {
        // PolicyDocument::default() has auto_approve.drafts.enabled = false —
        // mirrors §1.3's Human-in-the-Loop default: nothing auto-approves
        // until a human explicitly turns it on.
        let ctx = GraphContext::new("/tmp", "run-1");
        let reviewer = PolicyReviewer::default();
        let vote = reviewer.review(&input(&["src/main.rs"]), &ctx).unwrap();
        assert_eq!(vote.role, "policy");
        assert_eq!(vote.score, 0.0);
        assert!(!vote.timed_out);
    }

    #[test]
    fn approves_once_a_document_explicitly_enables_auto_approve() {
        let mut doc = PolicyDocument::default();
        doc.defaults.auto_approve.drafts.enabled = true;
        let ctx = GraphContext::new("/tmp", "run-1");
        let reviewer = PolicyReviewer::with_document(doc);
        let vote = reviewer.review(&input(&["src/main.rs"]), &ctx).unwrap();
        assert_eq!(vote.score, 1.0);
        assert!(!vote.findings.is_empty());
    }
}
