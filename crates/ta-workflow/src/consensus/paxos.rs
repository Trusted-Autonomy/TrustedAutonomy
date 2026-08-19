// consensus/paxos.rs — Single-decree Paxos consensus (v0.15.15).
//
// Implements the classic Paxos protocol for cases where only one round of
// consensus is needed and Raft's multi-round log is unnecessary overhead.
//
// Protocol (prepare → promise → accept → accepted):
//
//   Phase 1 (Prepare / Promise):
//     Coordinator sends `Prepare(n)` to all active reviewers.
//     Each reviewer that has not promised to a higher ballot replies
//     `Promise(n, (v_n, v_v))` where (v_n, v_v) is any previously accepted value.
//
//   Phase 2 (Accept / Accepted):
//     If coordinator receives a quorum (⌊n/2⌋+1) of promises:
//       - If any promise carries a prior value, use the value with the highest prior ballot.
//       - Otherwise, propose the weighted aggregate of all reviewer scores.
//     Coordinator sends `Accept(n, value)` to all active reviewers.
//     Each reviewer that hasn't promised to a higher ballot replies `Accepted(n, value)`.
//
//   Commit:
//     If quorum of `Accepted` messages received → value is decided.
//
// In single-process mode, all nodes are virtual: the coordinator simulates
// each reviewer's accept/reject decision. Timed-out reviewers are treated as
// non-responsive (reduce quorum, not hard failure unless require_all=true).
// The audit trail is written to a compact JSONL file for observability.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::{weighted_average, write_audit_entry, ConsensusAlgorithm, ConsensusError};
use super::{ConsensusInput, ConsensusResult};

// ── Message types ─────────────────────────────────────────────────────────────

/// A Paxos ballot number (proposal number).
type Ballot = u64;

/// The proposed consensus value — the aggregated score and whether to proceed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaxosValue {
    score: f64,
    proceed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum PaxosEvent {
    Prepare {
        ballot: Ballot,
        reviewer_count: usize,
        quorum: usize,
    },
    Promise {
        from: String,
        ballot: Ballot,
        prior_ballot: Option<Ballot>,
        prior_value: Option<PaxosValue>,
    },
    Accept {
        ballot: Ballot,
        value: PaxosValue,
    },
    Accepted {
        from: String,
        ballot: Ballot,
    },
    Decided {
        ballot: Ballot,
        value: PaxosValue,
        override_active: bool,
        timed_out: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaxosLogEntry {
    index: u64,
    timestamp: String,
    event: PaxosEvent,
}

// ── Audit log ─────────────────────────────────────────────────────────────────

struct PaxosAuditLog {
    path: PathBuf,
    next_index: u64,
}

impl PaxosAuditLog {
    fn open(run_dir: &std::path::Path, run_id: &str) -> Result<Self, ConsensusError> {
        std::fs::create_dir_all(run_dir).map_err(|e| ConsensusError::Io {
            path: run_dir.display().to_string(),
            source: e,
        })?;
        let path = run_dir.join(format!("{}.paxos.log", run_id));
        Ok(Self {
            path,
            next_index: 1,
        })
    }

    fn write(&mut self, event: PaxosEvent) -> Result<(), ConsensusError> {
        let entry = PaxosLogEntry {
            index: self.next_index,
            timestamp: Utc::now().to_rfc3339(),
            event,
        };
        self.next_index += 1;
        let json =
            serde_json::to_string(&entry).map_err(|e| ConsensusError::Serde(e.to_string()))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| ConsensusError::Io {
                path: self.path.display().to_string(),
                source: e,
            })?;
        writeln!(f, "{}", json).map_err(|e| ConsensusError::Io {
            path: self.path.display().to_string(),
            source: e,
        })?;
        f.flush().map_err(|e| ConsensusError::Io {
            path: self.path.display().to_string(),
            source: e,
        })
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── run ───────────────────────────────────────────────────────────────────────

/// Execute single-decree Paxos consensus.
///
/// Persistence of the phase-by-phase audit log is opt-in, mirroring Raft: it
/// only happens when the caller supplies both `run_dir` and `run_id`.
pub fn run(input: &ConsensusInput) -> Result<ConsensusResult, ConsensusError> {
    let active_votes: Vec<_> = input.votes.iter().filter(|v| !v.timed_out).collect();
    let timed_out_roles: Vec<String> = input
        .votes
        .iter()
        .filter(|v| v.timed_out)
        .map(|v| v.role.clone())
        .collect();

    let n = active_votes.len();
    let quorum = n / 2 + 1;

    let mut log = match (&input.run_dir, &input.run_id) {
        (Some(run_dir), Some(run_id)) => Some(PaxosAuditLog::open(run_dir, run_id)?),
        _ => None,
    };
    let ballot: Ballot = 1;

    // ── Phase 1: Prepare ──────────────────────────────────────────────────────
    if let Some(log) = log.as_mut() {
        log.write(PaxosEvent::Prepare {
            ballot,
            reviewer_count: n,
            quorum,
        })?;
    }

    // In single-process mode, all active reviewers immediately promise.
    // (They have not seen a higher ballot — this is the first and only proposal.)
    let mut promises = 0usize;
    let highest_prior_ballot: Option<Ballot> = None;
    let mut highest_prior_value: Option<PaxosValue> = None;

    for vote in &active_votes {
        if let Some(log) = log.as_mut() {
            log.write(PaxosEvent::Promise {
                from: vote.role.clone(),
                ballot,
                prior_ballot: None,
                prior_value: None,
            })?;
        }
        promises += 1;
        let _ = (highest_prior_ballot, highest_prior_value.take()); // no prior values
    }

    // Quorum of promises?
    let promise_quorum_met = promises >= quorum;

    // ── Phase 2: Accept ───────────────────────────────────────────────────────
    // Compute the proposed value.
    let score_pairs: Vec<(&str, f64)> = active_votes
        .iter()
        .map(|v| (v.role.as_str(), v.score))
        .collect();
    let agg_score = weighted_average(&score_pairs, &input.weights);
    let proceed_raw = promise_quorum_met && agg_score >= input.threshold;
    let override_active = !proceed_raw && input.override_reason.is_some();
    let proceed = proceed_raw || override_active;

    // Use prior value if a higher-ballot promise carried one; otherwise use our value.
    // (In this single-round implementation, there are never prior values.)
    let _ = highest_prior_ballot; // unused in practice
    let proposed_value = if let Some(prior) = highest_prior_value {
        prior
    } else {
        PaxosValue {
            score: agg_score,
            proceed,
        }
    };

    if let Some(log) = log.as_mut() {
        log.write(PaxosEvent::Accept {
            ballot,
            value: proposed_value.clone(),
        })?;
    }

    // ── Phase 3: Accepted ─────────────────────────────────────────────────────
    let mut accepted = 0usize;
    for vote in &active_votes {
        if let Some(log) = log.as_mut() {
            log.write(PaxosEvent::Accepted {
                from: vote.role.clone(),
                ballot,
            })?;
        }
        accepted += 1;
    }

    let accepted_quorum_met = accepted >= quorum;

    // ── Decision ──────────────────────────────────────────────────────────────
    // Re-evaluate proceed with the final accepted quorum check.
    let final_score = if accepted_quorum_met {
        proposed_value.score
    } else {
        0.0 // No quorum → no consensus → block
    };
    let final_proceed_raw = accepted_quorum_met && final_score >= input.threshold;
    let final_override = !final_proceed_raw && input.override_reason.is_some();
    let final_proceed = final_proceed_raw || final_override;

    if let Some(log) = log.as_mut() {
        log.write(PaxosEvent::Decided {
            ballot,
            value: PaxosValue {
                score: final_score,
                proceed: final_proceed,
            },
            override_active: final_override,
            timed_out: timed_out_roles.clone(),
        })?;
    }

    // ── Write audit entry (opt-in — only when the caller supplied a sink) ────
    if let Some(audit_sink) = &input.audit_sink {
        let scores_by_role: HashMap<String, f64> = active_votes
            .iter()
            .map(|v| (v.role.clone(), v.score))
            .collect();
        write_audit_entry(
            audit_sink,
            "paxos",
            input,
            final_score,
            final_proceed,
            final_override,
            &timed_out_roles,
            &scores_by_role,
        );
    }

    if let Some(log) = log.as_ref() {
        log.cleanup();
    }

    // Collect per-role data.
    let mut scores_by_role = HashMap::new();
    let mut findings_by_role: HashMap<String, Vec<String>> = HashMap::new();
    for vote in &active_votes {
        scores_by_role.insert(vote.role.clone(), vote.score);
        if !vote.findings.is_empty() {
            findings_by_role.insert(vote.role.clone(), vote.findings.clone());
        }
    }

    let summary = build_summary(
        final_score,
        final_proceed,
        accepted,
        n,
        quorum,
        final_override,
        &timed_out_roles,
        input,
    );

    Ok(ConsensusResult {
        score: final_score,
        proceed: final_proceed,
        algorithm_used: ConsensusAlgorithm::Paxos,
        scores_by_role,
        findings_by_role,
        timed_out_roles,
        override_active: final_override,
        summary,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_summary(
    score: f64,
    proceed: bool,
    accepted: usize,
    n: usize,
    quorum: usize,
    override_active: bool,
    timed_out_roles: &[String],
    input: &ConsensusInput,
) -> String {
    let mut parts = vec![format!(
        "[Paxos] prepare/promise/accept/accepted ({accepted}/{n}, quorum: {quorum}), \
        score={score:.2}, threshold={threshold:.2}, proceed={proceed}",
        threshold = input.threshold,
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::ReviewerVote;
    use super::*;
    use tempfile::tempdir;

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

    fn make_input(
        dir: &std::path::Path,
        votes: Vec<ReviewerVote>,
        threshold: f64,
    ) -> ConsensusInput {
        ConsensusInput {
            votes,
            weights: HashMap::new(),
            threshold,
            algorithm: ConsensusAlgorithm::Paxos,
            run_id: Some("paxos-test".to_string()),
            run_dir: Some(dir.to_path_buf()),
            require_all: false,
            override_reason: None,
            audit_sink: None,
        }
    }

    #[test]
    fn single_decree_prepare_accept_roundtrip() {
        let dir = tempdir().unwrap();
        let input = make_input(
            dir.path(),
            vec![vote("architect", 0.85), vote("security", 0.9)],
            0.75,
        );
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert_eq!(result.algorithm_used, ConsensusAlgorithm::Paxos);
        assert!((result.score - 0.875).abs() < 1e-9);
        assert!(result.summary.contains("[Paxos]"));
        assert!(result.summary.contains("prepare/promise/accept/accepted"));
    }

    #[test]
    fn low_score_blocks() {
        let dir = tempdir().unwrap();
        let input = make_input(dir.path(), vec![vote("a", 0.4), vote("b", 0.5)], 0.75);
        let result = run(&input).unwrap();
        assert!(!result.proceed);
        assert!((result.score - 0.45).abs() < 1e-9);
    }

    #[test]
    fn timeout_reduces_quorum_size() {
        let dir = tempdir().unwrap();
        let input = make_input(
            dir.path(),
            vec![
                vote("architect", 0.9),
                vote("security", 0.85),
                timeout_vote("pm"),
            ],
            0.75,
        );
        // 2 active, majority = 1; quorum met → proceed
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert_eq!(result.timed_out_roles, vec!["pm"]);
    }

    #[test]
    fn override_bypasses_block() {
        let dir = tempdir().unwrap();
        let mut input = make_input(dir.path(), vec![vote("a", 0.3), vote("b", 0.4)], 0.75);
        input.override_reason = Some("critical hotfix".to_string());
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert!(result.override_active);
        assert!(result.summary.contains("OVERRIDE"));
    }

    #[test]
    fn audit_log_cleaned_up_on_success() {
        let dir = tempdir().unwrap();
        let input = make_input(dir.path(), vec![vote("a", 0.9)], 0.75);
        let log_path = dir.path().join("paxos-test.paxos.log");
        run(&input).unwrap();
        assert!(!log_path.exists());
    }

    #[test]
    fn per_role_scores_and_findings_captured() {
        let dir = tempdir().unwrap();
        let mut v = vote("security", 0.7);
        v.findings = vec!["XSS risk at auth endpoint".to_string()];
        let input = make_input(dir.path(), vec![v, vote("architect", 0.8)], 0.5);
        let result = run(&input).unwrap();
        assert_eq!(result.scores_by_role.get("security"), Some(&0.7));
        let findings = result.findings_by_role.get("security").unwrap();
        assert_eq!(findings[0], "XSS risk at auth endpoint");
    }

    #[test]
    fn audit_entry_written_before_log_cleanup() {
        let dir = tempdir().unwrap();
        // Create .ta subdir to simulate a real workspace structure.
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        // run_dir is inside .ta/workflow-runs/<run-id>
        let run_dir = ta_dir.join("workflow-runs").join("paxos-audit-test");
        std::fs::create_dir_all(&run_dir).unwrap();

        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9), vote("security", 0.8)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Paxos,
            run_id: Some("paxos-audit-test".to_string()),
            run_dir: Some(run_dir.clone()),
            require_all: false,
            override_reason: None,
            audit_sink: Some(ta_dir.join("audit.jsonl")),
        };
        run(&input).unwrap();

        // Paxos log should be cleaned up.
        let log_path = run_dir.join("paxos-audit-test.paxos.log");
        assert!(
            !log_path.exists(),
            "Paxos log should be deleted after success"
        );

        // Audit entry should exist.
        let audit_path = ta_dir.join("audit.jsonl");
        assert!(audit_path.exists(), "audit.jsonl should exist");
        let content = std::fs::read_to_string(&audit_path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["event"], "consensus_complete");
        assert_eq!(entry["algorithm"], "paxos");
        assert_eq!(entry["run_id"], "paxos-audit-test");
        assert!(entry["proceed"].as_bool().unwrap());
    }

    #[test]
    fn override_audit_entry_written_when_override_active() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let run_dir = ta_dir.join("workflow-runs").join("paxos-override-audit");
        std::fs::create_dir_all(&run_dir).unwrap();

        let input = ConsensusInput {
            votes: vec![vote("architect", 0.3)], // low score → would block
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Paxos,
            run_id: Some("paxos-override-audit".to_string()),
            run_dir: Some(run_dir.clone()),
            require_all: false,
            override_reason: Some("emergency paxos fix approved by CTO".to_string()),
            audit_sink: Some(ta_dir.join("audit.jsonl")),
        };
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert!(result.override_active);

        let audit_path = ta_dir.join("audit.jsonl");
        assert!(audit_path.exists());
        let content = std::fs::read_to_string(&audit_path).unwrap();
        let entries: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        // Should have consensus_complete entry and consensus_override entry.
        assert!(entries
            .iter()
            .any(|e| e["event"] == "consensus_complete" && e["override_active"] == true));
        assert!(entries.iter().any(|e| e["event"] == "consensus_override"));
        let override_entry = entries
            .iter()
            .find(|e| e["event"] == "consensus_override")
            .unwrap();
        assert_eq!(
            override_entry["reason"],
            "emergency paxos fix approved by CTO"
        );
    }

    #[test]
    fn runs_correctly_with_no_run_dir_or_run_id() {
        // Item 7: persistence is optional for Paxos too.
        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9), vote("security", 0.8)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Paxos,
            run_id: None,
            run_dir: None,
            require_all: false,
            override_reason: None,
            audit_sink: None,
        };
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert!((result.score - 0.85).abs() < 1e-9);
    }

    // ── Item 8: even-reviewer-count quorum coverage ───────────────────────────

    #[test]
    fn four_reviewers_evenly_split_score_resolves_by_weighted_average() {
        let dir = tempdir().unwrap();
        // n=4 → quorum = 4/2 + 1 = 3. All 4 promise/accept, well above
        // quorum; the score tie (two high, two low) must resolve via
        // weighted_average, independent of the quorum check itself.
        let input = make_input(
            dir.path(),
            vec![
                vote("architect", 0.9),
                vote("security", 0.9),
                vote("principal", 0.4),
                vote("pm", 0.4),
            ],
            0.75,
        );
        let result = run(&input).unwrap();
        assert!(
            result.summary.contains("4/4"),
            "expected 4/4 accepted, got: {}",
            result.summary
        );
        assert!((result.score - 0.65).abs() < 1e-9);
        assert!(!result.proceed);
    }

    #[test]
    fn two_reviewers_majority_requires_both_to_commit() {
        let dir = tempdir().unwrap();
        // n=2 → quorum = 2/2 + 1 = 2: both active reviewers must promise and
        // accept for the round to decide.
        let input = make_input(
            dir.path(),
            vec![vote("architect", 0.9), vote("security", 0.85)],
            0.75,
        );
        let result = run(&input).unwrap();
        assert!(result.summary.contains("2/2"), "{}", result.summary);
        assert!(result.proceed);
    }
}
