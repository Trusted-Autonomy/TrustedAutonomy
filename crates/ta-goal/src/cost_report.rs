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

use crate::cost_detail::{DetailTotals, Dialect, SessionShape, ToolTracker, TurnDetail};
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
    /// Coarse family of the session's first user prompt (a fixed label for a
    /// known job, else `other:<hash>`). Never the prompt text. Transcripts only.
    pub family: Option<String>,
    /// How the session was started (`sdk-cli` headless, `cli` interactive).
    /// Claude Code transcripts only.
    pub entrypoint: Option<String>,
    /// Content-block measurements of the turn. Transcripts only.
    pub detail: Option<TurnDetail>,
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

/// The prompt text of a user message: a plain string, or the first text block.
/// Used only to classify the session; it is never stored.
fn prompt_text(content: &Value) -> Option<&str> {
    let text = match content {
        Value::String(s) => Some(s.as_str()),
        Value::Array(blocks) => blocks
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|b| b.get("text"))
            .and_then(Value::as_str),
        _ => None,
    };
    text.filter(|t| !t.trim().is_empty())
}

/// Identity of a content block of message `mid`, so a block repeated in a later
/// record or in a copied (resumed) transcript is counted once. `None` for a
/// tool call with no id, which cannot be told apart and is always counted.
fn block_key(mid: &str, block: &Value) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let ty = block.get("type").and_then(Value::as_str).unwrap_or("");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    mid.hash(&mut h);
    ty.hash(&mut h);
    if ty == "tool_use" || ty == "toolCall" {
        let id = block
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| !i.is_empty())?;
        id.hash(&mut h);
    } else {
        let text = block
            .get("text")
            .or_else(|| block.get("thinking"))
            .and_then(Value::as_str)
            .unwrap_or("");
        text.len().hash(&mut h);
        text.as_bytes()[..text.len().min(128)].hash(&mut h);
        block
            .get("signature")
            .or_else(|| block.get("thinkingSignature"))
            .and_then(Value::as_str)
            .hash(&mut h);
    }
    Some(h.finish())
}

fn chars(s: &str) -> u64 {
    s.chars().count() as u64
}

/// Add what the content blocks of one assistant message show to its turn. A
/// block already counted (same [`block_key`]) adds nothing; `seen` holds the
/// counted keys.
fn absorb_blocks(
    blocks: &[Value],
    mid: &str,
    seen: &mut HashSet<u64>,
    tracker: &mut ToolTracker,
    session: &str,
    row: usize,
    detail: &mut TurnDetail,
) {
    for block in blocks {
        if let Some(key) = block_key(mid, block) {
            if !seen.insert(key) {
                continue;
            }
        }
        match block.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                let t = block.get("thinking").and_then(Value::as_str).unwrap_or("");
                if t.is_empty() {
                    detail.thinking_hidden_blocks += 1;
                } else {
                    detail.thinking_chars += chars(t);
                }
            }
            Some("redacted_thinking") => detail.thinking_hidden_blocks += 1,
            Some("text") => {
                detail.text_chars += chars(block.get("text").and_then(Value::as_str).unwrap_or(""));
            }
            // Claude Code: tool_use/input. OMP: toolCall/arguments.
            Some(ty @ ("tool_use" | "toolCall")) => {
                let input = block
                    .get(if ty == "tool_use" {
                        "input"
                    } else {
                        "arguments"
                    })
                    .unwrap_or(&Value::Null);
                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                detail.merge(&tracker.call(session, row, id, name, input));
            }
            _ => {}
        }
    }
}

/// Apply a tool result to the turn that made the call.
fn apply_result(
    rows: &mut [CostRow],
    tracker: &mut ToolTracker,
    id: &str,
    is_error: bool,
    existing: Option<bool>,
) {
    if let Some((row, delta)) = tracker.result(id, is_error, existing) {
        rows[row]
            .detail
            .get_or_insert_with(TurnDetail::default)
            .merge(&delta);
    }
}

/// A Claude `user` record: the session's first prompt (for its family) and the
/// results of earlier tool calls.
fn claude_user_record(
    v: &Value,
    family: &mut Option<String>,
    tracker: &mut ToolTracker,
    rows: &mut [CostRow],
) {
    let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    if family.is_none() && v.get("isMeta").and_then(Value::as_bool) != Some(true) {
        if let Some(text) = prompt_text(content) {
            *family = Some(crate::cost_detail::classify_prompt(text));
        }
    }
    let Value::Array(blocks) = content else {
        return;
    };
    // `toolUseResult.type` is `create` or `update` for a Write.
    let existing = match v
        .get("toolUseResult")
        .and_then(|r| r.get("type"))
        .and_then(Value::as_str)
    {
        Some("update") => Some(true),
        Some("create") => Some(false),
        _ => None,
    };
    for b in blocks {
        if b.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(id) = b.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let is_error = b.get("is_error").and_then(Value::as_bool) == Some(true);
        apply_result(rows, tracker, id, is_error, existing);
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
    let mut seen_blocks: HashSet<u64> = HashSet::new();
    let mut tracker = ToolTracker::new(Dialect::Claude);
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
        let mut file_family: Option<String> = None;
        let mut file_entrypoint: Option<String> = None;
        let mut file_rows: Vec<usize> = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                scan.status.unparsable_lines += 1;
                continue;
            };
            if file_entrypoint.is_none() {
                file_entrypoint = v
                    .get("entrypoint")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            match v.get("type").and_then(Value::as_str) {
                Some("assistant") => {}
                Some("user") => {
                    claude_user_record(&v, &mut file_family, &mut tracker, &mut scan.rows);
                    continue;
                }
                _ => continue,
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
            let session = v
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let mut row = CostRow {
                source: CostSourceKind::Claude,
                session: session.clone(),
                ts: v.get("timestamp").and_then(parse_ts),
                project,
                model: model.to_string(),
                usage,
                usd,
                kind,
                context_tokens: Some(usage.input + usage.cache_read + usage.cache_creation),
                origin_class: None,
                family: file_family.clone(),
                entrypoint: file_entrypoint.clone(),
                detail: None,
            };
            let mid = msg.get("id").and_then(Value::as_str);
            let idx = match mid.and_then(|id| by_id.get(id).copied()) {
                Some(i) => {
                    // The newest row's usage wins; what earlier records of this
                    // message showed (blocks, tool results) is kept.
                    row.detail = scan.rows[i].detail.take();
                    row.family = row.family.or_else(|| scan.rows[i].family.take());
                    scan.rows[i] = row;
                    scan.status.deduplicated += 1;
                    i
                }
                None => {
                    let i = scan.rows.len();
                    if let Some(id) = mid {
                        by_id.insert(id.to_string(), i);
                    }
                    scan.rows.push(row);
                    i
                }
            };
            file_rows.push(idx);
            let mut rec = TurnDetail {
                // Claude Code reports the thinking share of the output exactly.
                thinking_reported: u
                    .get("output_tokens_details")
                    .and_then(|d| d.get("thinking_tokens"))
                    .and_then(Value::as_u64),
                ..TurnDetail::default()
            };
            if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
                absorb_blocks(
                    blocks,
                    mid.unwrap_or(""),
                    &mut seen_blocks,
                    &mut tracker,
                    &session,
                    idx,
                    &mut rec,
                );
            }
            scan.rows[idx]
                .detail
                .get_or_insert_with(TurnDetail::default)
                .merge(&rec);
        }
        // A turn can precede the record that names the family or entrypoint.
        for i in file_rows {
            let r = &mut scan.rows[i];
            if r.family.is_none() {
                r.family = file_family.clone();
            }
            if r.entrypoint.is_none() {
                r.entrypoint = file_entrypoint.clone();
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
    let mut seen_blocks: HashSet<u64> = HashSet::new();
    let mut tracker = ToolTracker::new(Dialect::Omp);
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
        let mut file_family: Option<String> = None;
        let first_row = scan.rows.len();
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
                    match msg.get("role").and_then(Value::as_str) {
                        Some("user") => {
                            if file_family.is_none() {
                                if let Some(t) = msg.get("content").and_then(prompt_text) {
                                    file_family = Some(crate::cost_detail::classify_prompt(t));
                                }
                            }
                            continue;
                        }
                        Some("toolResult") => {
                            let id = msg.get("toolCallId").and_then(Value::as_str).unwrap_or("");
                            let is_error =
                                msg.get("isError").and_then(Value::as_bool) == Some(true);
                            apply_result(&mut scan.rows, &mut tracker, id, is_error, None);
                            continue;
                        }
                        Some("assistant") => {}
                        _ => continue,
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
                    let idx = scan.rows.len();
                    // OMP reports no thinking/text/tool-argument split.
                    let mut detail = TurnDetail::default();
                    if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
                        absorb_blocks(
                            blocks,
                            "",
                            &mut seen_blocks,
                            &mut tracker,
                            &session,
                            idx,
                            &mut detail,
                        );
                    }
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
                        family: file_family.clone(),
                        entrypoint: None,
                        detail: Some(detail),
                    });
                }
                _ => {}
            }
        }
        if let Some(f) = &file_family {
            for r in &mut scan.rows[first_row..] {
                r.family.get_or_insert_with(|| f.clone());
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
            family: None,
            entrypoint: None,
            detail: None,
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
    /// The coarse family of the session's first user prompt (`n/a` for goals
    /// and sources with no prompt). See `ta cost report --detail`.
    SessionFamily,
    /// How the session was started: `sdk-cli` (headless) or `cli`.
    Entrypoint,
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
            CostGroup::SessionFamily => "session_family",
            CostGroup::Entrypoint => "entrypoint",
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
            CostGroup::SessionFamily => r.family.clone().unwrap_or_else(|| "n/a".to_string()),
            CostGroup::Entrypoint => r.entrypoint.clone().unwrap_or_else(|| "n/a".to_string()),
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
            "session_family" => CostGroup::SessionFamily,
            "entrypoint" => CostGroup::Entrypoint,
            other => {
                return Err(format!(
                    "unknown --group-by {other:?}: expected a comma-separated list of day, source, \
                     model, project, context_bucket, origin_class, goal, session_family, entrypoint \
                     (for example --group-by source,model). \
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
    /// Output split, edit and write measurements of the transcript turns in
    /// the group. Absent for a group of goals only. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<DetailTotals>,
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
        if let Some(d) = &r.detail {
            self.detail
                .get_or_insert_with(DetailTotals::default)
                .add(r.usage.output, d);
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
    /// Plan and budget usage (`plan_usage_pct`, `budget_used_pct`, meters).
    /// Additive: absent in older reports and filled in by the CLI.
    #[serde(default)]
    pub usage: Option<crate::usage::UsageSummary>,
    /// JSON schema version of this report. 1: the original report (the field
    /// is absent). 2: adds `detail` on every group and total, `session_shape`
    /// and `detail_notes`. Fields are only ever added within a version.
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    /// Turns-per-session histogram, session-start overhead and short-session
    /// families for the transcript sessions that started in the window.
    #[serde(default)]
    pub session_shape: Option<SessionShape>,
    /// How to read the `detail` and `session_shape` numbers.
    #[serde(default)]
    pub detail_notes: Vec<String>,
}

/// The version of a report written before `schema_version` existed.
fn schema_v1() -> u32 {
    1
}

/// The current `CostReport.schema_version`.
pub const COST_REPORT_SCHEMA_VERSION: u32 = 2;

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
    // The per-bucket table is about prompt size; the detail lives in `groups`.
    for g in &mut context_buckets {
        g.totals.detail = None;
    }
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
        usage: None,
        schema_version: COST_REPORT_SCHEMA_VERSION,
        session_shape: Some(crate::cost_detail::session_shape(&rows, |r| {
            filter.matches(r)
        })),
        detail_notes: detail_notes(),
    }
}

/// How to read the `detail` and `session_shape` numbers (JSON `detail_notes`,
/// printed by `--detail`).
fn detail_notes() -> Vec<String> {
    use crate::cost_detail::{
        CHARS_PER_TOKEN, LARGE_OVERWRITE_BYTES, MAX_FAMILIES, SHORT_SESSION_MAX_TURNS,
    };
    vec![
        format!(
            "Output split: thinking is EXACT (`thinking_tokens_reported`) where the runtime \
             reports it (Claude Code: usage.output_tokens_details.thinking_tokens). Everything \
             ending `_est` is an ESTIMATE from block length: assistant text and tool-call \
             arguments share the non-thinking output in proportion to their characters; where \
             nothing reports thinking (OMP) all three share the reported output total by \
             characters; where thinking is hidden and unreported it is the output total minus \
             the estimated text and argument tokens at {CHARS_PER_TOKEN} characters per token. \
             The parts always sum to the reported output tokens of the turns counted."
        ),
        "Edits: a failed edit is an edit tool call whose result was an error; a retry is an edit \
         call to a file whose previous edit in the same session failed. Tool names: Claude Code \
         Edit/MultiEdit/NotebookEdit and Write; OMP edit and write. Calls to non-file targets \
         (such as xd:// devices through `write`) are not counted as writes."
            .to_string(),
        format!(
            "Whole-file overwrites: a successful write over a file that already existed, large \
             at {LARGE_OVERWRITE_BYTES} bytes or more. Claude Code says so itself (Write result \
             type `update`). OMP does not, so a write counts as an overwrite only when the \
             session had already read, edited or written that path: for OMP it is a lower bound."
        ),
        format!(
            "Session shape: a turn is one deduplicated API call. A session is counted when its \
             first turn is inside the window, with all its turns. Short = at most \
             {SHORT_SESSION_MAX_TURNS} turns. First-turn cache write is the session-start \
             overhead. Family is a fixed label for a known automated job, else `other:` and a \
             hash of the first user prompt's opening; prompt text is never stored. The \
             {MAX_FAMILIES} costliest families are listed, the rest fold into `(remaining)`."
        ),
    ]
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

    // ── Instrumentation: output split, edit behaviour, session shape ────────

    use serde_json::json;

    /// One Claude assistant record. A real transcript writes one record per
    /// content block, each repeating the message's usage.
    fn assistant_rec(
        session: &str,
        ts: &str,
        mid: &str,
        blocks: Value,
        out: u64,
        thinking: Option<u64>,
        cache_write: u64,
    ) -> String {
        let mut usage = json!({"input_tokens": 1, "output_tokens": out,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": cache_write});
        if let Some(t) = thinking {
            usage["output_tokens_details"] = json!({"thinking_tokens": t});
        }
        json!({"type": "assistant", "timestamp": ts, "sessionId": session,
            "cwd": "/Users/x/dev/proj", "entrypoint": "sdk-cli",
            "message": {"id": mid, "model": "claude-sonnet-5-5", "role": "assistant",
                        "content": blocks, "usage": usage}})
        .to_string()
    }

    fn user_rec(session: &str, text: &str) -> String {
        json!({"type": "user", "sessionId": session, "entrypoint": "sdk-cli",
            "message": {"role": "user", "content": text}})
        .to_string()
    }

    fn result_rec(session: &str, id: &str, is_error: bool, kind: Option<&str>) -> String {
        let mut v = json!({"type": "user", "sessionId": session, "message": {"role": "user",
            "content": [{"type": "tool_result", "tool_use_id": id, "is_error": is_error,
                         "content": "x"}]}});
        if let Some(k) = kind {
            v["toolUseResult"] = json!({"type": k});
        }
        v.to_string()
    }

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({"type": "tool_use", "id": id, "name": name, "input": input})
    }

    fn edit_input(path: &str) -> Value {
        json!({"file_path": path, "old_string": "a", "new_string": "b"})
    }

    fn one_report(scan: Scan, group_by: &[CostGroup], filter: &CostFilter) -> CostReport {
        build_report(None, vec![scan], group_by, filter)
    }

    #[test]
    fn claude_thinking_is_exact_and_text_and_tool_args_are_estimates_counted_once() {
        let dir = tempdir().unwrap();
        let text = "x".repeat(100);
        let recs = vec![
            // Thinking hidden by the provider (empty text) but reported in usage.
            assistant_rec(
                "s",
                "2026-10-15T01:00:00Z",
                "m1",
                json!([{"type": "thinking", "thinking": "", "signature": "sig"}]),
                200,
                Some(100),
                0,
            ),
            assistant_rec(
                "s",
                "2026-10-15T01:00:01Z",
                "m1",
                json!([{"type": "text", "text": text}]),
                200,
                Some(100),
                0,
            ),
            assistant_rec(
                "s",
                "2026-10-15T01:00:02Z",
                "m1",
                json!([tool_use("tu1", "Bash", json!({"command": "echo hi"}))]),
                200,
                Some(100),
                0,
            ),
        ];
        write(dir.path(), "p/a.jsonl", &recs);
        // A resumed session copies the history into a second file.
        write(dir.path(), "p/b.jsonl", &recs);
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        assert_eq!(scan.rows.len(), 1, "one message, one turn");
        let rep = one_report(scan, &[CostGroup::Source], &CostFilter::default());
        let d = rep.totals.detail.expect("transcript turns carry detail");
        let o = &d.output_split;
        assert_eq!(d.output_tokens, 200, "the copy must not double the output");
        assert_eq!(o.thinking_tokens_reported, 100);
        assert_eq!(
            o.thinking_tokens_est, 0,
            "reported thinking is never re-estimated"
        );
        // 100 text chars vs `{"command":"echo hi"}` (20 chars) share the other 100.
        assert_eq!((o.text_tokens_est, o.tool_arg_tokens_est), (83, 17));
        assert_eq!(o.unattributed_tokens, 0);
        assert_eq!(o.turns_thinking_reported, 1);
        assert_eq!(d.edits.tool_calls, 1, "the copied tool call counts once");
    }

    #[test]
    fn omp_reports_no_split_so_every_part_is_an_estimate_summing_to_the_total() {
        let dir = tempdir().unwrap();
        let usage = json!({"input": 1, "output": 400, "cacheRead": 0, "cacheWrite": 0,
                           "cost": {"total": 0.01}});
        let lines = vec![
            json!({"type": "session", "id": "o1", "cwd": "/Users/x/dev/proj"}).to_string(),
            json!({"type": "message", "timestamp": "2026-10-15T01:00:00Z", "message": {
                "role": "user", "content": [{"type": "text", "text": "hello"}]}})
            .to_string(),
            json!({"type": "message", "timestamp": "2026-10-15T01:00:05Z", "message": {
                "role": "assistant", "model": "claude-sonnet-5-5", "usage": usage,
                "content": [
                    {"type": "thinking", "thinking": "t".repeat(100)},
                    {"type": "text", "text": "x".repeat(100)},
                    {"type": "toolCall", "id": "c1", "name": "bash",
                     "arguments": {"command": "ls"}}]}})
            .to_string(),
        ];
        write(dir.path(), "p/o.jsonl", &lines);
        let scan = scan_omp(dir.path(), &PriceTable::builtin());
        let rep = one_report(scan, &[CostGroup::Source], &CostFilter::default());
        let o = rep.totals.detail.unwrap().output_split;
        // Visible lengths 100 : 100 : 16 (`{"command":"ls"}`) over 400 tokens.
        assert_eq!(
            o.thinking_tokens_reported, 0,
            "OMP reports no thinking split"
        );
        assert_eq!(
            (
                o.thinking_tokens_est,
                o.text_tokens_est,
                o.tool_arg_tokens_est
            ),
            (185, 185, 30)
        );
        assert_eq!(o.turns_estimated_from_chars, 1);
    }

    #[test]
    fn claude_edit_failures_retries_and_whole_file_overwrites_are_detected() {
        let dir = tempdir().unwrap();
        let t = |n: u32| format!("2026-10-15T01:00:{n:02}Z");
        let call = |n: u32, mid: &str, block: Value| {
            assistant_rec("s", &t(n), mid, json!([block]), 10, Some(0), 0)
        };
        let recs = vec![
            user_rec("s", "do the edits"),
            call(1, "m1", tool_use("e1", "Edit", edit_input("/p/a.rs"))),
            result_rec("s", "e1", true, None),
            // Same file again after the failure: a retry.
            call(2, "m2", tool_use("e2", "Edit", edit_input("/p/a.rs"))),
            result_rec("s", "e2", false, None),
            // A different file: neither a failure nor a retry.
            call(3, "m3", tool_use("e3", "Edit", edit_input("/p/b.rs"))),
            result_rec("s", "e3", false, None),
            // A large overwrite, a create, a failed write and a small overwrite.
            call(
                4,
                "m4",
                tool_use(
                    "w1",
                    "Write",
                    json!({"file_path": "/p/a.rs", "content": "x".repeat(10_000)}),
                ),
            ),
            result_rec("s", "w1", false, Some("update")),
            call(
                5,
                "m5",
                tool_use(
                    "w2",
                    "Write",
                    json!({"file_path": "/p/new.rs", "content": "hi"}),
                ),
            ),
            result_rec("s", "w2", false, Some("create")),
            call(
                6,
                "m6",
                tool_use(
                    "w3",
                    "Write",
                    json!({"file_path": "/p/c.rs", "content": "x"}),
                ),
            ),
            result_rec("s", "w3", true, None),
            call(
                7,
                "m7",
                tool_use(
                    "w4",
                    "Write",
                    json!({"file_path": "/p/s.rs", "content": "y".repeat(100)}),
                ),
            ),
            result_rec("s", "w4", false, Some("update")),
        ];
        write(dir.path(), "p/a.jsonl", &recs);
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        let rep = one_report(scan, &[CostGroup::Source], &CostFilter::default());
        let e = rep.totals.detail.unwrap().edits;
        assert_eq!(e.tool_calls, 7);
        assert_eq!((e.edit_calls, e.edit_failures, e.edit_retries), (3, 1, 1));
        assert!(e.failed_edit_arg_tokens_est > 0, "{e:?}");
        assert_eq!((e.file_writes, e.write_failures), (3, 1));
        assert_eq!(
            e.whole_file_overwrites, 2,
            "w1 and w4 overwrote; w2 created"
        );
        assert_eq!(e.overwrite_bytes, 10_100);
        assert_eq!((e.large_overwrites, e.large_overwrite_bytes), (1, 10_000));
    }

    #[test]
    fn omp_retries_follow_the_file_across_relative_and_absolute_paths() {
        let dir = tempdir().unwrap();
        let usage = json!({"input": 1, "output": 10, "cacheRead": 0, "cacheWrite": 0,
                           "cost": {"total": 0.01}});
        let turn = |calls: Vec<Value>| {
            json!({"type": "message", "timestamp": "2026-10-15T01:00:00Z", "message": {
                "role": "assistant", "model": "claude-sonnet-5-5", "usage": usage,
                "content": calls}})
            .to_string()
        };
        let call = |id: &str, name: &str, args: Value| json!({"type": "toolCall", "id": id, "name": name, "arguments": args});
        let res = |id: &str, is_error: bool| {
            json!({"type": "message", "message": {"role": "toolResult", "toolCallId": id,
                "isError": is_error, "content": [{"type": "text", "text": "r"}]}})
            .to_string()
        };
        let lines = vec![
            json!({"type": "session", "id": "o1", "cwd": "/w"}).to_string(),
            turn(vec![
                call("r1", "read", json!({"path": "src/a.rs:1-20"})),
                call(
                    "e1",
                    "edit",
                    json!({"input": "[src/a.rs#AAAA]\nPUT 1:\n+x"}),
                ),
            ]),
            res("r1", false),
            res("e1", true),
            turn(vec![call(
                "e2",
                "edit",
                json!({"input": "[/w/src/a.rs#BBBB]\nPUT 1:\n+y"}),
            )]),
            res("e2", false),
            turn(vec![
                call(
                    "w1",
                    "write",
                    json!({"path": "/w/src/a.rs", "content": "zzzz"}),
                ),
                call(
                    "w2",
                    "write",
                    json!({"path": "/w/src/new.rs", "content": "zz"}),
                ),
                // `write` also carries xd:// device calls: not a file write.
                call(
                    "w3",
                    "write",
                    json!({"path": "xd://ast_edit", "content": "{}"}),
                ),
            ]),
            res("w1", false),
            res("w2", false),
            res("w3", false),
        ];
        write(dir.path(), "p/o.jsonl", &lines);
        let scan = scan_omp(dir.path(), &PriceTable::builtin());
        let rep = one_report(scan, &[CostGroup::Source], &CostFilter::default());
        let e = rep.totals.detail.unwrap().edits;
        assert_eq!(e.tool_calls, 6);
        assert_eq!((e.edit_calls, e.edit_failures, e.edit_retries), (2, 1, 1));
        assert_eq!(e.file_writes, 2, "the xd:// call is not a file write");
        assert_eq!(
            e.whole_file_overwrites, 1,
            "a.rs was read and edited earlier; new.rs was never seen"
        );
    }

    /// Three sessions after `since` and one before it.
    fn shape_fixture(dir: &Path) {
        let turns = |session: &str, day: &str, prompt: &str, n: u32, first_cw: u64| {
            let mut v = vec![user_rec(session, prompt)];
            for i in 0..n {
                let cw = if i == 0 { first_cw } else { 10 };
                v.push(assistant_rec(
                    session,
                    &format!("{day}T01:00:{i:02}Z"),
                    &format!("{session}-{i}"),
                    json!([]),
                    5,
                    None,
                    cw,
                ));
            }
            v
        };
        let title = "Extract the user's actual work request from hunter2-secret-token";
        write(
            dir,
            "p/a.jsonl",
            &turns("a", "2026-10-15", title, 3, 17_000),
        );
        write(
            dir,
            "p/b.jsonl",
            &turns("b", "2026-10-16", title, 2, 15_000),
        );
        write(
            dir,
            "p/c.jsonl",
            &turns(
                "c",
                "2026-10-16",
                "Plan the migration of table users",
                8,
                5_000,
            ),
        );
        write(
            dir,
            "p/d.jsonl",
            &turns("d", "2026-10-10", title, 1, 99_000),
        );
    }

    fn since_oct_14() -> CostFilter {
        CostFilter {
            since: Some(parse_ts(&json!("2026-10-14T00:00:00Z")).unwrap()),
            ..CostFilter::default()
        }
    }

    #[test]
    fn session_shape_has_a_turns_histogram_first_turn_overhead_and_families() {
        let dir = tempdir().unwrap();
        shape_fixture(dir.path());
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        let rep = one_report(scan, &[CostGroup::Source], &since_oct_14());
        let shape = rep.session_shape.as_ref().unwrap();
        assert_eq!(shape.sources.len(), 1);
        let src = &shape.sources[0];
        let hist: BTreeMap<&str, u64> = src
            .turns_histogram
            .iter()
            .map(|b| (b.turns.as_str(), b.sessions))
            .collect();
        assert_eq!(hist["1"], 0, "session d started before the window");
        assert_eq!((hist["2"], hist["3"], hist["6-10"]), (1, 1, 1));
        assert!(
            src.turns_histogram
                .iter()
                .filter(|b| b.sessions == 0)
                .all(|b| b.usd.is_sign_positive()),
            "an empty bucket must print 0.00, not -0.00"
        );
        let class = |name: &str| src.classes.iter().find(|c| c.class == name).unwrap();
        assert_eq!(class("all").sessions, 3);
        assert_eq!((class("short").sessions, class("long").sessions), (2, 1));
        // Short sessions started with 17,000 and 15,000 cache-write tokens.
        let f = &class("short").first_turn;
        assert_eq!(f.cache_write_tokens_total, 32_000);
        assert_eq!(f.cache_write_tokens_mean, 16_000);
        assert_eq!(f.cache_write_tokens_median, 15_000);
        assert_eq!(
            (f.cache_write_tokens_p90, f.cache_write_tokens_max),
            (17_000, 17_000)
        );
        // Only the short sessions are attributed, by label, with their entrypoint.
        assert_eq!(shape.families.len(), 1);
        let fam = &shape.families[0];
        assert_eq!(
            (
                fam.family.as_str(),
                fam.entrypoint.as_str(),
                fam.sessions,
                fam.turns
            ),
            ("title-extraction", "sdk-cli", 2, 5)
        );
        // The prompt text must not reach any output.
        let json = serde_json::to_string(&rep).unwrap();
        assert!(
            !json.contains("hunter2") && !json.contains("secret"),
            "{json}"
        );
    }

    #[test]
    fn session_family_and_entrypoint_are_group_by_values() {
        let dir = tempdir().unwrap();
        shape_fixture(dir.path());
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        let by = parse_cost_group_by("session_family,entrypoint").unwrap();
        let rep = one_report(scan, &by, &since_oct_14());
        let mut keys: Vec<(String, String, u64)> = rep
            .groups
            .iter()
            .map(|g| {
                (
                    g.group["session_family"].clone(),
                    g.group["entrypoint"].clone(),
                    g.totals.sessions,
                )
            })
            .collect();
        keys.sort();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1], ("title-extraction".into(), "sdk-cli".into(), 2));
        assert!(keys[0].0.starts_with("other:"), "{keys:?}");
    }

    #[test]
    fn reports_carry_a_schema_version_and_old_reports_read_as_version_1() {
        let dir = tempdir().unwrap();
        shape_fixture(dir.path());
        let scan = scan_claude(dir.path(), &PriceTable::builtin());
        let rep = one_report(scan, &[CostGroup::Source], &CostFilter::default());
        assert_eq!(rep.schema_version, COST_REPORT_SCHEMA_VERSION);
        let mut v = serde_json::to_value(&rep).unwrap();
        assert_eq!(v["schema_version"], 2);
        for k in ["schema_version", "session_shape", "detail_notes"] {
            v.as_object_mut().unwrap().remove(k);
        }
        for g in v["groups"].as_array_mut().unwrap() {
            g.as_object_mut().unwrap().remove("detail");
        }
        v["totals"].as_object_mut().unwrap().remove("detail");
        let old: CostReport = serde_json::from_value(v).unwrap();
        assert_eq!(old.schema_version, 1);
        assert!(old.session_shape.is_none() && old.totals.detail.is_none());
    }
}
