//! The aggregate usage ledger: one append-only JSONL line per model call (or per
//! goal) that TA funnels, kept per project and rolled up globally.
//!
//! Files:
//!   * per project: `<project>/.ta/usage-ledger.jsonl`
//!   * global roll-up: `<user usage dir>/ledger.jsonl` (see [`user_usage_dir`])
//!
//! Every record is written with a single `write_all` of one complete line to a
//! file opened in append mode, behind a process-wide mutex, so concurrent
//! writers in one process and (for lines under a page, which these always are)
//! concurrent processes cannot interleave or tear a line.
//!
//! A path that cannot report tokens (a `claude --print` text answer, for
//! example) is recorded with `tokens_unknown = true` rather than dropped, and a
//! line that cannot be parsed on read is counted and reported, never skipped
//! silently.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::spend::CompletionRecord;
use crate::token_cost::TokenUsage;

/// Project-relative ledger path.
pub const PROJECT_LEDGER_FILE: &str = ".ta/usage-ledger.jsonl";
/// Environment variable that moves the per-user usage directory (snapshot and
/// global ledger). Used by tests and by users who keep state elsewhere.
pub const USAGE_DIR_ENV: &str = "TA_USAGE_DIR";

/// Longest `note`/`model` kept, so a line always stays far below one page.
const MAX_TEXT: usize = 200;

static APPEND_LOCK: Mutex<()> = Mutex::new(());

/// The per-user usage directory: `$TA_USAGE_DIR`, else `~/.config/ta/usage`
/// (the literal `~/.config/ta` TA uses on every platform).
pub fn user_usage_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(USAGE_DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("ta")
        .join("usage")
}

/// What funneled the usage.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// A `ta run` goal.
    Goal,
    /// Advisor / shell Q&A.
    Advisor,
    /// Chief-of-Staff read-only chat / Q&A.
    CosQa,
    /// A `ta golden replay` goal.
    GoldenReplay,
    #[default]
    Other,
}

impl UsageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageKind::Goal => "goal",
            UsageKind::Advisor => "advisor",
            UsageKind::CosQa => "cos_qa",
            UsageKind::GoldenReplay => "golden_replay",
            UsageKind::Other => "other",
        }
    }

    /// Classify a finished goal from its title and origin. A chat-mode session
    /// (`origin = "chat"`) is the Chief-of-Staff Q&A path; a goal whose title
    /// starts with `Golden replay ` came from `ta golden replay`.
    pub fn classify_goal(title: &str, origin: Option<&str>) -> Self {
        if title.starts_with("Golden replay ") {
            UsageKind::GoldenReplay
        } else if origin == Some("chat") {
            UsageKind::CosQa
        } else {
            UsageKind::Goal
        }
    }
}

fn cap(s: &str) -> String {
    s.chars().take(MAX_TEXT).collect()
}

/// One ledger line. Every field but `ts` defaults so older or partial lines load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageRecord {
    pub ts: DateTime<Utc>,
    pub kind: UsageKind,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub usd: f64,
    pub goal_id: Option<String>,
    /// Project directory name, so the global roll-up can be split by project.
    pub project: String,
    /// `true` when the path could not report tokens. The call is still counted.
    pub tokens_unknown: bool,
    pub note: Option<String>,
}

impl Default for UsageRecord {
    fn default() -> Self {
        Self {
            ts: Utc::now(),
            kind: UsageKind::Other,
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            usd: 0.0,
            goal_id: None,
            project: String::new(),
            tokens_unknown: false,
            note: None,
        }
    }
}

impl UsageRecord {
    /// Every token of every kind (what a token budget is measured in).
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_creation_tokens)
    }

    pub fn usage(&self) -> TokenUsage {
        TokenUsage {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_read: self.cache_read_tokens,
            cache_creation: self.cache_creation_tokens,
        }
    }

    /// A record for a call whose tokens are not available.
    pub fn unknown_tokens(kind: UsageKind, model: &str, project: &str, note: &str) -> Self {
        Self {
            kind,
            model: cap(model),
            project: cap(project),
            tokens_unknown: true,
            note: Some(cap(note)),
            ..Self::default()
        }
    }

    /// A record for a finished goal, from its completion record.
    pub fn from_completion(rec: &CompletionRecord, project: &str) -> Self {
        let total = rec
            .input_tokens
            .saturating_add(rec.output_tokens)
            .saturating_add(rec.cache_read_tokens)
            .saturating_add(rec.cache_creation_tokens);
        let unknown = total == 0;
        Self {
            ts: Utc::now(),
            kind: UsageKind::classify_goal(&rec.title, rec.origin.as_deref()),
            model: cap(&rec.model),
            input_tokens: rec.input_tokens,
            output_tokens: rec.output_tokens,
            cache_read_tokens: rec.cache_read_tokens,
            cache_creation_tokens: rec.cache_creation_tokens,
            usd: if rec.usd.is_finite() { rec.usd } else { 0.0 },
            goal_id: Some(rec.goal_id.to_string()),
            project: cap(project),
            tokens_unknown: unknown,
            note: unknown.then(|| {
                "the agent runtime reported no token usage for this goal (for example a model \
                 or runtime that does not emit a usage report)"
                    .to_string()
            }),
        }
    }
}

/// What a read of the ledger found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedgerRead {
    pub records: Vec<UsageRecord>,
    /// Lines that were not valid records. Reported, never silently dropped.
    pub unparsable_lines: u64,
    /// `false` when the file does not exist yet.
    pub found: bool,
}

/// One append-only ledger file.
#[derive(Debug, Clone)]
pub struct UsageLedger {
    path: PathBuf,
}

impl UsageLedger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn for_project(project_root: &Path) -> Self {
        Self::new(project_root.join(PROJECT_LEDGER_FILE))
    }

    pub fn global() -> Self {
        Self::new(user_usage_dir().join("ledger.jsonl"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record as one line.
    pub fn append(&self, rec: &UsageRecord) -> anyhow::Result<()> {
        use anyhow::Context;
        let mut line = serde_json::to_string(rec).context("could not serialize a usage record")?;
        line.push('\n');
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "could not create {} for the usage ledger. Check the directory is writable",
                    parent.display()
                )
            })?;
        }
        let _guard = APPEND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("could not open {} for append", self.path.display()))?;
        f.write_all(line.as_bytes())
            .with_context(|| format!("could not append to {}", self.path.display()))?;
        Ok(())
    }

    /// Append a goal record unless this ledger already holds one for the same
    /// goal and kind (a goal can be published twice). Returns `true` if written.
    pub fn append_goal_once(&self, rec: &UsageRecord) -> anyhow::Result<bool> {
        if let Some(id) = &rec.goal_id {
            let existing = self.read();
            if existing
                .records
                .iter()
                .any(|r| r.goal_id.as_deref() == Some(id) && r.kind == rec.kind)
            {
                return Ok(false);
            }
        }
        self.append(rec)?;
        Ok(true)
    }

    /// Read every record. A missing file is an empty ledger.
    pub fn read(&self) -> LedgerRead {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LedgerRead::default(),
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "could not read the usage ledger; treating it as empty. Check the file is readable");
                return LedgerRead::default();
            }
        };
        let mut out = LedgerRead {
            found: true,
            ..LedgerRead::default()
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<UsageRecord>(line) {
                Ok(r) => out.records.push(r),
                Err(_) => out.unparsable_lines += 1,
            }
        }
        out
    }
}

/// Record a finished goal in the project ledger and the global roll-up.
/// Failures are logged with the path and a next step, and returned as `false`;
/// they never stop a goal from finishing.
pub fn record_goal_completion(project_root: &Path, rec: &CompletionRecord) -> bool {
    let project = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    record_usage_in(
        &UsageLedger::for_project(project_root),
        &UsageLedger::global(),
        &UsageRecord::from_completion(rec, &project),
        true,
    )
}

/// Record a call that is not a goal (advisor or CoS Q&A) in both ledgers.
pub fn record_usage(project_root: &Path, rec: &UsageRecord) -> bool {
    let mut rec = rec.clone();
    if rec.project.is_empty() {
        rec.project = project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
    }
    record_usage_in(
        &UsageLedger::for_project(project_root),
        &UsageLedger::global(),
        &rec,
        false,
    )
}

/// The explicit-path form of the two functions above.
pub fn record_usage_in(
    project: &UsageLedger,
    global: &UsageLedger,
    rec: &UsageRecord,
    once_per_goal: bool,
) -> bool {
    let mut ok = true;
    for (label, ledger) in [("project", project), ("global", global)] {
        let r = if once_per_goal {
            ledger.append_goal_once(rec).map(|_| ())
        } else {
            ledger.append(rec)
        };
        if let Err(e) = r {
            ok = false;
            tracing::warn!(
                ledger = label,
                path = %ledger.path().display(),
                kind = rec.kind.as_str(),
                goal_id = ?rec.goal_id,
                error = %e,
                "could not write the usage ledger line; this call is missing from `ta usage`. \
                 Check the path is writable (set {USAGE_DIR_ENV} to move the global ledger)"
            );
        }
    }
    ok
}

/// Totals for one group of records.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageBreakdown {
    pub key: String,
    pub records: u64,
    pub usd: f64,
    pub tokens: u64,
    /// Records that could not report tokens; their calls are counted, their
    /// tokens are not.
    pub tokens_unknown_records: u64,
}

impl UsageBreakdown {
    fn add(&mut self, r: &UsageRecord) {
        self.records += 1;
        self.usd += r.usd;
        self.tokens = self.tokens.saturating_add(r.total_tokens());
        if r.tokens_unknown {
            self.tokens_unknown_records += 1;
        }
    }
}

fn group_by<'a>(
    records: impl IntoIterator<Item = &'a UsageRecord>,
    key: impl Fn(&UsageRecord) -> String,
) -> Vec<UsageBreakdown> {
    let mut map: BTreeMap<String, UsageBreakdown> = BTreeMap::new();
    for r in records {
        let k = key(r);
        map.entry(k.clone())
            .or_insert_with(|| UsageBreakdown {
                key: k,
                ..UsageBreakdown::default()
            })
            .add(r);
    }
    map.into_values().collect()
}

/// Records at or after `since` (all when `None`).
pub fn in_window(records: &[UsageRecord], since: Option<DateTime<Utc>>) -> Vec<&UsageRecord> {
    records
        .iter()
        .filter(|r| since.is_none_or(|s| r.ts >= s))
        .collect()
}

pub fn by_kind<'a>(records: impl IntoIterator<Item = &'a UsageRecord>) -> Vec<UsageBreakdown> {
    group_by(records, |r| r.kind.as_str().to_string())
}

pub fn by_model<'a>(records: impl IntoIterator<Item = &'a UsageRecord>) -> Vec<UsageBreakdown> {
    group_by(records, |r| {
        if r.model.is_empty() {
            "unknown".to_string()
        } else {
            r.model.clone()
        }
    })
}

pub fn by_project<'a>(records: impl IntoIterator<Item = &'a UsageRecord>) -> Vec<UsageBreakdown> {
    group_by(records, |r| {
        if r.project.is_empty() {
            "unknown".to_string()
        } else {
            r.project.clone()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Arc;

    fn rec(kind: UsageKind, model: &str, out: u64, usd: f64, day: u32) -> UsageRecord {
        UsageRecord {
            ts: Utc.with_ymd_and_hms(2026, 10, day, 12, 0, 0).unwrap(),
            kind,
            model: model.into(),
            output_tokens: out,
            usd,
            ..UsageRecord::default()
        }
    }

    #[test]
    fn concurrent_appends_never_tear_or_lose_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(UsageLedger::new(dir.path().join("l.jsonl")));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let l = Arc::clone(&ledger);
                std::thread::spawn(move || {
                    for i in 0..100 {
                        let mut r = rec(UsageKind::Goal, "claude-sonnet-5-5", 10, 0.01, 1);
                        r.goal_id = Some(format!("{t}-{i}"));
                        r.note = Some("x".repeat(150));
                        l.append(&r).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let read = ledger.read();
        assert_eq!(read.unparsable_lines, 0, "a torn line was written");
        assert_eq!(read.records.len(), 800, "a line was lost");
        let mut ids: Vec<_> = read.records.iter().map(|r| r.goal_id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 800);
    }

    #[test]
    fn missing_ledger_is_empty_and_garbage_lines_are_counted_not_dropped_silently() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = UsageLedger::new(dir.path().join("sub").join("l.jsonl"));
        let none = ledger.read();
        assert!(!none.found && none.records.is_empty());
        ledger
            .append(&rec(UsageKind::Advisor, "m", 1, 0.0, 2))
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(ledger.path())
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        let read = ledger.read();
        assert!(read.found);
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.unparsable_lines, 1);
    }

    #[test]
    fn old_partial_lines_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("l.jsonl");
        std::fs::write(
            &p,
            "{\"ts\":\"2026-10-01T00:00:00Z\",\"model\":\"m\",\"usd\":1.5}\n",
        )
        .unwrap();
        let read = UsageLedger::new(p).read();
        assert_eq!(read.records.len(), 1);
        assert_eq!(read.records[0].kind, UsageKind::Other);
        assert_eq!(read.records[0].usd, 1.5);
    }

    #[test]
    fn aggregates_by_kind_model_and_window() {
        let records = vec![
            rec(UsageKind::Goal, "opus", 100, 1.0, 1),
            rec(UsageKind::Goal, "sonnet", 50, 0.5, 5),
            rec(UsageKind::CosQa, "sonnet", 10, 0.1, 6),
            UsageRecord {
                tokens_unknown: true,
                ..rec(UsageKind::Advisor, "", 0, 0.0, 7)
            },
        ];
        let kinds = by_kind(&records);
        let goal = kinds.iter().find(|b| b.key == "goal").unwrap();
        assert_eq!((goal.records, goal.tokens), (2, 150));
        assert!((goal.usd - 1.5).abs() < 1e-9);
        let advisor = kinds.iter().find(|b| b.key == "advisor").unwrap();
        assert_eq!(advisor.tokens_unknown_records, 1);
        let models = by_model(&records);
        assert!(models.iter().any(|b| b.key == "unknown" && b.records == 1));
        assert_eq!(
            models.iter().find(|b| b.key == "sonnet").unwrap().records,
            2
        );
        let since = Utc.with_ymd_and_hms(2026, 10, 5, 0, 0, 0).unwrap();
        assert_eq!(in_window(&records, Some(since)).len(), 3);
        assert_eq!(in_window(&records, None).len(), 4);
    }

    fn completion(tokens: u64, title: &str, origin: Option<&str>) -> CompletionRecord {
        let mut g = crate::GoalRun::new(
            title,
            "o",
            "claude-code",
            PathBuf::from("/w"),
            PathBuf::from("/s"),
        );
        g.output_tokens = tokens;
        g.agent_model = "claude-sonnet-5-5".into();
        g.origin = origin.map(str::to_string);
        CompletionRecord::from_goal(&g)
    }

    #[test]
    fn goal_with_no_reported_tokens_is_kept_and_flagged() {
        let r = UsageRecord::from_completion(&completion(0, "t", None), "proj");
        assert!(r.tokens_unknown);
        assert_eq!(r.kind, UsageKind::Goal);
        let r = UsageRecord::from_completion(&completion(5, "t", None), "proj");
        assert!(!r.tokens_unknown);
    }

    #[test]
    fn goal_kinds_are_classified() {
        assert_eq!(
            UsageRecord::from_completion(&completion(1, "Golden replay x y", None), "p").kind,
            UsageKind::GoldenReplay
        );
        assert_eq!(
            UsageRecord::from_completion(&completion(1, "t", Some("chat")), "p").kind,
            UsageKind::CosQa
        );
        assert_eq!(
            UsageRecord::from_completion(&completion(1, "t", Some("cos")), "p").kind,
            UsageKind::Goal
        );
    }

    #[test]
    fn publishing_a_goal_twice_writes_one_line_and_never_drops_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = UsageLedger::new(dir.path().join("p.jsonl"));
        let g = UsageLedger::new(dir.path().join("g.jsonl"));
        let r = UsageRecord::from_completion(&completion(5, "t", None), "proj");
        assert!(record_usage_in(&p, &g, &r, true));
        assert!(record_usage_in(&p, &g, &r, true));
        assert_eq!(p.read().records.len(), 1);
        assert_eq!(g.read().records.len(), 1);
    }

    #[test]
    fn a_failed_write_is_reported_not_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be makes the open fail.
        let bad = UsageLedger::new(dir.path());
        let good = UsageLedger::new(dir.path().join("g.jsonl"));
        let r = rec(UsageKind::Advisor, "m", 1, 0.0, 1);
        assert!(!record_usage_in(&bad, &good, &r, false));
        // The healthy ledger still got the line.
        assert_eq!(good.read().records.len(), 1);
    }
}
