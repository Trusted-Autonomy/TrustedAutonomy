// workflow_manager.rs — WorkflowSessionManager: CRUD for WorkflowSession (v0.14.11).
//
// Stores workflow sessions as JSON files in `.ta/sessions/` using the naming
// convention `workflow-<session-id>.json` to distinguish them from
// `TaSession` records (which use `<session-id>.json`).

use std::cmp::Reverse;
use std::fs;
use std::path::PathBuf;

use uuid::Uuid;

use crate::error::SessionError;
use crate::workflow_session::WorkflowSession;

/// Persistent storage manager for WorkflowSession instances.
pub struct WorkflowSessionManager {
    sessions_dir: PathBuf,
}

impl WorkflowSessionManager {
    /// Create a new manager, ensuring the storage directory exists.
    pub fn new(sessions_dir: PathBuf) -> Result<Self, SessionError> {
        fs::create_dir_all(&sessions_dir).map_err(|source| SessionError::Io {
            path: sessions_dir.display().to_string(),
            source,
        })?;
        Ok(Self { sessions_dir })
    }

    /// Persist a workflow session to disk.
    pub fn save(&self, session: &WorkflowSession) -> Result<(), SessionError> {
        let path = self.session_path(session.session_id);
        let json = serde_json::to_string_pretty(session)?;
        fs::write(&path, json).map_err(|source| SessionError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Load a workflow session by its exact UUID.
    pub fn load(&self, session_id: Uuid) -> Result<WorkflowSession, SessionError> {
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

    /// List all workflow sessions, sorted by most recently updated first.
    pub fn list(&self) -> Result<Vec<WorkflowSession>, SessionError> {
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
            if path.extension().is_some_and(|e| e == "json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem.starts_with("workflow-") {
                        if let Ok(json) = fs::read_to_string(&path) {
                            if let Ok(s) = serde_json::from_str::<WorkflowSession>(&json) {
                                sessions.push(s);
                            }
                        }
                    }
                }
            }
        }

        sessions.sort_by_key(|s| Reverse(s.updated_at));
        Ok(sessions)
    }

    /// Resolve a session ID from a full UUID string or an unambiguous prefix.
    ///
    /// Returns the full UUID on success, or an error if not found or ambiguous.
    pub fn resolve_id(&self, id: &str) -> Result<Uuid, SessionError> {
        if let Ok(uuid) = Uuid::parse_str(id) {
            return Ok(uuid);
        }
        let all = self.list()?;
        let matches: Vec<_> = all
            .iter()
            .filter(|s| s.session_id.to_string().starts_with(id))
            .collect();
        match matches.len() {
            0 => Err(SessionError::NotFound {
                session_id: id.to_string(),
            }),
            1 => Ok(matches[0].session_id),
            n => Err(SessionError::NotFound {
                session_id: format!("ambiguous prefix '{}' matches {} sessions", id, n),
            }),
        }
    }

    /// Find the active workflow session for a given plan ID (if any).
    pub fn find_for_plan(&self, plan_id: Uuid) -> Result<Option<WorkflowSession>, SessionError> {
        Ok(self.list()?.into_iter().find(|s| s.plan_id == plan_id))
    }

    /// Returns true if a session file exists for the given UUID.
    pub fn exists(&self, session_id: Uuid) -> bool {
        self.session_path(session_id).exists()
    }

    /// Delete a workflow session from disk.
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

    fn session_path(&self, session_id: Uuid) -> PathBuf {
        self.sessions_dir
            .join(format!("workflow-{}.json", session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{PlanDocument, PlanItem};
    use crate::workflow_session::{GateMode, WorkflowItemState, WorkflowSession};
    use tempfile::TempDir;

    fn make_session() -> WorkflowSession {
        let mut plan = PlanDocument::new("Test Plan");
        plan.add_item(PlanItem::new("Item 1"));
        plan.add_item(PlanItem::new("Item 2"));
        WorkflowSession::from_plan(&plan, GateMode::Auto)
    }

    #[test]
    fn save_and_load_round_trip() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();

        let session = make_session();
        mgr.save(&session).unwrap();

        let loaded = mgr.load(session.session_id).unwrap();
        assert_eq!(loaded.session_id, session.session_id);
        assert_eq!(loaded.plan_title, "Test Plan");
        assert_eq!(loaded.items.len(), 2);
    }

    #[test]
    fn load_nonexistent_returns_not_found() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();
        let err = mgr.load(Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, SessionError::NotFound { .. }));
    }

    #[test]
    fn list_sessions_sorted_by_updated_at() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();

        let s1 = make_session();
        let mut s2 = make_session();
        // Make s2 appear more recently updated.
        s2.updated_at = s1.updated_at + chrono::Duration::seconds(10);

        mgr.save(&s1).unwrap();
        mgr.save(&s2).unwrap();

        let sessions = mgr.list().unwrap();
        assert_eq!(sessions.len(), 2);
        // Most recently updated first.
        assert_eq!(sessions[0].session_id, s2.session_id);
    }

    #[test]
    fn list_only_returns_workflow_sessions() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();

        // Write a non-workflow file in the same directory — should be ignored.
        let non_workflow = temp.path().join("some-other-uuid.json");
        fs::write(&non_workflow, r#"{"not": "a workflow session"}"#).unwrap();

        let session = make_session();
        mgr.save(&session).unwrap();

        let sessions = mgr.list().unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn resolve_id_by_exact_uuid() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();
        let session = make_session();
        mgr.save(&session).unwrap();

        let resolved = mgr.resolve_id(&session.session_id.to_string()).unwrap();
        assert_eq!(resolved, session.session_id);
    }

    #[test]
    fn resolve_id_by_prefix() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();
        let session = make_session();
        mgr.save(&session).unwrap();

        let prefix = &session.session_id.to_string()[..8];
        let resolved = mgr.resolve_id(prefix).unwrap();
        assert_eq!(resolved, session.session_id);
    }

    #[test]
    fn resolve_id_unknown_returns_not_found() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();
        assert!(mgr.resolve_id("00000000").is_err());
    }

    #[test]
    fn find_for_plan() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();

        let session = make_session();
        let plan_id = session.plan_id;
        mgr.save(&session).unwrap();

        let found = mgr.find_for_plan(plan_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().plan_id, plan_id);

        let not_found = mgr.find_for_plan(Uuid::new_v4()).unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn exists_and_delete() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();
        let session = make_session();

        assert!(!mgr.exists(session.session_id));
        mgr.save(&session).unwrap();
        assert!(mgr.exists(session.session_id));

        mgr.delete(session.session_id).unwrap();
        assert!(!mgr.exists(session.session_id));
    }

    #[test]
    fn save_and_reload_with_item_state_change() {
        let temp = TempDir::new().unwrap();
        let mgr = WorkflowSessionManager::new(temp.path().to_path_buf()).unwrap();

        let mut session = make_session();
        let item_id = session.items[0].item_id;
        session.update_item_state(item_id, WorkflowItemState::Accepted);
        mgr.save(&session).unwrap();

        let loaded = mgr.load(session.session_id).unwrap();
        assert_eq!(loaded.items[0].state, WorkflowItemState::Accepted);
    }
}
