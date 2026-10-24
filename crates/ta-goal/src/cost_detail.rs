//! Output-token, edit and session-shape measurements for `ta cost report`.
//!
//! The base report says how many tokens were spent. This module says what the
//! output tokens were spent on and how the sessions were shaped, so a token
//! optimiser (terse prompts, edit formats, thinking budgets, fewer short
//! headless sessions) can be chosen from data instead of guessed.
//!
//! Three measurements, all read from transcript content blocks:
//!
//! 1. **Output split** into thinking, assistant text and tool-call arguments.
//!    Claude Code reports `usage.output_tokens_details.thinking_tokens`, so its
//!    thinking share is exact; the text / tool-argument split is not reported
//!    by any source and is ESTIMATED from block length. OMP reports no split;
//!    every part is estimated. Estimates are always carried in fields whose
//!    name ends in `_est`, never mixed into an exact field.
//! 2. **Edit behaviour**: edit-tool calls, failed edits, retries (a later edit
//!    of a file whose previous edit failed) and whole-file writes over an
//!    existing file, flagged by size.
//! 3. **Session shape**: a turns-per-session histogram, the cache write of the
//!    first turn (session-start overhead) and a coarse family for each short
//!    session, derived from a hash or a fixed-label match of the first user
//!    prompt. Prompt text is never stored or printed.
//!
//! Pure data in, data out, so it is testable from small synthetic fixtures.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::cost_report::{CostRow, CostSourceKind};

/// Characters per token used where a token count must be derived from a
/// character count without a reported total to calibrate against (only the
/// "thinking hidden" fallback). A rough rule of thumb, not a tokenizer.
pub const CHARS_PER_TOKEN: u64 = 4;

/// A whole-file overwrite of at least this many bytes is flagged `large`.
pub const LARGE_OVERWRITE_BYTES: u64 = 8 * 1024;

/// A session with at most this many API turns is a "short" session.
pub const SHORT_SESSION_MAX_TURNS: u64 = 5;

/// Most families kept in the report; the rest fold into one `(remaining)` row.
pub const MAX_FAMILIES: usize = 25;

/// Turns-per-session histogram buckets: label, lowest turns, highest turns.
pub const TURN_BUCKETS: [(&str, u64, u64); 8] = [
    ("1", 1, 1),
    ("2", 2, 2),
    ("3", 3, 3),
    ("4-5", 4, 5),
    ("6-10", 6, 10),
    ("11-25", 11, 25),
    ("26-100", 26, 100),
    (">100", 101, u64::MAX),
];

/// Prompt prefixes of known automated jobs, matched against the normalised
/// first user prompt (lower case, digits as `0`, whitespace collapsed). The
/// label is printed; the prompt text never is.
const KNOWN_FAMILIES: [(&str, &str); 2] = [
    // Meridian's `meridian_summarize_title` (a headless `claude -p` call).
    ("extract the user's actual work request", "title-extraction"),
    // TA's supervisor review of a goal draft.
    ("you are a supervisor reviewing", "supervisor-review"),
];

// ── Per-turn detail ─────────────────────────────────────────────────────────

/// What one API turn's content blocks showed. Counted from the transcript, so
/// it is exact as a count; converting characters to tokens is the estimate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnDetail {
    /// `usage.output_tokens_details.thinking_tokens`, when the runtime reports it.
    pub thinking_reported: Option<u64>,
    /// Characters of visible thinking text.
    pub thinking_chars: u64,
    /// Thinking blocks whose text is empty (the provider hid it).
    pub thinking_hidden_blocks: u32,
    pub text_chars: u64,
    /// Characters of the JSON arguments of every tool call.
    pub tool_arg_chars: u64,
    pub tool_calls: u32,
    pub edit_calls: u32,
    pub edit_arg_chars: u64,
    /// Edit calls whose result was an error.
    pub edit_failures: u32,
    /// Edit calls to a file whose previous edit in the session had failed.
    pub edit_retries: u32,
    /// Argument characters spent on the failed edit calls.
    pub failed_edit_arg_chars: u64,
    /// Whole-file write calls that succeeded (creates and overwrites).
    pub file_writes: u32,
    pub write_failures: u32,
    /// Successful writes over a file that already existed.
    pub overwrites: u32,
    pub overwrite_bytes: u64,
    pub large_overwrites: u32,
    pub large_overwrite_bytes: u64,
}

impl TurnDetail {
    /// Add `o` into `self`. A reported thinking count replaces, it does not
    /// add: every record of one message repeats the same usage object.
    pub fn merge(&mut self, o: &TurnDetail) {
        if o.thinking_reported.is_some() {
            self.thinking_reported = o.thinking_reported;
        }
        self.thinking_chars += o.thinking_chars;
        self.thinking_hidden_blocks += o.thinking_hidden_blocks;
        self.text_chars += o.text_chars;
        self.tool_arg_chars += o.tool_arg_chars;
        self.tool_calls += o.tool_calls;
        self.edit_calls += o.edit_calls;
        self.edit_arg_chars += o.edit_arg_chars;
        self.edit_failures += o.edit_failures;
        self.edit_retries += o.edit_retries;
        self.failed_edit_arg_chars += o.failed_edit_arg_chars;
        self.file_writes += o.file_writes;
        self.write_failures += o.write_failures;
        self.overwrites += o.overwrites;
        self.overwrite_bytes += o.overwrite_bytes;
        self.large_overwrites += o.large_overwrites;
        self.large_overwrite_bytes += o.large_overwrite_bytes;
    }
}

/// Characters of a JSON value's compact form (the arguments as the model wrote them).
pub fn json_chars(v: &Value) -> u64 {
    match v {
        Value::Null => 0,
        other => other.to_string().chars().count() as u64,
    }
}

// ── Output split ────────────────────────────────────────────────────────────

/// How a turn's split was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitBasis {
    /// Thinking tokens were reported by the runtime (exact); text and tool
    /// arguments share the rest in proportion to their length (estimate).
    ReportedThinking,
    /// All three parts share the reported output total in proportion to their
    /// visible length (estimate).
    FromChars,
    /// Thinking was hidden and not reported: it is the output total minus the
    /// estimated text and argument tokens (estimate).
    ResidualThinking,
    /// No block content to split on; the whole output is unattributed.
    Unattributed,
}

/// One turn's output tokens by kind. The four parts always sum to the output total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSplit {
    pub thinking: u64,
    pub text: u64,
    pub tool_args: u64,
    pub unattributed: u64,
    pub basis: SplitBasis,
}

/// Sum of costs. Starts from `+0.0`: an empty `Iterator::sum` of `f64` is
/// `-0.0`, which would print as `-0.00`.
fn usd_sum(values: impl Iterator<Item = f64>) -> f64 {
    values.fold(0.0, |a, b| a + b)
}

/// Split `total` over `weights` in proportion (largest remainder, so the parts
/// sum to `total` exactly and the result is deterministic).
fn apportion(total: u64, weights: &[u64]) -> Vec<u64> {
    let sum: u128 = weights.iter().map(|w| *w as u128).sum();
    if sum == 0 {
        return vec![0; weights.len()];
    }
    let mut parts: Vec<(u64, u128)> = weights
        .iter()
        .map(|w| {
            let scaled = total as u128 * *w as u128;
            ((scaled / sum) as u64, scaled % sum)
        })
        .collect();
    let mut left = total - parts.iter().map(|p| p.0).sum::<u64>();
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|a, b| parts[*b].1.cmp(&parts[*a].1).then(a.cmp(b)));
    for i in order {
        if left == 0 {
            break;
        }
        parts[i].0 += 1;
        left -= 1;
    }
    parts.into_iter().map(|p| p.0).collect()
}

/// Split one turn's reported `output` tokens by kind.
pub fn split_output(output: u64, d: &TurnDetail) -> OutputSplit {
    let out = |thinking, text, tool_args, unattributed, basis| OutputSplit {
        thinking,
        text,
        tool_args,
        unattributed,
        basis,
    };
    if let Some(reported) = d.thinking_reported {
        let thinking = reported.min(output);
        let rest = output - thinking;
        if d.text_chars + d.tool_arg_chars == 0 {
            return out(thinking, 0, 0, rest, SplitBasis::ReportedThinking);
        }
        let p = apportion(rest, &[d.text_chars, d.tool_arg_chars]);
        return out(thinking, p[0], p[1], 0, SplitBasis::ReportedThinking);
    }
    if d.thinking_chars == 0 && d.thinking_hidden_blocks > 0 {
        let text = d.text_chars.div_ceil(CHARS_PER_TOKEN);
        let args = d.tool_arg_chars.div_ceil(CHARS_PER_TOKEN);
        if text + args >= output {
            let p = apportion(output, &[d.text_chars, d.tool_arg_chars]);
            return out(0, p[0], p[1], 0, SplitBasis::ResidualThinking);
        }
        return out(
            output - text - args,
            text,
            args,
            0,
            SplitBasis::ResidualThinking,
        );
    }
    if d.thinking_chars + d.text_chars + d.tool_arg_chars == 0 {
        return out(0, 0, 0, output, SplitBasis::Unattributed);
    }
    let p = apportion(output, &[d.thinking_chars, d.text_chars, d.tool_arg_chars]);
    out(p[0], p[1], p[2], 0, SplitBasis::FromChars)
}

// ── Totals ──────────────────────────────────────────────────────────────────

/// Output tokens by kind. Fields ending `_est` are estimates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSplitTotals {
    /// Thinking tokens the runtime reported (exact).
    pub thinking_tokens_reported: u64,
    /// Thinking tokens derived from block length or as a remainder (estimate).
    pub thinking_tokens_est: u64,
    /// Assistant text tokens (estimate: no source reports this).
    pub text_tokens_est: u64,
    /// Tool-call argument tokens (estimate: no source reports this).
    pub tool_arg_tokens_est: u64,
    /// Output tokens with no block content to attribute them to.
    pub unattributed_tokens: u64,
    /// Turns by how their split was obtained.
    pub turns_thinking_reported: u64,
    pub turns_estimated_from_chars: u64,
    pub turns_thinking_residual: u64,
    pub turns_unattributed: u64,
}

/// Edit-tool and whole-file-write measurements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditTotals {
    pub tool_calls: u64,
    pub edit_calls: u64,
    /// Estimated tokens of edit-call arguments (characters / `CHARS_PER_TOKEN`).
    pub edit_arg_tokens_est: u64,
    pub edit_failures: u64,
    pub edit_retries: u64,
    /// Estimated tokens spent on the arguments of failed edit calls.
    pub failed_edit_arg_tokens_est: u64,
    pub file_writes: u64,
    pub write_failures: u64,
    /// Writes over a file known to exist (see the notes for how each source knows).
    pub whole_file_overwrites: u64,
    pub overwrite_bytes: u64,
    /// Overwrites of at least `LARGE_OVERWRITE_BYTES`.
    pub large_overwrites: u64,
    pub large_overwrite_bytes: u64,
}

/// Detail totals for a group of transcript turns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailTotals {
    /// Transcript turns that carry detail (goals have none).
    pub turns: u64,
    /// Output tokens of those turns: the denominator for the split below.
    pub output_tokens: u64,
    pub output_split: OutputSplitTotals,
    pub edits: EditTotals,
}

impl DetailTotals {
    pub fn add(&mut self, output: u64, d: &TurnDetail) {
        self.turns += 1;
        self.output_tokens += output;
        let s = split_output(output, d);
        let o = &mut self.output_split;
        match s.basis {
            SplitBasis::ReportedThinking => {
                o.thinking_tokens_reported += s.thinking;
                o.turns_thinking_reported += 1;
            }
            SplitBasis::FromChars => {
                o.thinking_tokens_est += s.thinking;
                o.turns_estimated_from_chars += 1;
            }
            SplitBasis::ResidualThinking => {
                o.thinking_tokens_est += s.thinking;
                o.turns_thinking_residual += 1;
            }
            SplitBasis::Unattributed => o.turns_unattributed += 1,
        }
        o.text_tokens_est += s.text;
        o.tool_arg_tokens_est += s.tool_args;
        o.unattributed_tokens += s.unattributed;
        let e = &mut self.edits;
        e.tool_calls += d.tool_calls as u64;
        e.edit_calls += d.edit_calls as u64;
        e.edit_arg_tokens_est += d.edit_arg_chars.div_ceil(CHARS_PER_TOKEN);
        e.edit_failures += d.edit_failures as u64;
        e.edit_retries += d.edit_retries as u64;
        e.failed_edit_arg_tokens_est += d.failed_edit_arg_chars.div_ceil(CHARS_PER_TOKEN);
        e.file_writes += d.file_writes as u64;
        e.write_failures += d.write_failures as u64;
        e.whole_file_overwrites += d.overwrites as u64;
        e.overwrite_bytes += d.overwrite_bytes;
        e.large_overwrites += d.large_overwrites as u64;
        e.large_overwrite_bytes += d.large_overwrite_bytes;
    }
}

// ── Tool tracking (edit retries, whole-file writes) ─────────────────────────

/// Which transcript format's tool names and result shapes to expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Claude Code: `Edit`/`MultiEdit`/`NotebookEdit`, `Write`; results are
    /// `tool_result` blocks and `toolUseResult.type` is `create` or `update`.
    Claude,
    /// OMP: `edit`, `write`, `read`; results are `toolResult` messages.
    Omp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolClass {
    Edit,
    Write,
    Read,
    Other,
}

fn classify(d: Dialect, name: &str) -> ToolClass {
    match (d, name) {
        (Dialect::Claude, "Edit" | "MultiEdit" | "NotebookEdit") => ToolClass::Edit,
        (Dialect::Claude, "Write") => ToolClass::Write,
        (Dialect::Omp, "edit") => ToolClass::Edit,
        (Dialect::Omp, "write") => ToolClass::Write,
        (Dialect::Omp, "read") => ToolClass::Read,
        _ => ToolClass::Other,
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The file an OMP `read` names, without a `:50-80` or `:raw` selector.
fn strip_selector(path: &str) -> &str {
    if let Some(i) = path.rfind(':') {
        let tail = &path[i + 1..];
        let numeric = !tail.is_empty()
            && tail
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | ',' | ':'));
        if numeric || matches!(tail, "raw" | "conflicts") {
            return strip_selector(&path[..i]);
        }
    }
    path
}

/// File paths an OMP `edit` call touches: the `[path#TAG]` headers of its
/// `input`, plus a plain `path` argument when the call has one.
fn omp_edit_paths(input: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: &str| {
        let p = p.trim();
        if !p.is_empty() && !out.iter().any(|q| q == p) {
            out.push(p.to_string());
        }
    };
    if let Some(p) = str_at(input, "path") {
        push(strip_selector(p));
    }
    if let Some(text) = str_at(input, "input") {
        for line in text.lines() {
            let line = line.trim_end();
            let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) else {
                continue;
            };
            if let Some(i) = inner.rfind('#') {
                let tag = &inner[i + 1..];
                if (1..=8).contains(&tag.len()) && tag.chars().all(|c| c.is_ascii_alphanumeric()) {
                    push(&inner[..i]);
                }
            }
        }
    }
    out
}

fn base_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `short` names the same file as `long` when it equals it or is a trailing
/// path of it (an OMP header may be relative where a result is absolute).
fn same_file(a: &str, b: &str) -> bool {
    let a = a.strip_prefix("./").unwrap_or(a);
    let b = b.strip_prefix("./").unwrap_or(b);
    let tail_of = |long: &str, short: &str| {
        long.len() > short.len()
            && long.ends_with(short)
            && long.as_bytes()[long.len() - short.len() - 1] == b'/'
    };
    a == b || tail_of(a, b) || tail_of(b, a)
}

#[derive(Debug)]
struct Pending {
    row: usize,
    session: String,
    class: ToolClass,
    paths: Vec<String>,
    arg_chars: u64,
    write_bytes: u64,
    /// OMP: the write targeted a path already read, edited or written in the session.
    existing_hint: Option<bool>,
}

#[derive(Debug, Default)]
struct SessionState {
    /// Files whose latest edit failed and has not been retried yet.
    failed_edits: Vec<String>,
    /// Files known to exist (OMP only), by base name.
    touched: HashMap<String, Vec<String>>,
}

impl SessionState {
    fn touch(&mut self, path: &str) {
        let v = self.touched.entry(base_name(path).to_string()).or_default();
        if !v.iter().any(|p| same_file(p, path)) {
            v.push(path.to_string());
        }
    }

    fn knows(&self, path: &str) -> bool {
        self.touched
            .get(base_name(path))
            .is_some_and(|v| v.iter().any(|p| same_file(p, path)))
    }
}

/// Follows tool calls from the assistant turn that made them to the result that
/// arrives later, to find failed edits, retries and overwrites.
#[derive(Debug)]
pub struct ToolTracker {
    dialect: Dialect,
    pending: HashMap<String, Pending>,
    sessions: HashMap<String, SessionState>,
}

impl ToolTracker {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            pending: HashMap::new(),
            sessions: HashMap::new(),
        }
    }

    /// Record a tool call made by turn `row` in `session` and return what it adds to that turn.
    pub fn call(
        &mut self,
        session: &str,
        row: usize,
        id: &str,
        name: &str,
        input: &Value,
    ) -> TurnDetail {
        let arg_chars = json_chars(input);
        let mut d = TurnDetail {
            tool_calls: 1,
            tool_arg_chars: arg_chars,
            ..TurnDetail::default()
        };
        let mut class = classify(self.dialect, name);
        let mut paths: Vec<String> = Vec::new();
        let mut write_bytes = 0;
        let mut existing_hint = None;
        match class {
            ToolClass::Edit => {
                d.edit_calls = 1;
                d.edit_arg_chars = arg_chars;
                paths = match self.dialect {
                    Dialect::Claude => str_at(input, "file_path")
                        .or_else(|| str_at(input, "notebook_path"))
                        .map(|p| vec![p.to_string()])
                        .unwrap_or_default(),
                    Dialect::Omp => omp_edit_paths(input),
                };
                let st = self.sessions.entry(session.to_string()).or_default();
                let before = st.failed_edits.len();
                st.failed_edits
                    .retain(|failed| !paths.iter().any(|p| same_file(failed, p)));
                if st.failed_edits.len() != before {
                    d.edit_retries = 1;
                }
            }
            ToolClass::Write => {
                // OMP also routes `xd://` device calls through `write`; only a
                // real file path with string content is a file write.
                let path = str_at(input, "file_path").or_else(|| str_at(input, "path"));
                let content = input.get("content").and_then(Value::as_str);
                match (path, content) {
                    (Some(p), Some(c)) if !p.contains("://") => {
                        write_bytes = c.len() as u64;
                        paths = vec![p.to_string()];
                        if self.dialect == Dialect::Omp {
                            existing_hint = Some(
                                self.sessions
                                    .entry(session.to_string())
                                    .or_default()
                                    .knows(p),
                            );
                        }
                    }
                    _ => class = ToolClass::Other,
                }
            }
            ToolClass::Read if self.dialect == Dialect::Omp => {
                if let Some(p) = str_at(input, "path") {
                    paths = vec![strip_selector(p).to_string()];
                }
            }
            ToolClass::Read | ToolClass::Other => {}
        }
        if class != ToolClass::Other && !id.is_empty() {
            self.pending.insert(
                id.to_string(),
                Pending {
                    row,
                    session: session.to_string(),
                    class,
                    paths,
                    arg_chars,
                    write_bytes,
                    existing_hint,
                },
            );
        }
        d
    }

    /// Record the result of call `id`. `existing` is the runtime's own answer
    /// to "did the file exist" when it gives one (Claude `toolUseResult.type`).
    /// Returns the row of the call and what the result adds to it; `None` when
    /// `id` is not a call this tracker is following.
    pub fn result(
        &mut self,
        id: &str,
        is_error: bool,
        existing: Option<bool>,
    ) -> Option<(usize, TurnDetail)> {
        let p = self.pending.remove(id)?;
        let omp = self.dialect == Dialect::Omp;
        let st = self.sessions.entry(p.session.clone()).or_default();
        let mut d = TurnDetail::default();
        match p.class {
            ToolClass::Edit if is_error => {
                d.edit_failures = 1;
                d.failed_edit_arg_chars = p.arg_chars;
                st.failed_edits.extend(p.paths);
            }
            ToolClass::Edit | ToolClass::Read => {
                if omp && !is_error {
                    p.paths.iter().for_each(|f| st.touch(f));
                }
            }
            ToolClass::Write if is_error => d.write_failures = 1,
            ToolClass::Write => {
                d.file_writes = 1;
                if existing.or(p.existing_hint) == Some(true) {
                    d.overwrites = 1;
                    d.overwrite_bytes = p.write_bytes;
                    if p.write_bytes >= LARGE_OVERWRITE_BYTES {
                        d.large_overwrites = 1;
                        d.large_overwrite_bytes = p.write_bytes;
                    }
                }
                if omp {
                    p.paths.iter().for_each(|f| st.touch(f));
                }
            }
            ToolClass::Other => {}
        }
        Some((p.row, d))
    }
}

// ── First-prompt families ───────────────────────────────────────────────────

/// A coarse label for a session from its first user prompt. A known automated
/// job gets its fixed label; anything else gets `other:<8 hex>`, the hash of
/// the normalised first 48 characters. The prompt text is never returned.
pub fn classify_prompt(prompt: &str) -> String {
    let mut norm = String::new();
    let mut last_space = true;
    for c in prompt.chars().take(160) {
        let c = if c.is_ascii_digit() {
            '0'
        } else {
            c.to_lowercase().next().unwrap_or(c)
        };
        if c.is_whitespace() {
            if !last_space {
                norm.push(' ');
            }
            last_space = true;
        } else {
            norm.push(c);
            last_space = false;
        }
    }
    let norm = norm.trim_end();
    for (prefix, label) in KNOWN_FAMILIES {
        if norm.starts_with(prefix) {
            return label.to_string();
        }
    }
    let head: String = norm.chars().take(48).collect();
    let digest = Sha256::digest(head.as_bytes());
    format!(
        "other:{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

// ── Session shape ───────────────────────────────────────────────────────────

/// Distribution of the cache write of each session's first turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FirstTurnStats {
    pub sessions: u64,
    pub cache_write_tokens_total: u64,
    pub cache_write_tokens_mean: u64,
    pub cache_write_tokens_median: u64,
    pub cache_write_tokens_p90: u64,
    pub cache_write_tokens_max: u64,
    /// First-turn cache write as a share of every cache write in these sessions.
    pub share_of_cache_write_pct: f64,
    /// Whole cost of the first turns (cache write, any cache read, input and output).
    pub usd_total: f64,
    pub usd_share_pct: f64,
}

/// One class of sessions (`all`, `short`, `long`) of one source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClassShape {
    pub class: String,
    pub sessions: u64,
    pub turns: u64,
    pub usd: f64,
    pub usd_share_pct: f64,
    pub cache_write_tokens: u64,
    pub first_turn: FirstTurnStats,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistogramBucket {
    pub turns: String,
    pub sessions: u64,
    pub usd: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceShape {
    pub source: String,
    pub turns_histogram: Vec<HistogramBucket>,
    pub classes: Vec<ClassShape>,
}

/// Short sessions of one (source, entrypoint, first-prompt family).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FamilyRow {
    pub source: String,
    /// `sdk-cli` for a headless `claude -p` run, `cli` for an interactive one;
    /// `n/a` where the transcript does not record it.
    pub entrypoint: String,
    pub family: String,
    pub sessions: u64,
    pub turns: u64,
    pub usd: f64,
    pub usd_share_of_short_pct: f64,
    pub cache_write_tokens: u64,
    pub first_turn_cache_write_tokens_mean: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionShape {
    /// A session with at most this many turns is `short`.
    pub short_max_turns: u64,
    pub sources: Vec<SourceShape>,
    /// Short sessions by first-prompt family, largest spend first.
    pub families: Vec<FamilyRow>,
    /// Families beyond `MAX_FAMILIES` folded into the `(remaining)` row.
    pub families_folded: u64,
}

struct SessionAgg {
    source: CostSourceKind,
    turns: u64,
    usd: f64,
    cache_write: u64,
    first_cache_write: u64,
    first_usd: f64,
    first_key: (bool, Option<chrono::DateTime<chrono::Utc>>, usize),
    family: Option<String>,
    entrypoint: Option<String>,
}

fn pct(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        part / whole * 100.0
    } else {
        0.0
    }
}

fn nearest_rank(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn first_turn_stats(sessions: &[&SessionAgg]) -> FirstTurnStats {
    let mut cw: Vec<u64> = sessions.iter().map(|s| s.first_cache_write).collect();
    cw.sort_unstable();
    let total: u64 = cw.iter().sum();
    let all_cw: u64 = sessions.iter().map(|s| s.cache_write).sum();
    let usd_first = usd_sum(sessions.iter().map(|s| s.first_usd));
    let usd_all = usd_sum(sessions.iter().map(|s| s.usd));
    FirstTurnStats {
        sessions: cw.len() as u64,
        cache_write_tokens_total: total,
        cache_write_tokens_mean: total.checked_div(cw.len() as u64).unwrap_or(0),
        cache_write_tokens_median: nearest_rank(&cw, 0.5),
        cache_write_tokens_p90: nearest_rank(&cw, 0.9),
        cache_write_tokens_max: cw.last().copied().unwrap_or(0),
        share_of_cache_write_pct: pct(total as f64, all_cw as f64),
        usd_total: usd_first,
        usd_share_pct: pct(usd_first, usd_all),
    }
}

fn class_shape(class: &str, sessions: &[&SessionAgg], source_usd: f64) -> ClassShape {
    let usd = usd_sum(sessions.iter().map(|s| s.usd));
    ClassShape {
        class: class.to_string(),
        sessions: sessions.len() as u64,
        turns: sessions.iter().map(|s| s.turns).sum(),
        usd,
        usd_share_pct: pct(usd, source_usd),
        cache_write_tokens: sessions.iter().map(|s| s.cache_write).sum(),
        first_turn: first_turn_stats(sessions),
    }
}

/// Session shape for the transcript sessions of `rows` (goals are not sessions).
///
/// A session is included when its FIRST turn passes `keep`, and then counts
/// all of its turns: "turns per session" and "first-turn overhead" are only
/// meaningful for a session seen from its start. This differs from the row
/// table, which counts only the turns inside the window.
pub fn session_shape(rows: &[CostRow], keep: impl Fn(&CostRow) -> bool) -> SessionShape {
    let mut by_session: HashMap<(CostSourceKind, &str), SessionAgg> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if r.source == CostSourceKind::Goals {
            continue;
        }
        // Earlier turn wins; a turn with no timestamp sorts after any dated turn.
        let key = (r.ts.is_none(), r.ts, i);
        let s = by_session
            .entry((r.source, r.session.as_str()))
            .or_insert_with(|| SessionAgg {
                source: r.source,
                turns: 0,
                usd: 0.0,
                cache_write: 0,
                first_cache_write: r.usage.cache_creation,
                first_usd: r.usd,
                first_key: key,
                family: None,
                entrypoint: None,
            });
        s.turns += 1;
        s.usd += r.usd;
        s.cache_write += r.usage.cache_creation;
        if key < s.first_key {
            s.first_key = key;
            s.first_cache_write = r.usage.cache_creation;
            s.first_usd = r.usd;
        }
        if s.family.is_none() {
            s.family = r.family.clone();
        }
        if s.entrypoint.is_none() {
            s.entrypoint = r.entrypoint.clone();
        }
    }
    let sessions: Vec<SessionAgg> = by_session
        .into_iter()
        .filter(|(_, s)| keep(&rows[s.first_key.2]))
        .map(|(_, s)| s)
        .collect();

    let mut sources: Vec<SourceShape> = Vec::new();
    let mut by_source: BTreeMap<CostSourceKind, Vec<&SessionAgg>> = BTreeMap::new();
    for s in &sessions {
        by_source.entry(s.source).or_default().push(s);
    }
    for (source, list) in &by_source {
        let usd = usd_sum(list.iter().map(|s| s.usd));
        let turns_histogram = TURN_BUCKETS
            .iter()
            .map(|(label, lo, hi)| {
                let inb: Vec<&&SessionAgg> = list
                    .iter()
                    .filter(|s| (*lo..=*hi).contains(&s.turns))
                    .collect();
                HistogramBucket {
                    turns: label.to_string(),
                    sessions: inb.len() as u64,
                    usd: usd_sum(inb.iter().map(|s| s.usd)),
                }
            })
            .collect();
        let short: Vec<&SessionAgg> = list
            .iter()
            .copied()
            .filter(|s| s.turns <= SHORT_SESSION_MAX_TURNS)
            .collect();
        let long: Vec<&SessionAgg> = list
            .iter()
            .copied()
            .filter(|s| s.turns > SHORT_SESSION_MAX_TURNS)
            .collect();
        sources.push(SourceShape {
            source: source.as_str().to_string(),
            turns_histogram,
            classes: vec![
                class_shape("all", list, usd),
                class_shape("short", &short, usd),
                class_shape("long", &long, usd),
            ],
        });
    }

    // Short sessions by (source, entrypoint, family).
    type FamilyKey = (CostSourceKind, String, String);
    let mut fam: BTreeMap<FamilyKey, Vec<&SessionAgg>> = BTreeMap::new();
    for s in sessions
        .iter()
        .filter(|s| s.turns <= SHORT_SESSION_MAX_TURNS)
    {
        fam.entry((
            s.source,
            s.entrypoint.clone().unwrap_or_else(|| "n/a".to_string()),
            s.family.clone().unwrap_or_else(|| "n/a".to_string()),
        ))
        .or_default()
        .push(s);
    }
    let short_usd = usd_sum(
        sessions
            .iter()
            .filter(|s| s.turns <= SHORT_SESSION_MAX_TURNS)
            .map(|s| s.usd),
    );
    let family_row = |source: &str, entrypoint: &str, family: &str, list: &[&SessionAgg]| {
        let usd = usd_sum(list.iter().map(|s| s.usd));
        let first: u64 = list.iter().map(|s| s.first_cache_write).sum();
        FamilyRow {
            source: source.to_string(),
            entrypoint: entrypoint.to_string(),
            family: family.to_string(),
            sessions: list.len() as u64,
            turns: list.iter().map(|s| s.turns).sum(),
            usd,
            usd_share_of_short_pct: pct(usd, short_usd),
            cache_write_tokens: list.iter().map(|s| s.cache_write).sum(),
            first_turn_cache_write_tokens_mean: first.checked_div(list.len() as u64).unwrap_or(0),
        }
    };
    let mut families: Vec<FamilyRow> = fam
        .iter()
        .map(|((src, ep, f), list)| family_row(src.as_str(), ep, f, list))
        .collect();
    families.sort_by(|a, b| {
        b.usd
            .partial_cmp(&a.usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.sessions.cmp(&a.sessions))
            .then(a.family.cmp(&b.family))
    });
    let mut families_folded = 0;
    if families.len() > MAX_FAMILIES {
        let rest = families.split_off(MAX_FAMILIES);
        families_folded = rest.len() as u64;
        let mut folded = FamilyRow {
            source: "*".into(),
            entrypoint: "*".into(),
            family: "(remaining)".into(),
            ..FamilyRow::default()
        };
        let mut first_total = 0u64;
        for r in &rest {
            folded.sessions += r.sessions;
            folded.turns += r.turns;
            folded.usd += r.usd;
            folded.cache_write_tokens += r.cache_write_tokens;
            first_total += r.first_turn_cache_write_tokens_mean * r.sessions;
        }
        folded.usd_share_of_short_pct = pct(folded.usd, short_usd);
        folded.first_turn_cache_write_tokens_mean =
            first_total.checked_div(folded.sessions).unwrap_or(0);
        families.push(folded);
    }
    SessionShape {
        short_max_turns: SHORT_SESSION_MAX_TURNS,
        sources,
        families,
        families_folded,
    }
}

/// Per-session instrumentation for the `--rows` and Meridian exports.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionInstrumentation {
    pub turns: u64,
    /// Cache write of the earliest turn (session-start overhead).
    pub first_turn_cache_write_tokens: u64,
    pub first_turn_context_tokens: Option<u64>,
    pub session_family: Option<String>,
    pub entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<DetailTotals>,
}

/// Instrumentation of one transcript session from its turns; `None` for goals
/// or an empty slice.
pub fn session_instrumentation(rows: &[&CostRow]) -> Option<SessionInstrumentation> {
    let first = rows
        .iter()
        .enumerate()
        .min_by_key(|(i, r)| (r.ts.is_none(), r.ts, *i))
        .map(|(_, r)| *r)?;
    if first.source == CostSourceKind::Goals {
        return None;
    }
    let mut detail: Option<DetailTotals> = None;
    for r in rows {
        if let Some(d) = &r.detail {
            detail
                .get_or_insert_with(DetailTotals::default)
                .add(r.usage.output, d);
        }
    }
    Some(SessionInstrumentation {
        turns: rows.len() as u64,
        first_turn_cache_write_tokens: first.usage.cache_creation,
        first_turn_context_tokens: first.context_tokens,
        session_family: rows.iter().find_map(|r| r.family.clone()),
        entrypoint: rows.iter().find_map(|r| r.entrypoint.clone()),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(thinking: u64, text: u64, args: u64) -> TurnDetail {
        TurnDetail {
            thinking_chars: thinking,
            text_chars: text,
            tool_arg_chars: args,
            ..TurnDetail::default()
        }
    }

    #[test]
    fn apportion_sums_exactly_and_is_proportional() {
        assert_eq!(apportion(10, &[1, 1, 1]).iter().sum::<u64>(), 10);
        assert_eq!(apportion(100, &[1, 3]), vec![25, 75]);
        assert_eq!(apportion(7, &[0, 0]), vec![0, 0]);
        assert_eq!(
            apportion(u64::MAX, &[u64::MAX, u64::MAX])
                .iter()
                .sum::<u64>(),
            u64::MAX
        );
    }

    #[test]
    fn reported_thinking_is_kept_exact_and_the_rest_is_split_by_length() {
        let mut t = d(0, 100, 300);
        t.thinking_reported = Some(60);
        let s = split_output(100, &t);
        assert_eq!(s.basis, SplitBasis::ReportedThinking);
        assert_eq!(
            (s.thinking, s.text, s.tool_args, s.unattributed),
            (60, 10, 30, 0)
        );
    }

    #[test]
    fn reported_thinking_above_the_total_is_clamped() {
        let mut t = d(0, 10, 10);
        t.thinking_reported = Some(500);
        let s = split_output(40, &t);
        assert_eq!((s.thinking, s.text, s.tool_args), (40, 0, 0));
    }

    #[test]
    fn visible_thinking_splits_the_total_by_length() {
        let s = split_output(200, &d(100, 100, 200));
        assert_eq!(s.basis, SplitBasis::FromChars);
        assert_eq!((s.thinking, s.text, s.tool_args), (50, 50, 100));
    }

    #[test]
    fn hidden_thinking_is_the_remainder_after_estimated_text_and_args() {
        let mut t = d(0, 400, 800); // 100 + 200 estimated tokens
        t.thinking_hidden_blocks = 1;
        let s = split_output(1000, &t);
        assert_eq!(s.basis, SplitBasis::ResidualThinking);
        assert_eq!((s.thinking, s.text, s.tool_args), (700, 100, 200));
        // Estimated visible tokens above the total: thinking is 0, not negative.
        let s = split_output(150, &t);
        assert_eq!(s.thinking, 0);
        assert_eq!(s.text + s.tool_args, 150);
    }

    #[test]
    fn no_blocks_means_unattributed_not_invented() {
        let s = split_output(42, &TurnDetail::default());
        assert_eq!(s.basis, SplitBasis::Unattributed);
        assert_eq!(s.unattributed, 42);
    }

    #[test]
    fn prompt_families_use_labels_for_known_jobs_and_hashes_otherwise() {
        assert_eq!(
            classify_prompt("Extract the user's actual work request from the text below"),
            "title-extraction"
        );
        assert_eq!(
            classify_prompt("  You are a  supervisor reviewing an AI agent's work"),
            "supervisor-review"
        );
        let a = classify_prompt("Fix the thing in PR 123 please");
        let b = classify_prompt("fix the thing in pr 456   please");
        assert_eq!(a, b, "digits and case do not split a family");
        assert!(
            a.starts_with("other:") && a.len() == "other:".len() + 8,
            "{a}"
        );
        assert_ne!(a, classify_prompt("Something else entirely"));
        assert!(!a.contains("thing"), "the prompt text must not leak: {a}");
    }

    #[test]
    fn omp_edit_paths_come_from_headers_not_body_lines() {
        let input = serde_json::json!({"input":
            "[src/a.rs#E9C6]\nPUT 1:\n+[not a header#ABCD]\n[/abs/b.py#12AF]\nCUT 3"});
        assert_eq!(omp_edit_paths(&input), vec!["src/a.rs", "/abs/b.py"]);
    }

    #[test]
    fn same_file_matches_relative_to_absolute_only_on_a_path_boundary() {
        assert!(same_file("src/a.rs", "/w/p/src/a.rs"));
        assert!(same_file("./src/a.rs", "src/a.rs"));
        assert!(!same_file("a.rs", "/w/p/data.rs"));
        assert!(!same_file("src/a.rs", "/w/p/asrc/a.rs"));
    }

    #[test]
    fn read_selectors_are_stripped() {
        assert_eq!(strip_selector("notes.txt:450-455"), "notes.txt");
        assert_eq!(strip_selector("notes.txt:raw:2-4"), "notes.txt");
        assert_eq!(strip_selector("C:/dir/x.rs"), "C:/dir/x.rs");
    }
}
