//! Shared spawn + newline-delimited-JSON framing + timeout transport, used by
//! every Plugin-category integration (VCS, messaging, social, agent-runtime,
//! tool, db, release). Format-agnostic: `call_json` is generic over the
//! caller's own request/response types, so it does not change any existing
//! plugin's wire bytes.

use crate::error::PluginError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const ETXTBSY_BACKOFF_MS: [u64; 4] = [0, 20, 80, 200];

fn spawn_with_retry(
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
) -> Result<Child, PluginError> {
    let mut parts = command.split_whitespace();
    let program = parts.next().ok_or_else(|| PluginError::SpawnFailed {
        command: command.to_string(),
        reason: "empty command".to_string(),
    })?;
    let baked_args: Vec<&str> = parts.collect();

    let mut last_err = None;
    for delay_ms in ETXTBSY_BACKOFF_MS {
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        let mut cmd = Command::new(program);
        cmd.args(&baked_args)
            .args(extra_args)
            .current_dir(work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(26) => {
                last_err = Some(e);
                continue;
            }
            Err(e) => {
                return Err(PluginError::SpawnFailed {
                    command: command.to_string(),
                    reason: e.to_string(),
                })
            }
        }
    }
    Err(PluginError::SpawnFailed {
        command: command.to_string(),
        reason: last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "ETXTBSY retry exhausted".to_string()),
    })
}

/// Wait for `child` to exit, killing it after `timeout` via a watchdog thread.
fn wait_with_timeout(child: Child, timeout: Duration) -> Result<std::process::Output, PluginError> {
    let pid = child.id();
    let (tx, rx) = mpsc::channel::<()>();
    let killed = Arc::new(AtomicBool::new(false));
    let killed_flag = killed.clone();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            killed_flag.store(true, Ordering::SeqCst);
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = pid;
        }
    });
    let output = child.wait_with_output();
    let _ = tx.send(());
    let _ = watchdog.join();
    let output = output?;
    if killed.load(Ordering::SeqCst) {
        return Err(PluginError::Timeout {
            name: String::new(),
            method: String::new(),
            timeout_secs: timeout.as_secs(),
        });
    }
    Ok(output)
}

/// Format-agnostic call: write `request_line` + "\n" to the plugin's stdin,
/// return the first line of stdout. Non-zero exit or empty stdout is an error.
#[allow(clippy::too_many_arguments)]
pub fn call_raw(
    name: &str,
    method: &str,
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request_line: &str,
    timeout: Duration,
) -> Result<String, PluginError> {
    let mut child = spawn_with_retry(command, extra_args, work_dir)?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| PluginError::CallFailed {
                name: name.to_string(),
                method: method.to_string(),
                reason: "plugin process has no stdin".to_string(),
            })?;
        stdin.write_all(request_line.as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    let output = wait_with_timeout(child, timeout).map_err(|e| match e {
        PluginError::Timeout { .. } => PluginError::Timeout {
            name: name.to_string(),
            method: method.to_string(),
            timeout_secs: timeout.as_secs(),
        },
        other => other,
    })?;

    if !output.status.success() {
        let mut stderr = String::new();
        let _ = std::io::Cursor::new(&output.stderr).read_to_string(&mut stderr);
        return Err(PluginError::CallFailed {
            name: name.to_string(),
            method: method.to_string(),
            reason: stderr.trim().to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("");
    if first_line.is_empty() {
        return Err(PluginError::InvalidResponse {
            name: name.to_string(),
            method: method.to_string(),
            reason: "plugin wrote no output line to stdout".to_string(),
        });
    }
    Ok(first_line.to_string())
}

/// JSON convenience wrapper over `call_raw`, generic over the caller's own
/// request/response wire types (VCS/messaging/social keep their existing
/// types; new integrations use `envelope::{PluginRequest,PluginResponse}`).
#[allow(clippy::too_many_arguments)]
pub fn call_json<Req: Serialize, Resp: DeserializeOwned>(
    name: &str,
    method: &str,
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request: &Req,
    timeout: Duration,
) -> Result<Resp, PluginError> {
    let line = serde_json::to_string(request)?;
    let response_line = call_raw(name, method, command, extra_args, work_dir, &line, timeout)?;
    serde_json::from_str(&response_line).map_err(|e| PluginError::InvalidResponse {
        name: name.to_string(),
        method: method.to_string(),
        reason: format!("{e} (raw: {})", truncate(&response_line, 200)),
    })
}

/// Write one JSON request line to an already-open stdin handle (long-lived
/// process framing, e.g. agent-runtime plugins that stay alive across calls).
pub fn write_line<Req: Serialize>(
    stdin: &mut impl Write,
    request: &Req,
) -> Result<(), PluginError> {
    let line = serde_json::to_string(request)?;
    stdin.write_all(line.as_bytes())?;
    stdin.write_all(b"\n")?;
    Ok(())
}

/// Read one JSON response line from an already-open stdout reader.
pub fn read_line<Resp: DeserializeOwned>(reader: &mut impl BufRead) -> Result<Resp, PluginError> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(PluginError::InvalidResponse {
            name: String::new(),
            method: String::new(),
            reason: "plugin wrote no output line to stdout".to_string(),
        });
    }
    serde_json::from_str(&line).map_err(PluginError::from)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

// All tests in this module spawn a `#!/bin/sh` mock plugin script — unix-only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn write_mock_script(dir: &Path, body: &str) -> String {
        let path = dir.join("mock-plugin.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn call_raw_round_trips_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(
            dir.path(),
            "read -r line\necho '{\"ok\":true,\"result\":{}}'",
        );
        let result = call_raw(
            "mock",
            "ping",
            &script,
            &[],
            dir.path(),
            "{\"method\":\"ping\"}",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(result, "{\"ok\":true,\"result\":{}}");
    }

    #[test]
    fn call_json_deserializes_response() {
        use crate::envelope::{PluginRequest, PluginResponse};
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(
            dir.path(),
            "read -r line\necho '{\"ok\":true,\"result\":{\"pong\":true}}'",
        );
        let req = PluginRequest::new("ping", serde_json::json!({}));
        let resp: PluginResponse = call_json(
            "mock",
            "ping",
            &script,
            &[],
            dir.path(),
            &req,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result["pong"], serde_json::json!(true));
    }

    #[test]
    fn call_raw_reports_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(dir.path(), "echo 'boom' >&2\nexit 1");
        let err = call_raw(
            "mock",
            "ping",
            &script,
            &[],
            dir.path(),
            "{}",
            Duration::from_secs(5),
        )
        .unwrap_err();
        match err {
            PluginError::CallFailed { reason, .. } => assert!(reason.contains("boom")),
            other => panic!("expected CallFailed, got {other:?}"),
        }
    }

    #[test]
    fn call_raw_kills_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(dir.path(), "sleep 5\necho '{\"ok\":true,\"result\":{}}'");
        let err = call_raw(
            "mock",
            "ping",
            &script,
            &[],
            dir.path(),
            "{}",
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(matches!(err, PluginError::Timeout { .. }));
    }
}
