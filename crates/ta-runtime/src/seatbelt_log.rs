// seatbelt_log.rs: Seatbelt denials from the macOS unified log (v0.17.12.7).
//
// The v2 profile is `(deny default (with message "ta:<goal>"))`, so every
// kernel denial for a sandboxed goal carries that tag in the unified log:
//
//   Sandbox: cat(4242) deny(1) file-read-data /Users/u/.ssh/id_ed25519
//   ta:e0d16d67
//
// The kernel collapses repeats into "N duplicate reports for Sandbox: ...".
// `parse_message` is pure so it is testable on every platform; `ViolationTail`
// runs `/usr/bin/log stream` and is only useful on macOS.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

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

/// Parse one `--style ndjson` line from `log stream`.
pub fn parse_ndjson_line(line: &str, tag: &str) -> Option<Violation> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    parse_message(v.get("eventMessage")?.as_str()?, tag)
}

/// Tails the unified log for one goal's denials until dropped.
pub struct ViolationTail {
    child: Child,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ViolationTail {
    /// Start `/usr/bin/log stream` and call `on_violation` for each denial
    /// tagged `tag`. Fails with guidance if the `log` tool cannot be started.
    pub fn start(
        tag: String,
        on_violation: Arc<dyn Fn(Violation) + Send + Sync>,
    ) -> std::io::Result<Self> {
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
        let thread = std::thread::Builder::new()
            .name("ta-seatbelt-log".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(v) = parse_ndjson_line(&line, &tag) {
                        on_violation(v);
                    }
                }
            })?;
        Ok(Self {
            child,
            thread: Some(thread),
        })
    }
}

impl Drop for ViolationTail {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
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
