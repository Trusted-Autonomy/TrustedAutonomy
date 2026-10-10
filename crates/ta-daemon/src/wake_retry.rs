// wake_retry.rs: retry cap, backoff, dead-letter and launch-rate guard for
// wake-on-demand listeners (wake_listener.rs).
//
// Found live on the first real chief-of-staff run: a launch that exited
// non-zero left its message unacked, JetStream redelivered it, and the same
// message relaunched a paid model run three times in about three minutes,
// with nothing to stop it from doing so forever. This module bounds that:
//
// - Every launch attempt for one message (keyed by session, role, stream
//   key and the transport's stable msg_id) is recorded on disk BEFORE the
//   launch starts, so a daemon crash or restart mid-launch still counts it.
// - A failed attempt is retried only after an exponential backoff
//   (default 30s, 2m, 10m).
// - After `max_attempts` failures the message is dead-lettered: acked so it
//   is never redelivered, appended to `.ta/wake-dead-letter.jsonl`, logged
//   at error level, and emitted as a `CommandFailed` TA event.
// - Independently, a per-listener rolling-hour launch cap (default 6) makes
//   further launches wait rather than run, as a cost safety net.
//
// - Redelivery of work that already finished is harmless (v0.17.11.28). A
//   launch longer than the transport's ack wait used to be redelivered and
//   launched a second time (a second paid run). Now (1) a running launch
//   sends ack-progress heartbeats so the transport does not redeliver it,
//   (2) every completed or dead-lettered sequence is recorded on disk in
//   `.ta/wake-completed/` BEFORE it is acked, and a message whose sequence is
//   already recorded is acked and skipped (also across daemon restarts), and
//   (3) a launch that fails because its phase is already done or already
//   claimed is told apart from a real failure.
//
// All decisions take an explicit `now` so they are unit-testable without a
// real clock.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use ta_agent_whiteboard::{StreamEnvelope, WhiteboardConfig, WhiteboardTransport};

/// Retry/rate settings for wake-on-demand listeners, from `[whiteboard]` in
/// `.ta/workflow.toml` (see `WhiteboardConfig`'s `wake_*` fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeRetryPolicy {
    pub max_attempts: u32,
    pub backoff: Vec<Duration>,
    /// `0` disables the rate guard.
    pub max_launches_per_hour: u32,
    /// Longest one launch may run before it is killed and counted as failed.
    pub launch_timeout: Duration,
    /// How long the transport waits for an ack (or ack-progress) before
    /// redelivering a message. Derived from `launch_timeout` when unset.
    pub ack_wait: Duration,
    /// How often a running launch sends ack-progress. Zero disables it.
    pub ack_heartbeat: Duration,
}

/// Slack added on top of the launch timeout when deriving or checking the
/// ack wait, so a launch that runs right up to its timeout, then needs a
/// moment to be recorded and acked, is still not redelivered.
pub const ACK_WAIT_MARGIN: Duration = Duration::from_secs(60);

/// Upper bound for the derived heartbeat interval.
const MAX_DERIVED_HEARTBEAT: Duration = Duration::from_secs(10);

impl Default for WakeRetryPolicy {
    fn default() -> Self {
        Self::from_config(&WhiteboardConfig::default())
    }
}

impl WakeRetryPolicy {
    pub fn from_config(c: &WhiteboardConfig) -> Self {
        let launch_timeout = Duration::from_secs(c.wake_launch_timeout_secs);
        let ack_wait = c
            .wake_ack_wait_secs
            .map(Duration::from_secs)
            .unwrap_or(launch_timeout + ACK_WAIT_MARGIN);
        let ack_heartbeat = c
            .wake_ack_heartbeat_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| (ack_wait / 3).min(MAX_DERIVED_HEARTBEAT));
        Self {
            launch_timeout,
            ack_wait,
            ack_heartbeat,
            // A cap of 0 would dead-letter every message unread; treat it
            // as the minimum meaningful value instead.
            max_attempts: c.wake_max_attempts.max(1),
            backoff: c
                .wake_backoff_secs
                .iter()
                .map(|s| Duration::from_secs(*s))
                .collect(),
            max_launches_per_hour: c.wake_max_launches_per_hour,
        }
    }

    pub fn load(project_root: &Path) -> Self {
        Self::from_config(&WhiteboardConfig::load(project_root))
    }

    /// Checks the launch timeout, ack wait and heartbeat against each other.
    /// A listener must not run with settings that let the transport redeliver
    /// a message that is still being worked on. The error names the settings
    /// involved and how to fix them.
    pub fn validate_timings(&self) -> Result<(), String> {
        const WHERE: &str = "in the [whiteboard] section of .ta/workflow.toml";
        if self.launch_timeout.is_zero() {
            return Err(format!(
                "wake_launch_timeout_secs is 0, so every wake launch would be killed at once.                  Set it to the longest a launch may run (default 3600) {WHERE}."
            ));
        }
        let needed = self.launch_timeout + ACK_WAIT_MARGIN;
        if self.ack_wait < needed {
            return Err(format!(
                "wake_ack_wait_secs = {} is shorter than wake_launch_timeout_secs = {} plus the                  {}s margin, so the transport could redeliver a message while its launch is                  still running. Raise wake_ack_wait_secs to at least {}, remove it to derive it                  from the launch timeout, or lower wake_launch_timeout_secs, {WHERE}.",
                self.ack_wait.as_secs(),
                self.launch_timeout.as_secs(),
                ACK_WAIT_MARGIN.as_secs(),
                needed.as_secs(),
            ));
        }
        if self.ack_heartbeat.is_zero() || self.ack_heartbeat * 2 > self.ack_wait {
            return Err(format!(
                "wake_ack_heartbeat_secs = {} must be above 0 and at most half of                  wake_ack_wait_secs = {}, or heartbeats cannot keep a running launch from being                  redelivered. Set it between 1 and {}, or remove it to derive it, {WHERE}.",
                self.ack_heartbeat.as_secs(),
                self.ack_wait.as_secs(),
                (self.ack_wait / 2).as_secs().max(1),
            ));
        }
        Ok(())
    }

    /// Wait after the `failures`-th failure (1-based). The last configured
    /// value repeats; no values configured means 30s.
    pub fn backoff_after(&self, failures: u32) -> Duration {
        if self.backoff.is_empty() {
            return Duration::from_secs(30);
        }
        let i = (failures.saturating_sub(1) as usize).min(self.backoff.len() - 1);
        self.backoff[i]
    }
}

/// One message's launch history, persisted in the listener's attempts file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub session: String,
    pub role: String,
    pub key: String,
    pub msg_id: String,
    pub attempts: u32,
    pub first_attempt_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    /// Earliest time another attempt may start.
    pub next_eligible_at: DateTime<Utc>,
    /// Tail of the most recent failure (empty while an attempt is running).
    #[serde(default)]
    pub last_error: String,
    /// Set once the dead-letter record has been written, so a failed ack
    /// retried later does not write a second record.
    #[serde(default)]
    pub dead_lettered: bool,
}

/// On-disk attempt records for ONE listener (`.ta/wake-attempts/
/// <session>__<role>.json`). One file per listener means the listener's own
/// sequential loop is the only writer, so no cross-task locking is needed.
pub struct AttemptStore {
    path: PathBuf,
}

/// Replace anything outside `[A-Za-z0-9._-]` so ids are safe in file names.
pub fn sanitize_component(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "_".to_string()
    } else {
        out
    }
}

impl AttemptStore {
    pub fn for_listener(project_root: &Path, session: &str, role: &str) -> Self {
        Self {
            path: project_root.join(".ta").join("wake-attempts").join(format!(
                "{}__{}.json",
                sanitize_component(session),
                sanitize_component(role)
            )),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn record_key(key: &str, msg_id: &str) -> String {
        format!("{key}/{msg_id}")
    }

    fn load_all(&self) -> HashMap<String, AttemptRecord> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::error!(
                    path = %self.path.display(),
                    error = %e,
                    "wake_listener: attempts file is unreadable; starting from empty. Retry \
                     caps for messages already in flight restart from zero."
                );
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        }
    }

    fn save_all(&self, all: &HashMap<String, AttemptRecord>) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
        std::fs::rename(&tmp, &self.path)
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ATTEMPTS_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, key: &str, msg_id: &str) -> Option<AttemptRecord> {
        let _g = Self::lock();
        self.load_all().remove(&Self::record_key(key, msg_id))
    }

    pub fn put(&self, rec: &AttemptRecord) -> std::io::Result<()> {
        let _g = Self::lock();
        self.put_unlocked(rec)
    }

    fn put_unlocked(&self, rec: &AttemptRecord) -> std::io::Result<()> {
        let mut all = self.load_all();
        all.insert(Self::record_key(&rec.key, &rec.msg_id), rec.clone());
        self.save_all(&all)
    }

    pub fn remove(&self, key: &str, msg_id: &str) -> std::io::Result<()> {
        let _g = Self::lock();
        let mut all = self.load_all();
        if all.remove(&Self::record_key(key, msg_id)).is_some() {
            self.save_all(&all)?;
        }
        Ok(())
    }
}

/// How a finished sequence ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletedStatus {
    /// The launch succeeded (or its phase was already done).
    Completed,
    /// The message used up its attempts and was dead-lettered.
    DeadLettered,
}

impl CompletedStatus {
    fn as_str(self) -> &'static str {
        match self {
            CompletedStatus::Completed => "completed",
            CompletedStatus::DeadLettered => "dead-lettered",
        }
    }
}

/// One finished stream sequence, persisted in the listener's completed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedRecord {
    pub session: String,
    pub role: String,
    pub key: String,
    /// The transport's stable id for the message; for NATS `seq-<stream
    /// sequence>`, so it names the sequence and survives redelivery.
    pub msg_id: String,
    /// SHA-256 of the payload. A sequence number can repeat if the stream is
    /// deleted and recreated; a different payload under a recorded id means
    /// a new message, which must not be skipped.
    pub payload_sha256: String,
    pub status: CompletedStatus,
    pub completed_at: DateTime<Utc>,
}

/// Most finished sequences kept per listener. The oldest are dropped first.
const COMPLETED_RETENTION: usize = 1000;

pub fn payload_digest(payload: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(payload))
}

/// On-disk record of finished sequences for ONE listener
/// (`.ta/wake-completed/<session>__<role>.json`), written next to the
/// attempts file in the same style. It is what makes a redelivered message
/// harmless: it is acked and skipped, also after a daemon restart. To make a
/// message run again, delete its entry (or the file) and re-send it.
pub struct CompletedStore {
    path: PathBuf,
}

impl CompletedStore {
    pub fn for_listener(project_root: &Path, session: &str, role: &str) -> Self {
        Self {
            path: project_root
                .join(".ta")
                .join("wake-completed")
                .join(format!(
                    "{}__{}.json",
                    sanitize_component(session),
                    sanitize_component(role)
                )),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load_all(&self) -> HashMap<String, CompletedRecord> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::error!(
                    path = %self.path.display(),
                    error = %e,
                    "wake_listener: completed-sequence file is unreadable; starting from empty. \
                     Messages that already ran may run again if they are redelivered. Delete the \
                     file to silence this, or restore it from backup."
                );
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        }
    }

    fn save_all(&self, all: &HashMap<String, CompletedRecord>) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
        std::fs::rename(&tmp, &self.path)
    }

    pub fn get(&self, key: &str, msg_id: &str) -> Option<CompletedRecord> {
        let _g = AttemptStore::lock();
        self.load_all()
            .remove(&AttemptStore::record_key(key, msg_id))
    }

    pub fn record(&self, rec: &CompletedRecord) -> std::io::Result<()> {
        let _g = AttemptStore::lock();
        let mut all = self.load_all();
        all.insert(AttemptStore::record_key(&rec.key, &rec.msg_id), rec.clone());
        if all.len() > COMPLETED_RETENTION {
            let mut by_age: Vec<(String, DateTime<Utc>)> = all
                .iter()
                .map(|(k, r)| (k.clone(), r.completed_at))
                .collect();
            by_age.sort_by_key(|(_, at)| *at);
            for (k, _) in by_age.into_iter().take(all.len() - COMPLETED_RETENTION) {
                all.remove(&k);
            }
        }
        self.save_all(&all)
    }
}

/// Serializes every read-modify-write of attempt files in this daemon.
/// Several listeners can share one role (and so one attempts file and one
/// durable consumer): sibling `engineer` seats registered for capacity
/// compete for the same stream, so a redelivered message can reach a
/// different sibling than the one that tried it. All listeners run in the
/// one daemon process, so a process-wide lock is enough.
static ATTEMPTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// While an attempt is running, the message is leased for this long, so a
/// redelivery that reaches a sibling listener sharing the role cannot start
/// a parallel launch of the same message. A daemon crash mid-launch means
/// the message waits out the lease (or is dead-lettered if its attempts are
/// used up) instead of relaunching immediately.
const IN_FLIGHT_LEASE: chrono::Duration = chrono::Duration::hours(1);

/// Rolling one-hour launch counter for one listener.
#[derive(Debug, Default)]
pub struct LaunchRateGuard {
    launches: VecDeque<DateTime<Utc>>,
    last_warned_at: Option<DateTime<Utc>>,
}

impl LaunchRateGuard {
    fn prune(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::hours(1);
        while self.launches.front().is_some_and(|t| *t <= cutoff) {
            self.launches.pop_front();
        }
    }

    /// `None` when a launch is allowed now, else the time the oldest
    /// launch in the window ages out.
    pub fn blocked_until(
        &mut self,
        now: DateTime<Utc>,
        max_per_hour: u32,
    ) -> Option<DateTime<Utc>> {
        if max_per_hour == 0 {
            return None;
        }
        self.prune(now);
        if (self.launches.len() as u32) < max_per_hour {
            None
        } else {
            self.launches
                .front()
                .map(|t| *t + chrono::Duration::hours(1))
        }
    }

    pub fn record(&mut self, now: DateTime<Utc>) {
        self.launches.push_back(now);
    }

    /// Rate-limit warnings at most once a minute, not every 3s poll.
    fn should_warn(&mut self, now: DateTime<Utc>) -> bool {
        match self.last_warned_at {
            Some(t) if now - t < chrono::Duration::seconds(60) => false,
            _ => {
                self.last_warned_at = Some(now);
                true
            }
        }
    }
}

/// What to do with a message just read from the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Launch,
    /// A previous attempt failed; wait until this time.
    Backoff(DateTime<Utc>),
    /// Cap already reached (e.g. the daemon restarted after the last
    /// failure, or a previous ack failed): dead-letter without launching.
    DeadLetter,
}

pub fn decide(
    rec: Option<&AttemptRecord>,
    now: DateTime<Utc>,
    policy: &WakeRetryPolicy,
) -> Decision {
    match rec {
        None => Decision::Launch,
        Some(r) if r.dead_lettered || r.attempts >= policy.max_attempts => Decision::DeadLetter,
        Some(r) if now < r.next_eligible_at => Decision::Backoff(r.next_eligible_at),
        Some(_) => Decision::Launch,
    }
}

/// One dead-letter line in `.ta/wake-dead-letter.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadLetterRecord {
    pub msg_id: String,
    pub session: String,
    pub role: String,
    pub key: String,
    pub attempts: u32,
    pub last_error_tail: String,
    pub payload_bytes: usize,
    pub first_attempt_at: DateTime<Utc>,
    pub last_attempt_at: DateTime<Utc>,
    pub dead_lettered_at: DateTime<Utc>,
}

pub fn dead_letter_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("wake-dead-letter.jsonl")
}

fn append_dead_letter(project_root: &Path, rec: &DeadLetterRecord) -> std::io::Result<()> {
    use std::io::Write;
    let path = dead_letter_path(project_root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{}", serde_json::to_string(rec)?)
}

/// Last `n` lines of `s`.
pub fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Identity of the listener processing a message.
pub struct ListenerIds<'a> {
    pub project_root: &'a Path,
    pub session: &'a str,
    pub role: &'a str,
    pub key: &'a str,
    pub consumer: &'a str,
}

/// What happened to one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageOutcome {
    LaunchSucceeded,
    /// Launch failed and will be retried after a backoff.
    LaunchFailed,
    SkippedBackoff,
    SkippedRateLimited,
    DeadLettered,
    /// The sequence was already completed or dead-lettered (a redelivery).
    /// It was acked, not launched. `acked` is false when the ack failed and
    /// the message will come back.
    SkippedCompleted {
        acked: bool,
    },
    /// The launch reported its phase was already done, so the message was
    /// recorded as completed and acked rather than retried.
    PhaseAlreadyDone,
}

impl MessageOutcome {
    /// Whether the listener did real work this round (and so should loop
    /// immediately to drain any backlog instead of sleeping).
    pub fn made_progress(&self) -> bool {
        matches!(
            self,
            MessageOutcome::LaunchSucceeded
                | MessageOutcome::LaunchFailed
                | MessageOutcome::DeadLettered
                | MessageOutcome::PhaseAlreadyDone
                | MessageOutcome::SkippedCompleted { acked: true }
        )
    }
}

/// Text of the daemon's 409 when a phase is held by another run.
pub const PHASE_CLAIM_MARKER: &str = "could not be claimed";
/// Text of the daemon's 409 when a phase is already finished.
pub const PHASE_DONE_MARKER: &str = "is already done";

/// Why a launch failed, as far as its error text says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchFailure {
    /// `Phase <id> is already done`: the work this message asked for is
    /// finished. Retrying would only fail the same way.
    PhaseAlreadyDone,
    /// `Phase <id> could not be claimed`: another run holds the phase.
    PhaseClaimConflict,
    Other,
}

pub fn classify_launch_failure(err: &str) -> LaunchFailure {
    if err.contains("Phase ") && err.contains(PHASE_DONE_MARKER) {
        LaunchFailure::PhaseAlreadyDone
    } else if err.contains("Phase ") && err.contains(PHASE_CLAIM_MARKER) {
        LaunchFailure::PhaseClaimConflict
    } else {
        LaunchFailure::Other
    }
}

/// Polls `fut` to completion, sending an ack-progress to the transport every
/// `interval` meanwhile (none if `interval` is zero). The heartbeat lives
/// inside this one future, so it stops the moment `fut` finishes or is
/// dropped; nothing is left running after the launch exits.
async fn with_ack_progress<Fut: std::future::Future>(
    transport: &dyn WhiteboardTransport,
    ids: &ListenerIds<'_>,
    msg_id: &str,
    interval: Duration,
    fut: Fut,
) -> Fut::Output {
    tokio::pin!(fut);
    if interval.is_zero() {
        return fut.await;
    }
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failing = false;
    loop {
        tokio::select! {
            out = &mut fut => return out,
            _ = tick.tick() => {
                match transport.stream_ack_progress(ids.key, ids.consumer, msg_id).await {
                    Ok(()) => {
                        if failing {
                            tracing::info!(
                                session = %ids.session, role = %ids.role, msg_id = %msg_id,
                                "wake_listener: ack-progress heartbeat recovered"
                            );
                        }
                        failing = false;
                    }
                    Err(e) => {
                        if !failing {
                            tracing::warn!(
                                session = %ids.session, role = %ids.role, key = %ids.key,
                                msg_id = %msg_id, error = %e,
                                heartbeat_secs = interval.as_secs_f64(),
                                "wake_listener: ack-progress heartbeat failed; the transport may \
                                 redeliver this message while it runs. A redelivery is skipped \
                                 once the launch is recorded as completed. Check the transport \
                                 connection and wake_ack_heartbeat_secs / wake_ack_wait_secs in \
                                 [whiteboard] of .ta/workflow.toml."
                            );
                        }
                        failing = true;
                    }
                }
            }
        }
    }
}

/// Record `status` for this sequence. A write failure is logged, not fatal:
/// the ack that follows is still the primary guard against redelivery.
fn record_finished(
    ids: &ListenerIds<'_>,
    envelope: &StreamEnvelope,
    status: CompletedStatus,
    at: DateTime<Utc>,
) {
    let completed = CompletedStore::for_listener(ids.project_root, ids.session, ids.role);
    let rec = CompletedRecord {
        session: ids.session.to_string(),
        role: ids.role.to_string(),
        key: ids.key.to_string(),
        msg_id: envelope.msg_id.clone(),
        payload_sha256: payload_digest(&envelope.payload),
        status,
        completed_at: at,
    };
    if let Err(e) = completed.record(&rec) {
        tracing::error!(
            path = %completed.path().display(),
            session = %ids.session,
            role = %ids.role,
            msg_id = %envelope.msg_id,
            error = %e,
            "wake_listener: could not record the finished sequence. If the ack below also fails, \
             this message will launch again when redelivered. Check that .ta/ is writable."
        );
    }
}

/// Ack a message whose sequence is already recorded, without launching.
async fn skip_completed(
    transport: &dyn WhiteboardTransport,
    store: &AttemptStore,
    ids: &ListenerIds<'_>,
    envelope: &StreamEnvelope,
    rec: &CompletedRecord,
) -> MessageOutcome {
    tracing::info!(
        session = %ids.session,
        role = %ids.role,
        key = %ids.key,
        msg_id = %envelope.msg_id,
        status = rec.status.as_str(),
        first_completed_at = %rec.completed_at,
        "wake_listener: message was already {} at the time shown; acking the redelivery and \
         NOT launching again. To run it again, delete its entry from .ta/wake-completed/ and \
         re-send the message.",
        rec.status.as_str()
    );
    match transport
        .stream_ack(ids.key, ids.consumer, &envelope.msg_id)
        .await
    {
        Ok(()) => {
            let _ = store.remove(ids.key, &envelope.msg_id);
            MessageOutcome::SkippedCompleted { acked: true }
        }
        Err(e) => {
            tracing::error!(
                session = %ids.session,
                role = %ids.role,
                msg_id = %envelope.msg_id,
                error = %e,
                "wake_listener: could not ack an already-completed message; it will be \
                 delivered again and skipped again (no launch). Check the transport connection."
            );
            MessageOutcome::SkippedCompleted { acked: false }
        }
    }
}

/// Apply the retry policy to one message read from the stream. `launch`
/// runs the actual `ta run`; it returns `Err(message)` on failure.
#[allow(clippy::too_many_arguments)]
pub async fn process_message<F, Fut, C>(
    transport: &dyn WhiteboardTransport,
    store: &AttemptStore,
    policy: &WakeRetryPolicy,
    rate: &mut LaunchRateGuard,
    ids: &ListenerIds<'_>,
    envelope: &StreamEnvelope,
    clock: C,
    launch: F,
) -> MessageOutcome
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
    C: Fn() -> DateTime<Utc>,
{
    let now = clock();
    // Decide and claim atomically (see ATTEMPTS_LOCK): a sibling listener
    // sharing this role must never launch the same message concurrently.
    enum Begin {
        Completed(CompletedRecord),
        Launch(AttemptRecord),
        DeadLetter(AttemptRecord, bool),
        Done(MessageOutcome),
    }
    let completed_store = CompletedStore::for_listener(ids.project_root, ids.session, ids.role);
    let digest = payload_digest(&envelope.payload);
    let begin = {
        let _g = AttemptStore::lock();
        let finished = completed_store
            .load_all()
            .remove(&AttemptStore::record_key(ids.key, &envelope.msg_id));
        let existing = store
            .load_all()
            .remove(&AttemptStore::record_key(ids.key, &envelope.msg_id));
        match finished {
            Some(rec) if rec.payload_sha256 == digest => Some(Begin::Completed(rec)),
            Some(rec) => {
                tracing::warn!(
                    session = %ids.session,
                    role = %ids.role,
                    key = %ids.key,
                    msg_id = %envelope.msg_id,
                    first_completed_at = %rec.completed_at,
                    "wake_listener: this id was recorded as finished but the payload is \
                     different, so the stream was probably recreated and the id reused. \
                     Treating it as a NEW message and launching."
                );
                None
            }
            None => None,
        }
        .unwrap_or_else(|| match decide(existing.as_ref(), now, policy) {
            Decision::DeadLetter => {
                // `existing` is Some whenever decide() says DeadLetter.
                let mut rec = existing.expect("dead-letter decision implies a record");
                let first = !rec.dead_lettered;
                if first {
                    rec.dead_lettered = true;
                    let _ = store.put_unlocked(&rec);
                }
                Begin::DeadLetter(rec, first)
            }
            Decision::Backoff(until) => {
                tracing::debug!(
                    session = %ids.session,
                    role = %ids.role,
                    msg_id = %envelope.msg_id,
                    retry_at = %until,
                    "wake_listener: message in retry backoff or being launched by a sibling, \
                     not launching"
                );
                Begin::Done(MessageOutcome::SkippedBackoff)
            }
            Decision::Launch => {
                if let Some(until) = rate.blocked_until(now, policy.max_launches_per_hour) {
                    if rate.should_warn(now) {
                        tracing::warn!(
                            session = %ids.session,
                            role = %ids.role,
                            msg_id = %envelope.msg_id,
                            max_launches_per_hour = policy.max_launches_per_hour,
                            next_launch_allowed_at = %until,
                            "wake_listener: launch-rate cap reached, delaying launch (message \
                             kept, not dropped). Raise [whiteboard] wake_max_launches_per_hour \
                             in .ta/workflow.toml if this volume is expected."
                        );
                    }
                    Begin::Done(MessageOutcome::SkippedRateLimited)
                } else {
                    // Count the attempt BEFORE launching, so a daemon crash
                    // or restart in the middle of a launch still uses it up.
                    let attempt_no = existing.as_ref().map(|r| r.attempts).unwrap_or(0) + 1;
                    let rec = AttemptRecord {
                        session: ids.session.to_string(),
                        role: ids.role.to_string(),
                        key: ids.key.to_string(),
                        msg_id: envelope.msg_id.clone(),
                        attempts: attempt_no,
                        first_attempt_at: existing
                            .as_ref()
                            .map(|r| r.first_attempt_at)
                            .unwrap_or(now),
                        last_attempt_at: now,
                        next_eligible_at: now + IN_FLIGHT_LEASE,
                        last_error: existing.map(|r| r.last_error).unwrap_or_default(),
                        dead_lettered: false,
                    };
                    match store.put_unlocked(&rec) {
                        Ok(()) => {
                            rate.record(now);
                            Begin::Launch(rec)
                        }
                        Err(e) => {
                            tracing::error!(
                                path = %store.path().display(),
                                error = %e,
                                "wake_listener: could not persist the launch attempt; NOT \
                                 launching, so an unrecorded attempt can never slip past the \
                                 retry cap. Check that .ta/ is writable."
                            );
                            Begin::Done(MessageOutcome::SkippedBackoff)
                        }
                    }
                }
            }
        })
    };
    let mut rec = match begin {
        Begin::Completed(done) => {
            return skip_completed(transport, store, ids, envelope, &done).await
        }
        Begin::Launch(rec) => rec,
        Begin::DeadLetter(rec, first) => {
            return dead_letter(transport, store, ids, envelope, rec, first, now).await
        }
        Begin::Done(outcome) => return outcome,
    };

    let launched = with_ack_progress(
        transport,
        ids,
        &envelope.msg_id,
        policy.ack_heartbeat,
        launch(),
    )
    .await;
    match launched {
        Ok(()) => {
            // Record BEFORE acking: a crash or failed ack between the two
            // then makes the redelivery a skip, not a second launch.
            record_finished(ids, envelope, CompletedStatus::Completed, clock());
            if let Err(e) = transport
                .stream_ack(ids.key, ids.consumer, &envelope.msg_id)
                .await
            {
                tracing::warn!(
                    role = %ids.role,
                    key = %ids.key,
                    msg_id = %envelope.msg_id,
                    error = %e,
                    "wake_listener: failed to ack message after successful launch -- \
                     it will be redelivered and skipped (the launch is recorded as completed)"
                );
                return MessageOutcome::LaunchSucceeded;
            }
            let _ = store.remove(ids.key, &envelope.msg_id);
            MessageOutcome::LaunchSucceeded
        }
        Err(err) => {
            let failed_at = clock();
            // A sibling listener may have finished this very message while
            // this launch ran; then this failure is not a failure.
            if let Some(done) = completed_store.get(ids.key, &envelope.msg_id) {
                if done.payload_sha256 == digest {
                    return skip_completed(transport, store, ids, envelope, &done).await;
                }
            }
            let failure = classify_launch_failure(&err);
            if failure == LaunchFailure::PhaseAlreadyDone {
                tracing::info!(
                    session = %ids.session,
                    role = %ids.role,
                    key = %ids.key,
                    msg_id = %envelope.msg_id,
                    error = %tail_lines(&err, 3),
                    "wake_listener: the launch reports its phase is already done, so the work \
                     this message asked for is finished. Recording the message as completed and \
                     acking it; it is NOT retried."
                );
                record_finished(ids, envelope, CompletedStatus::Completed, failed_at);
                match transport
                    .stream_ack(ids.key, ids.consumer, &envelope.msg_id)
                    .await
                {
                    Ok(()) => {
                        let _ = store.remove(ids.key, &envelope.msg_id);
                    }
                    Err(e) => tracing::error!(
                        msg_id = %envelope.msg_id,
                        error = %e,
                        "wake_listener: could not ack the already-done message; it will be acked \
                         without a launch when redelivered"
                    ),
                }
                return MessageOutcome::PhaseAlreadyDone;
            }
            if failure == LaunchFailure::PhaseClaimConflict {
                tracing::warn!(
                    session = %ids.session,
                    role = %ids.role,
                    key = %ids.key,
                    msg_id = %envelope.msg_id,
                    attempt = rec.attempts,
                    "wake_listener: phase claim conflict. Another run holds the phase this \
                     launch needs, so this is not a fresh failure of the work itself. Run \
                     `ta goal list` to find the holder; if it is stuck, `ta goal delete <id>` or \
                     `ta plan reset <phase>` releases it. Retrying after backoff."
                );
            }
            rec.last_error = tail_lines(&err, 40);
            rec.next_eligible_at = failed_at
                + chrono::Duration::from_std(policy.backoff_after(rec.attempts))
                    .unwrap_or_else(|_| chrono::Duration::seconds(30));
            if rec.attempts >= policy.max_attempts {
                rec.dead_lettered = true;
                let _ = store.put(&rec);
                return dead_letter(transport, store, ids, envelope, rec, true, failed_at).await;
            }
            if let Err(e) = store.put(&rec) {
                tracing::warn!(error = %e, "wake_listener: could not persist failure detail");
            }
            tracing::error!(
                session = %ids.session,
                role = %ids.role,
                key = %ids.key,
                msg_id = %envelope.msg_id,
                attempt = rec.attempts,
                max_attempts = policy.max_attempts,
                retry_at = %rec.next_eligible_at,
                error = %err,
                "wake_listener: launch failed, message left unacked and will be retried after \
                 backoff"
            );
            MessageOutcome::LaunchFailed
        }
    }
}

async fn dead_letter(
    transport: &dyn WhiteboardTransport,
    store: &AttemptStore,
    ids: &ListenerIds<'_>,
    envelope: &StreamEnvelope,
    rec: AttemptRecord,
    write_record: bool,
    now: DateTime<Utc>,
) -> MessageOutcome {
    // `write_record` is true for exactly one caller per message (the claim
    // that first set `dead_lettered`), so siblings never write it twice.
    if write_record {
        let dl = DeadLetterRecord {
            msg_id: envelope.msg_id.clone(),
            session: ids.session.to_string(),
            role: ids.role.to_string(),
            key: ids.key.to_string(),
            attempts: rec.attempts,
            last_error_tail: rec.last_error.clone(),
            payload_bytes: envelope.payload.len(),
            first_attempt_at: rec.first_attempt_at,
            last_attempt_at: rec.last_attempt_at,
            dead_lettered_at: now,
        };
        let path = dead_letter_path(ids.project_root);
        if let Err(e) = append_dead_letter(ids.project_root, &dl) {
            tracing::error!(path = %path.display(), error = %e, "wake_listener: could not write dead-letter record");
        }
        tracing::error!(
            session = %ids.session,
            role = %ids.role,
            key = %ids.key,
            msg_id = %envelope.msg_id,
            attempts = rec.attempts,
            dead_letter_file = %path.display(),
            last_error = %rec.last_error,
            "wake_listener: DEAD-LETTERED message after reaching the retry cap. It will not be \
             redelivered. Inspect the dead-letter file and the launch logs under \
             .ta/logs/wake-launches/, fix the cause, then re-send the message if it still \
             matters."
        );
        {
            use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
            let store = FsEventStore::new(ids.project_root.join(".ta").join("events"));
            let event = SessionEvent::CommandFailed {
                command: format!(
                    "wake-on-demand {}/{} msg {} dead-lettered after {} attempts (see {})",
                    ids.session,
                    ids.role,
                    envelope.msg_id,
                    rec.attempts,
                    path.display()
                ),
                exit_code: -1,
                stderr: rec.last_error.clone(),
            };
            if let Err(e) = store.append(&EventEnvelope::new(event)) {
                tracing::warn!(error = %e, "wake_listener: could not emit dead-letter event");
            }
        }
    }

    // Before acking, so a redelivery after a failed ack is skipped.
    if CompletedStore::for_listener(ids.project_root, ids.session, ids.role)
        .get(ids.key, &envelope.msg_id)
        .is_none()
    {
        record_finished(ids, envelope, CompletedStatus::DeadLettered, now);
    }
    match transport
        .stream_ack(ids.key, ids.consumer, &envelope.msg_id)
        .await
    {
        Ok(()) => {
            let _ = store.remove(ids.key, &envelope.msg_id);
        }
        Err(e) => tracing::error!(
            msg_id = %envelope.msg_id,
            error = %e,
            "wake_listener: could not ack dead-lettered message; it will be acked (not \
             relaunched) the next time it is delivered"
        ),
    }
    MessageOutcome::DeadLettered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use ta_agent_whiteboard::InMemoryTransport;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn secs(n: i64) -> chrono::Duration {
        chrono::Duration::seconds(n)
    }

    struct Harness {
        dir: tempfile::TempDir,
        transport: InMemoryTransport,
        policy: WakeRetryPolicy,
        rate: LaunchRateGuard,
        launches: Arc<AtomicU32>,
    }

    impl Harness {
        async fn new() -> Self {
            let transport = InMemoryTransport::new();
            transport.connect().await.unwrap();
            Self {
                dir: tempfile::tempdir().unwrap(),
                transport,
                policy: WakeRetryPolicy::default(),
                rate: LaunchRateGuard::default(),
                launches: Arc::new(AtomicU32::new(0)),
            }
        }

        fn store(&self) -> AttemptStore {
            AttemptStore::for_listener(self.dir.path(), "sess-1", "chief-of-staff")
        }

        /// Read the next message and process it with a launcher that
        /// succeeds or fails as told.
        async fn step(&mut self, now: DateTime<Utc>, succeed: bool) -> Option<MessageOutcome> {
            let env = self
                .transport
                .stream_read_next("intake", "wake-listener:chief-of-staff")
                .await
                .unwrap()?;
            let store = self.store();
            let ids = ListenerIds {
                project_root: self.dir.path(),
                session: "sess-1",
                role: "chief-of-staff",
                key: "intake",
                consumer: "wake-listener:chief-of-staff",
            };
            let launches = self.launches.clone();
            Some(
                process_message(
                    &self.transport,
                    &store,
                    &self.policy,
                    &mut self.rate,
                    &ids,
                    &env,
                    move || now,
                    move || async move {
                        launches.fetch_add(1, Ordering::SeqCst);
                        if succeed {
                            Ok(())
                        } else {
                            Err("Error: No changes detected in staging workspace.".to_string())
                        }
                    },
                )
                .await,
            )
        }
    }

    #[tokio::test]
    async fn failing_message_is_retried_with_backoff_then_dead_lettered() {
        let mut h = Harness::new().await;
        h.transport
            .stream_append("intake", b"wake up".to_vec())
            .await
            .unwrap();

        // Attempt 1 fails.
        assert_eq!(
            h.step(t0(), false).await,
            Some(MessageOutcome::LaunchFailed)
        );
        // Immediate redelivery (the live bug) must NOT relaunch.
        assert_eq!(
            h.step(t0() + secs(1), false).await,
            Some(MessageOutcome::SkippedBackoff)
        );
        assert_eq!(h.launches.load(Ordering::SeqCst), 1);

        // After the first 30s backoff: attempt 2 fails.
        assert_eq!(
            h.step(t0() + secs(31), false).await,
            Some(MessageOutcome::LaunchFailed)
        );
        // Second backoff is 2 minutes, not 30s.
        assert_eq!(
            h.step(t0() + secs(31 + 60), false).await,
            Some(MessageOutcome::SkippedBackoff)
        );
        // Attempt 3 fails: cap reached, dead-lettered and acked.
        assert_eq!(
            h.step(t0() + secs(31 + 121), false).await,
            Some(MessageOutcome::DeadLettered)
        );
        assert_eq!(h.launches.load(Ordering::SeqCst), 3);

        // Never redelivered.
        assert_eq!(h.step(t0() + secs(10_000), false).await, None);

        let dl = std::fs::read_to_string(dead_letter_path(h.dir.path())).unwrap();
        let lines: Vec<&str> = dl.lines().collect();
        assert_eq!(lines.len(), 1);
        let rec: DeadLetterRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(rec.attempts, 3);
        assert_eq!(rec.role, "chief-of-staff");
        assert!(rec.last_error_tail.contains("No changes detected"));

        // An error-level TA event was emitted.
        let events_dir = h.dir.path().join(".ta/events");
        assert!(events_dir.exists(), "dead-letter must emit a TA event");
    }

    #[tokio::test]
    async fn a_sibling_listener_cannot_relaunch_a_message_already_in_flight() {
        // Two listeners share a role (and so the attempts file). While one
        // is launching a message, a redelivery of it to the sibling must not
        // start a second launch.
        let h = Harness::new().await;
        h.transport
            .stream_append("intake", b"x".to_vec())
            .await
            .unwrap();
        let store = h.store();
        let ids = ListenerIds {
            project_root: h.dir.path(),
            session: "sess-1",
            role: "engineer",
            key: "intake",
            consumer: "wake-listener:engineer",
        };
        let env = h
            .transport
            .stream_read_next("intake", "wake-listener:engineer")
            .await
            .unwrap()
            .unwrap();
        let mut rate_a = LaunchRateGuard::default();
        let mut rate_b = LaunchRateGuard::default();
        let policy = WakeRetryPolicy::default();
        let sibling_outcome = std::sync::Mutex::new(None);
        let outcome_a = process_message(
            &h.transport,
            &store,
            &policy,
            &mut rate_a,
            &ids,
            &env,
            t0,
            || async {
                // Mid-launch, the sibling sees the same message (60s later,
                // past the first 30s backoff).
                let b = process_message(
                    &h.transport,
                    &store,
                    &policy,
                    &mut rate_b,
                    &ids,
                    &env,
                    || t0() + secs(60),
                    || async { panic!("sibling must not launch an in-flight message") },
                )
                .await;
                *sibling_outcome.lock().unwrap() = Some(b);
                Ok(())
            },
        )
        .await;
        assert_eq!(outcome_a, MessageOutcome::LaunchSucceeded);
        assert_eq!(
            sibling_outcome.lock().unwrap().clone(),
            Some(MessageOutcome::SkippedBackoff)
        );
    }

    #[tokio::test]
    async fn attempts_survive_a_daemon_restart() {
        let mut h = Harness::new().await;
        h.transport
            .stream_append("intake", b"x".to_vec())
            .await
            .unwrap();
        assert_eq!(
            h.step(t0(), false).await,
            Some(MessageOutcome::LaunchFailed)
        );
        assert_eq!(
            h.step(t0() + secs(31), false).await,
            Some(MessageOutcome::LaunchFailed)
        );
        // "Restart": fresh in-memory state, same on-disk attempts file.
        h.rate = LaunchRateGuard::default();
        assert_eq!(
            h.step(t0() + secs(31 + 121), false).await,
            Some(MessageOutcome::DeadLettered)
        );
        assert_eq!(h.launches.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn success_acks_and_clears_the_attempt_record() {
        let mut h = Harness::new().await;
        h.transport
            .stream_append("intake", b"x".to_vec())
            .await
            .unwrap();
        assert_eq!(
            h.step(t0(), false).await,
            Some(MessageOutcome::LaunchFailed)
        );
        assert_eq!(
            h.step(t0() + secs(31), true).await,
            Some(MessageOutcome::LaunchSucceeded)
        );
        assert_eq!(h.step(t0() + secs(32), true).await, None);
        assert!(h.store().get("intake", "0").is_none());
    }

    #[tokio::test]
    async fn crash_mid_launch_still_counts_the_attempt() {
        // A record written before launch, with no outcome recorded (the
        // daemon died during `ta run`), still counts toward the cap.
        let mut h = Harness::new().await;
        h.policy.max_attempts = 1;
        h.transport
            .stream_append("intake", b"x".to_vec())
            .await
            .unwrap();
        h.store()
            .put(&AttemptRecord {
                session: "sess-1".into(),
                role: "chief-of-staff".into(),
                key: "intake".into(),
                msg_id: "0".into(),
                attempts: 1,
                first_attempt_at: t0(),
                last_attempt_at: t0(),
                next_eligible_at: t0(),
                last_error: String::new(),
                dead_lettered: false,
            })
            .unwrap();
        assert_eq!(
            h.step(t0() + secs(5), true).await,
            Some(MessageOutcome::DeadLettered)
        );
        assert_eq!(h.launches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn launch_rate_guard_delays_instead_of_launching() {
        let mut h = Harness::new().await;
        h.policy.max_launches_per_hour = 2;
        for i in 0..3 {
            h.transport
                .stream_append("intake", format!("m{i}").into_bytes())
                .await
                .unwrap();
        }
        assert_eq!(
            h.step(t0(), true).await,
            Some(MessageOutcome::LaunchSucceeded)
        );
        assert_eq!(
            h.step(t0() + secs(10), true).await,
            Some(MessageOutcome::LaunchSucceeded)
        );
        assert_eq!(
            h.step(t0() + secs(20), true).await,
            Some(MessageOutcome::SkippedRateLimited)
        );
        assert_eq!(h.launches.load(Ordering::SeqCst), 2);
        // Once the first launch ages out of the hour window, it proceeds.
        assert_eq!(
            h.step(t0() + secs(3601), true).await,
            Some(MessageOutcome::LaunchSucceeded)
        );
    }

    #[test]
    fn backoff_schedule_defaults_and_repeats_last_value() {
        let p = WakeRetryPolicy::default();
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.backoff_after(1), Duration::from_secs(30));
        assert_eq!(p.backoff_after(2), Duration::from_secs(120));
        assert_eq!(p.backoff_after(3), Duration::from_secs(600));
        assert_eq!(p.backoff_after(9), Duration::from_secs(600));
        assert_eq!(p.max_launches_per_hour, 6);
    }

    #[test]
    fn policy_reads_whiteboard_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/workflow.toml"),
            "[whiteboard]\nenabled = true\nwake_max_attempts = 2\nwake_backoff_secs = [5]\n",
        )
        .unwrap();
        let p = WakeRetryPolicy::load(dir.path());
        assert_eq!(p.max_attempts, 2);
        assert_eq!(p.backoff_after(4), Duration::from_secs(5));
    }

    #[test]
    fn tail_lines_keeps_the_end() {
        let s = (1..=100)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let t = tail_lines(&s, 40);
        assert!(t.starts_with("61\n"));
        assert!(t.ends_with("100"));
    }

    // ---- v0.17.11.28: idempotent wake launches ----

    use async_trait::async_trait;
    use ta_agent_whiteboard::error::Result as WbResult;

    /// In-memory transport that can fail acks and counts ack-progress calls.
    struct Spy {
        inner: InMemoryTransport,
        fail_acks: AtomicU32,
        progress: AtomicU32,
    }

    impl Spy {
        fn new(fail_acks: u32) -> Self {
            Self {
                inner: InMemoryTransport::new(),
                fail_acks: AtomicU32::new(fail_acks),
                progress: AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl WhiteboardTransport for Spy {
        fn backend_name(&self) -> &str {
            "spy"
        }
        async fn connect(&self) -> WbResult<()> {
            self.inner.connect().await
        }
        async fn kv_put(
            &self,
            b: &str,
            k: &str,
            v: Vec<u8>,
            ttl: Option<Duration>,
        ) -> WbResult<()> {
            self.inner.kv_put(b, k, v, ttl).await
        }
        async fn kv_create(&self, b: &str, k: &str, v: Vec<u8>) -> WbResult<bool> {
            self.inner.kv_create(b, k, v).await
        }
        async fn kv_get(&self, b: &str, k: &str) -> WbResult<Option<Vec<u8>>> {
            self.inner.kv_get(b, k).await
        }
        async fn kv_delete(&self, b: &str, k: &str) -> WbResult<()> {
            self.inner.kv_delete(b, k).await
        }
        async fn kv_list(&self, b: &str) -> WbResult<Vec<(String, Vec<u8>)>> {
            self.inner.kv_list(b).await
        }
        async fn stream_append(&self, s: &str, p: Vec<u8>) -> WbResult<()> {
            self.inner.stream_append(s, p).await
        }
        async fn stream_read_next(&self, s: &str, c: &str) -> WbResult<Option<StreamEnvelope>> {
            self.inner.stream_read_next(s, c).await
        }
        async fn stream_ack(&self, s: &str, c: &str, id: &str) -> WbResult<()> {
            if self
                .fail_acks
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(ta_agent_whiteboard::error::WhiteboardError::Stream {
                    stream: s.to_string(),
                    detail: "simulated ack failure".to_string(),
                });
            }
            self.inner.stream_ack(s, c, id).await
        }
        async fn stream_ack_progress(&self, s: &str, c: &str, id: &str) -> WbResult<()> {
            self.progress.fetch_add(1, Ordering::SeqCst);
            self.inner.stream_ack_progress(s, c, id).await
        }
        async fn stream_set_ack_wait(&self, s: &str, c: &str, w: Duration) -> WbResult<()> {
            self.inner.stream_set_ack_wait(s, c, w).await
        }
    }

    const CONSUMER: &str = "wake-listener:chief-of-staff";

    fn ids<'a>(dir: &'a Path) -> ListenerIds<'a> {
        ListenerIds {
            project_root: dir,
            session: "sess-1",
            role: "chief-of-staff",
            key: "intake",
            consumer: CONSUMER,
        }
    }

    /// Read one message and process it once with a launcher that returns
    /// `result`. Fresh `AttemptStore` and `LaunchRateGuard` every call, as
    /// after a daemon restart.
    async fn process_once(
        transport: &dyn WhiteboardTransport,
        dir: &Path,
        launches: &Arc<AtomicU32>,
        result: Result<(), String>,
    ) -> Option<MessageOutcome> {
        let env = transport
            .stream_read_next("intake", CONSUMER)
            .await
            .unwrap()?;
        let store = AttemptStore::for_listener(dir, "sess-1", "chief-of-staff");
        let mut rate = LaunchRateGuard::default();
        let launches = launches.clone();
        Some(
            process_message(
                transport,
                &store,
                &WakeRetryPolicy::default(),
                &mut rate,
                &ids(dir),
                &env,
                t0,
                move || async move {
                    launches.fetch_add(1, Ordering::SeqCst);
                    result
                },
            )
            .await,
        )
    }

    fn completed(dir: &Path) -> CompletedStore {
        CompletedStore::for_listener(dir, "sess-1", "chief-of-staff")
    }

    #[tokio::test]
    async fn a_launch_longer_than_the_ack_wait_is_not_redelivered() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(0);
        spy.stream_append("intake", b"slow".to_vec()).await.unwrap();
        spy.stream_set_ack_wait("intake", CONSUMER, Duration::from_millis(150))
            .await
            .unwrap();
        let env = spy
            .stream_read_next("intake", CONSUMER)
            .await
            .unwrap()
            .unwrap();
        let policy = WakeRetryPolicy {
            ack_heartbeat: Duration::from_millis(40),
            ..WakeRetryPolicy::default()
        };
        let store = AttemptStore::for_listener(dir.path(), "sess-1", "chief-of-staff");
        let mut rate = LaunchRateGuard::default();
        let done = std::sync::atomic::AtomicBool::new(false);
        let launches = Arc::new(AtomicU32::new(0));
        let l = launches.clone();

        let run = async {
            let out = process_message(
                &spy,
                &store,
                &policy,
                &mut rate,
                &ids(dir.path()),
                &env,
                t0,
                move || async move {
                    l.fetch_add(1, Ordering::SeqCst);
                    // Over three times the ack wait.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    Ok(())
                },
            )
            .await;
            done.store(true, Ordering::SeqCst);
            out
        };
        let poll = async {
            let mut redeliveries = 0;
            while !done.load(Ordering::SeqCst) {
                if spy
                    .stream_read_next("intake", CONSUMER)
                    .await
                    .unwrap()
                    .is_some()
                {
                    redeliveries += 1;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            redeliveries
        };
        let (outcome, redeliveries) = tokio::join!(run, poll);
        assert_eq!(outcome, MessageOutcome::LaunchSucceeded);
        assert_eq!(redeliveries, 0, "heartbeats must keep the message leased");
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert!(spy.progress.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn without_heartbeats_the_same_long_launch_is_redelivered() {
        // Control for the test above: proves it can fail.
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(0);
        spy.stream_append("intake", b"slow".to_vec()).await.unwrap();
        spy.stream_set_ack_wait("intake", CONSUMER, Duration::from_millis(150))
            .await
            .unwrap();
        let env = spy
            .stream_read_next("intake", CONSUMER)
            .await
            .unwrap()
            .unwrap();
        let policy = WakeRetryPolicy {
            ack_heartbeat: Duration::ZERO,
            ..WakeRetryPolicy::default()
        };
        let store = AttemptStore::for_listener(dir.path(), "sess-1", "chief-of-staff");
        let mut rate = LaunchRateGuard::default();
        let done = std::sync::atomic::AtomicBool::new(false);
        let run = async {
            let out = process_message(
                &spy,
                &store,
                &policy,
                &mut rate,
                &ids(dir.path()),
                &env,
                t0,
                || async {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    Ok(())
                },
            )
            .await;
            done.store(true, Ordering::SeqCst);
            out
        };
        let poll = async {
            let mut redeliveries = 0;
            while !done.load(Ordering::SeqCst) {
                if spy
                    .stream_read_next("intake", CONSUMER)
                    .await
                    .unwrap()
                    .is_some()
                {
                    redeliveries += 1;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            redeliveries
        };
        let (_, redeliveries) = tokio::join!(run, poll);
        assert!(redeliveries >= 1);
        assert_eq!(spy.progress.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn heartbeats_stop_after_the_launch_exits() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(0);
        spy.stream_append("intake", b"x".to_vec()).await.unwrap();
        let env = spy
            .stream_read_next("intake", CONSUMER)
            .await
            .unwrap()
            .unwrap();
        let policy = WakeRetryPolicy {
            ack_heartbeat: Duration::from_millis(20),
            ..WakeRetryPolicy::default()
        };
        let store = AttemptStore::for_listener(dir.path(), "sess-1", "chief-of-staff");
        let mut rate = LaunchRateGuard::default();
        let out = process_message(
            &spy,
            &store,
            &policy,
            &mut rate,
            &ids(dir.path()),
            &env,
            t0,
            || async {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Ok(())
            },
        )
        .await;
        assert_eq!(out, MessageOutcome::LaunchSucceeded);
        let at_exit = spy.progress.load(Ordering::SeqCst);
        assert!(at_exit >= 3, "heartbeats ran during the launch: {at_exit}");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(spy.progress.load(Ordering::SeqCst), at_exit);
    }

    #[tokio::test]
    async fn a_redelivered_completed_sequence_is_acked_and_not_relaunched() {
        let dir = tempfile::tempdir().unwrap();
        // The first ack fails, so the transport redelivers the message.
        let spy = Spy::new(1);
        spy.stream_append("intake", b"wake".to_vec()).await.unwrap();
        let launches = Arc::new(AtomicU32::new(0));

        assert_eq!(
            process_once(&spy, dir.path(), &launches, Ok(())).await,
            Some(MessageOutcome::LaunchSucceeded)
        );
        // Redelivery: acked and skipped, never launched.
        assert_eq!(
            process_once(&spy, dir.path(), &launches, Ok(())).await,
            Some(MessageOutcome::SkippedCompleted { acked: true })
        );
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert!(process_once(&spy, dir.path(), &launches, Ok(()))
            .await
            .is_none());
        let rec = completed(dir.path()).get("intake", "0").unwrap();
        assert_eq!(rec.status, CompletedStatus::Completed);
        assert_eq!(rec.role, "chief-of-staff");
    }

    #[tokio::test]
    async fn a_restart_between_launch_and_ack_does_not_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(1);
        spy.stream_append("intake", b"wake".to_vec()).await.unwrap();
        let launches = Arc::new(AtomicU32::new(0));
        process_once(&spy, dir.path(), &launches, Ok(())).await;

        // "Restart": nothing in memory survives except the .ta/ files, and
        // the transport (JetStream) still holds the unacked message.
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(completed(dir.path()).path()).unwrap())
                .unwrap();
        assert!(on_disk["intake/0"]["completed_at"].is_string());
        assert_eq!(
            process_once(&spy, dir.path(), &launches, Err("must not run".into())).await,
            Some(MessageOutcome::SkippedCompleted { acked: true })
        );
        assert_eq!(launches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_redelivered_dead_lettered_sequence_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(1);
        spy.stream_append("intake", b"bad".to_vec()).await.unwrap();
        let store = AttemptStore::for_listener(dir.path(), "sess-1", "chief-of-staff");
        store
            .put(&AttemptRecord {
                session: "sess-1".into(),
                role: "chief-of-staff".into(),
                key: "intake".into(),
                msg_id: "0".into(),
                attempts: 3,
                first_attempt_at: t0(),
                last_attempt_at: t0(),
                next_eligible_at: t0(),
                last_error: "boom".into(),
                dead_lettered: false,
            })
            .unwrap();
        let launches = Arc::new(AtomicU32::new(0));
        // Dead-lettered, but its ack fails.
        assert_eq!(
            process_once(&spy, dir.path(), &launches, Ok(())).await,
            Some(MessageOutcome::DeadLettered)
        );
        assert_eq!(
            completed(dir.path()).get("intake", "0").unwrap().status,
            CompletedStatus::DeadLettered
        );
        assert_eq!(
            process_once(&spy, dir.path(), &launches, Ok(())).await,
            Some(MessageOutcome::SkippedCompleted { acked: true })
        );
        assert_eq!(launches.load(Ordering::SeqCst), 0);
        let dl = std::fs::read_to_string(dead_letter_path(dir.path())).unwrap();
        assert_eq!(
            dl.lines().count(),
            1,
            "dead-letter record shape unchanged, written once"
        );
    }

    #[tokio::test]
    async fn a_reused_id_with_a_different_payload_is_a_new_message() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(0);
        spy.stream_append("intake", b"new content".to_vec())
            .await
            .unwrap();
        completed(dir.path())
            .record(&CompletedRecord {
                session: "sess-1".into(),
                role: "chief-of-staff".into(),
                key: "intake".into(),
                msg_id: "0".into(),
                payload_sha256: payload_digest(b"old content"),
                status: CompletedStatus::Completed,
                completed_at: t0(),
            })
            .unwrap();
        let launches = Arc::new(AtomicU32::new(0));
        assert_eq!(
            process_once(&spy, dir.path(), &launches, Ok(())).await,
            Some(MessageOutcome::LaunchSucceeded)
        );
        assert_eq!(launches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_launch_reporting_its_phase_already_done_is_completed_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(0);
        spy.stream_append("intake", b"x".to_vec()).await.unwrap();
        let launches = Arc::new(AtomicU32::new(0));
        let out = process_once(
            &spy,
            dir.path(),
            &launches,
            Err("Phase v0.0.0.1 is already done".into()),
        )
        .await;
        assert_eq!(out, Some(MessageOutcome::PhaseAlreadyDone));
        assert!(process_once(&spy, dir.path(), &launches, Ok(()))
            .await
            .is_none());
        assert_eq!(
            completed(dir.path()).get("intake", "0").unwrap().status,
            CompletedStatus::Completed
        );
        assert!(!dead_letter_path(dir.path()).exists());
    }

    #[tokio::test]
    async fn a_claim_failure_after_a_sibling_completed_the_message_is_not_a_failure() {
        // The live run-5 shape: the message finished, then a second launch
        // of it failed with "could not be claimed".
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(0);
        spy.stream_append("intake", b"x".to_vec()).await.unwrap();
        let env = spy
            .stream_read_next("intake", CONSUMER)
            .await
            .unwrap()
            .unwrap();
        let store = AttemptStore::for_listener(dir.path(), "sess-1", "chief-of-staff");
        let mut rate = LaunchRateGuard::default();
        let root = dir.path().to_path_buf();
        let digest = payload_digest(&env.payload);
        let out = process_message(
            &spy,
            &store,
            &WakeRetryPolicy::default(),
            &mut rate,
            &ids(dir.path()),
            &env,
            t0,
            move || async move {
                completed(&root)
                    .record(&CompletedRecord {
                        session: "sess-1".into(),
                        role: "chief-of-staff".into(),
                        key: "intake".into(),
                        msg_id: "0".into(),
                        payload_sha256: digest,
                        status: CompletedStatus::Completed,
                        completed_at: t0(),
                    })
                    .unwrap();
                Err("Phase v0.0.0.1 could not be claimed: already in progress".to_string())
            },
        )
        .await;
        assert_eq!(out, MessageOutcome::SkippedCompleted { acked: true });
        assert!(spy
            .stream_read_next("intake", CONSUMER)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_real_claim_conflict_is_still_retried_with_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let spy = Spy::new(0);
        spy.stream_append("intake", b"x".to_vec()).await.unwrap();
        let launches = Arc::new(AtomicU32::new(0));
        let out = process_once(
            &spy,
            dir.path(),
            &launches,
            Err("Phase v0.0.0.1 could not be claimed: already in progress".into()),
        )
        .await;
        assert_eq!(out, Some(MessageOutcome::LaunchFailed));
        let rec = AttemptStore::for_listener(dir.path(), "sess-1", "chief-of-staff")
            .get("intake", "0")
            .unwrap();
        assert!(rec.last_error.contains(PHASE_CLAIM_MARKER));
        assert!(completed(dir.path()).get("intake", "0").is_none());
    }

    #[test]
    fn launch_failures_are_classified() {
        assert_eq!(
            classify_launch_failure("Phase v1 is already done"),
            LaunchFailure::PhaseAlreadyDone
        );
        assert_eq!(
            classify_launch_failure(
                "Error: Phase v0.0.0.1 could not be claimed: already in progress"
            ),
            LaunchFailure::PhaseClaimConflict
        );
        assert_eq!(
            classify_launch_failure("No changes detected"),
            LaunchFailure::Other
        );
    }

    #[test]
    fn timing_settings_are_derived_and_validated() {
        let p = WakeRetryPolicy::default();
        assert_eq!(p.launch_timeout, Duration::from_secs(3600));
        assert_eq!(p.ack_wait, Duration::from_secs(3660));
        assert_eq!(p.ack_heartbeat, Duration::from_secs(10));
        assert!(p.validate_timings().is_ok());
        // Retry defaults are untouched.
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.max_launches_per_hour, 6);

        let short = WakeRetryPolicy::from_config(&WhiteboardConfig {
            wake_ack_wait_secs: Some(30),
            ..Default::default()
        });
        let err = short.validate_timings().unwrap_err();
        assert!(err.contains("wake_ack_wait_secs") && err.contains("wake_launch_timeout_secs"));
        assert!(err.contains("3660"), "names the minimum: {err}");

        let slow_beat = WakeRetryPolicy::from_config(&WhiteboardConfig {
            wake_ack_heartbeat_secs: Some(3000),
            ..Default::default()
        });
        assert!(slow_beat
            .validate_timings()
            .unwrap_err()
            .contains("wake_ack_heartbeat_secs"));

        let tight = WakeRetryPolicy::from_config(&WhiteboardConfig {
            wake_launch_timeout_secs: 20,
            ..Default::default()
        });
        assert_eq!(tight.ack_wait, Duration::from_secs(80));
        assert!(tight.validate_timings().is_ok());

        let zero = WakeRetryPolicy::from_config(&WhiteboardConfig {
            wake_launch_timeout_secs: 0,
            ..Default::default()
        });
        assert!(zero
            .validate_timings()
            .unwrap_err()
            .contains("wake_launch_timeout_secs"));
    }

    #[test]
    fn completed_store_keeps_only_the_most_recent_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = completed(dir.path());
        for i in 0..(COMPLETED_RETENTION + 5) {
            store
                .record(&CompletedRecord {
                    session: "sess-1".into(),
                    role: "chief-of-staff".into(),
                    key: "intake".into(),
                    msg_id: format!("seq-{i}"),
                    payload_sha256: String::new(),
                    status: CompletedStatus::Completed,
                    completed_at: t0() + secs(i as i64),
                })
                .unwrap();
        }
        assert!(store.get("intake", "seq-0").is_none());
        assert!(store.get("intake", "seq-4").is_none());
        assert!(store.get("intake", "seq-5").is_some());
        assert!(store
            .get("intake", &format!("seq-{}", COMPLETED_RETENTION + 4))
            .is_some());
    }

    #[test]
    fn an_unreadable_completed_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = completed(dir.path());
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "{ not json").unwrap();
        assert!(store.get("intake", "0").is_none());
    }
}
