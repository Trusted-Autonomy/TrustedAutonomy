//! Provider-neutral usage meters: how much of a plan window or an owner-set
//! budget has been used, in one shape any consumer (TA, Meridian, Wayfinder)
//! can read for any plan or any per-model budget.
//!
//! Two percentages, deliberately separate:
//!
//!   * `plan_usage_pct`: percent (0-100) of the provider's subscription window
//!     consumed, AS REPORTED BY THE PROVIDER. For Claude Code that is what
//!     `/usage` shows, delivered to the user's statusline command on stdin and
//!     captured by `ta usage capture`. `None` for API-key users: there is no plan.
//!   * `budget_used_pct`: percent of an owner-configured budget (USD and/or
//!     tokens, optionally per model, over a window) consumed, computed from the
//!     aggregate usage ledger. `None` when no budget applies. Can exceed 100.
//!
//! A consumer that wants one number uses [`UsageSummary::headline`].
//!
//! Plan data comes only from a statusline-fed snapshot behind the
//! [`PlanUsageSource`] trait. Nothing here reads OAuth tokens, the Keychain, or
//! calls an undocumented endpoint. Everything is pure (JSON in, meters out) or a
//! small file reader, so it is testable against a tempdir.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::usage_ledger::{
    by_kind, by_model, in_window, user_usage_dir, LedgerRead, UsageBreakdown, UsageRecord,
};

/// Source label for meters built from Claude Code's statusline data.
pub const SOURCE_CLAUDE_STATUSLINE: &str = "claude_code_statusline";
/// Source label for meters computed from the TA usage ledger.
pub const SOURCE_TA_LEDGER: &str = "ta_ledger";
/// Default age after which a plan snapshot is reported as stale.
pub const DEFAULT_PLAN_MAX_AGE_MINUTES: u64 = 15;
/// Length of Claude's rolling session window, used for a `session` budget.
pub const SESSION_WINDOW_HOURS: i64 = 5;
/// File name of the plan snapshot inside the user usage dir.
pub const SNAPSHOT_FILE: &str = "plan-usage.json";

/// The default snapshot path (`<user usage dir>/plan-usage.json`).
pub fn default_snapshot_path() -> PathBuf {
    user_usage_dir().join(SNAPSHOT_FILE)
}

// ── Core types ──────────────────────────────────────────────────────────────

/// The span a meter covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageWindow {
    /// A rolling 5 hour session.
    Session,
    /// The current UTC day.
    Day,
    /// The current UTC week (Monday start).
    Week,
    /// The current UTC calendar month.
    #[default]
    Month,
    /// A rolling number of hours (`window_hours` in the budget).
    Custom,
}

impl UsageWindow {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageWindow::Session => "session",
            UsageWindow::Day => "day",
            UsageWindow::Week => "week",
            UsageWindow::Month => "month",
            UsageWindow::Custom => "custom",
        }
    }

    fn rank(self) -> u8 {
        match self {
            UsageWindow::Session => 0,
            UsageWindow::Day => 1,
            UsageWindow::Week => 2,
            UsageWindow::Month => 3,
            UsageWindow::Custom => 4,
        }
    }
}

/// The start of the window containing `now` and when it resets (`None` for
/// rolling windows, which never reset at a fixed time).
pub fn window_range(
    window: UsageWindow,
    window_hours: Option<u64>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, Option<DateTime<Utc>>) {
    let midnight = |d: chrono::NaiveDate| Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap());
    match window {
        UsageWindow::Session => (now - Duration::hours(SESSION_WINDOW_HOURS), None),
        UsageWindow::Custom => (
            now - Duration::hours(window_hours.unwrap_or(24).max(1) as i64),
            None,
        ),
        UsageWindow::Day => {
            let start = midnight(now.date_naive());
            (start, Some(start + Duration::days(1)))
        }
        UsageWindow::Week => {
            let back = now.weekday().num_days_from_monday() as i64;
            let start = midnight(now.date_naive() - Duration::days(back));
            (start, Some(start + Duration::days(7)))
        }
        UsageWindow::Month => {
            let first = now.date_naive().with_day(1).unwrap();
            let next = if first.month() == 12 {
                chrono::NaiveDate::from_ymd_opt(first.year() + 1, 1, 1).unwrap()
            } else {
                chrono::NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1).unwrap()
            };
            (midnight(first), Some(midnight(next)))
        }
    }
}

/// What a meter is narrowed to: a model or a surface (for example `Opus`, or
/// `Claude Code`). `None` on a meter means the whole plan or all models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageScope {
    /// `model` or `surface`.
    pub kind: String,
    pub name: String,
}

impl UsageScope {
    pub fn model(name: &str) -> Self {
        Self {
            kind: "model".into(),
            name: name.into(),
        }
    }
    pub fn surface(name: &str) -> Self {
        Self {
            kind: "surface".into(),
            name: name.into(),
        }
    }
    fn same(&self, other: &UsageScope) -> bool {
        self.kind == other.kind && self.name.eq_ignore_ascii_case(&other.name)
    }
}

/// An owner-set limit. Either or both of `usd` and `tokens`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    pub usd: Option<f64>,
    pub tokens: Option<u64>,
}

/// What was used inside a meter's window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsedTotals {
    pub usd: f64,
    /// Every token of every kind.
    pub tokens: u64,
    /// Ledger records in the window that could not report tokens. Their calls
    /// count toward `usd` only if priced; their tokens are missing.
    pub unknown_token_records: u64,
}

/// One meter. See the module docs for the two percentages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageMeter {
    pub window: UsageWindow,
    pub scope: Option<UsageScope>,
    pub plan_usage_pct: Option<f64>,
    pub budget_used_pct: Option<f64>,
    pub budget: Option<Budget>,
    pub used: Option<UsedTotals>,
    pub resets_at: Option<DateTime<Utc>>,
    /// Where the numbers came from: `claude_code_statusline`, `ta_ledger`, or
    /// both joined with `+` when a plan meter and a budget meter were merged.
    pub source: String,
    /// When the provider data was captured (plan meters). `None` for a pure
    /// budget meter, which is computed now.
    pub captured_at: Option<DateTime<Utc>>,
    /// `true` when the plan data is older than the max age or its window has reset.
    pub stale: bool,
}

impl Default for UsageMeter {
    fn default() -> Self {
        Self {
            window: UsageWindow::Month,
            scope: None,
            plan_usage_pct: None,
            budget_used_pct: None,
            budget: None,
            used: None,
            resets_at: None,
            source: String::new(),
            captured_at: None,
            stale: false,
        }
    }
}

impl UsageMeter {
    /// The plan percentage if it can be trusted (present and not stale).
    pub fn fresh_plan_pct(&self) -> Option<f64> {
        self.plan_usage_pct.filter(|_| !self.stale)
    }

    /// The one number for this meter: the larger of the fresh plan percentage
    /// and the budget percentage (the limit closest to binding).
    pub fn effective_pct(&self) -> Option<f64> {
        match (self.fresh_plan_pct(), self.budget_used_pct) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// `plan` or `budget`: which limit [`effective_pct`](Self::effective_pct) is.
    pub fn effective_basis(&self) -> Option<&'static str> {
        match (self.fresh_plan_pct(), self.budget_used_pct) {
            (Some(a), Some(b)) => Some(if a >= b { "plan" } else { "budget" }),
            (Some(_), None) => Some("plan"),
            (None, Some(_)) => Some("budget"),
            (None, None) => None,
        }
    }
}

/// Pick the headline meter: among whole-plan (unscoped) meters when there are
/// any, else among all, the one with the highest effective percentage; ties go
/// to the shorter window. Stale plan data never makes a headline.
pub fn pick_headline(meters: &[UsageMeter]) -> Option<&UsageMeter> {
    let pool = |unscoped_only: bool| {
        meters
            .iter()
            .filter(move |m| (!unscoped_only || m.scope.is_none()) && m.effective_pct().is_some())
            .max_by(|a, b| {
                a.effective_pct()
                    .partial_cmp(&b.effective_pct())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.window.rank().cmp(&a.window.rank()))
            })
    };
    pool(true).or_else(|| pool(false))
}

// ── Budget math ─────────────────────────────────────────────────────────────

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Percent of `budget` that `used` consumed. With both a USD and a token limit
/// the larger percentage wins (the limit that binds first). A limit that is
/// absent, zero, negative or not finite is ignored; `None` when no valid limit
/// is left. Not clamped: 130.0 means 30% over.
pub fn budget_used_pct(used: &UsedTotals, budget: &Budget) -> Option<f64> {
    let usd = budget
        .usd
        .filter(|b| b.is_finite() && *b > 0.0)
        .map(|b| used.usd / b * 100.0);
    let tokens = budget
        .tokens
        .filter(|b| *b > 0)
        .map(|b| used.tokens as f64 / b as f64 * 100.0);
    let pct = match (usd, tokens) {
        (Some(a), Some(b)) => a.max(b),
        (a, b) => a.or(b)?,
    };
    pct.is_finite().then(|| round2(pct.max(0.0)))
}

/// Total the ledger records into [`UsedTotals`].
pub fn totals_of<'a>(records: impl IntoIterator<Item = &'a UsageRecord>) -> UsedTotals {
    let mut t = UsedTotals::default();
    for r in records {
        t.usd += r.usd;
        t.tokens = t.tokens.saturating_add(r.total_tokens());
        if r.tokens_unknown {
            t.unknown_token_records += 1;
        }
    }
    t
}

// ── Budget configuration ────────────────────────────────────────────────────

/// One budget rule from `[budget]` or `[budget.model."<name>"]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetRule {
    /// `session`, `day`, `week`, `month` (default) or `custom`.
    pub window: UsageWindow,
    pub usd: Option<f64>,
    pub tokens: Option<u64>,
    /// Hours of a `custom` window.
    pub window_hours: Option<u64>,
}

impl BudgetRule {
    pub fn is_set(&self) -> bool {
        self.usd.is_some() || self.tokens.is_some()
    }

    pub fn budget(&self) -> Budget {
        Budget {
            usd: self.usd,
            tokens: self.tokens,
        }
    }
}

/// The `[budget]` table: a default rule for all models plus per-model rules.
///
/// ```toml
/// [budget]
/// window = "month"
/// usd = 200.0
/// tokens = 50000000
///
/// [budget.model."claude-opus-5-5"]
/// window = "week"
/// usd = 50.0
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetConfig {
    #[serde(flatten)]
    pub default: BudgetRule,
    pub model: BTreeMap<String, BudgetRule>,
}

/// `[usage]` and `[budget]` of `.ta/daemon.toml` (project) and
/// `~/.config/ta/daemon.toml` (global), project winning.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageConfig {
    pub budget: BudgetConfig,
    /// Minutes after which a plan snapshot is stale.
    pub plan_max_age_minutes: u64,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            budget: BudgetConfig::default(),
            plan_max_age_minutes: DEFAULT_PLAN_MAX_AGE_MINUTES,
        }
    }
}

/// The global config file (`~/.config/ta/daemon.toml`).
pub fn default_global_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("ta")
        .join("daemon.toml")
}

fn validate_rule(label: &str, rule: &mut BudgetRule, warnings: &mut Vec<String>) {
    if let Some(u) = rule.usd {
        if !u.is_finite() || u <= 0.0 {
            warnings.push(format!(
                "{label}: usd = {u} is not a positive amount, so that limit is ignored. Set it to a USD amount above 0, or remove it."
            ));
            rule.usd = None;
        }
    }
    if rule.tokens == Some(0) {
        warnings.push(format!(
            "{label}: tokens = 0 is not a positive limit, so it is ignored. Set it above 0, or remove it."
        ));
        rule.tokens = None;
    }
}

impl UsageConfig {
    /// Parse the text of one config file. `origin` names the file in warnings.
    pub fn from_toml(text: &str, origin: &Path) -> Result<(Self, Vec<String>), String> {
        let root: toml::Value = toml::from_str(text).map_err(|e| {
            format!(
                "{} is not valid TOML: {e}. Fix the file; until then no budget from it is applied.",
                origin.display()
            )
        })?;
        let mut cfg = UsageConfig::default();
        let mut warnings = Vec::new();
        if let Some(b) = root.get("budget") {
            let mut b: BudgetConfig = b.clone().try_into().map_err(|e| {
                format!(
                    "{}: the [budget] table is invalid: {e}. Expected optional window \
                     (session|day|week|month|custom), usd, tokens, window_hours, and \
                     [budget.model.\"<name>\"] tables with the same keys.",
                    origin.display()
                )
            })?;
            let o = origin.display();
            validate_rule(&format!("{o} [budget]"), &mut b.default, &mut warnings);
            for (name, r) in b.model.iter_mut() {
                validate_rule(&format!("{o} [budget.model.{name}]"), r, &mut warnings);
            }
            cfg.budget = b;
        }
        if let Some(v) = root
            .get("usage")
            .and_then(|u| u.get("plan_max_age_minutes"))
        {
            match v.as_integer().filter(|n| *n > 0) {
                Some(n) => cfg.plan_max_age_minutes = n as u64,
                None => warnings.push(format!(
                    "{}: [usage] plan_max_age_minutes must be a whole number of minutes above 0; using {}.",
                    origin.display(),
                    DEFAULT_PLAN_MAX_AGE_MINUTES
                )),
            }
        }
        Ok((cfg, warnings))
    }

    fn load_file(path: &Path, warnings: &mut Vec<String>) -> Option<(Self, bool)> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warnings.push(format!(
                    "could not read {}: {e}. Check the file is readable; no budget from it is applied.",
                    path.display()
                ));
                return None;
            }
        };
        match Self::from_toml(&text, path) {
            Ok((c, w)) => {
                warnings.extend(w);
                let has_age = text.contains("plan_max_age_minutes");
                Some((c, has_age))
            }
            Err(msg) => {
                warnings.push(msg);
                None
            }
        }
    }

    /// Load the global file then the project file; the project wins per key.
    /// Problems are returned as warnings (and never stop a report).
    pub fn load(project_root: &Path, global_file: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut out = UsageConfig::default();
        if let Some((g, age)) = Self::load_file(global_file, &mut warnings) {
            out.budget = g.budget;
            if age {
                out.plan_max_age_minutes = g.plan_max_age_minutes;
            }
        }
        let project_file = project_root.join(".ta").join("daemon.toml");
        if let Some((p, age)) = Self::load_file(&project_file, &mut warnings) {
            if p.budget.default.is_set() {
                out.budget.default = p.budget.default;
            }
            out.budget.model.extend(p.budget.model);
            if age {
                out.plan_max_age_minutes = p.plan_max_age_minutes;
            }
        }
        for w in &warnings {
            tracing::warn!("{w}");
        }
        (out, warnings)
    }
}

// ── Claude Code statusline parsing ──────────────────────────────────────────

/// What the statusline JSON said about the plan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedPlan {
    /// `pro`, `max`, `team`, `enterprise`, or `None`.
    pub plan_type: Option<String>,
    /// `false` for API key, Bedrock or Vertex sessions: there is no plan.
    pub rate_limits_available: bool,
    pub meters: Vec<UsageMeter>,
    /// Rows that were skipped, and why.
    pub notes: Vec<String>,
}

fn percent_of(v: &Value) -> Option<f64> {
    ["percent", "used_percentage", "utilization"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_f64))
        .filter(|p| p.is_finite())
        .map(|p| round2(p.clamp(0.0, 100.0)))
}

fn reset_of(v: &Value) -> Option<DateTime<Utc>> {
    match v.get("resets_at")? {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc)),
        Value::Number(n) => {
            let secs = n.as_f64()?;
            // Epoch milliseconds are far larger than any plausible epoch seconds.
            let secs = if secs > 1e11 { secs / 1000.0 } else { secs };
            Utc.timestamp_opt(secs as i64, 0).single()
        }
        _ => None,
    }
}

fn plan_meter(
    window: UsageWindow,
    scope: Option<UsageScope>,
    row: &Value,
    pct: f64,
    captured_at: DateTime<Utc>,
) -> UsageMeter {
    UsageMeter {
        window,
        scope,
        plan_usage_pct: Some(pct),
        resets_at: reset_of(row),
        source: SOURCE_CLAUDE_STATUSLINE.to_string(),
        captured_at: Some(captured_at),
        ..UsageMeter::default()
    }
}

fn display_name(v: &Value) -> Option<String> {
    v.get("display_name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Parse the object Claude Code passes to a statusline command (or the
/// captured subset of it). Both shapes are read; the newer `rate_limits.limits[]`
/// wins when present and non-empty. Never fails: an unreadable row is skipped
/// with a note and an absent `rate_limits` is "no plan data".
pub fn parse_claude_statusline(v: &Value, captured_at: DateTime<Utc>) -> ParsedPlan {
    let mut out = ParsedPlan {
        plan_type: v
            .get("subscription_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        ..ParsedPlan::default()
    };
    let rl = v.get("rate_limits").filter(|r| r.is_object());
    out.rate_limits_available = v
        .get("rate_limits_available")
        .and_then(Value::as_bool)
        .unwrap_or(rl.is_some());
    let Some(rl) = rl else {
        return out;
    };

    let push = |out: &mut ParsedPlan, m: UsageMeter| {
        if out
            .meters
            .iter()
            .any(|e| e.window == m.window && e.scope == m.scope)
        {
            return; // the first row for a (window, scope) wins
        }
        out.meters.push(m);
    };

    let limits = rl
        .get("limits")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty());
    if let Some(limits) = limits {
        for row in limits {
            let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
            let Some(pct) = percent_of(row) else {
                out.notes.push(format!(
                    "rate limit row {kind:?} has no readable percent; skipped"
                ));
                continue;
            };
            let scope = row.get("scope").filter(|s| s.is_object()).and_then(|s| {
                s.get("model")
                    .and_then(display_name)
                    .map(|n| UsageScope::model(&n))
                    .or_else(|| {
                        s.get("surface")
                            .and_then(display_name)
                            .map(|n| UsageScope::surface(&n))
                    })
            });
            let (window, scope) = match kind {
                "session" => (UsageWindow::Session, scope),
                "weekly_all" => (UsageWindow::Week, None),
                "weekly_scoped" => (UsageWindow::Week, scope),
                other => {
                    out.notes
                        .push(format!("rate limit kind {other:?} is not known; skipped"));
                    continue;
                }
            };
            if kind == "weekly_scoped" && scope.is_none() {
                out.notes
                    .push("a weekly_scoped row has no model or surface name; skipped".into());
                continue;
            }
            push(&mut out, plan_meter(window, scope, row, pct, captured_at));
        }
        return out;
    }

    // Older shape.
    for (key, window, scope) in [
        ("five_hour", UsageWindow::Session, None),
        ("seven_day", UsageWindow::Week, None),
        (
            "seven_day_opus",
            UsageWindow::Week,
            Some(UsageScope::model("Opus")),
        ),
        (
            "seven_day_sonnet",
            UsageWindow::Week,
            Some(UsageScope::model("Sonnet")),
        ),
    ] {
        let Some(row) = rl.get(key).filter(|r| r.is_object()) else {
            continue;
        };
        match percent_of(row) {
            Some(pct) => push(&mut out, plan_meter(window, scope, row, pct, captured_at)),
            None => out
                .notes
                .push(format!("{key} has no readable percent; skipped")),
        }
    }
    if let Some(rows) = rl.get("model_scoped").and_then(Value::as_array) {
        for row in rows {
            let (Some(name), Some(pct)) = (display_name(row), percent_of(row)) else {
                out.notes
                    .push("a model_scoped row has no display_name or percent; skipped".into());
                continue;
            };
            push(
                &mut out,
                plan_meter(
                    UsageWindow::Week,
                    Some(UsageScope::model(&name)),
                    row,
                    pct,
                    captured_at,
                ),
            );
        }
    }
    out
}

// ── Plan sources ────────────────────────────────────────────────────────────

/// How trustworthy the plan reading is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Fresh,
    /// Older than the max age, or every window has reset. Values are shown
    /// flagged, never as current.
    Stale,
    /// No snapshot has been captured yet.
    Missing,
    /// The provider reports no plan (API key, Bedrock, Vertex), or the source
    /// is the no-op source.
    NotApplicable,
    /// The snapshot exists but could not be read.
    Unreadable,
}

/// What a [`PlanUsageSource`] returned.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanUsageReading {
    pub status: PlanStatus,
    pub plan_type: Option<String>,
    pub captured_at: Option<DateTime<Utc>>,
    pub age_secs: Option<i64>,
    pub meters: Vec<UsageMeter>,
    /// One actionable sentence for the status line of a report.
    pub message: String,
}

/// A source of plan usage. Other providers add an implementation; consumers
/// only see [`PlanUsageReading`] and [`UsageMeter`].
pub trait PlanUsageSource {
    fn name(&self) -> &'static str;
    fn read(&self, now: DateTime<Utc>) -> PlanUsageReading;
}

/// For API-key users and anyone without a plan source: always "no plan".
pub struct NoPlanUsage;

impl PlanUsageSource for NoPlanUsage {
    fn name(&self) -> &'static str {
        "none"
    }
    fn read(&self, _now: DateTime<Utc>) -> PlanUsageReading {
        PlanUsageReading {
            status: PlanStatus::NotApplicable,
            plan_type: None,
            captured_at: None,
            age_secs: None,
            meters: Vec::new(),
            message: "No subscription plan is configured as a usage source, so plan_usage_pct \
                      is null. Set a [budget] to track spend instead."
                .to_string(),
        }
    }
}

/// The on-disk snapshot written by `ta usage capture` or
/// `scripts/ta-usage-capture.py`: when it was captured plus the statusline
/// fields that carry plan data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanSnapshotFile {
    pub schema: u32,
    pub provider: String,
    pub captured_at: Option<DateTime<Utc>>,
    /// The statusline JSON (or the subset holding `rate_limits`,
    /// `rate_limits_available` and `subscription_type`).
    pub statusline: Value,
}

/// Keep only the statusline fields the reader needs. A statusline object also
/// carries the working directory, transcript path and similar, which this file
/// has no use for.
pub fn snapshot_from_statusline(
    statusline: &Value,
    captured_at: DateTime<Utc>,
) -> PlanSnapshotFile {
    let mut keep = serde_json::Map::new();
    for k in ["rate_limits", "rate_limits_available", "subscription_type"] {
        if let Some(v) = statusline.get(k) {
            keep.insert(k.to_string(), v.clone());
        }
    }
    PlanSnapshotFile {
        schema: 1,
        provider: "claude_code".to_string(),
        captured_at: Some(captured_at),
        statusline: Value::Object(keep),
    }
}

/// Write `snapshot` to `path` atomically (temp file in the same directory, then
/// rename), so a reader never sees half a file.
pub fn write_snapshot(path: &Path, snapshot: &PlanSnapshotFile) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| {
        format!(
            "could not create {} for the plan usage snapshot. Check it is writable, or set TA_USAGE_DIR",
            dir.display()
        )
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("could not create a temp file in {}", dir.display()))?;
    tmp.write_all(serde_json::to_string_pretty(snapshot)?.as_bytes())?;
    tmp.persist(path).map_err(|e| {
        anyhow::anyhow!(
            "could not move the snapshot into place at {}: {}. Check the path is writable",
            path.display(),
            e.error
        )
    })?;
    Ok(())
}

/// Plan usage fed by Claude Code's statusline snapshot.
pub struct ClaudeCodeStatusline {
    pub path: PathBuf,
    pub max_age: Duration,
}

impl ClaudeCodeStatusline {
    pub fn new(path: impl Into<PathBuf>, max_age_minutes: u64) -> Self {
        Self {
            path: path.into(),
            max_age: Duration::minutes(max_age_minutes.max(1) as i64),
        }
    }
}

/// How to start capturing, shown whenever no snapshot exists.
pub const CAPTURE_HINT: &str = "Chain the capture script from your Claude Code statusline command \
(docs/USAGE.md, \"Usage and budget\"): pipe the statusline JSON to `ta usage capture` or \
`python3 scripts/ta-usage-capture.py`. It reads no credentials and calls no endpoint.";

impl PlanUsageSource for ClaudeCodeStatusline {
    fn name(&self) -> &'static str {
        SOURCE_CLAUDE_STATUSLINE
    }

    fn read(&self, now: DateTime<Utc>) -> PlanUsageReading {
        let unreadable = |why: String| PlanUsageReading {
            status: PlanStatus::Unreadable,
            plan_type: None,
            captured_at: None,
            age_secs: None,
            meters: Vec::new(),
            message: why,
        };
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return PlanUsageReading {
                    status: PlanStatus::Missing,
                    plan_type: None,
                    captured_at: None,
                    age_secs: None,
                    meters: Vec::new(),
                    message: format!(
                        "No plan usage snapshot at {}. {CAPTURE_HINT}",
                        self.path.display()
                    ),
                }
            }
            Err(e) => {
                return unreadable(format!(
                    "Could not read the plan usage snapshot {}: {e}. Check the file is readable.",
                    self.path.display()
                ))
            }
        };
        let file: PlanSnapshotFile = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                return unreadable(format!(
                    "The plan usage snapshot {} is not valid JSON ({e}). It is rewritten on the \
                     next statusline refresh; delete it if it stays broken.",
                    self.path.display()
                ))
            }
        };
        let Some(captured_at) = file.captured_at else {
            return unreadable(format!(
                "The plan usage snapshot {} has no captured_at, so its age is unknown and it \
                 cannot be trusted. Re-capture with `ta usage capture`.",
                self.path.display()
            ));
        };
        let parsed = parse_claude_statusline(&file.statusline, captured_at);
        let age = now - captured_at;
        let too_old = age > self.max_age;
        let mut meters = parsed.meters;
        for m in &mut meters {
            m.stale = too_old || m.resets_at.is_some_and(|r| r <= now);
        }
        if !parsed.rate_limits_available || meters.is_empty() {
            let why = if parsed.rate_limits_available {
                "the snapshot holds no rate-limit rows"
            } else {
                "Claude Code reports no rate limits for this session (API key, Bedrock or Vertex)"
            };
            return PlanUsageReading {
                status: PlanStatus::NotApplicable,
                plan_type: parsed.plan_type,
                captured_at: Some(captured_at),
                age_secs: Some(age.num_seconds()),
                meters,
                message: format!("No plan usage: {why}, so plan_usage_pct is null."),
            };
        }
        let all_stale = meters.iter().all(|m| m.stale);
        let (status, message) = if all_stale {
            (
                PlanStatus::Stale,
                format!(
                    "Plan usage is STALE: captured {} ago (max age {} min) or its window has \
                     reset. Values are flagged and left out of the headline. Open Claude Code to \
                     refresh the snapshot.",
                    human_age(age),
                    self.max_age.num_minutes()
                ),
            )
        } else {
            (
                PlanStatus::Fresh,
                format!("Plan usage captured {} ago.", human_age(age)),
            )
        };
        PlanUsageReading {
            status,
            plan_type: parsed.plan_type,
            captured_at: Some(captured_at),
            age_secs: Some(age.num_seconds()),
            meters,
            message,
        }
    }
}

/// `45s`, `12m`, `3h 5m`, `2d`.
pub fn human_age(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    match s {
        0..=89 => format!("{s}s"),
        90..=5399 => format!("{}m", s / 60),
        5400..=172_799 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d", s / 86_400),
    }
}

// ── Budget meters, merging, summary ─────────────────────────────────────────

/// One budget meter per configured rule, computed from the ledger records.
pub fn budget_meters(
    config: &BudgetConfig,
    records: &[UsageRecord],
    now: DateTime<Utc>,
) -> Vec<UsageMeter> {
    let mut out = Vec::new();
    let mut one = |scope: Option<UsageScope>, rule: &BudgetRule, filter: Option<&str>| {
        if !rule.is_set() {
            return;
        }
        let (start, resets_at) = window_range(rule.window, rule.window_hours, now);
        let hits = in_window(records, Some(start))
            .into_iter()
            .filter(|r| r.ts <= now)
            .filter(|r| filter.is_none_or(|f| r.model.to_lowercase().contains(f)));
        let used = totals_of(hits);
        let budget = rule.budget();
        out.push(UsageMeter {
            window: rule.window,
            scope,
            budget_used_pct: budget_used_pct(&used, &budget),
            budget: Some(budget),
            used: Some(used),
            resets_at,
            source: SOURCE_TA_LEDGER.to_string(),
            ..UsageMeter::default()
        });
    };
    one(None, &config.default, None);
    for (name, rule) in &config.model {
        let lower = name.to_lowercase();
        one(Some(UsageScope::model(name)), rule, Some(&lower));
    }
    out
}

/// Fold budget meters into plan meters that share a window and scope, so a
/// subscription user with a budget sees one row with both numbers.
pub fn merge_meters(plan: Vec<UsageMeter>, budget: Vec<UsageMeter>) -> Vec<UsageMeter> {
    let mut out = plan;
    for b in budget {
        let hit = out.iter_mut().find(|p| {
            p.window == b.window
                && p.budget.is_none()
                && match (&p.scope, &b.scope) {
                    (None, None) => true,
                    (Some(x), Some(y)) => x.same(y),
                    _ => false,
                }
        });
        match hit {
            Some(p) => {
                p.budget_used_pct = b.budget_used_pct;
                p.budget = b.budget;
                p.used = b.used;
                p.source = format!("{}+{}", p.source, b.source);
            }
            None => out.push(b),
        }
    }
    out.sort_by_key(|m| (m.scope.is_some(), m.window.rank()));
    out
}

/// The headline: one number, where it came from, and whether it is trustworthy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    pub pct: f64,
    /// `plan` or `budget`.
    pub basis: String,
    pub window: UsageWindow,
    pub scope: Option<UsageScope>,
    pub source: String,
}

/// Plan side of a summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanReport {
    pub status: PlanStatus,
    pub plan_type: Option<String>,
    pub captured_at: Option<DateTime<Utc>>,
    pub age_secs: Option<i64>,
    pub max_age_minutes: u64,
    pub message: String,
}

/// Ledger side of a summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerReport {
    /// `global` (all projects) or `project`.
    pub scope: String,
    pub path: String,
    pub found: bool,
    pub records: u64,
    pub unparsable_lines: u64,
    pub since: Option<DateTime<Utc>>,
    pub by_kind: Vec<UsageBreakdown>,
    pub by_model: Vec<UsageBreakdown>,
    pub total: UsedTotals,
}

/// Everything `ta usage` shows, and what `ta cost report` embeds as `usage`.
///
/// `plan_usage_pct` and `budget_used_pct` are the two stable top-level fields:
/// the headline whole-plan percentage (fresh data only) and the headline budget
/// percentage. `headline_pct` is the single number to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageSummary {
    pub generated_at: DateTime<Utc>,
    pub plan_usage_pct: Option<f64>,
    pub budget_used_pct: Option<f64>,
    pub headline_pct: Option<f64>,
    pub headline_basis: Option<String>,
    pub headline: Option<Headline>,
    pub plan: Option<PlanReport>,
    pub meters: Vec<UsageMeter>,
    pub ledger: Option<LedgerReport>,
    pub notes: Vec<String>,
    pub hints: Vec<String>,
}

impl Default for UsageSummary {
    fn default() -> Self {
        Self {
            generated_at: Utc::now(),
            plan_usage_pct: None,
            budget_used_pct: None,
            headline_pct: None,
            headline_basis: None,
            headline: None,
            plan: None,
            meters: Vec::new(),
            ledger: None,
            notes: Vec::new(),
            hints: Vec::new(),
        }
    }
}

/// Inputs to [`summarize`].
pub struct SummaryInputs<'a> {
    pub now: DateTime<Utc>,
    pub plan: PlanUsageReading,
    pub max_age_minutes: u64,
    pub config: &'a BudgetConfig,
    pub ledger: &'a LedgerRead,
    pub ledger_path: String,
    /// `global` or `project`.
    pub ledger_scope: String,
    /// Start of the by-kind / by-model aggregation window.
    pub aggregate_since: Option<DateTime<Utc>>,
    pub config_warnings: Vec<String>,
}

/// Combine a plan reading, the budget config and the ledger into one summary.
pub fn summarize(i: SummaryInputs<'_>) -> UsageSummary {
    let budget = budget_meters(i.config, &i.ledger.records, i.now);
    let any_budget = !budget.is_empty();
    let plan_status = i.plan.status;
    let meters = merge_meters(i.plan.meters.clone(), budget);

    let headline_meter = pick_headline(&meters);
    let headline = headline_meter.and_then(|m| {
        Some(Headline {
            pct: m.effective_pct()?,
            basis: m.effective_basis()?.to_string(),
            window: m.window,
            scope: m.scope.clone(),
            source: m.source.clone(),
        })
    });
    // Whole-plan fresh percentage and whole-budget percentage, independent of
    // which one wins the headline.
    let max_of = |f: &dyn Fn(&UsageMeter) -> Option<f64>| {
        meters
            .iter()
            .filter(|m| m.scope.is_none())
            .filter_map(f)
            .fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))))
    };
    let plan_usage_pct = max_of(&|m| m.fresh_plan_pct());
    let budget_used_pct = max_of(&|m| m.budget_used_pct).or_else(|| {
        // No unscoped budget: fall back to the highest scoped one so a
        // per-model-only budget still shows a number.
        meters
            .iter()
            .filter_map(|m| m.budget_used_pct)
            .fold(None, |a: Option<f64>, v| Some(a.map_or(v, |x| x.max(v))))
    });

    let mut notes = i.config_warnings;
    let mut hints = Vec::new();
    if i.ledger.unparsable_lines > 0 {
        notes.push(format!(
            "{} line(s) in the usage ledger {} could not be parsed and were not counted. Inspect or remove them.",
            i.ledger.unparsable_lines, i.ledger_path
        ));
    }
    let unknown = totals_of(in_window(&i.ledger.records, i.aggregate_since)).unknown_token_records;
    if unknown > 0 {
        notes.push(format!(
            "{unknown} ledger record(s) could not report tokens (counted as calls, tokens unknown). \
             Token budgets and totals understate real use by that much."
        ));
    }
    if matches!(plan_status, PlanStatus::Stale) {
        notes.push(i.plan.message.clone());
    }
    if matches!(plan_status, PlanStatus::Missing) {
        hints.push(i.plan.message.clone());
    }
    if !any_budget {
        hints.push(
            "No budget is set, so budget_used_pct is null. Add to .ta/daemon.toml (or \
             ~/.config/ta/daemon.toml):\n  [budget]\n  window = \"month\"\n  usd = 200.0\n\
             Per model: [budget.model.\"claude-opus-5-5\"] with window/usd/tokens."
                .to_string(),
        );
    }
    if headline.is_none() && any_budget {
        notes.push(
            "A budget is set but no meter has a usable percentage (check the limits are above 0)."
                .into(),
        );
    }

    let aggregate: Vec<&UsageRecord> = in_window(&i.ledger.records, i.aggregate_since);
    let ledger = LedgerReport {
        scope: i.ledger_scope,
        path: i.ledger_path,
        found: i.ledger.found,
        records: aggregate.len() as u64,
        unparsable_lines: i.ledger.unparsable_lines,
        since: i.aggregate_since,
        by_kind: by_kind(aggregate.iter().copied()),
        by_model: by_model(aggregate.iter().copied()),
        total: totals_of(aggregate.iter().copied()),
    };

    UsageSummary {
        generated_at: i.now,
        plan_usage_pct,
        budget_used_pct,
        headline_pct: headline.as_ref().map(|h| h.pct),
        headline_basis: headline.as_ref().map(|h| h.basis.clone()),
        headline,
        plan: Some(PlanReport {
            status: plan_status,
            plan_type: i.plan.plan_type,
            captured_at: i.plan.captured_at,
            age_secs: i.plan.age_secs,
            max_age_minutes: i.max_age_minutes,
            message: i.plan.message,
        }),
        meters,
        ledger: Some(ledger),
        notes,
        hints,
    }
}

/// Convenience: read the plan source, the config and the ledger, and summarize.
/// `project_only` reads the project ledger instead of the global roll-up.
pub fn gather_summary(
    project_root: &Path,
    now: DateTime<Utc>,
    project_only: bool,
    aggregate_since: Option<DateTime<Utc>>,
    max_age_override: Option<u64>,
    snapshot_path: Option<&Path>,
) -> UsageSummary {
    gather_summary_with(
        project_root,
        now,
        project_only,
        aggregate_since,
        max_age_override,
        snapshot_path,
        &default_global_config_path(),
        &crate::usage_ledger::UsageLedger::global(),
    )
}

/// [`gather_summary`] with the global config file and global ledger given
/// explicitly (tests, and callers with a non-default layout).
#[allow(clippy::too_many_arguments)]
pub fn gather_summary_with(
    project_root: &Path,
    now: DateTime<Utc>,
    project_only: bool,
    aggregate_since: Option<DateTime<Utc>>,
    max_age_override: Option<u64>,
    snapshot_path: Option<&Path>,
    global_config: &Path,
    global_ledger: &crate::usage_ledger::UsageLedger,
) -> UsageSummary {
    use crate::usage_ledger::UsageLedger;
    let (cfg, warnings) = UsageConfig::load(project_root, global_config);
    let max_age = max_age_override.unwrap_or(cfg.plan_max_age_minutes);
    let snap = snapshot_path
        .map(Path::to_path_buf)
        .unwrap_or_else(default_snapshot_path);
    let plan = ClaudeCodeStatusline::new(snap, max_age).read(now);
    let project_ledger = UsageLedger::for_project(project_root);
    let (ledger, scope, path) = if project_only {
        (
            project_ledger.read(),
            "project",
            project_ledger.path().to_path_buf(),
        )
    } else {
        let g = global_ledger.read();
        if g.found {
            (g, "global", global_ledger.path().to_path_buf())
        } else {
            (
                project_ledger.read(),
                "project",
                project_ledger.path().to_path_buf(),
            )
        }
    };
    summarize(SummaryInputs {
        now,
        plan,
        max_age_minutes: max_age,
        config: &cfg.budget,
        ledger: &ledger,
        ledger_path: path.display().to_string(),
        ledger_scope: scope.to_string(),
        aggregate_since,
        config_warnings: warnings,
    })
}

// ── Before / after marks (golden replay) ────────────────────────────────────

/// The two percentages at one instant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageMark {
    pub plan_usage_pct: Option<f64>,
    pub budget_used_pct: Option<f64>,
    /// When the plan snapshot behind `plan_usage_pct` was captured.
    pub plan_captured_at: Option<DateTime<Utc>>,
    pub taken_at: Option<DateTime<Utc>>,
}

impl UsageMark {
    pub fn from_summary(s: &UsageSummary) -> Self {
        Self {
            plan_usage_pct: s.plan_usage_pct,
            budget_used_pct: s.budget_used_pct,
            plan_captured_at: s.plan.as_ref().and_then(|p| p.captured_at),
            taken_at: Some(s.generated_at),
        }
    }
}

impl UsageMark {
    /// Fill in a missing `budget_used_pct` once the budget is known to exist.
    ///
    /// The "before" mark is taken before a run and "after" once it ends. If the
    /// `[budget]` was absent or unreadable at the start (for example the config
    /// was added, or rewritten non-atomically, while a long run was in flight)
    /// the before value is `None` and no delta can be computed, even though the
    /// budget is configured by the time the run ends. This re-reads the current
    /// config and ledger as of `self.taken_at` (records stamped later are
    /// ignored by the meter math), so a configured budget always yields a before
    /// value (0% when nothing had been used yet). Returns true when it filled it.
    pub fn backfill_budget(
        &mut self,
        project_root: &Path,
        project_only: bool,
        global_config: &Path,
        global_ledger: &crate::usage_ledger::UsageLedger,
    ) -> bool {
        let Some(taken_at) = self.taken_at else {
            return false;
        };
        if self.budget_used_pct.is_some() {
            return false;
        }
        let s = gather_summary_with(
            project_root,
            taken_at,
            project_only,
            None,
            None,
            None,
            global_config,
            global_ledger,
        );
        self.budget_used_pct = s.budget_used_pct;
        self.budget_used_pct.is_some()
    }
}

/// Usage before and after a measured run, plus the change. Every field is
/// optional and defaulted so records written before this existed still load.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageDelta {
    pub before: Option<UsageMark>,
    pub after: Option<UsageMark>,
    /// `after - before` of `plan_usage_pct`, only when the plan snapshot was
    /// refreshed during the run. Headless `claude --print` does not refresh the
    /// statusline, so this is often `None`; `note` says why.
    pub plan_usage_pct_delta: Option<f64>,
    /// `after - before` of `budget_used_pct`. Needs a budget, and the run's
    /// goal recorded in the ledger.
    pub budget_used_pct_delta: Option<f64>,
    pub note: Option<String>,
}

impl UsageDelta {
    pub fn compute(before: UsageMark, after: UsageMark) -> Self {
        let mut notes = Vec::new();
        let refreshed = match (before.plan_captured_at, after.plan_captured_at) {
            (Some(b), Some(a)) => a > b,
            (None, Some(_)) => true,
            _ => false,
        };
        let plan = match (before.plan_usage_pct, after.plan_usage_pct) {
            (Some(b), Some(a)) if refreshed => Some(round2(a - b)),
            (Some(_), Some(_)) => {
                notes.push(
                    "plan_usage_pct was not refreshed during the run (the statusline snapshot \
                     did not change), so no plan delta is reported. A headless agent does not \
                     update it; read budget_used_pct_delta, or compare the snapshot after an \
                     interactive Claude Code turn."
                        .to_string(),
                );
                None
            }
            _ => {
                notes.push(
                    "plan_usage_pct was unavailable before or after the run (no fresh \
                     snapshot), so no plan delta is reported."
                        .to_string(),
                );
                None
            }
        };
        let budget = match (before.budget_used_pct, after.budget_used_pct) {
            (Some(b), Some(a)) => Some(round2(a - b)),
            _ => {
                notes.push(
                    "budget_used_pct is unavailable before or after the run (no [budget] configured, or \
                     it could not be read), so no budget delta is reported."
                        .to_string(),
                );
                None
            }
        };
        Self {
            before: Some(before),
            after: Some(after),
            plan_usage_pct_delta: plan,
            budget_used_pct_delta: budget,
            note: (!notes.is_empty()).then(|| notes.join(" ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_ledger::UsageKind;
    use serde_json::json;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 21, h, m, 0).unwrap()
    }

    fn new_shape() -> Value {
        json!({
            "subscription_type": "max",
            "rate_limits_available": true,
            "rate_limits": {"limits": [
                {"kind":"session","group":"g","percent":42.5,"resets_at":"2026-10-21T15:00:00Z","scope":null,"severity":"ok","is_active":true},
                {"kind":"weekly_all","group":"g","percent":61.0,"resets_at":"2026-10-26T00:00:00Z","scope":null},
                {"kind":"weekly_scoped","group":"g","percent":88.0,"resets_at":null,
                 "scope":{"model":{"display_name":"Opus"},"surface":null}},
                {"kind":"weekly_scoped","percent":10,"scope":{"surface":{"display_name":"Claude Code"}}},
                {"kind":"mystery","percent":1}
            ]}
        })
    }

    #[test]
    fn parses_the_newer_limits_shape() {
        let p = parse_claude_statusline(&new_shape(), t(12, 0));
        assert_eq!(p.plan_type.as_deref(), Some("max"));
        assert!(p.rate_limits_available);
        assert_eq!(p.meters.len(), 4);
        let s = &p.meters[0];
        assert_eq!(
            (s.window, s.plan_usage_pct),
            (UsageWindow::Session, Some(42.5))
        );
        assert_eq!(s.resets_at, Some(t(15, 0)));
        assert_eq!(s.source, SOURCE_CLAUDE_STATUSLINE);
        let opus = p
            .meters
            .iter()
            .find(|m| m.scope == Some(UsageScope::model("Opus")))
            .unwrap();
        assert_eq!(
            (opus.window, opus.plan_usage_pct),
            (UsageWindow::Week, Some(88.0))
        );
        let surf = p
            .meters
            .iter()
            .find(|m| m.scope == Some(UsageScope::surface("Claude Code")))
            .unwrap();
        assert_eq!(surf.plan_usage_pct, Some(10.0));
        assert!(p.notes.iter().any(|n| n.contains("mystery")));
    }

    #[test]
    fn parses_the_older_shape_including_model_scoped_and_epoch_resets() {
        let v = json!({
            "rate_limits": {
                "five_hour": {"utilization": 30.0, "resets_at": "2026-10-21T15:00:00Z"},
                "seven_day": {"used_percentage": 55.5, "resets_at": 1792800000},
                "seven_day_opus": {"utilization": 70},
                "seven_day_sonnet": null,
                "model_scoped": [{"display_name":"Haiku","utilization":5.0,"resets_at":null}, {"utilization": 3}]
            }
        });
        let p = parse_claude_statusline(&v, t(12, 0));
        assert!(
            p.rate_limits_available,
            "available defaults to true when rate_limits is present"
        );
        assert_eq!(p.meters.len(), 4);
        assert_eq!(p.meters[0].window, UsageWindow::Session);
        assert_eq!(p.meters[1].plan_usage_pct, Some(55.5));
        assert_eq!(
            p.meters[1].resets_at,
            Some(Utc.timestamp_opt(1_792_800_000, 0).unwrap())
        );
        assert!(p
            .meters
            .iter()
            .any(|m| m.scope == Some(UsageScope::model("Haiku"))));
        assert!(p.notes.iter().any(|n| n.contains("model_scoped")));
    }

    #[test]
    fn newer_limits_win_over_older_keys() {
        let mut v = new_shape();
        v["rate_limits"]["five_hour"] = json!({"utilization": 99.0});
        let p = parse_claude_statusline(&v, t(12, 0));
        assert_eq!(p.meters[0].plan_usage_pct, Some(42.5));
    }

    #[test]
    fn api_key_and_garbage_input_yield_no_meters_without_failing() {
        let api = json!({"rate_limits_available": false, "subscription_type": null});
        let p = parse_claude_statusline(&api, t(12, 0));
        assert!(!p.rate_limits_available && p.meters.is_empty());
        for junk in [
            json!(null),
            json!([]),
            json!({"rate_limits": "x"}),
            json!({"rate_limits": {"limits": [{"kind":"session"}]}}),
        ] {
            let p = parse_claude_statusline(&junk, t(12, 0));
            assert!(p.meters.is_empty());
        }
    }

    #[test]
    fn percent_is_clamped_to_0_100() {
        let v = json!({"rate_limits": {"limits":[{"kind":"session","percent":140.0},{"kind":"weekly_all","percent":-5}]}});
        let p = parse_claude_statusline(&v, t(12, 0));
        assert_eq!(p.meters[0].plan_usage_pct, Some(100.0));
        assert_eq!(p.meters[1].plan_usage_pct, Some(0.0));
    }

    fn snapshot_at(dir: &Path, captured: DateTime<Utc>, v: &Value) -> PathBuf {
        let p = dir.join("plan-usage.json");
        write_snapshot(&p, &snapshot_from_statusline(v, captured)).unwrap();
        p
    }

    #[test]
    fn fresh_stale_missing_and_not_applicable_are_distinguished() {
        let dir = tempfile::tempdir().unwrap();
        let src = ClaudeCodeStatusline::new(dir.path().join("nope.json"), 15);
        let r = src.read(t(12, 0));
        assert_eq!(r.status, PlanStatus::Missing);
        assert!(r.message.contains("ta usage capture"));

        let p = snapshot_at(dir.path(), t(12, 0), &new_shape());
        let src = ClaudeCodeStatusline::new(&p, 15);
        let r = src.read(t(12, 10));
        assert_eq!(r.status, PlanStatus::Fresh);
        assert!(r.meters.iter().all(|m| !m.stale));
        assert_eq!(r.age_secs, Some(600));

        // Past the max age: stale, and the message says so.
        let r = src.read(t(12, 16));
        assert_eq!(r.status, PlanStatus::Stale);
        assert!(r.meters.iter().all(|m| m.stale));
        assert!(r.message.contains("STALE"), "{}", r.message);

        // A configurable max age.
        assert_eq!(
            ClaudeCodeStatusline::new(&p, 60).read(t(12, 16)).status,
            PlanStatus::Fresh
        );

        // The session window reset at 15:00: that meter is stale even if the file is fresh.
        let r = ClaudeCodeStatusline::new(&p, 600).read(t(15, 30));
        let session = r
            .meters
            .iter()
            .find(|m| m.window == UsageWindow::Session)
            .unwrap();
        assert!(session.stale);
        let week = r
            .meters
            .iter()
            .find(|m| m.window == UsageWindow::Week && m.scope.is_none())
            .unwrap();
        assert!(!week.stale);

        let api = snapshot_at(
            dir.path(),
            t(12, 0),
            &json!({"rate_limits_available": false}),
        );
        let r = ClaudeCodeStatusline::new(&api, 15).read(t(12, 1));
        assert_eq!(r.status, PlanStatus::NotApplicable);
        assert!(r.meters.is_empty());
        assert_eq!(NoPlanUsage.read(t(12, 0)).status, PlanStatus::NotApplicable);
    }

    #[test]
    fn unreadable_snapshots_say_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plan-usage.json");
        std::fs::write(&p, "{ not json").unwrap();
        let r = ClaudeCodeStatusline::new(&p, 15).read(t(12, 0));
        assert_eq!(r.status, PlanStatus::Unreadable);
        assert!(r.message.contains("not valid JSON"));
        // No captured_at: age unknown, so it is not trusted.
        std::fs::write(
            &p,
            r#"{"statusline":{"rate_limits":{"limits":[{"kind":"session","percent":1}]}}}"#,
        )
        .unwrap();
        let r = ClaudeCodeStatusline::new(&p, 15).read(t(12, 0));
        assert_eq!(r.status, PlanStatus::Unreadable);
        assert!(r.message.contains("captured_at"));
    }

    #[test]
    fn snapshot_keeps_only_plan_fields_and_write_is_atomic_replace() {
        let dir = tempfile::tempdir().unwrap();
        let v = json!({"cwd":"/secret/project","transcript_path":"/x","rate_limits_available":true,
            "rate_limits":{"five_hour":{"utilization":1.0}}});
        let p = snapshot_at(dir.path(), t(12, 0), &v);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("secret") && !text.contains("transcript"));
        // Overwrite leaves exactly one file behind (no stray temp files).
        snapshot_at(dir.path(), t(12, 5), &v);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn budget_math_usd_tokens_both_and_edge_cases() {
        let used = UsedTotals {
            usd: 25.0,
            tokens: 3_000_000,
            unknown_token_records: 0,
        };
        let b = |usd, tokens| Budget { usd, tokens };
        assert_eq!(budget_used_pct(&used, &b(Some(100.0), None)), Some(25.0));
        assert_eq!(
            budget_used_pct(&used, &b(None, Some(10_000_000))),
            Some(30.0)
        );
        // Both: the binding limit wins.
        assert_eq!(
            budget_used_pct(&used, &b(Some(100.0), Some(10_000_000))),
            Some(30.0)
        );
        // Over budget is not clamped.
        assert_eq!(budget_used_pct(&used, &b(Some(20.0), None)), Some(125.0));
        // Zero, negative, NaN and absent budgets give None, never a divide by zero.
        assert_eq!(budget_used_pct(&used, &b(Some(0.0), None)), None);
        assert_eq!(budget_used_pct(&used, &b(Some(-5.0), None)), None);
        assert_eq!(budget_used_pct(&used, &b(Some(f64::NAN), None)), None);
        assert_eq!(budget_used_pct(&used, &b(None, Some(0))), None);
        assert_eq!(budget_used_pct(&used, &b(None, None)), None);
        // A zero limit next to a valid one is ignored, not fatal.
        assert_eq!(
            budget_used_pct(&used, &b(Some(0.0), Some(10_000_000))),
            Some(30.0)
        );
        assert_eq!(
            budget_used_pct(&UsedTotals::default(), &b(Some(10.0), None)),
            Some(0.0)
        );
    }

    fn rec(model: &str, tokens: u64, usd: f64, ts: DateTime<Utc>) -> UsageRecord {
        UsageRecord {
            ts,
            kind: UsageKind::Goal,
            model: model.into(),
            output_tokens: tokens,
            usd,
            ..UsageRecord::default()
        }
    }

    #[test]
    fn windows_are_utc_aligned() {
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap(); // a Wednesday
        let (s, r) = window_range(UsageWindow::Week, None, now);
        assert_eq!(s, Utc.with_ymd_and_hms(2026, 10, 19, 0, 0, 0).unwrap());
        assert_eq!(
            r,
            Some(Utc.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).unwrap())
        );
        let (s, r) = window_range(UsageWindow::Month, None, now);
        assert_eq!(s, Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap());
        assert_eq!(r, Some(Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap()));
        let dec = Utc.with_ymd_and_hms(2026, 12, 31, 23, 0, 0).unwrap();
        assert_eq!(
            window_range(UsageWindow::Month, None, dec).1,
            Some(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap())
        );
        assert_eq!(
            window_range(UsageWindow::Session, None, now).0,
            now - Duration::hours(5)
        );
        assert_eq!(
            window_range(UsageWindow::Custom, Some(48), now).0,
            now - Duration::hours(48)
        );
        assert!(window_range(UsageWindow::Session, None, now).1.is_none());
    }

    fn cfg(text: &str) -> BudgetConfig {
        UsageConfig::from_toml(text, Path::new("daemon.toml"))
            .unwrap()
            .0
            .budget
    }

    #[test]
    fn budget_config_parses_default_and_per_model_rules_and_warns_on_bad_limits() {
        let c = cfg("[budget]\nwindow = \"week\"\nusd = 100.0\n[budget.model.\"claude-opus-5-5\"]\nwindow = \"day\"\ntokens = 1000000\n");
        assert_eq!(c.default.window, UsageWindow::Week);
        assert_eq!(c.default.usd, Some(100.0));
        assert_eq!(c.model["claude-opus-5-5"].tokens, Some(1_000_000));
        let (c, w) =
            UsageConfig::from_toml("[budget]\nusd = -3.0\ntokens = 0\n", Path::new("d.toml"))
                .unwrap();
        assert!(!c.budget.default.is_set());
        assert_eq!(w.len(), 2);
        assert!(w.iter().all(|m| m.contains("ignored")));
        assert!(
            UsageConfig::from_toml("[budget]\nwindow = \"fortnight\"\n", Path::new("d.toml"))
                .is_err()
        );
        assert!(!cfg("[server]\nport = 1\n").default.is_set());
    }

    #[test]
    fn project_config_overrides_global_and_unrelated_daemon_toml_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.toml");
        std::fs::write(&global, "[budget]\nusd = 500.0\n[budget.model.\"a\"]\nusd = 1.0\n[usage]\nplan_max_age_minutes = 30\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/daemon.toml"),
            "[server]\nport = 7700\n[budget]\nusd = 50.0\n[budget.model.\"b\"]\ntokens = 5\n",
        )
        .unwrap();
        let (c, w) = UsageConfig::load(dir.path(), &global);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.budget.default.usd, Some(50.0));
        assert_eq!(c.budget.model.len(), 2);
        assert_eq!(c.plan_max_age_minutes, 30);
        // A broken project file is a warning, and the global budget still applies.
        std::fs::write(dir.path().join(".ta/daemon.toml"), "[budget\n").unwrap();
        let (c, w) = UsageConfig::load(dir.path(), &global);
        assert_eq!(c.budget.default.usd, Some(500.0));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn per_model_budget_counts_only_that_model_inside_the_window() {
        let now = t(12, 0);
        let records = vec![
            rec("claude-opus-5-5", 600, 6.0, t(9, 0)),
            rec("claude-sonnet-5-5", 400, 4.0, t(9, 0)),
            // Yesterday: outside a day window.
            rec("claude-opus-5-5", 9999, 99.0, now - Duration::days(1)),
        ];
        let c = cfg("[budget]\nwindow=\"day\"\nusd=20.0\n[budget.model.\"opus\"]\nwindow=\"day\"\ntokens=1000\n");
        let m = budget_meters(&c, &records, now);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].budget_used_pct, Some(50.0)); // 10 of 20 USD
        assert_eq!(m[1].scope, Some(UsageScope::model("opus")));
        assert_eq!(m[1].budget_used_pct, Some(60.0)); // 600 of 1000 tokens
        assert_eq!(m[1].used.as_ref().unwrap().tokens, 600);
    }

    #[test]
    fn headline_prefers_unscoped_highest_and_ignores_stale_plan_data() {
        let mk = |w, scope: Option<UsageScope>, plan, budget, stale| UsageMeter {
            window: w,
            scope,
            plan_usage_pct: plan,
            budget_used_pct: budget,
            stale,
            ..UsageMeter::default()
        };
        let meters = vec![
            mk(UsageWindow::Session, None, Some(40.0), None, false),
            mk(UsageWindow::Week, None, Some(61.0), None, false),
            mk(
                UsageWindow::Week,
                Some(UsageScope::model("Opus")),
                Some(95.0),
                None,
                false,
            ),
        ];
        let h = pick_headline(&meters).unwrap();
        assert_eq!(
            h.plan_usage_pct,
            Some(61.0),
            "scoped meters do not beat the whole plan"
        );
        // Only a scoped meter has data: it is used rather than showing nothing.
        let only = vec![mk(
            UsageWindow::Week,
            Some(UsageScope::model("Opus")),
            None,
            Some(5.0),
            false,
        )];
        assert_eq!(pick_headline(&only).unwrap().budget_used_pct, Some(5.0));
        // Stale plan data is never the headline.
        let stale = vec![mk(UsageWindow::Week, None, Some(99.0), Some(10.0), true)];
        let h = pick_headline(&stale).unwrap();
        assert_eq!(
            (h.effective_pct(), h.effective_basis()),
            (Some(10.0), Some("budget"))
        );
        let all_stale = vec![mk(UsageWindow::Week, None, Some(99.0), None, true)];
        assert!(pick_headline(&all_stale).is_none());
        // Ties go to the shorter window.
        let tie = vec![
            mk(UsageWindow::Week, None, Some(50.0), None, false),
            mk(UsageWindow::Session, None, Some(50.0), None, false),
        ];
        assert_eq!(pick_headline(&tie).unwrap().window, UsageWindow::Session);
        assert!(pick_headline(&[]).is_none());
    }

    fn inputs<'a>(
        plan: PlanUsageReading,
        config: &'a BudgetConfig,
        ledger: &'a LedgerRead,
    ) -> SummaryInputs<'a> {
        SummaryInputs {
            now: t(12, 0),
            plan,
            max_age_minutes: 15,
            config,
            ledger,
            ledger_path: "ledger.jsonl".into(),
            ledger_scope: "project".into(),
            aggregate_since: None,
            config_warnings: vec![],
        }
    }

    #[test]
    fn api_plan_user_with_a_budget_gets_budget_as_the_headline() {
        let c = cfg("[budget]\nwindow=\"month\"\nusd=100.0\n");
        let ledger = LedgerRead {
            records: vec![rec("m", 10, 30.0, t(1, 0))],
            found: true,
            unparsable_lines: 0,
        };
        let s = summarize(inputs(NoPlanUsage.read(t(12, 0)), &c, &ledger));
        assert_eq!(s.plan_usage_pct, None);
        assert_eq!(s.budget_used_pct, Some(30.0));
        assert_eq!(s.headline_pct, Some(30.0));
        assert_eq!(s.headline_basis.as_deref(), Some("budget"));
    }

    #[test]
    fn subscription_user_with_a_matching_window_budget_gets_one_merged_row() {
        let dir = tempfile::tempdir().unwrap();
        let p = snapshot_at(dir.path(), t(11, 55), &new_shape());
        let plan = ClaudeCodeStatusline::new(&p, 15).read(t(12, 0));
        let c = cfg("[budget]\nwindow=\"week\"\nusd=100.0\n");
        let ledger = LedgerRead {
            records: vec![rec("m", 10, 70.0, t(1, 0))],
            found: true,
            unparsable_lines: 0,
        };
        let s = summarize(inputs(plan, &c, &ledger));
        let week: Vec<_> = s
            .meters
            .iter()
            .filter(|m| m.window == UsageWindow::Week && m.scope.is_none())
            .collect();
        assert_eq!(week.len(), 1, "plan and budget for the same window merge");
        assert_eq!(week[0].plan_usage_pct, Some(61.0));
        assert_eq!(week[0].budget_used_pct, Some(70.0));
        assert!(week[0].source.contains('+'));
        assert_eq!(s.plan_usage_pct, Some(61.0));
        assert_eq!(s.headline_pct, Some(70.0));
        assert_eq!(s.headline_basis.as_deref(), Some("budget"));
    }

    #[test]
    fn nothing_configured_gives_nulls_and_actionable_hints_not_zeros() {
        let ledger = LedgerRead::default();
        let missing = ClaudeCodeStatusline::new(PathBuf::from("/definitely/not/here.json"), 15)
            .read(t(12, 0));
        let s = summarize(inputs(missing, &BudgetConfig::default(), &ledger));
        assert!(s.plan_usage_pct.is_none() && s.budget_used_pct.is_none() && s.headline.is_none());
        assert!(s.hints.iter().any(|h| h.contains("[budget]")));
        assert!(s.hints.iter().any(|h| h.contains("ta usage capture")));
    }

    #[test]
    fn stale_plan_data_is_flagged_and_left_out_of_plan_usage_pct() {
        let dir = tempfile::tempdir().unwrap();
        let p = snapshot_at(dir.path(), t(9, 0), &new_shape());
        let plan = ClaudeCodeStatusline::new(&p, 15).read(t(12, 0));
        let s = summarize(inputs(
            plan,
            &BudgetConfig::default(),
            &LedgerRead::default(),
        ));
        assert_eq!(s.plan_usage_pct, None);
        assert!(s
            .meters
            .iter()
            .all(|m| m.stale && m.plan_usage_pct.is_some()));
        assert!(s.notes.iter().any(|n| n.contains("STALE")));
        assert_eq!(s.plan.as_ref().unwrap().status, PlanStatus::Stale);
    }

    #[test]
    fn unknown_token_records_and_unparsable_lines_are_surfaced() {
        let c = cfg("[budget]\ntokens=1000\n");
        let mut unknown = rec("m", 0, 0.0, t(1, 0));
        unknown.tokens_unknown = true;
        let ledger = LedgerRead {
            records: vec![unknown],
            found: true,
            unparsable_lines: 2,
        };
        let s = summarize(inputs(NoPlanUsage.read(t(12, 0)), &c, &ledger));
        assert!(s
            .notes
            .iter()
            .any(|n| n.contains("could not report tokens")));
        assert!(s.notes.iter().any(|n| n.contains("2 line(s)")));
        assert_eq!(s.ledger.unwrap().total.unknown_token_records, 1);
    }

    #[test]
    fn usage_delta_needs_a_refreshed_snapshot_for_the_plan_but_not_for_the_budget() {
        let mark = |plan, budget, cap: Option<DateTime<Utc>>| UsageMark {
            plan_usage_pct: plan,
            budget_used_pct: budget,
            plan_captured_at: cap,
            taken_at: None,
        };
        // Snapshot refreshed during the run: both deltas.
        let d = UsageDelta::compute(
            mark(Some(40.0), Some(10.0), Some(t(10, 0))),
            mark(Some(43.5), Some(12.5), Some(t(10, 20))),
        );
        assert_eq!(d.plan_usage_pct_delta, Some(3.5));
        assert_eq!(d.budget_used_pct_delta, Some(2.5));
        assert!(d.note.is_none());
        // Same snapshot before and after: no plan delta (never a fake 0), with a reason.
        let d = UsageDelta::compute(
            mark(Some(40.0), None, Some(t(10, 0))),
            mark(Some(40.0), None, Some(t(10, 0))),
        );
        assert_eq!(d.plan_usage_pct_delta, None);
        let note = d.note.unwrap();
        assert!(
            note.contains("not refreshed") && note.contains("no [budget]"),
            "{note}"
        );
        // Missing plan data on either side.
        let d = UsageDelta::compute(mark(None, None, None), mark(Some(1.0), None, Some(t(1, 0))));
        assert_eq!(d.plan_usage_pct_delta, None);
        // A bare object loads.
        let empty: UsageDelta = serde_json::from_str("{}").unwrap();
        assert!(empty.before.is_none());
    }

    #[test]
    fn meter_and_summary_json_shape_is_stable_and_old_json_loads() {
        let c = cfg("[budget]\nwindow=\"month\"\nusd=100.0\n");
        let ledger = LedgerRead {
            records: vec![rec("claude-sonnet-5-5", 10, 25.0, t(1, 0))],
            found: true,
            unparsable_lines: 0,
        };
        let s = summarize(inputs(NoPlanUsage.read(t(12, 0)), &c, &ledger));
        let v = serde_json::to_value(&s).unwrap();
        for k in [
            "plan_usage_pct",
            "budget_used_pct",
            "headline_pct",
            "headline_basis",
            "meters",
            "ledger",
            "plan",
            "notes",
            "hints",
            "generated_at",
        ] {
            assert!(v.get(k).is_some(), "missing top-level key {k}");
        }
        assert!(v["plan_usage_pct"].is_null());
        assert_eq!(v["budget_used_pct"], json!(25.0));
        let m = &v["meters"][0];
        for k in [
            "window",
            "scope",
            "plan_usage_pct",
            "budget_used_pct",
            "budget",
            "used",
            "resets_at",
            "source",
            "captured_at",
            "stale",
        ] {
            assert!(m.get(k).is_some(), "missing meter key {k}");
        }
        assert_eq!(m["window"], json!("month"));
        assert_eq!(m["budget"], json!({"usd": 100.0, "tokens": null}));
        assert_eq!(m["source"], json!("ta_ledger"));
        // Round trip, and a bare/old object loads thanks to serde(default).
        let back: UsageSummary = serde_json::from_value(v).unwrap();
        assert_eq!(back.budget_used_pct, s.budget_used_pct);
        let old: UsageMeter =
            serde_json::from_str(r#"{"window":"week","plan_usage_pct":5.0}"#).unwrap();
        assert_eq!(old.plan_usage_pct, Some(5.0));
        assert!(!old.stale);
        let empty: UsageSummary = serde_json::from_str("{}").unwrap();
        assert!(empty.meters.is_empty());
    }
    #[test]
    fn golden_style_before_mark_with_global_budget_and_empty_ledger_is_zero_not_none() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("daemon.toml");
        std::fs::write(&cfg, "[budget]\nwindow = \"month\"\nusd = 100.0\n").unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let ledger = crate::usage_ledger::UsageLedger::new(dir.path().join("g.jsonl"));
        let snap = dir.path().join("none.json");
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap();
        let before = UsageMark::from_summary(&gather_summary_with(
            &proj,
            now,
            false,
            None,
            None,
            Some(&snap),
            &cfg,
            &ledger,
        ));
        assert_eq!(before.budget_used_pct, Some(0.0), "{before:?}");
        ledger
            .append(&rec("claude-opus-5-5", 10, 0.4, now - Duration::minutes(1)))
            .unwrap();
        let after = UsageMark::from_summary(&gather_summary_with(
            &proj,
            now,
            false,
            None,
            None,
            Some(&snap),
            &cfg,
            &ledger,
        ));
        let d = UsageDelta::compute(before, after);
        assert_eq!(d.budget_used_pct_delta, Some(0.4), "{d:?}");
    }

    #[test]
    fn budget_added_during_a_run_still_gives_a_before_value_and_delta() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("daemon.toml");
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let ledger = crate::usage_ledger::UsageLedger::new(dir.path().join("g.jsonl"));
        let snap = dir.path().join("none.json");
        let t0 = Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap();
        let t1 = t0 + Duration::minutes(20);
        // No budget at the start of the run.
        let mut before = UsageMark::from_summary(&gather_summary_with(
            &proj,
            t0,
            false,
            None,
            None,
            Some(&snap),
            &cfg,
            &ledger,
        ));
        assert_eq!(before.budget_used_pct, None);
        // The budget appears and the run records usage before it ends.
        std::fs::write(&cfg, "[budget]\nwindow = \"month\"\nusd = 100.0\n").unwrap();
        ledger
            .append(&rec("claude-opus-5-5", 10, 0.4, t0 + Duration::minutes(5)))
            .unwrap();
        let after = UsageMark::from_summary(&gather_summary_with(
            &proj,
            t1,
            false,
            None,
            None,
            Some(&snap),
            &cfg,
            &ledger,
        ));
        assert_eq!(after.budget_used_pct, Some(0.4));
        assert!(before.backfill_budget(&proj, false, &cfg, &ledger));
        assert_eq!(
            before.budget_used_pct,
            Some(0.0),
            "usage after t0 not counted"
        );
        let d = UsageDelta::compute(before, after);
        assert_eq!(d.budget_used_pct_delta, Some(0.4), "{d:?}");
    }
}
