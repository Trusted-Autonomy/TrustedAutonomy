//! AuditStorageBackend — plugin trait for audit log storage (v0.14.4).
//!
//! The default [`LocalAuditStorage`] writes records to `.ta/audit.jsonl`
//! (append-only JSONL). Enterprise deployments may want to ship audit records
//! to a SIEM, cloud storage, or database. Implement this trait and register
//! it via `[plugins].audit_storage`.
//!
//! Note: The richer `AuditEntry` data model (goal context, artifact manifest,
//! denial reasons, hash chaining) is defined in v0.14.6. This trait works
//! with opaque [`RawAuditEntry`] records to keep v0.14.4 focused on the
//! extension surface, not the ledger format.
//!
//! ## Plugin registration
//!
//! ```toml
//! [plugins]
//! audit_storage = "ta-audit-splunk"
//! ```

use crate::ExtensionError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// A raw audit record passed to the storage backend.
///
/// The `body` field is opaque JSON — the schema evolves with TA's audit
/// model (see v0.14.6 for the full `AuditEntry` type). Plugins should store
/// and forward the body verbatim without parsing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawAuditEntry {
    /// Monotonic sequence number within the current log file.
    pub seq: u64,
    /// When this entry was produced.
    pub timestamp: DateTime<Utc>,
    /// Entry type tag (e.g., `"draft.apply"`, `"goal.start"`, `"goal.deny"`).
    pub event_type: String,
    /// Goal ID, if this entry is associated with a goal.
    pub goal_id: Option<String>,
    /// JSON-serialized entry body.
    pub body: serde_json::Value,
}

impl RawAuditEntry {
    /// Create a new entry with the current timestamp.
    pub fn new(seq: u64, event_type: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            seq,
            timestamp: Utc::now(),
            event_type: event_type.into(),
            goal_id: None,
            body,
        }
    }

    /// Attach a goal ID to this entry.
    pub fn with_goal_id(mut self, goal_id: impl Into<String>) -> Self {
        self.goal_id = Some(goal_id.into());
        self
    }
}

/// Plugin trait for audit log storage.
///
/// The daemon calls [`append`](AuditStorageBackend::append) for each audit
/// event. The backend is responsible for durability — the daemon does not
/// retry on failure beyond logging a warning.
///
/// [`flush`](AuditStorageBackend::flush) is called on clean shutdown.
///
/// # Stability contract (v0.14.4)
///
/// This interface is **stable**.
#[async_trait]
pub trait AuditStorageBackend: Send + Sync {
    /// Name for logging and diagnostics (e.g., `"local"`, `"splunk"`, `"s3"`).
    fn name(&self) -> &str;

    /// Append a record to the audit log.
    ///
    /// Implementations must be durable: once this returns `Ok(())`, the record
    /// must survive a process crash.
    async fn append(&self, entry: &RawAuditEntry) -> Result<(), ExtensionError>;

    /// Flush any buffered writes to storage.
    ///
    /// Called on clean daemon shutdown. Implementations with write buffers
    /// must flush here; no-op for unbuffered implementations.
    async fn flush(&self) -> Result<(), ExtensionError>;
}

/// Default audit backend — writes JSONL records to `.ta/audit.jsonl`.
///
/// Each call to [`append`] opens the file, writes one JSON line, and closes
/// it. This is deliberately simple: correctness over throughput, since audit
/// volume is low (one entry per significant event, not per tool call).
pub struct LocalAuditStorage {
    audit_path: PathBuf,
}

impl LocalAuditStorage {
    /// Create a backend that writes to `<project_root>/.ta/audit.jsonl`.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            audit_path: project_root.into().join(".ta").join("audit.jsonl"),
        }
    }
}

#[async_trait]
impl AuditStorageBackend for LocalAuditStorage {
    fn name(&self) -> &str {
        "local"
    }

    async fn append(&self, entry: &RawAuditEntry) -> Result<(), ExtensionError> {
        if let Some(parent) = self.audit_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)?;
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        file.write_all(&line)?;
        Ok(())
    }

    async fn flush(&self) -> Result<(), ExtensionError> {
        // No buffer — each append syncs immediately.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_backend_name() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalAuditStorage::new(dir.path());
        assert_eq!(b.name(), "local");
    }

    #[tokio::test]
    async fn append_creates_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalAuditStorage::new(dir.path());

        let e1 = RawAuditEntry::new(1, "goal.start", serde_json::json!({"goal_id": "abc"}));
        let e2 = RawAuditEntry::new(2, "draft.apply", serde_json::json!({"draft_id": "def"}));

        b.append(&e1).await.unwrap();
        b.append(&e2).await.unwrap();

        let content = std::fs::read_to_string(dir.path().join(".ta").join("audit.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["event_type"], "goal.start");
        assert_eq!(parsed["seq"], 1);
    }

    #[tokio::test]
    async fn flush_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let b = LocalAuditStorage::new(dir.path());
        assert!(b.flush().await.is_ok());
    }

    #[test]
    fn raw_entry_with_goal_id() {
        let e = RawAuditEntry::new(1, "test", serde_json::json!({})).with_goal_id("goal-123");
        assert_eq!(e.goal_id.as_deref(), Some("goal-123"));
    }
}
