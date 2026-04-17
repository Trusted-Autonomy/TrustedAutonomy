// interactive_session_store.rs — Persistent storage for InteractiveSession instances.
//
// Stores interactive sessions as JSON files in .ta/interactive_sessions/<session-id>.json
// Enables multi-invocation interactive workflows where humans can pause and resume sessions.

use std::cmp::Reverse;
use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use crate::session_channel::{InteractiveSession, InteractiveSessionState};
use crate::ChangeSetError;

/// Storage backend for InteractiveSession instances.
pub struct InteractiveSessionStore {
    sessions_dir: PathBuf,
}

impl InteractiveSessionStore {
    /// Create a new store with the given sessions directory.
    pub fn new(sessions_dir: PathBuf) -> Result<Self, ChangeSetError> {
        fs::create_dir_all(&sessions_dir)?;
        Ok(Self { sessions_dir })
    }

    /// Save an interactive session to disk.
    pub fn save(&self, session: &InteractiveSession) -> Result<(), ChangeSetError> {
        let path = self.session_path(session.session_id);
        let json = serde_json::to_string_pretty(session)?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// Load an interactive session from disk by ID.
    pub fn load(&self, session_id: Uuid) -> Result<InteractiveSession, ChangeSetError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(ChangeSetError::InvalidData(format!(
                "Interactive session not found: {}",
                session_id
            )));
        }
        let json = fs::read_to_string(&path)?;
        let session = serde_json::from_str(&json)?;
        Ok(session)
    }

    /// List all interactive sessions, sorted by most recently updated.
    pub fn list(&self) -> Result<Vec<InteractiveSession>, ChangeSetError> {
        let mut sessions = Vec::new();
        if !self.sessions_dir.exists() {
            return Ok(sessions);
        }

        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<InteractiveSession>(&json) {
                        sessions.push(session);
                    }
                }
            }
        }

        sessions.sort_by_key(|s| Reverse(s.updated_at));
        Ok(sessions)
    }

    /// List only active/paused (alive) sessions.
    pub fn list_alive(&self) -> Result<Vec<InteractiveSession>, ChangeSetError> {
        Ok(self.list()?.into_iter().filter(|s| s.is_alive()).collect())
    }

    /// Find the active interactive session for a goal (if any).
    pub fn find_active_for_goal(
        &self,
        goal_id: Uuid,
    ) -> Result<Option<InteractiveSession>, ChangeSetError> {
        let sessions = self.list()?;
        Ok(sessions
            .into_iter()
            .find(|s| s.goal_id == goal_id && s.state == InteractiveSessionState::Active))
    }

    /// Delete an interactive session from disk.
    pub fn delete(&self, session_id: Uuid) -> Result<(), ChangeSetError> {
        let path = self.session_path(session_id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Check if a session exists.
    pub fn exists(&self, session_id: Uuid) -> bool {
        self.session_path(session_id).exists()
    }

    /// Get the file path for a session ID.
    fn session_path(&self, session_id: Uuid) -> PathBuf {
        self.sessions_dir.join(format!("{}.json", session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn save_and_load_session() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();

        let mut session = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        session.log_message("human", "Test guidance");

        store.save(&session).unwrap();

        let loaded = store.load(session.session_id).unwrap();
        assert_eq!(loaded.session_id, session.session_id);
        assert_eq!(loaded.channel_id, "cli:tty0");
        assert_eq!(loaded.messages.len(), 1);
    }

    #[test]
    fn list_sessions_returns_all() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();

        let session1 = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        let session2 =
            InteractiveSession::new(Uuid::new_v4(), "cli:tty1".to_string(), "codex".to_string());

        store.save(&session1).unwrap();
        store.save(&session2).unwrap();

        let sessions = store.list().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn list_alive_filters_completed() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();

        let session1 = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        let mut session2 =
            InteractiveSession::new(Uuid::new_v4(), "cli:tty1".to_string(), "codex".to_string());
        session2
            .transition(InteractiveSessionState::Completed)
            .unwrap();

        store.save(&session1).unwrap();
        store.save(&session2).unwrap();

        let alive = store.list_alive().unwrap();
        assert_eq!(alive.len(), 1);
        assert_eq!(alive[0].session_id, session1.session_id);
    }

    #[test]
    fn find_active_for_goal_returns_matching() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();

        let goal_id = Uuid::new_v4();
        let session =
            InteractiveSession::new(goal_id, "cli:tty0".to_string(), "claude-code".to_string());
        store.save(&session).unwrap();

        let found = store.find_active_for_goal(goal_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().goal_id, goal_id);
    }

    #[test]
    fn find_active_for_goal_returns_none_when_completed() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();

        let goal_id = Uuid::new_v4();
        let mut session =
            InteractiveSession::new(goal_id, "cli:tty0".to_string(), "claude-code".to_string());
        session
            .transition(InteractiveSessionState::Completed)
            .unwrap();
        store.save(&session).unwrap();

        let found = store.find_active_for_goal(goal_id).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn delete_removes_session() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();

        let session = InteractiveSession::new(
            Uuid::new_v4(),
            "cli:tty0".to_string(),
            "claude-code".to_string(),
        );
        let session_id = session.session_id;

        store.save(&session).unwrap();
        assert!(store.exists(session_id));

        store.delete(session_id).unwrap();
        assert!(!store.exists(session_id));
    }

    #[test]
    fn exists_returns_false_for_nonexistent() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();
        assert!(!store.exists(Uuid::new_v4()));
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let temp = TempDir::new().unwrap();
        let store = InteractiveSessionStore::new(temp.path().to_path_buf()).unwrap();
        let result = store.load(Uuid::new_v4());
        assert!(result.is_err());
    }
}
