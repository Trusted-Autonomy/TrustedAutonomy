// phase_release.rs: the one place a goal's plan-phase claim is released
// (v0.17.11.30).
//
// A goal that links a plan phase holds a claim on it: PLAN.md says
// `in_progress` and the daemon keeps an in-memory claim. Every way a goal can
// stop holding the phase used to be released by its own caller (`ta draft
// deny`, `ta draft close`, `ta goal delete`, ...), so a path nobody wired up
// (a chat goal completing with no draft, a failure, a process death seen by the
// watchdog) left the phase `in_progress` forever and the next launch that
// linked it failed with "already in progress (unknown goal)".
//
// Now `GoalRunStore` is the enforcement point: every transition INTO a state
// that ends the claim (and every deletion of a goal that still holds it)
// calls the registered releaser, whichever caller made it. ta-goal cannot
// reach PLAN.md editing (ta-plan depends on it) or the daemon, so the process
// registers the releaser: `ta` registers one that resets PLAN.md and tells the
// daemon over HTTP, `ta-daemon` one that clears its in-memory registry.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use uuid::Uuid;

use crate::goal_run::{GoalRun, GoalRunState};

/// What made the goal stop holding its phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseTrigger {
    /// The goal moved into this state (its display name, e.g. `completed`).
    State(String),
    /// The goal record was deleted while it still held the phase.
    Deleted,
}

impl std::fmt::Display for ReleaseTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReleaseTrigger::State(s) => write!(f, "goal {s}"),
            ReleaseTrigger::Deleted => write!(f, "goal deleted"),
        }
    }
}

/// One claim to release.
#[derive(Debug, Clone)]
pub struct PhaseRelease {
    pub goal_id: Uuid,
    pub phase_id: String,
    /// The project the goal was started from (where PLAN.md lives).
    pub source_dir: Option<PathBuf>,
    pub trigger: ReleaseTrigger,
    /// Whether PLAN.md should go back from `in_progress` to `pending`. False
    /// when the goal's work landed (applied, merged, closed as applied
    /// externally, completed with a draft): the phase is then finished by
    /// `ta draft apply --phase`, not reset. The in-memory claim is released
    /// either way.
    pub reset_plan: bool,
}

/// Receives claims to release. Must be idempotent: the same claim can arrive
/// from more than one process.
pub trait PhaseClaimReleaser: Send + Sync {
    fn release(&self, release: &PhaseRelease);
}

static RELEASER: RwLock<Option<Arc<dyn PhaseClaimReleaser>>> = RwLock::new(None);

thread_local! {
    static THREAD_RELEASER: RefCell<Option<Arc<dyn PhaseClaimReleaser>>> =
        const { RefCell::new(None) };
}

/// Register the process-wide releaser (replaces any earlier one).
pub fn register_releaser(releaser: Arc<dyn PhaseClaimReleaser>) {
    *RELEASER.write().unwrap_or_else(|e| e.into_inner()) = Some(releaser);
}

/// Scoped releaser for the current thread only, taking precedence over the
/// process-wide one. For tests, so parallel tests do not see each other's.
pub struct ThreadReleaserGuard {
    previous: Option<Arc<dyn PhaseClaimReleaser>>,
}

impl ThreadReleaserGuard {
    pub fn set(releaser: Arc<dyn PhaseClaimReleaser>) -> Self {
        let previous = THREAD_RELEASER.with(|r| r.borrow_mut().replace(releaser));
        Self { previous }
    }
}

impl Drop for ThreadReleaserGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        THREAD_RELEASER.with(|r| *r.borrow_mut() = previous);
    }
}

fn current_releaser() -> Option<Arc<dyn PhaseClaimReleaser>> {
    if let Some(r) = THREAD_RELEASER.with(|r| r.borrow().clone()) {
        return Some(r);
    }
    RELEASER.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Whether a goal in `state` no longer holds its plan phase.
pub fn state_ends_claim(state: &GoalRunState) -> bool {
    matches!(
        state,
        GoalRunState::Applied
            | GoalRunState::Closed { .. }
            | GoalRunState::Merged
            | GoalRunState::Completed
            | GoalRunState::Failed { .. }
    )
}

/// Whether ending the claim should also reset PLAN.md to `pending`: true when
/// no work landed (failed, abandoned, completed without a draft).
pub fn plan_reset_warranted(goal: &GoalRun) -> bool {
    match &goal.state {
        GoalRunState::Failed { .. } => true,
        GoalRunState::Closed {
            applied_externally_ref,
            ..
        } => applied_externally_ref.is_none(),
        GoalRunState::Completed => goal.pr_package_id.is_none(),
        _ => false,
    }
}

/// Hand `goal`'s claim to the registered releaser, if it holds one.
/// `reset_plan` is decided by the caller (see [`plan_reset_warranted`]).
pub(crate) fn release(goal: &GoalRun, trigger: ReleaseTrigger, reset_plan: bool) {
    let Some(phase_id) = goal.plan_phase.clone() else {
        return;
    };
    let Some(releaser) = current_releaser() else {
        return;
    };
    tracing::info!(
        goal_id = %goal.goal_run_id,
        phase = %phase_id,
        trigger = %trigger,
        reset_plan,
        "releasing the plan-phase claim held by a goal that no longer holds it"
    );
    releaser.release(&PhaseRelease {
        goal_id: goal.goal_run_id,
        phase_id,
        source_dir: goal.source_dir.clone(),
        trigger,
        reset_plan,
    });
}
