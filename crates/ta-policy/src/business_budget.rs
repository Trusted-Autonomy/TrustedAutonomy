//! Workflow-defined business-metric budget guardrails (v0.17.5.2).
//!
//! A second, distinct budget concept alongside [`crate::document::BudgetConfig`]
//! (the existing LLM-token budget, unchanged by this module). Where
//! `BudgetConfig` tracks resource consumption (tokens spent), this module
//! tracks an arbitrary-unit business metric a workflow declares — dollars,
//! trade count, anything domain-specific — against a stated objective.
//!
//! Hard limits ([`BudgetCheckResult::HardLimitExceeded`]) are deterministic
//! pre-gate checks: no confidence score ever overrides one, and a violation
//! must reject the action before any probabilistic verify/approve pass runs.
//! Soft limits ([`BudgetCheckResult::SoftLimitCrossed`]) feed the Decision
//! gate as an escalation signal — the same "confidence/proximity-to-limit
//! downgrades autonomy" pattern already proven by
//! `ta-brain::route()`'s `AUTO_SECURITY_CONFIDENCE_THRESHOLD` downgrade.
//!
//! The running ledger is an append-only JSONL log, the same pattern used by
//! `.ta/human-verify-audit.jsonl` (`ta-mcp-gateway::tools::human_verify`):
//! a private borrow-based writer entry type, a public owned reader record
//! type, `OpenOptions::new().create(true).append(true)`, and a
//! missing-file-tolerant reader.

use std::io;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A workflow's business-metric budget guardrail, resolved from
/// `ta-workflow::WorkflowBudget` (or hand-constructed by callers that don't
/// depend on `ta-workflow`, e.g. `ta-daemon`'s team-session runtime).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetGuardrails {
    /// Arbitrary label for the unit being budgeted (e.g. "usd", "trade_count").
    pub metric: String,
    /// Total budget available for the session's lifetime, in `metric` units.
    pub total: f64,
    /// Hard limit: no single action may exceed this percentage of `total`.
    #[serde(default)]
    pub per_action_max_pct: Option<f64>,
    /// Soft limit: cumulative spend crossing this percentage of `total`
    /// forces every subsequent action to `Escalate`.
    #[serde(default)]
    pub soft_threshold_pct: Option<f64>,
    /// Human-readable objective this budget serves.
    #[serde(default)]
    pub objective: Option<String>,
}

/// One append-only entry in a session's business-budget ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetLedgerEntry {
    /// Free-text description of the action that consumed budget.
    pub action: String,
    /// Amount consumed by this action, in the guardrail's `metric` units.
    pub amount: f64,
    /// Cumulative total after this entry (previous running total + `amount`).
    pub running_total: f64,
    pub timestamp: DateTime<Utc>,
}

/// Result of checking a proposed action against a [`BudgetGuardrails`].
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetCheckResult {
    /// Within both hard and soft limits.
    Ok,
    /// Deterministic hard-limit violation — reject before any verify pass.
    HardLimitExceeded(String),
    /// Within hard limits but crosses the soft threshold — force `Escalate`.
    SoftLimitCrossed(String),
}

impl BudgetCheckResult {
    pub fn is_hard_limit_exceeded(&self) -> bool {
        matches!(self, BudgetCheckResult::HardLimitExceeded(_))
    }

    pub fn is_soft_limit_crossed(&self) -> bool {
        matches!(self, BudgetCheckResult::SoftLimitCrossed(_))
    }
}

/// Checks a proposed action of `amount` (in `guardrails.metric` units)
/// against the hard per-action cap, the total budget, and the soft
/// escalation threshold, given `ledger_total_before` (the running total
/// accumulated so far, from [`ledger_running_total`]).
///
/// Hard-limit checks (per-action cap, total budget) take priority over the
/// soft-limit check — an action that is rejected outright is never also
/// reported as merely crossing the soft threshold.
pub fn check_budget(
    guardrails: &BudgetGuardrails,
    ledger_total_before: f64,
    amount: f64,
) -> BudgetCheckResult {
    if let Some(per_action_max_pct) = guardrails.per_action_max_pct {
        let cap = guardrails.total * per_action_max_pct / 100.0;
        if amount > cap {
            return BudgetCheckResult::HardLimitExceeded(format!(
                "action amount {amount:.2} {metric} exceeds the per-action cap of {cap:.2} \
                 {metric} ({per_action_max_pct:.1}% of the {total:.2} {metric} total budget)",
                metric = guardrails.metric,
                total = guardrails.total,
            ));
        }
    }

    let running_total_after = ledger_total_before + amount;
    if running_total_after > guardrails.total {
        return BudgetCheckResult::HardLimitExceeded(format!(
            "action would bring the running total to {running_total_after:.2} {metric}, \
             exceeding the {total:.2} {metric} total budget ({ledger_total_before:.2} \
             {metric} already spent)",
            metric = guardrails.metric,
            total = guardrails.total,
        ));
    }

    if let Some(soft_threshold_pct) = guardrails.soft_threshold_pct {
        let threshold = guardrails.total * soft_threshold_pct / 100.0;
        if running_total_after >= threshold {
            return BudgetCheckResult::SoftLimitCrossed(format!(
                "running total of {running_total_after:.2} {metric} crosses the soft \
                 threshold of {threshold:.2} {metric} ({soft_threshold_pct:.1}% of the \
                 {total:.2} {metric} total budget) — forcing escalation",
                metric = guardrails.metric,
                total = guardrails.total,
            ));
        }
    }

    BudgetCheckResult::Ok
}

/// Appends one entry to a business-budget ledger JSONL file, creating the
/// parent directory and file if needed. Mirrors
/// `human_verify::write_audit_entry`'s append pattern exactly.
pub fn append_ledger_entry(log_path: &Path, entry: &BudgetLedgerEntry) -> io::Result<()> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line =
        serde_json::to_string(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(f, "{line}")
}

/// Reads all entries from a business-budget ledger JSONL file, in append
/// order. Tolerant of a missing file (returns empty) and of individual
/// malformed lines (skipped), same as `verify_audit::read_audit_entries`.
pub fn read_ledger_entries(log_path: &Path) -> Vec<BudgetLedgerEntry> {
    let Ok(content) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The cumulative running total from a ledger file, or `0.0` if the ledger
/// is empty or missing (a session with no spend yet).
pub fn ledger_running_total(log_path: &Path) -> f64 {
    read_ledger_entries(log_path)
        .last()
        .map(|e| e.running_total)
        .unwrap_or(0.0)
}

/// Appends a new entry for `amount` spent on `action`, computing the new
/// running total from the ledger's current tail. Returns the appended entry.
pub fn record_ledger_spend(
    log_path: &Path,
    action: &str,
    amount: f64,
) -> io::Result<BudgetLedgerEntry> {
    let running_total = ledger_running_total(log_path) + amount;
    let entry = BudgetLedgerEntry {
        action: action.to_string(),
        amount,
        running_total,
        timestamp: Utc::now(),
    };
    append_ledger_entry(log_path, &entry)?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guardrails() -> BudgetGuardrails {
        BudgetGuardrails {
            metric: "usd".to_string(),
            total: 1000.0,
            per_action_max_pct: Some(10.0),
            soft_threshold_pct: Some(80.0),
            objective: Some("generate income > 2x within 6 months after fees".to_string()),
        }
    }

    #[test]
    fn action_within_all_limits_is_ok() {
        let result = check_budget(&guardrails(), 0.0, 50.0);
        assert_eq!(result, BudgetCheckResult::Ok);
    }

    #[test]
    fn action_exceeding_per_action_cap_is_hard_rejected() {
        // 10% of 1000 = 100 cap; 150 exceeds it.
        let result = check_budget(&guardrails(), 0.0, 150.0);
        assert!(result.is_hard_limit_exceeded());
    }

    #[test]
    fn action_exceeding_total_budget_is_hard_rejected_even_under_per_action_cap() {
        // 90 is under the 100 per-action cap, but 950 already spent + 90 > 1000 total.
        let result = check_budget(&guardrails(), 950.0, 90.0);
        assert!(result.is_hard_limit_exceeded());
    }

    #[test]
    fn crossing_soft_threshold_is_reported_and_not_a_hard_rejection() {
        // 80% of 1000 = 800 threshold; 750 spent + 60 = 810 crosses it, but is
        // still within the 100 per-action cap and the 1000 total.
        let result = check_budget(&guardrails(), 750.0, 60.0);
        assert!(result.is_soft_limit_crossed());
        assert!(!result.is_hard_limit_exceeded());
    }

    #[test]
    fn hard_limit_takes_priority_over_soft_limit_reporting() {
        // Exceeds the per-action cap AND would cross the soft threshold —
        // must report as a hard rejection, not a soft crossing.
        let result = check_budget(&guardrails(), 750.0, 150.0);
        assert!(result.is_hard_limit_exceeded());
    }

    #[test]
    fn budget_without_thresholds_only_checks_total() {
        let g = BudgetGuardrails {
            metric: "usd".to_string(),
            total: 1000.0,
            per_action_max_pct: None,
            soft_threshold_pct: None,
            objective: None,
        };
        assert_eq!(check_budget(&g, 0.0, 999.0), BudgetCheckResult::Ok);
        assert!(check_budget(&g, 0.0, 1001.0).is_hard_limit_exceeded());
    }

    #[test]
    fn ledger_accumulates_across_multiple_sequential_actions() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("budget-ledger.jsonl");

        assert_eq!(ledger_running_total(&log_path), 0.0);

        let e1 = record_ledger_spend(&log_path, "buy AAPL", 100.0).unwrap();
        assert_eq!(e1.running_total, 100.0);

        let e2 = record_ledger_spend(&log_path, "buy TSLA", 250.0).unwrap();
        assert_eq!(e2.running_total, 350.0);

        let e3 = record_ledger_spend(&log_path, "sell AAPL", 50.0).unwrap();
        assert_eq!(e3.running_total, 400.0);

        assert_eq!(ledger_running_total(&log_path), 400.0);

        let entries = read_ledger_entries(&log_path);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].action, "buy AAPL");
        assert_eq!(entries[1].action, "buy TSLA");
        assert_eq!(entries[2].action, "sell AAPL");
    }

    #[test]
    fn reading_a_missing_ledger_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("does-not-exist.jsonl");
        assert!(read_ledger_entries(&log_path).is_empty());
        assert_eq!(ledger_running_total(&log_path), 0.0);
    }
}
