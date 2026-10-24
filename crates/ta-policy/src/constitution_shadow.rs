// Constitution shadow mode: the audit store and the promotion report.
//
// A rule in `mode = "shadow"` never changes an outcome. At evaluation time
// the gate writes one *decision* record per item: what the rule would have
// done if it were enforced. Once a human has approved or rejected the draft,
// a *resolved* record is appended that carries the human's action and whether
// it agreed with the rule. `ta constitution shadow report` folds those
// records into per-rule agreement and a promotion recommendation.
//
// The report only ever recommends. Nothing here switches a rule's mode: a
// promotion is an amendment to the constitution, made by a person.
//
// Records are untrusted text where they carry model output (`reason`); they
// are sanitized and capped on write and on export.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::approval_rules::ApprovalAction;
use crate::constitution_v2::{Layer, Mode, RuleRef};

/// Project-relative path of the shadow audit log.
pub const SHADOW_LOG_PATH: &str = ".ta/audit/auto-approve-shadow.jsonl";

/// Schema version of a [`ShadowRecord`] line and of the Wayfinder snapshot.
pub const SCHEMA_VERSION: u32 = 1;

/// Longest free-text field kept in a record or exported.
pub const MAX_REASON_CHARS: usize = 200;

/// Disagreements kept per rule in the exported snapshot.
pub const SNAPSHOT_MAX_DISAGREEMENTS: usize = 20;

/// Rules in one exported snapshot.
pub const SNAPSHOT_MAX_RULES: usize = 20;

/// Largest snapshot body the receiver accepts.
pub const SNAPSHOT_MAX_BYTES: usize = 256 * 1024;

/// What the human did with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanAction {
    Approved,
    Rejected,
}

impl std::fmt::Display for HumanAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        })
    }
}

/// Which step of the item's life a line records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Written at evaluation; the human has not acted yet.
    Decision,
    /// Written once the human acted; supersedes the decision line.
    Resolved,
}

/// One line of `.ta/audit/auto-approve-shadow.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowRecord {
    pub schema_version: u32,
    pub phase: Phase,
    /// When this line was written.
    pub at: DateTime<Utc>,
    /// When the evaluation happened (constant across both phases).
    pub decided_at: DateTime<Utc>,
    pub draft_id: String,
    pub item_id: String,
    pub rule_id: String,
    pub layer: Layer,
    /// The rule's mode when it was evaluated.
    pub mode: Mode,
    /// What the rule would do if it were enforced.
    pub would_be: ApprovalAction,
    /// What actually acted (enforce-mode rules only); `None` = baseline.
    #[serde(default)]
    pub actual: Option<ApprovalAction>,
    /// The draft was approved by policy, so no human looked at it. Such
    /// records never get a human action and are not compared.
    #[serde(default)]
    pub machine_decided: bool,
    #[serde(default)]
    pub human_action: Option<HumanAction>,
    /// Did the human agree with the rule? `None` until resolved, and for a
    /// `review` stance (a rule that asks for a human cannot be contradicted).
    #[serde(default)]
    pub agreement: Option<bool>,
    #[serde(default)]
    pub supervisor_verdict: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Why the rule decided as it did. Derived from model output: untrusted.
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub org_sha256: Option<String>,
    #[serde(default)]
    pub project_sha256: Option<String>,
}

/// Did the human agree with a rule that would have done `would_be`?
///
/// `approve` agrees with an approval and disagrees with a rejection. `block`
/// agrees with a rejection and disagrees with an approval. `review` hands the
/// call to a human, so there is nothing to agree or disagree with.
pub fn agreement(would_be: ApprovalAction, human: HumanAction) -> Option<bool> {
    match (would_be, human) {
        (ApprovalAction::Approve, HumanAction::Approved) => Some(true),
        (ApprovalAction::Approve, HumanAction::Rejected) => Some(false),
        (ApprovalAction::Block, HumanAction::Rejected) => Some(true),
        (ApprovalAction::Block, HumanAction::Approved) => Some(false),
        (ApprovalAction::Review, _) => None,
    }
}

/// Make model- or user-derived text safe to store and show as plain text:
/// control characters become spaces, whitespace is collapsed, length is capped.
pub fn sanitize_text(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ── Storage ─────────────────────────────────────────────────────────────

/// Append records, one line each, in a single write.
pub fn append(project_root: &Path, records: &[ShadowRecord]) -> std::io::Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let path = project_root.join(SHADOW_LOG_PATH);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut buf = String::new();
    for r in records {
        buf.push_str(&serde_json::to_string(r).map_err(std::io::Error::other)?);
        buf.push('\n');
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(buf.as_bytes())
}

/// The log's records and how many lines could not be read.
#[derive(Debug, Default)]
pub struct Loaded {
    pub records: Vec<ShadowRecord>,
    /// Lines that are not valid records (a torn final write, hand edits).
    pub malformed: usize,
}

/// Read the log. A missing file is an empty log; an unreadable one is an error.
pub fn load(project_root: &Path) -> std::io::Result<Loaded> {
    let path = project_root.join(SHADOW_LOG_PATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::default()),
        Err(e) => {
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "cannot read {}: {e}. Check the file's permissions.",
                    path.display()
                ),
            ))
        }
    };
    let mut out = Loaded::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<ShadowRecord>(line) {
            Ok(r) => out.records.push(r),
            Err(_) => out.malformed += 1,
        }
    }
    Ok(out)
}

type Key<'a> = (&'a str, &'a str, &'a str);

fn key(r: &ShadowRecord) -> Key<'_> {
    (&r.draft_id, &r.item_id, &r.rule_id)
}

/// The latest line per (draft, item, rule): a resolved line replaces its
/// decision line.
pub fn latest(records: &[ShadowRecord]) -> Vec<&ShadowRecord> {
    let mut map: BTreeMap<Key<'_>, &ShadowRecord> = BTreeMap::new();
    for r in records {
        match map.get(&key(r)) {
            Some(prev) if prev.phase == Phase::Resolved && r.phase == Phase::Decision => {}
            _ => {
                map.insert(key(r), r);
            }
        }
    }
    map.into_values().collect()
}

/// Drafts that still have an item waiting for a human outcome.
pub fn drafts_awaiting_human(records: &[ShadowRecord]) -> BTreeSet<String> {
    latest(records)
        .into_iter()
        .filter(|r| {
            r.phase == Phase::Decision
                && !r.machine_decided
                && matches!(r.would_be, ApprovalAction::Approve | ApprovalAction::Block)
        })
        .map(|r| r.draft_id.clone())
        .collect()
}

/// Resolved lines for one draft: `human` maps an item id to what the human
/// did. Items the human has not decided get no line.
pub fn resolution_records(
    records: &[ShadowRecord],
    draft_id: &str,
    human: &HashMap<String, HumanAction>,
    now: DateTime<Utc>,
) -> Vec<ShadowRecord> {
    latest(records)
        .into_iter()
        .filter(|r| r.draft_id == draft_id && r.phase == Phase::Decision && !r.machine_decided)
        .filter_map(|r| {
            let action = *human.get(&r.item_id)?;
            let mut resolved = r.clone();
            resolved.phase = Phase::Resolved;
            resolved.at = now;
            resolved.human_action = Some(action);
            resolved.agreement = agreement(r.would_be, action);
            Some(resolved)
        })
        .collect()
}

// ── Promotion bar and report ────────────────────────────────────────────

/// The bar a shadow rule has to clear before promotion is recommended.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PromotionBar {
    /// Decisions with a human outcome needed before the rate means anything.
    pub min_decisions: usize,
    /// Largest fraction of those decisions the human disagreed with.
    pub max_disagreement: f64,
}

impl Default for PromotionBar {
    fn default() -> Self {
        Self {
            min_decisions: 50,
            max_disagreement: 0.02,
        }
    }
}

impl PromotionBar {
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.max_disagreement) || self.max_disagreement.is_nan() {
            return Err(format!(
                "max_disagreement = {} is not a fraction between 0 and 1 (2 percent is 0.02)",
                self.max_disagreement
            ));
        }
        if self.min_decisions == 0 {
            return Err(
                "min_decisions = 0 would recommend a rule that has never been compared with a human. Use at least 1."
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// A time window over the decisions (by `decided_at`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl Window {
    fn contains(&self, t: DateTime<Utc>) -> bool {
        self.from.is_none_or(|f| t >= f) && self.to.is_none_or(|e| t <= e)
    }
}

/// One item where the human and the rule differed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Disagreement {
    pub draft_id: String,
    pub item_id: String,
    pub at: DateTime<Utc>,
    pub would_be: ApprovalAction,
    pub human: HumanAction,
    pub supervisor_verdict: Option<String>,
    pub confidence: Option<f64>,
    pub reason: String,
}

/// The promotion verdict for one rule. A recommendation, never an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Promotion {
    pub eligible: bool,
    pub min_decisions: usize,
    pub max_disagreement: f64,
    pub reason: String,
}

/// Per-rule figures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleReport {
    pub rule_id: String,
    pub layer: Layer,
    pub mode: Mode,
    pub action: Option<ApprovalAction>,
    /// The rule is still in the current constitution.
    pub current: bool,
    /// Items with a stance (approve or block) and a human outcome.
    pub decisions: usize,
    /// Of `decisions`, how many the rule would have approved.
    pub would_approve: usize,
    pub agree: usize,
    pub disagree: usize,
    pub agreement_rate: Option<f64>,
    /// The rule would have approved an item a human rejected.
    pub machine_approved_human_rejected: usize,
    /// Stance recorded, human has not decided yet.
    pub pending: usize,
    /// Items the rule sent to a human (review stance): nothing to compare.
    pub not_compared: usize,
    /// Items on drafts approved by policy: no human outcome exists.
    pub machine_decided: usize,
    pub promotion: Promotion,
    /// Newest first.
    pub disagreements: Vec<Disagreement>,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowReport {
    pub window: Window,
    pub bar: PromotionBar,
    pub org_sha256: Option<String>,
    pub project_sha256: Option<String>,
    pub rules: Vec<RuleReport>,
    /// Unreadable lines in the log.
    pub malformed_lines: usize,
}

fn percent(fraction: f64) -> String {
    let s = format!("{:.1}", fraction * 100.0);
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

fn promotion(
    action: Option<ApprovalAction>,
    mode: Mode,
    r: &RuleReport,
    bar: &PromotionBar,
) -> Promotion {
    let mut p = Promotion {
        eligible: false,
        min_decisions: bar.min_decisions,
        max_disagreement: bar.max_disagreement,
        reason: String::new(),
    };
    if action.is_some_and(|a| a != ApprovalAction::Approve) {
        p.reason = format!(
            "this is a {} rule; only approve rules are promoted (review and block rules always enforce)",
            action.map(|a| a.to_string()).unwrap_or_default()
        );
        return p;
    }
    if mode == Mode::Enforce {
        p.reason = "already enforcing".to_string();
        return p;
    }
    let mut failures = Vec::new();
    if r.machine_approved_human_rejected > 0 {
        failures.push(format!(
            "{} item(s) the rule would have approved were rejected by a human (must be 0)",
            r.machine_approved_human_rejected
        ));
    }
    if r.decisions < bar.min_decisions {
        failures.push(format!(
            "{} of {} decisions with a human outcome so far",
            r.decisions, bar.min_decisions
        ));
    } else {
        let disagreement = r.disagree as f64 / r.decisions as f64;
        if disagreement > bar.max_disagreement {
            failures.push(format!(
                "disagreement {} percent is above {} percent",
                percent(disagreement),
                percent(bar.max_disagreement)
            ));
        }
    }
    if failures.is_empty() {
        p.eligible = true;
        p.reason = format!(
            "{} decisions, {} disagreement(s) ({} percent), none machine-approved and human-rejected",
            r.decisions,
            r.disagree,
            percent(r.disagree as f64 / r.decisions.max(1) as f64)
        );
    } else {
        p.reason = failures.join("; ");
    }
    p
}

/// Build the report from the log. `current` is the constitution's rules
/// (their modes are the ones that matter now); `org_sha256`/`project_sha256`
/// identify the files they came from.
pub fn build_report(
    records: &[ShadowRecord],
    malformed_lines: usize,
    current: &[RuleRef],
    shas: (Option<String>, Option<String>),
    window: Window,
    bar: PromotionBar,
) -> ShadowReport {
    let mut per_rule: BTreeMap<String, RuleReport> = BTreeMap::new();
    let blank = |id: &str, layer: Layer, mode: Mode, action: Option<ApprovalAction>, cur: bool| {
        RuleReport {
            rule_id: id.to_string(),
            layer,
            mode,
            action,
            current: cur,
            decisions: 0,
            would_approve: 0,
            agree: 0,
            disagree: 0,
            agreement_rate: None,
            machine_approved_human_rejected: 0,
            pending: 0,
            not_compared: 0,
            machine_decided: 0,
            promotion: Promotion {
                eligible: false,
                min_decisions: bar.min_decisions,
                max_disagreement: bar.max_disagreement,
                reason: String::new(),
            },
            disagreements: Vec::new(),
        }
    };
    for r in current {
        per_rule.insert(
            r.id.clone(),
            blank(&r.id, r.layer, r.mode, Some(r.action), true),
        );
    }
    let mut newest: Vec<&ShadowRecord> = latest(records)
        .into_iter()
        .filter(|r| window.contains(r.decided_at))
        .collect();
    newest.sort_by_key(|r| std::cmp::Reverse(r.at));
    for r in newest {
        let rep = per_rule
            .entry(r.rule_id.clone())
            .or_insert_with(|| blank(&r.rule_id, r.layer, r.mode, None, false));
        if r.machine_decided {
            rep.machine_decided += 1;
        } else if !matches!(r.would_be, ApprovalAction::Approve | ApprovalAction::Block) {
            rep.not_compared += 1;
        } else if let Some(human) = r.human_action {
            rep.decisions += 1;
            if r.would_be == ApprovalAction::Approve {
                rep.would_approve += 1;
            }
            if r.agreement == Some(true) {
                rep.agree += 1;
            } else {
                rep.disagree += 1;
                if r.would_be == ApprovalAction::Approve && human == HumanAction::Rejected {
                    rep.machine_approved_human_rejected += 1;
                }
                rep.disagreements.push(Disagreement {
                    draft_id: r.draft_id.clone(),
                    item_id: r.item_id.clone(),
                    at: r.at,
                    would_be: r.would_be,
                    human,
                    supervisor_verdict: r.supervisor_verdict.clone(),
                    confidence: r.confidence,
                    reason: sanitize_text(&r.reason, MAX_REASON_CHARS),
                });
            }
        } else {
            rep.pending += 1;
        }
    }
    let rules = per_rule
        .into_values()
        .map(|mut rep| {
            rep.agreement_rate =
                (rep.decisions > 0).then(|| rep.agree as f64 / rep.decisions as f64);
            rep.promotion = promotion(rep.action, rep.mode, &rep, &bar);
            rep
        })
        .collect();
    ShadowReport {
        window,
        bar,
        org_sha256: shas.0,
        project_sha256: shas.1,
        rules,
        malformed_lines,
    }
}

// ── Wayfinder snapshot ──────────────────────────────────────────────────
//
// Shape: alpha-channel/from-wayfinder/018-constitution-reply-and-project-map-ask.md,
// "Ask 3, shadow report". TA pushes; Wayfinder shows. [owner/Wayfinder to
// confirm the shape and the push route before anything sends it.]

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotWindow {
    pub from: Option<DateTime<Utc>>,
    pub to: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotConstitution {
    pub org_sha256: Option<String>,
    pub project_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRule {
    pub rule_id: String,
    pub layer: Layer,
    pub mode: Mode,
    pub decisions: usize,
    pub would_approve: usize,
    pub agree: usize,
    pub disagree: usize,
    pub agreement_rate: Option<f64>,
    pub machine_approved_human_rejected: usize,
    pub promotion: Promotion,
    pub disagreements: Vec<Disagreement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WayfinderSnapshot {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub window: SnapshotWindow,
    pub constitution: SnapshotConstitution,
    pub rules: Vec<SnapshotRule>,
}

/// Convert a report into the snapshot Wayfinder receives. Errors say what to
/// narrow when the body would exceed the receiver's limits.
pub fn wayfinder_snapshot(
    report: &ShadowReport,
    now: DateTime<Utc>,
) -> Result<WayfinderSnapshot, String> {
    if report.rules.len() > SNAPSHOT_MAX_RULES {
        return Err(format!(
            "the constitution has {} rules; the Wayfinder receiver accepts at most {SNAPSHOT_MAX_RULES} per snapshot",
            report.rules.len()
        ));
    }
    let rules = report
        .rules
        .iter()
        .map(|r| SnapshotRule {
            rule_id: r.rule_id.clone(),
            layer: r.layer,
            mode: r.mode,
            decisions: r.decisions,
            would_approve: r.would_approve,
            agree: r.agree,
            disagree: r.disagree,
            agreement_rate: r.agreement_rate,
            machine_approved_human_rejected: r.machine_approved_human_rejected,
            promotion: r.promotion.clone(),
            disagreements: r
                .disagreements
                .iter()
                .take(SNAPSHOT_MAX_DISAGREEMENTS)
                .cloned()
                .map(|mut d| {
                    d.reason = sanitize_text(&d.reason, MAX_REASON_CHARS);
                    d
                })
                .collect(),
        })
        .collect();
    let snapshot = WayfinderSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at: now,
        window: SnapshotWindow {
            from: report.window.from,
            to: report.window.to.unwrap_or(now),
        },
        constitution: SnapshotConstitution {
            org_sha256: report.org_sha256.clone(),
            project_sha256: report.project_sha256.clone(),
        },
        rules,
    };
    let size = serde_json::to_vec(&snapshot)
        .map_err(|e| e.to_string())?
        .len();
    if size > SNAPSHOT_MAX_BYTES {
        return Err(format!(
            "the snapshot is {size} bytes; the Wayfinder receiver accepts at most {SNAPSHOT_MAX_BYTES}. Narrow the window with --since."
        ));
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap()
    }

    fn rule(id: &str, action: ApprovalAction, mode: Mode) -> RuleRef {
        RuleRef {
            id: id.to_string(),
            layer: Layer::Org,
            action,
            mode,
            min_confidence: None,
            label: None,
        }
    }

    fn decision(
        draft: &str,
        item: &str,
        rule: &str,
        would_be: ApprovalAction,
        day: u32,
    ) -> ShadowRecord {
        ShadowRecord {
            schema_version: SCHEMA_VERSION,
            phase: Phase::Decision,
            at: t(day),
            decided_at: t(day),
            draft_id: draft.to_string(),
            item_id: item.to_string(),
            rule_id: rule.to_string(),
            layer: Layer::Org,
            mode: Mode::Shadow,
            would_be,
            actual: None,
            machine_decided: false,
            human_action: None,
            agreement: None,
            supervisor_verdict: Some("pass".to_string()),
            confidence: Some(0.9),
            reason: "ok".to_string(),
            org_sha256: None,
            project_sha256: None,
        }
    }

    fn resolve(mut recs: Vec<ShadowRecord>, human: HumanAction) -> Vec<ShadowRecord> {
        let mut resolved = Vec::new();
        for r in &recs {
            let map = HashMap::from([(r.item_id.clone(), human)]);
            resolved.extend(resolution_records(
                std::slice::from_ref(r),
                &r.draft_id,
                &map,
                t(20),
            ));
        }
        recs.extend(resolved);
        recs
    }

    fn report(recs: &[ShadowRecord], bar: PromotionBar) -> ShadowReport {
        build_report(
            recs,
            0,
            &[rule("r", ApprovalAction::Approve, Mode::Shadow)],
            (None, None),
            Window::default(),
            bar,
        )
    }

    #[test]
    fn agreement_table() {
        use ApprovalAction::*;
        use HumanAction::*;
        assert_eq!(agreement(Approve, Approved), Some(true));
        assert_eq!(agreement(Approve, Rejected), Some(false));
        assert_eq!(agreement(Block, Rejected), Some(true));
        assert_eq!(agreement(Block, Approved), Some(false));
        assert_eq!(agreement(Review, Approved), None);
        assert_eq!(agreement(Review, Rejected), None);
    }

    #[test]
    fn append_then_load_round_trips_and_counts_torn_lines() {
        let dir = tempfile::tempdir().unwrap();
        let recs = vec![decision("d1", "i1", "r", ApprovalAction::Approve, 1)];
        append(dir.path(), &recs).unwrap();
        let log = dir.path().join(SHADOW_LOG_PATH);
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str("{\"schema_version\":1,\"phase\":\"deci");
        std::fs::write(&log, text).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.records, recs);
        assert_eq!(loaded.malformed, 1, "a torn line is counted, not fatal");
        assert!(load(tempfile::tempdir().unwrap().path())
            .unwrap()
            .records
            .is_empty());
    }

    #[test]
    fn a_resolved_line_replaces_its_decision_and_carries_agreement() {
        let recs = resolve(
            vec![decision("d1", "i1", "r", ApprovalAction::Approve, 1)],
            HumanAction::Rejected,
        );
        assert_eq!(recs.len(), 2);
        let l = latest(&recs);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].phase, Phase::Resolved);
        assert_eq!(l[0].human_action, Some(HumanAction::Rejected));
        assert_eq!(l[0].agreement, Some(false));
        assert!(drafts_awaiting_human(&recs).is_empty());
        assert_eq!(drafts_awaiting_human(&recs[..1]).len(), 1);
    }

    #[test]
    fn machine_decided_and_review_items_are_never_resolved_or_awaited() {
        let mut machine = decision("d1", "i1", "r", ApprovalAction::Approve, 1);
        machine.machine_decided = true;
        let review = decision("d2", "i2", "r", ApprovalAction::Review, 1);
        let recs = vec![machine, review];
        assert!(drafts_awaiting_human(&recs).is_empty());
        let map = HashMap::from([("i1".to_string(), HumanAction::Approved)]);
        assert!(resolution_records(&recs, "d1", &map, t(2)).is_empty());
    }

    fn many(n_agree: usize, n_reject: usize) -> Vec<ShadowRecord> {
        let mut out = Vec::new();
        for i in 0..n_agree + n_reject {
            let d = format!("d{i}");
            let rec = decision(&d, "i", "r", ApprovalAction::Approve, 1);
            let human = if i < n_agree {
                HumanAction::Approved
            } else {
                HumanAction::Rejected
            };
            out.extend(resolve(vec![rec], human));
        }
        out
    }

    #[test]
    fn promotion_is_recommended_only_when_the_whole_bar_is_met() {
        let bar = PromotionBar::default();
        let r = report(&many(50, 0), bar);
        let rep = &r.rules[0];
        assert_eq!(rep.decisions, 50);
        assert_eq!(rep.agreement_rate, Some(1.0));
        assert!(rep.promotion.eligible, "{}", rep.promotion.reason);

        let r = report(&many(49, 0), bar);
        assert!(!r.rules[0].promotion.eligible);
        assert!(
            r.rules[0].promotion.reason.contains("49 of 50"),
            "{}",
            r.rules[0].promotion.reason
        );
    }

    #[test]
    fn one_machine_approved_human_rejected_item_blocks_promotion_even_within_two_percent() {
        // 1 rejection in 100 is 1 percent: inside the rate, outside the bar.
        let r = report(&many(99, 1), PromotionBar::default());
        let rep = &r.rules[0];
        assert_eq!(rep.disagree, 1);
        assert_eq!(rep.machine_approved_human_rejected, 1);
        assert!(!rep.promotion.eligible);
        assert!(rep.promotion.reason.contains("must be 0"));
    }

    #[test]
    fn disagreement_above_the_threshold_says_so_with_both_numbers() {
        let bar = PromotionBar {
            min_decisions: 10,
            max_disagreement: 0.02,
        };
        let r = report(&many(26, 1), bar);
        let reason = &r.rules[0].promotion.reason;
        assert!(
            reason.contains("disagreement 3.7 percent is above 2 percent"),
            "{reason}"
        );
    }

    #[test]
    fn the_threshold_is_configurable() {
        let bar = PromotionBar {
            min_decisions: 3,
            max_disagreement: 0.0,
        };
        let r = report(&many(3, 0), bar);
        assert!(r.rules[0].promotion.eligible);
        assert_eq!(r.rules[0].promotion.min_decisions, 3);
        assert!(PromotionBar {
            min_decisions: 0,
            max_disagreement: 0.02
        }
        .validate()
        .is_err());
        assert!(PromotionBar {
            min_decisions: 5,
            max_disagreement: 2.0
        }
        .validate()
        .unwrap_err()
        .contains("0.02"));
    }

    #[test]
    fn enforcing_review_and_block_rules_are_never_recommended() {
        let recs = many(60, 0);
        let enforcing = build_report(
            &recs,
            0,
            &[rule("r", ApprovalAction::Approve, Mode::Enforce)],
            (None, None),
            Window::default(),
            PromotionBar::default(),
        );
        assert!(!enforcing.rules[0].promotion.eligible);
        assert_eq!(enforcing.rules[0].promotion.reason, "already enforcing");
        let blocking = build_report(
            &recs,
            0,
            &[rule("r", ApprovalAction::Block, Mode::Shadow)],
            (None, None),
            Window::default(),
            PromotionBar::default(),
        );
        assert!(!blocking.rules[0].promotion.eligible);
        assert!(blocking.rules[0].promotion.reason.contains("block rule"));
    }

    #[test]
    fn pending_review_and_machine_items_are_counted_apart_from_decisions() {
        let mut recs = vec![
            decision("d1", "i", "r", ApprovalAction::Approve, 1),
            decision("d2", "i", "r", ApprovalAction::Review, 1),
        ];
        let mut m = decision("d3", "i", "r", ApprovalAction::Approve, 1);
        m.machine_decided = true;
        recs.push(m);
        let r = report(&recs, PromotionBar::default());
        let rep = &r.rules[0];
        assert_eq!(
            (
                rep.decisions,
                rep.pending,
                rep.not_compared,
                rep.machine_decided
            ),
            (0, 1, 1, 1)
        );
        assert_eq!(rep.agreement_rate, None);
    }

    #[test]
    fn the_window_filters_by_decision_time() {
        let recs = many(5, 0);
        let w = Window {
            from: Some(t(2)),
            to: None,
        };
        let r = build_report(
            &recs,
            0,
            &[rule("r", ApprovalAction::Approve, Mode::Shadow)],
            (None, None),
            w,
            PromotionBar::default(),
        );
        assert_eq!(
            r.rules[0].decisions, 0,
            "decisions on day 1 are before the window"
        );
    }

    #[test]
    fn records_of_a_removed_rule_are_still_reported() {
        let recs = many(2, 0);
        let r = build_report(
            &recs,
            0,
            &[],
            (None, None),
            Window::default(),
            PromotionBar::default(),
        );
        assert_eq!(r.rules.len(), 1);
        assert!(!r.rules[0].current);
    }

    #[test]
    fn disagreements_are_newest_first_and_the_snapshot_caps_them_and_cleans_reason_text() {
        let mut recs = Vec::new();
        for i in 0..25u32 {
            let mut rec = decision(
                &format!("d{i}"),
                "i",
                "r",
                ApprovalAction::Approve,
                1 + i % 20,
            );
            rec.reason = format!(
                "line one\n<script>alert(1)</script>\x07 {}",
                "x".repeat(400)
            );
            recs.extend(resolve(vec![rec], HumanAction::Rejected));
        }
        let rep = report(&recs, PromotionBar::default());
        let d = &rep.rules[0].disagreements;
        assert_eq!(d.len(), 25);
        assert!(d.windows(2).all(|w| w[0].at >= w[1].at), "newest first");
        assert!(d[0].reason.chars().count() <= MAX_REASON_CHARS);
        assert!(!d[0].reason.contains('\n') && !d[0].reason.contains('\x07'));
        let snap = wayfinder_snapshot(&rep, t(25)).unwrap();
        assert_eq!(
            snap.rules[0].disagreements.len(),
            SNAPSHOT_MAX_DISAGREEMENTS
        );
        assert_eq!(snap.rules[0].machine_approved_human_rejected, 25);
        assert_eq!(snap.schema_version, 1);
    }

    #[test]
    fn snapshot_json_has_the_documented_keys() {
        let rep = report(&many(2, 0), PromotionBar::default());
        let v = serde_json::to_value(wayfinder_snapshot(&rep, t(25)).unwrap()).unwrap();
        for k in [
            "schema_version",
            "generated_at",
            "window",
            "constitution",
            "rules",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        let r = &v["rules"][0];
        for k in [
            "rule_id",
            "layer",
            "mode",
            "decisions",
            "would_approve",
            "agree",
            "disagree",
            "agreement_rate",
            "machine_approved_human_rejected",
            "promotion",
            "disagreements",
        ] {
            assert!(r.get(k).is_some(), "missing rules[].{k}");
        }
        for k in ["eligible", "min_decisions", "max_disagreement", "reason"] {
            assert!(r["promotion"].get(k).is_some(), "missing promotion.{k}");
        }
        assert_eq!(r["layer"], "org");
        assert_eq!(r["mode"], "shadow");
    }

    #[test]
    fn a_snapshot_with_too_many_rules_is_refused_with_the_limit() {
        let rules: Vec<RuleRef> = (0..21)
            .map(|i| rule(&format!("r{i}"), ApprovalAction::Review, Mode::Enforce))
            .collect();
        let rep = build_report(
            &[],
            0,
            &rules,
            (None, None),
            Window::default(),
            PromotionBar::default(),
        );
        let err = wayfinder_snapshot(&rep, t(1)).unwrap_err();
        assert!(err.contains("at most 20"), "{err}");
    }

    #[test]
    fn sanitize_collapses_controls_and_caps_with_an_ellipsis() {
        assert_eq!(sanitize_text("a\n\tb\x1b[31m c", 50), "a b [31m c");
        let long = "y".repeat(300);
        let s = sanitize_text(&long, 200);
        assert_eq!(s.chars().count(), 200);
        assert!(s.ends_with('…'));
    }
}
