//! Review/Decision gate for staged DB mutations (v0.17.0.12.15).
//!
//! Before this module, `DbProxyPlugin::apply_mutation` had no gate at all —
//! any staged mutation could be replayed against the real DB with no review
//! step, unlike every other Commit-contract implementation (VCS apply, social
//! publish). This builds that missing gate using the same shared
//! `ta-decision::decide()` function.
//!
//! Classification is rule-based (mutation kind + whether a pre-image is
//! known), not LLM-driven — DDL and deletions are inherently riskier than a
//! plain insert/update, and a missing `before` value means the reviewer can't
//! confirm what's being overwritten.

use ta_db_overlay::{OverlayEntry, OverlayEntryKind};
use ta_decision::{Decision, DecisionInput, DecisionThresholds, Verdict};

/// Classify a staged mutation into the shared Decision gate's input shape.
pub fn classify_for_review(entry: &OverlayEntry) -> DecisionInput {
    let (verdict, risk_score) = match entry.kind {
        OverlayEntryKind::Ddl => (Verdict::Warn, 80),
        OverlayEntryKind::Delete => (Verdict::Warn, 50),
        OverlayEntryKind::Update => (Verdict::Pass, 20),
        OverlayEntryKind::Insert => (Verdict::Pass, 10),
        OverlayEntryKind::Blob => (Verdict::Pass, 15),
    };
    // An Update/Delete without a captured pre-image means we can't confirm
    // what's being overwritten — treat that as a less confident review.
    let missing_pre_image = matches!(
        entry.kind,
        OverlayEntryKind::Update | OverlayEntryKind::Delete
    ) && entry.before.is_none();
    let confidence = if missing_pre_image { 0.5 } else { 0.95 };

    DecisionInput {
        verdict,
        risk_score,
        confidence,
    }
}

/// Review a staged mutation and return the Decision gate's routing.
///
/// `apply_mutation` must only be called when the returned `Decision` is
/// `is_auto_approvable()` — otherwise the mutation must go through a human
/// (`Rework`/`Escalate`) or be discarded (`Reject`), exactly like every other
/// Commit-contract endpoint.
pub fn review_mutation(entry: &OverlayEntry, thresholds: &DecisionThresholds) -> Decision {
    ta_decision::decide(&classify_for_review(entry), thresholds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn entry(kind: OverlayEntryKind, before: Option<serde_json::Value>) -> OverlayEntry {
        OverlayEntry {
            uri: "sqlite:///tmp/test.db/users/1".to_string(),
            before,
            after: json!({"name": "updated"}),
            ts: Utc::now(),
            kind,
        }
    }

    #[test]
    fn plain_insert_commits() {
        let e = entry(OverlayEntryKind::Insert, None);
        let decision = review_mutation(&e, &DecisionThresholds::default());
        assert_eq!(decision, Decision::Commit);
    }

    #[test]
    fn update_with_known_pre_image_commits() {
        let e = entry(OverlayEntryKind::Update, Some(json!({"name": "original"})));
        let decision = review_mutation(&e, &DecisionThresholds::default());
        assert_eq!(decision, Decision::Commit);
    }

    #[test]
    fn update_without_pre_image_does_not_auto_commit() {
        let e = entry(OverlayEntryKind::Update, None);
        let decision = review_mutation(&e, &DecisionThresholds::default());
        assert!(!decision.is_auto_approvable());
    }

    #[test]
    fn ddl_never_auto_commits() {
        let e = entry(OverlayEntryKind::Ddl, None);
        let decision = review_mutation(&e, &DecisionThresholds::default());
        assert!(!decision.is_auto_approvable());
    }

    #[test]
    fn delete_with_known_pre_image_still_reworks_not_commits() {
        // Deletions are inherently risky enough (Warn verdict) that they
        // never silently commit, even with a known pre-image.
        let e = entry(OverlayEntryKind::Delete, Some(json!({"name": "original"})));
        let decision = review_mutation(&e, &DecisionThresholds::default());
        assert!(!decision.is_auto_approvable());
    }

    #[test]
    fn same_entry_shape_produces_identical_decision_every_call() {
        let e1 = entry(OverlayEntryKind::Insert, None);
        let e2 = entry(OverlayEntryKind::Insert, None);
        let t = DecisionThresholds::default();
        assert_eq!(review_mutation(&e1, &t), review_mutation(&e2, &t));
    }
}
