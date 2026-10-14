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

/// Above this many (old lines x new lines) the line diff stops trying to find
/// common lines in the middle of a file and reports the middle as replaced.
const LCS_CELL_LIMIT: usize = 4_000_000;

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
    pub captured_at: DateTime<Utc>,
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
    #[serde(default)]
    pub diff_sha256: Option<String>,
    /// Similarity to the baseline diff, 0.0 to 1.0. `None` when no diff could
    /// be produced.
    #[serde(default)]
    pub similarity: Option<f64>,
    #[serde(default)]
    pub check_passed: Option<bool>,
    /// Why the replay could not be measured, in words the user can act on.
    #[serde(default)]
    pub failure: Option<String>,
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

/// Render the change to one file as `--- a/<path>`, `+++ b/<path>` and
/// `-`/`+` lines, without context or hunk headers. `None` means the file does
/// not exist on that side. Returns an empty string when nothing changed.
pub fn render_file_diff(path: &str, old: Option<&str>, new: Option<&str>) -> String {
    let old_lines: Vec<&str> = old.map(|s| s.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.map(|s| s.lines().collect()).unwrap_or_default();
    if old.is_some() && new.is_some() && old_lines == new_lines {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&format!(
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
    ));
    // Trim the common head and tail, then diff what is left.
    let head = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let tail = old_lines[head..]
        .iter()
        .rev()
        .zip(new_lines[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_mid = &old_lines[head..old_lines.len() - tail];
    let new_mid = &new_lines[head..new_lines.len() - tail];
    for op in diff_middle(old_mid, new_mid) {
        match op {
            Op::Del(l) => {
                out.push('-');
                out.push_str(l);
                out.push('\n');
            }
            Op::Add(l) => {
                out.push('+');
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    out
}

enum Op<'a> {
    Del(&'a str),
    Add(&'a str),
}

fn diff_middle<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Op<'a>> {
    if old.is_empty() || new.is_empty() || old.len().saturating_mul(new.len()) > LCS_CELL_LIMIT {
        return old
            .iter()
            .map(|l| Op::Del(l))
            .chain(new.iter().map(|l| Op::Add(l)))
            .collect();
    }
    let (n, m) = (old.len(), new.len());
    // lcs[i][j] = length of the longest common subsequence of old[i..], new[j..].
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if old[i] == new[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut ops = Vec::new();
    while i < n && j < m {
        if old[i] == new[j] {
            i += 1;
            j += 1;
        } else if lcs[at(i + 1, j)] >= lcs[at(i, j + 1)] {
            ops.push(Op::Del(old[i]));
            i += 1;
        } else {
            ops.push(Op::Add(new[j]));
            j += 1;
        }
    }
    ops.extend(old[i..].iter().map(|l| Op::Del(l)));
    ops.extend(new[j..].iter().map(|l| Op::Add(l)));
    ops
}

/// Size of a rendered diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStats {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

/// Count files and changed lines in a diff made by [`render_file_diff`].
pub fn diff_stats(diff: &str) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in diff.lines() {
        if line.starts_with("+++ ") {
            stats.files += 1;
        } else if line.starts_with("--- ") {
        } else if line.starts_with('+') {
            stats.added += 1;
        } else if line.starts_with('-') {
            stats.removed += 1;
        }
    }
    stats
}

/// Changed lines keyed by file, side and normalized text. Trailing whitespace
/// and lines that are only whitespace are ignored so formatting noise does
/// not count as a difference.
fn change_multiset(diff: &str) -> HashMap<String, usize> {
    let mut set = HashMap::new();
    let mut file = String::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            file = rest.trim_start_matches("b/").to_string();
            continue;
        }
        if line.starts_with("--- ") {
            continue;
        }
        let (side, text) = match line.chars().next() {
            Some('+') => ('+', &line[1..]),
            Some('-') => ('-', &line[1..]),
            _ => continue,
        };
        let text = text.trim_end();
        if text.trim().is_empty() {
            continue;
        }
        *set.entry(format!("{file}\u{0}{side}{text}")).or_insert(0) += 1;
    }
    set
}

/// How alike two diffs are, 0.0 to 1.0: the size of the changed lines they
/// share over the size of all changed lines either has (weighted Jaccard, per
/// file and per side). Two empty diffs are identical.
pub fn diff_similarity(a: &str, b: &str) -> f64 {
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
        if let Some(d) = diff {
            write_file(&dir.join(format!("{}.diff", record.replay_id)), d)?;
        }
        let path = dir.join(format!("{}.json", record.replay_id));
        write_file(&path, &serde_json::to_string_pretty(record)?)?;
        Ok(path)
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
            diff_sha256: Some("y".into()),
            similarity,
            check_passed: check,
            failure: None,
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
    fn render_diff_marks_added_removed_and_new_files() {
        let d = render_file_diff("a.txt", Some("one\ntwo\nthree\n"), Some("one\n2\nthree\n"));
        assert_eq!(d, "--- a/a.txt\n+++ b/a.txt\n-two\n+2\n");
        let created = render_file_diff("n.txt", None, Some("x\ny\n"));
        assert_eq!(created, "--- /dev/null\n+++ b/n.txt\n+x\n+y\n");
        let deleted = render_file_diff("d.txt", Some("x\n"), None);
        assert_eq!(deleted, "--- a/d.txt\n+++ /dev/null\n-x\n");
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
        assert_eq!(d, "--- a/f\n+++ b/f\n-a\n-c\n+X\n");
        let s = diff_stats(&d);
        assert_eq!((s.files, s.added, s.removed), (1, 1, 2));
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
}
