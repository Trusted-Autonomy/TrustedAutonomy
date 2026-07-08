//! Copy-truncate log rotation for `.ta/daemon.log` (v0.17.0.12.18).
//!
//! The daemon process never holds its own writable handle to `daemon.log` —
//! `ta daemon start` spawns the binary with stdout/stderr redirected to the
//! file at the OS level (see `apps/ta-cli/src/commands/daemon.rs`), and the
//! same append-redirect is used by the launchd/systemd unit files. A plain
//! `rename()`-based rotation (the original v0.17.0.12.8 behavior) silently
//! breaks while the daemon is running: renaming the file doesn't move the
//! process's already-open fd, so every subsequent write keeps landing in the
//! renamed backup and the "fresh" `daemon.log` never grows. That's why
//! rotation only ever appeared to work right after a full daemon restart.
//!
//! Copy-truncate avoids this: the backup is produced by *copying* the
//! current bytes out, then the original file is truncated to zero length
//! in place (same inode, same fd) rather than replaced. Any process still
//! holding that fd keeps appending correctly to the now-empty file.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Result of a rotation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationOutcome {
    /// Log was under the threshold (or missing); nothing was done.
    NotNeeded,
    /// Log was rotated. Carries the pre-rotation size and how many old
    /// generations beyond the retention count were pruned.
    Rotated { size_bytes: u64, pruned: usize },
}

/// Rotate `<workspace_root>/.ta/daemon.log` if it has grown past
/// `max_size_mb`, keeping up to `retention_count` prior generations
/// (`daemon.log.1` most recent, `daemon.log.2` older, ...).
///
/// Safe to call on a log file that a live process (this daemon or another)
/// is actively appending to via an OS-redirected stdout/stderr handle: the
/// active file is truncated in place rather than renamed or replaced, so
/// the writer's file descriptor stays valid across rotation.
///
/// `max_size_mb == 0` disables rotation (always returns `NotNeeded`).
pub fn rotate_if_needed(
    workspace_root: &Path,
    max_size_mb: u64,
    retention_count: u32,
) -> anyhow::Result<RotationOutcome> {
    if max_size_mb == 0 {
        return Ok(RotationOutcome::NotNeeded);
    }

    let log_path = workspace_root.join(".ta/daemon.log");
    let size_bytes = match std::fs::metadata(&log_path) {
        Ok(meta) => meta.len(),
        Err(_) => return Ok(RotationOutcome::NotNeeded), // no log yet — nothing to rotate
    };

    let threshold_bytes = max_size_mb * 1_048_576;
    if size_bytes < threshold_bytes {
        return Ok(RotationOutcome::NotNeeded);
    }

    do_rotate(workspace_root, retention_count)
}

/// Unconditionally rotate `daemon.log`, regardless of its current size.
///
/// Used by `ta doctor --fix`, where the caller has already decided rotation
/// should happen (e.g. an operator explicitly requested it, or a health
/// signal already confirmed the log is oversized). Errors if `daemon.log`
/// doesn't exist — there's nothing to rotate.
pub fn force_rotate(
    workspace_root: &Path,
    retention_count: u32,
) -> anyhow::Result<RotationOutcome> {
    let log_path = workspace_root.join(".ta/daemon.log");
    std::fs::metadata(&log_path)
        .map_err(|e| anyhow::anyhow!("Could not read {}: {}", log_path.display(), e))?;

    do_rotate(workspace_root, retention_count)
}

fn do_rotate(workspace_root: &Path, retention_count: u32) -> anyhow::Result<RotationOutcome> {
    let log_path = workspace_root.join(".ta/daemon.log");
    let size_bytes = std::fs::metadata(&log_path)
        .map_err(|e| anyhow::anyhow!("Could not read {}: {}", log_path.display(), e))?
        .len();

    let pruned = shift_generations(workspace_root, retention_count)?;
    copy_truncate(&log_path, size_bytes, retention_count > 0)?;

    Ok(RotationOutcome::Rotated { size_bytes, pruned })
}

/// Shift `daemon.log.N` → `daemon.log.(N+1)` for existing backups, from
/// oldest to newest so no generation is overwritten before it's moved.
/// Anything that would land beyond `retention_count` is deleted instead.
/// Returns the number of generations pruned (deleted rather than shifted).
fn shift_generations(workspace_root: &Path, retention_count: u32) -> anyhow::Result<usize> {
    let ta_dir = workspace_root.join(".ta");
    let mut pruned = 0usize;

    if retention_count == 0 {
        // No history retained — drop any existing backup outright.
        let backup = backup_path(&ta_dir, 1);
        if backup.exists() {
            std::fs::remove_file(&backup)?;
            pruned += 1;
        }
        return Ok(pruned);
    }

    // Walk from the oldest possible generation down to 1 so each rename
    // target is vacated before we write to it.
    for gen in (1..=retention_count).rev() {
        let src = backup_path(&ta_dir, gen);
        if !src.exists() {
            continue;
        }
        if gen == retention_count {
            // Oldest retained generation — anything here ages out.
            std::fs::remove_file(&src)?;
            pruned += 1;
            continue;
        }
        let dst = backup_path(&ta_dir, gen + 1);
        std::fs::rename(&src, &dst)?;
    }

    Ok(pruned)
}

fn backup_path(ta_dir: &Path, generation: u32) -> PathBuf {
    ta_dir.join(format!("daemon.log.{generation}"))
}

/// Copy the first `size_bytes` of `log_path` into `daemon.log.1` (unless
/// `keep_backup` is false, i.e. `retention_count == 0`), then truncate
/// `log_path` to zero length in place (no rename/unlink), so an open
/// writer's fd keeps pointing at valid, now-empty file content.
fn copy_truncate(log_path: &Path, size_bytes: u64, keep_backup: bool) -> anyhow::Result<()> {
    let ta_dir = log_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("daemon.log has no parent directory"))?;

    if keep_backup {
        let backup_path = backup_path(ta_dir, 1);
        std::fs::copy(log_path, &backup_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to copy {} to {} during log rotation: {}",
                log_path.display(),
                backup_path.display(),
                e
            )
        })?;
    }

    // Truncate in place — same inode/fd, so a live writer keeps working.
    let file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(log_path)
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to truncate {} during log rotation: {}",
                log_path.display(),
                e
            )
        })?;
    drop(file);

    let marker = format!(
        "[log-rotation] rotated daemon.log ({} bytes) to daemon.log.1 at {}\n",
        size_bytes,
        chrono::Utc::now().to_rfc3339(),
    );
    let mut appender = OpenOptions::new().append(true).open(log_path)?;
    appender.write_all(marker.as_bytes())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn write_log(dir: &Path, contents: &str) {
        std::fs::create_dir_all(dir.join(".ta")).unwrap();
        std::fs::write(dir.join(".ta/daemon.log"), contents).unwrap();
    }

    #[test]
    fn not_needed_when_under_threshold() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), "small log");
        let outcome = rotate_if_needed(dir.path(), 500, 3).unwrap();
        assert_eq!(outcome, RotationOutcome::NotNeeded);
    }

    #[test]
    fn not_needed_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), &"x".repeat(2_000_000));
        let outcome = rotate_if_needed(dir.path(), 0, 3).unwrap();
        assert_eq!(outcome, RotationOutcome::NotNeeded);
    }

    #[test]
    fn not_needed_when_log_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let outcome = rotate_if_needed(dir.path(), 1, 3).unwrap();
        assert_eq!(outcome, RotationOutcome::NotNeeded);
    }

    #[test]
    fn rotates_at_threshold_and_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(2 * 1_048_576); // 2 MB
        write_log(dir.path(), &big);

        let outcome = rotate_if_needed(dir.path(), 1, 3).unwrap();
        match outcome {
            RotationOutcome::Rotated { size_bytes, pruned } => {
                assert_eq!(size_bytes, 2 * 1_048_576);
                assert_eq!(pruned, 0);
            }
            other => panic!("expected Rotated, got {other:?}"),
        }

        let backup = std::fs::read_to_string(dir.path().join(".ta/daemon.log.1")).unwrap();
        assert_eq!(backup, big);

        let fresh = std::fs::read_to_string(dir.path().join(".ta/daemon.log")).unwrap();
        assert!(fresh.contains("rotated daemon.log"));
        assert!(fresh.len() < 1000);
    }

    #[test]
    fn retention_count_prunes_oldest_generation() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("daemon.log.1"), "gen1").unwrap();
        std::fs::write(ta_dir.join("daemon.log.2"), "gen2").unwrap();
        write_log(dir.path(), &"x".repeat(2 * 1_048_576));

        // retention_count = 2: gen2 ages out, gen1 -> gen2, fresh log -> gen1.
        let outcome = rotate_if_needed(dir.path(), 1, 2).unwrap();
        match outcome {
            RotationOutcome::Rotated { pruned, .. } => assert_eq!(pruned, 1),
            other => panic!("expected Rotated, got {other:?}"),
        }

        assert_eq!(
            std::fs::read_to_string(ta_dir.join("daemon.log.2")).unwrap(),
            "gen1"
        );
        assert!(std::fs::read_to_string(ta_dir.join("daemon.log.1"))
            .unwrap()
            .starts_with('x'));
    }

    #[test]
    fn retention_zero_drops_backup_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("daemon.log.1"), "old backup").unwrap();
        write_log(dir.path(), &"x".repeat(2 * 1_048_576));

        let outcome = rotate_if_needed(dir.path(), 1, 0).unwrap();
        match outcome {
            RotationOutcome::Rotated { pruned, .. } => assert_eq!(pruned, 1),
            other => panic!("expected Rotated, got {other:?}"),
        }
        assert!(!ta_dir.join("daemon.log.1").exists());
    }

    /// The core reliability property this phase exists to fix: a process
    /// holding an already-open append handle to daemon.log (mirroring how
    /// the OS redirects the daemon's stdout/stderr at spawn time) must keep
    /// writing successfully to the *same* logical log after rotation, with
    /// its new output landing in the post-rotation file rather than being
    /// silently lost in the renamed-away backup.
    #[test]
    fn live_writer_continues_uninterrupted_across_rotation() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), &"x".repeat(2 * 1_048_576));

        let log_path = dir.path().join(".ta/daemon.log");
        let mut live_handle = OpenOptions::new().append(true).open(&log_path).unwrap();

        rotate_if_needed(dir.path(), 1, 3).unwrap();

        live_handle.write_all(b"post-rotation line\n").unwrap();
        live_handle.flush().unwrap();

        let mut fresh = String::new();
        std::fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut fresh)
            .unwrap();
        assert!(
            fresh.contains("post-rotation line"),
            "live writer's output must appear in the post-rotation log, got: {fresh:?}"
        );

        let backup = std::fs::read_to_string(dir.path().join(".ta/daemon.log.1")).unwrap();
        assert!(
            !backup.contains("post-rotation line"),
            "post-rotation writes must not land in the backup"
        );
    }
}
