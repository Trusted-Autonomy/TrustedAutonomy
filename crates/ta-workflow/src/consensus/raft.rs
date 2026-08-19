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

use super::{weighted_average, write_audit_entry, ConsensusAlgorithm, ConsensusError};
use super::{ConsensusInput, ConsensusResult};

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
    pub fn open(run_dir: &std::path::Path, run_id: &str) -> Result<Self, ConsensusError> {
        std::fs::create_dir_all(run_dir).map_err(|e| ConsensusError::Io {
            path: run_dir.display().to_string(),
            source: e,
        })?;
        let path = run_dir.join(format!("{}.raft.log", run_id));

        let mut entries = Vec::new();
        let mut next_index = 1u64;
        let mut current_term = 1u64;

        if path.exists() {
            let f = std::fs::File::open(&path).map_err(|e| ConsensusError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
            let reader = BufReader::new(f);
            for line in reader.lines() {
                let line = line.map_err(|e| ConsensusError::Io {
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
    pub fn append(&mut self, mut entry: RaftLogEntry) -> Result<(), ConsensusError> {
        entry.index = self.next_index;
        entry.term = self.current_term;
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
///
/// Crash-recovery log persistence is opt-in: it only happens when the caller
/// supplies both `run_dir` and `run_id`. Without them, the algorithm still
/// runs correctly (append+commit is atomic in single-coordinator mode either
/// way) — it simply has nothing to replay if the process crashes mid-run.
pub fn run(input: &ConsensusInput) -> Result<ConsensusResult, ConsensusError> {
    let active_votes: Vec<_> = input.votes.iter().filter(|v| !v.timed_out).collect();
    let timed_out_roles: Vec<String> = input
        .votes
        .iter()
        .filter(|v| v.timed_out)
        .map(|v| v.role.clone())
        .collect();

    let n = active_votes.len();
    let majority = n / 2 + 1;

    let mut log = match (&input.run_dir, &input.run_id) {
        (Some(run_dir), Some(run_id)) => Some(RaftLog::open(run_dir, run_id)?),
        _ => None,
    };

    // ── Leader election ───────────────────────────────────────────────────────
    // If log is empty, start fresh as the initial leader.
    // If log has prior entries (crash recovery), increment term and continue.
    if let Some(log) = log.as_mut() {
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
    }

    // ── Append + commit each reviewer's entry ─────────────────────────────────
    // In single-coordinator mode, append and commit are atomic: the coordinator
    // is the only node, so every appended entry is immediately committed.
    let mut committed_count = 0usize;
    let mut scores_by_role: HashMap<String, f64> = HashMap::new();
    let mut findings_by_role: HashMap<String, Vec<String>> = HashMap::new();

    // Check if we have recovered committed entries from a prior partial run.
    if let Some(log) = log.as_ref() {
        for prior in log.committed_reviewer_entries() {
            if let (Some(role), Some(score)) = (&prior.role, prior.score) {
                scores_by_role.insert(role.clone(), score);
                if !prior.findings.is_empty() {
                    findings_by_role.insert(role.clone(), prior.findings.clone());
                }
                committed_count += 1;
            }
        }
    }

    // Append and commit any votes not yet in the log.
    for vote in &active_votes {
        if scores_by_role.contains_key(&vote.role) {
            continue; // already committed in a recovered log
        }
        if let Some(log) = log.as_mut() {
            // Append
            let mut append_entry =
                RaftLogEntry::new(0, log.current_term, RaftEventKind::EntryAppended);
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
            commit_entry.detail = format!(
                "Committed log entry {}/{} (majority: {})",
                committed_count + 1,
                n,
                majority
            );
            log.append(commit_entry)?;
        }

        committed_count += 1;
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
    if let Some(log) = log.as_mut() {
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
    }

    // ── Final decision ────────────────────────────────────────────────────────
    let proceed_raw = quorum_met && score >= input.threshold;
    let override_active = !proceed_raw && input.override_reason.is_some();
    let proceed = proceed_raw || override_active;

    if let Some(log) = log.as_mut() {
        let mut complete_entry = RaftLogEntry::new(0, log.current_term, RaftEventKind::RunComplete);
        complete_entry.detail = format!(
            "proceed={proceed}, override={override_active}, timed_out=[{timed_out}]",
            timed_out = timed_out_roles.join(", ")
        );
        log.append(complete_entry)?;
    }

    // ── Write audit entry (opt-in — only when the caller supplied a sink) ────
    if let Some(audit_sink) = &input.audit_sink {
        write_audit_entry(
            audit_sink,
            "raft",
            input,
            score,
            proceed,
            override_active,
            &timed_out_roles,
            &scores_by_role,
        );
    }

    // Clean up the log on success.
    if let Some(log) = log.as_ref() {
        log.cleanup();
    }

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
            run_id: Some("raft-test".to_string()),
            run_dir: Some(dir.to_path_buf()),
            require_all: false,
            override_reason: None,
            audit_sink: None,
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
            run_id: Some("recover-test".to_string()),
            run_dir: Some(dir.path().to_path_buf()),
            require_all: false,
            override_reason: None,
            audit_sink: None,
        };
        let result = run(&input).unwrap();
        assert!(result.proceed);
        // architect recovered from prior log, security newly committed
        assert!(result.scores_by_role.contains_key("architect"));
        assert!(result.scores_by_role.contains_key("security"));
    }

    #[test]
    fn runs_correctly_with_no_run_dir_or_run_id() {
        // Item 7: persistence is optional — omitting run_dir/run_id must not
        // change the computed result, only skip crash-recovery logging.
        let input = ConsensusInput {
            votes: vec![vote("architect", 0.9), vote("security", 0.8)],
            weights: HashMap::new(),
            threshold: 0.75,
            algorithm: ConsensusAlgorithm::Raft,
            run_id: None,
            run_dir: None,
            require_all: false,
            override_reason: None,
            audit_sink: None,
        };
        let result = run(&input).unwrap();
        assert!(result.proceed);
        assert!((result.score - 0.85).abs() < 1e-9);
        assert_eq!(result.scores_by_role.len(), 2);
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
            run_id: Some("audit-test".to_string()),
            run_dir: Some(run_dir.clone()),
            require_all: false,
            override_reason: None,
            audit_sink: Some(ta_dir.join("audit.jsonl")),
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
            run_id: Some("override-audit".to_string()),
            run_dir: Some(run_dir.clone()),
            require_all: false,
            override_reason: Some("emergency fix approved by CTO".to_string()),
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
        assert_eq!(override_entry["reason"], "emergency fix approved by CTO");
    }

    // ── Item 8: even-reviewer-count quorum/tie-break coverage ────────────────
    //
    // majority = n/2 + 1, so for even n the "majority" already exceeds a
    // 50/50 split — there is no tie state in Raft's quorum math itself (it
    // counts committed entries, not vote direction), but a tied *score*
    // split (half reviewers high, half low) must still resolve deterministically
    // from the weighted average, not from commit order.

    #[test]
    fn two_reviewers_majority_requires_both_to_commit() {
        let dir = tempdir().unwrap();
        // n=2 → majority = 2/2 + 1 = 2: both active reviewers must commit,
        // unlike odd-n panels where a bare majority is less than n.
        let input = make_input(
            dir.path(),
            vec![vote("architect", 0.9), vote("security", 0.85)],
            0.75,
        );
        let result = run(&input).unwrap();
        assert!(result.summary.contains("2/2"), "{}", result.summary);
        assert!(result.proceed);
    }

    #[test]
    fn two_reviewers_one_timeout_majority_of_one_still_commits() {
        let dir = tempdir().unwrap();
        // n_active=1 (one of 2 timed out) → majority = 1/2 + 1 = 1: the lone
        // active reviewer alone meets quorum.
        let input = make_input(
            dir.path(),
            vec![vote("architect", 0.9), timeout_vote("security")],
            0.75,
        );
        let result = run(&input).unwrap();
        assert_eq!(result.timed_out_roles, vec!["security"]);
        assert!(result.summary.contains("1/1"), "{}", result.summary);
        assert!(result.proceed);
    }

    #[test]
    fn four_reviewers_evenly_split_score_resolves_by_weighted_average() {
        let dir = tempdir().unwrap();
        // n=4 → majority = 4/2 + 1 = 3. All 4 commit (no timeouts), so
        // quorum is met regardless of the score split; the tie in *scores*
        // (two high, two low) must resolve via weighted_average, not by
        // which votes committed first.
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
        assert!(result.summary.contains("4/4"), "{}", result.summary);
        // avg = (0.9+0.9+0.4+0.4)/4 = 0.65 < threshold 0.75 → blocks despite
        // full quorum, since quorum and score-threshold are independent gates.
        assert!((result.score - 0.65).abs() < 1e-9);
        assert!(!result.proceed);
    }

    #[test]
    fn four_reviewers_one_timeout_majority_of_three_still_needs_three() {
        let dir = tempdir().unwrap();
        // n_active=3 (one of 4 timed out) → majority = 3/2 + 1 = 2. All 3
        // active reviewers commit, well above the 2-entry majority floor —
        // exercises the even-original-panel-size / odd-active-count boundary.
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
        assert!(result.summary.contains("3/3"), "{}", result.summary);
        assert!(result.proceed);
    }
}
