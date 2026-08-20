// source_lock.rs — Advisory lock serializing concurrent staging-create steps
// against the same source directory (v0.17.10.2).
//
// Problem: `ta_goal_start` can launch several `ta run --headless` subprocesses
// concurrently against the *same* `--source` directory (e.g. 5-6 goals started
// back-to-back against one project). Each subprocess independently claims the
// plan phase, commits + pushes PLAN.md, and creates its overlay workspace —
// all against the same real git working tree. With no coordination between
// these independent OS processes, two subprocesses can race on that working
// tree (see PLAN.md v0.17.10.2 for the concurrent-goal data-loss incident this
// guards against: deleted files, reverted content, clobbered `.ta-decisions.json`).
//
// `ApplyLock` (see `draft.rs`) already prevents concurrent `ta draft apply`
// runs, but it is keyed on the *applying* `config.workspace_root`, not on
// `source_dir` — the two are not guaranteed to match (that mismatch is exactly
// the item-1 bug this phase also fixes). `SourceStageLock` is keyed directly
// on the canonicalized source directory, independent of `config.workspace_root`,
// so it protects this case regardless of how each subprocess resolved its own
// config.
//
// Lock file: `<source_dir>/.ta/source-stage.lock`
// Format:    `{"pid": 12345, "goal_id": "...", "started_at": "<RFC3339>"}`

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Exclusive advisory lock held while a goal claims and stages against a
/// shared source directory. Two `ta run` processes launched concurrently
/// against the same `source` serialize on this lock instead of racing on the
/// same git working tree. Released (dropped) as soon as staging + the
/// pre-staging plan-phase claim/commit/push are done — the (potentially
/// long-running) agent work itself happens after the lock is released, so
/// independent goals can still work in parallel once each is safely staged.
pub struct SourceStageLock {
    lock_path: PathBuf,
}

impl SourceStageLock {
    fn lock_path(source_dir: &Path) -> PathBuf {
        source_dir.join(".ta").join("source-stage.lock")
    }

    /// Block until the lock is acquired or `timeout` elapses, retrying on a
    /// short fixed interval. Automatically reclaims a stale lock left behind
    /// by a process that is no longer alive.
    pub fn acquire_blocking(
        source_dir: &Path,
        goal_id: &str,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        let lock_path = Self::lock_path(source_dir);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create {} for source-stage lock: {}",
                    parent.display(),
                    e
                )
            })?;
        }

        let deadline = Instant::now() + timeout;
        loop {
            if Self::try_acquire_once(&lock_path, goal_id)? {
                return Ok(SourceStageLock { lock_path });
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "Timed out after {:?} waiting for source-stage lock at {} \
                     (another goal is currently staging against this source directory). \
                     Check for a dead process holding the lock; if the holder crashed, \
                     remove the lock file to recover: rm {}",
                    timeout,
                    lock_path.display(),
                    lock_path.display()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Returns `Ok(true)` if the lock was newly acquired, `Ok(false)` if it is
    /// currently held by a live process (retry later).
    fn try_acquire_once(lock_path: &Path, goal_id: &str) -> anyhow::Result<bool> {
        if let Ok(raw) = std::fs::read_to_string(lock_path) {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                let pid = data["pid"].as_u64().unwrap_or(0) as u32;
                if pid != 0 && is_lock_holder_alive(pid) {
                    return Ok(false);
                }
                eprintln!(
                    "[source-lock] Removing stale source-stage lock (PID {pid} is no longer running): {}",
                    lock_path.display()
                );
                let _ = std::fs::remove_file(lock_path);
            }
        }

        let content = serde_json::json!({
            "pid": std::process::id(),
            "goal_id": goal_id,
            "started_at": chrono::Utc::now().to_rfc3339(),
        })
        .to_string();

        // O_EXCL-style exclusive create: fails if another process wins the
        // race between our stale-check above and this write.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut f) => {
                f.write_all(content.as_bytes()).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to write source-stage lock at {}: {}",
                        lock_path.display(),
                        e
                    )
                })?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(anyhow::anyhow!(
                "Failed to create source-stage lock at {}: {}",
                lock_path.display(),
                e
            )),
        }
    }
}

impl Drop for SourceStageLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Returns `true` if the process with the given PID is currently alive.
/// Mirrors `draft::is_apply_process_alive` (kept separate to avoid making
/// that function `pub(crate)` purely for this one caller).
fn is_lock_holder_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Safety: kill(pid, 0) with signal 0 never sends a signal — it only
        // checks whether the process exists and we have permission to signal it.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        #[allow(non_upper_case_globals)]
        const SYNCHRONIZE: u32 = 0x00100000;
        #[allow(non_upper_case_globals)]
        const ERROR_ACCESS_DENIED: u32 = 5;
        extern "system" {
            fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;
            fn CloseHandle(hObject: isize) -> i32;
            fn GetLastError() -> u32;
        }
        unsafe {
            let handle = OpenProcess(SYNCHRONIZE, 0, pid);
            if handle == 0 || handle == -1isize {
                GetLastError() == ERROR_ACCESS_DENIED
            } else {
                CloseHandle(handle);
                true
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn acquire_then_release_allows_reacquire() {
        let dir = tempfile::TempDir::new().unwrap();
        {
            let _lock =
                SourceStageLock::acquire_blocking(dir.path(), "goal-a", Duration::from_secs(5))
                    .unwrap();
            assert!(SourceStageLock::lock_path(dir.path()).exists());
        }
        // Dropped — lock file must be gone.
        assert!(!SourceStageLock::lock_path(dir.path()).exists());

        let _lock2 =
            SourceStageLock::acquire_blocking(dir.path(), "goal-b", Duration::from_secs(5))
                .unwrap();
        assert!(SourceStageLock::lock_path(dir.path()).exists());
    }

    /// A lock held by a still-alive process (ourselves) must NOT be reclaimed
    /// as stale — the second acquire must time out, not silently proceed.
    #[test]
    fn live_lock_blocks_second_acquire() {
        let dir = tempfile::TempDir::new().unwrap();
        let _lock = SourceStageLock::acquire_blocking(dir.path(), "goal-a", Duration::from_secs(5))
            .unwrap();

        let result =
            SourceStageLock::acquire_blocking(dir.path(), "goal-b", Duration::from_millis(300));
        assert!(
            result.is_err(),
            "second acquire must time out while the first lock is live"
        );
    }

    /// A lock file left behind by a dead process (bogus/unused PID) must be
    /// reclaimed automatically rather than blocking forever.
    #[test]
    fn stale_lock_from_dead_process_is_reclaimed() {
        let dir = tempfile::TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        // A PID essentially guaranteed not to be alive in test environments.
        std::fs::write(
            ta_dir.join("source-stage.lock"),
            r#"{"pid": 999999, "goal_id": "goal-dead", "started_at": "2020-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        let lock = SourceStageLock::acquire_blocking(dir.path(), "goal-c", Duration::from_secs(5));
        assert!(lock.is_ok(), "stale lock from a dead PID must be reclaimed");
    }

    /// Two threads racing to acquire the same source lock must serialize:
    /// only one holds it at a time, and both eventually succeed.
    #[test]
    fn concurrent_acquires_serialize() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let concurrent_holders = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for i in 0..4 {
            let path = path.clone();
            let concurrent_holders = concurrent_holders.clone();
            let max_concurrent = max_concurrent.clone();
            handles.push(std::thread::spawn(move || {
                let _lock = SourceStageLock::acquire_blocking(
                    &path,
                    &format!("goal-{i}"),
                    Duration::from_secs(10),
                )
                .unwrap();
                let now = concurrent_holders.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                concurrent_holders.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "at most one thread must hold the source-stage lock at a time"
        );
    }
}
