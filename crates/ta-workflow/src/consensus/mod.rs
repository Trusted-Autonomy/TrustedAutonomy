// consensus/mod.rs — Multi-agent consensus algorithms for workflow review panels (v0.15.15).
//
// Three algorithms:
//   - Raft (default): crash-fault-tolerant, log-persisted, majority-quorum commit.
//   - Paxos: single-decree consensus, prepare/promise/accept/accepted phases.
//   - Weighted: simple weighted average, no coordination overhead.
//
// Auto-degrades to Weighted when only one reviewer is active.

pub mod decision_bridge;
pub mod paxos;
pub mod raft;
pub mod weighted;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── ConsensusError ───────────────────────────────────────────────────────────

/// Errors from running a consensus step. Local to the consensus engine —
/// deliberately not `ta-workflow`'s much larger `WorkflowError`, so this
/// module can be extracted as a standalone crate without dragging its host's
/// error surface along (mirrors `task-graph`'s own `WaveError`).
#[derive(Debug, thiserror::Error)]
pub enum ConsensusError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("serialization error: {0}")]
    Serde(String),
}

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
#[derive(Debug, Clone, Default)]
pub struct ConsensusInput {
    /// Votes from each reviewer (timed-out slots have `timed_out=true`).
    pub votes: Vec<ReviewerVote>,
    /// Per-role weights. Missing roles get weight 1.0.
    pub weights: HashMap<String, f64>,
    /// Minimum weighted score required to proceed (0.0–1.0).
    pub threshold: f64,
    /// Algorithm to use.
    pub algorithm: ConsensusAlgorithm,
    /// Unique run identifier, used to name Raft/Paxos log files. Only
    /// consulted when `run_dir` is also set — `Weighted` never touches
    /// either field, and Raft/Paxos run without crash-recovery persistence
    /// (in-memory only) when either is `None`.
    pub run_id: Option<String>,
    /// Directory for Raft/Paxos crash-recovery log files. See `run_id`.
    pub run_dir: Option<PathBuf>,
    /// If true, a timeout from any reviewer causes the run to fail rather than
    /// reducing the quorum.
    pub require_all: bool,
    /// When set, override any `proceed = false` decision with an audit entry.
    pub override_reason: Option<String>,
    /// Explicit, caller-supplied path to append a durable audit record to
    /// (one JSON line per run, plus a second line when an override fires).
    /// No write happens when `None` — audit logging is opt-in, not a
    /// hardcoded side effect of running consensus.
    pub audit_sink: Option<PathBuf>,
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
pub fn run_consensus(input: &ConsensusInput) -> Result<ConsensusResult, ConsensusError> {
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

// ── audit sink ────────────────────────────────────────────────────────────────

/// Append a `consensus_complete` record (and, when an override fired, a
/// second `consensus_override` record) to `audit_sink`. Best-effort: a
/// failure to write the audit trail must not fail the consensus decision
/// itself, so I/O errors here are swallowed rather than propagated.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_audit_entry(
    audit_sink: &Path,
    algorithm: &str,
    input: &ConsensusInput,
    score: f64,
    proceed: bool,
    override_active: bool,
    timed_out_roles: &[String],
    scores_by_role: &HashMap<String, f64>,
) {
    let scores_json: serde_json::Value = scores_by_role
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::from(*v)))
        .collect::<serde_json::Map<_, _>>()
        .into();

    let mut entry = serde_json::json!({
        "event": "consensus_complete",
        "algorithm": algorithm,
        "score": score,
        "proceed": proceed,
        "override_active": override_active,
        "timed_out_roles": timed_out_roles,
        "scores_by_role": scores_json,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(run_id) = &input.run_id {
        entry["run_id"] = serde_json::Value::String(run_id.clone());
    }
    if let Some(reason) = &input.override_reason {
        entry["override_reason"] = serde_json::Value::String(reason.clone());
    }

    if let Some(parent) = audit_sink.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_sink)
    {
        let _ = writeln!(f, "{}", entry);
    }

    if override_active {
        let override_entry = serde_json::json!({
            "event": "consensus_override",
            "run_id": input.run_id.clone().unwrap_or_default(),
            "reason": input.override_reason.as_deref().unwrap_or(""),
            "score_before_override": score,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(audit_sink)
        {
            let _ = writeln!(f, "{}", override_entry);
        }
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
            run_id: Some("test-degrade-1".to_string()),
            run_dir: Some(dir.path().to_path_buf()),
            require_all: false,
            override_reason: None,
            audit_sink: None,
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
            run_id: Some("test-degrade-2".to_string()),
            run_dir: Some(dir.path().to_path_buf()),
            require_all: false,
            override_reason: None,
            audit_sink: None,
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
            run_id: Some("test-timeout-1".to_string()),
            run_dir: Some(dir.path().to_path_buf()),
            require_all: false,
            override_reason: None,
            audit_sink: None,
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
            run_id: Some("test-override-1".to_string()),
            run_dir: Some(dir.path().to_path_buf()),
            require_all: false,
            override_reason: Some("emergency hotfix — approved by tech lead".to_string()),
            audit_sink: None,
        };
        let result = run_consensus(&input).unwrap();
        assert!(result.proceed, "override should force proceed=true");
        assert!(result.override_active);
        assert!(result.summary.contains("OVERRIDE"));
    }

    #[test]
    fn raft_and_paxos_run_without_persistence_when_run_dir_is_none() {
        // Item 7: run_dir/run_id are optional — Raft/Paxos must still compute
        // a correct result, just without crash-recovery log persistence.
        for algorithm in [ConsensusAlgorithm::Raft, ConsensusAlgorithm::Paxos] {
            let input = ConsensusInput {
                votes: vec![vote("architect", 0.9), vote("security", 0.8)],
                weights: HashMap::new(),
                threshold: 0.75,
                algorithm: algorithm.clone(),
                run_id: None,
                run_dir: None,
                require_all: false,
                override_reason: None,
                audit_sink: None,
            };
            let result = run_consensus(&input).unwrap();
            assert!(result.proceed, "{algorithm} should proceed with no run_dir");
            assert!((result.score - 0.85).abs() < 1e-9, "{algorithm}");
        }
    }

    #[test]
    fn audit_sink_is_opt_in_no_write_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Weighted,
            run_id: None,
            run_dir: None,
            require_all: false,
            override_reason: None,
            audit_sink: None,
        };
        run_consensus(&input).unwrap();
        // No audit_sink supplied — nothing in the tempdir should be written.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn audit_sink_writes_to_exact_caller_supplied_path() {
        let dir = tempfile::tempdir().unwrap();
        let audit_path = dir.path().join("nested").join("audit.jsonl");
        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Weighted,
            run_id: Some("audit-sink-test".to_string()),
            run_dir: None,
            require_all: false,
            override_reason: None,
            audit_sink: Some(audit_path.clone()),
        };
        run_consensus(&input).unwrap();
        assert!(audit_path.exists());
        let content = std::fs::read_to_string(&audit_path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["event"], "consensus_complete");
        assert_eq!(entry["run_id"], "audit-sink-test");
    }
}
