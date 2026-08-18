// consensus/mod.rs — Multi-agent consensus algorithms for workflow review panels (v0.15.15).
//
// Three algorithms:
//   - Raft (default): crash-fault-tolerant, log-persisted, majority-quorum commit.
//   - Paxos: single-decree consensus, prepare/promise/accept/accepted phases.
//   - Weighted: simple weighted average, no coordination overhead.
//
// Auto-degrades to Weighted when only one reviewer is active.

pub mod paxos;
pub mod raft;
pub mod weighted;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── ConsensusAlgorithm ───────────────────────────────────────────────────────

/// Consensus algorithm used to aggregate multi-agent review scores.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConsensusAlgorithm {
    /// Raft: crash-fault-tolerant log replication. Majority quorum commits each
    /// reviewer's entry before computing the final weighted score. Session log
    /// persisted to `.ta/workflow-runs/<run-id>/raft.log`.
    #[default]
    Raft,
    /// Paxos: single-decree consensus. Suitable when only one round of agreement
    /// is needed and Raft's multi-round log would be unnecessary overhead.
    Paxos,
    /// Weighted threshold: simple weighted average with no coordination overhead.
    /// Used automatically when only one reviewer is active.
    Weighted,
}

impl std::fmt::Display for ConsensusAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsensusAlgorithm::Raft => write!(f, "raft"),
            ConsensusAlgorithm::Paxos => write!(f, "paxos"),
            ConsensusAlgorithm::Weighted => write!(f, "weighted"),
        }
    }
}

/// Parses the same lowercase names `Display` produces — used by
/// `graph::WeightedDecisionNode` to read a TOML `[decision] algorithm`
/// string (v0.17.7.1) rather than hardcoding an algorithm at each call site.
impl std::str::FromStr for ConsensusAlgorithm {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "raft" => Ok(ConsensusAlgorithm::Raft),
            "paxos" => Ok(ConsensusAlgorithm::Paxos),
            "weighted" => Ok(ConsensusAlgorithm::Weighted),
            other => Err(format!(
                "unknown consensus algorithm '{other}' (expected raft/paxos/weighted)"
            )),
        }
    }
}

// ── ReviewerVote ─────────────────────────────────────────────────────────────

/// A single reviewer's contribution to the consensus panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewerVote {
    /// Role identifier (e.g., "architect", "security", "principal", "pm").
    pub role: String,
    /// Score in the range 0.0–1.0.
    pub score: f64,
    /// Findings from this reviewer.
    #[serde(default)]
    pub findings: Vec<String>,
    /// True when this reviewer did not respond within the timeout window.
    #[serde(default)]
    pub timed_out: bool,
}

// ── ConsensusInput ───────────────────────────────────────────────────────────

/// All inputs required to run a consensus step.
#[derive(Debug, Clone)]
pub struct ConsensusInput {
    /// Votes from each reviewer (timed-out slots have `timed_out=true`).
    pub votes: Vec<ReviewerVote>,
    /// Per-role weights. Missing roles get weight 1.0.
    pub weights: HashMap<String, f64>,
    /// Minimum weighted score required to proceed (0.0–1.0).
    pub threshold: f64,
    /// Algorithm to use.
    pub algorithm: ConsensusAlgorithm,
    /// Unique run identifier (used for log file paths).
    pub run_id: String,
    /// Directory for persisted state (`.ta/workflow-runs/<run-id>/`).
    pub run_dir: PathBuf,
    /// If true, a timeout from any reviewer causes the run to fail rather than
    /// reducing the quorum.
    pub require_all: bool,
    /// When set, override any `proceed = false` decision with an audit entry.
    pub override_reason: Option<String>,
}

// ── ConsensusResult ──────────────────────────────────────────────────────────

/// Output of a consensus step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusResult {
    /// Weighted aggregate score (0.0–1.0).
    pub score: f64,
    /// True when `score >= threshold` (or override is active).
    pub proceed: bool,
    /// Algorithm that produced this result.
    pub algorithm_used: ConsensusAlgorithm,
    /// Per-role scores that were included in the quorum.
    pub scores_by_role: HashMap<String, f64>,
    /// Per-role findings included in the quorum.
    pub findings_by_role: HashMap<String, Vec<String>>,
    /// Roles that timed out and were excluded from the quorum.
    pub timed_out_roles: Vec<String>,
    /// True when the `override_reason` flag bypassed a `proceed = false` gate.
    pub override_active: bool,
    /// Human-readable summary line (e.g., "[Raft] score=0.81, proceed=true").
    pub summary: String,
}

// ── run_consensus ─────────────────────────────────────────────────────────────

/// Dispatch to the appropriate consensus algorithm.
///
/// Auto-degrades to `Weighted` when:
/// - The `algorithm` is `Raft` or `Paxos`, but there is only one non-timed-out reviewer.
pub fn run_consensus(input: &ConsensusInput) -> Result<ConsensusResult, crate::WorkflowError> {
    let active_votes: Vec<&ReviewerVote> = input.votes.iter().filter(|v| !v.timed_out).collect();

    // Degrade to Weighted for single-reviewer panels — no coordination overhead.
    let effective_algorithm =
        if active_votes.len() <= 1 && !matches!(input.algorithm, ConsensusAlgorithm::Weighted) {
            ConsensusAlgorithm::Weighted
        } else {
            input.algorithm.clone()
        };

    match effective_algorithm {
        ConsensusAlgorithm::Raft => raft::run(input),
        ConsensusAlgorithm::Paxos => paxos::run(input),
        ConsensusAlgorithm::Weighted => weighted::run(input),
    }
}

// ── weighted_average helper ───────────────────────────────────────────────────

/// Compute the weighted average of `scores`. Missing weights default to 1.0.
pub(crate) fn weighted_average(scores: &[(&str, f64)], weights: &HashMap<String, f64>) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    let mut total_score = 0.0_f64;
    let mut total_weight = 0.0_f64;
    for (role, score) in scores {
        let w = weights.get(*role).copied().unwrap_or(1.0);
        total_score += score * w;
        total_weight += w;
    }
    if total_weight == 0.0 {
        0.0
    } else {
        total_score / total_weight
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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

    fn timeout_vote(role: &str) -> ReviewerVote {
        ReviewerVote {
            role: role.to_string(),
            score: 0.0,
            findings: vec![],
            timed_out: true,
        }
    }

    #[test]
    fn algorithm_default_is_raft() {
        let algo: ConsensusAlgorithm = Default::default();
        assert_eq!(algo, ConsensusAlgorithm::Raft);
    }

    #[test]
    fn algorithm_display() {
        assert_eq!(ConsensusAlgorithm::Raft.to_string(), "raft");
        assert_eq!(ConsensusAlgorithm::Paxos.to_string(), "paxos");
        assert_eq!(ConsensusAlgorithm::Weighted.to_string(), "weighted");
    }

    #[test]
    fn algorithm_from_str_round_trips_display() {
        use std::str::FromStr;
        for variant in [
            ConsensusAlgorithm::Raft,
            ConsensusAlgorithm::Paxos,
            ConsensusAlgorithm::Weighted,
        ] {
            let parsed = ConsensusAlgorithm::from_str(&variant.to_string()).unwrap();
            assert_eq!(parsed, variant);
        }
        assert!(ConsensusAlgorithm::from_str("bogus").is_err());
    }

    #[test]
    fn algorithm_roundtrip_json() {
        for variant in [
            ConsensusAlgorithm::Raft,
            ConsensusAlgorithm::Paxos,
            ConsensusAlgorithm::Weighted,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let restored: ConsensusAlgorithm = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, restored);
        }
    }

    #[test]
    fn weighted_average_equal_weights() {
        let scores = vec![("a", 0.8), ("b", 0.6)];
        let weights = HashMap::new();
        let avg = weighted_average(&scores, &weights);
        assert!((avg - 0.7).abs() < 1e-9, "expected 0.7, got {avg}");
    }

    #[test]
    fn weighted_average_security_upweighted() {
        let scores = vec![("architect", 0.8_f64), ("security", 0.4_f64)];
        let mut weights = HashMap::new();
        weights.insert("security".to_string(), 1.5_f64);
        // total_weight = 1.0 + 1.5 = 2.5; total_score = 0.8 + 0.6 = 1.4; avg = 0.56
        let avg = weighted_average(&scores, &weights);
        assert!((avg - 0.56).abs() < 1e-9, "expected 0.56, got {avg}");
    }

    #[test]
    fn weighted_average_empty() {
        let avg = weighted_average(&[], &HashMap::new());
        assert_eq!(avg, 0.0);
    }

    #[test]
    fn single_reviewer_degrades_to_weighted() {
        let dir = tempfile::tempdir().unwrap();
        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Raft, // would normally use Raft
            run_id: "test-degrade-1".to_string(),
            run_dir: dir.path().to_path_buf(),
            require_all: false,
            override_reason: None,
        };
        let result = run_consensus(&input).unwrap();
        assert_eq!(result.algorithm_used, ConsensusAlgorithm::Weighted);
        assert!(result.proceed);
        assert!((result.score - 0.9).abs() < 1e-9);
    }

    #[test]
    fn single_reviewer_degrades_paxos_to_weighted() {
        let dir = tempfile::tempdir().unwrap();
        let input = ConsensusInput {
            votes: vec![vote("security", 0.5)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Paxos,
            run_id: "test-degrade-2".to_string(),
            run_dir: dir.path().to_path_buf(),
            require_all: false,
            override_reason: None,
        };
        let result = run_consensus(&input).unwrap();
        assert_eq!(result.algorithm_used, ConsensusAlgorithm::Weighted);
        assert!(!result.proceed); // 0.5 < 0.75
    }

    #[test]
    fn all_timed_out_degrades_to_weighted_zero() {
        let dir = tempfile::tempdir().unwrap();
        let input = ConsensusInput {
            votes: vec![timeout_vote("architect"), timeout_vote("security")],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Raft,
            run_id: "test-timeout-1".to_string(),
            run_dir: dir.path().to_path_buf(),
            require_all: false,
            override_reason: None,
        };
        // All timed out → 0 active votes → degrades to Weighted → score 0.0
        let result = run_consensus(&input).unwrap();
        assert!(!result.proceed);
        assert_eq!(result.timed_out_roles.len(), 2);
    }

    #[test]
    fn override_bypasses_block() {
        let dir = tempfile::tempdir().unwrap();
        let input = ConsensusInput {
            votes: vec![vote("architect", 0.3)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Weighted,
            run_id: "test-override-1".to_string(),
            run_dir: dir.path().to_path_buf(),
            require_all: false,
            override_reason: Some("emergency hotfix — approved by tech lead".to_string()),
        };
        let result = run_consensus(&input).unwrap();
        assert!(result.proceed, "override should force proceed=true");
        assert!(result.override_active);
        assert!(result.summary.contains("OVERRIDE"));
    }
}
