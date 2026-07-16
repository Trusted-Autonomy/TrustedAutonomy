// verify_audit.rs — Red-team adversarial review of ta_human_verify's
// auto-confirmed (`Commit`) decisions (v0.17.0.12.27).
//
// Not an MCP tool -- invoked from `ta audit human-verify` in the CLI
// process, since the review pass runs independently of any live agent
// session. Closes 12.26's feedback-loop gap: `ta_human_verify` auto-confirms
// when an opinion pass and a validator pass agree, but the validator only
// checks whether the opinion's reasoning is internally *sound* -- it can't
// catch a mistake baked into a reasoning style both passes share. This
// module:
//
//   1. Samples `Commit`-decision entries from `.ta/human-verify-audit.jsonl`
//      that haven't been red-team-reviewed yet.
//   2. Runs each through an explicitly adversarial pass -- "assume this is
//      wrong, find the failure the opinion+validator pair missed," never a
//      second soundness re-check -- producing confirmed-correct or
//      confirmed-miss.
//   3. Confirmed misses are appended to `.ta/verify-failures.jsonl`, a
//      durable (committed) calibration dataset, and folded into future
//      opinion/validator prompts as few-shot context for that workload
//      (see `human_verify::render_few_shot_misses`).
//   4. Metrics (auto-confirm rate, red-team-catch rate, false-auto-confirm
//      rate) are computed per workload_type from the invocation log and the
//      failures dataset.
//   5. When misses cluster above a configurable rate, a threshold-tightening
//      *proposal* is appended to `.ta/verify-threshold-proposals.jsonl` --
//      nothing in this module ever writes `.ta/workflow.toml`. Thresholds
//      are a trust boundary; changing them without a human approving the
//      change would defeat the point.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ta_decision::{Decision, DecisionThresholds};

use crate::tools::human_verify::{
    extract_marker_json, load_thresholds, spawn_headless_and_capture, write_context_file,
    OpinionResult, ValidatorResult,
};

// ── Reading the audit trail ───────────────────────────────────────────────

/// Owned, deserializable mirror of `human_verify`'s private
/// `HumanVerifyAuditEntry` JSON shape, for reading back
/// `.ta/human-verify-audit.jsonl`.
#[derive(Debug, Clone, Deserialize)]
pub struct HumanVerifyAuditRecord {
    pub id: Uuid,
    pub timestamp: String,
    pub question: String,
    #[serde(default)]
    pub context: Option<String>,
    pub workload_type: String,
    pub opinion: OpinionResult,
    pub validator: ValidatorResult,
    pub decision: Decision,
}

fn audit_log_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".ta").join("human-verify-audit.jsonl")
}

/// All audit entries ever written. Empty (not an error) if the log doesn't
/// exist yet -- a fresh project has auto-confirmed nothing.
pub fn read_audit_entries(workspace_root: &Path) -> Vec<HumanVerifyAuditRecord> {
    let Ok(content) = std::fs::read_to_string(audit_log_path(workspace_root)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// ── Review cursor (.ta/verify-audit-reviewed.jsonl) ───────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct ReviewedEntry {
    id: Uuid,
    reviewed_at: String,
}

fn reviewed_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".ta")
        .join("verify-audit-reviewed.jsonl")
}

/// IDs of audit entries a red-team pass has already looked at -- a cursor,
/// not a log to prune.
pub fn load_reviewed_ids(workspace_root: &Path) -> HashSet<Uuid> {
    let Ok(content) = std::fs::read_to_string(reviewed_path(workspace_root)) else {
        return HashSet::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ReviewedEntry>(l).ok())
        .map(|e| e.id)
        .collect()
}

pub fn mark_reviewed(workspace_root: &Path, id: Uuid) {
    let path = reviewed_path(workspace_root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = ReviewedEntry {
        id,
        reviewed_at: Utc::now().to_rfc3339(),
    };
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

/// Unreviewed entries, optionally filtered by `workload_type` and capped at
/// `sample` (first-come order -- oldest unreviewed first, matching JSONL
/// append order).
pub fn select_unreviewed<'a>(
    entries: &'a [HumanVerifyAuditRecord],
    reviewed: &HashSet<Uuid>,
    workload: Option<&str>,
    sample: Option<usize>,
) -> Vec<&'a HumanVerifyAuditRecord> {
    let mut selected: Vec<&HumanVerifyAuditRecord> = entries
        .iter()
        .filter(|e| !reviewed.contains(&e.id))
        .filter(|e| workload.map(|w| e.workload_type == w).unwrap_or(true))
        .collect();
    if let Some(n) = sample {
        selected.truncate(n);
    }
    selected
}

// ── Red-team pass ──────────────────────────────────────────────────────────

/// Outcome of an adversarial red-team review of one auto-confirmed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedTeamVerdict {
    ConfirmedCorrect,
    ConfirmedMiss,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedTeamResult {
    pub verdict: RedTeamVerdict,
    pub explanation: String,
}

/// The red-team pass, abstracted so sampling/gating logic is testable
/// without spawning real subprocesses -- same reason
/// `human_verify::SyntheticPipeline` exists.
pub trait RedTeamPipeline {
    fn review(&self, entry: &HumanVerifyAuditRecord) -> Result<RedTeamResult, String>;
}

const RED_TEAM_MARKER: &str = "RED_TEAM_VERDICT_JSON:";

/// Production `RedTeamPipeline`: spawns an independent headless `ta run
/// --headless` subprocess per review, reusing `human_verify`'s
/// spawn-and-capture helpers. Distinctly framed from the validator pass --
/// "assume this is wrong, find the failure," never "is this reasoning
/// sound" (item 2's explicit adversarial framing requirement).
pub struct HeadlessRedTeamPipeline {
    workspace_root: PathBuf,
    ta_bin: PathBuf,
}

impl HeadlessRedTeamPipeline {
    pub fn new(workspace_root: PathBuf, ta_bin: PathBuf) -> Self {
        Self {
            workspace_root,
            ta_bin,
        }
    }
}

impl RedTeamPipeline for HeadlessRedTeamPipeline {
    fn review(&self, entry: &HumanVerifyAuditRecord) -> Result<RedTeamResult, String> {
        let content = build_red_team_context(entry);
        let context_path = write_context_file(&self.workspace_root, "red-team-context", &content)
            .map_err(|e| format!("failed to write red-team context file: {e}"))?;

        let result = spawn_headless_and_capture(
            &self.ta_bin,
            &self.workspace_root,
            "ta audit human-verify: red-team pass",
            &context_path,
        );
        let _ = std::fs::remove_file(&context_path);
        let (stdout, stderr) = result?;
        extract_marker_json(&stdout, &stderr, RED_TEAM_MARKER)
    }
}

fn build_red_team_context(entry: &HumanVerifyAuditRecord) -> String {
    let mut ctx = String::new();
    ctx.push_str(
        "# Red-Team Review — Adversarial Validation of an Auto-Confirmed Verification\n\n",
    );
    ctx.push_str(
        "An opinion pass and an independent validator pass already agreed on the \
         answer below, and it was auto-confirmed. Do **not** re-check whether their \
         reasoning is internally sound — they already checked that, and a second \
         soundness check finds nothing new if they share a blind spot. Instead: \
         **assume the auto-confirmed answer is wrong, and find the failure the \
         opinion+validator pair missed.** Look for bias, an exploitable framing, an \
         unstated assumption, or a risk category both passes under-weighted.\n\n",
    );
    ctx.push_str(&format!("## Original question\n\n{}\n\n", entry.question));
    if let Some(c) = &entry.context {
        ctx.push_str(&format!(
            "## Context given to the original pipeline\n\n{c}\n\n"
        ));
    }
    ctx.push_str(&format!(
        "## Opinion pass\n\n**Answer**: {}\n\n**Reasoning**: {}\n\n**Confidence**: {:.2}\n\n",
        entry.opinion.answer, entry.opinion.reasoning, entry.opinion.confidence
    ));
    ctx.push_str(&format!(
        "## Validator pass\n\n**Verdict**: {:?}\n\n**Risk score**: {}\n\n**Confidence**: {:.2}\n\n**Reasoning**: {}\n\n",
        entry.validator.verdict,
        entry.validator.risk_score,
        entry.validator.confidence,
        entry.validator.reasoning
    ));
    ctx.push_str(
        "## Your job\n\n\
         Produce a verdict: \"confirmed_correct\" (you tried to find a failure and \
         couldn't -- the auto-confirm was right) or \"confirmed_miss\" (you found a \
         real failure the pair missed). Then explain the specific failure (or, for \
         confirmed_correct, what you checked and ruled out).\n\n\
         ## Output protocol\n\n\
         Print exactly one final line to stdout of the form:\n\n\
         RED_TEAM_VERDICT_JSON: {\"verdict\": \"confirmed_correct\", \"explanation\": \"...\"}\n\n\
         Print nothing after that line.\n",
    );
    ctx
}

// ── Durable calibration dataset (.ta/verify-failures.jsonl, committed) ────

/// A confirmed miss: an auto-confirmed answer a red-team pass found to
/// actually be wrong. Committed to git -- a durable calibration dataset,
/// not a per-run operational log (v0.17.0.12.27 item 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyFailureRecord {
    pub id: Uuid,
    pub audit_entry_id: Uuid,
    pub workload_type: String,
    pub question: String,
    #[serde(default)]
    pub context: Option<String>,
    pub opinion: OpinionResult,
    pub validator: ValidatorResult,
    pub red_team_explanation: String,
    pub timestamp: String,
}

fn verify_failures_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".ta").join("verify-failures.jsonl")
}

pub fn append_verify_failure(
    workspace_root: &Path,
    record: &VerifyFailureRecord,
) -> std::io::Result<()> {
    let path = verify_failures_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    use std::io::Write as _;
    writeln!(f, "{line}")?;
    Ok(())
}

fn load_all_failures(workspace_root: &Path) -> Vec<VerifyFailureRecord> {
    let Ok(content) = std::fs::read_to_string(verify_failures_path(workspace_root)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Rolling sample of the `limit` most recent confirmed misses for
/// `workload_type` -- folded into future opinion/validator prompts as
/// few-shot context (v0.17.0.12.27 item 4a). Append order is chronological
/// order, so the tail of the filtered list is the most recent.
pub fn load_recent_misses(
    workspace_root: &Path,
    workload_type: &str,
    limit: usize,
) -> Vec<VerifyFailureRecord> {
    let mut matches: Vec<VerifyFailureRecord> = load_all_failures(workspace_root)
        .into_iter()
        .filter(|r| r.workload_type == workload_type)
        .collect();
    if matches.len() > limit {
        matches = matches.split_off(matches.len() - limit);
    }
    matches
}

// ── Invocation log (.ta/human-verify-invocations.jsonl) ───────────────────

#[derive(Debug, Serialize)]
struct InvocationRecord<'a> {
    timestamp: String,
    workload_type: &'a str,
    decision: Decision,
}

#[derive(Debug, Deserialize)]
struct InvocationRecordOwned {
    workload_type: String,
    decision: Decision,
}

fn invocations_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".ta")
        .join("human-verify-invocations.jsonl")
}

/// Record one *rendered* gate decision (every branch: Commit, Reject,
/// Rework, Escalate) -- the metrics denominator
/// `.ta/human-verify-audit.jsonl` alone can't provide, since that log only
/// ever gets a line on the Commit path (v0.17.0.12.26's design, unchanged
/// here). Called by `human_verify::handle_human_verify_with_pipeline` right
/// after `decide()` runs.
pub fn record_invocation(workspace_root: &Path, workload_type: &str, decision: Decision) {
    let path = invocations_path(workspace_root);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = InvocationRecord {
        timestamp: Utc::now().to_rfc3339(),
        workload_type,
        decision,
    };
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{line}");
    }
}

fn read_invocations(workspace_root: &Path) -> Vec<InvocationRecordOwned> {
    let Ok(content) = std::fs::read_to_string(invocations_path(workspace_root)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// ── Metrics ────────────────────────────────────────────────────────────────

/// Per-`workload_type` metrics (v0.17.0.12.27 item 5). `red_team_catch_rate`
/// and `false_auto_confirm_rate` are sampled estimates bounded by how much
/// of the Commit population has been reviewed so far (`reviewed_count`),
/// not a full-population guarantee -- reviewing more narrows them.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadMetrics {
    pub workload_type: String,
    pub total_decisions: usize,
    pub auto_confirm_count: usize,
    pub auto_confirm_rate: f64,
    pub reviewed_count: usize,
    pub confirmed_miss_count: usize,
    pub red_team_catch_rate: f64,
    pub false_auto_confirm_rate: f64,
}

pub fn compute_metrics(workspace_root: &Path) -> Vec<WorkloadMetrics> {
    let invocations = read_invocations(workspace_root);
    let audit_entries = read_audit_entries(workspace_root);
    let reviewed = load_reviewed_ids(workspace_root);
    let failures = load_all_failures(workspace_root);

    let mut workloads: Vec<String> = invocations
        .iter()
        .map(|i| i.workload_type.clone())
        .chain(audit_entries.iter().map(|e| e.workload_type.clone()))
        .collect();
    workloads.sort();
    workloads.dedup();

    workloads
        .into_iter()
        .map(|workload_type| {
            let workload_invocations: Vec<&InvocationRecordOwned> = invocations
                .iter()
                .filter(|i| i.workload_type == workload_type)
                .collect();
            let total_decisions = workload_invocations.len();
            let auto_confirm_count = workload_invocations
                .iter()
                .filter(|i| i.decision == Decision::Commit)
                .count();
            let auto_confirm_rate = if total_decisions > 0 {
                auto_confirm_count as f64 / total_decisions as f64
            } else {
                0.0
            };

            let reviewed_count = audit_entries
                .iter()
                .filter(|e| e.workload_type == workload_type && reviewed.contains(&e.id))
                .count();
            let confirmed_miss_count = failures
                .iter()
                .filter(|f| f.workload_type == workload_type)
                .count();
            let red_team_catch_rate = if reviewed_count > 0 {
                confirmed_miss_count as f64 / reviewed_count as f64
            } else {
                0.0
            };
            let false_auto_confirm_rate = if auto_confirm_count > 0 {
                confirmed_miss_count as f64 / auto_confirm_count as f64
            } else {
                0.0
            };

            WorkloadMetrics {
                workload_type,
                total_decisions,
                auto_confirm_count,
                auto_confirm_rate,
                reviewed_count,
                confirmed_miss_count,
                red_team_catch_rate,
                false_auto_confirm_rate,
            }
        })
        .collect()
}

// ── Threshold-tightening proposals (never auto-applied) ───────────────────

#[derive(Debug, Clone, Copy)]
pub struct RedTeamThresholdConfig {
    pub miss_rate_threshold: f64,
    pub min_sample_size: usize,
    pub tighten_min_confidence_step: f64,
    pub tighten_max_risk_step: u32,
}

impl Default for RedTeamThresholdConfig {
    fn default() -> Self {
        Self {
            miss_rate_threshold: 0.25,
            min_sample_size: 5,
            tighten_min_confidence_step: 0.05,
            tighten_max_risk_step: 5,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
struct PartialRedTeamConfig {
    miss_rate_threshold: Option<f64>,
    min_sample_size: Option<usize>,
    tighten_min_confidence_step: Option<f64>,
    tighten_max_risk_step: Option<u32>,
}

impl PartialRedTeamConfig {
    fn apply_over(&self, base: RedTeamThresholdConfig) -> RedTeamThresholdConfig {
        RedTeamThresholdConfig {
            miss_rate_threshold: self.miss_rate_threshold.unwrap_or(base.miss_rate_threshold),
            min_sample_size: self.min_sample_size.unwrap_or(base.min_sample_size),
            tighten_min_confidence_step: self
                .tighten_min_confidence_step
                .unwrap_or(base.tighten_min_confidence_step),
            tighten_max_risk_step: self
                .tighten_max_risk_step
                .unwrap_or(base.tighten_max_risk_step),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RedTeamWorkflowToml {
    verify_redteam: std::collections::HashMap<String, PartialRedTeamConfig>,
}

/// Resolve `RedTeamThresholdConfig` for `workload_type`: `[verify_redteam.default]`
/// layers over the built-in default, then `[verify_redteam.<workload_type>]`
/// layers over that -- same override pattern as `human_verify::load_thresholds`.
pub fn load_redteam_config(workspace_root: &Path, workload_type: &str) -> RedTeamThresholdConfig {
    let toml: RedTeamWorkflowToml =
        std::fs::read_to_string(workspace_root.join(".ta").join("workflow.toml"))
            .ok()
            .and_then(|c| toml::from_str(&c).ok())
            .unwrap_or_default();
    let base = toml
        .verify_redteam
        .get("default")
        .map(|p| p.apply_over(RedTeamThresholdConfig::default()))
        .unwrap_or_default();
    toml.verify_redteam
        .get(workload_type)
        .map(|p| p.apply_over(base))
        .unwrap_or(base)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdProposal {
    pub workload_type: String,
    pub miss_rate: f64,
    pub sample_size: usize,
    pub current_thresholds: DecisionThresholds,
    pub proposed_thresholds: DecisionThresholds,
    pub generated_at: String,
}

/// Pure decision function: never writes `.ta/workflow.toml`. A proposal is
/// surfaced for a human to review and apply by hand -- thresholds are a
/// trust boundary, so changing them silently would defeat the point
/// (v0.17.0.12.27 item 4b). Returns `None` below the configured miss-rate
/// threshold, or when the reviewed sample is too small to trust.
pub fn maybe_propose_threshold_tightening(
    workload_type: &str,
    current: DecisionThresholds,
    miss_rate: f64,
    sample_size: usize,
    config: RedTeamThresholdConfig,
) -> Option<ThresholdProposal> {
    if sample_size < config.min_sample_size || miss_rate <= config.miss_rate_threshold {
        return None;
    }
    let proposed = DecisionThresholds {
        min_confidence: (current.min_confidence + config.tighten_min_confidence_step).min(1.0),
        max_risk_score: current
            .max_risk_score
            .saturating_sub(config.tighten_max_risk_step),
        escalate_risk_score: current.escalate_risk_score,
    };
    Some(ThresholdProposal {
        workload_type: workload_type.to_string(),
        miss_rate,
        sample_size,
        current_thresholds: current,
        proposed_thresholds: proposed,
        generated_at: Utc::now().to_rfc3339(),
    })
}

fn threshold_proposals_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".ta")
        .join("verify-threshold-proposals.jsonl")
}

pub fn append_threshold_proposal(
    workspace_root: &Path,
    proposal: &ThresholdProposal,
) -> std::io::Result<()> {
    let path = threshold_proposals_path(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(proposal)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    use std::io::Write as _;
    writeln!(f, "{line}")?;
    Ok(())
}

/// All proposals ever generated, for `ta audit human-verify proposals`.
pub fn load_all_proposals(workspace_root: &Path) -> Vec<ThresholdProposal> {
    let Ok(content) = std::fs::read_to_string(threshold_proposals_path(workspace_root)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// ── Orchestration entry point (called by `ta audit human-verify sample`) ──

#[derive(Debug, Default, Serialize)]
pub struct RedTeamRunSummary {
    pub reviewed: usize,
    pub confirmed_correct: usize,
    pub confirmed_miss: usize,
    pub errors: Vec<String>,
    pub proposals: Vec<ThresholdProposal>,
}

/// Sample unreviewed `Commit` entries, run each through `pipeline`, record
/// confirmed misses, mark all as reviewed, then check touched workloads for
/// a threshold-tightening proposal. This is the one function the CLI calls.
pub fn run_redteam_review(
    workspace_root: &Path,
    pipeline: &dyn RedTeamPipeline,
    sample: Option<usize>,
    workload: Option<&str>,
) -> RedTeamRunSummary {
    let entries = read_audit_entries(workspace_root);
    let reviewed_before = load_reviewed_ids(workspace_root);
    let selected = select_unreviewed(&entries, &reviewed_before, workload, sample);

    let mut summary = RedTeamRunSummary::default();
    let mut touched_workloads: Vec<String> = Vec::new();

    for entry in selected {
        match pipeline.review(entry) {
            Ok(result) => {
                summary.reviewed += 1;
                mark_reviewed(workspace_root, entry.id);
                if !touched_workloads.contains(&entry.workload_type) {
                    touched_workloads.push(entry.workload_type.clone());
                }
                match result.verdict {
                    RedTeamVerdict::ConfirmedCorrect => summary.confirmed_correct += 1,
                    RedTeamVerdict::ConfirmedMiss => {
                        summary.confirmed_miss += 1;
                        let failure = VerifyFailureRecord {
                            id: Uuid::new_v4(),
                            audit_entry_id: entry.id,
                            workload_type: entry.workload_type.clone(),
                            question: entry.question.clone(),
                            context: entry.context.clone(),
                            opinion: entry.opinion.clone(),
                            validator: entry.validator.clone(),
                            red_team_explanation: result.explanation.clone(),
                            timestamp: Utc::now().to_rfc3339(),
                        };
                        if let Err(e) = append_verify_failure(workspace_root, &failure) {
                            summary.errors.push(format!(
                                "failed to append verify-failures.jsonl for entry {}: {e}",
                                entry.id
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                summary.errors.push(format!(
                    "red-team review failed for entry {}: {e}",
                    entry.id
                ));
            }
        }
    }

    // Re-read reviewed/failures fresh (this run's writes above are already
    // on disk) so the miss rate reflects everything reviewed to date, not
    // just this run's sample.
    let reviewed_now = load_reviewed_ids(workspace_root);
    let failures_now = load_all_failures(workspace_root);
    for workload_type in &touched_workloads {
        let reviewed_count = entries
            .iter()
            .filter(|e| &e.workload_type == workload_type && reviewed_now.contains(&e.id))
            .count();
        if reviewed_count == 0 {
            continue;
        }
        let miss_count = failures_now
            .iter()
            .filter(|f| &f.workload_type == workload_type)
            .count();
        let miss_rate = miss_count as f64 / reviewed_count as f64;
        let config = load_redteam_config(workspace_root, workload_type);
        let current_thresholds = load_thresholds(workspace_root, workload_type);
        if let Some(proposal) = maybe_propose_threshold_tightening(
            workload_type,
            current_thresholds,
            miss_rate,
            reviewed_count,
            config,
        ) {
            if let Err(e) = append_threshold_proposal(workspace_root, &proposal) {
                summary
                    .errors
                    .push(format!("failed to append threshold proposal: {e}"));
            }
            summary.proposals.push(proposal);
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_decision::Verdict;
    use tempfile::tempdir;

    fn sample_opinion() -> OpinionResult {
        OpinionResult {
            answer: "Yes, ship it.".to_string(),
            reasoning: "All checks green.".to_string(),
            confidence: 0.95,
        }
    }

    fn sample_validator() -> ValidatorResult {
        ValidatorResult {
            verdict: Verdict::Pass,
            risk_score: 5,
            confidence: 0.95,
            reasoning: "Sound reasoning, low stakes.".to_string(),
        }
    }

    fn write_audit_entry(workspace_root: &Path, id: Uuid, workload_type: &str, question: &str) {
        let path = audit_log_path(workspace_root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let line = serde_json::json!({
            "id": id,
            "timestamp": "2026-07-16T00:00:00Z",
            "question": question,
            "context": "some context",
            "workload_type": workload_type,
            "opinion": sample_opinion(),
            "validator": sample_validator(),
            "decision": "commit",
        });
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write as _;
        writeln!(f, "{}", line).unwrap();
    }

    struct FakeRedTeamPipeline {
        verdict: RedTeamVerdict,
        explanation: String,
    }

    impl RedTeamPipeline for FakeRedTeamPipeline {
        fn review(&self, _entry: &HumanVerifyAuditRecord) -> Result<RedTeamResult, String> {
            Ok(RedTeamResult {
                verdict: self.verdict,
                explanation: self.explanation.clone(),
            })
        }
    }

    #[test]
    fn read_audit_entries_round_trips_and_defaults_empty() {
        let dir = tempdir().unwrap();
        assert!(read_audit_entries(dir.path()).is_empty());

        let id = Uuid::new_v4();
        write_audit_entry(dir.path(), id, "docs", "Should we ship?");
        let entries = read_audit_entries(dir.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].workload_type, "docs");
        assert_eq!(entries[0].decision, Decision::Commit);
    }

    #[test]
    fn mark_reviewed_round_trips_through_load_reviewed_ids() {
        let dir = tempdir().unwrap();
        let id = Uuid::new_v4();
        assert!(!load_reviewed_ids(dir.path()).contains(&id));
        mark_reviewed(dir.path(), id);
        assert!(load_reviewed_ids(dir.path()).contains(&id));
    }

    #[test]
    fn select_unreviewed_filters_reviewed_workload_and_caps_sample() {
        let dir = tempdir().unwrap();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_c = Uuid::new_v4();
        write_audit_entry(dir.path(), id_a, "docs", "Q1");
        write_audit_entry(dir.path(), id_b, "docs", "Q2");
        write_audit_entry(dir.path(), id_c, "security", "Q3");

        let entries = read_audit_entries(dir.path());
        let mut reviewed = HashSet::new();
        reviewed.insert(id_a);

        let selected = select_unreviewed(&entries, &reviewed, Some("docs"), None);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, id_b);

        let selected_all = select_unreviewed(&entries, &HashSet::new(), None, Some(2));
        assert_eq!(selected_all.len(), 2);
    }

    #[test]
    fn run_redteam_review_confirmed_miss_appends_failure_and_marks_reviewed() {
        let dir = tempdir().unwrap();
        let id = Uuid::new_v4();
        write_audit_entry(dir.path(), id, "docs", "Should we ship?");

        let pipeline = FakeRedTeamPipeline {
            verdict: RedTeamVerdict::ConfirmedMiss,
            explanation: "The opinion ignored a known edge case.".to_string(),
        };

        let summary = run_redteam_review(dir.path(), &pipeline, None, None);
        assert_eq!(summary.reviewed, 1);
        assert_eq!(summary.confirmed_miss, 1);
        assert_eq!(summary.confirmed_correct, 0);
        assert!(summary.errors.is_empty());

        assert!(load_reviewed_ids(dir.path()).contains(&id));
        let failures = load_all_failures(dir.path());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].audit_entry_id, id);
        assert_eq!(failures[0].workload_type, "docs");
        assert_eq!(
            failures[0].red_team_explanation,
            "The opinion ignored a known edge case."
        );
    }

    #[test]
    fn run_redteam_review_confirmed_correct_marks_reviewed_without_failure() {
        let dir = tempdir().unwrap();
        let id = Uuid::new_v4();
        write_audit_entry(dir.path(), id, "docs", "Should we ship?");

        let pipeline = FakeRedTeamPipeline {
            verdict: RedTeamVerdict::ConfirmedCorrect,
            explanation: "Checked for bias, found none.".to_string(),
        };

        let summary = run_redteam_review(dir.path(), &pipeline, None, None);
        assert_eq!(summary.reviewed, 1);
        assert_eq!(summary.confirmed_correct, 1);
        assert!(load_reviewed_ids(dir.path()).contains(&id));
        assert!(load_all_failures(dir.path()).is_empty());
    }

    #[test]
    fn run_redteam_review_does_not_reprocess_already_reviewed_entries() {
        let dir = tempdir().unwrap();
        let id = Uuid::new_v4();
        write_audit_entry(dir.path(), id, "docs", "Should we ship?");
        mark_reviewed(dir.path(), id);

        let pipeline = FakeRedTeamPipeline {
            verdict: RedTeamVerdict::ConfirmedMiss,
            explanation: "should not run".to_string(),
        };
        let summary = run_redteam_review(dir.path(), &pipeline, None, None);
        assert_eq!(summary.reviewed, 0);
        assert!(load_all_failures(dir.path()).is_empty());
    }

    #[test]
    fn load_recent_misses_filters_by_workload_and_caps_at_limit() {
        let dir = tempdir().unwrap();
        for i in 0..3 {
            let record = VerifyFailureRecord {
                id: Uuid::new_v4(),
                audit_entry_id: Uuid::new_v4(),
                workload_type: "docs".to_string(),
                question: format!("Q{i}"),
                context: None,
                opinion: sample_opinion(),
                validator: sample_validator(),
                red_team_explanation: format!("explanation {i}"),
                timestamp: Utc::now().to_rfc3339(),
            };
            append_verify_failure(dir.path(), &record).unwrap();
        }
        let other_workload = VerifyFailureRecord {
            id: Uuid::new_v4(),
            audit_entry_id: Uuid::new_v4(),
            workload_type: "security".to_string(),
            question: "Q-other".to_string(),
            context: None,
            opinion: sample_opinion(),
            validator: sample_validator(),
            red_team_explanation: "unrelated".to_string(),
            timestamp: Utc::now().to_rfc3339(),
        };
        append_verify_failure(dir.path(), &other_workload).unwrap();

        let recent = load_recent_misses(dir.path(), "docs", 2);
        assert_eq!(recent.len(), 2);
        // Rolling sample keeps the most recent (tail) entries.
        assert_eq!(recent[0].question, "Q1");
        assert_eq!(recent[1].question, "Q2");

        assert!(load_recent_misses(dir.path(), "nonexistent", 5).is_empty());
    }

    #[test]
    fn compute_metrics_aggregates_mixed_hits_and_misses_per_workload() {
        let dir = tempdir().unwrap();

        record_invocation(dir.path(), "docs", Decision::Commit);
        record_invocation(dir.path(), "docs", Decision::Commit);
        record_invocation(dir.path(), "docs", Decision::Escalate);
        record_invocation(dir.path(), "security", Decision::Escalate);

        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        write_audit_entry(dir.path(), id_a, "docs", "Q1");
        write_audit_entry(dir.path(), id_b, "docs", "Q2");
        mark_reviewed(dir.path(), id_a);
        mark_reviewed(dir.path(), id_b);

        let failure = VerifyFailureRecord {
            id: Uuid::new_v4(),
            audit_entry_id: id_a,
            workload_type: "docs".to_string(),
            question: "Q1".to_string(),
            context: None,
            opinion: sample_opinion(),
            validator: sample_validator(),
            red_team_explanation: "missed an edge case".to_string(),
            timestamp: Utc::now().to_rfc3339(),
        };
        append_verify_failure(dir.path(), &failure).unwrap();

        let metrics = compute_metrics(dir.path());
        let docs = metrics
            .iter()
            .find(|m| m.workload_type == "docs")
            .expect("docs metrics present");

        assert_eq!(docs.total_decisions, 3);
        assert_eq!(docs.auto_confirm_count, 2);
        assert!((docs.auto_confirm_rate - (2.0 / 3.0)).abs() < 1e-9);
        assert_eq!(docs.reviewed_count, 2);
        assert_eq!(docs.confirmed_miss_count, 1);
        assert!((docs.red_team_catch_rate - 0.5).abs() < 1e-9);
        assert!((docs.false_auto_confirm_rate - 0.5).abs() < 1e-9);

        let security = metrics
            .iter()
            .find(|m| m.workload_type == "security")
            .expect("security metrics present");
        assert_eq!(security.total_decisions, 1);
        assert_eq!(security.auto_confirm_count, 0);
        assert_eq!(security.auto_confirm_rate, 0.0);
    }

    #[test]
    fn compute_metrics_on_empty_project_returns_empty() {
        let dir = tempdir().unwrap();
        assert!(compute_metrics(dir.path()).is_empty());
    }

    #[test]
    fn threshold_tightening_fires_only_above_rate_and_sample_size() {
        let current = DecisionThresholds::default();
        let config = RedTeamThresholdConfig::default();

        // Below the miss-rate threshold: no proposal.
        assert!(maybe_propose_threshold_tightening("docs", current, 0.1, 10, config).is_none());

        // Sample too small, even though miss rate is high: no proposal.
        assert!(maybe_propose_threshold_tightening("docs", current, 0.9, 2, config).is_none());

        // Above threshold and enough sample: proposal fires with tightened values.
        let proposal =
            maybe_propose_threshold_tightening("docs", current, 0.5, 10, config).unwrap();
        assert_eq!(proposal.workload_type, "docs");
        assert!(proposal.proposed_thresholds.min_confidence > current.min_confidence);
        assert!(proposal.proposed_thresholds.max_risk_score < current.max_risk_score);
        // escalate_risk_score is untouched by tightening.
        assert_eq!(
            proposal.proposed_thresholds.escalate_risk_score,
            current.escalate_risk_score
        );
    }

    #[test]
    fn threshold_tightening_never_writes_workflow_toml() {
        let dir = tempdir().unwrap();
        let workflow_toml = dir.path().join(".ta").join("workflow.toml");
        let current = DecisionThresholds::default();
        let config = RedTeamThresholdConfig::default();

        let proposal =
            maybe_propose_threshold_tightening("docs", current, 0.9, 10, config).unwrap();
        append_threshold_proposal(dir.path(), &proposal).unwrap();

        // A proposal was recorded, but workflow.toml itself was never
        // created or written by any function in this module.
        assert!(!workflow_toml.exists());
        let proposals = load_all_proposals(dir.path());
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].workload_type, "docs");
    }

    #[test]
    fn load_redteam_config_layers_default_under_workload_override() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            "[verify_redteam.default]\nmiss_rate_threshold = 0.3\nmin_sample_size = 10\n\n\
             [verify_redteam.docs]\nmin_sample_size = 3\n",
        )
        .unwrap();

        let docs_config = load_redteam_config(dir.path(), "docs");
        assert_eq!(docs_config.min_sample_size, 3);
        // Inherited from [default] since [docs] doesn't override it.
        assert_eq!(docs_config.miss_rate_threshold, 0.3);

        let other_config = load_redteam_config(dir.path(), "security");
        assert_eq!(other_config.min_sample_size, 10);
    }

    #[test]
    fn run_redteam_review_proposes_tightening_when_misses_cluster() {
        let dir = tempdir().unwrap();
        // Configure a low bar so the test doesn't need a huge sample.
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("workflow.toml"),
            "[verify_redteam.docs]\nmiss_rate_threshold = 0.4\nmin_sample_size = 2\n",
        )
        .unwrap();

        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        write_audit_entry(dir.path(), id_a, "docs", "Q1");
        write_audit_entry(dir.path(), id_b, "docs", "Q2");

        // Both entries confirmed-miss -> 100% miss rate for this workload.
        let pipeline = FakeRedTeamPipeline {
            verdict: RedTeamVerdict::ConfirmedMiss,
            explanation: "systemic bias".to_string(),
        };
        let summary = run_redteam_review(dir.path(), &pipeline, None, Some("docs"));
        assert_eq!(summary.confirmed_miss, 2);
        assert_eq!(summary.proposals.len(), 1);
        assert_eq!(summary.proposals[0].workload_type, "docs");

        let proposals_on_disk = load_all_proposals(dir.path());
        assert_eq!(proposals_on_disk.len(), 1);
    }
}
