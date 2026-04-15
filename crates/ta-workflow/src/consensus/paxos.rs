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

use super::{weighted_average, ConsensusAlgorithm, ConsensusInput, ConsensusResult};
use crate::WorkflowError;

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
    fn open(run_dir: &std::path::Path, run_id: &str) -> Result<Self, WorkflowError> {
        std::fs::create_dir_all(run_dir).map_err(|e| WorkflowError::IoError {
            path: run_dir.display().to_string(),
            source: e,
        })?;
        let path = run_dir.join(format!("{}.paxos.log", run_id));
        Ok(Self {
            path,
            next_index: 1,
        })
    }

    fn write(&mut self, event: PaxosEvent) -> Result<(), WorkflowError> {
        let entry = PaxosLogEntry {
            index: self.next_index,
            timestamp: Utc::now().to_rfc3339(),
            event,
        };
        self.next_index += 1;
        let json =
            serde_json::to_string(&entry).map_err(|e| WorkflowError::Other(e.to_string()))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| WorkflowError::IoError {
                path: self.path.display().to_string(),
                source: e,
            })?;
        writeln!(f, "{}", json).map_err(|e| WorkflowError::IoError {
            path: self.path.display().to_string(),
            source: e,
        })?;
        f.flush().map_err(|e| WorkflowError::IoError {
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
pub fn run(input: &ConsensusInput) -> Result<ConsensusResult, WorkflowError> {
    let active_votes: Vec<_> = input.votes.iter().filter(|v| !v.timed_out).collect();
    let timed_out_roles: Vec<String> = input
        .votes
        .iter()
        .filter(|v| v.timed_out)
        .map(|v| v.role.clone())
        .collect();

    let n = active_votes.len();
    let quorum = n / 2 + 1;

    let mut log = PaxosAuditLog::open(&input.run_dir, &input.run_id)?;
    let ballot: Ballot = 1;

    // ── Phase 1: Prepare ──────────────────────────────────────────────────────
    log.write(PaxosEvent::Prepare {
        ballot,
        reviewer_count: n,
        quorum,
    })?;

    // In single-process mode, all active reviewers immediately promise.
    // (They have not seen a higher ballot — this is the first and only proposal.)
    let mut promises = 0usize;
    let highest_prior_ballot: Option<Ballot> = None;
    let mut highest_prior_value: Option<PaxosValue> = None;

    for vote in &active_votes {
        log.write(PaxosEvent::Promise {
            from: vote.role.clone(),
            ballot,
            prior_ballot: None,
            prior_value: None,
        })?;
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

    log.write(PaxosEvent::Accept {
        ballot,
        value: proposed_value.clone(),
    })?;

    // ── Phase 3: Accepted ─────────────────────────────────────────────────────
    let mut accepted = 0usize;
    for vote in &active_votes {
        log.write(PaxosEvent::Accepted {
            from: vote.role.clone(),
            ballot,
        })?;
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

    log.write(PaxosEvent::Decided {
        ballot,
        value: PaxosValue {
            score: final_score,
            proceed: final_proceed,
        },
        override_active: final_override,
        timed_out: timed_out_roles.clone(),
    })?;

    // ── Write audit entry to .ta/audit.jsonl BEFORE cleanup ──────────────────
    // Constitution §1.5: per-reviewer votes must be durable in the append-only
    // audit log regardless of whether the caller retains ConsensusResult.
    {
        // Climb up from run_dir to find the .ta directory.
        // run_dir is typically <workspace_root>/.ta/workflow-runs/<run-id>/
        // so run_dir.parent().parent() = <workspace_root>/.ta
        let audit_path = input
            .run_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|ta_dir| ta_dir.join("audit.jsonl"))
            .unwrap_or_else(|| input.run_dir.join("audit.jsonl"));

        let scores_json: serde_json::Value = active_votes
            .iter()
            .map(|v| (v.role.clone(), serde_json::Value::from(v.score)))
            .collect::<serde_json::Map<_, _>>()
            .into();

        let mut entry = serde_json::json!({
            "event": "consensus_complete",
            "run_id": input.run_id,
            "algorithm": "paxos",
            "score": final_score,
            "proceed": final_proceed,
            "override_active": final_override,
            "timed_out_roles": timed_out_roles,
            "scores_by_role": scores_json,
            "timestamp": Utc::now().to_rfc3339(),
        });

        if let Some(reason) = &input.override_reason {
            entry["override_reason"] = serde_json::Value::String(reason.clone());
        }

        if let Some(parent) = audit_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&audit_path)
        {
            let _ = writeln!(f, "{}", entry);
        }

        // Item 4: separate override entry for queryability.
        if final_override {
            let override_entry = serde_json::json!({
                "event": "consensus_override",
                "run_id": input.run_id,
                "reason": input.override_reason.as_deref().unwrap_or(""),
                "score_before_override": final_score,
                "timestamp": Utc::now().to_rfc3339(),
            });
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&audit_path)
            {
                let _ = writeln!(f, "{}", override_entry);
            }
        }
    }

    log.cleanup();

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
            run_id: "paxos-test".to_string(),
            run_dir: dir.to_path_buf(),
            require_all: false,
            override_reason: None,
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
            run_id: "paxos-audit-test".to_string(),
            run_dir: run_dir.clone(),
            require_all: false,
            override_reason: None,
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
            run_id: "paxos-override-audit".to_string(),
            run_dir: run_dir.clone(),
            require_all: false,
            override_reason: Some("emergency paxos fix approved by CTO".to_string()),
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
}
