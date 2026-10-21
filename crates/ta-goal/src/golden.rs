//! Golden set: capture a completed goal and compare replays of it (v0.17.12.20).
//!
//! A golden entry is a completed goal stored as (git commit, prompt, baseline
//! result) under `.ta/golden/<id>/`. A replay runs the same prompt from the
//! same commit with an override and is compared to the baseline: cost, tokens,
//! outcome, whether the check command passes and how close the diff is.
//!
//! This module holds the pure parts (data model, storage, diff normalization,
//! similarity, verdict). Git, `ta run` and the draft store live in the CLI.
//!
//! The verdict is advisory. Anything that is not `equivalent` is listed for a
//! person to review; nothing here accepts, applies or merges a replay.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Diff similarity at or above this counts as the same change.
pub const EQUIVALENCE_THRESHOLD: f64 = 0.80;

/// Longest id accepted for an entry (it names a directory).
const MAX_ID_LEN: usize = 64;

/// `.ta/golden` under `project_root`.
pub fn golden_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("golden")
}

/// What the original goal cost and produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub cost_usd: f64,
    /// `true` when `cost_usd` came from the price table, not the runtime.
    #[serde(default)]
    pub cost_estimated: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    /// Agent turns. `None` because the agent runtime does not report a turn
    /// count to TA today; shown as "not recorded", never guessed.
    #[serde(default)]
    pub turns: Option<u64>,
    /// Goal state when captured (for example `applied`).
    pub outcome: String,
    pub files_changed: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub diff_sha256: String,
    /// Result of the check command run against the goal's staging workspace at
    /// capture. `None` when no check was given or it could not be run.
    #[serde(default)]
    pub check_passed: Option<bool>,
    /// The full record of that check run (command, times, exit code, output tails,
    /// test counts). `check_passed` is kept for entries that predate it.
    #[serde(default)]
    pub check_run: Option<CheckRun>,
    /// Counts recomputed by `ta golden recompute`. The numbers above are never
    /// overwritten, so the original and the corrected values can be compared.
    #[serde(default)]
    pub recomputed: Option<RecomputedStats>,
    pub captured_at: DateTime<Utc>,
}

/// The settings a run happened under, so a baseline can be compared with a later run on
/// like terms. Without it a token or cost delta cannot be attributed to anything.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunEnvironment {
    /// How this was obtained: `measured at replay start`, `measured at capture (after the run
    /// finished, so it may differ from the run's own settings)` or `declared by owner: ...`.
    pub provenance: String,
    #[serde(default)]
    pub ta_version: String,
    #[serde(default)]
    pub claude_code_version: String,
    #[serde(default)]
    pub rtk_version: String,
    #[serde(default)]
    pub headroom_version: String,
    /// Resolved optimizer choice per slot (`none`, `rtk`, `headroom`, ... or `not chosen`).
    #[serde(default)]
    pub input_slot: String,
    #[serde(default)]
    pub context_slot: String,
    #[serde(default)]
    pub output_slot: String,
    #[serde(default)]
    pub compression_enabled: bool,
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    /// Claude Code `autoCompactWindow` from `~/.claude/settings.json`, when set.
    #[serde(default)]
    pub auto_compact_window: Option<u64>,
    #[serde(default)]
    pub daemon_toml_sha256: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl RunEnvironment {
    /// One line for reports.
    pub fn summary(&self) -> String {
        format!(
            "input={} context={} output={} compression={} ta={} claude={} rtk={} headroom={} ({})",
            self.input_slot,
            self.context_slot,
            self.output_slot,
            if self.compression_enabled {
                "on"
            } else {
                "off"
            },
            or_unknown(&self.ta_version),
            or_unknown(&self.claude_code_version),
            or_unknown(&self.rtk_version),
            or_unknown(&self.headroom_version),
            self.provenance
        )
    }
}

fn or_unknown(s: &str) -> &str {
    if s.is_empty() {
        "?"
    } else {
        s
    }
}

/// One golden entry (`entry.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenEntry {
    pub id: String,
    pub title: String,
    pub source_goal_id: String,
    pub start_commit: String,
    #[serde(default)]
    pub start_dirty: bool,
    #[serde(default)]
    pub start_branch: Option<String>,
    pub prompt_sha256: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub agent_model: String,
    #[serde(default)]
    pub check_command: Option<String>,
    pub baseline: Baseline,
    /// What the baseline ran under. `None` on entries captured before this was recorded;
    /// such a baseline is not a valid comparison until `ta golden annotate` fills it in.
    #[serde(default)]
    pub baseline_environment: Option<RunEnvironment>,
}

/// What one replay produced (`replays/<replay_id>.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayRecord {
    pub replay_id: String,
    pub golden_id: String,
    /// The `experiment_arm` the replay goal carries.
    pub tag: String,
    /// The `--set key=value` pairs as given.
    #[serde(default)]
    pub set: Vec<String>,
    /// The goal the replay created, when `ta run` got far enough to create one.
    #[serde(default)]
    pub goal_id: Option<String>,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// Goal state when measured, or `no goal created`.
    pub outcome: String,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub cost_estimated: bool,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub turns: Option<u64>,
    /// The goal stopped on `--max-tokens` or `--max-usd`.
    #[serde(default)]
    pub aborted_budget: bool,
    #[serde(default)]
    pub files_changed: usize,
    /// Changed lines, from a real unified diff. `None` on replays recorded before
    /// these were stored (their `files_changed` came from the old differ).
    #[serde(default)]
    pub lines_added: Option<usize>,
    #[serde(default)]
    pub lines_removed: Option<usize>,
    #[serde(default)]
    pub diff_sha256: Option<String>,
    /// Similarity to the baseline diff, 0.0 to 1.0. `None` when no diff could
    /// be produced.
    #[serde(default)]
    pub similarity: Option<f64>,
    #[serde(default)]
    pub check_passed: Option<bool>,
    /// The full record of the check run for this replay: command line, times, exit
    /// code, pass/fail, output tails and parsed test counts. Lives in this record,
    /// which is keyed by the unique `replay_id`, so two runs cannot collide.
    #[serde(default)]
    pub check_run: Option<CheckRun>,
    /// 1-based position of this replay among the entry's replays, in the order they
    /// were run. With `started_at` and `completed_at` (wall clock) this lets run-order
    /// effects such as prompt cache warmth be analysed.
    #[serde(default)]
    pub run_index: Option<u32>,
    /// Counts recomputed by `ta golden recompute`; the original numbers stay as recorded.
    #[serde(default)]
    pub recomputed: Option<RecomputedStats>,
    /// Why the replay could not be measured, in words the user can act on.
    #[serde(default)]
    pub failure: Option<String>,
    /// The settings in force when the replay started.
    #[serde(default)]
    pub environment: Option<RunEnvironment>,
}

/// Verdict for one replay against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Check passes (or none was defined) and the diff is close to the baseline.
    Equivalent,
    /// Ran to a result but needs a person to look at it.
    Differs,
    /// No usable result: it failed, aborted, or the check failed.
    Failed,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Equivalent => "equivalent",
            Verdict::Differs => "differs",
            Verdict::Failed => "failed",
        }
    }
}

/// A verdict and the reasons behind it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerdictResult {
    pub verdict: Verdict,
    pub reasons: Vec<String>,
}

impl VerdictResult {
    /// Anything not `equivalent` is listed for a person; it is never accepted
    /// automatically.
    pub fn needs_human_review(&self) -> bool {
        self.verdict != Verdict::Equivalent
    }
}

/// Decide the verdict for `replay` against `entry`.
///
/// `failed`: the replay created no goal, hit a budget ceiling, ended in a
/// failed state, had no diff to compare, or its check failed.
/// `equivalent`: not failed, the check passed or none is defined, and the diff
/// similarity is at least [`EQUIVALENCE_THRESHOLD`].
/// `differs`: everything else.
pub fn verdict(entry: &GoldenEntry, replay: &ReplayRecord) -> VerdictResult {
    let mut failed = Vec::new();
    if let Some(f) = &replay.failure {
        failed.push(f.clone());
    }
    if replay.aborted_budget {
        failed.push(
            "the replay hit its --max-tokens or --max-usd ceiling and stopped early".to_string(),
        );
    }
    let state = replay.outcome.as_str();
    if state == "failed" || state.starts_with("failed") {
        failed.push(format!("the replay goal ended in state `{state}`"));
    }
    if replay.check_passed == Some(false) {
        failed.push(format!(
            "the check command failed: {}",
            entry.check_command.as_deref().unwrap_or("(not recorded)")
        ));
    }
    if replay.similarity.is_none() && failed.is_empty() {
        failed.push("no diff could be produced to compare with the baseline".to_string());
    }
    if !failed.is_empty() {
        return VerdictResult {
            verdict: Verdict::Failed,
            reasons: failed,
        };
    }

    let similarity = replay.similarity.unwrap_or(0.0);
    let mut reasons = Vec::new();
    if entry.check_command.is_none() {
        reasons.push(
            "no check command was captured, so this rests on diff similarity alone".to_string(),
        );
    }
    if similarity >= EQUIVALENCE_THRESHOLD {
        return VerdictResult {
            verdict: Verdict::Equivalent,
            reasons,
        };
    }
    reasons.insert(
        0,
        format!(
            "diff similarity {:.0}% is below the {:.0}% threshold",
            similarity * 100.0,
            EQUIVALENCE_THRESHOLD * 100.0
        ),
    );
    VerdictResult {
        verdict: Verdict::Differs,
        reasons,
    }
}

/// Reject ids that are empty, too long or could leave `.ta/golden`.
pub fn validate_id(id: &str) -> anyhow::Result<()> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && !id.starts_with('.');
    if ok {
        Ok(())
    } else {
        anyhow::bail!(
            "'{id}' is not a valid golden id. Ids are 1 to {MAX_ID_LEN} characters of a-z, 0-9, '-', '_' and '.', \
             and do not start with '.'. Run `ta golden list` to see the ids that exist."
        )
    }
}

/// Entry id for a goal: the title as a slug plus the first 8 characters of the
/// goal id, so two goals with one title do not collide.
pub fn default_entry_id(title: &str, goal_id: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_matches('-');
    let short: String = goal_id.chars().filter(|c| *c != '-').take(8).collect();
    if slug.is_empty() {
        format!("goal-{short}")
    } else {
        format!("{slug}-{short}")
    }
}

/// SHA-256 of `text` as lowercase hex.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parse `--set key=value` pairs into the override object stored on the goal.
///
/// A value that is valid JSON (`true`, `3`, `"x"`, `[1]`) is kept as that
/// JSON value; anything else is a string. Keys are kept flat (`a.b` stays
/// `a.b`), like experiment arms. A repeated key keeps the last value.
pub fn parse_overrides(pairs: &[String]) -> anyhow::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let (key, value) = pair.split_once('=').ok_or_else(|| {
            anyhow::anyhow!(
                "--set '{pair}' is not key=value. Example: --set optimizers.input.use=none"
            )
        })?;
        let key = key.trim();
        if key.is_empty() {
            anyhow::bail!(
                "--set '{pair}' has an empty key. Example: --set optimizers.input.use=none"
            );
        }
        let parsed = serde_json::from_str::<serde_json::Value>(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));
        map.insert(key.to_string(), parsed);
    }
    Ok(serde_json::Value::Object(map))
}

// ── Diff ────────────────────────────────────────────────────────────────

/// Unchanged lines shown around each change, like `git diff` (`-U3`).
pub const CONTEXT_LINES: usize = 3;

/// Render the change to one file as a real unified diff: `--- a/<path>`,
/// `+++ b/<path>` and hunks (`@@ -a,b +c,d @@`) with [`CONTEXT_LINES`] lines of
/// context, the way `git diff` prints it. `None` means the file does not exist
/// on that side. Returns an empty string when nothing changed.
///
/// The line diff is a Myers diff over the whole file, so it finds the minimal
/// set of changed lines wherever they are. Counts taken from this output equal
/// `git diff --numstat` for the same two texts. (An earlier version trimmed the
/// common head and tail and then ran a quadratic LCS on the middle, giving up
/// above a size limit and reporting the whole middle as replaced. A file with a
/// small edit near the top and another near the bottom has a huge middle, so a
/// 370 line change was recorded as thousands.)
pub fn render_file_diff(path: &str, old: Option<&str>, new: Option<&str>) -> String {
    if old.is_some() && new.is_some() && old == new {
        return String::new();
    }
    let mut out = format!(
        "--- {}\n+++ {}\n",
        if old.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".to_string()
        },
        if new.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".to_string()
        }
    );
    let diff = similar::TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_lines(old.unwrap_or(""), new.unwrap_or(""));
    for group in diff.grouped_ops(CONTEXT_LINES) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let (old_start, old_end) = (first.old_range().start, last.old_range().end);
        let (new_start, new_end) = (first.new_range().start, last.new_range().end);
        let (old_len, new_len) = (old_end - old_start, new_end - new_start);
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            hunk_range(old_start, old_len),
            hunk_range(new_start, new_len)
        ));
        for op in &group {
            for change in diff.iter_changes(op) {
                out.push(match change.tag() {
                    similar::ChangeTag::Equal => ' ',
                    similar::ChangeTag::Delete => '-',
                    similar::ChangeTag::Insert => '+',
                });
                out.push_str(change.value());
                if change.missing_newline() {
                    out.push_str("\n\\ No newline at end of file");
                }
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// `start,len` as git prints it: 1-based, except an empty range which names the
/// line before it. A length of one is still written out (always `a,b`).
fn hunk_range(start0: usize, len: usize) -> String {
    let start = if len == 0 { start0 } else { start0 + 1 };
    format!("{start},{len}")
}

/// Size of a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffStats {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

/// The changed lines of one file in a stored diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Hunks (`@@`) seen for this file. Zero with changed lines means the file
    /// was stored in the old changed-lines-only format.
    pub hunks: usize,
}

/// A stored diff read back into per-file changed lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDiff {
    pub files: Vec<FileChange>,
}

impl ParsedDiff {
    /// `true` when every changed file carries hunk headers, so the counts are
    /// exactly what the diff says. `false` for the old changed-lines-only format,
    /// whose counts were inflated by the old differ.
    pub fn has_context(&self) -> bool {
        self.files
            .iter()
            .all(|f| f.hunks > 0 || (f.added.is_empty() && f.removed.is_empty()))
    }

    pub fn stats(&self) -> DiffStats {
        stats_of(&self.files)
    }

    /// Rebuild the real changes of a changed-lines-only diff. The old differ either
    /// found the minimal edit of the middle of a file or, above its size limit, listed
    /// the whole middle as removed then added. Diffing the removed lines against the
    /// added lines again therefore recovers the minimal edit in both cases (the common
    /// head and tail it trimmed are common to both sides and cannot change it). Files
    /// that already have hunks are kept as they are: re-diffing across separate hunks
    /// could pair up lines that moved and give a smaller count than git.
    pub fn reconstructed(&self) -> Vec<FileChange> {
        self.files
            .iter()
            .map(|f| {
                if f.hunks > 0 || f.added.is_empty() || f.removed.is_empty() {
                    return f.clone();
                }
                let old: String = f.removed.iter().map(|l| format!("{l}\n")).collect();
                let new: String = f.added.iter().map(|l| format!("{l}\n")).collect();
                let diff = similar::TextDiff::configure()
                    .algorithm(similar::Algorithm::Myers)
                    .diff_lines(&old, &new);
                let mut out = FileChange {
                    path: f.path.clone(),
                    hunks: 1,
                    ..Default::default()
                };
                for c in diff.iter_all_changes() {
                    let text = c.value().trim_end_matches('\n').to_string();
                    match c.tag() {
                        similar::ChangeTag::Delete => out.removed.push(text),
                        similar::ChangeTag::Insert => out.added.push(text),
                        similar::ChangeTag::Equal => {}
                    }
                }
                out
            })
            .collect()
    }
}

fn stats_of(files: &[FileChange]) -> DiffStats {
    DiffStats {
        files: files.len(),
        added: files.iter().map(|f| f.added.len()).sum(),
        removed: files.iter().map(|f| f.removed.len()).sum(),
    }
}

/// Parse a stored diff: either a unified diff with hunks or the old
/// changed-lines-only format (`---`/`+++` headers then bare `-`/`+` lines).
/// Hunk bodies are read by their line counts, so a removed line whose text starts
/// with `-- ` is not mistaken for a file header.
pub fn parse_diff(diff: &str) -> ParsedDiff {
    let lines: Vec<&str> = diff.lines().collect();
    let mut files: Vec<FileChange> = Vec::new();
    let (mut rem_old, mut rem_new) = (0usize, 0usize);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if rem_old > 0 || rem_new > 0 {
            match line.chars().next() {
                Some('-') => {
                    if let Some(f) = files.last_mut() {
                        f.removed.push(line[1..].to_string());
                    }
                    rem_old = rem_old.saturating_sub(1);
                }
                Some('+') => {
                    if let Some(f) = files.last_mut() {
                        f.added.push(line[1..].to_string());
                    }
                    rem_new = rem_new.saturating_sub(1);
                }
                Some('\\') => {}
                // A context line (a blank one may have lost its leading space).
                _ => {
                    rem_old = rem_old.saturating_sub(1);
                    rem_new = rem_new.saturating_sub(1);
                }
            }
            i += 1;
            continue;
        }
        if line.starts_with("--- ") && lines.get(i + 1).is_some_and(|l| l.starts_with("+++ ")) {
            let minus = line[4..].trim();
            let plus = lines[i + 1][4..].trim();
            let path = if plus == "/dev/null" { minus } else { plus };
            let path = path
                .strip_prefix("b/")
                .or_else(|| path.strip_prefix("a/"))
                .unwrap_or(path);
            files.push(FileChange {
                path: path.to_string(),
                ..Default::default()
            });
            i += 2;
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            if let Some((a, b)) = parse_hunk_header(rest) {
                rem_old = a;
                rem_new = b;
                if let Some(f) = files.last_mut() {
                    f.hunks += 1;
                }
            }
            i += 1;
            continue;
        }
        // Old format: bare changed lines under a file header.
        if let Some(f) = files.last_mut() {
            if f.hunks == 0 {
                match line.chars().next() {
                    Some('-') => f.removed.push(line[1..].to_string()),
                    Some('+') => f.added.push(line[1..].to_string()),
                    _ => {}
                }
            }
        }
        i += 1;
    }
    ParsedDiff { files }
}

/// `-a,b +c,d @@ ...` to `(b, d)`; a missing count means one line.
fn parse_hunk_header(rest: &str) -> Option<(usize, usize)> {
    let mut parts = rest.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let count = |s: &str| -> Option<usize> {
        match s.split_once(',') {
            Some((_, n)) => n.parse().ok(),
            None => s.parse::<usize>().ok().map(|_| 1),
        }
    };
    Some((count(old)?, count(new)?))
}

/// Count files and changed lines in a stored diff. For a diff in the old
/// changed-lines-only format this counts what that format recorded, which was
/// often far too high; see [`ParsedDiff::has_context`].
pub fn diff_stats(diff: &str) -> DiffStats {
    parse_diff(diff).stats()
}

/// Changed lines keyed by file, side and normalized text. Trailing whitespace
/// and lines that are only whitespace are ignored so formatting noise does
/// not count as a difference.
fn change_multiset(files: &[FileChange]) -> HashMap<String, usize> {
    let mut set = HashMap::new();
    for f in files {
        for (side, lines) in [('-', &f.removed), ('+', &f.added)] {
            for text in lines {
                let text = text.trim_end();
                if text.trim().is_empty() {
                    continue;
                }
                *set.entry(format!("{}\u{0}{side}{text}", f.path))
                    .or_insert(0) += 1;
            }
        }
    }
    set
}

fn similarity_of(a: &[FileChange], b: &[FileChange]) -> f64 {
    let (ma, mb) = (change_multiset(a), change_multiset(b));
    if ma.is_empty() && mb.is_empty() {
        return 1.0;
    }
    let (mut inter, mut union) = (0usize, 0usize);
    for (k, &ca) in &ma {
        let cb = mb.get(k).copied().unwrap_or(0);
        inter += ca.min(cb);
        union += ca.max(cb);
    }
    for (k, &cb) in &mb {
        if !ma.contains_key(k) {
            union += cb;
        }
    }
    if union == 0 {
        1.0
    } else {
        inter as f64 / union as f64
    }
}

/// How alike two diffs are, 0.0 to 1.0: the size of the changed lines they
/// share over the size of all changed lines either has (weighted Jaccard, per
/// file and per side). Two empty diffs are identical.
pub fn diff_similarity(a: &str, b: &str) -> f64 {
    similarity_of(&parse_diff(a).files, &parse_diff(b).files)
}

/// What recomputing a stored diff's numbers did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecomputedStats {
    pub files_changed: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
    /// Similarity to the baseline diff. `None` for the baseline itself.
    #[serde(default)]
    pub similarity: Option<f64>,
    /// How the numbers were obtained, in words.
    pub method: String,
    pub computed_at: DateTime<Utc>,
}

/// Bytes of each of stdout and stderr kept in a [`CheckRun`].
pub const CHECK_TAIL_BYTES: usize = 4096;

/// Passed, failed and ignored test counts summed over every `test result:` line
/// of cargo test output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TestCounts {
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
}

/// The full record of one run of the check command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckRun {
    /// The exact command line given to the shell.
    pub command: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// `None` when the process was ended by a signal.
    pub exit_code: Option<i32>,
    pub passed: bool,
    /// Last [`CHECK_TAIL_BYTES`] bytes of stdout.
    pub stdout_tail: String,
    /// Last [`CHECK_TAIL_BYTES`] bytes of stderr.
    pub stderr_tail: String,
    /// Parsed from the full output (before tailing) when it looks like cargo test output.
    #[serde(default)]
    pub tests: Option<TestCounts>,
}

impl CheckRun {
    pub fn from_output(
        command: &str,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) -> Self {
        let out = String::from_utf8_lossy(stdout);
        let err = String::from_utf8_lossy(stderr);
        let tests = parse_test_counts(&out).or_else(|| parse_test_counts(&err));
        CheckRun {
            command: command.to_string(),
            started_at,
            finished_at,
            exit_code,
            passed: exit_code == Some(0),
            stdout_tail: tail(&out, CHECK_TAIL_BYTES),
            stderr_tail: tail(&err, CHECK_TAIL_BYTES),
            tests,
        }
    }
}

/// The last `max` bytes of `s`, cut on a character boundary.
pub fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Sum the counts on every `test result: ... N passed; N failed; N ignored` line.
/// `None` when no such line is present.
pub fn parse_test_counts(output: &str) -> Option<TestCounts> {
    let mut total = TestCounts::default();
    let mut seen = false;
    for line in output.lines() {
        let Some(rest) = line.trim_start().strip_prefix("test result:") else {
            continue;
        };
        seen = true;
        for part in rest.split([';', '.']) {
            let mut words = part.split_whitespace();
            let (Some(n), Some(kind)) = (words.next(), words.next()) else {
                continue;
            };
            let Ok(n) = n.parse::<u64>() else { continue };
            match kind {
                "passed" => total.passed += n,
                "failed" => total.failed += n,
                "ignored" => total.ignored += n,
                _ => {}
            }
        }
    }
    seen.then_some(total)
}

// ── Storage ─────────────────────────────────────────────────────────────

/// Reads and writes `.ta/golden`.
pub struct GoldenStore {
    root: PathBuf,
}

impl GoldenStore {
    pub fn new(project_root: &Path) -> Self {
        Self {
            root: golden_dir(project_root),
        }
    }

    pub fn entry_dir(&self, id: &str) -> anyhow::Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(id))
    }

    pub fn exists(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self.entry_dir(id)?.join("entry.json").exists())
    }

    /// Write `entry.json`, `prompt.md` and `baseline.diff`. Refuses to
    /// overwrite an existing entry.
    pub fn save_entry(
        &self,
        entry: &GoldenEntry,
        prompt: &str,
        baseline_diff: &str,
    ) -> anyhow::Result<PathBuf> {
        let dir = self.entry_dir(&entry.id)?;
        if dir.join("entry.json").exists() {
            anyhow::bail!(
                "golden entry '{}' already exists at {}. Remove that directory to capture it again.",
                entry.id,
                dir.display()
            );
        }
        std::fs::create_dir_all(&dir).map_err(|e| {
            anyhow::anyhow!(
                "could not create {}: {e}. Check the directory is writable.",
                dir.display()
            )
        })?;
        write_file(&dir.join("prompt.md"), prompt)?;
        write_file(&dir.join("baseline.diff"), baseline_diff)?;
        write_file(
            &dir.join("entry.json"),
            &serde_json::to_string_pretty(entry)?,
        )?;
        Ok(dir)
    }

    /// Rewrite `entry.json` of an existing entry (used by `ta golden annotate`).
    pub fn update_entry(&self, entry: &GoldenEntry) -> anyhow::Result<()> {
        let dir = self.entry_dir(&entry.id)?;
        write_file(
            &dir.join("entry.json"),
            &serde_json::to_string_pretty(entry)?,
        )
    }

    pub fn load_entry(&self, id: &str) -> anyhow::Result<GoldenEntry> {
        let path = self.entry_dir(id)?.join("entry.json");
        let text = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "no golden entry '{id}' (could not read {}: {e}). Run `ta golden list` to see what exists, \
                 or `ta golden capture <goal-id>` to create one.",
                path.display()
            )
        })?;
        serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "{} is not a valid golden entry: {e}. Fix or remove that file.",
                path.display()
            )
        })
    }

    pub fn load_prompt(&self, id: &str) -> anyhow::Result<String> {
        let path = self.entry_dir(id)?.join("prompt.md");
        std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))
    }

    pub fn prompt_path(&self, id: &str) -> anyhow::Result<PathBuf> {
        Ok(self.entry_dir(id)?.join("prompt.md"))
    }

    pub fn load_baseline_diff(&self, id: &str) -> anyhow::Result<String> {
        let path = self.entry_dir(id)?.join("baseline.diff");
        std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))
    }

    /// Every entry, oldest capture first. An unreadable entry is returned as
    /// an error string so one bad file does not hide the rest.
    pub fn list(&self) -> anyhow::Result<(Vec<GoldenEntry>, Vec<String>)> {
        let mut entries = Vec::new();
        let mut problems = Vec::new();
        let read = match std::fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((entries, problems)),
            Err(e) => anyhow::bail!("could not read {}: {e}", self.root.display()),
        };
        for item in read.flatten() {
            if !item.path().is_dir() {
                continue;
            }
            let name = item.file_name().to_string_lossy().to_string();
            match self.load_entry(&name) {
                Ok(e) => entries.push(e),
                Err(e) => problems.push(format!("{name}: {e}")),
            }
        }
        entries.sort_by(|a, b| {
            a.baseline
                .captured_at
                .cmp(&b.baseline.captured_at)
                .then(a.id.cmp(&b.id))
        });
        Ok((entries, problems))
    }

    pub fn save_replay(
        &self,
        record: &ReplayRecord,
        diff: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let dir = self.entry_dir(&record.golden_id)?.join("replays");
        std::fs::create_dir_all(&dir).map_err(|e| {
            anyhow::anyhow!(
                "could not create {}: {e}. Check the directory is writable.",
                dir.display()
            )
        })?;
        validate_id(&record.replay_id)?;
        let path = dir.join(format!("{}.json", record.replay_id));
        let diff_path = dir.join(format!("{}.diff", record.replay_id));
        // Results are keyed by the unique replay id and never replaced: a collision
        // would silently lose a measurement, so it is an error instead.
        for existing in [&path, &diff_path] {
            if existing.exists() {
                anyhow::bail!(
                    "{} already exists. Replay results are never overwritten; replay id '{}' must be unique. \
                     Keep the existing file and investigate why the id was reused.",
                    existing.display(),
                    record.replay_id
                );
            }
        }
        if let Some(d) = diff {
            write_new_file(&diff_path, d)?;
        }
        write_new_file(&path, &serde_json::to_string_pretty(record)?)?;
        Ok(path)
    }

    /// Rewrite an existing replay record (used by `ta golden recompute`, which only
    /// adds new fields). Fails if the record does not exist.
    pub fn update_replay(&self, record: &ReplayRecord) -> anyhow::Result<()> {
        validate_id(&record.replay_id)?;
        let path = self
            .entry_dir(&record.golden_id)?
            .join("replays")
            .join(format!("{}.json", record.replay_id));
        if !path.exists() {
            anyhow::bail!(
                "{} does not exist, so it cannot be updated.",
                path.display()
            );
        }
        write_file(&path, &serde_json::to_string_pretty(record)?)
    }

    /// The stored diff of a replay, if one was saved.
    pub fn load_replay_diff(&self, id: &str, replay_id: &str) -> anyhow::Result<Option<String>> {
        validate_id(replay_id)?;
        let path = self
            .entry_dir(id)?
            .join("replays")
            .join(format!("{replay_id}.diff"));
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => anyhow::bail!("could not read {}: {e}", path.display()),
        }
    }

    /// The `run_index` the next replay of `id` should get.
    pub fn next_run_index(&self, id: &str) -> anyhow::Result<u32> {
        let replays = self.list_replays(id)?;
        let highest = replays
            .iter()
            .filter_map(|r| r.run_index)
            .max()
            .unwrap_or(0);
        Ok(highest.max(replays.len() as u32) + 1)
    }

    /// Replays of an entry, oldest first.
    pub fn list_replays(&self, id: &str) -> anyhow::Result<Vec<ReplayRecord>> {
        let dir = self.entry_dir(id)?.join("replays");
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => anyhow::bail!("could not read {}: {e}", dir.display()),
        };
        let mut out = Vec::new();
        for item in read.flatten() {
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("could not read {}: {e}", path.display()))?;
            match serde_json::from_str::<ReplayRecord>(&text) {
                Ok(r) => out.push(r),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable golden replay record")
                }
            }
        }
        out.sort_by_key(|r| r.started_at);
        Ok(out)
    }

    /// Number of replays per entry id, for `ta golden list`.
    pub fn replay_counts(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        if let Ok((entries, _)) = self.list() {
            for e in entries {
                let n = self.list_replays(&e.id).map(|r| r.len()).unwrap_or(0);
                counts.insert(e.id, n);
            }
        }
        counts
    }
}

// ── Recompute ───────────────────────────────────────────────────────────

/// What `recompute_entry` did, or would do, for the baseline or one replay.
#[derive(Debug, Clone, PartialEq)]
pub struct RecomputeItem {
    /// `baseline` or `replay <tag> (<id>)`.
    pub subject: String,
    /// The numbers as originally recorded: files, added, removed, similarity.
    pub original: (usize, Option<usize>, Option<usize>, Option<f64>),
    pub outcome: RecomputeOutcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecomputeOutcome {
    /// New numbers computed (written unless it was a dry run).
    Recomputed(RecomputedStats),
    /// Already carries identical recomputed numbers.
    Unchanged,
    /// Could not be recomputed; says why and what to do. Nothing was guessed.
    Flagged(String),
}

type Effective = (Vec<FileChange>, &'static str);

/// The changed lines to count for a stored diff, and how they were obtained.
fn effective_changes(diff: &str, reconstruct: bool) -> Result<Effective, String> {
    let parsed = parse_diff(diff);
    if parsed.has_context() {
        return Ok((parsed.files, "exact, from the stored unified diff"));
    }
    if reconstruct {
        return Ok((
            parsed.reconstructed(),
            "reconstructed by re-diffing the stored removed lines against the added lines",
        ));
    }
    Err(
        "the stored diff has no context (it is in the old changed-lines-only format), so the \
         true counts cannot be read from it exactly. Re-run with --reconstruct to rebuild them \
         from the changed lines (an estimate, labelled as such), or replay again."
            .to_string(),
    )
}

/// Recompute files, lines and similarity for the baseline and every replay of
/// entry `id` from their stored diffs, writing the results to the new `recomputed`
/// fields. The originally recorded numbers are never changed. With `dry_run`
/// nothing is written. A diff that cannot be recomputed is flagged, not guessed.
pub fn recompute_entry(
    store: &GoldenStore,
    id: &str,
    reconstruct: bool,
    dry_run: bool,
) -> anyhow::Result<Vec<RecomputeItem>> {
    let mut entry = store.load_entry(id)?;
    let mut items = Vec::new();
    let now = Utc::now();
    let baseline_diff = store.load_baseline_diff(id).ok();
    let baseline_eff = match &baseline_diff {
        Some(d) => effective_changes(d, reconstruct),
        None => Err("baseline.diff is missing".to_string()),
    };

    // Baseline.
    let b = &entry.baseline;
    let original = (
        b.files_changed,
        Some(b.lines_added),
        Some(b.lines_removed),
        None,
    );
    let outcome = match &baseline_eff {
        Err(why) => RecomputeOutcome::Flagged(why.clone()),
        Ok((files, method)) => {
            let s = stats_of(files);
            let fresh = RecomputedStats {
                files_changed: s.files,
                lines_added: s.added,
                lines_removed: s.removed,
                similarity: None,
                method: (*method).to_string(),
                computed_at: now,
            };
            if same_numbers(b.recomputed.as_ref(), &fresh) {
                RecomputeOutcome::Unchanged
            } else {
                entry.baseline.recomputed = Some(fresh.clone());
                RecomputeOutcome::Recomputed(fresh)
            }
        }
    };
    let baseline_changed = matches!(outcome, RecomputeOutcome::Recomputed(_));
    items.push(RecomputeItem {
        subject: "baseline".to_string(),
        original,
        outcome,
    });
    if baseline_changed && !dry_run {
        store.update_entry(&entry)?;
    }

    // Replays.
    for mut r in store.list_replays(id)? {
        let original = (
            r.files_changed,
            r.lines_added,
            r.lines_removed,
            r.similarity,
        );
        let subject = format!("replay {} ({})", r.tag, r.replay_id);
        let diff = store.load_replay_diff(id, &r.replay_id)?;
        let outcome = match diff {
            None => RecomputeOutcome::Flagged(
                "no stored diff for this replay (it produced none, or the file was removed)"
                    .to_string(),
            ),
            Some(d) => match effective_changes(&d, reconstruct) {
                Err(why) => RecomputeOutcome::Flagged(why),
                Ok((files, method)) => {
                    let s = stats_of(&files);
                    let (similarity, method) = match &baseline_eff {
                        Ok((bfiles, bmethod)) => (
                            Some(similarity_of(bfiles, &files)),
                            format!("{method}; similarity against the baseline: {bmethod}"),
                        ),
                        Err(why) => (
                            None,
                            format!("{method}; similarity not recomputed because the baseline could not be: {why}"),
                        ),
                    };
                    let fresh = RecomputedStats {
                        files_changed: s.files,
                        lines_added: s.added,
                        lines_removed: s.removed,
                        similarity,
                        method,
                        computed_at: now,
                    };
                    if same_numbers(r.recomputed.as_ref(), &fresh) {
                        RecomputeOutcome::Unchanged
                    } else {
                        r.recomputed = Some(fresh.clone());
                        if !dry_run {
                            store.update_replay(&r)?;
                        }
                        RecomputeOutcome::Recomputed(fresh)
                    }
                }
            },
        };
        items.push(RecomputeItem {
            subject,
            original,
            outcome,
        });
    }
    Ok(items)
}

fn same_numbers(existing: Option<&RecomputedStats>, fresh: &RecomputedStats) -> bool {
    existing.is_some_and(|e| {
        e.files_changed == fresh.files_changed
            && e.lines_added == fresh.lines_added
            && e.lines_removed == fresh.lines_removed
            && e.similarity == fresh.similarity
            && e.method == fresh.method
    })
}

// ── Arm statistics ──────────────────────────────────────────────────────

/// Mean, min, max and sample count of a series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SeriesStats {
    pub n: usize,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
}

impl SeriesStats {
    /// `None` for an empty series.
    pub fn of(values: &[f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        Some(Self {
            n: values.len(),
            mean: values.iter().sum::<f64>() / values.len() as f64,
            min: values.iter().copied().fold(f64::INFINITY, f64::min),
            max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        })
    }
}

/// Replays that ran under the same settings.
#[derive(Debug, Clone, Serialize)]
pub struct ArmStats {
    /// The recorded optimizer settings, or the label when none were recorded.
    pub arm: String,
    /// The labels (tags) the replays carried.
    pub tags: Vec<String>,
    pub tokens: Option<SeriesStats>,
    pub output_tokens: Option<SeriesStats>,
    /// Replays with no recorded cost are left out, so `cost.n` can be below the replay count.
    pub cost_usd: Option<SeriesStats>,
}

/// Total tokens of a replay (input, output and both cache kinds).
pub fn replay_total_tokens(r: &ReplayRecord) -> u64 {
    r.input_tokens + r.output_tokens + r.cache_read_tokens + r.cache_creation_tokens
}

/// The arm a replay belongs to: its recorded optimizer settings when it has them,
/// otherwise its label.
pub fn arm_key(r: &ReplayRecord) -> String {
    match &r.environment {
        Some(e) => format!(
            "input={} context={} output={} compression={}",
            e.input_slot,
            e.context_slot,
            e.output_slot,
            if e.compression_enabled { "on" } else { "off" }
        ),
        None => format!("label {}", r.tag),
    }
}

/// Group replays by arm and summarise tokens (primary) and cost (secondary).
/// Replays that failed to produce a measurement (no goal) are left out.
pub fn arm_stats(replays: &[ReplayRecord]) -> Vec<ArmStats> {
    let mut groups: BTreeMap<String, Vec<&ReplayRecord>> = BTreeMap::new();
    for r in replays.iter().filter(|r| r.goal_id.is_some()) {
        groups.entry(arm_key(r)).or_default().push(r);
    }
    groups
        .into_iter()
        .map(|(arm, rs)| {
            let tokens: Vec<f64> = rs.iter().map(|r| replay_total_tokens(r) as f64).collect();
            let out: Vec<f64> = rs.iter().map(|r| r.output_tokens as f64).collect();
            let cost: Vec<f64> = rs
                .iter()
                .filter(|r| r.cost_usd > 0.0)
                .map(|r| r.cost_usd)
                .collect();
            let mut tags: Vec<String> = rs.iter().map(|r| r.tag.clone()).collect();
            tags.dedup();
            ArmStats {
                arm,
                tags,
                tokens: SeriesStats::of(&tokens),
                output_tokens: SeriesStats::of(&out),
                cost_usd: SeriesStats::of(&cost),
            }
        })
        .collect()
}

fn write_new_file(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            anyhow::anyhow!(
                "could not create {}: {e}. Existing results are never overwritten; check the directory is writable.",
                path.display()
            )
        })?;
    f.write_all(text.as_bytes())
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", path.display()))
}

fn write_file(path: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, text).map_err(|e| {
        anyhow::anyhow!(
            "could not write {}: {e}. Check the directory is writable.",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> Baseline {
        Baseline {
            cost_usd: 1.5,
            cost_estimated: false,
            input_tokens: 100,
            output_tokens: 200,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            turns: None,
            outcome: "applied".into(),
            files_changed: 1,
            lines_added: 1,
            lines_removed: 1,
            diff_sha256: "x".into(),
            check_passed: None,
            check_run: None,
            recomputed: None,
            captured_at: Utc::now(),
        }
    }

    fn entry(check: Option<&str>) -> GoldenEntry {
        GoldenEntry {
            id: "fix-bug-abcd1234".into(),
            title: "Fix bug".into(),
            source_goal_id: "abcd1234-0000-0000-0000-000000000000".into(),
            start_commit: "a".repeat(40),
            start_dirty: false,
            start_branch: Some("main".into()),
            prompt_sha256: sha256_hex("p"),
            agent_id: "claude-code".into(),
            agent_model: String::new(),
            check_command: check.map(str::to_string),
            baseline: baseline(),
            baseline_environment: None,
        }
    }

    fn replay(similarity: Option<f64>, check: Option<bool>) -> ReplayRecord {
        ReplayRecord {
            replay_id: "r1".into(),
            golden_id: "fix-bug-abcd1234".into(),
            tag: "t".into(),
            set: vec![],
            goal_id: Some("g".into()),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            outcome: "pr_ready".into(),
            cost_usd: 1.0,
            cost_estimated: false,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            turns: None,
            aborted_budget: false,
            files_changed: 1,
            lines_added: None,
            lines_removed: None,
            diff_sha256: Some("y".into()),
            similarity,
            check_passed: check,
            check_run: None,
            run_index: None,
            recomputed: None,
            failure: None,
            environment: None,
        }
    }

    #[test]
    fn verdict_equivalent_when_check_passes_and_diff_is_close() {
        let v = verdict(&entry(Some("true")), &replay(Some(0.95), Some(true)));
        assert_eq!(v.verdict, Verdict::Equivalent);
        assert!(!v.needs_human_review());
    }

    #[test]
    fn verdict_differs_when_diff_is_far() {
        let v = verdict(&entry(Some("true")), &replay(Some(0.3), Some(true)));
        assert_eq!(v.verdict, Verdict::Differs);
        assert!(v.needs_human_review());
        assert!(v.reasons[0].contains("30%"));
    }

    #[test]
    fn verdict_failed_when_check_fails_even_with_identical_diff() {
        let v = verdict(&entry(Some("make test")), &replay(Some(1.0), Some(false)));
        assert_eq!(v.verdict, Verdict::Failed);
        assert!(v.reasons.iter().any(|r| r.contains("make test")));
    }

    #[test]
    fn verdict_failed_on_budget_abort_missing_goal_or_failed_state() {
        let mut r = replay(Some(1.0), Some(true));
        r.aborted_budget = true;
        assert_eq!(verdict(&entry(None), &r).verdict, Verdict::Failed);

        let mut r = replay(None, None);
        r.failure = Some("ta run created no goal".into());
        r.goal_id = None;
        assert_eq!(verdict(&entry(None), &r).verdict, Verdict::Failed);

        let mut r = replay(Some(1.0), None);
        r.outcome = "failed".into();
        assert_eq!(verdict(&entry(None), &r).verdict, Verdict::Failed);
    }

    #[test]
    fn verdict_failed_when_no_diff_could_be_compared() {
        let v = verdict(&entry(None), &replay(None, None));
        assert_eq!(v.verdict, Verdict::Failed);
    }

    #[test]
    fn verdict_without_check_is_equivalent_but_says_so() {
        let v = verdict(&entry(None), &replay(Some(0.9), None));
        assert_eq!(v.verdict, Verdict::Equivalent);
        assert!(v.reasons.iter().any(|r| r.contains("no check command")));
    }

    #[test]
    fn threshold_is_inclusive() {
        let v = verdict(
            &entry(Some("t")),
            &replay(Some(EQUIVALENCE_THRESHOLD), Some(true)),
        );
        assert_eq!(v.verdict, Verdict::Equivalent);
    }

    #[test]
    fn render_diff_is_a_unified_diff_with_context_and_hunk_headers() {
        let d = render_file_diff("a.txt", Some("one\ntwo\nthree\n"), Some("one\n2\nthree\n"));
        assert_eq!(
            d,
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        );
        let created = render_file_diff("n.txt", None, Some("x\ny\n"));
        assert_eq!(
            created,
            "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n"
        );
        let deleted = render_file_diff("d.txt", Some("x\n"), None);
        assert_eq!(deleted, "--- a/d.txt\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-x\n");
        assert_eq!(
            render_file_diff("s.txt", Some("same\n"), Some("same\n")),
            ""
        );
    }

    #[test]
    fn render_diff_finds_common_lines_in_the_middle() {
        let old = "h\na\nb\nc\nd\nt\n";
        let new = "h\nb\nX\nd\nt\n";
        let d = render_file_diff("f", Some(old), Some(new));
        let s = diff_stats(&d);
        assert_eq!((s.files, s.added, s.removed), (1, 1, 2));
        assert!(parse_diff(&d).has_context());
    }

    #[test]
    fn removed_line_that_looks_like_a_header_is_still_a_removed_line() {
        // A removed SQL comment "-- x" renders as "--- x"; it must not start a new file.
        let d = render_file_diff("q.sql", Some("a\n-- x\nb\n"), Some("a\nb\n"));
        assert!(d.contains("\n--- x\n"), "{d}");
        let s = diff_stats(&d);
        assert_eq!((s.files, s.added, s.removed), (1, 0, 1));
    }

    #[test]
    fn missing_newline_at_end_of_file_is_a_change_like_git_reports() {
        let d = render_file_diff("f", Some("a\nb"), Some("a\nb\n"));
        assert!(d.contains("\\ No newline at end of file"), "{d}");
        let s = diff_stats(&d);
        assert_eq!((s.added, s.removed), (1, 1));
    }

    #[test]
    fn similarity_identical_disjoint_and_partial() {
        let a = render_file_diff("f", Some("a\nb\n"), Some("a\nc\n"));
        assert_eq!(diff_similarity(&a, &a), 1.0);
        let other = render_file_diff("g", Some("a\nb\n"), Some("a\nc\n"));
        assert_eq!(
            diff_similarity(&a, &other),
            0.0,
            "same text in another file is a different change"
        );
        let partial = format!(
            "{}{}",
            render_file_diff("f", Some("a\nb\nx\n"), Some("a\nc\ny\n")),
            ""
        );
        let sim = diff_similarity(&a, &partial);
        assert!(sim > 0.0 && sim < 1.0, "{sim}");
        assert_eq!(diff_similarity("", ""), 1.0);
        assert_eq!(diff_similarity(&a, ""), 0.0);
    }

    #[test]
    fn similarity_ignores_trailing_whitespace_and_blank_lines() {
        let a = "--- a/f\n+++ b/f\n+foo\n+\n";
        let b = "--- a/f\n+++ b/f\n+foo   \n";
        assert_eq!(diff_similarity(a, b), 1.0);
    }

    #[test]
    fn ids_are_validated_and_generated_safely() {
        assert!(validate_id("fix-bug-1a2b3c4d").is_ok());
        for bad in [
            "",
            "..",
            "../x",
            "a/b",
            "A",
            ".hidden",
            "a b",
            &"x".repeat(65),
        ] {
            assert!(validate_id(bad).is_err(), "{bad}");
        }
        let id = default_entry_id("Fix: the  Bug!!", "abcd1234-5678-0000-0000-000000000000");
        assert_eq!(id, "fix-the-bug-abcd1234");
        assert!(validate_id(&id).is_ok());
        assert_eq!(default_entry_id("???", "abcd1234-0"), "goal-abcd1234");
        let long = default_entry_id(&"word ".repeat(40), "abcd1234-0");
        assert!(validate_id(&long).is_ok());
    }

    #[test]
    fn parse_overrides_types_and_errors() {
        let v = parse_overrides(&[
            "optimizers.input.use=none".into(),
            "n=3".into(),
            "flag=true".into(),
        ])
        .unwrap();
        assert_eq!(v["optimizers.input.use"], "none");
        assert_eq!(v["n"], 3);
        assert_eq!(v["flag"], true);
        assert!(parse_overrides(&["novalue".into()]).is_err());
        assert!(parse_overrides(&["=x".into()]).is_err());
        assert_eq!(parse_overrides(&[]).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn store_round_trip_list_and_refuse_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = GoldenStore::new(dir.path());
        assert!(store.list().unwrap().0.is_empty());
        let e = entry(Some("cargo test"));
        let path = store
            .save_entry(&e, "do it", "--- a/f\n+++ b/f\n+x\n")
            .unwrap();
        assert!(path.join("entry.json").exists());
        assert!(path.join("prompt.md").exists());
        assert!(path.join("baseline.diff").exists());
        assert_eq!(store.load_entry(&e.id).unwrap(), e);
        assert_eq!(store.load_prompt(&e.id).unwrap(), "do it");
        assert!(store.save_entry(&e, "again", "").is_err());

        let r = replay(Some(1.0), None);
        store.save_replay(&r, Some("d")).unwrap();
        let replays = store.list_replays(&e.id).unwrap();
        assert_eq!(replays.len(), 1);
        assert_eq!(store.replay_counts()[&e.id], 1);

        let (entries, problems) = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(problems.is_empty());
    }

    #[test]
    fn load_entry_error_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let err = GoldenStore::new(dir.path())
            .load_entry("nope")
            .unwrap_err()
            .to_string();
        assert!(err.contains("ta golden list"), "{err}");
        assert!(GoldenStore::new(dir.path()).load_entry("../x").is_err());
    }

    // ── Diff counts against git ─────────────────────────────────────────

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// A source-like file with a long test module, so the middle between two
    /// edits (one near the top, one near the bottom) is thousands of lines.
    fn big_file(lines: usize) -> String {
        let mut s = String::from("//! header\nuse std::fmt;\n\n");
        for i in 0..lines {
            s.push_str(&format!("fn item_{i}() -> usize {{ {} }}\n", i * 7 % 13));
        }
        s.push_str("#[cfg(test)]\nmod tests {\n    // end\n}\n");
        s
    }

    fn edited(base: &str) -> String {
        base.replacen("use std::fmt;", "use std::fmt;\nuse std::io;", 1)
            .replace(
                "    // end\n",
                "    // end\n    // added one\n    // added two\n",
            )
    }

    /// Commit `old` then `new` in a temp repo and return git's numstat (added, removed).
    fn git_numstat(old: &str, new: &str) -> (usize, usize) {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git_in(d, &["init", "-q"]);
        // Windows git defaults to autocrlf, which rewrites line endings on apply.
        git_in(d, &["config", "core.autocrlf", "false"]);
        git_in(d, &["config", "user.email", "t@example.com"]);
        git_in(d, &["config", "user.name", "t"]);
        git_in(d, &["config", "commit.gpgsign", "false"]);
        std::fs::write(d.join("f.rs"), old).unwrap();
        git_in(d, &["add", "."]);
        git_in(d, &["commit", "-q", "-m", "a"]);
        std::fs::write(d.join("f.rs"), new).unwrap();
        let n = git_in(d, &["diff", "--numstat"]);
        let mut cols = n.split_whitespace();
        (
            cols.next().unwrap().parse().unwrap(),
            cols.next().unwrap().parse().unwrap(),
        )
    }

    /// The differ this module used before: trim head and tail, then a quadratic LCS
    /// that gives up above 4,000,000 cells and reports the middle as replaced.
    /// Kept here only to reproduce the overcounting.
    fn old_render(path: &str, old: &str, new: &str) -> String {
        let (o, n): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
        let head = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
        let tail = o[head..]
            .iter()
            .rev()
            .zip(n[head..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let (om, nm) = (&o[head..o.len() - tail], &n[head..n.len() - tail]);
        assert!(
            om.len() * nm.len() > 4_000_000,
            "fixture must be big enough to trigger the old fallback"
        );
        let mut out = format!("--- a/{path}\n+++ b/{path}\n");
        for l in om {
            out.push_str(&format!("-{l}\n"));
        }
        for l in nm {
            out.push_str(&format!("+{l}\n"));
        }
        out
    }

    #[test]
    fn counts_match_git_numstat_for_a_big_file_with_two_distant_edits() {
        let old = big_file(4000);
        let new = edited(&old);
        let (git_add, git_rem) = git_numstat(&old, &new);
        assert_eq!((git_add, git_rem), (3, 0), "fixture sanity");
        let d = render_file_diff("f.rs", Some(&old), Some(&new));
        let s = diff_stats(&d);
        assert_eq!((s.files, s.added, s.removed), (1, git_add, git_rem));
        assert!(parse_diff(&d).has_context());
        // Context is kept and small: far fewer lines than the file.
        assert!(d.lines().count() < 40, "{} lines", d.lines().count());
        assert!(d.contains("@@ "));
    }

    #[test]
    fn counts_match_git_numstat_for_replacements_and_deletions() {
        let old = big_file(300);
        let new = old
            .replace("item_10()", "renamed_10()")
            .replace("fn item_200() -> usize { 3 }\n", "")
            .replace("item_299", "item_299b");
        let (a, r) = git_numstat(&old, &new);
        let s = diff_stats(&render_file_diff("f.rs", Some(&old), Some(&new)));
        assert_eq!((s.added, s.removed), (a, r));
    }

    #[test]
    fn rendered_diff_applies_with_git_and_reproduces_the_new_file() {
        let old = big_file(500);
        let new = edited(&old).replace("item_42()", "item_42b()");
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git_in(d, &["init", "-q"]);
        // Windows git defaults to autocrlf, which rewrites line endings on apply.
        git_in(d, &["config", "core.autocrlf", "false"]);
        std::fs::write(d.join("f.rs"), &old).unwrap();
        let patch = render_file_diff("f.rs", Some(&old), Some(&new));
        std::fs::write(d.join("p.diff"), &patch).unwrap();
        git_in(d, &["apply", "p.diff"]);
        assert_eq!(std::fs::read_to_string(d.join("f.rs")).unwrap(), new);
    }

    #[test]
    fn golden_failure_old_differ_overcounted_and_the_fix_does_not() {
        // The wiki entry recorded +5809/-5442 for a +370/-2 commit. Same shape, smaller:
        // two small edits far apart in a big file.
        let old = big_file(4000);
        let new = edited(&old);
        let legacy = old_render("f.rs", &old, &new);
        let inflated = diff_stats(&legacy);
        assert!(
            inflated.added > 3000 && inflated.removed > 3000,
            "the old differ should have overcounted, got {inflated:?}"
        );
        let (git_add, git_rem) = git_numstat(&old, &new);
        let fixed = diff_stats(&render_file_diff("f.rs", Some(&old), Some(&new)));
        assert_eq!((fixed.added, fixed.removed), (git_add, git_rem));

        // And the stored legacy diff is recognised as lacking context.
        assert!(!parse_diff(&legacy).has_context());
        // Rebuilding the changes from it recovers git's numbers.
        let rebuilt = stats_of(&parse_diff(&legacy).reconstructed());
        assert_eq!((rebuilt.added, rebuilt.removed), (git_add, git_rem));
    }

    #[test]
    fn crlf_and_lf_text_that_were_normalised_by_the_caller_do_not_differ() {
        assert_eq!(render_file_diff("f", Some("a\nb\n"), Some("a\nb\n")), "");
    }

    // ── Check runs ──────────────────────────────────────────────────────

    #[test]
    fn check_run_keeps_bounded_tails_and_parses_cargo_test_counts() {
        let big = "x".repeat(10_000);
        let out = format!(
            "{big}\ntest result: ok. 12 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.50s\n\
             test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s\n"
        );
        let now = Utc::now();
        let run = CheckRun::from_output("cargo test", now, now, Some(101), out.as_bytes(), b"boom");
        assert!(!run.passed);
        assert_eq!(run.exit_code, Some(101));
        assert!(run.stdout_tail.len() <= CHECK_TAIL_BYTES);
        assert!(run.stdout_tail.contains("2 failed"));
        assert_eq!(run.stderr_tail, "boom");
        assert_eq!(
            run.tests,
            Some(TestCounts {
                passed: 15,
                failed: 2,
                ignored: 1
            })
        );
        let ok = CheckRun::from_output("true", now, now, Some(0), b"", b"");
        assert!(ok.passed);
        assert_eq!(ok.tests, None);
        let sig = CheckRun::from_output("x", now, now, None, b"", b"");
        assert!(!sig.passed, "no exit code (killed by signal) is not a pass");
    }

    #[test]
    fn tail_cuts_on_a_character_boundary() {
        let s = "aé".repeat(3000);
        let t = tail(&s, 4096);
        assert!(t.len() <= 4096);
        assert!(s.ends_with(&t));
    }

    #[test]
    fn replay_record_holds_check_run_and_old_records_still_load() {
        let now = Utc::now();
        let mut r = replay(Some(1.0), Some(true));
        r.check_run = Some(CheckRun::from_output(
            "make test",
            now,
            now,
            Some(0),
            b"ok",
            b"",
        ));
        r.run_index = Some(2);
        let json = serde_json::to_string(&r).unwrap();
        let back: ReplayRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);

        // A record written before any of the new fields existed.
        let old = r#"{"replay_id":"r0","golden_id":"g","tag":"t","started_at":"2026-10-18T19:11:20Z",
            "outcome":"pr_ready","files_changed":6,"similarity":0.8}"#;
        let loaded: ReplayRecord = serde_json::from_str(old).unwrap();
        assert!(loaded.check_run.is_none() && loaded.run_index.is_none());
        assert!(loaded.recomputed.is_none() && loaded.lines_added.is_none());
    }

    #[test]
    fn saving_a_replay_never_overwrites_an_existing_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = GoldenStore::new(dir.path());
        let e = entry(None);
        store.save_entry(&e, "p", "").unwrap();
        let mut a = replay(Some(1.0), Some(true));
        a.replay_id = "run-aaaa".into();
        let path = store.save_replay(&a, Some("diff-a")).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let mut collide = replay(Some(0.1), Some(false));
        collide.replay_id = "run-aaaa".into();
        let err = store
            .save_replay(&collide, Some("diff-b"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("never overwritten"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            store
                .load_replay_diff(&e.id, "run-aaaa")
                .unwrap()
                .as_deref(),
            Some("diff-a")
        );

        // Two different run ids coexist, each with its own record.
        let mut b = replay(Some(0.9), Some(true));
        b.replay_id = "run-bbbb".into();
        store.save_replay(&b, None).unwrap();
        assert_eq!(store.list_replays(&e.id).unwrap().len(), 2);
    }

    #[test]
    fn run_index_counts_up_within_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = GoldenStore::new(dir.path());
        let e = entry(None);
        store.save_entry(&e, "p", "").unwrap();
        assert_eq!(store.next_run_index(&e.id).unwrap(), 1);
        for (i, id) in ["run-a", "run-b"].iter().enumerate() {
            let mut r = replay(None, None);
            r.replay_id = (*id).into();
            r.run_index = Some(store.next_run_index(&e.id).unwrap());
            r.started_at = Utc::now() + chrono::Duration::seconds(i as i64);
            store.save_replay(&r, None).unwrap();
        }
        assert_eq!(store.next_run_index(&e.id).unwrap(), 3);
    }

    // ── Recompute ───────────────────────────────────────────────────────

    fn seeded_store(legacy: bool) -> (tempfile::TempDir, GoldenStore, GoldenEntry, (usize, usize)) {
        let dir = tempfile::tempdir().unwrap();
        let store = GoldenStore::new(dir.path());
        let old = big_file(4000);
        let new = edited(&old);
        let (diff_base, diff_rep) = if legacy {
            (
                old_render("f.rs", &old, &new),
                old_render("f.rs", &old, &new),
            )
        } else {
            (
                render_file_diff("f.rs", Some(&old), Some(&new)),
                render_file_diff("f.rs", Some(&old), Some(&new)),
            )
        };
        let mut e = entry(None);
        e.baseline.files_changed = 1;
        e.baseline.lines_added = 5809;
        e.baseline.lines_removed = 5442;
        store.save_entry(&e, "p", &diff_base).unwrap();
        let mut r = replay(Some(0.5), None);
        r.replay_id = "run-1".into();
        r.files_changed = 1;
        store.save_replay(&r, Some(&diff_rep)).unwrap();
        (dir, store, e, git_numstat(&old, &new))
    }

    #[test]
    fn recompute_flags_context_free_diffs_and_changes_nothing() {
        let (_d, store, e, _) = seeded_store(true);
        let before = store.list_replays(&e.id).unwrap();
        let items = recompute_entry(&store, &e.id, false, false).unwrap();
        assert_eq!(items.len(), 2);
        for i in &items {
            match &i.outcome {
                RecomputeOutcome::Flagged(why) => {
                    assert!(
                        why.contains("no context") && why.contains("--reconstruct"),
                        "{why}"
                    )
                }
                other => panic!("expected a flag, got {other:?}"),
            }
        }
        assert_eq!(store.list_replays(&e.id).unwrap(), before);
        assert_eq!(store.load_entry(&e.id).unwrap().baseline.recomputed, None);
    }

    #[test]
    fn recompute_reconstruct_dry_run_writes_nothing_then_real_run_adds_new_fields() {
        let (_d, store, e, (ga, gr)) = seeded_store(true);
        let dry = recompute_entry(&store, &e.id, true, true).unwrap();
        assert!(dry
            .iter()
            .all(|i| matches!(i.outcome, RecomputeOutcome::Recomputed(_))));
        assert!(store.list_replays(&e.id).unwrap()[0].recomputed.is_none());
        assert!(store
            .load_entry(&e.id)
            .unwrap()
            .baseline
            .recomputed
            .is_none());

        recompute_entry(&store, &e.id, true, false).unwrap();
        let entry_after = store.load_entry(&e.id).unwrap();
        let rc = entry_after.baseline.recomputed.clone().unwrap();
        assert_eq!((rc.lines_added, rc.lines_removed), (ga, gr));
        assert!(rc.method.starts_with("reconstructed"));
        // The original numbers are untouched.
        assert_eq!(entry_after.baseline.lines_added, 5809);
        assert_eq!(entry_after.baseline.lines_removed, 5442);
        let r = &store.list_replays(&e.id).unwrap()[0];
        assert_eq!(r.similarity, Some(0.5), "original similarity is kept");
        let rr = r.recomputed.clone().unwrap();
        assert_eq!((rr.lines_added, rr.lines_removed), (ga, gr));
        assert_eq!(rr.similarity, Some(1.0));

        // A second run changes nothing.
        let again = recompute_entry(&store, &e.id, true, false).unwrap();
        assert!(again
            .iter()
            .all(|i| i.outcome == RecomputeOutcome::Unchanged));
    }

    #[test]
    fn recompute_reads_exact_numbers_from_diffs_that_have_context() {
        let (_d, store, e, (ga, gr)) = seeded_store(false);
        let items = recompute_entry(&store, &e.id, false, false).unwrap();
        for i in &items {
            let RecomputeOutcome::Recomputed(s) = &i.outcome else {
                panic!("{:?}", i.outcome)
            };
            assert_eq!((s.lines_added, s.lines_removed), (ga, gr));
            assert!(s.method.starts_with("exact"), "{}", s.method);
        }
    }

    #[test]
    fn recompute_flags_a_replay_with_no_stored_diff() {
        let (_d, store, e, _) = seeded_store(false);
        let mut r = replay(None, None);
        r.replay_id = "run-2".into();
        store.save_replay(&r, None).unwrap();
        let items = recompute_entry(&store, &e.id, false, true).unwrap();
        let flagged: Vec<_> = items
            .iter()
            .filter(|i| matches!(&i.outcome, RecomputeOutcome::Flagged(w) if w.contains("no stored diff")))
            .collect();
        assert_eq!(flagged.len(), 1);
    }

    // ── Arm statistics ──────────────────────────────────────────────────

    fn arm_replay(id: &str, tag: &str, input_slot: &str, out: u64, cost: f64) -> ReplayRecord {
        let mut r = replay(Some(1.0), None);
        r.replay_id = id.into();
        r.tag = tag.into();
        r.output_tokens = out;
        r.input_tokens = 0;
        r.cost_usd = cost;
        r.environment = Some(RunEnvironment {
            input_slot: input_slot.into(),
            context_slot: "none".into(),
            output_slot: "none".into(),
            ..Default::default()
        });
        r
    }

    #[test]
    fn arm_stats_groups_by_settings_and_reports_mean_min_max_n() {
        let rs = vec![
            arm_replay("a", "x", "rtk", 100, 1.0),
            arm_replay("b", "y", "rtk", 300, 3.0),
            arm_replay("c", "z", "none", 50, 0.0),
        ];
        let arms = arm_stats(&rs);
        assert_eq!(arms.len(), 2);
        let rtk = arms.iter().find(|a| a.arm.contains("input=rtk")).unwrap();
        let t = rtk.tokens.unwrap();
        assert_eq!((t.n, t.mean, t.min, t.max), (2, 200.0, 100.0, 300.0));
        let c = rtk.cost_usd.unwrap();
        assert_eq!((c.n, c.mean, c.min, c.max), (2, 2.0, 1.0, 3.0));
        let none = arms.iter().find(|a| a.arm.contains("input=none")).unwrap();
        assert_eq!(none.tokens.unwrap().n, 1);
        assert!(
            none.cost_usd.is_none(),
            "a replay with no cost is left out of cost stats"
        );
    }
}
