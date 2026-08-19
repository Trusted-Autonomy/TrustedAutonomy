// consensus/decision_bridge.rs — consensus-panel → decision-gate adapter (v0.17.10).
//
// Consensus and decision-gate are a pipeline, not two implementations of one
// interface: N reviewer votes -> one aggregated score (consensus-panel) ->
// policy thresholds -> a 4-way decision (decision-gate). This module is the
// bridge between them.
//
// This lives in-tree today because `consensus` and `ta-decision` both still
// live in the TA workspace. Once both are extracted as standalone crates
// (v0.17.10 Parts 1 and 4), this module is the literal candidate for
// `consensus-panel`'s optional `decision-gate` Cargo feature — it depends on
// `ta_decision`'s public API only, never on anything TA-specific, so the move
// is a file copy, not a rewrite.
//
// The adapter produces an *honest* `DecisionInput` only: it does not
// special-case outcomes. `verdict` is never `Block` — consensus (N humans or
// agents scoring a proposal) has no basis for the kind of hard veto `Block`
// represents in decision-gate's own vocabulary (that's reserved for signals
// like "the action violates an explicit policy", which consensus doesn't
// evaluate). Every decision — single-role, rule-matched, or panel — should
// terminate at the same `decide()` call with one vocabulary; this adapter
// must not become a side-channel around it.

use std::collections::HashMap;

use ta_decision::{Decision, DecisionInput, Verdict};

use super::ConsensusResult;

/// Tunable weights for how `ConsensusResult` maps to `DecisionInput`.
/// Configurable per caller — a general-purpose engine should not hardwire
/// one consumer's risk tolerance.
#[derive(Debug, Clone, Copy)]
pub struct ConsensusDecisionPolicy {
    /// Risk points (0-100 scale) added per timed-out reviewer.
    pub timeout_risk_weight: u32,
    /// Risk points (0-100 scale) contributed at maximum score variance
    /// across `scores_by_role` (variance is normalized to 0.0-1.0 before
    /// this weight is applied).
    pub variance_risk_weight: u32,
    /// Floor applied to `confidence` when `override_active` is set. An
    /// override reflects an informed decision to proceed despite the raw
    /// gate outcome, so the resulting confidence should not read as
    /// baselessly low even though the underlying score may have been.
    pub override_confidence_floor: f64,
}

impl Default for ConsensusDecisionPolicy {
    fn default() -> Self {
        Self {
            timeout_risk_weight: 15,
            variance_risk_weight: 40,
            override_confidence_floor: 0.5,
        }
    }
}

/// Map a completed consensus round to the one input `decision_gate::decide`
/// understands, per `policy`.
///
/// - `verdict`: `Pass` when the panel proceeded with no timeouts; `Warn`
///   otherwise (a low score, a timeout, or an override are all reasons to
///   want a human or a lower-trust path, not reasons to hard-block outright).
/// - `confidence`: `score` dampened by disagreement — the more the panel's
///   scores diverge, the less any single aggregate score should be trusted.
/// - `risk_score`: driven by how many reviewers didn't respond and by that
///   same disagreement, halved when an override is active (the override is
///   evidence the risk was already assessed and accepted by a human, not
///   evidence it disappeared).
pub fn to_decision_input(
    result: &ConsensusResult,
    policy: &ConsensusDecisionPolicy,
) -> DecisionInput {
    let verdict = if result.proceed && result.timed_out_roles.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Warn
    };

    let normalized_variance = normalized_variance(&result.scores_by_role);

    let mut confidence = (result.score * (1.0 - normalized_variance)).clamp(0.0, 1.0);
    if result.override_active {
        confidence = confidence.max(policy.override_confidence_floor);
    }

    let timeout_points = policy
        .timeout_risk_weight
        .saturating_mul(result.timed_out_roles.len() as u32);
    let variance_points =
        (f64::from(policy.variance_risk_weight) * normalized_variance).round() as u32;
    let mut risk_score = timeout_points.saturating_add(variance_points).min(100);
    if result.override_active {
        risk_score /= 2;
    }

    DecisionInput {
        verdict,
        risk_score,
        confidence,
    }
}

/// Convenience: run the full consensus -> decision-gate pipeline in one call.
pub fn to_decision(
    result: &ConsensusResult,
    policy: &ConsensusDecisionPolicy,
    thresholds: &ta_decision::DecisionThresholds,
) -> Decision {
    ta_decision::decide(&to_decision_input(result, policy), thresholds)
}

/// Population variance of `scores_by_role`'s values, normalized to 0.0-1.0.
/// Scores live in `[0.0, 1.0]`, so the maximum possible variance is `0.25`
/// (half the panel at 0.0, half at 1.0) — dividing by that ceiling maps the
/// raw variance onto a comparable 0.0-1.0 scale regardless of panel size.
fn normalized_variance(scores_by_role: &HashMap<String, f64>) -> f64 {
    if scores_by_role.len() < 2 {
        return 0.0;
    }
    let scores: Vec<f64> = scores_by_role.values().copied().collect();
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    let variance = scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / scores.len() as f64;
    (variance / 0.25).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::ConsensusAlgorithm;

    fn result(
        score: f64,
        proceed: bool,
        scores_by_role: &[(&str, f64)],
        timed_out_roles: &[&str],
        override_active: bool,
    ) -> ConsensusResult {
        ConsensusResult {
            score,
            proceed,
            algorithm_used: ConsensusAlgorithm::Weighted,
            scores_by_role: scores_by_role
                .iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect(),
            findings_by_role: HashMap::new(),
            timed_out_roles: timed_out_roles.iter().map(|s| s.to_string()).collect(),
            override_active,
            summary: "test".to_string(),
        }
    }

    #[test]
    fn clean_unanimous_panel_commits() {
        let r = result(
            0.95,
            true,
            &[("architect", 0.95), ("security", 0.95), ("principal", 0.95)],
            &[],
            false,
        );
        let policy = ConsensusDecisionPolicy::default();
        let thresholds = ta_decision::DecisionThresholds::default();
        let input = to_decision_input(&r, &policy);
        assert_eq!(input.verdict, Verdict::Pass);
        assert!(input.confidence > 0.9, "confidence={}", input.confidence);
        assert_eq!(input.risk_score, 0);
        assert_eq!(ta_decision::decide(&input, &thresholds), Decision::Commit);
    }

    #[test]
    fn split_high_variance_panel_does_not_commit() {
        // Same average score as a unanimous 0.55 panel, but here it's an
        // even split (0.95/0.15) — variance should tank confidence relative
        // to a genuinely unanimous panel at the same raw score.
        let split = result(
            0.55,
            false,
            &[("architect", 0.95), ("security", 0.15)],
            &[],
            false,
        );
        let unanimous = result(
            0.55,
            false,
            &[("architect", 0.55), ("security", 0.55)],
            &[],
            false,
        );
        let policy = ConsensusDecisionPolicy::default();
        let split_input = to_decision_input(&split, &policy);
        let unanimous_input = to_decision_input(&unanimous, &policy);
        assert!(
            split_input.confidence < unanimous_input.confidence,
            "split={} unanimous={}",
            split_input.confidence,
            unanimous_input.confidence
        );
        assert!(split_input.risk_score > unanimous_input.risk_score);
        assert_eq!(split_input.verdict, Verdict::Warn);

        let thresholds = ta_decision::DecisionThresholds::default();
        assert_ne!(
            ta_decision::decide(&split_input, &thresholds),
            Decision::Commit
        );
    }

    #[test]
    fn panel_with_timeouts_is_warn_never_block() {
        let r = result(
            0.9,
            true,
            &[("architect", 0.9), ("security", 0.9)],
            &["principal"],
            false,
        );
        let policy = ConsensusDecisionPolicy::default();
        let input = to_decision_input(&r, &policy);
        assert_eq!(
            input.verdict,
            Verdict::Warn,
            "a timeout must never escalate to Block"
        );
        assert!(input.risk_score > 0);
    }

    #[test]
    fn override_active_panel_lowers_risk_and_floors_confidence_but_still_gates() {
        // A low, blocked-then-overridden score: override should not zero out
        // risk or force max confidence — it should read as "assessed and
        // accepted", still subject to decide()'s own thresholds.
        let r = result(0.3, true, &[("architect", 0.3)], &[], true);
        let policy = ConsensusDecisionPolicy::default();
        let input = to_decision_input(&r, &policy);
        assert!(input.confidence >= policy.override_confidence_floor);
        // verdict tracks proceed+timeouts only (per spec), so an
        // override-forced proceed=true with no timeouts still reads as
        // Pass — it's confidence/risk that carry the "this was overridden"
        // signal, not verdict.
        assert_eq!(input.verdict, Verdict::Pass);
        let thresholds = ta_decision::DecisionThresholds::default();
        let decision = ta_decision::decide(&input, &thresholds);
        // Not asserting a specific outcome here beyond "not silently forced
        // to Commit" — that would defeat the point of routing through
        // decide() at all instead of special-casing override_active.
        assert_ne!(
            decision,
            Decision::Commit,
            "override alone must not bypass decide()'s own gate"
        );
    }

    #[test]
    fn single_reviewer_zero_variance() {
        let r = result(0.9, true, &[("architect", 0.9)], &[], false);
        let policy = ConsensusDecisionPolicy::default();
        let input = to_decision_input(&r, &policy);
        assert!((input.confidence - 0.9).abs() < 1e-9);
        assert_eq!(input.risk_score, 0);
    }

    #[test]
    fn to_decision_convenience_matches_manual_pipeline() {
        let r = result(0.95, true, &[("architect", 0.95)], &[], false);
        let policy = ConsensusDecisionPolicy::default();
        let thresholds = ta_decision::DecisionThresholds::default();
        let direct = ta_decision::decide(&to_decision_input(&r, &policy), &thresholds);
        let via_helper = to_decision(&r, &policy, &thresholds);
        assert_eq!(direct, via_helper);
    }
}
