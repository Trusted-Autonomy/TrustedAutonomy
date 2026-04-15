// consensus/raft.rs — Raft-based consensus for multi-agent review panels (v0.15.15).
//
// Implements a session-scoped, crash-fault-tolerant log for aggregating reviewer
// votes. Designed for single-process use (the consensus step IS the leader) but
// provides durability: if the coordinator process crashes mid-run, the log can be
// replayed to recover committed entries.
//
// Protocol (simplified for single-coordinator use):
//   1. Leader election: coordinator starts as leader (term=1). If it detects a
//      stale log from a prior crashed run, it increments the term and adopts the
//      latest committed entries.
//   2. Log append: each non-timed-out reviewer's { role, score, findings } is
//      appended as a log entry and flushed to disk.
//   3. Commit: an entry is committed once written (the coordinator is both
//      proposer and majority-of-one for crash recovery). Final commit requires
//      ⌊n/2⌋+1 entries where n = number of active reviewers.
//   4. Aggregate: once committed, compute weighted_average(committed_scores).
//
// Log file: `.ta/workflow-runs/<run-id>/raft.log` (newline-delimited JSON).
// Cleared on successful run completion.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::{weighted_average, ConsensusAlgorithm, ConsensusInput, ConsensusResult};
use crate::WorkflowError;

// ── Log entry types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RaftEventKind {
    LeaderElected,
    EntryAppended,
    EntryCommitted,
    QuorumReached,
    RunComplete,
}

/// A single entry in the Raft log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaftLogEntry {
    pub index: u64,
    pub term: u64,
    pub(crate) kind: RaftEventKind,
    pub role: Option<String>,
    pub score: Option<f64>,
    #[serde(default)]
    pub findings: Vec<String>,
    pub timestamp: String,
    #[serde(default)]
    pub detail: String,
}

impl RaftLogEntry {
    fn new(index: u64, term: u64, kind: RaftEventKind) -> Self {
        Self {
            index,
            term,
            kind,
            role: None,
            score: None,
            findings: vec![],
            timestamp: Utc::now().to_rfc3339(),
            detail: String::new(),
        }
    }
}

// ── Raft log ──────────────────────────────────────────────────────────────────

/// Session-scoped Raft log persisted to `.ta/workflow-runs/<run-id>/raft.log`.
pub struct RaftLog {
    path: PathBuf,
    entries: Vec<RaftLogEntry>,
    next_index: u64,
    current_term: u64,
}

impl RaftLog {
    /// Open (or create) the log file for this run.
    pub fn open(run_dir: &std::path::Path, run_id: &str) -> Result<Self, WorkflowError> {
        std::fs::create_dir_all(run_dir).map_err(|e| WorkflowError::IoError {
            path: run_dir.display().to_string(),
            source: e,
        })?;
        let path = run_dir.join(format!("{}.raft.log", run_id));

        let mut entries = Vec::new();
        let mut next_index = 1u64;
        let mut current_term = 1u64;

        if path.exists() {
            let f = std::fs::File::open(&path).map_err(|e| WorkflowError::IoError {
                path: path.display().to_string(),
                source: e,
            })?;
            let reader = BufReader::new(f);
            for line in reader.lines() {
                let line = line.map_err(|e| WorkflowError::IoError {
                    path: path.display().to_string(),
                    source: e,
                })?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<RaftLogEntry>(&line) {
                    if entry.index >= next_index {
                        next_index = entry.index + 1;
                    }
                    if entry.term > current_term {
                        current_term = entry.term;
                    }
                    entries.push(entry);
                }
            }
        }

        Ok(Self {
            path,
            entries,
            next_index,
            current_term,
        })
    }

    /// Append a new entry, flushing to disk immediately for crash recovery.
    pub fn append(&mut self, mut entry: RaftLogEntry) -> Result<(), WorkflowError> {
        entry.index = self.next_index;
        entry.term = self.current_term;
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
        })?;
        self.next_index += 1;
        self.entries.push(entry);
        Ok(())
    }

    /// Increment the term (used when adopting a recovered log).
    pub fn increment_term(&mut self) -> u64 {
        self.current_term += 1;
        self.current_term
    }

    /// All committed reviewer entries (EntryCommitted with a role).
    pub fn committed_reviewer_entries(&self) -> Vec<&RaftLogEntry> {
        self.entries
            .iter()
            .filter(|e| e.kind == RaftEventKind::EntryCommitted && e.role.is_some())
            .collect()
    }

    /// Delete the log file on successful completion.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

// ── run ───────────────────────────────────────────────────────────────────────

/// Execute Raft-based consensus.
pub fn run(input: &ConsensusInput) -> Result<ConsensusResult, WorkflowError> {
    let active_votes: Vec<_> = input.votes.iter().filter(|v| !v.timed_out).collect();
    let timed_out_roles: Vec<String> = input
        .votes
        .iter()
        .filter(|v| v.timed_out)
        .map(|v| v.role.clone())
        .collect();

    let n = active_votes.len();
    let majority = n / 2 + 1;

    let run_dir = &input.run_dir;
    let mut log = RaftLog::open(run_dir, &input.run_id)?;

    // ── Leader election ───────────────────────────────────────────────────────
    // If log is empty, start fresh as the initial leader.
    // If log has prior entries (crash recovery), increment term and continue.
    let recovered = !log.entries.is_empty();
    if recovered {
        let new_term = log.increment_term();
        let mut entry = RaftLogEntry::new(0, new_term, RaftEventKind::LeaderElected);
        entry.detail = format!("Recovered from crash — new term {}", new_term);
        log.append(entry)?;
    } else {
        let mut entry = RaftLogEntry::new(0, log.current_term, RaftEventKind::LeaderElected);
        entry.detail = format!(
            "Leader elected — {} reviewers, majority threshold {}",
            n, majority
        );
        log.append(entry)?;
    }

    // ── Append + commit each reviewer's entry ─────────────────────────────────
    // In single-coordinator mode, append and commit are atomic: the coordinator
    // is the only node, so every appended entry is immediately committed.
    let mut committed_count = 0usize;
    let mut scores_by_role: HashMap<String, f64> = HashMap::new();
    let mut findings_by_role: HashMap<String, Vec<String>> = HashMap::new();

    // Check if we have recovered committed entries from a prior partial run.
    for prior in log.committed_reviewer_entries() {
        if let (Some(role), Some(score)) = (&prior.role, prior.score) {
            scores_by_role.insert(role.clone(), score);
            if !prior.findings.is_empty() {
                findings_by_role.insert(role.clone(), prior.findings.clone());
            }
            committed_count += 1;
        }
    }

    // Append and commit any votes not yet in the log.
    for vote in &active_votes {
        if scores_by_role.contains_key(&vote.role) {
            continue; // already committed in a recovered log
        }
        // Append
        let mut append_entry = RaftLogEntry::new(0, log.current_term, RaftEventKind::EntryAppended);
        append_entry.role = Some(vote.role.clone());
        append_entry.score = Some(vote.score);
        append_entry.findings = vote.findings.clone();
        append_entry.detail = format!(
            "Reviewer '{}' vote appended (score={:.2})",
            vote.role, vote.score
        );
        log.append(append_entry)?;

        // Commit (coordinator acknowledges immediately)
        let mut commit_entry =
            RaftLogEntry::new(0, log.current_term, RaftEventKind::EntryCommitted);
        commit_entry.role = Some(vote.role.clone());
        commit_entry.score = Some(vote.score);
        commit_entry.findings = vote.findings.clone();
        committed_count += 1;
        commit_entry.detail = format!(
            "Committed log entry {}/{} (majority: {})",
            committed_count, n, majority
        );
        log.append(commit_entry)?;

        scores_by_role.insert(vote.role.clone(), vote.score);
        if !vote.findings.is_empty() {
            findings_by_role.insert(vote.role.clone(), vote.findings.clone());
        }
    }

    // ── Quorum check ──────────────────────────────────────────────────────────
    let quorum_met = committed_count >= majority;

    let score_pairs: Vec<(&str, f64)> = scores_by_role
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    let score = weighted_average(&score_pairs, &input.weights);

    // ── Log quorum event ──────────────────────────────────────────────────────
    let mut quorum_entry = RaftLogEntry::new(0, log.current_term, RaftEventKind::QuorumReached);
    quorum_entry.detail = format!(
        "[Raft] Committed log entry {committed}/{n} (majority: {majority}), \
        score={score:.2}, threshold={threshold:.2}, quorum_met={quorum_met}",
        committed = committed_count,
        n = n,
        majority = majority,
        score = score,
        threshold = input.threshold,
        quorum_met = quorum_met,
    );
    log.append(quorum_entry)?;

    // ── Final decision ────────────────────────────────────────────────────────
    let proceed_raw = quorum_met && score >= input.threshold;
    let override_active = !proceed_raw && input.override_reason.is_some();
    let proceed = proceed_raw || override_active;

    let mut complete_entry = RaftLogEntry::new(0, log.current_term, RaftEventKind::RunComplete);
    complete_entry.detail = format!(
        "proceed={proceed}, override={override_active}, timed_out=[{timed_out}]",
        timed_out = timed_out_roles.join(", ")
    );
    log.append(complete_entry)?;

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

        let scores_json: serde_json::Value = scores_by_role
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::from(*v)))
            .collect::<serde_json::Map<_, _>>()
            .into();

        let mut entry = serde_json::json!({
            "event": "consensus_complete",
            "run_id": input.run_id,
            "algorithm": "raft",
            "score": score,
            "proceed": proceed,
            "override_active": override_active,
            "timed_out_roles": timed_out_roles,
            "scores_by_role": scores_json,
            "timestamp": chrono::Utc::now().to_rfc3339(),
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
        if override_active {
            let override_entry = serde_json::json!({
                "event": "consensus_override",
                "run_id": input.run_id,
                "reason": input.override_reason.as_deref().unwrap_or(""),
                "score_before_override": score,
                "timestamp": chrono::Utc::now().to_rfc3339(),
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

    // Clean up the log on success.
    log.cleanup();

    let summary = build_summary(
        score,
        proceed,
        committed_count,
        n,
        majority,
        override_active,
        &timed_out_roles,
        input,
    );

    Ok(ConsensusResult {
        score,
        proceed,
        algorithm_used: ConsensusAlgorithm::Raft,
        scores_by_role,
        findings_by_role,
        timed_out_roles,
        override_active,
        summary,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_summary(
    score: f64,
    proceed: bool,
    committed: usize,
    n: usize,
    majority: usize,
    override_active: bool,
    timed_out_roles: &[String],
    input: &ConsensusInput,
) -> String {
    let mut parts = vec![format!(
        "[Raft] Committed log entry {committed}/{n} (majority: {majority}), \
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
            algorithm: ConsensusAlgorithm::Raft,
            run_id: "raft-test".to_string(),
            run_dir: dir.to_path_buf(),
            require_all: false,
            override_reason: None,
        }
    }

    #[test]
    fn four_reviewers_all_commit_proceed() {
        let dir = tempdir().unwrap();
        let input = make_input(
            dir.path(),
            vec![
                vote("architect", 0.9),
                vote("security", 0.8),
                vote("principal", 0.85),
                vote("pm", 0.7),
            ],
            0.75,
        );
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert_eq!(result.algorithm_used, ConsensusAlgorithm::Raft);
        assert_eq!(result.timed_out_roles.len(), 0);
        // avg = (0.9+0.8+0.85+0.7)/4 = 0.8125
        assert!((result.score - 0.8125).abs() < 1e-9);
        assert!(result.summary.contains("[Raft]"));
        assert!(result.summary.contains("4/4"));
    }

    #[test]
    fn four_reviewers_one_stall_majority_of_three_commits() {
        let dir = tempdir().unwrap();
        let input = make_input(
            dir.path(),
            vec![
                vote("architect", 0.9),
                vote("security", 0.8),
                vote("principal", 0.85),
                timeout_vote("pm"),
            ],
            0.75,
        );
        let result = run(&input).unwrap();
        // 3 active reviewers, majority = 2, so quorum met
        assert!(result.proceed);
        assert_eq!(result.timed_out_roles, vec!["pm"]);
        // avg of architect, security, principal = (0.9+0.8+0.85)/3 = 0.85
        assert!((result.score - 0.85).abs() < 1e-9);
        assert!(result.summary.contains("timed_out=[pm]"));
    }

    #[test]
    fn low_score_blocks() {
        let dir = tempdir().unwrap();
        let input = make_input(
            dir.path(),
            vec![vote("architect", 0.4), vote("security", 0.3)],
            0.75,
        );
        let result = run(&input).unwrap();
        assert!(!result.proceed);
        assert!(!result.override_active);
    }

    #[test]
    fn override_bypasses_block() {
        let dir = tempdir().unwrap();
        let mut input = make_input(dir.path(), vec![vote("architect", 0.3)], 0.75);
        input.override_reason = Some("emergency hotfix".to_string());
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert!(result.override_active);
        assert!(result.summary.contains("OVERRIDE"));
    }

    #[test]
    fn log_file_created_then_deleted_on_success() {
        let dir = tempdir().unwrap();
        let input = make_input(dir.path(), vec![vote("a", 0.9)], 0.75);
        // Log should not exist before
        let log_path = dir.path().join("raft-test.raft.log");
        assert!(!log_path.exists());
        run(&input).unwrap();
        // Log should be cleaned up after success
        assert!(!log_path.exists());
    }

    #[test]
    fn log_recovery_from_partial_run() {
        let dir = tempdir().unwrap();

        // Write a partial log manually (simulating a crashed run).
        let log_path = dir.path().join("recover-test.raft.log");
        let prior_entries = vec![
            RaftLogEntry {
                index: 1,
                term: 1,
                kind: RaftEventKind::LeaderElected,
                role: None,
                score: None,
                findings: vec![],
                timestamp: Utc::now().to_rfc3339(),
                detail: "Initial leader".to_string(),
            },
            RaftLogEntry {
                index: 2,
                term: 1,
                kind: RaftEventKind::EntryCommitted,
                role: Some("architect".to_string()),
                score: Some(0.85),
                findings: vec![],
                timestamp: Utc::now().to_rfc3339(),
                detail: "Committed".to_string(),
            },
        ];
        let mut f = std::fs::File::create(&log_path).unwrap();
        for e in &prior_entries {
            writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
        }
        drop(f);

        let input = ConsensusInput {
            votes: vec![
                vote("architect", 0.85), // already committed in prior log
                vote("security", 0.9),   // new vote
            ],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Raft,
            run_id: "recover-test".to_string(),
            run_dir: dir.path().to_path_buf(),
            require_all: false,
            override_reason: None,
        };
        let result = run(&input).unwrap();
        assert!(result.proceed);
        // architect recovered from prior log, security newly committed
        assert!(result.scores_by_role.contains_key("architect"));
        assert!(result.scores_by_role.contains_key("security"));
    }

    #[test]
    fn raft_log_open_creates_directory() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("nested").join("run-dir");
        let _log = RaftLog::open(&subdir, "test-run").unwrap();
        assert!(subdir.exists());
    }

    #[test]
    fn findings_are_committed_to_log_and_result() {
        let dir = tempdir().unwrap();
        let mut v = vote("security", 0.7);
        v.findings = vec!["Unvalidated input at line 42".to_string()];
        let input = make_input(dir.path(), vec![v], 0.5);
        let result = run(&input).unwrap();
        let findings = result.findings_by_role.get("security").unwrap();
        assert_eq!(findings[0], "Unvalidated input at line 42");
    }

    #[test]
    fn audit_entry_written_before_log_cleanup() {
        let dir = tempdir().unwrap();
        // Create .ta subdir to simulate a real workspace structure.
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        // run_dir is inside .ta/workflow-runs/<run-id>
        let run_dir = ta_dir.join("workflow-runs").join("audit-test");
        std::fs::create_dir_all(&run_dir).unwrap();

        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9), vote("security", 0.8)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Raft,
            run_id: "audit-test".to_string(),
            run_dir: run_dir.clone(),
            require_all: false,
            override_reason: None,
        };
        run(&input).unwrap();

        // Raft log should be cleaned up.
        let log_path = run_dir.join("audit-test.raft.log");
        assert!(
            !log_path.exists(),
            "Raft log should be deleted after success"
        );

        // Audit entry should exist.
        let audit_path = ta_dir.join("audit.jsonl");
        assert!(audit_path.exists(), "audit.jsonl should exist");
        let content = std::fs::read_to_string(&audit_path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(entry["event"], "consensus_complete");
        assert_eq!(entry["algorithm"], "raft");
        assert_eq!(entry["run_id"], "audit-test");
        assert!(entry["proceed"].as_bool().unwrap());
    }

    #[test]
    fn override_audit_entry_written_when_override_active() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let run_dir = ta_dir.join("workflow-runs").join("override-audit");
        std::fs::create_dir_all(&run_dir).unwrap();

        let input = ConsensusInput {
            votes: vec![vote("architect", 0.3)], // low score → would block
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Raft,
            run_id: "override-audit".to_string(),
            run_dir: run_dir.clone(),
            require_all: false,
            override_reason: Some("emergency fix approved by CTO".to_string()),
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
        assert_eq!(override_entry["reason"], "emergency fix approved by CTO");
    }
}
