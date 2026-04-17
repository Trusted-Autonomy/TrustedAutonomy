//! ReviewQueueBackend — plugin trait for draft routing and multi-user review (v0.14.4).
//!
//! The default [`LocalReviewQueue`] stores pending drafts in `.ta/review_queue/`
//! and delivers them only to the local CLI (`ta draft list`). Enterprise
//! deployments can route drafts to external ticketing systems (Jira, Linear,
//! GitHub Issues) or multi-user approval workflows via a plugin.
//!
//! ## Plugin registration
//!
//! ```toml
//! [plugins]
//! review_queue = "ta-review-jira"
//! ```

use crate::ExtensionError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The outcome of a review decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Draft was approved for apply.
    Approved,
    /// Draft was denied.
    Denied {
        /// Reviewer's reason for denial.
        reason: String,
    },
}

/// A draft queued for review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewQueueEntry {
    /// Draft package ID.
    pub draft_id: String,
    /// Goal title (for display in external systems).
    pub title: String,
    /// Goal ID.
    pub goal_id: String,
    /// When this entry was enqueued.
    pub enqueued_at: DateTime<Utc>,
    /// Serialized draft summary (opaque bytes for external plugins).
    /// JSON-encoded for the local queue; passed through verbatim for remote.
    #[serde(default)]
    pub payload: Vec<u8>,
}

/// Plugin trait for draft routing and multi-user review queues.
///
/// The daemon calls [`enqueue`](ReviewQueueBackend::enqueue) when a draft
/// enters `PendingReview` state. External review systems call back (via the
/// HTTP API) to record a decision, which the daemon reflects via
/// [`complete`](ReviewQueueBackend::complete).
///
/// # Stability contract (v0.14.4)
///
/// This interface is **stable**.
#[async_trait]
pub trait ReviewQueueBackend: Send + Sync {
    /// Name for logging and diagnostics (e.g., `"local"`, `"jira"`, `"github"`).
    fn name(&self) -> &str;

    /// Enqueue a draft for review.
    ///
    /// The implementation is responsible for notifying reviewers (e.g.,
    /// creating a Jira ticket, posting to Slack, sending email). TA handles
    /// local delivery via `ta draft list`.
    async fn enqueue(&self, entry: ReviewQueueEntry) -> Result<(), ExtensionError>;

    /// Return all pending (unresolved) queue entries.
    async fn pending(&self) -> Result<Vec<ReviewQueueEntry>, ExtensionError>;

    /// Record the outcome of a review and remove the entry from the queue.
    ///
    /// `draft_id` matches [`ReviewQueueEntry::draft_id`].
    async fn complete(
        &self,
        draft_id: &str,
        decision: ReviewDecision,
    ) -> Result<(), ExtensionError>;
}

/// Default review queue — stores entries in `.ta/review_queue/` as JSONL.
pub struct LocalReviewQueue {
    queue_path: std::path::PathBuf,
}

impl LocalReviewQueue {
    /// Create a queue rooted at `<project_root>/.ta/review_queue/`.
    pub fn new(project_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            queue_path: project_root.into().join(".ta").join("review_queue"),
        }
    }

    fn entry_path(&self, draft_id: &str) -> std::path::PathBuf {
        self.queue_path.join(format!("{}.json", draft_id))
    }
}

#[async_trait]
impl ReviewQueueBackend for LocalReviewQueue {
    fn name(&self) -> &str {
        "local"
    }

    async fn enqueue(&self, entry: ReviewQueueEntry) -> Result<(), ExtensionError> {
        std::fs::create_dir_all(&self.queue_path)?;
        let path = self.entry_path(&entry.draft_id);
        let json = serde_json::to_vec_pretty(&entry)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    async fn pending(&self) -> Result<Vec<ReviewQueueEntry>, ExtensionError> {
        if !self.queue_path.exists() {
            return Ok(vec![]);
        }
        let mut entries = Vec::new();
        for file in std::fs::read_dir(&self.queue_path)? {
            let file = file?;
            if file.path().extension().and_then(|e| e.to_str()) == Some("json") {
                let data = std::fs::read(file.path())?;
                match serde_json::from_slice::<ReviewQueueEntry>(&data) {
                    Ok(entry) => entries.push(entry),
                    Err(e) => tracing::warn!(
                        path = %file.path().display(),
                        error = %e,
                        "Skipping malformed review queue entry"
                    ),
                }
            }
        }
        Ok(entries)
    }

    async fn complete(
        &self,
        draft_id: &str,
        _decision: ReviewDecision,
    ) -> Result<(), ExtensionError> {
        let path = self.entry_path(draft_id);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(draft_id: &str) -> ReviewQueueEntry {
        ReviewQueueEntry {
            draft_id: draft_id.to_string(),
            title: format!("Goal for {}", draft_id),
            goal_id: "goal-xyz".to_string(),
            enqueued_at: Utc::now(),
            payload: b"{}".to_vec(),
        }
    }

    #[tokio::test]
    async fn local_queue_name() {
        let dir = tempfile::tempdir().unwrap();
        let q = LocalReviewQueue::new(dir.path());
        assert_eq!(q.name(), "local");
    }

    #[tokio::test]
    async fn enqueue_and_pending() {
        let dir = tempfile::tempdir().unwrap();
        let q = LocalReviewQueue::new(dir.path());

        assert!(q.pending().await.unwrap().is_empty());

        q.enqueue(make_entry("d1")).await.unwrap();
        q.enqueue(make_entry("d2")).await.unwrap();

        let mut pending = q.pending().await.unwrap();
        pending.sort_by_key(|e| e.draft_id.clone());
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].draft_id, "d1");
    }

    #[tokio::test]
    async fn complete_removes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let q = LocalReviewQueue::new(dir.path());
        q.enqueue(make_entry("d1")).await.unwrap();
        q.complete("d1", ReviewDecision::Approved).await.unwrap();
        assert!(q.pending().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn complete_nonexistent_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let q = LocalReviewQueue::new(dir.path());
        assert!(q
            .complete(
                "ghost",
                ReviewDecision::Denied {
                    reason: "no reason".to_string()
                }
            )
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn pending_empty_root() {
        let dir = tempfile::tempdir().unwrap();
        let q = LocalReviewQueue::new(dir.path().join("nonexistent"));
        assert!(q.pending().await.unwrap().is_empty());
    }
}
