// spend.rs: what a goal cost and why it started (v0.17.12.15).
//
// Four pieces, all pure so they can be tested without an agent:
//   * `UsageTracker`    reads the agent runtime's stream-json lines and keeps
//                       the running token counts, the model and any reported cost;
//   * `BudgetLimits`    the per-goal ceilings (`--max-tokens`, `--max-usd`);
//   * `CompletionRecord` the machine-readable result of a finished goal, shown by
//                       `ta goal status --json`, the `goal.completed` event and
//                       the headless result line;
//   * `aggregate`       the grouped totals behind `GET /api/spend`.
//
// `origin_class` appears here only as a label to group and report by. Nothing in
// this file (or anywhere in an authorization path) decides anything with it.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::goal_run::{BudgetAbort, GoalRun};
use crate::origin::OriginClass;
use crate::token_cost::{resolve_cost, CostSource, PriceTable, TokenUsage};

// ── Reading the agent runtime's usage report ────────────────────────────────

/// Running usage for one agent session, built from stream-json lines.
///
/// The runtime reports usage two ways: each `assistant` message carries the
/// usage of its turn (repeated for every content block of the same message, so
/// the latest value per message id is kept), and the closing `result` line
/// carries the session total plus `total_cost_usd`. While the agent runs the
/// per-message figures give a live count (what `--max-tokens` is enforced
/// against); once `result` arrives it is authoritative.
#[derive(Debug, Default, Clone)]
pub struct UsageTracker {
    model: String,
    per_message: HashMap<String, TokenUsage>,
    unkeyed: TokenUsage,
    result: Option<TokenUsage>,
    reported_cost_usd: Option<f64>,
}

fn u64_of(v: &serde_json::Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn usage_of(v: &serde_json::Value) -> TokenUsage {
    TokenUsage {
        input: u64_of(v, "input_tokens"),
        output: u64_of(v, "output_tokens"),
        cache_read: u64_of(v, "cache_read_input_tokens"),
        cache_creation: u64_of(v, "cache_creation_input_tokens"),
    }
}

fn max_usage(a: TokenUsage, b: TokenUsage) -> TokenUsage {
    TokenUsage {
        input: a.input.max(b.input),
        output: a.output.max(b.output),
        cache_read: a.cache_read.max(b.cache_read),
        cache_creation: a.cache_creation.max(b.cache_creation),
    }
}

fn add_usage(a: TokenUsage, b: TokenUsage) -> TokenUsage {
    TokenUsage {
        input: a.input.saturating_add(b.input),
        output: a.output.saturating_add(b.output),
        cache_read: a.cache_read.saturating_add(b.cache_read),
        cache_creation: a.cache_creation.saturating_add(b.cache_creation),
    }
}

impl UsageTracker {
    /// Feed one stdout line. Lines that are not JSON, or not usage-bearing
    /// events, are ignored.
    pub fn observe_line(&mut self, line: &str) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        match val.get("type").and_then(|t| t.as_str()) {
            Some("result") => {
                if let Some(u) = val.get("usage") {
                    // A session can end with more than one result line (a
                    // resumed session): their totals add.
                    self.result = Some(add_usage(self.result.unwrap_or_default(), usage_of(u)));
                }
                if let Some(c) = val.get("total_cost_usd").and_then(|c| c.as_f64()) {
                    if c.is_finite() && c >= 0.0 {
                        self.reported_cost_usd = Some(self.reported_cost_usd.unwrap_or(0.0) + c);
                    }
                }
                if self.model.is_empty() {
                    if let Some(m) = val
                        .get("modelUsage")
                        .and_then(|m| m.as_object())
                        .and_then(|m| m.keys().next())
                    {
                        self.model = m.clone();
                    }
                }
            }
            Some("system") if self.model.is_empty() => {
                if let Some(m) = val.get("model").and_then(|v| v.as_str()) {
                    self.model = m.to_string();
                }
            }
            Some("assistant") => {
                let Some(msg) = val.get("message") else {
                    return;
                };
                if self.model.is_empty() {
                    if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                        self.model = m.to_string();
                    }
                }
                if let Some(u) = msg.get("usage") {
                    let u = usage_of(u);
                    match msg.get("id").and_then(|i| i.as_str()) {
                        Some(id) => {
                            let e = self.per_message.entry(id.to_string()).or_default();
                            *e = max_usage(*e, u);
                        }
                        // No id to de-duplicate on: count it once, as reported.
                        None => self.unkeyed = add_usage(self.unkeyed, u),
                    }
                }
            }
            _ => {}
        }
    }

    /// The model the agent reported, or empty.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Tokens so far: the session total once `result` has arrived, else the
    /// sum of the per-message figures seen.
    pub fn usage(&self) -> TokenUsage {
        if let Some(r) = self.result {
            return r;
        }
        self.per_message
            .values()
            .fold(self.unkeyed, |acc, u| add_usage(acc, *u))
    }

    /// The cost the runtime itself reported (`total_cost_usd`), if any.
    pub fn reported_cost_usd(&self) -> Option<f64> {
        self.reported_cost_usd
    }

    /// `true` once the closing `result` line has been seen.
    pub fn is_final(&self) -> bool {
        self.result.is_some()
    }

    /// Cost so far for budget checks: the reported cost when final, else the
    /// table price of the live count, else `None` (unpriced model).
    pub fn live_cost_usd(&self, table: &PriceTable) -> Option<f64> {
        if let Some(c) = self.reported_cost_usd {
            return Some(c);
        }
        if self.model.is_empty() {
            return None;
        }
        table.price(&self.model, &self.usage())
    }
}

// ── Per-goal ceilings ───────────────────────────────────────────────────────

/// Per-goal spend ceilings. `None` means no ceiling of that kind.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BudgetLimits {
    pub max_tokens: Option<u64>,
    pub max_usd: Option<f64>,
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

impl BudgetLimits {
    /// A ceiling given on the command line wins over the `[budget]` default.
    pub fn resolve(
        cli_tokens: Option<u64>,
        cli_usd: Option<f64>,
        default_tokens: Option<u64>,
        default_usd: Option<f64>,
    ) -> Result<Self, String> {
        if let Some(u) = cli_usd.or(default_usd) {
            if !u.is_finite() || u <= 0.0 {
                return Err(format!(
                    "invalid --max-usd / [budget] max_usd {u}: expected a positive number of US \
                     dollars, for example --max-usd 5.0"
                ));
            }
        }
        if cli_tokens.or(default_tokens) == Some(0) {
            return Err(
                "invalid --max-tokens / [budget] max_tokens 0: expected a positive number of \
                 tokens, for example --max-tokens 2000000"
                    .to_string(),
            );
        }
        Ok(Self {
            max_tokens: cli_tokens.or(default_tokens),
            max_usd: cli_usd.or(default_usd),
        })
    }

    /// No ceiling set.
    pub fn is_unlimited(&self) -> bool {
        self.max_tokens.is_none() && self.max_usd.is_none()
    }

    /// Whether `usage` (and `usd`, when the model is priced) has reached a
    /// ceiling. The token ceiling is checked first.
    pub fn check(&self, usage: &TokenUsage, usd: Option<f64>) -> Option<BudgetAbort> {
        if let Some(max) = self.max_tokens {
            let total = usage.total();
            if total >= max {
                return Some(BudgetAbort {
                    limit: "max_tokens".to_string(),
                    limit_value: max as f64,
                    observed: total as f64,
                    message: format!(
                        "Goal stopped: it used {} tokens (input {}, output {}, cache read {}, \
                         cache write {}), reaching its ceiling of {} (--max-tokens / [budget] \
                         max_tokens). The agent was stopped between turns, so it may have \
                         overshot by up to one turn. The partial work is in the draft, marked \
                         incomplete. To continue, raise the ceiling and run `ta run --goal-id \
                         <id>` or start a new goal.",
                        thousands(total),
                        thousands(usage.input),
                        thousands(usage.output),
                        thousands(usage.cache_read),
                        thousands(usage.cache_creation),
                        thousands(max)
                    ),
                });
            }
        }
        if let (Some(max), Some(cost)) = (self.max_usd, usd) {
            if cost >= max {
                return Some(BudgetAbort {
                    limit: "max_usd".to_string(),
                    limit_value: max,
                    observed: cost,
                    message: format!(
                        "Goal stopped: it cost ${cost:.2}, reaching its ceiling of ${max:.2} \
                         (--max-usd / [budget] max_usd). The agent was stopped between turns, so \
                         it may have overshot by up to one turn. The partial work is in the \
                         draft, marked incomplete. To continue, raise the ceiling and run \
                         `ta run --goal-id <id>` or start a new goal."
                    ),
                });
            }
        }
        None
    }
}

// ── The completion record ───────────────────────────────────────────────────

/// The machine-readable result of a goal's agent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompletionRecord {
    pub goal_id: Uuid,
    pub title: String,
    /// The goal's lifecycle state, for example `pr_ready`.
    pub state: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    /// USD cost. `0` with `cost_source` `none` when the model has no price.
    pub usd: f64,
    /// `true` only when `usd` came from a price-table row; `false` for a cost
    /// the runtime reported (exact) and for an unpriced model.
    pub cost_estimated: bool,
    pub cost_source: CostSource,
    /// The component that originated the goal (`cos`, `chat`, ...), if any.
    pub origin: Option<String>,
    /// Why the goal started. Accounting data only.
    pub origin_class: OriginClass,
    /// `true` when the agent was stopped by a spend ceiling.
    pub aborted: bool,
    /// `aborted_budget` when `aborted`, else absent.
    pub outcome: Option<String>,
    pub budget_abort: Option<BudgetAbort>,
}

impl CompletionRecord {
    /// Build the record from the goal's stored fields.
    pub fn from_goal(goal: &GoalRun) -> Self {
        let aborted = goal.budget_abort.is_some();
        Self {
            goal_id: goal.goal_run_id,
            title: goal.title.clone(),
            state: goal.state.to_string(),
            model: goal.agent_model.clone(),
            input_tokens: goal.input_tokens,
            output_tokens: goal.output_tokens,
            cache_read_tokens: goal.cache_read_input_tokens,
            cache_creation_tokens: goal.cache_creation_input_tokens,
            usd: goal.cost_usd,
            cost_estimated: goal.cost_estimated,
            cost_source: goal.cost_source,
            origin: goal.origin.clone(),
            origin_class: goal.effective_origin_class(),
            aborted,
            outcome: aborted.then(|| "aborted_budget".to_string()),
            budget_abort: goal.budget_abort.clone(),
        }
    }
}

/// Record a finished agent session's usage on the goal: tokens, model, and the
/// cost (runtime-reported when available, else priced from `table`).
pub fn record_usage_on_goal(goal: &mut GoalRun, tracker: &UsageTracker, table: &PriceTable) {
    let usage = tracker.usage();
    if usage.total() > 0 {
        goal.input_tokens = usage.input;
        goal.output_tokens = usage.output;
        goal.cache_read_input_tokens = usage.cache_read;
        goal.cache_creation_input_tokens = usage.cache_creation;
    }
    if !tracker.model().is_empty() {
        goal.agent_model = tracker.model().to_string();
    }
    let tokens = goal.token_usage();
    if tokens.total() > 0 || tracker.reported_cost_usd().is_some() {
        let r = resolve_cost(
            table,
            &goal.agent_model,
            &tokens,
            tracker.reported_cost_usd(),
        );
        goal.cost_usd = r.usd;
        goal.cost_estimated = r.estimated;
        goal.cost_source = r.source;
    }
}

/// Price a goal recorded before cost tracking existed, for a report.
///
/// Such a goal has real tokens and a model name but `cost_source` `none`, so a
/// report would count it unpriced and leave its spend out of the total. When it
/// has tokens, a model the `table` prices and no `cost_source`, set its cost
/// from the table and label it `estimated` (`cost_source` `table`). Goals with
/// no tokens, no model or an unpriced model stay unpriced. Returns whether the
/// goal was priced. Only the in-memory copy changes: the goal record on disk is
/// never rewritten by a report.
pub fn estimate_legacy_goal_cost(goal: &mut GoalRun, table: &PriceTable) -> bool {
    if goal.cost_source != CostSource::None || goal.agent_model.is_empty() {
        return false;
    }
    let tokens = goal.token_usage();
    if tokens.total() == 0 {
        return false;
    }
    let r = resolve_cost(table, &goal.agent_model, &tokens, None);
    if r.source != CostSource::Table {
        return false;
    }
    goal.cost_usd = r.usd;
    goal.cost_estimated = true;
    goal.cost_source = CostSource::Table;
    true
}

// ── Aggregation behind GET /api/spend ───────────────────────────────────────

/// A dimension to group spend by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpendGroup {
    OriginClass,
    Project,
    Origin,
    Model,
}

impl SpendGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            SpendGroup::OriginClass => "origin_class",
            SpendGroup::Project => "project",
            SpendGroup::Origin => "origin",
            SpendGroup::Model => "model",
        }
    }
}

/// Parse a `group_by` value such as `origin_class,project`. Empty means one
/// group holding everything. Unknown or repeated names are errors.
pub fn parse_group_by(s: &str) -> Result<Vec<SpendGroup>, String> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let g = match part {
            "origin_class" => SpendGroup::OriginClass,
            "project" => SpendGroup::Project,
            "origin" => SpendGroup::Origin,
            "model" => SpendGroup::Model,
            other => {
                return Err(format!(
                    "unknown group_by {other:?}: expected a comma-separated list of origin_class, \
                     project, origin, model (for example group_by=origin_class,project)"
                ))
            }
        };
        if out.contains(&g) {
            return Err(format!(
                "group_by lists {part:?} twice; list each name once"
            ));
        }
        out.push(g);
    }
    Ok(out)
}

/// Parse a `since` / `until` bound: an RFC 3339 timestamp or a `YYYY-MM-DD`
/// date (midnight UTC).
pub fn parse_time_bound(s: &str) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0) {
            return Ok(dt.and_utc());
        }
    }
    Err(format!(
        "invalid time {s:?}: expected an RFC 3339 timestamp (2026-10-12T09:30:00Z) or a date \
         (2026-10-12)"
    ))
}

/// Totals for one group.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpendTotals {
    pub goals: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_tokens: u64,
    pub usd: f64,
    /// Goals whose cost the runtime reported (exact).
    pub exact_cost_goals: u64,
    /// Goals whose cost came from a price table (estimates).
    pub estimated_cost_goals: u64,
    /// Goals with no price at all (unknown model, or no usage recorded).
    pub unpriced_goals: u64,
    /// Goals stopped by a spend ceiling.
    pub aborted_budget_goals: u64,
}

impl SpendTotals {
    fn add_goal(&mut self, g: &GoalRun) {
        let u = g.token_usage();
        self.goals += 1;
        self.input_tokens = self.input_tokens.saturating_add(u.input);
        self.output_tokens = self.output_tokens.saturating_add(u.output);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(u.cache_read);
        self.cache_creation_tokens = self.cache_creation_tokens.saturating_add(u.cache_creation);
        self.total_tokens = self.total_tokens.saturating_add(u.total());
        self.usd += g.cost_usd;
        match g.cost_source {
            CostSource::Runtime => self.exact_cost_goals += 1,
            CostSource::Table => self.estimated_cost_goals += 1,
            CostSource::None => self.unpriced_goals += 1,
        }
        if g.budget_abort.is_some() {
            self.aborted_budget_goals += 1;
        }
    }
}

/// One row of a spend report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpendGroupRow {
    /// The group's key, one entry per `group_by` name.
    pub group: BTreeMap<String, String>,
    #[serde(flatten)]
    pub totals: SpendTotals,
}

/// The whole report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpendReport {
    pub group_by: Vec<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub groups: Vec<SpendGroupRow>,
    pub totals: SpendTotals,
    /// How the numbers are made, so a reader does not have to guess.
    pub notes: Vec<String>,
}

/// Group goals and total their spend. `goals` pairs each goal with the name of
/// its project. A goal is included when its `created_at` is at or after
/// `since` and before `until`. Rows are ordered by group key.
pub fn aggregate(
    goals: &[(String, GoalRun)],
    group_by: &[SpendGroup],
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> SpendReport {
    let mut rows: BTreeMap<Vec<String>, SpendTotals> = BTreeMap::new();
    let mut totals = SpendTotals::default();
    for (project, g) in goals {
        if since.is_some_and(|s| g.created_at < s) || until.is_some_and(|u| g.created_at >= u) {
            continue;
        }
        let key: Vec<String> = group_by
            .iter()
            .map(|d| match d {
                SpendGroup::OriginClass => g.effective_origin_class().to_string(),
                SpendGroup::Project => project.clone(),
                SpendGroup::Origin => g.origin.clone().unwrap_or_else(|| "none".to_string()),
                SpendGroup::Model => {
                    if g.agent_model.is_empty() {
                        "unknown".to_string()
                    } else {
                        g.agent_model.clone()
                    }
                }
            })
            .collect();
        rows.entry(key).or_default().add_goal(g);
        totals.add_goal(g);
    }
    let names: Vec<String> = group_by.iter().map(|d| d.as_str().to_string()).collect();
    let groups = rows
        .into_iter()
        .map(|(key, totals)| SpendGroupRow {
            group: names.iter().cloned().zip(key).collect(),
            totals,
        })
        .collect();
    SpendReport {
        group_by: names,
        since,
        until,
        groups,
        totals,
        notes: vec![
            "Read from the goal records, not from the velocity file an agent can write."
                .to_string(),
            "exact_cost_goals: the agent runtime reported the cost. estimated_cost_goals: priced \
             from a price table (.ta/pricing.toml or built in). unpriced_goals: no price; their \
             tokens are counted and their usd is 0."
                .to_string(),
            "Goals recorded before cost tracking existed, with tokens and a priced model, are \
             priced from the table at report time and counted as estimated; goals with no \
             tokens or no priced model stay unpriced."
                .to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn goal(model: &str, class: Option<OriginClass>, usd: f64, src: CostSource) -> GoalRun {
        let mut g = GoalRun::new(
            "t",
            "o",
            "claude-code",
            PathBuf::from("/tmp/w"),
            PathBuf::from("/tmp/s"),
        );
        g.agent_model = model.to_string();
        g.origin_class = class;
        g.input_tokens = 10;
        g.output_tokens = 20;
        g.cache_read_input_tokens = 300;
        g.cache_creation_input_tokens = 40;
        g.cost_usd = usd;
        g.cost_source = src;
        g.cost_estimated = src == CostSource::Table;
        g
    }

    // The real shape of a Claude Code result line (trimmed from a saved log).
    const RESULT_LINE: &str = r#"{"type":"result","subtype":"success","total_cost_usd":1.4391539,"usage":{"input_tokens":86,"cache_creation_input_tokens":138618,"cache_read_input_tokens":4423499,"output_tokens":44216,"cache_creation":{"ephemeral_1h_input_tokens":138618,"ephemeral_5m_input_tokens":0}},"modelUsage":{"claude-sonnet-5-5":{"inputTokens":86,"outputTokens":44216}},"is_error":false}"#;

    #[test]
    fn result_line_gives_all_four_token_kinds_model_and_exact_cost() {
        let mut t = UsageTracker::default();
        t.observe_line(RESULT_LINE);
        assert!(t.is_final());
        assert_eq!(
            t.usage(),
            TokenUsage {
                input: 86,
                output: 44_216,
                cache_read: 4_423_499,
                cache_creation: 138_618
            }
        );
        assert_eq!(t.model(), "claude-sonnet-5-5");
        assert_eq!(t.reported_cost_usd(), Some(1.4391539));
    }

    #[test]
    fn streamed_assistant_usage_is_deduplicated_per_message_id() {
        let mut t = UsageTracker::default();
        t.observe_line(r#"{"type":"system","subtype":"init","model":"claude-sonnet-5-5"}"#);
        // Two content blocks of one message repeat its usage.
        let m1a = r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":5,"output_tokens":2,"cache_read_input_tokens":100,"cache_creation_input_tokens":10}}}"#;
        let m1b = r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":5,"output_tokens":9,"cache_read_input_tokens":100,"cache_creation_input_tokens":10}}}"#;
        let m2 = r#"{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":1,"output_tokens":4,"cache_read_input_tokens":200,"cache_creation_input_tokens":0}}}"#;
        for l in [m1a, m1b, m2] {
            t.observe_line(l);
        }
        assert!(!t.is_final());
        assert_eq!(
            t.usage(),
            TokenUsage {
                input: 6,
                output: 13,
                cache_read: 300,
                cache_creation: 10
            }
        );
        assert_eq!(t.model(), "claude-sonnet-5-5");
        // A later result line replaces the live count.
        t.observe_line(RESULT_LINE);
        assert_eq!(t.usage().cache_read, 4_423_499);
    }

    #[test]
    fn garbage_and_unrelated_lines_are_ignored() {
        let mut t = UsageTracker::default();
        for l in [
            "",
            "not json",
            "{}",
            r#"{"type":"user"}"#,
            r#"{"type":"result"}"#,
        ] {
            t.observe_line(l);
        }
        assert_eq!(t.usage().total(), 0);
        assert_eq!(t.reported_cost_usd(), None);
    }

    #[test]
    fn live_cost_prefers_reported_then_table_then_none() {
        let table = PriceTable::builtin();
        let mut t = UsageTracker::default();
        assert_eq!(t.live_cost_usd(&table), None, "no model yet");
        t.observe_line(r#"{"type":"system","model":"claude-sonnet-4-6"}"#);
        t.observe_line(r#"{"type":"assistant","message":{"id":"a","usage":{"input_tokens":1000000,"output_tokens":0}}}"#);
        assert!((t.live_cost_usd(&table).unwrap() - 3.0).abs() < 1e-9);
        t.observe_line(
            r#"{"type":"result","total_cost_usd":9.5,"usage":{"input_tokens":1000000}}"#,
        );
        assert_eq!(t.live_cost_usd(&table), Some(9.5));
        let mut u = UsageTracker::default();
        u.observe_line(r#"{"type":"system","model":"qwen3.5:9b"}"#);
        u.observe_line(r#"{"type":"assistant","message":{"id":"a","usage":{"input_tokens":5}}}"#);
        assert_eq!(u.live_cost_usd(&table), None);
    }

    #[test]
    fn record_usage_stores_exact_cost_and_cache_tokens() {
        let mut g = goal("", None, 0.0, CostSource::None);
        g.input_tokens = 0;
        g.output_tokens = 0;
        g.cache_read_input_tokens = 0;
        g.cache_creation_input_tokens = 0;
        let mut t = UsageTracker::default();
        t.observe_line(RESULT_LINE);
        record_usage_on_goal(&mut g, &t, &PriceTable::builtin());
        assert_eq!(g.cache_read_input_tokens, 4_423_499);
        assert_eq!(g.cache_creation_input_tokens, 138_618);
        assert_eq!(g.cost_source, CostSource::Runtime);
        assert!(!g.cost_estimated);
        assert_eq!(g.cost_usd, 1.4391539);
        assert_eq!(g.agent_model, "claude-sonnet-5-5");
    }

    #[test]
    fn record_usage_without_reported_cost_prices_from_the_table() {
        let mut g = goal("", None, 0.0, CostSource::None);
        let mut t = UsageTracker::default();
        t.observe_line(r#"{"type":"system","model":"claude-sonnet-5-5"}"#);
        t.observe_line(r#"{"type":"result","usage":{"output_tokens":1000000}}"#);
        record_usage_on_goal(&mut g, &t, &PriceTable::builtin());
        assert_eq!(g.cost_source, CostSource::Table);
        assert!(g.cost_estimated);
        assert!((g.cost_usd - 15.0).abs() < 1e-9);
    }

    #[test]
    fn record_usage_for_an_unknown_model_keeps_tokens_and_invents_no_price() {
        let mut g = goal("", None, 0.0, CostSource::None);
        let mut t = UsageTracker::default();
        t.observe_line(r#"{"type":"system","model":"qwen3.5:9b"}"#);
        t.observe_line(r#"{"type":"result","usage":{"input_tokens":7,"output_tokens":9}}"#);
        record_usage_on_goal(&mut g, &t, &PriceTable::builtin());
        assert_eq!((g.input_tokens, g.output_tokens), (7, 9));
        assert_eq!(g.cost_usd, 0.0);
        assert!(!g.cost_estimated);
        assert_eq!(g.cost_source, CostSource::None);
    }

    #[test]
    fn budget_limits_resolve_cli_over_default_and_validate() {
        let l = BudgetLimits::resolve(Some(5), None, Some(9), Some(2.5)).unwrap();
        assert_eq!(l.max_tokens, Some(5));
        assert_eq!(l.max_usd, Some(2.5));
        assert!(BudgetLimits::resolve(None, None, None, None)
            .unwrap()
            .is_unlimited());
        assert!(BudgetLimits::resolve(Some(0), None, None, None).is_err());
        for bad in [0.0, -1.0, f64::NAN] {
            let e = BudgetLimits::resolve(None, Some(bad), None, None).unwrap_err();
            assert!(e.contains("--max-usd"), "{e}");
        }
    }

    #[test]
    fn token_ceiling_counts_every_token_kind() {
        let l = BudgetLimits {
            max_tokens: Some(1_000),
            max_usd: None,
        };
        let under = TokenUsage {
            input: 100,
            output: 100,
            cache_read: 700,
            cache_creation: 99,
        };
        assert!(l.check(&under, None).is_none());
        let at = TokenUsage {
            cache_creation: 100,
            ..under
        };
        let a = l.check(&at, None).expect("ceiling reached");
        assert_eq!(a.limit, "max_tokens");
        assert_eq!(a.observed, 1_000.0);
        assert!(a.message.contains("1,000"), "{}", a.message);
        assert!(a.message.contains("incomplete"), "{}", a.message);
        assert!(a.message.contains("overshot"), "{}", a.message);
    }

    #[test]
    fn usd_ceiling_needs_a_price() {
        let l = BudgetLimits {
            max_tokens: None,
            max_usd: Some(2.0),
        };
        let u = TokenUsage::default();
        assert!(l.check(&u, Some(1.99)).is_none());
        assert!(
            l.check(&u, None).is_none(),
            "an unpriced model cannot hit a USD ceiling"
        );
        let a = l.check(&u, Some(2.0)).unwrap();
        assert_eq!(a.limit, "max_usd");
        assert!(a.message.contains("$2.00"), "{}", a.message);
    }

    #[test]
    fn completion_record_has_the_documented_shape() {
        let mut g = goal(
            "claude-sonnet-5-5",
            Some(OriginClass::Asked),
            1.5,
            CostSource::Runtime,
        );
        g.origin = Some("cos".to_string());
        let r = CompletionRecord::from_goal(&g);
        let v = serde_json::to_value(&r).unwrap();
        for key in [
            "goal_id",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "usd",
            "cost_estimated",
            "origin",
            "origin_class",
            "aborted",
        ] {
            assert!(v.get(key).is_some(), "missing {key} in {v}");
        }
        assert_eq!(v["origin_class"], "asked");
        assert_eq!(v["origin"], "cos");
        assert_eq!(v["cost_estimated"], false);
        assert_eq!(v["cost_source"], "runtime");
        assert_eq!(v["aborted"], false);
        assert!(v["outcome"].is_null());
        assert_eq!(v["cache_read_tokens"], 300);
    }

    #[test]
    fn completion_record_of_an_aborted_goal() {
        let mut g = goal("claude-sonnet-5-5", None, 0.0, CostSource::None);
        g.budget_abort = Some(BudgetAbort {
            limit: "max_tokens".into(),
            limit_value: 10.0,
            observed: 11.0,
            message: "m".into(),
        });
        let r = CompletionRecord::from_goal(&g);
        assert!(r.aborted);
        assert_eq!(r.outcome.as_deref(), Some("aborted_budget"));
        assert_eq!(
            r.origin_class,
            OriginClass::Unasked,
            "a missing class reads as unasked"
        );
    }

    #[test]
    fn group_by_parsing() {
        assert_eq!(
            parse_group_by("origin_class, project").unwrap(),
            vec![SpendGroup::OriginClass, SpendGroup::Project]
        );
        assert!(parse_group_by("").unwrap().is_empty());
        assert!(parse_group_by("color")
            .unwrap_err()
            .contains("origin_class"));
        assert!(parse_group_by("project,project").is_err());
    }

    #[test]
    fn time_bound_parsing() {
        assert!(parse_time_bound("2026-10-12").is_ok());
        assert!(parse_time_bound("2026-10-12T09:30:00Z").is_ok());
        assert!(parse_time_bound("yesterday")
            .unwrap_err()
            .contains("RFC 3339"));
    }

    fn corpus() -> Vec<(String, GoalRun)> {
        let mut a = goal(
            "claude-sonnet-5-5",
            Some(OriginClass::Asked),
            2.0,
            CostSource::Runtime,
        );
        let mut b = goal(
            "claude-sonnet-5-5",
            Some(OriginClass::Unasked),
            1.0,
            CostSource::Table,
        );
        let mut c = goal("qwen3.5:9b", None, 0.0, CostSource::None);
        let mut d = goal(
            "claude-opus-5-5",
            Some(OriginClass::Asked),
            4.0,
            CostSource::Runtime,
        );
        d.budget_abort = Some(BudgetAbort {
            limit: "max_usd".into(),
            limit_value: 4.0,
            observed: 4.0,
            message: String::new(),
        });
        let base = DateTime::parse_from_rfc3339("2026-10-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        a.created_at = base;
        b.created_at = base + chrono::Duration::days(1);
        c.created_at = base + chrono::Duration::days(2);
        d.created_at = base + chrono::Duration::days(3);
        vec![
            ("alpha".to_string(), a),
            ("alpha".to_string(), b),
            ("beta".to_string(), c),
            ("beta".to_string(), d),
        ]
    }

    #[test]
    fn a_goal_recorded_before_cost_tracking_is_priced_as_an_estimate() {
        let table = PriceTable::builtin();
        // Tokens and a priced model, but cost_source null: the real shape of
        // goals recorded 2026-10-10 to 12.
        let mut legacy = goal("claude-sonnet-5-5", None, 0.0, CostSource::None);
        legacy.input_tokens = 1_000_000;
        legacy.output_tokens = 0;
        legacy.cache_read_input_tokens = 0;
        legacy.cache_creation_input_tokens = 0;
        assert!(estimate_legacy_goal_cost(&mut legacy, &table));
        assert_eq!(legacy.cost_source, CostSource::Table);
        assert!(legacy.cost_estimated);
        assert!(legacy.cost_usd > 0.0);

        let r = aggregate(&[("p".to_string(), legacy.clone())], &[], None, None);
        assert_eq!(r.totals.unpriced_goals, 0);
        assert_eq!(r.totals.estimated_cost_goals, 1);
        assert!((r.totals.usd - legacy.cost_usd).abs() < 1e-12);
    }

    #[test]
    fn legacy_pricing_leaves_goals_without_tokens_or_a_priced_model_unpriced() {
        let table = PriceTable::builtin();
        let mut no_tokens = goal("claude-sonnet-5-5", None, 0.0, CostSource::None);
        no_tokens.input_tokens = 0;
        no_tokens.output_tokens = 0;
        no_tokens.cache_read_input_tokens = 0;
        no_tokens.cache_creation_input_tokens = 0;
        let mut unknown_model = goal("qwen3.5:9b", None, 0.0, CostSource::None);
        let mut no_model = goal("", None, 0.0, CostSource::None);
        for g in [&mut no_tokens, &mut unknown_model, &mut no_model] {
            assert!(!estimate_legacy_goal_cost(g, &table));
            assert_eq!(g.cost_source, CostSource::None);
            assert_eq!(g.cost_usd, 0.0);
        }
        // A runtime-priced goal is never repriced.
        let mut exact = goal("claude-sonnet-5-5", None, 9.0, CostSource::Runtime);
        assert!(!estimate_legacy_goal_cost(&mut exact, &table));
        assert_eq!(exact.cost_usd, 9.0);
    }

    #[test]
    fn aggregate_groups_by_origin_class_and_counts_cost_kinds() {
        let r = aggregate(&corpus(), &[SpendGroup::OriginClass], None, None);
        assert_eq!(
            r.groups.len(),
            2,
            "asked and unasked (a missing class is unasked)"
        );
        let asked = r
            .groups
            .iter()
            .find(|g| g.group["origin_class"] == "asked")
            .unwrap();
        assert_eq!(asked.totals.goals, 2);
        assert_eq!(asked.totals.exact_cost_goals, 2);
        assert_eq!(asked.totals.estimated_cost_goals, 0);
        assert!((asked.totals.usd - 6.0).abs() < 1e-9);
        assert_eq!(asked.totals.aborted_budget_goals, 1);
        let un = r
            .groups
            .iter()
            .find(|g| g.group["origin_class"] == "unasked")
            .unwrap();
        assert_eq!(un.totals.goals, 2);
        assert_eq!(un.totals.estimated_cost_goals, 1);
        assert_eq!(un.totals.unpriced_goals, 1);
        assert_eq!(r.totals.goals, 4);
        assert_eq!(r.totals.exact_cost_goals, 2);
        assert_eq!(r.totals.estimated_cost_goals, 1);
        assert_eq!(r.totals.unpriced_goals, 1);
        // Cache tokens are in the totals: 4 goals x (300 read + 40 write).
        assert_eq!(r.totals.cache_read_tokens, 1_200);
        assert_eq!(r.totals.cache_creation_tokens, 160);
        assert_eq!(r.totals.total_tokens, 4 * (10 + 20 + 300 + 40));
    }

    #[test]
    fn aggregate_groups_by_two_dimensions() {
        let r = aggregate(
            &corpus(),
            &[SpendGroup::OriginClass, SpendGroup::Project],
            None,
            None,
        );
        let keys: Vec<(String, String)> = r
            .groups
            .iter()
            .map(|g| (g.group["origin_class"].clone(), g.group["project"].clone()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("asked".to_string(), "alpha".to_string()),
                ("asked".to_string(), "beta".to_string()),
                ("unasked".to_string(), "alpha".to_string()),
                ("unasked".to_string(), "beta".to_string()),
            ]
        );
        assert_eq!(r.group_by, vec!["origin_class", "project"]);
    }

    #[test]
    fn aggregate_filters_by_since_and_until() {
        let since = parse_time_bound("2026-10-11").unwrap();
        let until = parse_time_bound("2026-10-13").unwrap();
        // Goals are created on 10, 11, 12 and 13 October: 11 and 12 qualify.
        let r = aggregate(&corpus(), &[], Some(since), Some(until));
        assert_eq!(r.totals.goals, 2);
        assert_eq!(r.groups.len(), 1, "no group_by is one group");
        assert!(r.groups[0].group.is_empty());
        assert_eq!(r.since, Some(since));
        let none = aggregate(&corpus(), &[], Some(until), None);
        assert_eq!(none.totals.goals, 1);
        let empty = aggregate(
            &corpus(),
            &[SpendGroup::Model],
            Some(until + chrono::Duration::days(30)),
            None,
        );
        assert_eq!(empty.totals.goals, 0);
        assert!(empty.groups.is_empty());
    }

    #[test]
    fn aggregate_by_model_labels_a_missing_model() {
        let mut g = goal("", None, 0.0, CostSource::None);
        g.agent_model = String::new();
        let r = aggregate(
            &[("p".into(), g)],
            &[SpendGroup::Model, SpendGroup::Origin],
            None,
            None,
        );
        assert_eq!(r.groups[0].group["model"], "unknown");
        assert_eq!(r.groups[0].group["origin"], "none");
    }

    #[test]
    fn report_serializes_with_flattened_totals() {
        let r = aggregate(&corpus(), &[SpendGroup::OriginClass], None, None);
        let v = serde_json::to_value(&r).unwrap();
        let g0 = &v["groups"][0];
        assert!(g0["group"].is_object());
        assert!(g0["goals"].is_number());
        assert!(g0["exact_cost_goals"].is_number());
        assert!(v["totals"]["estimated_cost_goals"].is_number());
        assert!(v["notes"].as_array().unwrap().len() >= 2);
    }
}
