// action_audit.rs — Flock-protected JSONL append log for ActionEnvelope (v0.17.0.2).
//
// Every `ActionEnvelope` routed through `ActionRouter` is appended here for a
// complete, queryable record of structured agent actions.
//
// Path: `<workspace_root>/.ta/action-log.jsonl`
// Concurrency: exclusive flock(2) before each write on Unix; no-op fallback elsewhere.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::agent_action::ActionEnvelope;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum AuditLogError {
    #[error("failed to open action audit log at {path}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write to action audit log at {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to serialize action envelope: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to read action audit log at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

// ── ActionAuditLog ─────────────────────────────────────────────────────────────

/// Append-only JSONL log for `ActionEnvelope` records.
///
/// Uses an exclusive advisory flock (on Unix) to serialize concurrent writers
/// so that each JSON line lands atomically. On non-Unix platforms the `O_APPEND`
/// flag provides the same guarantee for writes smaller than `PIPE_BUF`.
pub struct ActionAuditLog {
    log_path: PathBuf,
}

impl ActionAuditLog {
    /// Create an `ActionAuditLog` backed by `<workspace_root>/.ta/action-log.jsonl`.
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            log_path: workspace_root.join(".ta").join("action-log.jsonl"),
        }
    }

    /// Append an `ActionEnvelope` as a single JSON line.
    ///
    /// Acquires an exclusive flock before writing; the lock is released when
    /// the `File` handle is dropped at the end of this function.
    pub fn append(&self, envelope: &ActionEnvelope) -> Result<(), AuditLogError> {
        if let Some(parent) = self.log_path.parent() {
            fs::create_dir_all(parent).map_err(|e| AuditLogError::Open {
                path: self.log_path.clone(),
                source: e,
            })?;
        }

        let mut line = serde_json::to_string(envelope)?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| AuditLogError::Open {
                path: self.log_path.clone(),
                source: e,
            })?;

        acquire_exclusive_lock(&file).map_err(|e| AuditLogError::Write {
            path: self.log_path.clone(),
            source: e,
        })?;

        file.write_all(line.as_bytes())
            .map_err(|e| AuditLogError::Write {
                path: self.log_path.clone(),
                source: e,
            })?;
        file.flush().map_err(|e| AuditLogError::Write {
            path: self.log_path.clone(),
            source: e,
        })?;

        // flock released on `file` drop.
        tracing::debug!(
            action_id = %envelope.action_id,
            agent_id = %envelope.agent_id,
            action = %envelope.action,
            "action envelope appended to audit log"
        );

        Ok(())
    }

    /// Read all `ActionEnvelope` records from the log, oldest first.
    ///
    /// Returns an empty vec when the log does not yet exist.
    pub fn read_all(&self) -> Result<Vec<ActionEnvelope>, AuditLogError> {
        if !self.log_path.exists() {
            return Ok(vec![]);
        }

        let content = fs::read_to_string(&self.log_path).map_err(|e| AuditLogError::Read {
            path: self.log_path.clone(),
            source: e,
        })?;

        let mut results = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ActionEnvelope>(line) {
                Ok(env) => results.push(env),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping malformed action audit log entry");
                }
            }
        }

        Ok(results)
    }

    /// Return the path to the log file.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

// ── Platform flock ────────────────────────────────────────────────────────────

#[cfg(unix)]
fn acquire_exclusive_lock(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: fd is valid for the lifetime of `file`.
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn acquire_exclusive_lock(_file: &std::fs::File) -> std::io::Result<()> {
    // On Windows, O_APPEND writes < PIPE_BUF are atomic at the OS level.
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_action::{AgentAction, TeamRole};
    use tempfile::TempDir;

    fn make_log(dir: &TempDir) -> ActionAuditLog {
        ActionAuditLog::new(dir.path())
    }

    fn sample_envelope() -> ActionEnvelope {
        ActionEnvelope::new("agent-1", TeamRole::Reviewer, AgentAction::Continue)
    }

    #[test]
    fn append_and_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let log = make_log(&tmp);

        let e1 = sample_envelope();
        let e2 = sample_envelope();
        log.append(&e1).unwrap();
        log.append(&e2).unwrap();

        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action_id, e1.action_id);
        assert_eq!(entries[1].action_id, e2.action_id);
    }

    #[test]
    fn read_all_returns_empty_when_no_log() {
        let tmp = TempDir::new().unwrap();
        let log = make_log(&tmp);
        assert!(log.read_all().unwrap().is_empty());
    }

    #[test]
    fn append_creates_ta_directory() {
        let tmp = TempDir::new().unwrap();
        let log = make_log(&tmp);
        log.append(&sample_envelope()).unwrap();
        assert!(tmp.path().join(".ta").join("action-log.jsonl").exists());
    }

    #[test]
    fn log_path_accessor() {
        let tmp = TempDir::new().unwrap();
        let log = make_log(&tmp);
        assert!(log.log_path().ends_with("action-log.jsonl"));
    }

    #[test]
    fn flock_safe_concurrent_writes() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8u32)
            .map(|i| {
                let root = root.clone();
                let b = barrier.clone();
                thread::spawn(move || {
                    let log = ActionAuditLog::new(&root);
                    let env = ActionEnvelope::new(
                        format!("agent-{}", i),
                        TeamRole::Implementer,
                        AgentAction::Continue,
                    );
                    b.wait(); // release all threads simultaneously
                    log.append(&env).unwrap();
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let log = ActionAuditLog::new(tmp.path());
        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 8, "all 8 concurrent writes must land intact");

        // Every line must be valid JSON (no interleaved writes).
        for entry in &entries {
            // Re-serialize and verify it round-trips cleanly.
            let json = serde_json::to_string(entry).unwrap();
            let _: ActionEnvelope = serde_json::from_str(&json).unwrap();
        }
    }
}
