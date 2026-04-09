//! Audit record for TA-created social media drafts and scheduled posts.
//!
//! Every draft or scheduled post created via a `SocialAdapter` is recorded as a
//! `DraftSocialRecord` and appended to `.ta/social-audit.jsonl`.
//! The log is append-only; no records are deleted or modified.
//!
//! ## Usage
//!
//! ```ignore
//! let record = DraftSocialRecord {
//!     post_id: "linkedin-draft-abc123".to_string(),
//!     platform: "linkedin".to_string(),
//!     handle: "@username".to_string(),
//!     body_preview: "Excited to share the cinepipe launch!".to_string(),
//!     created_at: chrono::Utc::now().to_rfc3339(),
//!     state: SocialPostRecordState::Draft,
//!     goal_id: Some(goal.goal_run_id.to_string()),
//!     supervisor_score: Some(0.95),
//!     manually_approved: false,
//! };
//! SocialAuditLog::append_to(&project_root, &record)?;
//! ```

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------------

/// Current lifecycle state of a TA-created social media draft or scheduled post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialPostRecordState {
    /// Draft exists in the platform's native draft state.
    Draft,
    /// Post has been published (by the user or platform scheduler).
    Published,
    /// Draft or scheduled post was deleted.
    Deleted,
    /// State is unknown (platform did not report definitively).
    Unknown,
}

impl std::fmt::Display for SocialPostRecordState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SocialPostRecordState::Draft => write!(f, "draft"),
            SocialPostRecordState::Published => write!(f, "published"),
            SocialPostRecordState::Deleted => write!(f, "deleted"),
            SocialPostRecordState::Unknown => write!(f, "unknown"),
        }
    }
}

/// Audit record for a single TA-created social media draft or scheduled post.
///
/// Persisted as a JSON line in `.ta/social-audit.jsonl`.
/// One record per post; `state` is updated when `draft_status` is polled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftSocialRecord {
    /// Platform-specific post identifier (e.g., "linkedin-draft-abc123").
    pub post_id: String,

    /// Social media platform (e.g., "linkedin", "x", "buffer").
    pub platform: String,

    /// Connected handle / username on the platform (e.g., "@username").
    pub handle: String,

    /// First 100 characters of the post body (for audit review without full body).
    pub body_preview: String,

    /// ISO-8601 timestamp when the draft was created by TA.
    pub created_at: String,

    /// Current lifecycle state.
    pub state: SocialPostRecordState,

    /// Goal run that produced this post (if created within a goal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,

    /// Supervisor confidence score [0.0, 1.0] assigned by the review step.
    /// `None` if no supervisory review was performed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervisor_score: Option<f64>,

    /// Whether this post was manually approved after being flagged for review.
    pub manually_approved: bool,
}

impl DraftSocialRecord {
    /// Create a preview string from a full post body (first 100 Unicode scalar values).
    pub fn make_body_preview(body: &str) -> String {
        let chars: Vec<char> = body.chars().collect();
        if chars.len() <= 100 {
            body.to_string()
        } else {
            let preview: String = chars[..99].iter().collect();
            format!("{}…", preview)
        }
    }

    /// Update the state field in-place (does not persist).
    pub fn set_state(&mut self, state: SocialPostRecordState) {
        self.state = state;
    }
}

// ---------------------------------------------------------------------------
// Log
// ---------------------------------------------------------------------------

/// Append-only log of `DraftSocialRecord` entries stored at
/// `.ta/social-audit.jsonl`.
pub struct SocialAuditLog {
    path: PathBuf,
}

impl SocialAuditLog {
    /// Open the log at its default location under `project_root`.
    pub fn open(project_root: &Path) -> Self {
        Self {
            path: project_root.join(".ta").join("social-audit.jsonl"),
        }
    }

    /// Open the log at an explicit path.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path to the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a record to the log.
    ///
    /// Creates the file (and parent directories) if they do not exist.
    pub fn append(&self, record: &DraftSocialRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", json)?;

        Ok(())
    }

    /// Read all records from the log. Returns an empty vec if the file does
    /// not exist. Malformed lines are skipped with a warning.
    pub fn read_all(&self) -> std::io::Result<Vec<DraftSocialRecord>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }

        let file = std::fs::File::open(&self.path)?;
        let reader = std::io::BufReader::new(file);
        let mut records = Vec::new();

        for (line_no, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<DraftSocialRecord>(trimmed) {
                Ok(r) => records.push(r),
                Err(e) => {
                    tracing::warn!(
                        line = line_no + 1,
                        path = %self.path.display(),
                        error = %e,
                        "Skipping malformed social audit record"
                    );
                }
            }
        }

        Ok(records)
    }

    /// Append a record directly from project root (convenience wrapper).
    pub fn append_to(project_root: &Path, record: &DraftSocialRecord) -> std::io::Result<()> {
        Self::open(project_root).append(record)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(post_id: &str) -> DraftSocialRecord {
        DraftSocialRecord {
            post_id: post_id.to_string(),
            platform: "linkedin".to_string(),
            handle: "@testuser".to_string(),
            body_preview: "Excited to share the cinepipe launch!".to_string(),
            created_at: "2026-04-09T10:00:00Z".to_string(),
            state: SocialPostRecordState::Draft,
            goal_id: Some("goal-abc123".to_string()),
            supervisor_score: Some(0.95),
            manually_approved: false,
        }
    }

    #[test]
    fn append_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let log = SocialAuditLog::open(dir.path());

        let r1 = make_record("post-001");
        let r2 = make_record("post-002");

        log.append(&r1).unwrap();
        log.append(&r2).unwrap();

        let records = log.read_all().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].post_id, "post-001");
        assert_eq!(records[1].post_id, "post-002");
    }

    #[test]
    fn read_all_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = SocialAuditLog::open(dir.path());
        let records = log.read_all().unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn append_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let log = SocialAuditLog::at(dir.path().join("sub").join("nested").join("social.jsonl"));
        let record = make_record("post-abc");
        log.append(&record).unwrap();
        assert!(log.path().exists());
    }

    #[test]
    fn record_state_display() {
        assert_eq!(SocialPostRecordState::Draft.to_string(), "draft");
        assert_eq!(SocialPostRecordState::Published.to_string(), "published");
        assert_eq!(SocialPostRecordState::Deleted.to_string(), "deleted");
        assert_eq!(SocialPostRecordState::Unknown.to_string(), "unknown");
    }

    #[test]
    fn set_state_updates_in_place() {
        let mut record = make_record("post-xyz");
        assert_eq!(record.state, SocialPostRecordState::Draft);
        record.set_state(SocialPostRecordState::Published);
        assert_eq!(record.state, SocialPostRecordState::Published);
    }

    #[test]
    fn record_serialization_skips_none_fields() {
        let record = DraftSocialRecord {
            post_id: "post-minimal".to_string(),
            platform: "x".to_string(),
            handle: "@xuser".to_string(),
            body_preview: "Short post.".to_string(),
            created_at: "2026-04-09T10:00:00Z".to_string(),
            state: SocialPostRecordState::Draft,
            goal_id: None,
            supervisor_score: None,
            manually_approved: false,
        };

        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("goal_id"));
        assert!(!json.contains("supervisor_score"));
    }

    #[test]
    fn record_roundtrip_with_all_fields() {
        let record = make_record("post-full");
        let json = serde_json::to_string(&record).unwrap();
        let parsed: DraftSocialRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.post_id, "post-full");
        assert_eq!(parsed.platform, "linkedin");
        assert_eq!(parsed.state, SocialPostRecordState::Draft);
        assert_eq!(parsed.supervisor_score, Some(0.95));
        assert!(!parsed.manually_approved);
    }

    #[test]
    fn append_to_convenience_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let record = make_record("post-via-convenience");
        SocialAuditLog::append_to(dir.path(), &record).unwrap();

        let log = SocialAuditLog::open(dir.path());
        let records = log.read_all().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].post_id, "post-via-convenience");
    }

    #[test]
    fn skips_malformed_lines_continues_reading() {
        let dir = tempfile::tempdir().unwrap();
        let log = SocialAuditLog::open(dir.path());

        let good = make_record("post-good");
        log.append(&good).unwrap();
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(log.path())
                .unwrap();
            writeln!(f, "{{this is not valid json}}").unwrap();
        }
        log.append(&good).unwrap();

        let records = log.read_all().unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn body_preview_truncates_long_body() {
        let long_body = "a".repeat(200);
        let preview = DraftSocialRecord::make_body_preview(&long_body);
        // 99 ASCII chars + '…' (3 UTF-8 bytes) = 102 bytes, 100 chars
        assert_eq!(preview.chars().count(), 100);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn body_preview_keeps_short_body() {
        let short = "Short post text";
        let preview = DraftSocialRecord::make_body_preview(short);
        assert_eq!(preview, short);
    }
}
