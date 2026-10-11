// store.rs — GoalRunStore: persistence for GoalRun lifecycle state.
//
// Each GoalRun is stored as a JSON file: `<store_dir>/<goal_run_id>.json`.
// This keeps goals isolated and makes the store easy to inspect manually.
//
// The store supports CRUD operations plus filtering by state.

use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::GoalError;
use crate::goal_run::{slugify_title, GoalRun, GoalRunState};
use crate::phase_release;

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
    ///
    /// This is the single enforcement point for releasing a plan-phase claim
    /// (see `phase_release`): a save that moves a goal INTO a state that ends
    /// its claim releases it, whichever caller made the save.
    pub fn save(&self, goal_run: &GoalRun) -> Result<(), GoalError> {
        let path = self.goal_file(goal_run.goal_run_id);
        let previous = self.get(goal_run.goal_run_id).ok().flatten();
        let json = serde_json::to_string_pretty(goal_run)?;
        fs::write(&path, json).map_err(|source| GoalError::IoError {
            path: path.display().to_string(),
            source,
        })?;
        let ends_claim_now = phase_release::state_ends_claim(&goal_run.state);
        let held_before = previous
            .as_ref()
            .is_some_and(|p| !phase_release::state_ends_claim(&p.state));
        if ends_claim_now && held_before && goal_run.plan_phase.is_some() {
            self.release_claim(
                goal_run,
                phase_release::ReleaseTrigger::State(goal_run.state.to_string()),
                phase_release::plan_reset_warranted(goal_run),
            );
        }
        Ok(())
    }

    /// Release `goal`'s phase claim unless another live goal on the same
    /// phase (a follow-up, a retry) still holds it.
    fn release_claim(
        &self,
        goal: &GoalRun,
        trigger: phase_release::ReleaseTrigger,
        reset_plan: bool,
    ) {
        let Some(phase) = goal.plan_phase.as_deref() else {
            return;
        };
        let other_holder = self.list().unwrap_or_default().into_iter().find(|g| {
            g.goal_run_id != goal.goal_run_id
                && g.plan_phase.as_deref() == Some(phase)
                && !phase_release::state_ends_claim(&g.state)
        });
        if let Some(other) = other_holder {
            tracing::info!(
                goal_id = %goal.goal_run_id,
                phase = %phase,
                held_by = %other.goal_run_id,
                held_by_state = %other.state,
                "keeping the plan-phase claim: another live goal still holds this phase"
            );
            return;
        }
        phase_release::release(goal, trigger, reset_plan);
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
        goals.sort_by_key(|g| Reverse(g.created_at));
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
        let removed = self.get(goal_run_id).ok().flatten();
        fs::remove_file(&path).map_err(|source| GoalError::IoError {
            path: path.display().to_string(),
            source,
        })?;
        // A goal deleted while it still held its phase gives the claim up.
        if let Some(goal) = removed {
            if goal.plan_phase.is_some() && !phase_release::state_ends_claim(&goal.state) {
                let reset = !matches!(goal.state, GoalRunState::Applied | GoalRunState::Merged);
                self.release_claim(&goal, phase_release::ReleaseTrigger::Deleted, reset);
            }
        }
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

    // ---- v0.17.11.30: one enforcement point for phase claims ----

    use crate::phase_release::{PhaseClaimReleaser, PhaseRelease, ThreadReleaserGuard};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorder(Mutex<Vec<PhaseRelease>>);

    impl PhaseClaimReleaser for Recorder {
        fn release(&self, release: &PhaseRelease) {
            self.0.lock().unwrap().push(release.clone());
        }
    }

    fn goal_on_phase(store: &GoalRunStore, phase: &str) -> GoalRun {
        let mut g = make_goal_run("phase goal");
        g.plan_phase = Some(phase.to_string());
        g.state = GoalRunState::Running;
        store.save(&g).unwrap();
        g
    }

    #[test]
    fn every_terminal_state_releases_the_claim() {
        let terminal: Vec<(GoalRunState, bool)> = vec![
            (GoalRunState::Completed, true),
            (GoalRunState::Failed { reason: "x".into() }, true),
            (
                GoalRunState::Closed {
                    reason: None,
                    applied_externally_ref: None,
                },
                true,
            ),
            (
                GoalRunState::Closed {
                    reason: None,
                    applied_externally_ref: Some("PR #1".into()),
                },
                false,
            ),
            (GoalRunState::Applied, false),
            (GoalRunState::Merged, false),
        ];
        for (state, resets_plan) in terminal {
            let dir = tempdir().unwrap();
            let store = GoalRunStore::new(dir.path().join("goals")).unwrap();
            let rec = Arc::new(Recorder::default());
            let _guard = ThreadReleaserGuard::set(rec.clone());
            let mut g = goal_on_phase(&store, "v1.0.0");
            g.state = state.clone();
            store.save(&g).unwrap();
            let seen = rec.0.lock().unwrap();
            assert_eq!(seen.len(), 1, "state {state} must release exactly once");
            assert_eq!(seen[0].phase_id, "v1.0.0");
            assert_eq!(seen[0].goal_id, g.goal_run_id);
            assert_eq!(seen[0].reset_plan, resets_plan, "state {state}");
        }
    }

    #[test]
    fn completed_with_a_draft_releases_the_claim_but_keeps_the_plan_status() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();
        let rec = Arc::new(Recorder::default());
        let _guard = ThreadReleaserGuard::set(rec.clone());
        let mut g = goal_on_phase(&store, "v1.0.0");
        g.pr_package_id = Some(Uuid::new_v4());
        g.state = GoalRunState::Completed;
        store.save(&g).unwrap();
        let seen = rec.0.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(!seen[0].reset_plan);
    }

    #[test]
    fn non_terminal_saves_and_repeat_saves_do_not_release() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();
        let rec = Arc::new(Recorder::default());
        let _guard = ThreadReleaserGuard::set(rec.clone());
        let mut g = goal_on_phase(&store, "v1.0.0");
        g.state = GoalRunState::PrReady;
        store.save(&g).unwrap();
        assert!(rec.0.lock().unwrap().is_empty());
        g.state = GoalRunState::Completed;
        store.save(&g).unwrap();
        // A later progress-note style save of the finished goal must not
        // release a claim another goal may hold by now.
        store.save(&g).unwrap();
        store.update_progress_note(g.goal_run_id, "note").unwrap();
        assert_eq!(rec.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_goal_with_no_phase_never_releases() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();
        let rec = Arc::new(Recorder::default());
        let _guard = ThreadReleaserGuard::set(rec.clone());
        let mut g = make_goal_run("chat goal");
        g.state = GoalRunState::Running;
        store.save(&g).unwrap();
        g.state = GoalRunState::Completed;
        store.save(&g).unwrap();
        assert!(rec.0.lock().unwrap().is_empty());
    }

    #[test]
    fn deleting_a_goal_that_holds_its_phase_releases_the_claim() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();
        let rec = Arc::new(Recorder::default());
        let _guard = ThreadReleaserGuard::set(rec.clone());
        let g = goal_on_phase(&store, "v1.0.0");
        assert!(store.delete(g.goal_run_id).unwrap());
        let seen = rec.0.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].trigger, phase_release::ReleaseTrigger::Deleted);
        drop(seen);
        // Deleting an already-finished goal releases nothing more.
        let mut done = goal_on_phase(&store, "v1.0.1");
        done.state = GoalRunState::Completed;
        store.save(&done).unwrap();
        let before = rec.0.lock().unwrap().len();
        store.delete(done.goal_run_id).unwrap();
        assert_eq!(rec.0.lock().unwrap().len(), before);
    }

    #[test]
    fn another_live_goal_on_the_phase_keeps_the_claim() {
        let dir = tempdir().unwrap();
        let store = GoalRunStore::new(dir.path().join("goals")).unwrap();
        let rec = Arc::new(Recorder::default());
        let _guard = ThreadReleaserGuard::set(rec.clone());
        let mut first = goal_on_phase(&store, "v1.0.0");
        let _follow_up = goal_on_phase(&store, "v1.0.0");
        first.state = GoalRunState::Failed { reason: "x".into() };
        store.save(&first).unwrap();
        assert!(rec.0.lock().unwrap().is_empty());
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
