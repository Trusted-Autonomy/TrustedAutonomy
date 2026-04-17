// velocity.rs — Feature Velocity Stats & Outcome Telemetry (v0.13.10).
//
// Tracks per-goal timing and outcome data in `.ta/velocity-stats.json`.
// Append-only: one entry per goal terminal outcome.
//
// v0.15.7: Added `velocity-history.jsonl` — a committed, shared log written on
// `ta draft apply --git-commit`. Each entry tagged with machine_id + committer
// so multi-machine appends are conflict-free. `ta stats velocity` merges both
// files (dedup by goal_id). `ta stats velocity --team` reads shared history only.
//
// v0.15.14.2: Added token cost fields, phase-prefix filtering, rework tracking.

use std::cmp::Reverse;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

// Serde skip helpers for default zero values.
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}
/// Public version of `is_zero_u64` for use in `goal_run.rs` serde attributes.
pub fn is_zero_u64_pub(v: &u64) -> bool {
    *v == 0
}

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::GoalError;
use crate::goal_run::GoalRun;

/// Compute a stable 8-character machine identifier: first 8 hex chars of
/// SHA-256(hostname). Falls back to `"unknown"` if hostname is unavailable.
pub fn machine_id() -> String {
    use sha2::{Digest, Sha256};
    let hostname = hostname_str();
    let mut hasher = Sha256::new();
    hasher.update(hostname.as_bytes());
    let result = hasher.finalize();
    // Encode as lowercase hex, take first 8 characters.
    let hex: String = result.iter().map(|b| format!("{:02x}", b)).collect();
    hex[..8].to_string()
}

/// Return the current system hostname as a string, or `"unknown"` on failure.
fn hostname_str() -> String {
    // std::net::gethostname is not stable; use the hostname via env or process.
    // Prefer HOSTNAME env var (set on Linux), fall back to running `hostname`.
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    // Try the `hostname` command as a cross-platform fallback.
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Read `git config user.name` from the given directory (or the global config).
/// Returns `None` if git is unavailable or the config is not set.
pub fn git_committer(work_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(work_dir)
        .output()
        .ok()?;
    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    } else {
        None
    }
}

/// Outcome of a goal run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalOutcome {
    Applied,
    Denied,
    Cancelled,
    Failed,
    Timeout,
}

impl std::fmt::Display for GoalOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoalOutcome::Applied => write!(f, "applied"),
            GoalOutcome::Denied => write!(f, "denied"),
            GoalOutcome::Cancelled => write!(f, "cancelled"),
            GoalOutcome::Failed => write!(f, "failed"),
            GoalOutcome::Timeout => write!(f, "timeout"),
        }
    }
}

/// A single goal's velocity record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VelocityEntry {
    pub goal_id: Uuid,
    pub title: String,
    /// Workflow type: "code", "doc", "qa", etc.
    #[serde(default)]
    pub workflow: String,
    /// Agent backend used.
    pub agent: String,
    /// Optional plan phase link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_phase: Option<String>,
    pub outcome: GoalOutcome,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_ready_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Seconds from start to pr_ready (agent build time).
    pub build_seconds: i64,
    /// Seconds from pr_ready to applied/denied (review time).
    #[serde(default)]
    pub review_seconds: i64,
    /// Total seconds from start to terminal state.
    pub total_seconds: i64,
    /// Whether a human amended any artifact before apply.
    #[serde(default)]
    pub amended: bool,
    /// Number of follow-up goals spawned from this one.
    #[serde(default)]
    pub follow_up_count: u32,
    /// Sum of follow-up goal build_seconds (rework cost).
    #[serde(default)]
    pub rework_seconds: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    /// First 8 hex chars of SHA-256(hostname) — identifies the machine that applied.
    /// Added in v0.15.7; absent in entries written by earlier versions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine_id: String,
    /// Git committer name at apply time (`git config user.name`).
    /// Added in v0.15.7; absent in entries written by earlier versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committer: Option<String>,
    // ── Token cost fields (v0.15.14.2) ──────────────────────────────
    /// LLM input tokens consumed by this goal's agent session.
    /// `0` when not available (non-Claude agents or older entries).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub input_tokens: u64,
    /// LLM output tokens generated by this goal's agent session.
    /// `0` when not available.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub output_tokens: u64,
    /// Estimated USD cost derived from `input_tokens`/`output_tokens` using the
    /// model rate table in `token_cost.rs`. `0.0` for non-Claude agents.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub cost_usd: f64,
    /// Model identifier string (e.g. `"claude-sonnet-4-6"`).
    /// Empty for non-Claude agents or entries written by older versions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// `true` if `cost_usd` was computed from a known model rate table.
    /// `false` for Ollama/Codex/unknown agents where cost data is unavailable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cost_estimated: bool,
}

impl VelocityEntry {
    /// Build a VelocityEntry from a GoalRun at terminal state.
    pub fn from_goal(goal: &GoalRun, outcome: GoalOutcome) -> Self {
        let now = Utc::now();
        let total_seconds = (now - goal.created_at).num_seconds().max(0);

        // pr_ready_at is not stored on GoalRun — use updated_at for applied/denied,
        // and estimate build time as total (review time is unknown without pr_ready_at).
        let build_seconds = match outcome {
            GoalOutcome::Applied | GoalOutcome::Denied => {
                // Best effort: use updated_at as the completion time
                (now - goal.created_at).num_seconds().max(0)
            }
            _ => total_seconds,
        };

        Self {
            goal_id: goal.goal_run_id,
            title: goal.title.clone(),
            workflow: String::new(),
            agent: goal.agent_id.clone(),
            plan_phase: goal.plan_phase.clone(),
            outcome,
            started_at: goal.created_at,
            pr_ready_at: None,
            completed_at: Some(now),
            build_seconds,
            review_seconds: 0,
            total_seconds,
            amended: false,
            follow_up_count: 0,
            rework_seconds: 0,
            denial_reason: None,
            cancel_reason: None,
            machine_id: String::new(),
            committer: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            model: String::new(),
            cost_estimated: false,
        }
    }

    /// Set the denial reason (for Denied outcome).
    pub fn with_denial_reason(mut self, reason: impl Into<String>) -> Self {
        self.denial_reason = Some(reason.into());
        self
    }

    /// Set the cancel reason (for Cancelled outcome).
    pub fn with_cancel_reason(mut self, reason: impl Into<String>) -> Self {
        self.cancel_reason = Some(reason.into());
        self
    }

    /// Set the workflow type.
    pub fn with_workflow(mut self, workflow: impl Into<String>) -> Self {
        self.workflow = workflow.into();
        self
    }

    /// Stamp with the current machine_id (SHA-256 hostname hash).
    pub fn with_machine_id(mut self) -> Self {
        self.machine_id = machine_id();
        self
    }

    /// Stamp with the git committer name from the given working directory.
    pub fn with_committer(mut self, work_dir: &Path) -> Self {
        self.committer = git_committer(work_dir);
        self
    }

    /// Populate token cost fields from accumulated agent token counts (v0.15.14.2).
    ///
    /// Uses the model rate table to compute `cost_usd`. For non-Claude models,
    /// sets `cost_usd = 0.0` and `cost_estimated = false`.
    pub fn with_token_cost(mut self, input_tokens: u64, output_tokens: u64, model: &str) -> Self {
        use crate::token_cost::compute_cost;
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        self.model = model.to_string();
        let (cost, estimated) = compute_cost(model, input_tokens, output_tokens);
        self.cost_usd = cost;
        self.cost_estimated = estimated;
        self
    }
}

/// Aggregate statistics over a set of velocity entries.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VelocityAggregate {
    pub total_goals: usize,
    pub applied: usize,
    pub denied: usize,
    pub cancelled: usize,
    pub failed: usize,
    pub avg_build_seconds: i64,
    pub avg_rework_seconds: i64,
    pub p90_build_seconds: i64,
    pub total_rework_seconds: i64,
    /// Total USD cost across all goals in the aggregate set.
    #[serde(default)]
    pub total_cost_usd: f64,
    /// Average USD cost per goal.
    #[serde(default)]
    pub avg_cost_usd: f64,
}

impl VelocityAggregate {
    pub fn from_entries(entries: &[VelocityEntry]) -> Self {
        let total_goals = entries.len();
        let applied = entries
            .iter()
            .filter(|e| e.outcome == GoalOutcome::Applied)
            .count();
        let denied = entries
            .iter()
            .filter(|e| e.outcome == GoalOutcome::Denied)
            .count();
        let cancelled = entries
            .iter()
            .filter(|e| e.outcome == GoalOutcome::Cancelled)
            .count();
        let failed = entries
            .iter()
            .filter(|e| matches!(e.outcome, GoalOutcome::Failed | GoalOutcome::Timeout))
            .count();

        let total_rework_seconds: i64 = entries.iter().map(|e| e.rework_seconds).sum();

        let avg_build_seconds = if total_goals > 0 {
            entries.iter().map(|e| e.build_seconds).sum::<i64>() / total_goals as i64
        } else {
            0
        };

        let avg_rework_seconds = if total_goals > 0 {
            total_rework_seconds / total_goals as i64
        } else {
            0
        };

        let mut build_times: Vec<i64> = entries.iter().map(|e| e.build_seconds).collect();
        build_times.sort_unstable();
        let p90_build_seconds = if build_times.is_empty() {
            0
        } else {
            let idx = ((build_times.len() as f64 * 0.9) as usize).min(build_times.len() - 1);
            build_times[idx]
        };

        let total_cost_usd: f64 = entries.iter().map(|e| e.cost_usd).sum();
        let avg_cost_usd = if total_goals > 0 {
            total_cost_usd / total_goals as f64
        } else {
            0.0
        };

        Self {
            total_goals,
            applied,
            denied,
            cancelled,
            failed,
            avg_build_seconds,
            avg_rework_seconds,
            p90_build_seconds,
            total_rework_seconds,
            total_cost_usd,
            avg_cost_usd,
        }
    }
}

/// The velocity stats file: `.ta/velocity-stats.jsonl` (one JSON entry per line).
pub struct VelocityStore {
    path: PathBuf,
}

impl VelocityStore {
    /// Create a store backed by the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Convenience constructor: `.ta/velocity-stats.jsonl` relative to project root.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self::new(project_root.as_ref().join(".ta/velocity-stats.jsonl"))
    }

    /// Append an entry to the store.
    pub fn append(&self, entry: &VelocityEntry) -> Result<(), GoalError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| GoalError::IoError {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{}", line).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Load all entries from the store.
    pub fn load_all(&self) -> Result<Vec<VelocityEntry>, GoalError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<VelocityEntry>(trimmed) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Load entries since the given date (UTC).
    pub fn load_since(&self, since: DateTime<Utc>) -> Result<Vec<VelocityEntry>, GoalError> {
        let all = self.load_all()?;
        Ok(all.into_iter().filter(|e| e.started_at >= since).collect())
    }

    /// Load entries filtered by outcome.
    pub fn load_by_outcome(&self, outcome: &GoalOutcome) -> Result<Vec<VelocityEntry>, GoalError> {
        let all = self.load_all()?;
        Ok(all.into_iter().filter(|e| &e.outcome == outcome).collect())
    }

    /// Compute aggregate stats over all stored entries.
    pub fn aggregate(&self) -> Result<VelocityAggregate, GoalError> {
        let all = self.load_all()?;
        Ok(VelocityAggregate::from_entries(&all))
    }
}

/// The committed shared velocity history: `.ta/velocity-history.jsonl`.
///
/// Written by `ta draft apply --git-commit` (same moment as `plan_history.jsonl`).
/// Each entry carries `machine_id` and `committer` so multi-machine appends produce
/// unique lines that merge without conflicts.
///
/// This store is intentionally separate from `VelocityStore` (local log) so the two
/// files can evolve independently.
pub struct VelocityHistoryStore {
    path: PathBuf,
}

impl VelocityHistoryStore {
    /// Create a store backed by the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Convenience constructor: `.ta/velocity-history.jsonl` relative to project root.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        Self::new(project_root.as_ref().join(".ta/velocity-history.jsonl"))
    }

    /// Append an entry to the committed history store.
    pub fn append(&self, entry: &VelocityEntry) -> Result<(), GoalError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| GoalError::IoError {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{}", line).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Load all entries from the committed history.
    pub fn load_all(&self) -> Result<Vec<VelocityEntry>, GoalError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(|source| GoalError::IoError {
            path: self.path.display().to_string(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|source| GoalError::IoError {
                path: self.path.display().to_string(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<VelocityEntry>(trimmed) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

/// Merge local (`velocity-stats.jsonl`) and committed (`velocity-history.jsonl`) entries,
/// deduplicating by `goal_id`. Entries that appear in the committed store are canonical;
/// local-only entries are included but flagged by the caller as `[local]` if needed.
///
/// Result is sorted by `started_at` ascending.
pub fn merge_velocity_entries(
    local: Vec<VelocityEntry>,
    committed: Vec<VelocityEntry>,
) -> (Vec<VelocityEntry>, std::collections::HashSet<Uuid>) {
    use std::collections::{HashMap, HashSet};

    let committed_ids: HashSet<Uuid> = committed.iter().map(|e| e.goal_id).collect();

    // Build a map from goal_id -> entry, starting with local, overwritten by committed.
    let mut by_id: HashMap<Uuid, VelocityEntry> =
        local.into_iter().map(|e| (e.goal_id, e)).collect();
    for entry in committed {
        by_id.insert(entry.goal_id, entry);
    }

    let mut merged: Vec<VelocityEntry> = by_id.into_values().collect();
    merged.sort_by_key(|e| e.started_at);

    (merged, committed_ids)
}

/// Per-committer or per-machine aggregate for `--team` view.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContributorAggregate {
    /// Name or machine-id of the contributor.
    pub contributor: String,
    pub total_goals: usize,
    pub applied: usize,
    pub avg_build_seconds: i64,
    pub total_build_seconds: i64,
    /// Total USD cost for this contributor.
    #[serde(default)]
    pub total_cost_usd: f64,
}

/// Aggregate committed history by committer (falls back to machine_id).
pub fn aggregate_by_contributor(entries: &[VelocityEntry]) -> Vec<ContributorAggregate> {
    use std::collections::HashMap;
    let mut map: HashMap<String, Vec<&VelocityEntry>> = HashMap::new();
    for entry in entries {
        let key = entry.committer.clone().unwrap_or_else(|| {
            if entry.machine_id.is_empty() {
                "unknown".to_string()
            } else {
                entry.machine_id.clone()
            }
        });
        map.entry(key).or_default().push(entry);
    }

    let mut result: Vec<ContributorAggregate> = map
        .into_iter()
        .map(|(contributor, es)| {
            let total_goals = es.len();
            let applied = es
                .iter()
                .filter(|e| e.outcome == GoalOutcome::Applied)
                .count();
            let total_build_seconds: i64 = es.iter().map(|e| e.build_seconds).sum();
            let avg_build_seconds = if total_goals > 0 {
                total_build_seconds / total_goals as i64
            } else {
                0
            };
            let total_cost_usd: f64 = es.iter().map(|e| e.cost_usd).sum();
            ContributorAggregate {
                contributor,
                total_goals,
                applied,
                avg_build_seconds,
                total_build_seconds,
                total_cost_usd,
            }
        })
        .collect();
    result.sort_by_key(|e| Reverse(e.total_goals));
    result
}

/// A plan phase that had contributions from more than one committer/machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseConflict {
    /// The plan phase ID (e.g. "v0.15.7").
    pub phase_id: String,
    /// Contributors who each have at least one entry for this phase.
    pub contributors: Vec<String>,
    /// Number of entries for this phase across all contributors.
    pub entry_count: usize,
}

/// Detect plan phases that appear in committed history entries from more than one
/// distinct contributor (committer or machine_id). These indicate parallel work
/// that may need coordination.
///
/// Only entries with a non-empty `plan_phase` are considered.
pub fn detect_phase_conflicts(committed: &[VelocityEntry]) -> Vec<PhaseConflict> {
    use std::collections::{HashMap, HashSet};

    // phase_id -> set of contributor keys
    let mut phase_contributors: HashMap<String, HashSet<String>> = HashMap::new();
    let mut phase_counts: HashMap<String, usize> = HashMap::new();

    for entry in committed {
        let phase = match &entry.plan_phase {
            Some(p) if !p.is_empty() => p.clone(),
            _ => continue,
        };
        let contributor = entry.committer.clone().unwrap_or_else(|| {
            if entry.machine_id.is_empty() {
                "unknown".to_string()
            } else {
                entry.machine_id.clone()
            }
        });
        phase_contributors
            .entry(phase.clone())
            .or_default()
            .insert(contributor);
        *phase_counts.entry(phase).or_default() += 1;
    }

    let mut conflicts: Vec<PhaseConflict> = phase_contributors
        .into_iter()
        .filter(|(_, contributors)| contributors.len() > 1)
        .map(|(phase_id, contributors)| {
            let entry_count = phase_counts[&phase_id];
            let mut sorted: Vec<String> = contributors.into_iter().collect();
            sorted.sort();
            PhaseConflict {
                phase_id,
                contributors: sorted,
                entry_count,
            }
        })
        .collect();
    conflicts.sort_by_key(|c| c.phase_id.clone());
    conflicts
}

/// Migrate all entries from a local VelocityStore into the VelocityHistoryStore,
/// stamping each entry with the current machine_id. Skips entries already present
/// in the history store (by goal_id). Non-destructive — local file is unchanged.
///
/// Returns the number of entries written.
pub fn migrate_local_to_history(
    local_store: &VelocityStore,
    history_store: &VelocityHistoryStore,
    work_dir: &Path,
) -> Result<usize, GoalError> {
    let local_entries = local_store.load_all()?;
    let committed_entries = history_store.load_all()?;
    let committed_ids: std::collections::HashSet<Uuid> =
        committed_entries.iter().map(|e| e.goal_id).collect();

    let mid = machine_id();
    let committer = git_committer(work_dir);
    let mut written = 0;

    for mut entry in local_entries {
        if committed_ids.contains(&entry.goal_id) {
            continue;
        }
        if entry.machine_id.is_empty() {
            entry.machine_id = mid.clone();
        }
        if entry.committer.is_none() {
            entry.committer = committer.clone();
        }
        history_store.append(&entry)?;
        written += 1;
    }
    Ok(written)
}

/// Filter velocity entries by plan-phase version prefix (v0.15.14.2).
///
/// Matches entries whose title starts with `"v<prefix>."` (following the TA phase-naming
/// convention). For example, `phase_prefix = "0.15"` keeps entries whose title starts
/// with `"v0.15."`. `phase_prefix = "0.15.13"` narrows to `"v0.15.13."`.
///
/// If `phase_prefix` is empty, returns all entries unchanged.
pub fn filter_by_phase_prefix(
    entries: Vec<VelocityEntry>,
    phase_prefix: &str,
) -> Vec<VelocityEntry> {
    if phase_prefix.is_empty() {
        return entries;
    }
    let prefix = format!("v{}.", phase_prefix);
    entries
        .into_iter()
        .filter(|e| {
            e.title.starts_with(&prefix)
                || e.plan_phase
                    .as_deref()
                    .map(|p| p.starts_with(&prefix))
                    .unwrap_or(false)
        })
        .collect()
}

/// Update the parent goal's velocity entry to add rework cost from a follow-up apply.
///
/// Increments `follow_up_count` by 1 and adds `follow_up_build_seconds` to
/// `rework_seconds` in both the local store and the committed history store.
///
/// If the parent entry is not found in a store, that store is silently skipped.
/// Non-destructive: each store is rewritten atomically (read → update → overwrite).
///
/// Returns `Ok(true)` if at least one store was updated, `Ok(false)` if not found.
pub fn update_parent_rework(
    local_store: &VelocityStore,
    history_store: &VelocityHistoryStore,
    parent_goal_id: Uuid,
    follow_up_build_seconds: i64,
) -> Result<bool, GoalError> {
    let mut updated = false;

    // Update local store.
    if let Ok(mut entries) = local_store.load_all() {
        if let Some(entry) = entries.iter_mut().find(|e| e.goal_id == parent_goal_id) {
            entry.follow_up_count += 1;
            entry.rework_seconds += follow_up_build_seconds;
            updated = true;
            // Rewrite the store atomically.
            let _ = rewrite_store(&local_store.path, &entries);
        }
    }

    // Update history store.
    if let Ok(mut entries) = history_store.load_all() {
        if let Some(entry) = entries.iter_mut().find(|e| e.goal_id == parent_goal_id) {
            entry.follow_up_count += 1;
            entry.rework_seconds += follow_up_build_seconds;
            updated = true;
            let _ = rewrite_store(&history_store.path, &entries);
        }
    }

    Ok(updated)
}

/// Rewrite a JSONL velocity store file with updated entries.
/// Each entry is serialized as one JSON line. Overwrites the file atomically
/// by writing to a temp file then renaming.
fn rewrite_store(path: &std::path::Path, entries: &[VelocityEntry]) -> Result<(), GoalError> {
    use std::io::Write as IoWrite;

    // Build the new content.
    let mut content = String::new();
    for entry in entries {
        let line = serde_json::to_string(entry)?;
        content.push_str(&line);
        content.push('\n');
    }

    // Write to a temp file in the same directory then rename.
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|source| GoalError::IoError {
            path: tmp.display().to_string(),
            source,
        })?;
        f.write_all(content.as_bytes())
            .map_err(|source| GoalError::IoError {
                path: tmp.display().to_string(),
                source,
            })?;
    }
    std::fs::rename(&tmp, path).map_err(|source| GoalError::IoError {
        path: path.display().to_string(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_goal() -> GoalRun {
        GoalRun::new(
            "Test Goal",
            "test objective",
            "claude-code",
            PathBuf::from("/tmp/staging"),
            PathBuf::from("/tmp/store"),
        )
    }

    #[test]
    fn velocity_entry_from_goal() {
        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
        assert_eq!(entry.goal_id, goal.goal_run_id);
        assert_eq!(entry.outcome, GoalOutcome::Applied);
        assert!(entry.total_seconds >= 0);
    }

    #[test]
    fn velocity_store_append_and_load() {
        let dir = tempdir().unwrap();
        let store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));

        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
        store.append(&entry).unwrap();

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].goal_id, goal.goal_run_id);
    }

    #[test]
    fn velocity_store_empty_when_no_file() {
        let dir = tempdir().unwrap();
        let store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));
        let loaded = store.load_all().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn aggregate_calculates_correctly() {
        let dir = tempdir().unwrap();
        let store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));

        for _ in 0..3 {
            let goal = make_goal();
            let mut entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
            entry.build_seconds = 600;
            store.append(&entry).unwrap();
        }
        let goal = make_goal();
        let mut entry = VelocityEntry::from_goal(&goal, GoalOutcome::Failed);
        entry.build_seconds = 300;
        store.append(&entry).unwrap();

        let agg = store.aggregate().unwrap();
        assert_eq!(agg.total_goals, 4);
        assert_eq!(agg.applied, 3);
        assert_eq!(agg.failed, 1);
        assert!(agg.avg_build_seconds > 0);
    }

    #[test]
    fn machine_id_is_eight_hex_chars() {
        let mid = machine_id();
        assert_eq!(mid.len(), 8);
        assert!(mid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn machine_id_is_stable() {
        // Two calls must produce the same value.
        assert_eq!(machine_id(), machine_id());
    }

    #[test]
    fn velocity_history_store_append_and_load() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::new(dir.path().join("velocity-history.jsonl"));

        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied)
            .with_machine_id()
            .with_committer(dir.path());
        history.append(&entry).unwrap();

        let loaded = history.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].goal_id, goal.goal_run_id);
        assert_eq!(loaded[0].machine_id.len(), 8);
    }

    #[test]
    fn velocity_history_empty_when_no_file() {
        let dir = tempdir().unwrap();
        let history = VelocityHistoryStore::new(dir.path().join("velocity-history.jsonl"));
        assert!(history.load_all().unwrap().is_empty());
    }

    #[test]
    fn merge_deduplicates_by_goal_id() {
        let dir = tempdir().unwrap();
        let local_store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));
        let history_store = VelocityHistoryStore::new(dir.path().join("velocity-history.jsonl"));

        let goal = make_goal();
        // Same goal appears in both local and committed.
        let local_entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
        let mut committed_entry = local_entry.clone();
        committed_entry.machine_id = "aabbccdd".to_string();

        local_store.append(&local_entry).unwrap();
        history_store.append(&committed_entry).unwrap();

        // Unique goal only in local.
        let local_only = make_goal();
        local_store
            .append(&VelocityEntry::from_goal(&local_only, GoalOutcome::Failed))
            .unwrap();

        let local = local_store.load_all().unwrap();
        let committed = history_store.load_all().unwrap();
        let (merged, committed_ids) = merge_velocity_entries(local, committed);

        // 2 unique goal_ids after dedup.
        assert_eq!(merged.len(), 2);
        assert!(committed_ids.contains(&goal.goal_run_id));
        assert!(!committed_ids.contains(&local_only.goal_run_id));

        // The merged entry for the shared goal takes the committed version (has machine_id).
        let shared = merged
            .iter()
            .find(|e| e.goal_id == goal.goal_run_id)
            .unwrap();
        assert_eq!(shared.machine_id, "aabbccdd");
    }

    #[test]
    fn migrate_promotes_local_entries_to_history() {
        let dir = tempdir().unwrap();
        let local_store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));
        let history_store = VelocityHistoryStore::new(dir.path().join("velocity-history.jsonl"));

        // Two goals in local.
        let goal1 = make_goal();
        let goal2 = make_goal();
        local_store
            .append(&VelocityEntry::from_goal(&goal1, GoalOutcome::Applied))
            .unwrap();
        local_store
            .append(&VelocityEntry::from_goal(&goal2, GoalOutcome::Denied))
            .unwrap();

        // Pre-populate history with goal1 — should be skipped during migrate.
        let mut pre = VelocityEntry::from_goal(&goal1, GoalOutcome::Applied);
        pre.machine_id = "existing1".to_string();
        history_store.append(&pre).unwrap();

        let written = migrate_local_to_history(&local_store, &history_store, dir.path()).unwrap();

        // Only goal2 is new.
        assert_eq!(written, 1);

        let all = history_store.load_all().unwrap();
        assert_eq!(all.len(), 2);

        // goal1's entry in history retains the original machine_id.
        let g1 = all.iter().find(|e| e.goal_id == goal1.goal_run_id).unwrap();
        assert_eq!(g1.machine_id, "existing1");
    }

    #[test]
    fn aggregate_by_contributor_groups_by_committer() {
        let _dir = tempdir().unwrap();
        let make_entry = |committer: &str, outcome: GoalOutcome| {
            let goal = make_goal();
            let mut e = VelocityEntry::from_goal(&goal, outcome);
            e.committer = Some(committer.to_string());
            e.machine_id = "abc12345".to_string();
            e
        };

        let entries = vec![
            make_entry("alice", GoalOutcome::Applied),
            make_entry("alice", GoalOutcome::Applied),
            make_entry("bob", GoalOutcome::Denied),
        ];

        let agg = aggregate_by_contributor(&entries);
        let alice = agg.iter().find(|a| a.contributor == "alice").unwrap();
        let bob = agg.iter().find(|a| a.contributor == "bob").unwrap();

        assert_eq!(alice.total_goals, 2);
        assert_eq!(alice.applied, 2);
        assert_eq!(bob.total_goals, 1);
        assert_eq!(bob.applied, 0);
    }

    #[test]
    fn detect_phase_conflicts_flags_multi_contributor_phases() {
        let make_entry = |committer: &str, phase: Option<&str>| {
            let goal = make_goal();
            let mut e = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
            e.committer = Some(committer.to_string());
            e.machine_id = "abc12345".to_string();
            e.plan_phase = phase.map(|s| s.to_string());
            e
        };

        let entries = vec![
            make_entry("alice", Some("v0.15.7")),
            make_entry("bob", Some("v0.15.7")), // conflict: same phase, different person
            make_entry("alice", Some("v0.15.8")), // no conflict: only alice
            make_entry("alice", None),          // no phase: ignored
        ];

        let conflicts = detect_phase_conflicts(&entries);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].phase_id, "v0.15.7");
        assert_eq!(conflicts[0].contributors.len(), 2);
        assert!(conflicts[0].contributors.contains(&"alice".to_string()));
        assert!(conflicts[0].contributors.contains(&"bob".to_string()));
        assert_eq!(conflicts[0].entry_count, 2);
    }

    #[test]
    fn filter_by_phase_prefix_matches_title() {
        let make = |title: &str| {
            let goal = make_goal();
            let mut e = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
            e.title = title.to_string();
            e
        };
        let entries = vec![
            make("v0.15.13 — Something"),
            make("v0.15.14 — Another"),
            make("v0.14.0 — Old"),
        ];
        let filtered = filter_by_phase_prefix(entries, "0.15");
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|e| e.title.starts_with("v0.15.")));
    }

    #[test]
    fn filter_by_phase_prefix_empty_prefix_returns_all() {
        let make = |title: &str| {
            let goal = make_goal();
            let mut e = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
            e.title = title.to_string();
            e
        };
        let entries = vec![make("v0.15.13 — A"), make("v0.14.0 — B")];
        let filtered = filter_by_phase_prefix(entries, "");
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn update_parent_rework_increments_fields() {
        let dir = tempdir().unwrap();
        let local_store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));
        let history_store = VelocityHistoryStore::new(dir.path().join("velocity-history.jsonl"));

        let parent_goal = make_goal();
        let entry = VelocityEntry::from_goal(&parent_goal, GoalOutcome::Applied);
        local_store.append(&entry).unwrap();
        history_store.append(&entry).unwrap();

        let updated =
            update_parent_rework(&local_store, &history_store, parent_goal.goal_run_id, 300)
                .unwrap();
        assert!(updated);

        let local = local_store.load_all().unwrap();
        assert_eq!(local[0].follow_up_count, 1);
        assert_eq!(local[0].rework_seconds, 300);

        let hist = history_store.load_all().unwrap();
        assert_eq!(hist[0].follow_up_count, 1);
        assert_eq!(hist[0].rework_seconds, 300);
    }

    #[test]
    fn update_parent_rework_not_found_returns_false() {
        let dir = tempdir().unwrap();
        let local_store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));
        let history_store = VelocityHistoryStore::new(dir.path().join("velocity-history.jsonl"));
        let unknown_id = Uuid::new_v4();
        let updated = update_parent_rework(&local_store, &history_store, unknown_id, 60).unwrap();
        assert!(!updated);
    }

    #[test]
    fn with_token_cost_sets_fields_for_claude() {
        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied).with_token_cost(
            100_000,
            20_000,
            "claude-sonnet-4-6",
        );
        assert_eq!(entry.input_tokens, 100_000);
        assert_eq!(entry.output_tokens, 20_000);
        assert!(entry.cost_estimated);
        assert!(entry.cost_usd > 0.0);
        assert_eq!(entry.model, "claude-sonnet-4-6");
    }

    #[test]
    fn with_token_cost_ollama_is_zero_not_estimated() {
        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied).with_token_cost(
            50_000,
            10_000,
            "qwen3.5:9b",
        );
        assert_eq!(entry.cost_usd, 0.0);
        assert!(!entry.cost_estimated);
    }

    #[test]
    fn aggregate_includes_cost_fields() {
        let dir = tempdir().unwrap();
        let store = VelocityStore::new(dir.path().join("velocity-stats.jsonl"));

        let goal = make_goal();
        let entry = VelocityEntry::from_goal(&goal, GoalOutcome::Applied).with_token_cost(
            1_000_000,
            0,
            "claude-sonnet-4-6",
        );
        store.append(&entry).unwrap();

        let agg = store.aggregate().unwrap();
        assert!(agg.total_cost_usd > 0.0);
        assert_eq!(agg.avg_cost_usd, agg.total_cost_usd);
    }

    #[test]
    fn detect_phase_conflicts_no_conflicts_when_single_contributor() {
        let make_entry = |committer: &str, phase: &str| {
            let goal = make_goal();
            let mut e = VelocityEntry::from_goal(&goal, GoalOutcome::Applied);
            e.committer = Some(committer.to_string());
            e.plan_phase = Some(phase.to_string());
            e
        };

        let entries = vec![
            make_entry("alice", "v0.15.7"),
            make_entry("alice", "v0.15.8"),
        ];
        let conflicts = detect_phase_conflicts(&entries);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn old_entry_without_machine_id_deserializes_ok() {
        // Simulate an entry written before v0.15.7 (no machine_id / committer fields).
        let old_json = r#"{"goal_id":"550e8400-e29b-41d4-a716-446655440000","title":"old","workflow":"","agent":"claude","outcome":"applied","started_at":"2025-01-01T00:00:00Z","build_seconds":300,"review_seconds":0,"total_seconds":300,"amended":false,"follow_up_count":0,"rework_seconds":0}"#;
        let entry: VelocityEntry = serde_json::from_str(old_json).unwrap();
        assert!(entry.machine_id.is_empty());
        assert!(entry.committer.is_none());
    }
}
