// review_session.rs — ReviewSession: persistent multi-interaction review state.
//
// A ReviewSession tracks the state of a human reviewer working through a DraftPackage
// across multiple CLI invocations. It includes:
// - Which artifacts have been reviewed
// - Per-artifact comment threads
// - Current review focus (which artifact the reviewer is examining)
// - Session metadata (created, last updated, reviewer identity)
//
// This enables workflows like:
//   ta draft review start <draft-id>
//   ta draft review comment <artifact-uri> "needs error handling"
//   ta draft review next
//   ta draft review comment <artifact-uri> "looks good"
//   ta draft review finish --approve "src/**" --reject "config.toml"

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::draft_package::ArtifactDisposition;

/// A persistent review session for a DraftPackage.
///
/// Tracks the reviewer's progress through a draft across multiple CLI invocations,
/// including comments, dispositions, and current focus.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewSession {
    /// Unique session identifier.
    pub session_id: Uuid,
    /// The DraftPackage being reviewed.
    pub draft_package_id: Uuid,
    /// Reviewer identity.
    pub reviewer: String,
    /// Session creation time.
    pub created_at: DateTime<Utc>,
    /// Last activity time.
    pub updated_at: DateTime<Utc>,
    /// Current review state.
    pub state: ReviewState,
    /// Per-artifact review data (keyed by resource_uri).
    pub artifact_reviews: HashMap<String, ArtifactReview>,
    /// Session-level notes (not tied to specific artifacts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_notes: Vec<SessionNote>,
    /// Current focus: which artifact URI the reviewer is examining.
    /// Used by "ta draft review next" to resume from where they left off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_focus: Option<String>,
}

impl ReviewSession {
    /// Create a new review session for a draft package.
    pub fn new(draft_package_id: Uuid, reviewer: String) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            draft_package_id,
            reviewer,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            state: ReviewState::Active,
            artifact_reviews: HashMap::new(),
            session_notes: Vec::new(),
            current_focus: None,
        }
    }

    /// Mark the session as updated (call after any mutation).
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Add a comment to an artifact.
    pub fn add_comment(
        &mut self,
        artifact_uri: &str,
        commenter: &str,
        text: &str,
    ) -> &CommentThread {
        self.touch();
        let review = self
            .artifact_reviews
            .entry(artifact_uri.to_string())
            .or_insert_with(|| ArtifactReview {
                resource_uri: artifact_uri.to_string(),
                disposition: ArtifactDisposition::Pending,
                comments: CommentThread::new(),
                reviewed_at: None,
            });
        review.comments.add(commenter, text);
        &review.comments
    }

    /// Set the disposition for an artifact.
    pub fn set_disposition(&mut self, artifact_uri: &str, disposition: ArtifactDisposition) {
        self.touch();
        let review = self
            .artifact_reviews
            .entry(artifact_uri.to_string())
            .or_insert_with(|| ArtifactReview {
                resource_uri: artifact_uri.to_string(),
                disposition: ArtifactDisposition::Pending,
                comments: CommentThread::new(),
                reviewed_at: None,
            });
        review.disposition = disposition;
        review.reviewed_at = Some(Utc::now());
    }

    /// Add a session-level note (not tied to a specific artifact).
    pub fn add_session_note(&mut self, text: &str) {
        self.touch();
        self.session_notes.push(SessionNote {
            text: text.to_string(),
            created_at: Utc::now(),
        });
    }

    /// Get the current disposition for an artifact (None if not yet reviewed).
    pub fn get_disposition(&self, artifact_uri: &str) -> Option<ArtifactDisposition> {
        self.artifact_reviews
            .get(artifact_uri)
            .map(|r| r.disposition.clone())
    }

    /// Get all artifacts with a specific disposition.
    pub fn artifacts_with_disposition(
        &self,
        disposition: &ArtifactDisposition,
    ) -> Vec<&ArtifactReview> {
        self.artifact_reviews
            .values()
            .filter(|r| &r.disposition == disposition)
            .collect()
    }

    /// Count artifacts by disposition.
    pub fn disposition_counts(&self) -> DispositionCounts {
        let mut counts = DispositionCounts::default();
        for review in self.artifact_reviews.values() {
            match review.disposition {
                ArtifactDisposition::Pending => counts.pending += 1,
                ArtifactDisposition::Approved => counts.approved += 1,
                ArtifactDisposition::Rejected => counts.rejected += 1,
                ArtifactDisposition::Discuss => counts.discuss += 1,
            }
        }
        counts
    }

    /// Finish the review session and return final disposition summary.
    pub fn finish(&mut self) -> DispositionCounts {
        self.touch();
        self.state = ReviewState::Completed;
        self.disposition_counts()
    }

    /// Check if the session has any unresolved discuss items.
    pub fn has_unresolved_discuss(&self) -> bool {
        self.artifact_reviews
            .values()
            .any(|r| r.disposition == ArtifactDisposition::Discuss)
    }
}

/// Review state lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// Session is active (reviewer is working through artifacts).
    Active,
    /// Session is paused (can be resumed later).
    Paused,
    /// Session is completed (review finished, dispositions finalized).
    Completed,
    /// Session was abandoned (reviewer gave up or session expired).
    Abandoned,
}

/// Review data for a single artifact.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactReview {
    /// The artifact's resource URI.
    pub resource_uri: String,
    /// Current disposition (defaults to Pending).
    pub disposition: ArtifactDisposition,
    /// Comment thread for this artifact.
    pub comments: CommentThread,
    /// When this artifact was last reviewed (disposition set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<DateTime<Utc>>,
}

/// A thread of comments on an artifact.
///
/// Supports multi-party discussion: reviewer, agent, other reviewers, etc.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CommentThread {
    pub comments: Vec<Comment>,
}

impl CommentThread {
    pub fn new() -> Self {
        Self {
            comments: Vec::new(),
        }
    }

    /// Add a comment to the thread.
    pub fn add(&mut self, commenter: &str, text: &str) {
        self.comments.push(Comment {
            commenter: commenter.to_string(),
            text: text.to_string(),
            created_at: Utc::now(),
            reasoning: None,
        });
    }

    /// Add a comment with structured reasoning to the thread (v0.3.3).
    pub fn add_with_reasoning(&mut self, commenter: &str, text: &str, reasoning: ReviewReasoning) {
        self.comments.push(Comment {
            commenter: commenter.to_string(),
            text: text.to_string(),
            created_at: Utc::now(),
            reasoning: Some(reasoning),
        });
    }

    /// Check if the thread is empty.
    pub fn is_empty(&self) -> bool {
        self.comments.is_empty()
    }

    /// Get the number of comments in the thread.
    pub fn len(&self) -> usize {
        self.comments.len()
    }
}

impl Default for CommentThread {
    fn default() -> Self {
        Self::new()
    }
}

/// A single comment in a thread.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Comment {
    /// Who wrote the comment (human reviewer, agent, etc.).
    pub commenter: String,
    /// Comment text (markdown supported).
    pub text: String,
    /// When the comment was created.
    pub created_at: DateTime<Utc>,
    /// Structured reasoning for this review decision (v0.3.3).
    /// Reviewer can explain *why* they approved/rejected, not just leave text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReviewReasoning>,
}

/// Structured reasoning attached to a review comment (v0.3.3).
///
/// Enables compliance reporting: reviewers document *why* they approved or rejected,
/// what alternatives they considered, and what principles guided the decision.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewReasoning {
    /// The reviewer's rationale for their decision.
    pub rationale: String,
    /// Alternatives the reviewer considered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives_considered: Vec<String>,
    /// Principles or policies that informed the decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_principles: Vec<String>,
}

/// Session-level note (not tied to a specific artifact).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionNote {
    pub text: String,
    pub created_at: DateTime<Utc>,
}

/// Summary counts of artifact dispositions.
#[derive(Debug, Clone, Default)]
pub struct DispositionCounts {
    pub pending: usize,
    pub approved: usize,
    pub rejected: usize,
    pub discuss: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_has_active_state() {
        let session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        assert_eq!(session.state, ReviewState::Active);
        assert!(session.artifact_reviews.is_empty());
        assert!(session.session_notes.is_empty());
        assert!(session.current_focus.is_none());
    }

    #[test]
    fn add_comment_creates_artifact_review() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        let uri = "fs://workspace/src/main.rs";

        session.add_comment(uri, "reviewer-1", "Needs error handling");

        assert_eq!(session.artifact_reviews.len(), 1);
        let review = session.artifact_reviews.get(uri).unwrap();
        assert_eq!(review.comments.len(), 1);
        assert_eq!(review.comments.comments[0].text, "Needs error handling");
        assert_eq!(review.disposition, ArtifactDisposition::Pending);
    }

    #[test]
    fn set_disposition_updates_review() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        let uri = "fs://workspace/src/main.rs";

        session.set_disposition(uri, ArtifactDisposition::Approved);

        let review = session.artifact_reviews.get(uri).unwrap();
        assert_eq!(review.disposition, ArtifactDisposition::Approved);
        assert!(review.reviewed_at.is_some());
    }

    #[test]
    fn disposition_counts_are_accurate() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());

        session.set_disposition("fs://workspace/a.rs", ArtifactDisposition::Approved);
        session.set_disposition("fs://workspace/b.rs", ArtifactDisposition::Approved);
        session.set_disposition("fs://workspace/c.rs", ArtifactDisposition::Rejected);
        session.set_disposition("fs://workspace/d.rs", ArtifactDisposition::Discuss);

        let counts = session.disposition_counts();
        assert_eq!(counts.approved, 2);
        assert_eq!(counts.rejected, 1);
        assert_eq!(counts.discuss, 1);
        assert_eq!(counts.pending, 0);
    }

    #[test]
    fn has_unresolved_discuss_returns_true_when_discuss_items_exist() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());

        session.set_disposition("fs://workspace/a.rs", ArtifactDisposition::Approved);
        session.set_disposition("fs://workspace/b.rs", ArtifactDisposition::Discuss);

        assert!(session.has_unresolved_discuss());
    }

    #[test]
    fn has_unresolved_discuss_returns_false_when_no_discuss_items() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());

        session.set_disposition("fs://workspace/a.rs", ArtifactDisposition::Approved);
        session.set_disposition("fs://workspace/b.rs", ArtifactDisposition::Rejected);

        assert!(!session.has_unresolved_discuss());
    }

    #[test]
    fn finish_sets_state_to_completed() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        session.set_disposition("fs://workspace/a.rs", ArtifactDisposition::Approved);

        let counts = session.finish();

        assert_eq!(session.state, ReviewState::Completed);
        assert_eq!(counts.approved, 1);
    }

    #[test]
    fn session_serialization_round_trip() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        session.add_comment("fs://workspace/main.rs", "reviewer-1", "Looks good");
        session.set_disposition("fs://workspace/main.rs", ArtifactDisposition::Approved);
        session.add_session_note("Overall: well structured");

        let json = serde_json::to_string(&session).unwrap();
        let restored: ReviewSession = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.session_id, session.session_id);
        assert_eq!(restored.reviewer, session.reviewer);
        assert_eq!(restored.artifact_reviews.len(), 1);
        assert_eq!(restored.session_notes.len(), 1);
    }

    #[test]
    fn comment_thread_tracks_multiple_comments() {
        let mut thread = CommentThread::new();
        thread.add("reviewer-1", "First comment");
        thread.add("agent-1", "Response from agent");
        thread.add("reviewer-1", "Follow-up");

        assert_eq!(thread.len(), 3);
        assert_eq!(thread.comments[0].commenter, "reviewer-1");
        assert_eq!(thread.comments[1].commenter, "agent-1");
        assert_eq!(thread.comments[2].commenter, "reviewer-1");
    }

    #[test]
    fn artifacts_with_disposition_filters_correctly() {
        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        session.set_disposition("fs://workspace/a.rs", ArtifactDisposition::Approved);
        session.set_disposition("fs://workspace/b.rs", ArtifactDisposition::Approved);
        session.set_disposition("fs://workspace/c.rs", ArtifactDisposition::Rejected);

        let approved = session.artifacts_with_disposition(&ArtifactDisposition::Approved);
        assert_eq!(approved.len(), 2);

        let rejected = session.artifacts_with_disposition(&ArtifactDisposition::Rejected);
        assert_eq!(rejected.len(), 1);
    }

    // ── v0.3.3 Review Reasoning tests ──

    #[test]
    fn comment_with_reasoning_round_trip() {
        let reasoning = ReviewReasoning {
            rationale: "Change is well-tested and follows conventions".to_string(),
            alternatives_considered: vec!["Request rework with different approach".to_string()],
            applied_principles: vec!["code-review-checklist".to_string()],
        };

        let mut thread = CommentThread::new();
        thread.add_with_reasoning("reviewer-1", "Approved with minor note", reasoning);

        assert_eq!(thread.len(), 1);
        assert!(thread.comments[0].reasoning.is_some());

        let r = thread.comments[0].reasoning.as_ref().unwrap();
        assert!(r.rationale.contains("well-tested"));
        assert_eq!(r.alternatives_considered.len(), 1);
        assert_eq!(r.applied_principles.len(), 1);
    }

    #[test]
    fn comment_without_reasoning_backward_compatible() {
        // Old JSON without reasoning field should deserialize fine.
        let json = r#"{
            "commenter": "reviewer-1",
            "text": "Looks good",
            "created_at": "2026-02-25T12:00:00Z"
        }"#;
        let comment: Comment = serde_json::from_str(json).unwrap();
        assert!(comment.reasoning.is_none());
    }

    #[test]
    fn review_reasoning_serialization() {
        let reasoning = ReviewReasoning {
            rationale: "Security fix verified".to_string(),
            alternatives_considered: vec![],
            applied_principles: vec!["security-first".to_string()],
        };

        let json = serde_json::to_string(&reasoning).unwrap();
        let restored: ReviewReasoning = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.rationale, "Security fix verified");
        // Empty alternatives_considered should be skipped in serialization.
        assert!(!json.contains("alternatives_considered"));
    }
}
