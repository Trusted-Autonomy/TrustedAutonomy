//! `InMemoryPlanStore` — a second, minimal `PlanStore` implementation
//! (v0.17.11.1.2 item 3), proving the trait boundary is real rather than
//! `FilePlanStore`-shaped in disguise. Also directly useful: any test
//! elsewhere in the workspace that needs a `dyn PlanStore` without touching
//! disk or `GoalRunStore`'s JSON files can use this instead of a
//! `tempfile::TempDir` + `FilePlanStore`.

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use ta_goal::{GoalRun, GoalRunState};
use uuid::Uuid;

use crate::query;
use crate::schema::PlanStatus;
use crate::store::{ChangeCursor, ChangeSet, PlanStore, PlanStoreCapabilities};
use crate::PlanPhase;

#[derive(Default)]
struct Inner {
    phases: HashMap<String, PlanPhase>,
    goals: HashMap<Uuid, GoalRun>,
}

/// In-process, in-memory `PlanStore`. Not persisted, not shared across
/// processes — for tests and any in-process-only use case, not a real
/// alternative deployment backend the way a future `WayfinderPlanStore`
/// would be.
#[derive(Default)]
pub struct InMemoryPlanStore {
    inner: Mutex<Inner>,
}

impl InMemoryPlanStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a phase directly — the in-memory equivalent of writing a
    /// `### id — title` block to PLAN.md, without needing a real file.
    pub fn insert_phase(&self, phase: PlanPhase) {
        self.inner
            .lock()
            .unwrap()
            .phases
            .insert(phase.id.clone(), phase);
    }
}

impl PlanStore for InMemoryPlanStore {
    fn backend_name(&self) -> &str {
        "memory"
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
        let inner = self.inner.lock().unwrap();
        let mut phases: Vec<PlanPhase> = inner.phases.values().cloned().collect();
        phases.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(phases)
    }

    fn get_phase(&self, id: &str) -> anyhow::Result<Option<PlanPhase>> {
        Ok(self.inner.lock().unwrap().phases.get(id).cloned())
    }

    fn update_phase_status(
        &self,
        id: &str,
        new_status: PlanStatus,
        _note: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let phase = inner
            .phases
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("InMemoryPlanStore: phase {} not found", id))?;
        phase.status = new_status;
        Ok(())
    }

    fn next_ready_phases(&self) -> anyhow::Result<Vec<PlanPhase>> {
        let phases = self.list_phases()?;
        Ok(query::next_actionable_phases(&phases)
            .into_iter()
            .cloned()
            .collect())
    }

    fn save_goal(&self, goal: &GoalRun) -> anyhow::Result<()> {
        self.inner
            .lock()
            .unwrap()
            .goals
            .insert(goal.goal_run_id, goal.clone());
        Ok(())
    }

    fn get_goal(&self, id: Uuid) -> anyhow::Result<Option<GoalRun>> {
        Ok(self.inner.lock().unwrap().goals.get(&id).cloned())
    }

    fn transition_goal(&self, id: Uuid, new_state: GoalRunState) -> anyhow::Result<GoalRun> {
        let mut inner = self.inner.lock().unwrap();
        let goal = inner
            .goals
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("InMemoryPlanStore: goal {} not found", id))?;
        goal.transition(new_state)?;
        Ok(goal.clone())
    }

    fn list_goals_for_phase(&self, phase_id: &str) -> anyhow::Result<Vec<GoalRun>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .goals
            .values()
            .filter(|g| g.plan_phase.as_deref() == Some(phase_id))
            .cloned()
            .collect())
    }

    fn poll_changes(&self, since: &ChangeCursor) -> anyhow::Result<ChangeSet> {
        let inner = self.inner.lock().unwrap();
        let mut hasher = Sha256::new();
        let mut phase_ids: Vec<&String> = inner.phases.keys().collect();
        phase_ids.sort();
        for id in &phase_ids {
            hasher.update(id.as_bytes());
            hasher.update(inner.phases[*id].status.to_string().as_bytes());
        }
        let mut goal_ids: Vec<Uuid> = inner.goals.keys().copied().collect();
        goal_ids.sort();
        for id in &goal_ids {
            hasher.update(id.as_bytes());
        }
        let digest = format!("{:x}", hasher.finalize());
        let next_cursor = ChangeCursor::from_digest(digest.clone());

        if since.digest() == digest {
            return Ok(ChangeSet {
                changed_phase_ids: Vec::new(),
                changed_goal_ids: Vec::new(),
                next_cursor,
            });
        }

        Ok(ChangeSet {
            changed_phase_ids: phase_ids.into_iter().cloned().collect(),
            changed_goal_ids: goal_ids,
            next_cursor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(id: &str, status: PlanStatus, depends_on: Vec<&str>) -> PlanPhase {
        PlanPhase {
            id: id.to_string(),
            title: format!("Phase {id}"),
            status,
            depends_on: depends_on.into_iter().map(|s| s.to_string()).collect(),
            human_review_items: Vec::new(),
            api_impact: Vec::new(),
        }
    }

    #[test]
    fn lists_and_gets_phases() {
        let store = InMemoryPlanStore::new();
        store.insert_phase(phase("v0.1.0", PlanStatus::Done, vec![]));
        assert_eq!(store.list_phases().unwrap().len(), 1);
        assert_eq!(
            store.get_phase("v0.1.0").unwrap().unwrap().status,
            PlanStatus::Done
        );
        assert!(store.get_phase("v9.9.9").unwrap().is_none());
    }

    #[test]
    fn next_ready_phases_respects_dependencies() {
        let store = InMemoryPlanStore::new();
        store.insert_phase(phase("v0.1.0", PlanStatus::Done, vec![]));
        store.insert_phase(phase("v0.2.0", PlanStatus::Pending, vec!["v0.1.0"]));
        let ready = store.next_ready_phases().unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "v0.2.0");
    }

    #[test]
    fn update_phase_status_round_trips() {
        let store = InMemoryPlanStore::new();
        store.insert_phase(phase("v0.1.0", PlanStatus::Pending, vec![]));
        store
            .update_phase_status("v0.1.0", PlanStatus::Done, None)
            .unwrap();
        assert_eq!(
            store.get_phase("v0.1.0").unwrap().unwrap().status,
            PlanStatus::Done
        );
    }

    #[test]
    fn update_phase_status_fails_for_unknown_phase() {
        let store = InMemoryPlanStore::new();
        assert!(store
            .update_phase_status("v9.9.9", PlanStatus::Done, None)
            .is_err());
    }

    #[test]
    fn goal_layer_round_trips() {
        let store = InMemoryPlanStore::new();
        let dir = tempfile::tempdir().unwrap();
        let mut goal = GoalRun::new(
            "test goal",
            "objective",
            "claude-code",
            dir.path().join("workspace"),
            dir.path().join(".ta/goals"),
        );
        goal.plan_phase = Some("v0.1.0".to_string());
        let goal_id = goal.goal_run_id;
        store.save_goal(&goal).unwrap();

        assert_eq!(store.get_goal(goal_id).unwrap().unwrap().title, "test goal");
        assert_eq!(store.list_goals_for_phase("v0.1.0").unwrap().len(), 1);

        let transitioned = store
            .transition_goal(goal_id, GoalRunState::Configured)
            .unwrap();
        assert_eq!(transitioned.state, GoalRunState::Configured);
    }

    #[test]
    fn poll_changes_detects_a_transition() {
        let store = InMemoryPlanStore::new();
        store.insert_phase(phase("v0.1.0", PlanStatus::Pending, vec![]));
        let baseline = store.poll_changes(&ChangeCursor::initial()).unwrap();
        store
            .update_phase_status("v0.1.0", PlanStatus::Done, None)
            .unwrap();
        let after = store.poll_changes(&baseline.next_cursor).unwrap();
        assert!(after.changed_phase_ids.contains(&"v0.1.0".to_string()));
    }

    /// The actual point of this file: prove the trait boundary is real by
    /// driving `InMemoryPlanStore` through `dyn PlanStore` — seeded via its
    /// own concrete `insert_phase` (test scaffolding, not part of the
    /// trait every backend must support) before being erased to a trait
    /// object, then exercised purely through the trait interface from
    /// there on.
    #[test]
    fn is_polymorphic_via_dyn_plan_store() {
        let concrete = InMemoryPlanStore::new();
        concrete.insert_phase(phase("v0.1.0", PlanStatus::Pending, vec![]));

        let store: Box<dyn PlanStore> = Box::new(concrete);
        assert_eq!(store.backend_name(), "memory");
        assert_eq!(store.list_phases().unwrap().len(), 1);
        store
            .update_phase_status("v0.1.0", PlanStatus::Done, None)
            .unwrap();
        assert_eq!(
            store.get_phase("v0.1.0").unwrap().unwrap().status,
            PlanStatus::Done
        );
    }
}
