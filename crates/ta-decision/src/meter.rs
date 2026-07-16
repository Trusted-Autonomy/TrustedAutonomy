use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::gate::Decision;

/// Which step of the Write -> Review -> Decision -> Commit/Reject graph a
/// telemetry record was captured for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Write,
    Review,
    Decision,
    Commit,
    Reject,
}

/// A single per-action telemetry record: one Write, one Review, one Decision,
/// or one Commit/Reject, each metered independently so cost/duration can be
/// attributed to the specific step that incurred it, not just the goal as a
/// whole.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRecord {
    pub goal_id: Uuid,
    pub action: ActionKind,
    /// Human-readable label for the action (e.g. "draft apply", "social publish").
    pub label: String,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub duration_secs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_score: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    pub recorded_at: DateTime<Utc>,
}

impl ActionRecord {
    pub fn new(goal_id: Uuid, action: ActionKind, label: impl Into<String>) -> Self {
        Self {
            goal_id,
            action,
            label: label.into(),
            cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            duration_secs: 0.0,
            confidence: None,
            risk_score: None,
            decision: None,
            recorded_at: Utc::now(),
        }
    }

    pub fn with_tokens(mut self, input_tokens: u64, output_tokens: u64) -> Self {
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        self
    }

    pub fn with_cost(mut self, cost_usd: f64) -> Self {
        self.cost_usd = cost_usd;
        self
    }

    pub fn with_duration(mut self, duration_secs: f64) -> Self {
        self.duration_secs = duration_secs;
        self
    }

    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = Some(confidence);
        self
    }

    pub fn with_risk_score(mut self, risk_score: u32) -> Self {
        self.risk_score = Some(risk_score);
        self
    }

    pub fn with_decision(mut self, decision: Decision) -> Self {
        self.decision = Some(decision);
        self
    }
}

/// Append-only per-action telemetry store, backed by a single JSONL file.
/// One `Meter` per project (typically `.ta/telemetry.jsonl`); records for
/// every goal are interleaved and filtered on query.
#[derive(Debug, Clone)]
pub struct Meter {
    path: PathBuf,
}

impl Meter {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one action record. Creates the parent directory and file if
    /// they don't exist yet.
    pub fn record(&self, record: &ActionRecord) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// All records ever recorded, in append order. Empty if the file doesn't
    /// exist yet (a fresh project has recorded nothing).
    pub fn all(&self) -> io::Result<Vec<ActionRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path)?;
        let reader = io::BufReader::new(file);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: ActionRecord = serde_json::from_str(&line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            records.push(record);
        }
        Ok(records)
    }

    /// All records for a specific goal, in append order.
    pub fn query_by_goal(&self, goal_id: Uuid) -> io::Result<Vec<ActionRecord>> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|r| r.goal_id == goal_id)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::Decision;

    #[test]
    fn records_round_trip_and_query_by_goal() {
        let dir = tempfile::tempdir().unwrap();
        let meter = Meter::new(dir.path().join("telemetry.jsonl"));

        let goal_a = Uuid::new_v4();
        let goal_b = Uuid::new_v4();

        let rec_a1 = ActionRecord::new(goal_a, ActionKind::Write, "agent write")
            .with_tokens(100, 50)
            .with_cost(0.01)
            .with_duration(2.5);
        let rec_a2 = ActionRecord::new(goal_a, ActionKind::Decision, "draft apply gate")
            .with_confidence(0.9)
            .with_risk_score(10)
            .with_decision(Decision::Commit);
        let rec_b1 = ActionRecord::new(goal_b, ActionKind::Write, "other goal write");

        meter.record(&rec_a1).unwrap();
        meter.record(&rec_a2).unwrap();
        meter.record(&rec_b1).unwrap();

        let all = meter.all().unwrap();
        assert_eq!(all.len(), 3);

        let goal_a_records = meter.query_by_goal(goal_a).unwrap();
        assert_eq!(goal_a_records.len(), 2);
        assert_eq!(goal_a_records[0].action, ActionKind::Write);
        assert_eq!(goal_a_records[0].input_tokens, 100);
        assert_eq!(goal_a_records[1].action, ActionKind::Decision);
        assert_eq!(goal_a_records[1].decision, Some(Decision::Commit));

        let goal_b_records = meter.query_by_goal(goal_b).unwrap();
        assert_eq!(goal_b_records.len(), 1);
    }

    #[test]
    fn querying_before_any_record_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let meter = Meter::new(dir.path().join("nonexistent.jsonl"));
        assert!(meter.query_by_goal(Uuid::new_v4()).unwrap().is_empty());
    }
}
