//! Team-coordinator capability (v0.17.0.12.20).
//!
//! **Design decision**: the team coordinator is implemented as a new
//! capability on the *existing* Advisor (this module), not a new persistent
//! team role. Rationale, per `TA-CONSTITUTION.md` §1.7 ("Reuse Before
//! Reinventing") and `docs/design/ta-concepts-and-architecture.md` §3:
//!
//! - The coordinator's core job — "look at pending work, recommend
//!   next-actions, act autonomously only up to a configured trust level" —
//!   is exactly what `AdvisorSecurity` (`ta_session::workflow_session`)
//!   already models for the Advisor's other capabilities
//!   (`read_only`/`suggest`/`auto`). Reusing it here means "how autonomous
//!   should queue coordination be" is answered by the same config surface
//!   a human already understands, not a second trust-level concept.
//! - Introducing a new persistent `TeamRole` with its own runtime/daemon
//!   lifecycle would duplicate machinery the Advisor already has (a
//!   classifier, a security tri-state, an existing CLI/API surface) for no
//!   behavioral gain — the coordinator doesn't need its own identity as a
//!   team member; it needs to *read the queue and recommend*, which is a
//!   capability, not a role.
//!
//! What this module does NOT do: it never dispatches goals itself. It
//! builds a priority-ordered [`CoordinationReport`] from `ta-brain::route()`
//! and leaves the actual dispatch action (a process boundary — shelling to
//! `ta run`) to the CLI caller (`ta intake coordinate --dispatch`), exactly
//! as `AdvisorSecurity::Auto` elsewhere means "the advisor may act" without
//! the advisor library itself owning process execution.

use std::path::Path;

use ta_brain::{prioritize, RoutingDecision};
use ta_intake::TriggerEvent;
use ta_session::workflow_session::AdvisorSecurity;

use crate::classify::AMBIGUOUS_CONFIDENCE_THRESHOLD;

/// What the coordinator recommends doing with one queued event.
///
/// Replaces the earlier binary `auto_dispatch_eligible: bool` split
/// (v0.17.0.12.20) with a third outcome (v0.17.0.12.23): a routing decision
/// this low-confidence isn't safe to either fire automatically *or* silently
/// queue for later human review — it should ask a real clarifying question
/// now, the same `ta_ask_human`-backed mechanism `ta-advisor::pipeline` uses
/// for the free-text goal-creation entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendationOutcome {
    /// `decision.security_tier == AdvisorSecurity::Auto` at sufficient
    /// workload-classification confidence — the coordinator may dispatch
    /// this one without further human review.
    AutoEligible,
    /// Confidently classified, but not at `Auto` security — needs a human
    /// to review and promote via `ta intake fire`.
    NeedsReview,
    /// Workload-classification confidence is below
    /// `AMBIGUOUS_CONFIDENCE_THRESHOLD` — too low to trust either an
    /// autonomous dispatch or a silent "needs review" queue entry; the
    /// caller should fire a clarifying question before doing anything else.
    NeedsClarification,
}

/// One queued event, routed and ranked.
#[derive(Debug, Clone)]
pub struct CoordinatorRecommendation {
    pub event: TriggerEvent,
    pub decision: RoutingDecision,
    pub outcome: RecommendationOutcome,
}

impl CoordinatorRecommendation {
    /// `true` when `outcome == RecommendationOutcome::AutoEligible` — kept
    /// as a convenience accessor for callers that only care about the
    /// auto-dispatch bit, mirroring the field this replaces.
    pub fn auto_dispatch_eligible(&self) -> bool {
        self.outcome == RecommendationOutcome::AutoEligible
    }
}

/// A priority-ordered set of recommendations for `.ta/intake-queue.jsonl`'s
/// current contents (Observable & Actionable — every recommendation carries
/// its full `RoutingDecision.rationale`).
#[derive(Debug, Clone)]
pub struct CoordinationReport {
    pub recommendations: Vec<CoordinatorRecommendation>,
}

impl CoordinationReport {
    pub fn auto_eligible(&self) -> impl Iterator<Item = &CoordinatorRecommendation> {
        self.recommendations
            .iter()
            .filter(|r| r.outcome == RecommendationOutcome::AutoEligible)
    }

    pub fn needs_review(&self) -> impl Iterator<Item = &CoordinatorRecommendation> {
        self.recommendations
            .iter()
            .filter(|r| r.outcome == RecommendationOutcome::NeedsReview)
    }

    pub fn needs_clarification(&self) -> impl Iterator<Item = &CoordinatorRecommendation> {
        self.recommendations
            .iter()
            .filter(|r| r.outcome == RecommendationOutcome::NeedsClarification)
    }
}

/// Build a coordination report from the current `.ta/intake-queue.jsonl`
/// contents, read-only (Observable, no side effects — see module docs for
/// why dispatch/clarification are separate, caller-owned steps).
pub fn build_report(project_root: &Path) -> CoordinationReport {
    let events = ta_intake::read_queue(project_root);
    let routed = prioritize(&events, project_root);
    let recommendations = routed
        .into_iter()
        .map(|(event, decision)| {
            let outcome = classify_outcome(&decision);
            CoordinatorRecommendation {
                event,
                decision,
                outcome,
            }
        })
        .collect();
    CoordinationReport { recommendations }
}

fn classify_outcome(decision: &RoutingDecision) -> RecommendationOutcome {
    if decision.workload_confidence < AMBIGUOUS_CONFIDENCE_THRESHOLD {
        RecommendationOutcome::NeedsClarification
    } else if decision.security_tier == AdvisorSecurity::Auto {
        RecommendationOutcome::AutoEligible
    } else {
        RecommendationOutcome::NeedsReview
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn write_queued_event(project_root: &Path, title: &str) {
        let event = TriggerEvent {
            id: Uuid::new_v4(),
            trigger_type: "schedule".to_string(),
            source: "test".to_string(),
            occurred_at: Utc::now(),
            payload: serde_json::json!({}),
            suggested_goal_title: title.to_string(),
            dedupe_key: None,
        };
        ta_intake::append_to_queue(project_root, &[event]).unwrap();
    }

    #[test]
    fn empty_queue_yields_empty_report() {
        let tmp = tempfile::tempdir().unwrap();
        let report = build_report(tmp.path());
        assert!(report.recommendations.is_empty());
    }

    #[test]
    fn report_is_priority_ordered_and_partitions_by_security() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".ta")).unwrap();
        std::fs::write(
            tmp.path().join(".ta").join("workflow.toml"),
            "[security]\ndefault = \"auto\"\n",
        )
        .unwrap();
        write_queued_event(tmp.path(), "Update the README docs");
        write_queued_event(tmp.path(), "Production down, need a hotfix");

        let report = build_report(tmp.path());
        assert_eq!(report.recommendations.len(), 2);
        // Urgent (hotfix) sorts before Low (docs).
        assert_eq!(
            report.recommendations[0].event.suggested_goal_title,
            "Production down, need a hotfix"
        );
        // "auto" security default + high-confidence "bugfix"/"docs"
        // classification both clear the auto-eligibility confidence gate.
        assert_eq!(report.auto_eligible().count(), 2);
        assert_eq!(report.needs_review().count(), 0);
        assert_eq!(report.needs_clarification().count(), 0);
        assert!(report
            .recommendations
            .iter()
            .all(|r| r.outcome == RecommendationOutcome::AutoEligible));
        assert!(report
            .recommendations
            .iter()
            .all(|r| r.auto_dispatch_eligible()));
    }

    /// v0.17.0.12.23: a low-confidence event must not be silently folded
    /// into `needs_review` — it needs its own `needs_clarification` outcome
    /// so the caller knows to ask a real question rather than just queueing
    /// it for a human to eventually notice.
    #[test]
    fn low_confidence_event_needs_clarification() {
        let tmp = tempfile::tempdir().unwrap();
        // "xyzzy plugh" classifies as "other" at 0.3 confidence — below the
        // 0.65 threshold — regardless of default security tier.
        write_queued_event(tmp.path(), "xyzzy plugh");

        let report = build_report(tmp.path());
        assert_eq!(report.recommendations.len(), 1);
        assert_eq!(
            report.recommendations[0].outcome,
            RecommendationOutcome::NeedsClarification
        );
        assert_eq!(report.needs_clarification().count(), 1);
        assert_eq!(report.auto_eligible().count(), 0);
        assert_eq!(report.needs_review().count(), 0);
        assert!(!report.recommendations[0].auto_dispatch_eligible());
    }
}
