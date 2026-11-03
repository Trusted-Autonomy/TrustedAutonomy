//! Bounded, shell-free subprocess execution for VCS read primitives.
//!
//! Used by `SourceAdapter::changed_paths_since` / `diff_since`, which are
//! called from long-running pollers: a hung `git`/`svn` must never block the
//! caller, so every invocation has a hard wall-clock deadline, stdin closed,
//! and stdout/stderr drained on reader threads (a diff larger than the pipe
//! buffer would otherwise deadlock the child against the polling loop).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::adapter::{Result, SubmitError};

/// Run `program args` in `cwd` without a shell and return its stdout bytes.
///
/// * `remove_env` — environment variables removed from the child (e.g. the
///   git repo-selecting variables).
/// * `timeout` — hard deadline; on expiry the child is killed and an error
///   naming the command and the duration is returned.
///
/// Errors always say which command failed, why (spawn error, timeout, exit
/// status + trimmed stderr), and where it ran.
pub(crate) fn run_bounded(
    program: &str,
    args: &[&str],
    cwd: &Path,
    remove_env: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let display = format!("{} {}", program, args.join(" "));

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in remove_env {
        cmd.env_remove(var);
    }

    let mut child = cmd.spawn().map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::NotFound {
            format!(" — is `{}` installed and on PATH?", program)
        } else {
            String::new()
        };
        SubmitError::VcsError(format!(
            "could not start `{}` in {}: {}{}",
            display,
            cwd.display(),
            e,
            hint
        ))
    })?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stdout_pipe {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr_pipe {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SubmitError::VcsError(format!(
                    "could not wait for `{}` in {}: {}",
                    display,
                    cwd.display(),
                    e
                )));
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SubmitError::VcsError(format!(
                "`{}` timed out after {}s in {} and was killed; the repository may be very large \
                 or the VCS server unreachable — retry, or raise the timeout the caller passes",
                display,
                timeout.as_secs_f64(),
                cwd.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| SubmitError::VcsError(format!("`{}`: stdout reader panicked", display)))?;
    let stderr = stderr_reader.join().unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(SubmitError::VcsError(format!(
            "`{}` failed in {} ({}): {}",
            display,
            cwd.display(),
            status,
            stderr.trim()
        )));
    }
    Ok(stdout)
}

/// Reject a caller-supplied revision that could be parsed as a command-line
/// option (`--output=...`) or is empty. `what` names the VCS for the message.
pub(crate) fn check_rev_arg(since_rev: &str, what: &str) -> Result<()> {
    let rev = since_rev.trim();
    if rev.is_empty() || rev.starts_with('-') || rev.chars().any(|c| c.is_control()) {
        return Err(SubmitError::InvalidState(format!(
            "invalid {} revision {:?}: expected the value returned earlier by the adapter's \
             head_rev_id()",
            what, since_rev
        )));
    }
    Ok(())
}

/// Reject a project-relative path that is absolute or escapes the project
/// root, so `diff_since` can only look inside the project.
pub(crate) fn check_rel_path(rel_path: &str) -> Result<()> {
    let p = Path::new(rel_path);
    let escapes = p
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if rel_path.is_empty() || p.is_absolute() || escapes || rel_path.contains('\0') {
        return Err(SubmitError::InvalidState(format!(
            "invalid path {:?}: expected a non-empty path relative to the project root \
             without `..` components",
            rel_path
        )));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn returns_stdout_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_bounded(
            "sh",
            &["-c", "printf hello"],
            dir.path(),
            &[],
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn drains_output_larger_than_pipe_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_bounded(
            "sh",
            &["-c", "head -c 300000 /dev/zero"],
            dir.path(),
            &[],
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(out.len(), 300_000);
    }

    #[test]
    fn nonzero_exit_reports_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_bounded(
            "sh",
            &["-c", "echo boom >&2; exit 3"],
            dir.path(),
            &[],
            Duration::from_secs(10),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("boom"), "{err}");
        assert!(err.contains("sh -c"), "{err}");
    }

    #[test]
    fn timeout_kills_child_and_names_duration() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let err = run_bounded(
            "sh",
            &["-c", "sleep 30"],
            dir.path(),
            &[],
            Duration::from_millis(200),
        )
        .unwrap_err()
        .to_string();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(err.contains("timed out"), "{err}");
    }

    #[test]
    fn missing_program_says_how_to_fix() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_bounded(
            "ta-definitely-not-a-real-binary",
            &[],
            dir.path(),
            &[],
            Duration::from_secs(5),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("installed and on PATH"), "{err}");
    }

    #[test]
    fn removes_requested_env_vars() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("TA_BOUNDED_CMD_TEST_VAR", "x");
        let out = run_bounded(
            "sh",
            &["-c", "printf '%s' \"${TA_BOUNDED_CMD_TEST_VAR-unset}\""],
            dir.path(),
            &["TA_BOUNDED_CMD_TEST_VAR"],
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out, b"unset");
    }

    #[test]
    fn rev_arg_rejects_option_like_and_empty() {
        assert!(check_rev_arg("abc1234", "git").is_ok());
        assert!(check_rev_arg("", "git").is_err());
        assert!(check_rev_arg("--output=/tmp/x", "git").is_err());
    }

    #[test]
    fn rel_path_rejects_escapes() {
        assert!(check_rel_path("docs/a.md").is_ok());
        assert!(check_rel_path("../x").is_err());
        assert!(check_rel_path("/etc/passwd").is_err());
        assert!(check_rel_path("").is_err());
    }
}
