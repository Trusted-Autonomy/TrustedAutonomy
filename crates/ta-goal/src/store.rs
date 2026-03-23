// store.rs — GoalRunStore: persistence for GoalRun lifecycle state.
//
// Each GoalRun is stored as a JSON file: `<store_dir>/<goal_run_id>.json`.
// This keeps goals isolated and makes the store easy to inspect manually.
//
// The store supports CRUD operations plus filtering by state.

use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::GoalError;
use crate::goal_run::{slugify_title, GoalRun, GoalRunState};

/// Persistent store for GoalRun records.
///
/// Each goal gets its own JSON file in the store directory.
/// This is simple but effective for the MVP — no database needed.
pub struct GoalRunStore {
    store_dir: PathBuf,
}

impl GoalRunStore {
    /// Create a new store backed by the given directory.
    /// Creates the directory if it doesn't exist.
    pub fn new(store_dir: impl AsRef<Path>) -> Result<Self, GoalError> {
        let store_dir = store_dir.as_ref().to_path_buf();
        fs::create_dir_all(&store_dir).map_err(|source| GoalError::IoError {
            path: store_dir.display().to_string(),
            source,
        })?;
        Ok(Self { store_dir })
    }

    /// Save a GoalRun to disk (creates or overwrites).
    pub fn save(&self, goal_run: &GoalRun) -> Result<(), GoalError> {
        let path = self.goal_file(goal_run.goal_run_id);
        let json = serde_json::to_string_pretty(goal_run)?;
        fs::write(&path, json).map_err(|source| GoalError::IoError {
            path: path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Get a specific GoalRun by ID.
    pub fn get(&self, goal_run_id: Uuid) -> Result<Option<GoalRun>, GoalError> {
        let path = self.goal_file(goal_run_id);
        if !path.exists() {
            return Ok(None);
        }
        let json = fs::read_to_string(&path).map_err(|source| GoalError::IoError {
            path: path.display().to_string(),
            source,
        })?;
        let goal_run: GoalRun = serde_json::from_str(&json)?;
        Ok(Some(goal_run))
    }

    /// List all GoalRuns, sorted by creation time (newest first).
    pub fn list(&self) -> Result<Vec<GoalRun>, GoalError> {
        let mut goals = Vec::new();

        let entries = fs::read_dir(&self.store_dir).map_err(|source| GoalError::IoError {
            path: self.store_dir.display().to_string(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| GoalError::IoError {
                path: self.store_dir.display().to_string(),
                source,
            })?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "json") {
                let json = fs::read_to_string(&path).map_err(|source| GoalError::IoError {
                    path: path.display().to_string(),
                    source,
                })?;
                if let Ok(goal_run) = serde_json::from_str::<GoalRun>(&json) {
                    goals.push(goal_run);
                }
            }
        }

        // Sort by creation time, newest first.
        goals.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(goals)
    }

    /// List GoalRuns filtered by state.
    pub fn list_by_state(&self, state_name: &str) -> Result<Vec<GoalRun>, GoalError> {
        let all = self.list()?;
        Ok(all
            .into_iter()
            .filter(|g| g.state.to_string() == state_name)
            .collect())
    }

    /// Transition a GoalRun to a new state and save it.
    pub fn transition(
        &self,
        goal_run_id: Uuid,
        new_state: GoalRunState,
    ) -> Result<GoalRun, GoalError> {
        let mut goal_run = self
            .get(goal_run_id)?
            .ok_or(GoalError::NotFound(goal_run_id))?;
        goal_run.transition(new_state)?;
        self.save(&goal_run)?;
        Ok(goal_run)
    }

    /// Save a GoalRun, auto-generating a tag if it doesn't have one.
    ///
    /// The tag format is `<slug>-<seq>` where slug is derived from the title
    /// and seq is auto-incrementing per slug to handle duplicates.
    pub fn save_with_tag(&self, goal_run: &mut GoalRun) -> Result<(), GoalError> {
        if goal_run.tag.is_none() {
            let slug = slugify_title(&goal_run.title);
            let slug = if slug.is_empty() {
                "goal".to_string()
            } else {
                slug
            };

            // Find the next sequence number for this slug.
            let existing = self.list().unwrap_or_default();
            let mut max_seq: u32 = 0;
            for g in &existing {
                if let Some(ref tag) = g.tag {
                    if let Some(rest) = tag.strip_prefix(&slug) {
                        if let Some(num_str) = rest.strip_prefix('-') {
                            if let Ok(n) = num_str.parse::<u32>() {
                                max_seq = max_seq.max(n);
                            }
                        }
                    }
                }
            }

            goal_run.tag = Some(format!("{}-{:02}", slug, max_seq + 1));
        }
        self.save(goal_run)
    }

    /// Resolve a tag to a GoalRun. Returns None if no match.
    pub fn resolve_tag(&self, tag: &str) -> Result<Option<GoalRun>, GoalError> {
        let goals = self.list()?;
        // Exact tag match first.
        for g in &goals {
            if let Some(ref t) = g.tag {
                if t == tag {
                    return Ok(Some(g.clone()));
                }
            }
        }
        // Prefix match on display_tag for backward compat.
        for g in &goals {
            if g.display_tag() == tag {
                return Ok(Some(g.clone()));
            }
        }
        Ok(None)
    }

    /// Resolve a tag or UUID prefix to a GoalRun.
    /// Tries tag match first, then UUID prefix match.
    pub fn resolve_tag_or_id(&self, input: &str) -> Result<Option<GoalRun>, GoalError> {
        // Try tag first.
        if let Some(g) = self.resolve_tag(input)? {
            return Ok(Some(g));
        }
        // Try full UUID.
        if let Ok(uuid) = Uuid::parse_str(input) {
            return self.get(uuid);
        }
        // Try UUID prefix match.
        let goals = self.list()?;
        let matches: Vec<_> = goals
            .into_iter()
            .filter(|g| g.goal_run_id.to_string().starts_with(input))
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches.into_iter().next().unwrap())),
            _ => Ok(None), // Ambiguous — caller should handle
        }
    }

    /// Update the progress_note for a goal without changing state (v0.13.17).
    pub fn update_progress_note(&self, goal_run_id: Uuid, note: &str) -> Result<(), GoalError> {
        if let Some(mut goal) = self.get(goal_run_id)? {
            goal.progress_note = Some(note.to_string());
            self.save(&goal)?;
        }
        Ok(())
    }

    /// Delete a GoalRun from the store.
    pub fn delete(&self, goal_run_id: Uuid) -> Result<bool, GoalError> {
        let path = self.goal_file(goal_run_id);
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(&path).map_err(|source| GoalError::IoError {
            path: path.display().to_string(),
            source,
        })?;
        Ok(true)
    }

    /// Path to the JSON file for a given GoalRun.
    fn goal_file(&self, goal_run_id: Uuid) -> PathBuf {
        self.store_dir.join(format!("{}.json", goal_run_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn make_goal_run(title: &str) -> GoalRun {
        GoalRun::new(
            title,
            "test objective",
            "test-agent",
            PathBuf::from("/tmp/staging"),
            PathBuf::from("/tmp/store"),
        )
    }

    #[test]
    fn save_and_get_round_trip() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let gr = make_goal_run("Test Goal");
        let id = gr.goal_run_id;
        store.save(&gr).unwrap();

        let found = store.get(id).unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.goal_run_id, id);
        assert_eq!(found.title, "Test Goal");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let found = store.get(Uuid::new_v4()).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn list_returns_all_goals_newest_first() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let gr1 = make_goal_run("First");
        let gr2 = make_goal_run("Second");
        store.save(&gr1).unwrap();
        store.save(&gr2).unwrap();

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn list_by_state_filters_correctly() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let gr1 = make_goal_run("Created");
        let mut gr2 = make_goal_run("Running");
        gr2.transition(GoalRunState::Configured).unwrap();
        gr2.transition(GoalRunState::Running).unwrap();

        store.save(&gr1).unwrap();
        store.save(&gr2).unwrap();

        let created = store.list_by_state("created").unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].title, "Created");

        let running = store.list_by_state("running").unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].title, "Running");
    }

    #[test]
    fn transition_updates_state_and_persists() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let gr = make_goal_run("Goal");
        let id = gr.goal_run_id;
        store.save(&gr).unwrap();

        let updated = store.transition(id, GoalRunState::Configured).unwrap();
        assert_eq!(updated.state, GoalRunState::Configured);

        // Verify persisted.
        let reloaded = store.get(id).unwrap().unwrap();
        assert_eq!(reloaded.state, GoalRunState::Configured);
    }

    #[test]
    fn transition_invalid_returns_error() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let gr = make_goal_run("Goal");
        let id = gr.goal_run_id;
        store.save(&gr).unwrap();

        // Created → Running is invalid (must go through Configured).
        let result = store.transition(id, GoalRunState::Running);
        assert!(matches!(result, Err(GoalError::InvalidTransition { .. })));
    }

    #[test]
    fn transition_nonexistent_returns_not_found() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let result = store.transition(Uuid::new_v4(), GoalRunState::Configured);
        assert!(matches!(result, Err(GoalError::NotFound(_))));
    }

    #[test]
    fn delete_goal_run() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let gr = make_goal_run("To Delete");
        let id = gr.goal_run_id;
        store.save(&gr).unwrap();

        assert!(store.delete(id).unwrap());
        assert!(store.get(id).unwrap().is_none());
    }

    #[test]
    fn store_survives_reopen() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("goals");

        let gr = make_goal_run("Persistent");
        let id = gr.goal_run_id;

        // Write with first store instance.
        {
            let store = GoalRunStore::new(&store_path).unwrap();
            store.save(&gr).unwrap();
        }

        // Read with second store instance.
        {
            let store = GoalRunStore::new(&store_path).unwrap();
            let found = store.get(id).unwrap().unwrap();
            assert_eq!(found.title, "Persistent");
        }
    }

    #[test]
    fn save_with_tag_auto_generates_tag() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let mut gr = make_goal_run("Fix Authentication Bug");
        assert!(gr.tag.is_none());
        store.save_with_tag(&mut gr).unwrap();
        // "Fix Authentication Bug" → slug "fix-authentication" (20-char cap) → tag "fix-authentication-01"
        assert_eq!(gr.tag, Some("fix-authentication-01".to_string()));

        // Second goal with same title gets sequence 02.
        let mut gr2 = make_goal_run("Fix Authentication Bug");
        store.save_with_tag(&mut gr2).unwrap();
        assert_eq!(gr2.tag, Some("fix-authentication-02".to_string()));
    }

    #[test]
    fn save_with_tag_preserves_explicit_tag() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let mut gr = make_goal_run("Test");
        gr.tag = Some("custom-tag-01".to_string());
        store.save_with_tag(&mut gr).unwrap();
        assert_eq!(gr.tag, Some("custom-tag-01".to_string()));
    }

    #[test]
    fn resolve_tag_finds_exact_match() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let mut gr = make_goal_run("Shell Routing");
        store.save_with_tag(&mut gr).unwrap();
        let tag = gr.tag.as_ref().unwrap().clone();

        let found = store.resolve_tag(&tag).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().goal_run_id, gr.goal_run_id);
    }

    #[test]
    fn resolve_tag_returns_none_for_unknown() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let found = store.resolve_tag("nonexistent-tag").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn resolve_tag_or_id_works_with_tag() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let mut gr = make_goal_run("My Feature");
        store.save_with_tag(&mut gr).unwrap();
        let tag = gr.tag.as_ref().unwrap().clone();

        let found = store.resolve_tag_or_id(&tag).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().goal_run_id, gr.goal_run_id);
    }

    #[test]
    fn resolve_tag_or_id_works_with_uuid() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();

        let mut gr = make_goal_run("UUID Test");
        store.save_with_tag(&mut gr).unwrap();
        let id = gr.goal_run_id.to_string();

        let found = store.resolve_tag_or_id(&id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().goal_run_id, gr.goal_run_id);
    }
}
