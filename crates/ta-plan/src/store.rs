// store.rs — `PlanStore` trait: the plan/goal storage abstraction
// (v0.17.11.1 items 3-4). `FilePlanStore` is the sole implementation for
// now, wrapping the existing PLAN.md-file + `GoalRunStore`-JSON logic
// already extracted into this crate — no behavior change relative to what
// `apps/ta-cli` did before this phase, just abstracted behind a trait so a
// future non-file backend (e.g. Wayfinder-backed, out of scope here — see
// PLAN.md v0.17.11.1's own notes) can be swapped in later without touching
// any `PlanStore` consumer.
//
// Deliberately synchronous, not `async fn` (unlike the original design
// sketch in the TA<->Wayfinder integration doc): every current caller and
// the sole current implementation are synchronous filesystem operations.
// Adding `async-trait`/tokio requirements now, for a hypothetical future
// network-backed implementation that is explicitly out of scope for this
// phase, would be speculative complexity with zero present benefit. If a
// real async backend need arises later, the trait can be migrated then —
// or a sync backend can always be adapted with `tokio::task::spawn_blocking`
// at the call site without needing the trait itself to be async.

use std::path::{Path, PathBuf};

use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use uuid::Uuid;

use crate::history;
use crate::parse;
use crate::query;
use crate::schema::{PlanPhase, PlanStatus};

/// What a `PlanStore` implementation can and can't do — callers use this to
/// degrade gracefully rather than assume every backend is equally capable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanStoreCapabilities {
    /// Whether phases can be grouped natively by this backend (a file-based
    /// PLAN.md has no grouping layer above a phase; a future Wayfinder
    /// backend would need synthetic gate tasks to fake it — see the
    /// TA<->Wayfinder design doc's Candidate 1 risk notes).
    pub supports_native_phase_grouping: bool,
    /// Whether the backend can push change notifications rather than
    /// requiring callers to poll.
    pub supports_webhooks: bool,
    /// Whether goal/phase creation is idempotent (safe to retry without
    /// risk of duplicate creation).
    pub supports_idempotent_create: bool,
}

/// The plan/goal storage abstraction. `FilePlanStore` is the sole
/// implementation as of v0.17.11.1 — see the module doc comment for why
/// this is deliberately synchronous.
pub trait PlanStore: Send + Sync {
    /// Short, stable identifier for this backend (e.g. `"file"`,
    /// `"wayfinder"`), for logging/diagnostics — not a display name.
    fn backend_name(&self) -> &str;

    /// What this backend can and can't do.
    fn capabilities(&self) -> PlanStoreCapabilities;

    // ── Phase layer ──────────────────────────────────────────────────

    /// All known phases, in document/declaration order.
    fn list_phases(&self) -> anyhow::Result<Vec<PlanPhase>>;

    /// A single phase by ID, or `None` if not found.
    fn get_phase(&self, id: &str) -> anyhow::Result<Option<PlanPhase>>;

    /// Transition a phase's status. `note` is recorded in the transition
    /// history log when present (used for reset/deny/delete reasons).
    fn update_phase_status(
        &self,
        id: &str,
        new_status: PlanStatus,
        note: Option<&str>,
    ) -> anyhow::Result<()>;

    /// Phases that are `Pending` and whose declared dependencies are all
    /// `Done` — the live "what's actually ready right now" set.
    fn next_ready_phases(&self) -> anyhow::Result<Vec<PlanPhase>>;

    // ── Goal layer ───────────────────────────────────────────────────

    /// Persist a goal run (create or update — `GoalRunStore::save` is
    /// upsert-by-id).
    fn save_goal(&self, goal: &GoalRun) -> anyhow::Result<()>;

    /// A single goal run by ID, or `None` if not found.
    fn get_goal(&self, id: Uuid) -> anyhow::Result<Option<GoalRun>>;

    /// Transition a goal run to a new state. Returns the updated record.
    fn transition_goal(&self, id: Uuid, new_state: GoalRunState) -> anyhow::Result<GoalRun>;

    /// All goal runs whose `plan_phase` matches `phase_id`.
    fn list_goals_for_phase(&self, phase_id: &str) -> anyhow::Result<Vec<GoalRun>>;
}

/// The default, and currently sole, `PlanStore` implementation: PLAN.md on
/// disk (via this crate's `parse`/`history`/`query` modules) for phases,
/// `GoalRunStore`'s JSON files for goals. No behavior change relative to
/// what `apps/ta-cli` did directly before this phase — this is the same
/// logic, just behind the trait.
pub struct FilePlanStore {
    project_root: PathBuf,
    goal_store: GoalRunStore,
}

impl FilePlanStore {
    /// `goals_dir` is the directory `GoalRunStore` persists goal-run JSON
    /// files to (typically `<project_root>/.ta/goals`) — passed explicitly
    /// rather than derived, matching how `GoalRunStore::new` already works
    /// everywhere else in the codebase.
    pub fn new(
        project_root: impl AsRef<Path>,
        goals_dir: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            project_root: project_root.as_ref().to_path_buf(),
            goal_store: GoalRunStore::new(goals_dir)?,
        })
    }
}

impl PlanStore for FilePlanStore {
    fn backend_name(&self) -> &str {
        "file"
    }

    fn capabilities(&self) -> PlanStoreCapabilities {
        PlanStoreCapabilities {
            supports_native_phase_grouping: false,
            supports_webhooks: false,
            supports_idempotent_create: true,
        }
    }

    fn list_phases(&self) -> anyhow::Result<Vec<PlanPhase>> {
        parse::load_plan(&self.project_root)
    }

    fn get_phase(&self, id: &str) -> anyhow::Result<Option<PlanPhase>> {
        let phases = self.list_phases()?;
        Ok(phases
            .into_iter()
            .find(|p| parse::phase_ids_match(&p.id, id)))
    }

    fn update_phase_status(
        &self,
        id: &str,
        new_status: PlanStatus,
        note: Option<&str>,
    ) -> anyhow::Result<()> {
        match new_status {
            PlanStatus::InProgress => history::mark_phase_in_source(&self.project_root, id),
            PlanStatus::Pending => {
                history::reset_phase_if_in_progress(&self.project_root, id, note.unwrap_or(""))
                    .map(|_| ())
            }
            PlanStatus::Done | PlanStatus::Deferred => {
                // Not currently reachable through a dedicated ta-plan helper
                // (the CLI's `mark_done_batch`/apply-pipeline paths write
                // status transitions to `Done` directly via
                // `update_phase_status_with_schema` today, outside this
                // trait). Left as an explicit, honest error rather than a
                // silent no-op, so a future caller relying on this path
                // finds out immediately rather than assuming it worked.
                anyhow::bail!(
                    "FilePlanStore::update_phase_status: transition to {} is not yet \
                     implemented via PlanStore (id: {})",
                    new_status,
                    id
                )
            }
        }
    }

    fn next_ready_phases(&self) -> anyhow::Result<Vec<PlanPhase>> {
        let phases = self.list_phases()?;
        Ok(query::next_actionable_phases(&phases)
            .into_iter()
            .cloned()
            .collect())
    }

    fn save_goal(&self, goal: &GoalRun) -> anyhow::Result<()> {
        self.goal_store.save(goal).map_err(anyhow::Error::from)
    }

    fn get_goal(&self, id: Uuid) -> anyhow::Result<Option<GoalRun>> {
        self.goal_store.get(id).map_err(anyhow::Error::from)
    }

    fn transition_goal(&self, id: Uuid, new_state: GoalRunState) -> anyhow::Result<GoalRun> {
        self.goal_store
            .transition(id, new_state)
            .map_err(anyhow::Error::from)
    }

    fn list_goals_for_phase(&self, phase_id: &str) -> anyhow::Result<Vec<GoalRun>> {
        let all = self.goal_store.list().map_err(anyhow::Error::from)?;
        Ok(all
            .into_iter()
            .filter(|g| g.plan_phase.as_deref() == Some(phase_id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, FilePlanStore) {
        let dir = TempDir::new().unwrap();
        let goals_dir = dir.path().join(".ta/goals");
        std::fs::create_dir_all(&goals_dir).unwrap();
        std::fs::write(
            dir.path().join("PLAN.md"),
            "### v0.1.0 — First phase\n\
             <!-- status: done -->\n\n\
             ### v0.2.0 — Second phase\n\
             <!-- status: pending -->\n\
             **Depends on**: v0.1.0\n",
        )
        .unwrap();
        let store = FilePlanStore::new(dir.path(), &goals_dir).unwrap();
        (dir, store)
    }

    #[test]
    fn file_plan_store_lists_and_gets_phases() {
        let (_dir, store) = setup();
        let phases = store.list_phases().unwrap();
        assert_eq!(phases.len(), 2);
        assert_eq!(
            store.get_phase("v0.1.0").unwrap().unwrap().status,
            PlanStatus::Done
        );
        assert!(store.get_phase("v9.9.9").unwrap().is_none());
    }

    #[test]
    fn file_plan_store_next_ready_phases_respects_dependencies() {
        let (_dir, store) = setup();
        let ready = store.next_ready_phases().unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "v0.2.0");
    }

    #[test]
    fn file_plan_store_update_phase_status_in_progress_then_pending_round_trips() {
        let (dir, store) = setup();
        store
            .update_phase_status("v0.2.0", PlanStatus::InProgress, None)
            .unwrap();
        assert_eq!(
            store.get_phase("v0.2.0").unwrap().unwrap().status,
            PlanStatus::InProgress
        );

        store
            .update_phase_status("v0.2.0", PlanStatus::Pending, Some("test reset"))
            .unwrap();
        assert_eq!(
            store.get_phase("v0.2.0").unwrap().unwrap().status,
            PlanStatus::Pending
        );

        // Reset must be logged with the provided note.
        let history = crate::history::load_history(dir.path()).unwrap();
        assert!(history
            .iter()
            .any(|e| e.get("note").and_then(|n| n.as_str()) == Some("test reset")));
    }

    #[test]
    fn file_plan_store_goal_layer_round_trips() {
        let (dir, store) = setup();
        let mut goal = GoalRun::new(
            "test goal",
            "test objective",
            "claude-code",
            dir.path().join("workspace"),
            dir.path().join(".ta/goals"),
        );
        goal.plan_phase = Some("v0.2.0".to_string());
        let goal_id = goal.goal_run_id;
        store.save_goal(&goal).unwrap();

        let fetched = store.get_goal(goal_id).unwrap().unwrap();
        assert_eq!(fetched.title, "test goal");

        let for_phase = store.list_goals_for_phase("v0.2.0").unwrap();
        assert_eq!(for_phase.len(), 1);
        assert_eq!(for_phase[0].goal_run_id, goal_id);

        let transitioned = store
            .transition_goal(goal_id, GoalRunState::Configured)
            .unwrap();
        assert_eq!(transitioned.state, GoalRunState::Configured);
    }

    #[test]
    fn file_plan_store_capabilities_are_honest_about_file_backend_limits() {
        let (_dir, store) = setup();
        let caps = store.capabilities();
        assert_eq!(store.backend_name(), "file");
        assert!(!caps.supports_native_phase_grouping);
        assert!(!caps.supports_webhooks);
        assert!(caps.supports_idempotent_create);
    }
}
