// queue.rs: the durable action queue (v0.17.12.3).
//
// Outside actions (the ones a draft bundles: a Slack post, an email, a PR) are
// not part of the all-or-nothing local commit. `ta draft apply` writes them
// into this queue in the same step that marks the draft Applied, and a worker
// in the daemon sends them afterwards. Properties this module guarantees:
//
// * DURABLE. State is an append-only JSONL journal, fsynced on every event.
//   The queue survives restarts and crashes. State is rebuilt by replay, so
//   there is no second index to disagree with the journal.
// * AT MOST ONCE. The key is `<draft id>:<action id>`. Enqueueing the same key
//   again is a no-op, and a key is only ever claimed (`sending`) by one caller
//   at a time under the queue lock.
// * INTENT BEFORE SEND. The `sending` event is fsynced before the connector is
//   called. If the process dies after that, replay finds an action that is
//   `sending` with no result: the outcome is UNKNOWN. It moves to
//   `needs attention` and is never guessed or resent automatically.
// * POLICY EVERY ATTEMPT. The send policy is consulted before every attempt,
//   including retries, and fails closed.
// * IRRECOVERABLE LAST. An irrecoverable action is held until every other
//   action of the same draft is done.
// * ISOLATED. One action failing, panicking or going unknown never stops the
//   others.
//
// WHERE IT LIVES. The queue directory is chosen by the caller and MUST be the
// daemon-only per-user state directory (`ProjectState::action_queue_dir()` in
// `ta-credentials`), outside every project tree and every staging workspace,
// so an agent working in staging cannot reach it with `../../`. This module
// creates the directory mode 0700 on Unix.
//
// Authority: docs/superpowers/specs/2026-10-07-atomic-apply-and-action-queue-design.md.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::reversibility::{Idempotency, Reversibility};

/// Journal file name inside the queue directory.
pub const QUEUE_FILE: &str = "queue.jsonl";
const LOCK_FILE: &str = "queue.lock";
const LOCK_WAIT: StdDuration = StdDuration::from_secs(10);
const LOCK_STALE: StdDuration = StdDuration::from_secs(60);
const MAX_RESULT_BYTES: usize = 16 * 1024;

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors from the queue. Each says what was being attempted and what to do.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error(
        "action queue: could not {op} {path}: {source}. Check that the directory is writable by \
         your user and the disk is not full. No action was sent or recorded by this call."
    )]
    Io {
        op: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "action queue: could not take the queue lock {path} within {waited_secs}s. Another TA \
         process is using the queue. Wait and retry; if no TA process is running, delete the \
         lock file."
    )]
    Locked { path: PathBuf, waited_secs: u64 },
    #[error(
        "action queue: no action with key '{key}'. Run `ta show actions` to list keys \
         (draft id plus action id)."
    )]
    UnknownKey { key: String },
    #[error("action queue: {what}. {next}")]
    Refused { what: String, next: String },
    #[error("action queue: could not serialize an event: {0}. This is a bug; report it.")]
    Serialize(#[from] serde_json::Error),
}

fn io_err<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> QueueError + 'a {
    move |source| QueueError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

// ── Clock ────────────────────────────────────────────────────────────────────

/// Time source, injected so retry backoff is testable without sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock tests advance by hand.
#[derive(Debug)]
pub struct ManualClock(Mutex<DateTime<Utc>>);

impl ManualClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self(Mutex::new(start))
    }
    pub fn advance(&self, by: Duration) {
        let mut g = self.0.lock().unwrap_or_else(|p| p.into_inner());
        *g += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

// ── Model ────────────────────────────────────────────────────────────────────

/// Lifecycle state of one queued action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    Queued,
    Sending,
    Done,
    Retrying,
    NeedsAttention,
}

impl QueueState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Done => "done",
            Self::Retrying => "retrying",
            Self::NeedsAttention => "needs attention",
        }
    }
}

/// Why an action needs a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReason {
    /// The process died or the connector gave no clear answer after intent was recorded.
    UnknownOutcome,
    /// Policy or configuration at send time no longer permits the action.
    PolicyBlocked,
    /// Every allowed attempt failed.
    RetriesExhausted,
    /// The connector rejected the action outright; retrying cannot help.
    Rejected,
}

impl AttentionReason {
    /// One plain sentence plus the one thing to do.
    pub fn explain(self) -> (&'static str, &'static str) {
        match self {
            Self::UnknownOutcome => (
                "TA cannot tell whether this action went out.",
                "Check the destination, then run `ta show actions --resolve <key> --sent` if it \
                 went out or `--resend` if it did not.",
            ),
            Self::PolicyBlocked => (
                "Policy or configuration now blocks this action.",
                "Fix the policy, then run `ta show actions --resolve <key> --resend`, or drop it \
                 with `--drop`.",
            ),
            Self::RetriesExhausted => (
                "Every retry failed.",
                "Fix the cause shown in the last error, then run `ta show actions --resolve \
                 <key> --resend`, or drop it with `--drop`.",
            ),
            Self::Rejected => (
                "The destination rejected this action.",
                "Read the last error, then run `ta show actions --resolve <key> --resend` after \
                 fixing it, or drop it with `--drop`.",
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub reason: AttentionReason,
    pub message: String,
}

/// What a person decided for an action in `needs attention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// The person confirmed it went out.
    SentConfirmed,
    /// The person confirmed it did not go out; send it again.
    Resend,
    /// The person decided not to send it.
    Dropped,
}

/// An action to enqueue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAction {
    pub draft_id: String,
    pub action_id: String,
    pub tool_name: String,
    pub parameters: Value,
    #[serde(default)]
    pub description: String,
    pub class: Reversibility,
    /// Verb that counters this action, for compensable actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    #[serde(default)]
    pub idempotency: Idempotency,
    /// Set when this entry IS an undo, naming the key it counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_of: Option<String>,
}

impl NewAction {
    pub fn key(&self) -> String {
        make_key(&self.draft_id, &self.action_id)
    }
}

/// `<draft id>:<action id>`.
pub fn make_key(draft_id: &str, action_id: &str) -> String {
    format!("{draft_id}:{action_id}")
}

/// `ta_propose_*` actions are replayed against Wayfinder by `draft_task_replay`
/// with their own ledger. They never enter this queue (kept from PR #642).
pub fn is_queueable_tool(tool_name: &str) -> bool {
    !tool_name.starts_with("ta_propose_")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Enqueued {
        #[serde(flatten)]
        action: NewAction,
        at: DateTime<Utc>,
    },
    Sending {
        key: String,
        attempt: u32,
        at: DateTime<Utc>,
    },
    Done {
        key: String,
        result: Value,
        at: DateTime<Utc>,
    },
    Retrying {
        key: String,
        attempt: u32,
        next_at: DateTime<Utc>,
        error: String,
        at: DateTime<Utc>,
    },
    NeedsAttention {
        key: String,
        reason: AttentionReason,
        message: String,
        at: DateTime<Utc>,
    },
    Resolved {
        key: String,
        resolution: Resolution,
        note: String,
        at: DateTime<Utc>,
    },
}

/// One action as rebuilt from the journal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueueEntry {
    pub key: String,
    pub action: NewAction,
    pub state: QueueState,
    pub attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub attention: Option<Attention>,
    pub result: Option<Value>,
    /// Key of the undo entry that countered this action, if one was queued.
    pub undone_by: Option<String>,
    pub enqueued_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Position in enqueue order.
    pub seq: usize,
}

impl QueueEntry {
    fn due(&self, now: DateTime<Utc>) -> bool {
        match self.state {
            QueueState::Queued => true,
            QueueState::Retrying => self.next_attempt_at.map(|t| t <= now).unwrap_or(true),
            _ => false,
        }
    }
}

/// The queue as of one replay.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub entries: Vec<QueueEntry>,
    /// Journal lines that could not be parsed (a torn tail after a crash, or damage).
    pub skipped_lines: usize,
}

impl Snapshot {
    pub fn get(&self, key: &str) -> Option<&QueueEntry> {
        self.entries.iter().find(|e| e.key == key)
    }
}

fn replay(text: &str) -> Snapshot {
    let mut by_key: BTreeMap<String, QueueEntry> = BTreeMap::new();
    let mut skipped = 0usize;
    let mut seq = 0usize;
    // `lines()` strips both \n and \r\n, so a journal rewritten with CRLF
    // line endings (Windows tooling) replays identically.
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Event>(line) else {
            skipped += 1;
            continue;
        };
        match ev {
            Event::Enqueued { action, at } => {
                let key = action.key();
                if by_key.contains_key(&key) {
                    continue; // first enqueue wins; a key never runs twice
                }
                if let Some(orig) = &action.undo_of {
                    if let Some(o) = by_key.get_mut(orig) {
                        o.undone_by = Some(key.clone());
                    }
                }
                by_key.insert(
                    key.clone(),
                    QueueEntry {
                        key,
                        action,
                        state: QueueState::Queued,
                        attempts: 0,
                        next_attempt_at: None,
                        last_error: None,
                        attention: None,
                        result: None,
                        undone_by: None,
                        enqueued_at: at,
                        updated_at: at,
                        seq,
                    },
                );
                seq += 1;
            }
            Event::Sending { key, attempt, at } => {
                if let Some(e) = by_key.get_mut(&key) {
                    e.state = QueueState::Sending;
                    e.attempts = attempt;
                    e.next_attempt_at = None;
                    e.updated_at = at;
                }
            }
            Event::Done { key, result, at } => {
                if let Some(e) = by_key.get_mut(&key) {
                    e.state = QueueState::Done;
                    e.result = Some(result);
                    e.attention = None;
                    e.last_error = None;
                    e.updated_at = at;
                }
            }
            Event::Retrying {
                key,
                attempt,
                next_at,
                error,
                at,
            } => {
                if let Some(e) = by_key.get_mut(&key) {
                    e.state = QueueState::Retrying;
                    e.attempts = attempt;
                    e.next_attempt_at = Some(next_at);
                    e.last_error = Some(error);
                    e.updated_at = at;
                }
            }
            Event::NeedsAttention {
                key,
                reason,
                message,
                at,
            } => {
                if let Some(e) = by_key.get_mut(&key) {
                    e.state = QueueState::NeedsAttention;
                    e.last_error = Some(message.clone());
                    e.attention = Some(Attention { reason, message });
                    e.next_attempt_at = None;
                    e.updated_at = at;
                }
            }
            Event::Resolved {
                key,
                resolution,
                note,
                at,
            } => {
                if let Some(e) = by_key.get_mut(&key) {
                    e.attention = None;
                    e.updated_at = at;
                    match resolution {
                        Resolution::SentConfirmed => {
                            e.state = QueueState::Done;
                            e.result =
                                Some(Value::String(format!("confirmed sent by a person: {note}")));
                        }
                        Resolution::Dropped => {
                            e.state = QueueState::Done;
                            e.result = Some(Value::String(format!("dropped by a person: {note}")));
                        }
                        Resolution::Resend => {
                            e.state = QueueState::Queued;
                            e.next_attempt_at = None;
                        }
                    }
                }
            }
        }
    }
    let mut entries: Vec<QueueEntry> = by_key.into_values().collect();
    entries.sort_by_key(|e| e.seq);
    Snapshot {
        entries,
        skipped_lines: skipped,
    }
}

// ── Queue ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnqueueReport {
    pub added: Vec<String>,
    pub already_present: Vec<String>,
}

/// Handle on the queue directory.
#[derive(Debug, Clone)]
pub struct ActionQueue {
    dir: PathBuf,
}

struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl ActionQueue {
    /// Opens (creating if needed) the queue at `dir`, which must be the
    /// daemon-only state directory. Creates it mode 0700 on Unix.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, QueueError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(io_err("create the queue directory", &dir))?;
        restrict_dir(&dir);
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn journal_path(&self) -> PathBuf {
        self.dir.join(QUEUE_FILE)
    }

    fn lock(&self) -> Result<LockGuard, QueueError> {
        let path = self.dir.join(LOCK_FILE);
        let start = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(LockGuard(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .map(|age| age > LOCK_STALE)
                        .unwrap_or(false);
                    if stale {
                        tracing::warn!(path = %path.display(),
                            "removing a stale action queue lock left by a dead process");
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() > LOCK_WAIT {
                        return Err(QueueError::Locked {
                            path,
                            waited_secs: LOCK_WAIT.as_secs(),
                        });
                    }
                    std::thread::sleep(StdDuration::from_millis(2));
                }
                Err(e) => return Err(io_err("create the queue lock", &path)(e)),
            }
        }
    }

    /// Replays the journal. Lock-free: appends are single `write` calls of
    /// whole lines, so a reader sees complete lines or a torn tail it skips.
    pub fn snapshot(&self) -> Result<Snapshot, QueueError> {
        let path = self.journal_path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Snapshot::default()),
            Err(e) => return Err(io_err("read the queue journal", &path)(e)),
        };
        let snap = replay(&String::from_utf8_lossy(&bytes));
        if snap.skipped_lines > 0 {
            tracing::warn!(path = %path.display(), skipped = snap.skipped_lines,
                "action queue journal has unreadable lines (a crash mid-write or damage); they were skipped");
        }
        Ok(snap)
    }

    /// Appends events as whole lines in one write, then fsyncs. Caller holds the lock.
    fn append_locked(&self, events: &[Event]) -> Result<(), QueueError> {
        if events.is_empty() {
            return Ok(());
        }
        let path = self.journal_path();
        let mut buf = String::new();
        // A crash mid-write can leave a torn last line with no newline. Start
        // on a fresh line so the torn fragment cannot swallow our event.
        if let Ok(existing) = std::fs::read(&path) {
            if existing.last().map(|b| *b != b'\n').unwrap_or(false) {
                buf.push('\n');
            }
        }
        for ev in events {
            buf.push_str(&serde_json::to_string(ev)?);
            buf.push('\n');
        }
        let created = !path.exists();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_err("open the queue journal", &path))?;
        f.write_all(buf.as_bytes())
            .map_err(io_err("append to the queue journal", &path))?;
        f.sync_all()
            .map_err(io_err("fsync the queue journal", &path))?;
        if created {
            sync_dir(&self.dir);
        }
        Ok(())
    }

    /// Writes every action in ONE append and ONE fsync, skipping keys already
    /// present. Safe to call again with the same actions (recovery does).
    pub fn enqueue_batch(
        &self,
        actions: &[NewAction],
        clock: &dyn Clock,
    ) -> Result<EnqueueReport, QueueError> {
        let _g = self.lock()?;
        let snap = self.snapshot()?;
        let mut known: HashSet<String> = snap.entries.iter().map(|e| e.key.clone()).collect();
        let mut report = EnqueueReport::default();
        let mut events = Vec::new();
        let at = clock.now();
        for a in actions {
            let key = a.key();
            if !known.insert(key.clone()) {
                report.already_present.push(key);
                continue;
            }
            report.added.push(key);
            events.push(Event::Enqueued {
                action: a.clone(),
                at,
            });
        }
        self.append_locked(&events)?;
        Ok(report)
    }

    /// Startup recovery: any action still `sending` had its intent recorded
    /// but no result, so its outcome is unknown. Moves each to `needs
    /// attention`; NEVER resends. Returns the keys moved.
    pub fn recover_in_flight(&self, clock: &dyn Clock) -> Result<Vec<String>, QueueError> {
        let _g = self.lock()?;
        let snap = self.snapshot()?;
        let at = clock.now();
        let mut keys = Vec::new();
        let mut events = Vec::new();
        for e in snap
            .entries
            .iter()
            .filter(|e| e.state == QueueState::Sending)
        {
            keys.push(e.key.clone());
            events.push(Event::NeedsAttention {
                key: e.key.clone(),
                reason: AttentionReason::UnknownOutcome,
                message: format!(
                    "attempt {} of '{}' was started but no result was recorded (TA stopped or \
                     crashed mid-send), so it may or may not have gone out",
                    e.attempts, e.action.tool_name
                ),
                at,
            });
        }
        self.append_locked(&events)?;
        if !keys.is_empty() {
            tracing::warn!(count = keys.len(), keys = ?keys,
                "action queue: in-flight actions have an unknown outcome and need attention; nothing was resent");
        }
        Ok(keys)
    }

    /// A person's decision for an action in `needs attention`.
    pub fn resolve(
        &self,
        key: &str,
        resolution: Resolution,
        note: &str,
        clock: &dyn Clock,
    ) -> Result<(), QueueError> {
        let _g = self.lock()?;
        let snap = self.snapshot()?;
        let e = snap.get(key).ok_or_else(|| QueueError::UnknownKey {
            key: key.to_string(),
        })?;
        if e.state != QueueState::NeedsAttention {
            return Err(QueueError::Refused {
                what: format!(
                    "'{key}' is {} and does not need a decision",
                    e.state.label()
                ),
                next: "Only actions in `needs attention` can be resolved. Run `ta show actions`."
                    .into(),
            });
        }
        self.append_locked(&[Event::Resolved {
            key: key.to_string(),
            resolution,
            note: note.to_string(),
            at: clock.now(),
        }])
    }

    /// Queues the recorded undo of a completed compensable action as a NEW
    /// entry (a new logged action, not a rewrite of history). Returns its key.
    pub fn request_undo(&self, key: &str, clock: &dyn Clock) -> Result<String, QueueError> {
        let _g = self.lock()?;
        let snap = self.snapshot()?;
        let e = snap.get(key).ok_or_else(|| QueueError::UnknownKey {
            key: key.to_string(),
        })?;
        let refuse = |what: String, next: &str| QueueError::Refused {
            what,
            next: next.to_string(),
        };
        if e.state != QueueState::Done {
            return Err(refuse(
                format!(
                    "'{key}' is {}, so there is nothing to undo yet",
                    e.state.label()
                ),
                "Undo is offered once the action is done.",
            ));
        }
        if e.action.class != Reversibility::Compensable {
            return Err(refuse(
                format!("'{key}' is {} and has no recorded undo", e.action.class),
                "Only compensable actions can be undone.",
            ));
        }
        let Some(verb) = e.action.undo.clone() else {
            return Err(refuse(
                format!("'{key}' declares no undo verb"),
                "Add `undo = \"<verb>\"` to the connector manifest.",
            ));
        };
        if let Some(prev) = &e.undone_by {
            return Err(refuse(
                format!("'{key}' was already undone by '{prev}'"),
                "Run `ta show actions` to see its state.",
            ));
        }
        let undo = NewAction {
            draft_id: e.action.draft_id.clone(),
            action_id: format!("undo-{}", e.action.action_id),
            tool_name: format!("ta_external_action:{verb}"),
            parameters: serde_json::json!({
                "undo_of": key,
                "original_parameters": e.action.parameters,
                "original_result": e.result,
            }),
            description: format!("undo of: {}", e.action.description),
            class: Reversibility::Irrecoverable,
            undo: None,
            idempotency: e.action.idempotency,
            undo_of: Some(key.to_string()),
        };
        let undo_key = undo.key();
        self.append_locked(&[Event::Enqueued {
            action: undo,
            at: clock.now(),
        }])?;
        Ok(undo_key)
    }

    fn claim(
        &self,
        key: &str,
        clock: &dyn Clock,
        policy: &dyn SendPolicy,
    ) -> Result<Claim, QueueError> {
        let _g = self.lock()?;
        let snap = self.snapshot()?;
        let now = clock.now();
        let Some(e) = snap.get(key) else {
            return Ok(Claim::Skip);
        };
        if !e.due(now) {
            return Ok(Claim::Skip);
        }
        // Policy is re-checked on EVERY attempt, before intent is recorded.
        if let PolicyVerdict::Block { reason } = policy.check(e) {
            self.append_locked(&[Event::NeedsAttention {
                key: key.to_string(),
                reason: AttentionReason::PolicyBlocked,
                message: reason,
                at: now,
            }])?;
            return Ok(Claim::Blocked);
        }
        let attempt = e.attempts + 1;
        // Fsynced BEFORE the connector is called. If this fails, nothing is sent.
        self.append_locked(&[Event::Sending {
            key: key.to_string(),
            attempt,
            at: now,
        }])?;
        Ok(Claim::Claimed {
            entry: Box::new(e.clone()),
            attempt,
        })
    }

    fn record(&self, ev: Event) -> Result<(), QueueError> {
        let _g = self.lock()?;
        self.append_locked(&[ev])
    }
}

enum Claim {
    Skip,
    Blocked,
    Claimed {
        entry: Box<QueueEntry>,
        attempt: u32,
    },
}

fn restrict_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::warn!(path = %dir.display(), error = %e,
                "could not restrict the action queue directory to mode 0700");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(f) = std::fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

// ── Retry ────────────────────────────────────────────────────────────────────

/// Growing delays up to a cap, and a maximum attempt count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub base: Duration,
    pub cap: Duration,
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::seconds(30),
            cap: Duration::hours(1),
            max_attempts: 6,
        }
    }
}

impl RetryPolicy {
    /// Delay before the next attempt after `attempts_made` failed attempts
    /// (1 after the first failure): base, 2x, 4x, ... capped.
    pub fn delay_after(&self, attempts_made: u32) -> Duration {
        let shift = attempts_made.saturating_sub(1).min(20);
        let secs = self
            .base
            .num_seconds()
            .saturating_mul(1i64 << shift)
            .min(self.cap.num_seconds());
        Duration::seconds(secs)
    }
}

// ── Worker ───────────────────────────────────────────────────────────────────

/// Result of one attempt, as classified by the connector.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecOutcome {
    /// It went out.
    Done { result: Value },
    /// It definitely did NOT go out and trying again may work (connection refused, 503 before accept).
    Retryable { message: String },
    /// It definitely did NOT go out and trying again cannot work (validation, no connector).
    Rejected { message: String },
    /// No clear answer (timeout, crash, ambiguous). Never retried automatically.
    Unknown { message: String },
}

pub trait ActionExecutor {
    /// `idempotency_key` is `<draft id>:<action id>`; connectors that declared
    /// `idempotency = "key"` pass it to the remote.
    fn execute(&self, entry: &QueueEntry, idempotency_key: &str) -> ExecOutcome;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyVerdict {
    Allow,
    Block { reason: String },
}

pub trait SendPolicy {
    fn check(&self, entry: &QueueEntry) -> PolicyVerdict;
}

/// What one `run_once` pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub done: Vec<String>,
    pub retrying: Vec<String>,
    pub needs_attention: Vec<String>,
    /// Irrecoverable actions held back because something else of their draft is not done.
    pub held: Vec<String>,
}

pub struct QueueWorker<'a> {
    pub queue: &'a ActionQueue,
    pub clock: &'a dyn Clock,
    pub executor: &'a dyn ActionExecutor,
    pub policy: &'a dyn SendPolicy,
    pub retry: RetryPolicy,
}

fn class_rank(c: Reversibility) -> u8 {
    match c {
        Reversibility::Reversible => 0,
        Reversibility::Compensable => 1,
        Reversibility::Irrecoverable => 2,
    }
}

impl<'a> QueueWorker<'a> {
    /// One pass: attempts every due action once, compensable before
    /// irrecoverable. Never resends an unknown outcome. One action's failure
    /// or panic does not stop the rest.
    pub fn run_once(&self) -> Result<RunReport, QueueError> {
        let mut report = RunReport::default();
        let snap = self.queue.snapshot()?;
        let now = self.clock.now();
        let mut due: Vec<(u8, usize, String)> = snap
            .entries
            .iter()
            .filter(|e| e.due(now))
            .map(|e| (class_rank(e.action.class), e.seq, e.key.clone()))
            .collect();
        due.sort();
        for (_, _, key) in due {
            // Fresh state per candidate: earlier actions in this pass may have finished.
            let fresh = self.queue.snapshot()?;
            let Some(entry) = fresh.get(&key) else {
                continue;
            };
            if entry.action.class == Reversibility::Irrecoverable && entry.action.undo_of.is_none()
            {
                let blocked_by = fresh.entries.iter().find(|o| {
                    o.action.draft_id == entry.action.draft_id
                        && o.key != entry.key
                        && o.action.undo_of.is_none()
                        && o.action.class != Reversibility::Irrecoverable
                        && o.state != QueueState::Done
                });
                if let Some(b) = blocked_by {
                    tracing::info!(key = %entry.key, waiting_on = %b.key, state = b.state.label(),
                        "irrecoverable action held until every other action of the draft is done");
                    report.held.push(key);
                    continue;
                }
            }
            if let Err(e) = self.attempt(&key, &mut report) {
                // A queue I/O failure on one action must not stop the others.
                tracing::error!(key = %key, error = %e,
                    "action queue: could not record an attempt; the action stays as journaled");
            }
        }
        Ok(report)
    }

    fn attempt(&self, key: &str, report: &mut RunReport) -> Result<(), QueueError> {
        let (entry, attempt) = match self.queue.claim(key, self.clock, self.policy)? {
            Claim::Skip => return Ok(()),
            Claim::Blocked => {
                report.needs_attention.push(key.to_string());
                return Ok(());
            }
            Claim::Claimed { entry, attempt } => (entry, attempt),
        };
        let idem = entry.key.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.executor.execute(&entry, &idem)
        }))
        .unwrap_or_else(|p| {
            let what = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            ExecOutcome::Unknown {
                message: format!("the connector panicked mid-send: {what}"),
            }
        });
        let at = self.clock.now();
        match outcome {
            ExecOutcome::Done { result } => {
                self.queue.record(Event::Done {
                    key: key.to_string(),
                    result: cap_result(result),
                    at,
                })?;
                report.done.push(key.to_string());
            }
            ExecOutcome::Retryable { message } => {
                if attempt >= self.retry.max_attempts {
                    self.queue.record(Event::NeedsAttention {
                        key: key.to_string(),
                        reason: AttentionReason::RetriesExhausted,
                        message: format!("{attempt} attempts failed; last error: {message}"),
                        at,
                    })?;
                    report.needs_attention.push(key.to_string());
                } else {
                    let next_at = at + self.retry.delay_after(attempt);
                    self.queue.record(Event::Retrying {
                        key: key.to_string(),
                        attempt,
                        next_at,
                        error: message,
                        at,
                    })?;
                    report.retrying.push(key.to_string());
                }
            }
            ExecOutcome::Rejected { message } => {
                self.queue.record(Event::NeedsAttention {
                    key: key.to_string(),
                    reason: AttentionReason::Rejected,
                    message,
                    at,
                })?;
                report.needs_attention.push(key.to_string());
            }
            ExecOutcome::Unknown { message } => {
                self.queue.record(Event::NeedsAttention {
                    key: key.to_string(),
                    reason: AttentionReason::UnknownOutcome,
                    message,
                    at,
                })?;
                report.needs_attention.push(key.to_string());
            }
        }
        Ok(())
    }
}

fn cap_result(v: Value) -> Value {
    match serde_json::to_string(&v) {
        Ok(s) if s.len() > MAX_RESULT_BYTES => Value::String(format!(
            "(result of {} bytes not stored; see the connector's own log)",
            s.len()
        )),
        _ => v,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-12T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn act(draft: &str, id: &str, class: Reversibility) -> NewAction {
        NewAction {
            draft_id: draft.into(),
            action_id: id.into(),
            tool_name: "ta_external_action:email".into(),
            parameters: serde_json::json!({"to": "a@example.com"}),
            description: format!("action {id}"),
            class,
            undo: if class == Reversibility::Compensable {
                Some("slack.delete".into())
            } else {
                None
            },
            idempotency: Idempotency::Key,
            undo_of: None,
        }
    }

    /// Scripted executor: pops one outcome per call and records the order and keys.
    struct Script {
        outcomes: RefCell<VecDeque<ExecOutcome>>,
        calls: RefCell<Vec<(String, String)>>,
    }
    impl Script {
        fn new(o: Vec<ExecOutcome>) -> Self {
            Self {
                outcomes: RefCell::new(o.into()),
                calls: RefCell::new(vec![]),
            }
        }
        fn ok() -> ExecOutcome {
            ExecOutcome::Done {
                result: serde_json::json!({"ok": true}),
            }
        }
    }
    impl ActionExecutor for Script {
        fn execute(&self, e: &QueueEntry, idem: &str) -> ExecOutcome {
            self.calls
                .borrow_mut()
                .push((e.key.clone(), idem.to_string()));
            self.outcomes
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(Script::ok)
        }
    }

    struct Allow;
    impl SendPolicy for Allow {
        fn check(&self, _: &QueueEntry) -> PolicyVerdict {
            PolicyVerdict::Allow
        }
    }

    fn worker<'a>(
        q: &'a ActionQueue,
        clock: &'a ManualClock,
        ex: &'a Script,
        policy: &'a dyn SendPolicy,
    ) -> QueueWorker<'a> {
        QueueWorker {
            queue: q,
            clock,
            executor: ex,
            policy,
            retry: RetryPolicy::default(),
        }
    }

    #[test]
    fn enqueue_is_idempotent_and_batch_is_one_append() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        let batch = vec![
            act("d1", "a1", Reversibility::Compensable),
            act("d1", "a2", Reversibility::Irrecoverable),
        ];
        let r = q.enqueue_batch(&batch, &clock).unwrap();
        assert_eq!(r.added.len(), 2);
        let r = q.enqueue_batch(&batch, &clock).unwrap();
        assert!(r.added.is_empty());
        assert_eq!(r.already_present.len(), 2);
        let text = std::fs::read_to_string(q.journal_path()).unwrap();
        assert_eq!(text.lines().count(), 2, "re-enqueue must not append");
    }

    #[test]
    fn queue_survives_restart_and_never_double_sends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q");
        let clock = ManualClock::new(t0());
        {
            let q = ActionQueue::open(&path).unwrap();
            q.enqueue_batch(&[act("d1", "a1", Reversibility::Compensable)], &clock)
                .unwrap();
        }
        // "Restart": a brand-new handle replays the journal.
        let q = ActionQueue::open(&path).unwrap();
        assert_eq!(q.snapshot().unwrap().entries[0].state, QueueState::Queued);
        let ex = Script::new(vec![]);
        let w = worker(&q, &clock, &ex, &Allow);
        assert_eq!(w.run_once().unwrap().done.len(), 1);
        // Second restart and second pass: the key is done, nothing is sent again.
        let q2 = ActionQueue::open(&path).unwrap();
        let ex2 = Script::new(vec![]);
        let w2 = worker(&q2, &clock, &ex2, &Allow);
        assert!(w2.run_once().unwrap().done.is_empty());
        assert!(ex2.calls.borrow().is_empty());
        // Re-applying the same draft (re-enqueue) does not resurrect it.
        q2.enqueue_batch(&[act("d1", "a1", Reversibility::Compensable)], &clock)
            .unwrap();
        assert!(w2.run_once().unwrap().done.is_empty());
        assert_eq!(ex.calls.borrow().len(), 1);
        assert_eq!(
            ex.calls.borrow()[0].1,
            "d1:a1",
            "idempotency key is draft:action"
        );
    }

    #[test]
    fn retry_backoff_grows_and_respects_injected_clock() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(&[act("d1", "a1", Reversibility::Compensable)], &clock)
            .unwrap();
        let fail = || ExecOutcome::Retryable {
            message: "connection refused".into(),
        };
        let ex = Script::new(vec![fail(), fail(), fail(), Script::ok()]);
        let w = worker(&q, &clock, &ex, &Allow);

        assert_eq!(w.run_once().unwrap().retrying.len(), 1); // attempt 1 at t0
        let e = q.snapshot().unwrap().entries[0].clone();
        assert_eq!(e.next_attempt_at, Some(t0() + Duration::seconds(30)));
        // Not due yet: nothing happens, no sleeping needed.
        clock.advance(Duration::seconds(29));
        assert!(w.run_once().unwrap().retrying.is_empty());
        assert_eq!(ex.calls.borrow().len(), 1);
        clock.advance(Duration::seconds(1));
        assert_eq!(w.run_once().unwrap().retrying.len(), 1); // attempt 2
        let e = q.snapshot().unwrap().entries[0].clone();
        assert_eq!(e.next_attempt_at, Some(clock.now() + Duration::seconds(60)));
        clock.advance(Duration::seconds(60));
        assert_eq!(w.run_once().unwrap().retrying.len(), 1); // attempt 3
        let e = q.snapshot().unwrap().entries[0].clone();
        assert_eq!(
            e.next_attempt_at,
            Some(clock.now() + Duration::seconds(120))
        );
        clock.advance(Duration::seconds(120));
        assert_eq!(w.run_once().unwrap().done.len(), 1); // attempt 4 succeeds
        assert_eq!(ex.calls.borrow().len(), 4);
    }

    #[test]
    fn delay_is_capped() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay_after(1), Duration::seconds(30));
        assert_eq!(p.delay_after(2), Duration::seconds(60));
        assert_eq!(p.delay_after(30), p.cap);
        assert_eq!(p.delay_after(u32::MAX), p.cap);
    }

    #[test]
    fn retries_run_out_into_needs_attention() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(&[act("d1", "a1", Reversibility::Compensable)], &clock)
            .unwrap();
        let ex = Script::new(
            (0..10)
                .map(|_| ExecOutcome::Retryable {
                    message: "503".into(),
                })
                .collect(),
        );
        let w = worker(&q, &clock, &ex, &Allow);
        for _ in 0..10 {
            w.run_once().unwrap();
            clock.advance(Duration::hours(2));
        }
        let e = q.snapshot().unwrap().entries[0].clone();
        assert_eq!(e.state, QueueState::NeedsAttention);
        assert_eq!(
            e.attention.unwrap().reason,
            AttentionReason::RetriesExhausted
        );
        assert_eq!(ex.calls.borrow().len(), 6, "max_attempts is honoured");
    }

    #[test]
    fn crash_after_intent_is_unknown_outcome_and_never_resent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q");
        let clock = ManualClock::new(t0());
        let q = ActionQueue::open(&path).unwrap();
        q.enqueue_batch(&[act("d1", "a1", Reversibility::Irrecoverable)], &clock)
            .unwrap();
        // Simulate the process dying right after the fsynced intent: claim, then no result.
        let ex = Script::new(vec![]);
        let claim = q.claim("d1:a1", &clock, &Allow).unwrap();
        assert!(matches!(claim, Claim::Claimed { .. }));
        assert_eq!(q.snapshot().unwrap().entries[0].state, QueueState::Sending);
        drop(q);

        // Daemon restarts.
        let q = ActionQueue::open(&path).unwrap();
        let moved = q.recover_in_flight(&clock).unwrap();
        assert_eq!(moved, vec!["d1:a1".to_string()]);
        let e = q.snapshot().unwrap().entries[0].clone();
        assert_eq!(e.state, QueueState::NeedsAttention);
        assert_eq!(e.attention.unwrap().reason, AttentionReason::UnknownOutcome);
        // However long we wait, the worker never touches it.
        let w = worker(&q, &clock, &ex, &Allow);
        clock.advance(Duration::days(30));
        assert!(w.run_once().unwrap().done.is_empty());
        assert!(
            ex.calls.borrow().is_empty(),
            "unknown outcome must not be resent"
        );
    }

    #[test]
    fn unknown_and_panic_outcomes_need_attention_and_do_not_stop_others() {
        struct Boom;
        impl ActionExecutor for Boom {
            fn execute(&self, e: &QueueEntry, _: &str) -> ExecOutcome {
                if e.action.action_id == "a1" {
                    panic!("plugin exploded");
                }
                ExecOutcome::Done {
                    result: Value::Null,
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[
                act("d1", "a1", Reversibility::Compensable),
                act("d2", "a2", Reversibility::Compensable),
            ],
            &clock,
        )
        .unwrap();
        let w = QueueWorker {
            queue: &q,
            clock: &clock,
            executor: &Boom,
            policy: &Allow,
            retry: RetryPolicy::default(),
        };
        let r = w.run_once().unwrap();
        assert_eq!(r.needs_attention, vec!["d1:a1".to_string()]);
        assert_eq!(r.done, vec!["d2:a2".to_string()]);
        let snap = q.snapshot().unwrap();
        let a = snap.get("d1:a1").unwrap();
        assert_eq!(
            a.attention.as_ref().unwrap().reason,
            AttentionReason::UnknownOutcome
        );
        assert!(a
            .attention
            .as_ref()
            .unwrap()
            .message
            .contains("plugin exploded"));
    }

    #[test]
    fn policy_is_rechecked_every_attempt_and_fails_closed() {
        struct Flip(RefCell<bool>);
        impl SendPolicy for Flip {
            fn check(&self, _: &QueueEntry) -> PolicyVerdict {
                if *self.0.borrow() {
                    PolicyVerdict::Block {
                        reason: "workflow.toml now blocks email".into(),
                    }
                } else {
                    PolicyVerdict::Allow
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(&[act("d1", "a1", Reversibility::Compensable)], &clock)
            .unwrap();
        let policy = Flip(RefCell::new(false));
        let ex = Script::new(vec![ExecOutcome::Retryable {
            message: "503".into(),
        }]);
        let w = worker(&q, &clock, &ex, &policy);
        w.run_once().unwrap(); // attempt 1 allowed, fails retryably
        *policy.0.borrow_mut() = true; // policy tightens between attempts
        clock.advance(Duration::minutes(5));
        let r = w.run_once().unwrap();
        assert_eq!(r.needs_attention.len(), 1);
        assert_eq!(
            ex.calls.borrow().len(),
            1,
            "blocked retry must not reach the connector"
        );
        let e = q.snapshot().unwrap().entries[0].clone();
        let att = e.attention.unwrap();
        assert_eq!(att.reason, AttentionReason::PolicyBlocked);
        assert!(att.message.contains("blocks email"));
    }

    #[test]
    fn irrecoverable_runs_last_and_waits_for_the_rest_of_its_draft() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        // Enqueued with the irrecoverable one FIRST.
        q.enqueue_batch(
            &[
                act("d1", "mail", Reversibility::Irrecoverable),
                act("d1", "slack", Reversibility::Compensable),
            ],
            &clock,
        )
        .unwrap();
        let ex = Script::new(vec![ExecOutcome::Retryable {
            message: "503".into(),
        }]);
        let w = worker(&q, &clock, &ex, &Allow);
        let r = w.run_once().unwrap();
        assert_eq!(r.retrying, vec!["d1:slack".to_string()]);
        assert_eq!(r.held, vec!["d1:mail".to_string()], "mail waits for slack");
        assert_eq!(ex.calls.borrow().len(), 1);
        clock.advance(Duration::minutes(1));
        let r = w.run_once().unwrap();
        // slack succeeds and mail runs afterwards in the same pass.
        assert_eq!(r.done, vec!["d1:slack".to_string(), "d1:mail".to_string()]);
        let order: Vec<_> = ex.calls.borrow().iter().map(|c| c.0.clone()).collect();
        assert_eq!(order, vec!["d1:slack", "d1:slack", "d1:mail"]);
    }

    #[test]
    fn irrecoverable_of_another_draft_is_not_held() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[
                act("d1", "slack", Reversibility::Compensable),
                act("d2", "mail", Reversibility::Irrecoverable),
            ],
            &clock,
        )
        .unwrap();
        let ex = Script::new(vec![ExecOutcome::Rejected {
            message: "bad token".into(),
        }]);
        let w = worker(&q, &clock, &ex, &Allow);
        let r = w.run_once().unwrap();
        assert_eq!(r.needs_attention, vec!["d1:slack".to_string()]);
        assert_eq!(r.done, vec!["d2:mail".to_string()]);
    }

    #[test]
    fn irrecoverable_stays_held_while_a_sibling_needs_attention() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[
                act("d1", "slack", Reversibility::Compensable),
                act("d1", "mail", Reversibility::Irrecoverable),
            ],
            &clock,
        )
        .unwrap();
        let ex = Script::new(vec![ExecOutcome::Unknown {
            message: "timeout".into(),
        }]);
        let w = worker(&q, &clock, &ex, &Allow);
        w.run_once().unwrap();
        clock.advance(Duration::days(1));
        let r = w.run_once().unwrap();
        assert_eq!(r.held, vec!["d1:mail".to_string()]);
        assert_eq!(
            ex.calls.borrow().len(),
            1,
            "mail never runs while slack is unresolved"
        );
    }

    #[test]
    fn resolve_resend_requeues_and_sent_confirmed_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[
                act("d1", "a1", Reversibility::Compensable),
                act("d1", "a2", Reversibility::Compensable),
            ],
            &clock,
        )
        .unwrap();
        let ex = Script::new(vec![
            ExecOutcome::Unknown {
                message: "t/o".into(),
            },
            ExecOutcome::Unknown {
                message: "t/o".into(),
            },
        ]);
        worker(&q, &clock, &ex, &Allow).run_once().unwrap();
        // Refuses to resolve something that is not waiting on a person.
        q.enqueue_batch(&[act("d1", "a3", Reversibility::Compensable)], &clock)
            .unwrap();
        assert!(matches!(
            q.resolve("d1:a3", Resolution::Resend, "", &clock),
            Err(QueueError::Refused { .. })
        ));
        q.resolve("d1:a1", Resolution::SentConfirmed, "saw it arrive", &clock)
            .unwrap();
        q.resolve("d1:a2", Resolution::Resend, "did not arrive", &clock)
            .unwrap();
        let snap = q.snapshot().unwrap();
        assert_eq!(snap.get("d1:a1").unwrap().state, QueueState::Done);
        assert_eq!(snap.get("d1:a2").unwrap().state, QueueState::Queued);
    }

    #[test]
    fn undo_of_compensable_is_a_new_logged_entry_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(
            &[
                act("d1", "post", Reversibility::Compensable),
                act("d1", "mail", Reversibility::Irrecoverable),
            ],
            &clock,
        )
        .unwrap();
        // Not done yet: refused.
        assert!(q.request_undo("d1:post", &clock).is_err());
        let ex = Script::new(vec![]);
        worker(&q, &clock, &ex, &Allow).run_once().unwrap();
        // Irrecoverable has no undo.
        let err = q.request_undo("d1:mail", &clock).unwrap_err().to_string();
        assert!(err.contains("no recorded undo"), "{err}");
        let undo_key = q.request_undo("d1:post", &clock).unwrap();
        assert_eq!(undo_key, "d1:undo-post");
        assert!(q
            .request_undo("d1:post", &clock)
            .unwrap_err()
            .to_string()
            .contains("already undone"));
        let snap = q.snapshot().unwrap();
        assert_eq!(
            snap.get("d1:post").unwrap().undone_by.as_deref(),
            Some("d1:undo-post")
        );
        let u = snap.get(&undo_key).unwrap();
        assert_eq!(u.action.tool_name, "ta_external_action:slack.delete");
        assert_eq!(u.action.undo_of.as_deref(), Some("d1:post"));
        // The undo runs through the same worker, policy and journal.
        let r = worker(&q, &clock, &ex, &Allow).run_once().unwrap();
        assert_eq!(r.done, vec![undo_key]);
    }

    #[test]
    fn torn_tail_and_crlf_journal_replay_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let clock = ManualClock::new(t0());
        q.enqueue_batch(&[act("d1", "a1", Reversibility::Compensable)], &clock)
            .unwrap();
        // CRLF-normalised copy (as Windows tooling or git autocrlf would leave it).
        let text = std::fs::read_to_string(q.journal_path()).unwrap();
        std::fs::write(q.journal_path(), text.replace('\n', "\r\n")).unwrap();
        assert_eq!(q.snapshot().unwrap().entries.len(), 1);
        // A torn half-line at the end (crash mid-write) is skipped, and the next
        // append starts on a fresh line instead of being swallowed by it.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(q.journal_path())
            .unwrap();
        f.write_all(b"{\"event\":\"enqueued\",\"draft_id\":\"d9\"")
            .unwrap();
        drop(f);
        let snap = q.snapshot().unwrap();
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.skipped_lines, 1);
        q.enqueue_batch(&[act("d1", "a2", Reversibility::Compensable)], &clock)
            .unwrap();
        assert_eq!(q.snapshot().unwrap().entries.len(), 2);
    }

    #[test]
    fn two_handles_cannot_both_claim_the_same_action() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q");
        let a = ActionQueue::open(&path).unwrap();
        let b = ActionQueue::open(&path).unwrap();
        let clock = ManualClock::new(t0());
        a.enqueue_batch(&[act("d1", "a1", Reversibility::Compensable)], &clock)
            .unwrap();
        assert!(matches!(
            a.claim("d1:a1", &clock, &Allow).unwrap(),
            Claim::Claimed { .. }
        ));
        assert!(matches!(
            b.claim("d1:a1", &clock, &Allow).unwrap(),
            Claim::Skip
        ));
    }

    #[test]
    fn propose_actions_are_not_queueable() {
        assert!(!is_queueable_tool("ta_propose_task_create"));
        assert!(is_queueable_tool("ta_external_action:email"));
    }

    #[cfg(unix)]
    #[test]
    fn queue_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let q = ActionQueue::open(dir.path().join("q")).unwrap();
        let mode = std::fs::metadata(q.dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
