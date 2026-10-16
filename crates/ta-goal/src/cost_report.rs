//! Spend across TA goals and interactive agent sessions (`ta cost report`).
//!
//! `GET /api/spend` only sees TA goals. The expensive workload is often a long
//! interactive Claude Code or OMP session, so this module reads three sources
//! into one list of [`CostRow`]s and groups them:
//!
//! 1. TA goal records (the same `GoalRun`s `spend::aggregate` totals);
//! 2. Claude Code transcripts, `~/.claude/projects/**/*.jsonl`;
//! 3. OMP transcripts, `~/.omp/agent/sessions/**/*.jsonl`.
//!
//! Counting rules that matter for correctness:
//!
//! - A Claude transcript repeats an assistant message once per streamed content
//!   block, all with the same `message.id` and (usually) growing usage. Rows are
//!   deduplicated by `message.id`, last row wins, across every file read.
//!   `model == "<synthetic>"` rows are not API calls and are skipped.
//! - OMP records a per-message `usage.cost.total`; it is used as the exact cost.
//! - A cost is `exact` when the source reported it and `estimated` when it came
//!   from the [`PriceTable`]. A model with no price row is `unpriced`: its tokens
//!   count and its cost is 0. A price is never invented.
//!
//! Everything here is pure (files in, report out) so it is testable against a
//! tempdir fixture. The CLI in `apps/ta-cli/src/commands/cost.rs` renders it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::spend::{self, SpendReport};
use crate::token_cost::{CostSource, PriceTable, TokenUsage};
use crate::GoalRun;

/// Where a row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSourceKind {
    Goals,
    Claude,
    Omp,
}

impl CostSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CostSourceKind::Goals => "ta_goals",
            CostSourceKind::Claude => "claude_code",
            CostSourceKind::Omp => "omp",
        }
    }
}

/// How trustworthy a row's cost is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostKind {
    /// The runtime or OMP reported it.
    Exact,
    /// Computed from a price-table row.
    Estimated,
    /// No price: tokens are counted, cost is 0.
    Unpriced,
}

/// One API turn (transcripts) or one goal (TA goals).
#[derive(Debug, Clone, PartialEq)]
pub struct CostRow {
    pub source: CostSourceKind,
    /// Session id (transcripts) or goal id.
    pub session: String,
    pub ts: Option<DateTime<Utc>>,
    pub project: String,
    pub model: String,
    pub usage: TokenUsage,
    pub usd: f64,
    pub kind: CostKind,
    /// Prompt size of the turn: input + cache read + cache creation. `None`
    /// for goals, which are many turns.
    pub context_tokens: Option<u64>,
    pub origin_class: Option<String>,
}

// ── Context buckets ─────────────────────────────────────────────────────────

/// Bucket labels in order, as printed.
pub const CONTEXT_BUCKETS: [&str; 5] = ["0-50k", "50-100k", "100-200k", "200-400k", ">400k"];

/// The bucket label for a prompt size in tokens.
pub fn context_bucket(tokens: u64) -> &'static str {
    match tokens {
        0..=49_999 => CONTEXT_BUCKETS[0],
        50_000..=99_999 => CONTEXT_BUCKETS[1],
        100_000..=199_999 => CONTEXT_BUCKETS[2],
        200_000..=399_999 => CONTEXT_BUCKETS[3],
        _ => CONTEXT_BUCKETS[4],
    }
}

// ── Scanning ────────────────────────────────────────────────────────────────

/// What a scan of one transcript source found. Reported in the header so the
/// reader knows what the numbers rest on.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SourceStatus {
    pub source: String,
    /// The directory looked in.
    pub path: String,
    /// `false` when the directory does not exist (not an error).
    pub found: bool,
    pub files: u64,
    /// Rows kept (after deduplication).
    pub rows: u64,
    /// Repeated `message.id` rows folded into an earlier one.
    pub deduplicated: u64,
    /// `<synthetic>` rows skipped.
    pub synthetic_skipped: u64,
    /// Lines that were not valid JSON (a torn last line of a live session).
    pub unparsable_lines: u64,
    /// Files that could not be read, with the reason.
    pub unreadable_files: Vec<String>,
}

/// Rows plus the status of the scan that produced them.
#[derive(Debug, Default, Clone)]
pub struct Scan {
    pub rows: Vec<CostRow>,
    pub status: SourceStatus,
}

fn walk_jsonl(dir: &Path, out: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // file_type() does not follow symlinks, so a link cannot loop the walk.
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk_jsonl(&path, out, errors),
            Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "jsonl") => {
                out.push(path)
            }
            _ => {}
        }
    }
}

fn jsonl_files(root: &Path, status: &mut SourceStatus) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_jsonl(root, &mut files, &mut status.unreadable_files);
    files.sort();
    status.files = files.len() as u64;
    files
}

fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn parse_ts(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// A project name from a working directory: the directory above a TA staging
/// copy or a Claude worktree when the cwd is inside one, else the last
/// component.
pub fn project_from_cwd(cwd: &str) -> String {
    let norm = cwd.replace('\\', "/");
    let mut base = norm.as_str();
    for marker in ["/.ta/staging/", "/.claude/worktrees/"] {
        if let Some(i) = base.find(marker) {
            base = &base[..i];
        }
    }
    base.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

fn cost_of(
    table: &PriceTable,
    model: &str,
    usage: &TokenUsage,
    one_hour: u64,
    reported: Option<f64>,
) -> (f64, CostKind) {
    if let Some(usd) = reported.filter(|v| v.is_finite() && *v >= 0.0) {
        return (usd, CostKind::Exact);
    }
    match table.price_split(model, usage, one_hour) {
        Some(usd) => (usd, CostKind::Estimated),
        None => (0.0, CostKind::Unpriced),
    }
}

/// Read Claude Code transcripts under `root` (normally `~/.claude/projects`).
/// A missing `root` is reported in the status, not an error.
pub fn scan_claude(root: &Path, table: &PriceTable) -> Scan {
    let mut scan = Scan {
        status: SourceStatus {
            source: CostSourceKind::Claude.as_str().to_string(),
            path: root.display().to_string(),
            found: root.is_dir(),
            ..SourceStatus::default()
        },
        ..Scan::default()
    };
    if !scan.status.found {
        return scan;
    }
    let files = jsonl_files(root, &mut scan.status);
    // message.id -> index into scan.rows; the last row for an id replaces it.
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for file in files {
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(path = %file.display(), error = %e, "could not read a Claude transcript; skipped");
                scan.status
                    .unreadable_files
                    .push(format!("{}: {e}", file.display()));
                continue;
            }
        };
        let dir_project = file
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                scan.status.unparsable_lines += 1;
                continue;
            };
            if v.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(msg) = v.get("message") else {
                continue;
            };
            let Some(u) = msg.get("usage") else {
                continue;
            };
            let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
            if model == "<synthetic>" {
                scan.status.synthetic_skipped += 1;
                continue;
            }
            let usage = TokenUsage {
                input: u64_at(u, "input_tokens"),
                output: u64_at(u, "output_tokens"),
                cache_read: u64_at(u, "cache_read_input_tokens"),
                cache_creation: u64_at(u, "cache_creation_input_tokens"),
            };
            let one_hour = u
                .get("cache_creation")
                .map(|c| u64_at(c, "ephemeral_1h_input_tokens"))
                .unwrap_or(0);
            let (usd, kind) = cost_of(
                table,
                model,
                &usage,
                one_hour,
                v.get("costUSD").and_then(Value::as_f64),
            );
            let project = v
                .get("cwd")
                .and_then(Value::as_str)
                .map(project_from_cwd)
                .unwrap_or_else(|| dir_project.clone());
            let row = CostRow {
                source: CostSourceKind::Claude,
                session: v
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                ts: v.get("timestamp").and_then(parse_ts),
                project,
                model: model.to_string(),
                usage,
                usd,
                kind,
                context_tokens: Some(usage.input + usage.cache_read + usage.cache_creation),
                origin_class: None,
            };
            match msg.get("id").and_then(Value::as_str) {
                Some(id) => match by_id.get(id) {
                    Some(&i) => {
                        scan.rows[i] = row;
                        scan.status.deduplicated += 1;
                    }
                    None => {
                        by_id.insert(id.to_string(), scan.rows.len());
                        scan.rows.push(row);
                    }
                },
                // No id to deduplicate on: keep the row.
                None => scan.rows.push(row),
            }
        }
    }
    scan.status.rows = scan.rows.len() as u64;
    scan
}

/// Read OMP transcripts under `root` (normally `~/.omp/agent/sessions`).
pub fn scan_omp(root: &Path, table: &PriceTable) -> Scan {
    let mut scan = Scan {
        status: SourceStatus {
            source: CostSourceKind::Omp.as_str().to_string(),
            path: root.display().to_string(),
            found: root.is_dir(),
            ..SourceStatus::default()
        },
        ..Scan::default()
    };
    if !scan.status.found {
        return scan;
    }
    let files = jsonl_files(root, &mut scan.status);
    for file in files {
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(path = %file.display(), error = %e, "could not read an OMP transcript; skipped");
                scan.status
                    .unreadable_files
                    .push(format!("{}: {e}", file.display()));
                continue;
            }
        };
        let mut session = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut project = file
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                scan.status.unparsable_lines += 1;
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("session") => {
                    if let Some(id) = v.get("id").and_then(Value::as_str) {
                        session = id.to_string();
                    }
                    if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
                        project = project_from_cwd(cwd);
                    }
                }
                Some("message") => {
                    let Some(msg) = v.get("message") else {
                        continue;
                    };
                    if msg.get("role").and_then(Value::as_str) != Some("assistant") {
                        continue;
                    }
                    let Some(u) = msg.get("usage") else {
                        continue;
                    };
                    let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
                    let usage = TokenUsage {
                        input: u64_at(u, "input"),
                        output: u64_at(u, "output"),
                        cache_read: u64_at(u, "cacheRead"),
                        cache_creation: u64_at(u, "cacheWrite"),
                    };
                    let one_hour = u.get("cttl").map(|c| u64_at(c, "ephemeral1h")).unwrap_or(0);
                    let reported = u
                        .get("cost")
                        .and_then(|c| c.get("total"))
                        .and_then(Value::as_f64);
                    let (usd, kind) = cost_of(table, model, &usage, one_hour, reported);
                    scan.rows.push(CostRow {
                        source: CostSourceKind::Omp,
                        session: session.clone(),
                        ts: v.get("timestamp").and_then(parse_ts),
                        project: project.clone(),
                        model: model.to_string(),
                        usage,
                        usd,
                        kind,
                        context_tokens: Some(usage.input + usage.cache_read + usage.cache_creation),
                        origin_class: None,
                    });
                }
                _ => {}
            }
        }
    }
    scan.status.rows = scan.rows.len() as u64;
    scan
}

/// Turn goal records into rows. Goals priced by the runtime are exact, by a
/// table estimated, and the rest unpriced (the same split `spend::aggregate`
/// reports).
pub fn goal_rows(goals: &[(String, GoalRun)]) -> Vec<CostRow> {
    goals
        .iter()
        .map(|(project, g)| CostRow {
            source: CostSourceKind::Goals,
            session: g.goal_run_id.to_string(),
            ts: Some(g.created_at),
            project: project.clone(),
            model: if g.agent_model.is_empty() {
                "unknown".to_string()
            } else {
                g.agent_model.clone()
            },
            usage: g.token_usage(),
            usd: g.cost_usd,
            kind: match g.cost_source {
                CostSource::Runtime => CostKind::Exact,
                CostSource::Table => CostKind::Estimated,
                CostSource::None => CostKind::Unpriced,
            },
            context_tokens: None,
            origin_class: Some(g.effective_origin_class().to_string()),
        })
        .collect()
}

// ── Grouping ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CostGroup {
    Day,
    Source,
    Model,
    Project,
    ContextBucket,
    OriginClass,
    /// One group per TA goal (transcript sessions show `n/a`).
    Goal,
}

impl CostGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            CostGroup::Day => "day",
            CostGroup::Source => "source",
            CostGroup::Model => "model",
            CostGroup::Project => "project",
            CostGroup::ContextBucket => "context_bucket",
            CostGroup::OriginClass => "origin_class",
            CostGroup::Goal => "goal",
        }
    }

    fn key(self, r: &CostRow) -> String {
        match self {
            CostGroup::Day => {
                r.ts.map(|t| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            }
            CostGroup::Source => r.source.as_str().to_string(),
            CostGroup::Model => {
                if r.model.is_empty() {
                    "unknown".to_string()
                } else {
                    r.model.clone()
                }
            }
            CostGroup::Project => r.project.clone(),
            CostGroup::ContextBucket => r
                .context_tokens
                .map(|t| context_bucket(t).to_string())
                .unwrap_or_else(|| "n/a".to_string()),
            CostGroup::OriginClass => r.origin_class.clone().unwrap_or_else(|| "n/a".to_string()),
            CostGroup::Goal if r.source == CostSourceKind::Goals => r.session.clone(),
            CostGroup::Goal => "n/a".to_string(),
        }
    }
}

/// Parse `--group-by day,model`. Empty means one group holding everything.
pub fn parse_cost_group_by(s: &str) -> Result<Vec<CostGroup>, String> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let g = match part {
            "day" => CostGroup::Day,
            "source" => CostGroup::Source,
            "model" => CostGroup::Model,
            "project" => CostGroup::Project,
            "context_bucket" => CostGroup::ContextBucket,
            "origin_class" => CostGroup::OriginClass,
            "goal" => CostGroup::Goal,
            other => {
                return Err(format!(
                    "unknown --group-by {other:?}: expected a comma-separated list of day, source, \
                     model, project, context_bucket, origin_class, goal (for example --group-by source,model). \
                     For one typed row per goal and session use --json --rows"
                ))
            }
        };
        if out.contains(&g) {
            return Err(format!(
                "--group-by lists {part:?} twice; list each name once"
            ));
        }
        out.push(g);
    }
    Ok(out)
}

/// Totals for one group.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CostTotals {
    /// Turns (transcripts) plus goals.
    pub rows: u64,
    pub sessions: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_tokens: u64,
    pub usd: f64,
    pub exact_rows: u64,
    pub estimated_rows: u64,
    pub unpriced_rows: u64,
}

impl CostTotals {
    fn add(&mut self, r: &CostRow) {
        self.rows += 1;
        self.input_tokens = self.input_tokens.saturating_add(r.usage.input);
        self.output_tokens = self.output_tokens.saturating_add(r.usage.output);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(r.usage.cache_read);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(r.usage.cache_creation);
        self.total_tokens = self.total_tokens.saturating_add(r.usage.total());
        self.usd += r.usd;
        match r.kind {
            CostKind::Exact => self.exact_rows += 1,
            CostKind::Estimated => self.estimated_rows += 1,
            CostKind::Unpriced => self.unpriced_rows += 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostGroupRow {
    pub group: BTreeMap<String, String>,
    #[serde(flatten)]
    pub totals: CostTotals,
}

/// Row filters shared by every source.
#[derive(Debug, Clone, Default)]
pub struct CostFilter {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Case-insensitive substring of the project name.
    pub project: Option<String>,
}

impl CostFilter {
    pub fn matches(&self, r: &CostRow) -> bool {
        if self.since.is_some() || self.until.is_some() {
            match r.ts {
                Some(t) => {
                    if self.since.is_some_and(|s| t < s) || self.until.is_some_and(|u| t >= u) {
                        return false;
                    }
                }
                // No timestamp cannot be placed in a window.
                None => return false,
            }
        }
        if let Some(p) = &self.project {
            if !r.project.to_lowercase().contains(&p.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

type SessionSet = HashSet<(CostSourceKind, String)>;

fn group_rows(rows: &[&CostRow], group_by: &[CostGroup]) -> (Vec<CostGroupRow>, CostTotals) {
    let mut map: BTreeMap<Vec<String>, (CostTotals, SessionSet)> = BTreeMap::new();
    let mut total = CostTotals::default();
    let mut all_sessions: HashSet<(CostSourceKind, String)> = HashSet::new();
    for r in rows {
        let key: Vec<String> = group_by.iter().map(|g| g.key(r)).collect();
        let entry = map.entry(key).or_default();
        entry.0.add(r);
        entry.1.insert((r.source, r.session.clone()));
        total.add(r);
        all_sessions.insert((r.source, r.session.clone()));
    }
    total.sessions = all_sessions.len() as u64;
    let names: Vec<String> = group_by.iter().map(|g| g.as_str().to_string()).collect();
    let groups = map
        .into_iter()
        .map(|(key, (mut totals, sessions))| {
            totals.sessions = sessions.len() as u64;
            CostGroupRow {
                group: names.iter().cloned().zip(key).collect(),
                totals,
            }
        })
        .collect();
    (groups, total)
}

/// The full report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostReport {
    pub group_by: Vec<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub project: Option<String>,
    pub sources: Vec<SourceStatus>,
    /// TA goals as `spend::aggregate` totals them (same filter, ungrouped).
    pub goals: Option<SpendReport>,
    pub groups: Vec<CostGroupRow>,
    pub totals: CostTotals,
    /// Turns, cache-read tokens and cost by prompt size, per source. Goals
    /// have no per-turn prompt size and are not in this table.
    pub context_buckets: Vec<CostGroupRow>,
    pub notes: Vec<String>,
}

/// Build the report from already-scanned sources.
///
/// `goals` pairs each goal with its project name; `goals_dir_found` is false
/// when the project has no goal directory (reported, not an error).
pub fn build_report(
    goals: Option<&[(String, GoalRun)]>,
    mut scans: Vec<Scan>,
    group_by: &[CostGroup],
    filter: &CostFilter,
) -> CostReport {
    let mut rows: Vec<CostRow> = Vec::new();
    let mut sources = Vec::new();
    let goals_report = goals.map(|g| {
        // The goal total comes from the shared aggregator so `ta cost report`
        // and GET /api/spend cannot disagree about goals. The project filter
        // is applied before it, like the row filter below.
        let kept: Vec<(String, GoalRun)> = g
            .iter()
            .filter(|(p, _)| {
                filter
                    .project
                    .as_ref()
                    .is_none_or(|f| p.to_lowercase().contains(&f.to_lowercase()))
            })
            .cloned()
            .collect();
        rows.extend(goal_rows(&kept));
        spend::aggregate(&kept, &[], filter.since, filter.until)
    });
    for scan in scans.drain(..) {
        sources.push(scan.status);
        rows.extend(scan.rows);
    }
    let kept: Vec<&CostRow> = rows.iter().filter(|r| filter.matches(r)).collect();
    let (groups, totals) = group_rows(&kept, group_by);
    let bucketed: Vec<&CostRow> = kept
        .iter()
        .copied()
        .filter(|r| r.context_tokens.is_some())
        .collect();
    let (mut context_buckets, _) =
        group_rows(&bucketed, &[CostGroup::Source, CostGroup::ContextBucket]);
    // BTreeMap order is alphabetical; present buckets small to large.
    let order = |g: &CostGroupRow| {
        let b = g.group.get("context_bucket").cloned().unwrap_or_default();
        (
            g.group.get("source").cloned().unwrap_or_default(),
            CONTEXT_BUCKETS.iter().position(|x| *x == b).unwrap_or(99),
        )
    };
    context_buckets.sort_by_key(order);
    CostReport {
        group_by: group_by.iter().map(|g| g.as_str().to_string()).collect(),
        since: filter.since,
        until: filter.until,
        project: filter.project.clone(),
        sources,
        goals: goals_report,
        groups,
        totals,
        context_buckets,
        notes: vec![
            "exact: the runtime or OMP reported the cost. estimated: priced from a price table \
             (.ta/pricing.toml or built in; the 1 hour cache-write rate is 2x input). unpriced: \
             no price for the model; tokens are counted and cost is 0."
                .to_string(),
            "Claude assistant rows are deduplicated by message.id (last row wins); <synthetic> \
             rows are skipped."
                .to_string(),
            "Context bucket = input + cache-read + cache-creation tokens of one turn. Goals are \
             many turns and have no bucket."
                .to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, lines: &[String]) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, lines.join("\n") + "\n").unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn claude_line(
        id: &str,
        model: &str,
        ts: &str,
        input: u64,
        output: u64,
        read: u64,
        write5: u64,
        write1: u64,
    ) -> String {
        serde_json::json!({
            "type": "assistant", "timestamp": ts, "sessionId": "s1",
            "cwd": "/Users/x/dev/proj/.ta/staging/abc",
            "message": {"id": id, "model": model, "role": "assistant", "usage": {
                "input_tokens": input, "output_tokens": output,
                "cache_read_input_tokens": read,
                "cache_creation_input_tokens": write5 + write1,
                "cache_creation": {"ephemeral_5m_input_tokens": write5,
                                   "ephemeral_1h_input_tokens": write1}}}
        })
        .to_string()
    }

    fn omp_lines() -> Vec<String> {
        let session = serde_json::json!({"type":"session","id":"om1","cwd":"/Users/x/dev/proj"});
        let msg = |ts: &str, model: &str, i: u64, o: u64, r: u64, w: u64, total: f64| {
            serde_json::json!({"type":"message","timestamp":ts,"message":{
                "role":"assistant","model":model,
                "usage":{"input":i,"output":o,"cacheRead":r,"cacheWrite":w,
                         "cost":{"total":total}}}})
            .to_string()
        };
        let user = serde_json::json!({"type":"message","timestamp":"2026-10-10T00:00:00Z",
            "message":{"role":"user","content":[]}});
        vec![
            session.to_string(),
            user.to_string(),
            msg(
                "2026-10-10T01:00:00Z",
                "claude-sonnet-5-5",
                4,
                285,
                0,
                24_778,
                0.10197,
            ),
            msg(
                "2026-10-10T02:00:00Z",
                "ollama/qwen2.5vl:7b",
                100,
                50,
                0,
                0,
                0.0,
            ),
        ]
    }

    #[test]
    fn buckets_have_the_documented_edges() {
        assert_eq!(context_bucket(0), "0-50k");
        assert_eq!(context_bucket(49_999), "0-50k");
        assert_eq!(context_bucket(50_000), "50-100k");
        assert_eq!(context_bucket(199_999), "100-200k");
        assert_eq!(context_bucket(200_000), "200-400k");
        assert_eq!(context_bucket(400_000), ">400k");
        assert_eq!(context_bucket(u64::MAX), ">400k");
    }

    #[test]
    fn repeated_message_ids_count_once_and_last_row_wins() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "-proj/a.jsonl",
            &[
                claude_line(
                    "m1",
                    "claude-sonnet-5-5",
                    "2026-10-10T01:00:00Z",
                    2,
                    5,
                    1000,
                    0,
                    500,
                ),
                // Same id, streamed again with the final usage.
                claude_line(
                    "m1",
                    "claude-sonnet-5-5",
                    "2026-10-10T01:00:01Z",
                    2,
                    90,
                    1000,
                    0,
                    500,
                ),
                claude_line(
                    "m2",
                    "claude-sonnet-5-5",
                    "2026-10-10T01:05:00Z",
                    3,
                    10,
                    2000,
                    0,
                    0,
                ),
                claude_line("m3", "<synthetic>", "2026-10-10T01:06:00Z", 0, 0, 0, 0, 0),
                "{ torn line".to_string(),
            ],
        );
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        assert_eq!(scan.rows.len(), 2);
        assert_eq!(scan.status.deduplicated, 1);
        assert_eq!(scan.status.synthetic_skipped, 1);
        assert_eq!(scan.status.unparsable_lines, 1);
        // Last row of m1 won: output 90, not 5 and not 95.
        let out: u64 = scan.rows.iter().map(|r| r.usage.output).sum();
        assert_eq!(out, 100);
        assert_eq!(scan.rows[0].project, "proj");
    }

    #[test]
    fn ids_are_deduplicated_across_files() {
        let dir = tempdir().unwrap();
        let l = claude_line(
            "m1",
            "claude-sonnet-5-5",
            "2026-10-10T01:00:00Z",
            1,
            1,
            0,
            0,
            0,
        );
        write(dir.path(), "p/a.jsonl", std::slice::from_ref(&l));
        write(dir.path(), "p/sub/b.jsonl", &[l]);
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        assert_eq!((scan.rows.len(), scan.status.deduplicated), (1, 1));
        assert_eq!(scan.status.files, 2);
    }

    #[test]
    fn claude_cost_uses_the_one_hour_write_rate_and_is_estimated() {
        let dir = tempdir().unwrap();
        // sonnet-5-5: input 3, output 15, read 0.30, write5 3.75, write1h 6.0 (per M).
        write(
            dir.path(),
            "p/a.jsonl",
            &[claude_line(
                "m1",
                "claude-sonnet-5-5",
                "2026-10-10T01:00:00Z",
                1_000_000,
                1_000_000,
                1_000_000,
                1_000_000,
                1_000_000,
            )],
        );
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        let r = &scan.rows[0];
        assert_eq!(r.kind, CostKind::Estimated);
        let want = 3.0 + 15.0 + 0.30 + 3.75 + 6.0;
        assert!((r.usd - want).abs() < 1e-9, "{} vs {want}", r.usd);
        assert_eq!(r.context_tokens, Some(1_000_000 + 1_000_000 + 2_000_000));
    }

    #[test]
    fn unknown_model_is_unpriced_never_invented() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "p/a.jsonl",
            &[claude_line(
                "m1",
                "mystery-model-9",
                "2026-10-10T01:00:00Z",
                10,
                10,
                0,
                0,
                0,
            )],
        );
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        assert_eq!(scan.rows[0].kind, CostKind::Unpriced);
        assert_eq!(scan.rows[0].usd, 0.0);
        assert_eq!(scan.rows[0].usage.total(), 20);
    }

    #[test]
    fn omp_uses_the_recorded_cost_as_exact_including_zero() {
        let dir = tempdir().unwrap();
        write(dir.path(), "-proj/x.jsonl", &omp_lines());
        let scan = scan_omp(dir.path(), &PriceTable::builtin());
        assert_eq!(scan.rows.len(), 2);
        assert!(scan.rows.iter().all(|r| r.kind == CostKind::Exact));
        assert!((scan.rows[0].usd - 0.10197).abs() < 1e-12);
        assert_eq!(scan.rows[1].usd, 0.0);
        assert_eq!(scan.rows[0].project, "proj");
        assert_eq!(scan.rows[0].session, "om1");
    }

    #[test]
    fn missing_source_directory_is_reported_not_an_error() {
        let dir = tempdir().unwrap();
        let scan = scan_claude(&dir.path().join("nope"), &PriceTable::builtin());
        assert!(!scan.status.found);
        assert!(scan.rows.is_empty());
    }

    #[test]
    fn report_totals_match_an_independent_sum_and_buckets_split_by_size() {
        let dir = tempdir().unwrap();
        let claude = dir.path().join("claude");
        let omp = dir.path().join("omp");
        write(
            &claude,
            "p/a.jsonl",
            &[
                // 60k context (50-100k), 600k context (>400k), 10k context (0-50k)
                claude_line(
                    "a",
                    "claude-sonnet-5-5",
                    "2026-10-10T01:00:00Z",
                    10,
                    100,
                    60_000,
                    0,
                    0,
                ),
                claude_line(
                    "b",
                    "claude-sonnet-5-5",
                    "2026-10-11T01:00:00Z",
                    10,
                    200,
                    300_000,
                    0,
                    300_000,
                ),
                claude_line(
                    "c",
                    "claude-sonnet-5-5",
                    "2026-10-11T02:00:00Z",
                    10,
                    300,
                    10_000,
                    0,
                    0,
                ),
            ],
        );
        write(&omp, "-proj/x.jsonl", &omp_lines());
        let table = PriceTable::builtin();
        let scans = vec![scan_claude(&claude, &table), scan_omp(&omp, &table)];
        let want_usd: f64 = scans
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| r.usd)
            .sum();
        let want_tokens: u64 = scans
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| r.usage.total())
            .sum();
        let rep = build_report(None, scans, &[CostGroup::Source], &CostFilter::default());
        assert!((rep.totals.usd - want_usd).abs() < 1e-9);
        assert_eq!(rep.totals.total_tokens, want_tokens);
        assert_eq!(rep.totals.rows, 5);
        let by_source: f64 = rep.groups.iter().map(|g| g.totals.usd).sum();
        assert!((by_source - want_usd).abs() < 1e-9);

        let bucket = |src: &str, b: &str| {
            rep.context_buckets
                .iter()
                .find(|g| g.group["source"] == src && g.group["context_bucket"] == b)
                .map(|g| g.totals.clone())
        };
        let b = bucket("claude_code", "50-100k").unwrap();
        assert_eq!((b.rows, b.cache_read_tokens), (1, 60_000));
        let b = bucket("claude_code", ">400k").unwrap();
        assert_eq!((b.rows, b.cache_read_tokens), (1, 300_000));
        assert!(bucket("claude_code", "100-200k").is_none());
        // Buckets are listed small to large within a source.
        let claude_order: Vec<&str> = rep
            .context_buckets
            .iter()
            .filter(|g| g.group["source"] == "claude_code")
            .map(|g| g.group["context_bucket"].as_str())
            .collect();
        assert_eq!(claude_order, vec!["0-50k", "50-100k", ">400k"]);
    }

    #[test]
    fn day_grouping_and_filters_apply() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "p/a.jsonl",
            &[
                claude_line(
                    "a",
                    "claude-sonnet-5-5",
                    "2026-10-10T23:59:00Z",
                    1,
                    1,
                    0,
                    0,
                    0,
                ),
                claude_line(
                    "b",
                    "claude-sonnet-5-5",
                    "2026-10-11T00:00:00Z",
                    1,
                    1,
                    0,
                    0,
                    0,
                ),
                claude_line(
                    "c",
                    "claude-sonnet-5-5",
                    "2026-10-12T00:00:00Z",
                    1,
                    1,
                    0,
                    0,
                    0,
                ),
            ],
        );
        let scans = || vec![scan_claude(dir.path(), &PriceTable::builtin())];
        let rep = build_report(None, scans(), &[CostGroup::Day], &CostFilter::default());
        let days: Vec<_> = rep.groups.iter().map(|g| g.group["day"].clone()).collect();
        assert_eq!(days, ["2026-10-10", "2026-10-11", "2026-10-12"]);

        let f = CostFilter {
            since: Some(spend::parse_time_bound("2026-10-11").unwrap()),
            until: Some(spend::parse_time_bound("2026-10-12").unwrap()),
            project: None,
        };
        let rep = build_report(None, scans(), &[], &f);
        assert_eq!(rep.totals.rows, 1);

        let f = CostFilter {
            project: Some("PROJ".into()),
            ..CostFilter::default()
        };
        assert_eq!(build_report(None, scans(), &[], &f).totals.rows, 3);
        let f = CostFilter {
            project: Some("other".into()),
            ..CostFilter::default()
        };
        assert_eq!(build_report(None, scans(), &[], &f).totals.rows, 0);
    }

    #[test]
    fn goal_rows_agree_with_the_shared_aggregator() {
        let mut g = GoalRun::new(
            "t",
            "o",
            "claude-code",
            PathBuf::from("/tmp/w"),
            PathBuf::from("/tmp/s"),
        );
        g.agent_model = "claude-sonnet-5-5".into();
        g.input_tokens = 10;
        g.output_tokens = 20;
        g.cache_read_input_tokens = 30;
        g.cache_creation_input_tokens = 40;
        g.cost_usd = 1.25;
        g.cost_source = CostSource::Runtime;
        let goals = vec![("proj".to_string(), g)];
        let rep = build_report(
            Some(&goals),
            vec![],
            &[CostGroup::OriginClass],
            &CostFilter::default(),
        );
        let agg = rep.goals.as_ref().unwrap();
        assert_eq!(rep.totals.total_tokens, agg.totals.total_tokens);
        assert!((rep.totals.usd - agg.totals.usd).abs() < 1e-12);
        assert_eq!(rep.totals.exact_rows, agg.totals.exact_cost_goals);
        // Goals are not in the context-bucket table.
        assert!(rep.context_buckets.is_empty());
    }

    #[test]
    fn group_by_parsing_rejects_unknown_and_repeated() {
        assert_eq!(
            parse_cost_group_by("day, model").unwrap(),
            vec![CostGroup::Day, CostGroup::Model]
        );
        assert!(parse_cost_group_by("").unwrap().is_empty());
        assert!(parse_cost_group_by("hour")
            .unwrap_err()
            .contains("expected"));
        assert!(parse_cost_group_by("day,day").is_err());
    }

    #[test]
    fn project_names_unwrap_staging_and_worktrees() {
        assert_eq!(project_from_cwd("/a/b/proj"), "proj");
        assert_eq!(project_from_cwd("/a/b/proj/.ta/staging/uuid"), "proj");
        assert_eq!(
            project_from_cwd("/a/b/proj/.claude/worktrees/x/apps/y"),
            "proj"
        );
        assert_eq!(project_from_cwd("C:\\a\\proj"), "proj");
        assert_eq!(project_from_cwd(""), "unknown");
    }
}
