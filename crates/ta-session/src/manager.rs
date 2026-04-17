// manager.rs — SessionManager: CRUD for TaSession instances.
//
// Stores sessions as JSON files in .ta/sessions/<session-id>.json.

use std::cmp::Reverse;
use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use crate::error::SessionError;
use crate::session::{SessionState, TaSession};

/// Persistent storage and lifecycle management for TaSession instances.
pub struct SessionManager {
    sessions_dir: PathBuf,
}

impl SessionManager {
    /// Create a new session manager with the given storage directory.
    pub fn new(sessions_dir: PathBuf) -> Result<Self, SessionError> {
        fs::create_dir_all(&sessions_dir).map_err(|source| SessionError::Io {
            path: sessions_dir.display().to_string(),
            source,
        })?;
        Ok(Self { sessions_dir })
    }

    /// Create and persist a new session for a goal.
    pub fn create(&self, goal_id: Uuid, agent_id: &str) -> Result<TaSession, SessionError> {
        let session = TaSession::new(goal_id, agent_id);
        self.save(&session)?;
        Ok(session)
    }

    /// Save a session to disk.
    pub fn save(&self, session: &TaSession) -> Result<(), SessionError> {
        let path = self.session_path(session.session_id);
        let json = serde_json::to_string_pretty(session)?;
        fs::write(&path, json).map_err(|source| SessionError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Load a session from disk by ID.
    pub fn load(&self, session_id: Uuid) -> Result<TaSession, SessionError> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Err(SessionError::NotFound {
                session_id: session_id.to_string(),
            });
        }
        let json = fs::read_to_string(&path).map_err(|source| SessionError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let session = serde_json::from_str(&json)?;
        Ok(session)
    }

    /// Find the active session for a goal (if any).
    pub fn find_for_goal(&self, goal_id: Uuid) -> Result<Option<TaSession>, SessionError> {
        let sessions = self.list()?;
        Ok(sessions
            .into_iter()
            .find(|s| s.goal_id == goal_id && s.is_active()))
    }

    /// List all sessions, sorted by most recently updated.
    pub fn list(&self) -> Result<Vec<TaSession>, SessionError> {
        let mut sessions = Vec::new();
        if !self.sessions_dir.exists() {
            return Ok(sessions);
        }

        for entry in fs::read_dir(&self.sessions_dir).map_err(|source| SessionError::Io {
            path: self.sessions_dir.display().to_string(),
            source,
        })? {
            let entry = entry.map_err(|source| SessionError::Io {
                path: self.sessions_dir.display().to_string(),
                source,
            })?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<TaSession>(&json) {
                        sessions.push(session);
                    }
                }
            }
        }

        sessions.sort_by_key(|s| Reverse(s.updated_at));
        Ok(sessions)
    }

    /// List only active sessions (not completed, aborted, or failed).
    pub fn list_active(&self) -> Result<Vec<TaSession>, SessionError> {
        Ok(self.list()?.into_iter().filter(|s| s.is_active()).collect())
    }

    /// Pause a session (transition to Paused state).
    pub fn pause(&self, session_id: Uuid) -> Result<TaSession, SessionError> {
        let mut session = self.load(session_id)?;
        session.transition(SessionState::Paused)?;
        self.save(&session)?;
        Ok(session)
    }

    /// Resume a paused session (transition back to AgentRunning).
    pub fn resume(&self, session_id: Uuid) -> Result<TaSession, SessionError> {
        let mut session = self.load(session_id)?;
        session.transition(SessionState::AgentRunning)?;
        self.save(&session)?;
        Ok(session)
    }

    /// Abort a session.
    pub fn abort(&self, session_id: Uuid) -> Result<TaSession, SessionError> {
        let mut session = self.load(session_id)?;
        session.transition(SessionState::Aborted)?;
        self.save(&session)?;
        Ok(session)
    }

    /// Delete a session from disk.
    pub fn delete(&self, session_id: Uuid) -> Result<(), SessionError> {
        let path = self.session_path(session_id);
        if path.exists() {
            fs::remove_file(&path).map_err(|source| SessionError::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        Ok(())
    }

    /// Check if a session exists.
    pub fn exists(&self, session_id: Uuid) -> bool {
        self.session_path(session_id).exists()
    }

    fn session_path(&self, session_id: Uuid) -> PathBuf {
        self.sessions_dir.join(format!("{}.json", session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_and_load_session() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();

        let goal_id = Uuid::new_v4();
        let session = manager.create(goal_id, "claude-code").unwrap();

        let loaded = manager.load(session.session_id).unwrap();
        assert_eq!(loaded.session_id, session.session_id);
        assert_eq!(loaded.goal_id, goal_id);
        assert_eq!(loaded.agent_id, "claude-code");
    }

    #[test]
    fn list_sessions() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();

        manager.create(Uuid::new_v4(), "agent-1").unwrap();
        manager.create(Uuid::new_v4(), "agent-2").unwrap();

        let sessions = manager.list().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn list_active_filters_completed() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();

        let s1 = manager.create(Uuid::new_v4(), "agent-1").unwrap();
        let s2 = manager.create(Uuid::new_v4(), "agent-2").unwrap();

        // Complete s2
        let mut s2_loaded = manager.load(s2.session_id).unwrap();
        s2_loaded.state = SessionState::Completed;
        manager.save(&s2_loaded).unwrap();

        let active = manager.list_active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].session_id, s1.session_id);
    }

    #[test]
    fn find_for_goal() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();

        let goal_id = Uuid::new_v4();
        let session = manager.create(goal_id, "agent").unwrap();

        let found = manager.find_for_goal(goal_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().session_id, session.session_id);

        let not_found = manager.find_for_goal(Uuid::new_v4()).unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn pause_and_resume() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();

        let mut session = manager.create(Uuid::new_v4(), "agent").unwrap();
        session.transition(SessionState::AgentRunning).unwrap();
        manager.save(&session).unwrap();

        let paused = manager.pause(session.session_id).unwrap();
        assert_eq!(paused.state, SessionState::Paused);

        let resumed = manager.resume(session.session_id).unwrap();
        assert_eq!(resumed.state, SessionState::AgentRunning);
    }

    #[test]
    fn abort_session() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();

        let mut session = manager.create(Uuid::new_v4(), "agent").unwrap();
        session.transition(SessionState::AgentRunning).unwrap();
        manager.save(&session).unwrap();

        let aborted = manager.abort(session.session_id).unwrap();
        assert_eq!(aborted.state, SessionState::Aborted);
        assert!(!aborted.is_active());
    }

    #[test]
    fn delete_session() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();

        let session = manager.create(Uuid::new_v4(), "agent").unwrap();
        assert!(manager.exists(session.session_id));

        manager.delete(session.session_id).unwrap();
        assert!(!manager.exists(session.session_id));
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let temp = TempDir::new().unwrap();
        let manager = SessionManager::new(temp.path().to_path_buf()).unwrap();
        assert!(manager.load(Uuid::new_v4()).is_err());
    }
}
