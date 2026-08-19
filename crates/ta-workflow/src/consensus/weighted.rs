// consensus/weighted.rs — Weighted threshold consensus (v0.15.15).
//
// Simple weighted average with no coordination overhead. Used when:
//   - `algorithm = "weighted"` is explicitly configured.
//   - Only one reviewer is active (auto-degraded from Raft or Paxos).
//   - All reviewers have timed out (score → 0.0, proceed = false).
//
// No log files, no round-trips — just math.

use std::collections::HashMap;

use super::{weighted_average, write_audit_entry, ConsensusError, ConsensusInput, ConsensusResult};

/// Run the weighted threshold consensus algorithm.
pub fn run(input: &ConsensusInput) -> Result<ConsensusResult, ConsensusError> {
    let active_votes: Vec<_> = input.votes.iter().filter(|v| !v.timed_out).collect();
    let timed_out_roles: Vec<String> = input
        .votes
        .iter()
        .filter(|v| v.timed_out)
        .map(|v| v.role.clone())
        .collect();

    let score_pairs: Vec<(&str, f64)> = active_votes
        .iter()
        .map(|v| (v.role.as_str(), v.score))
        .collect();

    let score = weighted_average(&score_pairs, &input.weights);

    let mut scores_by_role = HashMap::new();
    let mut findings_by_role: HashMap<String, Vec<String>> = HashMap::new();
    for vote in &active_votes {
        scores_by_role.insert(vote.role.clone(), vote.score);
        if !vote.findings.is_empty() {
            findings_by_role.insert(vote.role.clone(), vote.findings.clone());
        }
    }

    let proceed_raw = score >= input.threshold;
    let override_active = !proceed_raw && input.override_reason.is_some();
    let proceed = proceed_raw || override_active;

    let summary = build_summary(score, proceed, override_active, &timed_out_roles, input);

    if let Some(audit_sink) = &input.audit_sink {
        write_audit_entry(
            audit_sink,
            "weighted",
            input,
            score,
            proceed,
            override_active,
            &timed_out_roles,
            &scores_by_role,
        );
    }

    Ok(ConsensusResult {
        score,
        proceed,
        algorithm_used: super::ConsensusAlgorithm::Weighted,
        scores_by_role,
        findings_by_role,
        timed_out_roles,
        override_active,
        summary,
    })
}

fn build_summary(
    score: f64,
    proceed: bool,
    override_active: bool,
    timed_out_roles: &[String],
    input: &ConsensusInput,
) -> String {
    let mut parts = vec![format!(
        "[Weighted] score={:.2}, threshold={:.2}, proceed={}",
        score, input.threshold, proceed
    )];
    if !timed_out_roles.is_empty() {
        parts.push(format!("timed_out=[{}]", timed_out_roles.join(", ")));
    }
    if override_active {
        parts.push(format!(
            "OVERRIDE reason=\"{}\"",
            input.override_reason.as_deref().unwrap_or("")
        ));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::super::ReviewerVote;
    use super::*;

    fn vote(role: &str, score: f64) -> ReviewerVote {
        ReviewerVote {
            role: role.to_string(),
            score,
            findings: vec![],
            timed_out: false,
        }
    }

    fn timeout_vote(role: &str) -> ReviewerVote {
        ReviewerVote {
            role: role.to_string(),
            score: 0.0,
            findings: vec!["timeout".to_string()],
            timed_out: true,
        }
    }

    fn make_input(votes: Vec<ReviewerVote>, threshold: f64) -> ConsensusInput {
        ConsensusInput {
            votes,
            threshold,
            algorithm: super::super::ConsensusAlgorithm::Weighted,
            ..Default::default()
        }
    }

    #[test]
    fn equal_weights_above_threshold_proceeds() {
        let input = make_input(vec![vote("a", 0.8), vote("b", 0.9), vote("c", 0.85)], 0.75);
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert!((result.score - 0.85).abs() < 1e-9);
        assert_eq!(
            result.algorithm_used,
            super::super::ConsensusAlgorithm::Weighted
        );
        assert!(result.timed_out_roles.is_empty());
        assert!(!result.override_active);
    }

    #[test]
    fn below_threshold_does_not_proceed() {
        let input = make_input(vec![vote("a", 0.5), vote("b", 0.6)], 0.75);
        let result = run(&input).unwrap();
        assert!(!result.proceed);
        assert!((result.score - 0.55).abs() < 1e-9);
    }

    #[test]
    fn security_upweighted_blocks() {
        // architect=0.9 (w=1), security=0.3 (w=1.5)
        // total_weight=2.5; weighted_score = (0.9 + 0.45)/2.5 = 0.54
        let mut weights = HashMap::new();
        weights.insert("security".to_string(), 1.5_f64);
        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9), vote("security", 0.3)],
            weights,
            threshold: 0.75,
            algorithm: super::super::ConsensusAlgorithm::Weighted,
            ..Default::default()
        };
        let result = run(&input).unwrap();
        assert!(!result.proceed);
        assert!((result.score - 0.54).abs() < 1e-9);
    }

    #[test]
    fn timeout_slot_excluded_from_score() {
        let input = make_input(vec![vote("architect", 0.9), timeout_vote("security")], 0.75);
        let result = run(&input).unwrap();
        // Only "architect" score counts: 0.9 >= 0.75 → proceed
        assert!(result.proceed);
        assert!((result.score - 0.9).abs() < 1e-9);
        assert_eq!(result.timed_out_roles, vec!["security"]);
    }

    #[test]
    fn all_timed_out_score_zero_blocks() {
        let input = make_input(
            vec![timeout_vote("architect"), timeout_vote("security")],
            0.75,
        );
        let result = run(&input).unwrap();
        assert!(!result.proceed);
        assert_eq!(result.score, 0.0);
        assert_eq!(result.timed_out_roles.len(), 2);
    }

    #[test]
    fn override_bypasses_failed_gate() {
        let mut input = make_input(vec![vote("architect", 0.3)], 0.75);
        input.override_reason = Some("emergency hotfix".to_string());
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert!(result.override_active);
        assert!(result.summary.contains("OVERRIDE"));
    }

    #[test]
    fn override_not_set_when_proceeding_naturally() {
        let mut input = make_input(vec![vote("architect", 0.9)], 0.75);
        input.override_reason = Some("not needed".to_string());
        let result = run(&input).unwrap();
        assert!(result.proceed);
        // proceed_raw was true, so override_active = false (no bypass needed)
        assert!(!result.override_active);
    }

    #[test]
    fn findings_captured_per_role() {
        let mut v = vote("security", 0.6);
        v.findings = vec!["Finding A".to_string(), "Finding B".to_string()];
        let input = make_input(vec![v], 0.5);
        let result = run(&input).unwrap();
        let findings = result.findings_by_role.get("security").unwrap();
        assert_eq!(findings.len(), 2);
        assert!(findings.contains(&"Finding A".to_string()));
    }

    #[test]
    fn scores_by_role_populated() {
        let input = make_input(vec![vote("a", 0.7), vote("b", 0.8)], 0.75);
        let result = run(&input).unwrap();
        assert_eq!(result.scores_by_role.get("a"), Some(&0.7));
        assert_eq!(result.scores_by_role.get("b"), Some(&0.8));
    }

    #[test]
    fn summary_contains_algorithm_label() {
        let input = make_input(vec![vote("a", 0.8)], 0.75);
        let result = run(&input).unwrap();
        assert!(result.summary.starts_with("[Weighted]"));
    }
}
