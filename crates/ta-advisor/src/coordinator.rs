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

/// One queued event, routed and ranked.
#[derive(Debug, Clone)]
pub struct CoordinatorRecommendation {
    pub event: TriggerEvent,
    pub decision: RoutingDecision,
    /// `true` when `decision.security_tier == AdvisorSecurity::Auto` — the
    /// coordinator may dispatch this one without further human review, the
    /// same meaning `AdvisorSecurity::Auto` already carries for the
    /// Advisor's other capabilities.
    pub auto_dispatch_eligible: bool,
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
            .filter(|r| r.auto_dispatch_eligible)
    }

    pub fn needs_review(&self) -> impl Iterator<Item = &CoordinatorRecommendation> {
        self.recommendations
            .iter()
            .filter(|r| !r.auto_dispatch_eligible)
    }
}

/// Build a coordination report from the current `.ta/intake-queue.jsonl`
/// contents, read-only (Observable, no side effects — see module docs for
/// why dispatch is a separate, caller-owned step).
pub fn build_report(project_root: &Path) -> CoordinationReport {
    let events = ta_intake::read_queue(project_root);
    let routed = prioritize(&events, project_root);
    let recommendations = routed
        .into_iter()
        .map(|(event, decision)| {
            let auto_dispatch_eligible = decision.security_tier == AdvisorSecurity::Auto;
            CoordinatorRecommendation {
                event,
                decision,
                auto_dispatch_eligible,
            }
        })
        .collect();
    CoordinationReport { recommendations }
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
    }
}
