// ledger.rs — Goal-level audit ledger (v0.14.6).
//
// Captures rich goal lifecycle records for every terminal outcome:
// applied, denied, cancelled, abandoned, gc, timeout, crashed, closed.
//
// Unlike `.ta/audit.jsonl` (which records granular tool calls), the goal
// audit ledger at `.ta/goal-audit.jsonl` records one entry per goal
// lifecycle completion. Hash chaining provides tamper detection.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AuditError;
use crate::hasher;

// ── AuditDisposition ──────────────────────────────────────────────────────────

/// How a goal's lifecycle ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditDisposition {
    /// Changes were applied to the target (happy path).
    Applied,
    /// Reviewer denied the draft.
    Denied,
    /// User or system cancelled the goal before completion.
    Cancelled,
    /// Goal was deleted before producing a draft.
    Abandoned,
    /// Garbage-collected (stale, zombie, or missing staging).
    Gc,
    /// Timed out waiting for the agent or reviewer.
    Timeout,
    /// Agent process crashed or exited unexpectedly.
    Crashed,
    /// Draft closed without applying (hand-merged, obsolete, etc.).
    Closed,
}

impl std::fmt::Display for AuditDisposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AuditDisposition::Applied => "applied",
            AuditDisposition::Denied => "denied",
            AuditDisposition::Cancelled => "cancelled",
            AuditDisposition::Abandoned => "abandoned",
            AuditDisposition::Gc => "gc",
            AuditDisposition::Timeout => "timeout",
            AuditDisposition::Crashed => "crashed",
            AuditDisposition::Closed => "closed",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for AuditDisposition {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "applied" => Ok(AuditDisposition::Applied),
            "denied" => Ok(AuditDisposition::Denied),
            "cancelled" => Ok(AuditDisposition::Cancelled),
            "abandoned" => Ok(AuditDisposition::Abandoned),
            "gc" => Ok(AuditDisposition::Gc),
            "timeout" => Ok(AuditDisposition::Timeout),
            "crashed" => Ok(AuditDisposition::Crashed),
            "closed" => Ok(AuditDisposition::Closed),
            other => Err(format!("unknown disposition: {}", other)),
        }
    }
}

// ── ArtifactRecord ────────────────────────────────────────────────────────────

/// A compact record of a single artifact included in the goal's draft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// Resource URI (e.g., "fs://workspace/src/main.rs").
    pub uri: String,
    /// Type of change: add, modify, delete, rename.
    pub change_type: String,
}

// ── AuditEntry ────────────────────────────────────────────────────────────────

/// Rich goal-lifecycle audit record stored in the goal audit ledger.
///
/// One entry per terminal goal outcome. Hash-chained for tamper detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    // Identity
    pub goal_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    pub disposition: AuditDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub agent: String,

    // Timestamps
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_ready_at: Option<DateTime<Utc>>,
    pub recorded_at: DateTime<Utc>,

    // Timing (seconds)
    pub build_seconds: i64,
    pub review_seconds: i64,
    pub total_seconds: i64,

    // Draft
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_summary: Option<String>,

    // Review
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,

    // Artifacts
    pub artifact_count: usize,
    pub lines_changed: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRecord>,

    // Policy
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_result: Option<String>,

    // Lineage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_goal_id: Option<Uuid>,

    // Integrity chain (set by GoalAuditLedger::append)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
}

impl AuditEntry {
    /// Create a minimal entry for a goal that never produced a draft.
    pub fn abandoned(goal_id: Uuid, title: &str, agent: &str, phase: Option<&str>) -> Self {
        let now = Utc::now();
        Self {
            goal_id,
            title: title.to_string(),
            objective: None,
            disposition: AuditDisposition::Abandoned,
            phase: phase.map(|s| s.to_string()),
            agent: agent.to_string(),
            created_at: now,
            pr_ready_at: None,
            recorded_at: now,
            build_seconds: 0,
            review_seconds: 0,
            total_seconds: 0,
            draft_id: None,
            ai_summary: None,
            reviewer: None,
            denial_reason: None,
            cancel_reason: None,
            artifact_count: 0,
            lines_changed: 0,
            artifacts: Vec::new(),
            policy_result: None,
            parent_goal_id: None,
            previous_hash: None,
        }
    }
}

// ── GoalAuditLedger ───────────────────────────────────────────────────────────

/// Append-only goal audit ledger backed by a JSONL file.
///
/// Each entry is hash-chained to the previous one for tamper detection.
/// One entry per terminal goal lifecycle outcome.
pub struct GoalAuditLedger {
    writer: BufWriter<File>,
    #[allow(dead_code)]
    path: PathBuf,
    last_hash: Option<String>,
}

impl GoalAuditLedger {
    /// Open (or create) a ledger at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| AuditError::OpenFailed {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let last_hash = if path.exists() {
            Self::read_last_hash(&path)?
        } else {
            None
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| AuditError::OpenFailed {
                path: path.clone(),
                source,
            })?;

        Ok(Self {
            writer: BufWriter::new(file),
            path,
            last_hash,
        })
    }

    /// Standard path for a project's goal audit ledger.
    pub fn path_for(project_root: &Path) -> PathBuf {
        project_root.join(".ta").join("goal-audit.jsonl")
    }

    /// Append an entry to the ledger, setting its hash chain link.
    pub fn append(&mut self, entry: &mut AuditEntry) -> Result<(), AuditError> {
        entry.previous_hash = self.last_hash.clone();

        let json = serde_json::to_string(entry)?;
        self.last_hash = Some(hasher::hash_str(&json));

        writeln!(self.writer, "{}", json)?;
        self.writer.flush()?;

        Ok(())
    }

    /// Read all entries from the ledger file.
    pub fn read_all(path: impl AsRef<Path>) -> Result<Vec<AuditEntry>, AuditError> {
        let file = File::open(path.as_ref()).map_err(|source| AuditError::OpenFailed {
            path: path.as_ref().to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: AuditEntry = serde_json::from_str(&line)?;
            entries.push(entry);
        }

        Ok(entries)
    }

    /// Verify the hash chain integrity of the ledger.
    pub fn verify_chain(path: impl AsRef<Path>) -> Result<bool, AuditError> {
        let file = File::open(path.as_ref()).map_err(|source| AuditError::OpenFailed {
            path: path.as_ref().to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut previous_hash: Option<String> = None;

        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let entry: AuditEntry = serde_json::from_str(&line)?;

            if entry.previous_hash != previous_hash {
                return Err(AuditError::IntegrityViolation {
                    line: line_num + 1,
                    expected: previous_hash.unwrap_or_else(|| "None".to_string()),
                    actual: entry.previous_hash.unwrap_or_else(|| "None".to_string()),
                });
            }

            previous_hash = Some(hasher::hash_str(&line));
        }

        Ok(true)
    }

    fn read_last_hash(path: &Path) -> Result<Option<String>, AuditError> {
        let file = File::open(path).map_err(|source| AuditError::OpenFailed {
            path: path.to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut last_line: Option<String> = None;

        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() {
                last_line = Some(line);
            }
        }

        Ok(last_line.map(|line| hasher::hash_str(&line)))
    }
}

// ── LedgerFilter ──────────────────────────────────────────────────────────────

/// Filter criteria for querying the goal audit ledger.
#[derive(Debug, Default)]
pub struct LedgerFilter {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub phase: Option<String>,
    pub agent: Option<String>,
    pub disposition: Option<AuditDisposition>,
}

impl LedgerFilter {
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        if let Some(since) = self.since {
            if entry.recorded_at < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if entry.recorded_at > until {
                return false;
            }
        }
        if let Some(ref phase) = self.phase {
            if entry.phase.as_deref() != Some(phase.as_str()) {
                return false;
            }
        }
        if let Some(ref agent) = self.agent {
            if entry.agent != *agent {
                return false;
            }
        }
        if let Some(ref disp) = self.disposition {
            if &entry.disposition != disp {
                return false;
            }
        }
        true
    }
}

// ── Migration ─────────────────────────────────────────────────────────────────

/// Migrate `.ta/goal-history.jsonl` entries into the goal audit ledger.
///
/// Returns the number of entries migrated. Already-migrated entries are
/// skipped by checking whether the goal_id already appears in the ledger.
pub fn migrate_from_history(
    history_path: &Path,
    ledger: &mut GoalAuditLedger,
    existing_ids: &std::collections::HashSet<Uuid>,
) -> Result<usize, AuditError> {
    if !history_path.exists() {
        return Ok(0);
    }

    let file = File::open(history_path).map_err(|source| AuditError::OpenFailed {
        path: history_path.to_path_buf(),
        source,
    })?;
    let reader = BufReader::new(file);
    let mut count = 0;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        // Parse as a loose JSON object — we don't want to depend on ta-goal
        // types here (would create a circular dependency).
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        let goal_id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        let Some(goal_id) = goal_id else {
            continue;
        };

        if existing_ids.contains(&goal_id) {
            continue;
        }

        let state = obj
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("applied");

        let disposition = match state {
            "applied" | "completed" | "merged" => AuditDisposition::Applied,
            "denied" => AuditDisposition::Denied,
            "cancelled" => AuditDisposition::Cancelled,
            "failed" => AuditDisposition::Crashed,
            _ => AuditDisposition::Applied,
        };

        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string();

        let agent = obj
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string();

        let phase = obj
            .get("phase")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let created_at = obj
            .get("created")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now);

        let completed_at = obj
            .get("completed")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());

        let total_seconds = completed_at
            .map(|c| c.signed_duration_since(created_at).num_seconds())
            .unwrap_or(0);

        let draft_id = obj
            .get("draft_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        let artifact_count = obj
            .get("artifact_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let lines_changed = obj
            .get("lines_changed")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let mut entry = AuditEntry {
            goal_id,
            title,
            objective: None,
            disposition,
            phase,
            agent,
            created_at,
            pr_ready_at: None,
            recorded_at: completed_at.unwrap_or(created_at),
            build_seconds: 0,
            review_seconds: 0,
            total_seconds,
            draft_id,
            ai_summary: None,
            reviewer: None,
            denial_reason: None,
            cancel_reason: None,
            artifact_count,
            lines_changed,
            artifacts: Vec::new(),
            policy_result: None,
            parent_goal_id: None,
            previous_hash: None,
        };

        ledger.append(&mut entry)?;
        count += 1;
    }

    Ok(count)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_entry(title: &str, disp: AuditDisposition) -> AuditEntry {
        AuditEntry {
            goal_id: Uuid::new_v4(),
            title: title.to_string(),
            objective: None,
            disposition: disp,
            phase: Some("v0.14.6".to_string()),
            agent: "claude-code".to_string(),
            created_at: Utc::now(),
            pr_ready_at: None,
            recorded_at: Utc::now(),
            build_seconds: 120,
            review_seconds: 60,
            total_seconds: 180,
            draft_id: Some(Uuid::new_v4()),
            ai_summary: None,
            reviewer: Some("alice".to_string()),
            denial_reason: None,
            cancel_reason: None,
            artifact_count: 5,
            lines_changed: 42,
            artifacts: vec![ArtifactRecord {
                uri: "fs://workspace/src/main.rs".to_string(),
                change_type: "modify".to_string(),
            }],
            policy_result: None,
            parent_goal_id: None,
            previous_hash: None,
        }
    }

    #[test]
    fn append_and_read_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("goal-audit.jsonl");

        {
            let mut ledger = GoalAuditLedger::open(&path).unwrap();
            let mut e1 = make_entry("Goal A", AuditDisposition::Applied);
            let mut e2 = make_entry("Goal B", AuditDisposition::Denied);
            ledger.append(&mut e1).unwrap();
            ledger.append(&mut e2).unwrap();
        }

        let entries = GoalAuditLedger::read_all(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "Goal A");
        assert_eq!(entries[0].disposition, AuditDisposition::Applied);
        assert_eq!(entries[1].title, "Goal B");
        assert_eq!(entries[1].disposition, AuditDisposition::Denied);
    }

    #[test]
    fn hash_chain_is_valid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("goal-audit.jsonl");

        {
            let mut ledger = GoalAuditLedger::open(&path).unwrap();
            for i in 0..5 {
                let mut e = make_entry(&format!("Goal {}", i), AuditDisposition::Applied);
                ledger.append(&mut e).unwrap();
            }
        }

        assert!(GoalAuditLedger::verify_chain(&path).unwrap());
    }

    #[test]
    fn first_entry_has_no_previous_hash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("goal-audit.jsonl");

        {
            let mut ledger = GoalAuditLedger::open(&path).unwrap();
            let mut e = make_entry("First", AuditDisposition::Applied);
            ledger.append(&mut e).unwrap();
        }

        let entries = GoalAuditLedger::read_all(&path).unwrap();
        assert!(entries[0].previous_hash.is_none());
    }

    #[test]
    fn reopen_continues_chain() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("goal-audit.jsonl");

        {
            let mut ledger = GoalAuditLedger::open(&path).unwrap();
            let mut e = make_entry("First", AuditDisposition::Applied);
            ledger.append(&mut e).unwrap();
        }
        {
            let mut ledger = GoalAuditLedger::open(&path).unwrap();
            let mut e = make_entry("Second", AuditDisposition::Denied);
            ledger.append(&mut e).unwrap();
        }

        assert!(GoalAuditLedger::verify_chain(&path).unwrap());
        assert_eq!(GoalAuditLedger::read_all(&path).unwrap().len(), 2);
    }

    #[test]
    fn ledger_filter_by_disposition() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("goal-audit.jsonl");

        {
            let mut ledger = GoalAuditLedger::open(&path).unwrap();
            let mut e1 = make_entry("Applied", AuditDisposition::Applied);
            let mut e2 = make_entry("Denied", AuditDisposition::Denied);
            let mut e3 = make_entry("Gc", AuditDisposition::Gc);
            ledger.append(&mut e1).unwrap();
            ledger.append(&mut e2).unwrap();
            ledger.append(&mut e3).unwrap();
        }

        let filter = LedgerFilter {
            disposition: Some(AuditDisposition::Denied),
            ..Default::default()
        };

        let entries = GoalAuditLedger::read_all(&path).unwrap();
        let filtered: Vec<_> = entries.iter().filter(|e| filter.matches(e)).collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].title, "Denied");
    }

    #[test]
    fn ledger_filter_by_phase() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("goal-audit.jsonl");

        {
            let mut ledger = GoalAuditLedger::open(&path).unwrap();
            let mut e1 = make_entry("A", AuditDisposition::Applied);
            e1.phase = Some("v0.14.6".to_string());
            let mut e2 = make_entry("B", AuditDisposition::Applied);
            e2.phase = Some("v0.14.7".to_string());
            ledger.append(&mut e1).unwrap();
            ledger.append(&mut e2).unwrap();
        }

        let filter = LedgerFilter {
            phase: Some("v0.14.6".to_string()),
            ..Default::default()
        };

        let entries = GoalAuditLedger::read_all(&path).unwrap();
        let filtered: Vec<_> = entries.iter().filter(|e| filter.matches(e)).collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].title, "A");
    }

    #[test]
    fn abandoned_entry_constructor() {
        let id = Uuid::new_v4();
        let entry = AuditEntry::abandoned(id, "Test goal", "claude-code", Some("v0.14.6"));
        assert_eq!(entry.goal_id, id);
        assert_eq!(entry.disposition, AuditDisposition::Abandoned);
        assert_eq!(entry.artifact_count, 0);
        assert!(entry.previous_hash.is_none());
    }

    #[test]
    fn migrate_from_history_basic() {
        let dir = tempdir().unwrap();
        let history_path = dir.path().join("goal-history.jsonl");
        let ledger_path = dir.path().join("goal-audit.jsonl");

        // Write a minimal history entry.
        let history_json = serde_json::json!({
            "id": Uuid::new_v4().to_string(),
            "title": "Old goal",
            "state": "applied",
            "agent": "claude-code",
            "created": Utc::now().to_rfc3339(),
            "artifact_count": 3,
            "lines_changed": 15
        });
        std::fs::write(&history_path, format!("{}\n", history_json)).unwrap();

        let mut ledger = GoalAuditLedger::open(&ledger_path).unwrap();
        let existing = std::collections::HashSet::new();
        let count = migrate_from_history(&history_path, &mut ledger, &existing).unwrap();
        assert_eq!(count, 1);

        let entries = GoalAuditLedger::read_all(&ledger_path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Old goal");
        assert_eq!(entries[0].disposition, AuditDisposition::Applied);
        assert_eq!(entries[0].artifact_count, 3);
    }

    #[test]
    fn migrate_skips_already_migrated() {
        let dir = tempdir().unwrap();
        let history_path = dir.path().join("goal-history.jsonl");
        let ledger_path = dir.path().join("goal-audit.jsonl");

        let id = Uuid::new_v4();
        let history_json = serde_json::json!({
            "id": id.to_string(),
            "title": "Already migrated",
            "state": "applied",
            "agent": "claude-code",
            "created": Utc::now().to_rfc3339(),
        });
        std::fs::write(&history_path, format!("{}\n", history_json)).unwrap();

        let mut ledger = GoalAuditLedger::open(&ledger_path).unwrap();
        let mut existing = std::collections::HashSet::new();
        existing.insert(id);

        let count = migrate_from_history(&history_path, &mut ledger, &existing).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn disposition_display_round_trip() {
        for d in [
            AuditDisposition::Applied,
            AuditDisposition::Denied,
            AuditDisposition::Cancelled,
            AuditDisposition::Abandoned,
            AuditDisposition::Gc,
            AuditDisposition::Timeout,
            AuditDisposition::Crashed,
            AuditDisposition::Closed,
        ] {
            let s = d.to_string();
            let parsed: AuditDisposition = s.parse().unwrap();
            assert_eq!(d, parsed);
        }
    }
}
