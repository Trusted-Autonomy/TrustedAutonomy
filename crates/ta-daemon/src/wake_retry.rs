// wake_retry.rs — retry cap, backoff, dead-letter and launch-rate guard for
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
}

impl Default for WakeRetryPolicy {
    fn default() -> Self {
        Self::from_config(&WhiteboardConfig::default())
    }
}

impl WakeRetryPolicy {
    pub fn from_config(c: &WhiteboardConfig) -> Self {
        Self {
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

    pub fn get(&self, key: &str, msg_id: &str) -> Option<AttemptRecord> {
        self.load_all().remove(&Self::record_key(key, msg_id))
    }

    pub fn put(&self, rec: &AttemptRecord) -> std::io::Result<()> {
        let mut all = self.load_all();
        all.insert(Self::record_key(&rec.key, &rec.msg_id), rec.clone());
        self.save_all(&all)
    }

    pub fn remove(&self, key: &str, msg_id: &str) -> std::io::Result<()> {
        let mut all = self.load_all();
        if all.remove(&Self::record_key(key, msg_id)).is_some() {
            self.save_all(&all)?;
        }
        Ok(())
    }
}

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
        )
    }
}

/// Apply the retry policy to one message read from the stream. `launch`
/// runs the actual `ta run`; it returns `Err(message)` on failure.
#[allow(clippy::too_many_arguments)]
pub async fn process_message<F, Fut>(
    transport: &dyn WhiteboardTransport,
    store: &AttemptStore,
    policy: &WakeRetryPolicy,
    rate: &mut LaunchRateGuard,
    ids: &ListenerIds<'_>,
    envelope: &StreamEnvelope,
    now: DateTime<Utc>,
    launch: F,
) -> MessageOutcome
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let existing = store.get(ids.key, &envelope.msg_id);
    match decide(existing.as_ref(), now, policy) {
        Decision::DeadLetter => {
            // `existing` is Some whenever decide() says DeadLetter.
            let rec = existing.expect("dead-letter decision implies a record");
            return dead_letter(transport, store, ids, envelope, rec, now).await;
        }
        Decision::Backoff(until) => {
            tracing::debug!(
                session = %ids.session,
                role = %ids.role,
                msg_id = %envelope.msg_id,
                retry_at = %until,
                "wake_listener: message in retry backoff, not launching yet"
            );
            return MessageOutcome::SkippedBackoff;
        }
        Decision::Launch => {}
    }

    if let Some(until) = rate.blocked_until(now, policy.max_launches_per_hour) {
        if rate.should_warn(now) {
            tracing::warn!(
                session = %ids.session,
                role = %ids.role,
                msg_id = %envelope.msg_id,
                max_launches_per_hour = policy.max_launches_per_hour,
                next_launch_allowed_at = %until,
                "wake_listener: launch-rate cap reached, delaying launch (message kept, not \
                 dropped). Raise [whiteboard] wake_max_launches_per_hour in .ta/workflow.toml \
                 if this volume is expected."
            );
        }
        return MessageOutcome::SkippedRateLimited;
    }

    // Count the attempt BEFORE launching, so a daemon crash or restart in
    // the middle of a launch still uses up one attempt.
    let attempt_no = existing.as_ref().map(|r| r.attempts).unwrap_or(0) + 1;
    let mut rec = AttemptRecord {
        session: ids.session.to_string(),
        role: ids.role.to_string(),
        key: ids.key.to_string(),
        msg_id: envelope.msg_id.clone(),
        attempts: attempt_no,
        first_attempt_at: existing.as_ref().map(|r| r.first_attempt_at).unwrap_or(now),
        last_attempt_at: now,
        next_eligible_at: now
            + chrono::Duration::from_std(policy.backoff_after(attempt_no))
                .unwrap_or_else(|_| chrono::Duration::seconds(30)),
        last_error: existing.map(|r| r.last_error).unwrap_or_default(),
        dead_lettered: false,
    };
    if let Err(e) = store.put(&rec) {
        tracing::error!(
            path = %store.path().display(),
            error = %e,
            "wake_listener: could not persist the launch attempt; NOT launching, so an \
             unrecorded attempt can never slip past the retry cap. Check that .ta/ is writable."
        );
        return MessageOutcome::SkippedBackoff;
    }
    rate.record(now);

    match launch().await {
        Ok(()) => {
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
                     it will be redelivered and count against its retry cap"
                );
                return MessageOutcome::LaunchSucceeded;
            }
            let _ = store.remove(ids.key, &envelope.msg_id);
            MessageOutcome::LaunchSucceeded
        }
        Err(err) => {
            rec.last_error = tail_lines(&err, 40);
            if rec.attempts >= policy.max_attempts {
                return dead_letter(transport, store, ids, envelope, rec, now).await;
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
    mut rec: AttemptRecord,
    now: DateTime<Utc>,
) -> MessageOutcome {
    if !rec.dead_lettered {
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
        rec.dead_lettered = true;
        let _ = store.put(&rec);
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
                    now,
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
}
