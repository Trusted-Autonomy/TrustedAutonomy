// seatbelt_log.rs: Seatbelt denials from the macOS unified log (v0.17.12.7).
//
// The v2 profile tags every explicit deny rule with `(with message "ta:<goal>")`
// (the default rule stays untagged, see seatbelt.rs), so a denial of a protected
// path carries that tag in the unified log:
//
//   Sandbox: cat(4242) deny(1) file-read-data /Users/u/.ssh/id_ed25519
//   ta:e0d16d67
//
// The kernel collapses repeats into "N duplicate reports for Sandbox: ...".
// `parse_message` is pure so it is testable on every platform; `ViolationTail`
// runs `/usr/bin/log stream` (live) and `/usr/bin/log show` (persisted) and is
// only useful on macOS.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One parsed denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub process: String,
    pub pid: u32,
    pub operation: String,
    pub target: String,
    /// Occurrences this report stands for (1 unless the kernel collapsed repeats).
    pub count: u32,
}

impl Violation {
    /// Why it happened and what the owner can do, for the event and the terminal.
    pub fn explain(&self) -> String {
        format!(
            "the sandbox profile denied {} on {} for {}({}). If the agent legitimately needs \
             this, widen the policy in the TA state dir policy.toml; otherwise this is the \
             sandbox working as intended.",
            self.operation, self.target, self.process, self.pid
        )
    }
}

/// Parse a unified-log `eventMessage`. Returns `None` unless it is a Sandbox
/// denial carrying exactly `tag` on its own line.
pub fn parse_message(message: &str, tag: &str) -> Option<Violation> {
    let mut lines = message.lines();
    let first = lines.next()?.trim();
    if !lines.any(|l| l.trim() == tag) {
        return None;
    }

    let (count, body) = match first.split_once(" duplicate reports for ") {
        Some((n, rest)) => (n.trim().parse::<u32>().ok()?, rest),
        None => (1, first),
    };
    let body = body.strip_prefix("Sandbox: ")?;
    // `proc(pid) deny(n) operation target...`; the process name may hold spaces.
    let (proc_part, rest) = body.split_once(") deny(")?;
    let (process, pid) = proc_part.rsplit_once('(')?;
    let pid = pid.parse::<u32>().ok()?;
    let (_, rest) = rest.split_once(')')?;
    let rest = rest.trim_start();
    let (operation, target) = match rest.split_once(' ') {
        Some((op, t)) => (op, t.trim()),
        None => (rest, ""),
    };
    if operation.is_empty() {
        return None;
    }
    Some(Violation {
        process: process.to_string(),
        pid,
        operation: operation.to_string(),
        target: target.to_string(),
        count,
    })
}

/// Parse one `--style ndjson` line from `log stream` or `log show`.
pub fn parse_ndjson_line(line: &str, tag: &str) -> Option<Violation> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    parse_message(v.get("eventMessage")?.as_str()?, tag)
}

/// Where a log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The live `log stream`.
    Live,
    /// The persisted store, read with `log show` after the run.
    Persisted,
}

/// The line of a canary's message: `<goal tag>:c:<nonce>`. It is never equal to
/// the goal tag, so a canary can never be mistaken for a goal's denial.
fn canary_tag(tag: &str, nonce: &str) -> String {
    format!("{tag}:c:{nonce}")
}

/// The nonce of a canary denial, when `message` is one for `tag`.
fn parse_canary(message: &str, tag: &str) -> Option<String> {
    let prefix = format!("{tag}:c:");
    message
        .lines()
        .skip(1)
        .find_map(|l| l.trim().strip_prefix(&prefix).map(str::to_string))
}

/// Everything the reader thread, the canary probes and the reconciliation share.
struct Shared {
    tag: String,
    on_violation: Arc<dyn Fn(Violation) + Send + Sync>,
    /// Keys (`machTimestamp|message`) of denials already delivered, so the
    /// live stream and the persisted log never report one denial twice.
    delivered: Mutex<HashSet<String>>,
    canaries_live: Mutex<HashSet<String>>,
    canaries_persisted: Mutex<HashSet<String>>,
    lines_seen: AtomicU64,
    live_denials: AtomicU64,
    recovered_denials: AtomicU64,
}

impl Shared {
    fn ingest(&self, line: &str, source: Source) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        let Some(message) = v.get("eventMessage").and_then(|m| m.as_str()) else {
            return;
        };
        if let Some(nonce) = parse_canary(message, &self.tag) {
            let set = match source {
                Source::Live => &self.canaries_live,
                Source::Persisted => &self.canaries_persisted,
            };
            if let Ok(mut set) = set.lock() {
                set.insert(nonce);
            }
            return;
        }
        let Some(violation) = parse_message(message, &self.tag) else {
            return;
        };
        let stamp = v
            .get("machTimestamp")
            .map(|t| t.to_string())
            .or_else(|| v.get("timestamp").map(|t| t.to_string()))
            .unwrap_or_default();
        let fresh = self
            .delivered
            .lock()
            .map(|mut d| d.insert(format!("{stamp}|{message}")))
            .unwrap_or(true);
        if !fresh {
            return;
        }
        match source {
            Source::Live => self.live_denials.fetch_add(1, Ordering::Relaxed),
            Source::Persisted => self.recovered_denials.fetch_add(1, Ordering::Relaxed),
        };
        (self.on_violation)(violation);
    }

    fn canary_seen(&self, nonce: &str, source: Source) -> bool {
        let set = match source {
            Source::Live => &self.canaries_live,
            Source::Persisted => &self.canaries_persisted,
        };
        set.lock().map(|s| s.contains(nonce)).unwrap_or(false)
    }
}

/// How one canary check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Canary {
    /// Not attempted (the stream did not start, or this phase did not run).
    NotRun,
    /// A denial TA caused on purpose came back through the live stream.
    Delivered { attempts: u32, waited_ms: u64 },
    /// No canary denial came back in time, so denials during this window may
    /// have been dropped by the OS.
    Missed { attempts: u32, waited_ms: u64 },
}

impl Canary {
    pub fn delivered(&self) -> bool {
        matches!(self, Canary::Delivered { .. })
    }
}

/// What one goal's denial capture achieved. The unified log is lossy (see
/// the module notes on `ViolationTail`); this says how far to trust the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureReport {
    /// Canary run before the agent started.
    pub start: Canary,
    /// Canary run after the agent exited.
    pub end: Canary,
    /// The persisted log was read and had caught up to the end canary, so every
    /// denial the OS kept before the agent exited has been looked at.
    pub reconciled: bool,
    /// Tagged denials delivered by the live stream.
    pub live_denials: u64,
    /// Tagged denials the live stream missed that the persisted log had.
    pub recovered_denials: u64,
    /// Raw `Sandbox` lines the live stream delivered.
    pub stream_lines: u64,
    /// Why capture is not verified, when it is not. Empty when healthy.
    pub problems: Vec<String>,
}

impl CaptureReport {
    /// Both canaries came back live and the persisted log was reconciled.
    pub fn healthy(&self) -> bool {
        self.problems.is_empty()
    }

    /// One line for the terminal.
    pub fn summary(&self) -> String {
        if self.healthy() {
            format!(
                "denial capture verified (canary denials delivered at start and end; {} live, {} \
                 recovered from the persisted log)",
                self.live_denials, self.recovered_denials
            )
        } else {
            format!(
                "denial capture NOT verified: {}. Denials during this goal may be missing from \
                 `ta sandbox explain`; the sandbox still enforced.",
                self.problems.join("; ")
            )
        }
    }
}

/// Time a canary waits for one attempt to come back before trying again.
const CANARY_ATTEMPT_WAIT: Duration = Duration::from_millis(1500);
/// Longest `log show` is allowed to run before it is killed.
const LOG_SHOW_TIMEOUT: Duration = Duration::from_secs(60);

static CANARY_SEQ: AtomicU64 = AtomicU64::new(0);

/// Tails the unified log for one goal's denials until dropped.
///
/// Why this is more than `log stream`: measured on macOS 14, the live stream
/// loses denials that the persisted log still has (1 to 6 of every 100 in
/// bursts, more under load), and the OS itself keeps no record of some
/// isolated denials at all, with no loss marker in either output. So capture
/// is three parts. The live stream gives immediacy. [`ViolationTail::probe`]
/// denies a read on purpose, retrying until the stream shows it, which proves
/// the pipeline is attached before the agent starts and still working after it
/// exits. [`ViolationTail::finish`] then reads the persisted log with
/// `log show` once the persisted copy of the end canary proves it is up to
/// date, and delivers whatever the stream missed. A denial the OS never logged
/// cannot be recovered, so `finish` reports what it could verify and what not.
pub struct ViolationTail {
    child: Child,
    thread: Option<std::thread::JoinHandle<()>>,
    shared: Arc<Shared>,
    started_at: chrono::DateTime<chrono::Local>,
}

impl ViolationTail {
    /// Start `/usr/bin/log stream` and call `on_violation` for each denial
    /// tagged `tag`. Fails with guidance if the `log` tool cannot be started.
    pub fn start(
        tag: String,
        on_violation: Arc<dyn Fn(Violation) + Send + Sync>,
    ) -> std::io::Result<Self> {
        let started_at = chrono::Local::now();
        let mut child = Command::new("/usr/bin/log")
            .args([
                "stream",
                "--style",
                "ndjson",
                "--predicate",
                r#"sender == "Sandbox""#,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "could not start /usr/bin/log to watch sandbox denials: {e}. The \
                         sandbox still enforces; only denial events are unavailable."
                    ),
                )
            })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::other("/usr/bin/log gave no stdout; sandbox denial events are off")
        })?;
        let shared = Arc::new(Shared {
            tag,
            on_violation,
            delivered: Mutex::new(HashSet::new()),
            canaries_live: Mutex::new(HashSet::new()),
            canaries_persisted: Mutex::new(HashSet::new()),
            lines_seen: AtomicU64::new(0),
            live_denials: AtomicU64::new(0),
            recovered_denials: AtomicU64::new(0),
        });
        let reader = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("ta-seatbelt-log".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    reader.lines_seen.fetch_add(1, Ordering::Relaxed);
                    reader.ingest(&line, Source::Live);
                }
            })?;
        Ok(Self {
            child,
            thread: Some(thread),
            shared,
            started_at,
        })
    }

    /// Raw lines `log stream` has delivered so far, tagged or not. Zero after a
    /// busy window means the stream itself is blind (restricted log access), as
    /// opposed to the stream working while a denial went missing.
    pub fn lines_seen(&self) -> u64 {
        self.shared.lines_seen.load(Ordering::Relaxed)
    }

    /// Deny a read on purpose, over and over, until the live stream shows one
    /// or `timeout` passes. Each attempt is a fresh denial of a file TA creates
    /// and removes, tagged with this goal's tag plus a nonce, so it is never
    /// reported as a goal denial. Returns what happened and the nonces used.
    fn probe_inner(&self, timeout: Duration) -> (Canary, Vec<String>) {
        let begin = Instant::now();
        let mut nonces = Vec::new();
        let mut attempts = 0u32;
        while begin.elapsed() < timeout {
            attempts += 1;
            let nonce = format!(
                "{:x}{:x}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                CANARY_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            nonces.push(nonce.clone());
            if let Err(e) = self.deny_once(&nonce) {
                tracing::warn!(error = %e, "could not run the sandbox denial canary");
                return (
                    Canary::Missed {
                        attempts,
                        waited_ms: begin.elapsed().as_millis() as u64,
                    },
                    nonces,
                );
            }
            let wait_until = Instant::now() + CANARY_ATTEMPT_WAIT;
            while Instant::now() < wait_until {
                if self.shared.canary_seen(&nonce, Source::Live) {
                    return (
                        Canary::Delivered {
                            attempts,
                            waited_ms: begin.elapsed().as_millis() as u64,
                        },
                        nonces,
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        (
            Canary::Missed {
                attempts,
                waited_ms: begin.elapsed().as_millis() as u64,
            },
            nonces,
        )
    }

    /// Prove the live stream is attached and delivering: see [`Self::probe_inner`].
    /// Call it before spawning the agent; a [`Canary::Missed`] result means denials
    /// may be dropped for this goal and `finish` will say so.
    pub fn probe(&self, timeout: Duration) -> Canary {
        self.probe_inner(timeout).0
    }

    /// One tagged denial of a file nobody else can read the denial of.
    fn deny_once(&self, nonce: &str) -> std::io::Result<()> {
        let dir = std::fs::canonicalize(std::env::temp_dir())?;
        let file = dir.join(format!("ta-seatbelt-canary-{nonce}"));
        std::fs::write(&file, b"canary")?;
        let path = file.to_string_lossy().into_owned();
        let profile = format!(
            "(version 1)\n(allow default)\n(deny file-read-data (literal \"{}\") (with message \"{}\"))\n",
            crate::seatbelt::escape(&path),
            crate::seatbelt::escape(&canary_tag(&self.shared.tag, nonce)),
        );
        let result = Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/bin/cat", &path])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_file(&file);
        result.map(|_| ())
    }

    /// Read the persisted log from the start of this tail and deliver every
    /// tagged denial the live stream missed.
    fn read_persisted(&self) -> std::io::Result<()> {
        let start = (self.started_at - chrono::Duration::seconds(2))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let predicate = format!(
            r#"sender == "Sandbox" AND eventMessage CONTAINS "{}""#,
            self.shared.tag.replace('"', "")
        );
        let mut child = Command::new("/usr/bin/log")
            .args([
                "show",
                "--style",
                "ndjson",
                "--start",
                &start,
                "--predicate",
                &predicate,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("/usr/bin/log show gave no stdout"))?;
        let shared = Arc::clone(&self.shared);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                shared.ingest(&line, Source::Persisted);
            }
        });
        let begin = Instant::now();
        loop {
            match child.try_wait()? {
                Some(_) => break,
                None if begin.elapsed() > LOG_SHOW_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "/usr/bin/log show did not finish in {}s",
                            LOG_SHOW_TIMEOUT.as_secs()
                        ),
                    ));
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        let _ = reader.join();
        Ok(())
    }

    /// Finish the capture after the agent has exited: prove the stream still
    /// works (end canary), read the persisted log until it contains that
    /// canary, deliver the denials the stream missed, and stop the tail.
    /// `start` is the result of the canary [`Self::probe`] run before the spawn.
    pub fn finish(mut self, start: Canary, timeout: Duration) -> CaptureReport {
        let (end, nonces) = self.probe_inner(timeout);
        let mut problems = Vec::new();
        match start {
            Canary::Delivered { .. } => {}
            Canary::NotRun => problems
                .push("the live log stream was not verified before the agent started".to_string()),
            Canary::Missed {
                attempts,
                waited_ms,
            } => problems.push(format!(
                "the live log stream did not deliver a canary denial before the agent started \
                 ({attempts} attempt(s), {waited_ms} ms)"
            )),
        }
        if let Canary::Missed {
            attempts,
            waited_ms,
        } = end
        {
            problems.push(format!(
                "the live log stream did not deliver a canary denial after the agent exited \
                 ({attempts} attempt(s), {waited_ms} ms)"
            ));
        }
        // The persisted log lags the stream. Read it until it holds one of the
        // end canaries: that proves it covers everything up to the agent's exit.
        let mut reconciled = false;
        let mut read_error = None;
        let read_deadline = Instant::now() + timeout.max(Duration::from_secs(1));
        loop {
            match self.read_persisted() {
                Ok(()) => {
                    if nonces
                        .iter()
                        .any(|n| self.shared.canary_seen(n, Source::Persisted))
                    {
                        reconciled = true;
                        break;
                    }
                }
                Err(e) => {
                    read_error = Some(e.to_string());
                    break;
                }
            }
            if Instant::now() >= read_deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(750));
        }
        if !reconciled {
            problems.push(match read_error {
                Some(e) => format!("the persisted log could not be read ({e})"),
                None => "the persisted log did not catch up to the end canary in time, so \
                         denials the live stream missed may not have been recovered"
                    .to_string(),
            });
        }
        let report = CaptureReport {
            start,
            end,
            reconciled,
            live_denials: self.shared.live_denials.load(Ordering::Relaxed),
            recovered_denials: self.shared.recovered_denials.load(Ordering::Relaxed),
            stream_lines: self.lines_seen(),
            problems,
        };
        self.stop();
        report
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ViolationTail {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "ta:e0d16d67";

    #[test]
    fn parses_a_file_denial() {
        let m = "Sandbox: cat(4242) deny(1) file-read-data /Users/u/.ssh/id_ed25519\nta:e0d16d67";
        let v = parse_message(m, TAG).unwrap();
        assert_eq!(v.process, "cat");
        assert_eq!(v.pid, 4242);
        assert_eq!(v.operation, "file-read-data");
        assert_eq!(v.target, "/Users/u/.ssh/id_ed25519");
        assert_eq!(v.count, 1);
    }

    #[test]
    fn target_may_contain_spaces_and_parens_in_the_process_name() {
        let m = "Sandbox: Google Chrome(7) deny(1) file-write-create /Users/u/My Docs/a b.txt\nta:e0d16d67";
        let v = parse_message(m, TAG).unwrap();
        assert_eq!(v.process, "Google Chrome");
        assert_eq!(v.target, "/Users/u/My Docs/a b.txt");
    }

    #[test]
    fn parses_mach_and_network_denials_and_missing_target() {
        let m = "Sandbox: git(9) deny(1) mach-lookup com.apple.SecurityServer\nta:e0d16d67";
        assert_eq!(
            parse_message(m, TAG).unwrap().target,
            "com.apple.SecurityServer"
        );
        let n = "Sandbox: curl(10) deny(1) network-outbound 127.0.0.1:7700\nta:e0d16d67";
        assert_eq!(parse_message(n, TAG).unwrap().operation, "network-outbound");
        let bare = "Sandbox: sh(11) deny(1) process-info-pidinfo\nta:e0d16d67";
        let v = parse_message(bare, TAG).unwrap();
        assert_eq!(
            (v.operation.as_str(), v.target.as_str()),
            ("process-info-pidinfo", "")
        );
    }

    #[test]
    fn duplicate_report_lines_carry_their_count() {
        let m = "5 duplicate reports for Sandbox: node(3) deny(1) file-read-data /x\nta:e0d16d67";
        let v = parse_message(m, TAG).unwrap();
        assert_eq!(v.count, 5);
        assert_eq!(v.process, "node");
    }

    #[test]
    fn other_goals_and_other_sandboxes_are_ignored() {
        let other = "Sandbox: cat(1) deny(1) file-read-data /x\nta:ffffffff";
        assert!(parse_message(other, TAG).is_none());
        let untagged = "Sandbox: cat(1) deny(1) file-read-data /x";
        assert!(parse_message(untagged, TAG).is_none());
        // A target that merely mentions the tag does not count as the tag line.
        let spoof = "Sandbox: cat(1) deny(1) file-read-data /tmp/ta:e0d16d67\nta:ffffffff";
        assert!(parse_message(spoof, TAG).is_none());
        assert!(parse_message("Sandbox: junk\nta:e0d16d67", TAG).is_none());
        assert!(parse_message("", TAG).is_none());
    }

    #[test]
    fn ndjson_wrapper_is_unwrapped() {
        let line = serde_json::json!({
            "eventMessage": "Sandbox: cat(4242) deny(1) file-read-data /x\nta:e0d16d67",
            "subsystem": ""
        })
        .to_string();
        assert_eq!(parse_ndjson_line(&line, TAG).unwrap().pid, 4242);
        assert!(parse_ndjson_line("not json", TAG).is_none());
        assert!(parse_ndjson_line(r#"{"other":1}"#, TAG).is_none());
    }

    fn shared(tag: &str) -> (Arc<Shared>, Arc<Mutex<Vec<Violation>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&got);
        let shared = Arc::new(Shared {
            tag: tag.to_string(),
            on_violation: Arc::new(move |v| sink.lock().unwrap().push(v)),
            delivered: Mutex::new(HashSet::new()),
            canaries_live: Mutex::new(HashSet::new()),
            canaries_persisted: Mutex::new(HashSet::new()),
            lines_seen: AtomicU64::new(0),
            live_denials: AtomicU64::new(0),
            recovered_denials: AtomicU64::new(0),
        });
        (shared, got)
    }

    fn event(mach: u64, message: &str) -> String {
        serde_json::json!({"machTimestamp": mach, "eventMessage": message}).to_string()
    }

    #[test]
    fn a_denial_seen_live_and_again_in_the_persisted_log_is_delivered_once() {
        let (s, got) = shared(TAG);
        let line = event(
            100,
            "Sandbox: cat(7) deny(1) file-read-data /x\nta:e0d16d67",
        );
        s.ingest(&line, Source::Live);
        s.ingest(&line, Source::Persisted);
        assert_eq!(got.lock().unwrap().len(), 1);
        assert_eq!(s.live_denials.load(Ordering::Relaxed), 1);
        assert_eq!(s.recovered_denials.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_denial_only_the_persisted_log_has_is_recovered_and_counted() {
        let (s, got) = shared(TAG);
        s.ingest(
            &event(1, "Sandbox: cat(7) deny(1) file-read-data /a\nta:e0d16d67"),
            Source::Live,
        );
        // A second, different denial of the same file at another instant is a
        // separate denial, not a duplicate.
        s.ingest(
            &event(2, "Sandbox: cat(8) deny(1) file-read-data /a\nta:e0d16d67"),
            Source::Persisted,
        );
        assert_eq!(got.lock().unwrap().len(), 2);
        assert_eq!(s.live_denials.load(Ordering::Relaxed), 1);
        assert_eq!(s.recovered_denials.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn canary_denials_are_tracked_per_source_and_never_reported_as_violations() {
        let (s, got) = shared(TAG);
        let msg = format!(
            "Sandbox: cat(7) deny(1) file-read-data /tmp/c\n{}",
            canary_tag(TAG, "abc123")
        );
        s.ingest(&event(5, &msg), Source::Live);
        assert!(s.canary_seen("abc123", Source::Live));
        assert!(!s.canary_seen("abc123", Source::Persisted));
        s.ingest(&event(5, &msg), Source::Persisted);
        assert!(s.canary_seen("abc123", Source::Persisted));
        assert!(got.lock().unwrap().is_empty());
        assert_eq!(s.live_denials.load(Ordering::Relaxed), 0);
        // Another goal's canary is not ours.
        let other = "Sandbox: cat(7) deny(1) file-read-data /tmp/c\nta:ffffffff:c:zzz";
        s.ingest(&event(6, other), Source::Live);
        assert!(!s.canary_seen("zzz", Source::Live));
    }

    #[test]
    fn a_canary_tag_is_not_the_goal_tag() {
        let msg = format!(
            "Sandbox: cat(7) deny(1) file-read-data /x\n{}",
            canary_tag(TAG, "n1")
        );
        assert!(parse_message(&msg, TAG).is_none());
        assert_eq!(parse_canary(&msg, TAG).as_deref(), Some("n1"));
        // The nonce cannot ride in the denied target.
        let spoof = format!(
            "Sandbox: cat(7) deny(1) file-read-data {}",
            canary_tag(TAG, "n2")
        );
        assert!(parse_canary(&spoof, TAG).is_none());
    }

    #[test]
    fn the_report_says_what_is_unverified_and_what_to_do() {
        let healthy = CaptureReport {
            start: Canary::Delivered {
                attempts: 1,
                waited_ms: 300,
            },
            end: Canary::Delivered {
                attempts: 2,
                waited_ms: 900,
            },
            reconciled: true,
            live_denials: 3,
            recovered_denials: 1,
            stream_lines: 40,
            problems: vec![],
        };
        assert!(healthy.healthy());
        assert!(healthy.summary().contains("3 live, 1 recovered"));
        let bad = CaptureReport {
            problems: vec!["the live log stream was not verified before the agent started".into()],
            ..healthy
        };
        assert!(!bad.healthy());
        let text = bad.summary();
        assert!(
            text.contains("NOT verified") && text.contains("sandbox still enforced"),
            "{text}"
        );
    }

    #[test]
    fn explanation_is_actionable() {
        let v = parse_message(
            "Sandbox: cat(1) deny(1) file-read-data /x\nta:e0d16d67",
            TAG,
        )
        .unwrap();
        let e = v.explain();
        assert!(e.contains("file-read-data") && e.contains("policy.toml"));
    }
}
