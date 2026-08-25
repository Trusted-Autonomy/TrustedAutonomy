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

use sha2::{Digest, Sha256};
use ta_goal::{GoalRun, GoalRunState, GoalRunStore};
use uuid::Uuid;

use crate::history;
use crate::parse;
use crate::query;
use crate::schema::{PlanPhase, PlanSchema, PlanStatus};

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
    /// Whether `poll_changes` reports a true item-level delta (e.g. a
    /// future `WayfinderPlanStore`, backed by `updated_at`-filtered
    /// queries) or only "something changed, re-fetch everything" (`false`
    /// — `FilePlanStore`'s honest answer, since flat-file PLAN.md has no
    /// per-phase change timestamp to diff against).
    pub supports_granular_changes: bool,
}

/// Opaque position marker for [`PlanStore::poll_changes`]. Callers must
/// treat this as backend-specific and opaque — persist it verbatim between
/// polls, never construct or parse one by hand. `FilePlanStore` uses a
/// content hash; a future `WayfinderPlanStore` would use an `updated_at`
/// watermark against the delta-sync endpoint from the TA↔Wayfinder design
/// doc's §7.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeCursor(pub(crate) String);

impl ChangeCursor {
    /// A cursor representing "nothing seen yet" — the first `poll_changes`
    /// call with this cursor reports every phase/goal as changed.
    pub fn initial() -> Self {
        Self::default()
    }

    /// Build a cursor from a backend-computed digest — `pub(crate)` since
    /// only `PlanStore` implementations inside this crate construct real
    /// (non-initial) cursors; external callers only ever receive one from
    /// `poll_changes` and pass it back verbatim.
    pub(crate) fn from_digest(digest: String) -> Self {
        Self(digest)
    }

    pub(crate) fn digest(&self) -> &str {
        &self.0
    }
}

/// What changed since a given [`ChangeCursor`], plus the cursor to persist
/// for the next poll.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    pub changed_phase_ids: Vec<String>,
    pub changed_goal_ids: Vec<Uuid>,
    pub next_cursor: ChangeCursor,
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

    // ── Change awareness ─────────────────────────────────────────────

    /// What's changed since `since`. Required because a future
    /// Wayfinder-backed implementation has no webhook/push mechanism (see
    /// the TA↔Wayfinder design doc §1.4) and must poll — but every backend
    /// implements this, including `FilePlanStore`, so callers never need to
    /// special-case "this backend can't tell me what changed." Check
    /// `capabilities().supports_granular_changes` to know whether the
    /// result is a true delta or a "re-fetch everything" signal.
    fn poll_changes(&self, since: &ChangeCursor) -> anyhow::Result<ChangeSet>;
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

    /// Shared implementation for `Done`/`Deferred` transitions — mirrors
    /// `apps/ta-cli`'s `mark_done_batch` (read → transform → write →
    /// record) rather than duplicating that logic, per the project's own
    /// "reuse before reinventing" convention.
    fn write_terminal_status(
        &self,
        id: &str,
        new_status: PlanStatus,
        note: Option<&str>,
    ) -> anyhow::Result<()> {
        let schema = PlanSchema::load_or_default(&self.project_root);
        let plan_path = self.project_root.join(&schema.source);
        let content = std::fs::read_to_string(&plan_path)?;

        let old_status = parse::parse_plan_with_schema(&content, &schema)
            .into_iter()
            .find(|p| parse::phase_ids_match(&p.id, id))
            .map(|p| p.status)
            .ok_or_else(|| anyhow::anyhow!("FilePlanStore: phase {} not found", id))?;

        let updated =
            parse::update_phase_status_with_schema(&content, id, new_status.clone(), &schema);
        std::fs::write(&plan_path, &updated)?;

        // Best-effort — a failed history write must not roll back the
        // status change that already landed on disk, matching
        // `mark_done_batch`'s own `let _ = record_history(...)` treatment.
        let _ = history::record_history(&self.project_root, id, &old_status, &new_status);
        if note.is_some() {
            tracing::debug!(
                phase = id,
                "FilePlanStore::update_phase_status: note is ignored for Done/Deferred \
                 transitions — record_history's note field is reserved for reset/deny reasons, \
                 matching mark_done_batch's own behavior"
            );
        }
        Ok(())
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
            supports_granular_changes: false,
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
                self.write_terminal_status(id, new_status, note)
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

    /// Honest, cheap, non-granular: hashes PLAN.md content plus every
    /// goal's `(id, updated_at)` pair into one digest. If the digest
    /// matches `since`, nothing changed. If it doesn't, every current
    /// phase and goal id is reported as changed — `FilePlanStore` has no
    /// per-item timestamp to diff against (unlike a future
    /// `WayfinderPlanStore` polling `updated_since`), so "something
    /// changed" can only mean "assume everything did." Cheap either way:
    /// one file read plus one directory listing, no parsing beyond what
    /// `list_phases`/`list_goals_for_phase`-equivalent calls already do.
    fn poll_changes(&self, since: &ChangeCursor) -> anyhow::Result<ChangeSet> {
        let phases = self.list_phases()?;
        let goals = self.goal_store.list().map_err(anyhow::Error::from)?;

        let mut hasher = Sha256::new();
        for phase in &phases {
            hasher.update(phase.id.as_bytes());
            hasher.update(phase.status.to_string().as_bytes());
        }
        let mut goal_ids: Vec<Uuid> = goals.iter().map(|g| g.goal_run_id).collect();
        goal_ids.sort();
        for id in &goal_ids {
            hasher.update(id.as_bytes());
        }
        for goal in &goals {
            hasher.update(goal.updated_at.to_rfc3339().as_bytes());
        }
        let digest = format!("{:x}", hasher.finalize());
        let next_cursor = ChangeCursor(digest.clone());

        if since.0 == digest {
            return Ok(ChangeSet {
                changed_phase_ids: Vec::new(),
                changed_goal_ids: Vec::new(),
                next_cursor,
            });
        }

        Ok(ChangeSet {
            changed_phase_ids: phases.into_iter().map(|p| p.id).collect(),
            changed_goal_ids: goal_ids,
            next_cursor,
        })
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
        assert!(!caps.supports_granular_changes);
    }

    #[test]
    fn file_plan_store_poll_changes_from_initial_cursor_reports_everything() {
        let (_dir, store) = setup();
        let changes = store.poll_changes(&ChangeCursor::initial()).unwrap();
        assert_eq!(changes.changed_phase_ids.len(), 2);
    }

    #[test]
    fn file_plan_store_poll_changes_is_empty_when_nothing_changed() {
        let (_dir, store) = setup();
        let first = store.poll_changes(&ChangeCursor::initial()).unwrap();
        let second = store.poll_changes(&first.next_cursor).unwrap();
        assert!(second.changed_phase_ids.is_empty());
        assert!(second.changed_goal_ids.is_empty());
        assert_eq!(second.next_cursor, first.next_cursor);
    }

    #[test]
    fn file_plan_store_poll_changes_detects_a_status_transition() {
        let (_dir, store) = setup();
        let baseline = store.poll_changes(&ChangeCursor::initial()).unwrap();
        store
            .update_phase_status("v0.2.0", PlanStatus::InProgress, None)
            .unwrap();
        let after = store.poll_changes(&baseline.next_cursor).unwrap();
        assert!(after.changed_phase_ids.contains(&"v0.2.0".to_string()));
        assert_ne!(after.next_cursor, baseline.next_cursor);
    }

    #[test]
    fn file_plan_store_poll_changes_detects_a_new_goal() {
        let (dir, store) = setup();
        let baseline = store.poll_changes(&ChangeCursor::initial()).unwrap();

        let goal = GoalRun::new(
            "new goal",
            "objective",
            "claude-code",
            dir.path().join("workspace"),
            dir.path().join(".ta/goals"),
        );
        let goal_id = goal.goal_run_id;
        store.save_goal(&goal).unwrap();

        let after = store.poll_changes(&baseline.next_cursor).unwrap();
        assert!(after.changed_goal_ids.contains(&goal_id));
    }

    #[test]
    fn file_plan_store_marks_a_phase_done() {
        let (_dir, store) = setup();
        store
            .update_phase_status("v0.2.0", PlanStatus::Done, None)
            .unwrap();
        assert_eq!(
            store.get_phase("v0.2.0").unwrap().unwrap().status,
            PlanStatus::Done
        );
    }

    #[test]
    fn file_plan_store_marks_a_phase_deferred() {
        let (_dir, store) = setup();
        store
            .update_phase_status("v0.2.0", PlanStatus::Deferred, None)
            .unwrap();
        assert_eq!(
            store.get_phase("v0.2.0").unwrap().unwrap().status,
            PlanStatus::Deferred
        );
    }

    #[test]
    fn file_plan_store_done_transition_is_recorded_in_history() {
        let (dir, store) = setup();
        store
            .update_phase_status("v0.2.0", PlanStatus::Done, None)
            .unwrap();
        let history = crate::history::load_history(dir.path()).unwrap();
        assert!(history
            .iter()
            .any(
                |e| e.get("phase_id").and_then(|p| p.as_str()) == Some("v0.2.0")
                    && e.get("new_status").and_then(|s| s.as_str()) == Some("done")
            ));
    }

    #[test]
    fn file_plan_store_done_transition_fails_for_unknown_phase() {
        let (_dir, store) = setup();
        assert!(store
            .update_phase_status("v9.9.9", PlanStatus::Done, None)
            .is_err());
    }
}
