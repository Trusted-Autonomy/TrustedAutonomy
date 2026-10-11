//! Process helpers shared by the bounded subprocess runners.
//!
//! `Child::kill` ends only the direct child. When that child is a wrapper
//! (the `gh` credential shim, a `cmd` shim, a `.cmd` script) its own children
//! are orphaned and keep running after the timeout. On Windows a CI run
//! accumulated thousands of such orphans, so timeout paths kill the whole
//! tree instead.

use std::process::Child;
#[cfg(windows)]
use std::process::{Command, Stdio};

/// Kill `child` and every process it started, then reap it.
///
/// Windows: `taskkill /PID <pid> /T /F` ends the whole tree. Other platforms:
/// the direct child is killed (the Unix `gh` wrapper `exec`s the real binary,
/// so there is no intermediate process to orphan).
///
/// Never panics and never blocks on a stuck child: failures are logged at
/// `warn` with the pid so a leftover process can be traced, and the plain
/// `Child::kill` fallback always runs.
pub fn kill_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        match Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!(
                pid,
                exit = ?status.code(),
                "taskkill /T /F did not end the process tree; falling back to killing the direct child. \
                 Check for leftover processes with: tasklist"
            ),
            Err(e) => tracing::warn!(
                pid,
                error = %e,
                "could not run taskkill; falling back to killing the direct child. \
                 Check for leftover processes with: tasklist"
            ),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    #[cfg(windows)]
    use std::process::Stdio;

    #[cfg(unix)]
    #[test]
    fn kill_process_tree_reaps_a_running_child() {
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        kill_process_tree(&mut child);
        // `kill_process_tree` already waited; a second wait must report the
        // exit status immediately rather than hanging.
        let status = child.try_wait().expect("try_wait").expect("child exited");
        assert!(!status.success());
    }

    #[cfg(windows)]
    #[test]
    fn kill_process_tree_reaps_a_running_child() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn ping");
        kill_process_tree(&mut child);
        let status = child.try_wait().expect("try_wait").expect("child exited");
        assert!(!status.success());
    }
}
