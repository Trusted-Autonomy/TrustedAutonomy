// review_session_store.rs — Persistent storage for ReviewSession instances.
//
// Stores review sessions as JSON files in ~/.ta/review_sessions/<session-id>.json
// Enables multi-invocation review workflows where reviewers can pause and resume.

use std::cmp::Reverse;
use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use crate::review_session::ReviewSession;
use crate::ChangeSetError;

/// Storage backend for ReviewSession instances.
pub struct ReviewSessionStore {
    sessions_dir: PathBuf,
}

impl ReviewSessionStore {
    /// Create a new store with the given sessions directory.
    pub fn new(sessions_dir: PathBuf) -> Result<Self, ChangeSetError> {
        fs::create_dir_all(&sessions_dir)?;
        Ok(Self { sessions_dir })
    }

    /// Save a review session to disk.
    pub fn save(&self, session: &ReviewSession) -> Result<(), ChangeSetError> {
        let path = self.session_path(session.session_id);
        let json = serde_json::to_string_pretty(session)?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// Load a review session from disk by ID.
    pub fn load(&self, session_id: Uuid) -> Result<ReviewSession, ChangeSetError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(ChangeSetError::InvalidData(format!(
                "Review session not found: {}",
                session_id
            )));
        }
        let json = fs::read_to_string(&path)?;
        let session = serde_json::from_str(&json)?;
        Ok(session)
    }

    /// List all review sessions.
    pub fn list(&self) -> Result<Vec<ReviewSession>, ChangeSetError> {
        let mut sessions = Vec::new();
        if !self.sessions_dir.exists() {
            return Ok(sessions);
        }

        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<ReviewSession>(&json) {
                        sessions.push(session);
                    }
                }
            }
        }

        // Sort by updated_at descending (most recent first).
        sessions.sort_by_key(|s| Reverse(s.updated_at));
        Ok(sessions)
    }

    /// Find the active review session for a given draft package (if any).
    pub fn find_active_for_draft(
        &self,
        draft_package_id: Uuid,
    ) -> Result<Option<ReviewSession>, ChangeSetError> {
        let sessions = self.list()?;
        Ok(sessions.into_iter().find(|s| {
            s.draft_package_id == draft_package_id
                && s.state == crate::review_session::ReviewState::Active
        }))
    }

    /// Delete a review session from disk.
    pub fn delete(&self, session_id: Uuid) -> Result<(), ChangeSetError> {
        let path = self.session_path(session_id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Get the file path for a session ID.
    fn session_path(&self, session_id: Uuid) -> PathBuf {
        self.sessions_dir.join(format!("{}.json", session_id))
    }

    /// Check if a session exists.
    pub fn exists(&self, session_id: Uuid) -> bool {
        self.session_path(session_id).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn save_and_load_session() {
        let temp = TempDir::new().unwrap();
        let store = ReviewSessionStore::new(temp.path().to_path_buf()).unwrap();

        let mut session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        session.add_comment("fs://workspace/main.rs", "reviewer-1", "Looks good");
        session.set_disposition(
            "fs://workspace/main.rs",
            crate::draft_package::ArtifactDisposition::Approved,
        );

        store.save(&session).unwrap();

        let loaded = store.load(session.session_id).unwrap();
        assert_eq!(loaded.session_id, session.session_id);
        assert_eq!(loaded.reviewer, session.reviewer);
        assert_eq!(loaded.artifact_reviews.len(), 1);
    }

    #[test]
    fn list_sessions_returns_all() {
        let temp = TempDir::new().unwrap();
        let store = ReviewSessionStore::new(temp.path().to_path_buf()).unwrap();

        let session1 = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        let session2 = ReviewSession::new(Uuid::new_v4(), "reviewer-2".to_string());

        store.save(&session1).unwrap();
        store.save(&session2).unwrap();

        let sessions = store.list().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn find_active_for_draft_returns_active_session() {
        let temp = TempDir::new().unwrap();
        let store = ReviewSessionStore::new(temp.path().to_path_buf()).unwrap();

        let draft_id = Uuid::new_v4();
        let session = ReviewSession::new(draft_id, "reviewer-1".to_string());
        store.save(&session).unwrap();

        let found = store.find_active_for_draft(draft_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().draft_package_id, draft_id);
    }

    #[test]
    fn find_active_for_draft_returns_none_when_no_active() {
        let temp = TempDir::new().unwrap();
        let store = ReviewSessionStore::new(temp.path().to_path_buf()).unwrap();

        let draft_id = Uuid::new_v4();
        let mut session = ReviewSession::new(draft_id, "reviewer-1".to_string());
        session.state = crate::review_session::ReviewState::Completed;
        store.save(&session).unwrap();

        let found = store.find_active_for_draft(draft_id).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn delete_removes_session() {
        let temp = TempDir::new().unwrap();
        let store = ReviewSessionStore::new(temp.path().to_path_buf()).unwrap();

        let session = ReviewSession::new(Uuid::new_v4(), "reviewer-1".to_string());
        let session_id = session.session_id;

        store.save(&session).unwrap();
        assert!(store.exists(session_id));

        store.delete(session_id).unwrap();
        assert!(!store.exists(session_id));
    }

    #[test]
    fn exists_returns_false_for_nonexistent_session() {
        let temp = TempDir::new().unwrap();
        let store = ReviewSessionStore::new(temp.path().to_path_buf()).unwrap();

        assert!(!store.exists(Uuid::new_v4()));
    }
}
