// mapping.rs — how TA's `PlanPhase`/`GoalRun` concepts map onto Wayfinder
// tasks, per the design doc's §2 risk note and §7.2's field-ownership rule.
//
// Both TA concepts become Wayfinder *tasks*, distinguished by an
// `external_id` prefix:
//   - `ta-phase-gate:<phase_id>`  — a synthetic, non-work "gate" task, one
//     per PLAN.md phase, dependency-chained phase-to-phase so Wayfinder's
//     `task_dependency`-gated readiness fakes the ordering PLAN.md
//     expresses natively (Wayfinder's `Goal` has no grouping/ordering layer
//     above it — see the design doc's §1 grounding constraint #6).
//   - `ta-goal:<goal_run_id>`     — a real Wayfinder task mirroring one
//     TA `GoalRun` (an actual agent execution attempt).
//
// Status mapping is deliberately lossy in one direction only: TA's richer
// `GoalRunState` collapses onto Wayfinder's flat `open|in_progress|done|
// cancelled|on_hold`, with the state name preserved in `hold_reason` for
// states that don't fit (`AwaitingInput`, `Failed`, `Custom`) so a human
// looking at the Wayfinder UI can still tell *why* something is stuck —
// per the design doc's explicit instruction not to drop this to ad hoc
// text. TA never reconstructs a `GoalRunState` from a Wayfinder status —
// that direction genuinely isn't reversible, and per §7.2's field-ownership
// rule, TA's own local `GoalRun` is authoritative for this field regardless
// of what Wayfinder shows.

use uuid::Uuid;

use ta_goal::GoalRunState;
use ta_plan::PlanStatus;

use crate::client::{
    STATUS_CANCELLED, STATUS_DONE, STATUS_IN_PROGRESS, STATUS_ON_HOLD, STATUS_OPEN,
};

const PHASE_GATE_PREFIX: &str = "ta-phase-gate:";
const GOAL_PREFIX: &str = "ta-goal:";

pub fn phase_gate_external_id(phase_id: &str) -> String {
    format!("{PHASE_GATE_PREFIX}{phase_id}")
}

pub fn goal_external_id(goal_id: Uuid) -> String {
    format!("{GOAL_PREFIX}{goal_id}")
}

/// Recovers the original PLAN.md phase id from a gate task's
/// `external_id`, if `external_id` is one.
pub fn phase_id_from_external_id(external_id: &str) -> Option<&str> {
    external_id.strip_prefix(PHASE_GATE_PREFIX)
}

/// Recovers the original `GoalRun` id from a goal task's `external_id`, if
/// `external_id` is one and parses as a UUID.
pub fn goal_id_from_external_id(external_id: &str) -> Option<Uuid> {
    external_id
        .strip_prefix(GOAL_PREFIX)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// `PlanStatus` -> Wayfinder `(status, hold_reason)` for a gate task.
/// `Deferred` maps to `cancelled` — "not doing this now" is closer to
/// cancelled than to a resumable on-hold pause, and matches TA's own
/// `next_ready_phases` treatment of `Deferred` as not actionable, the same
/// way Wayfinder's `ready_queue` excludes `cancelled` tasks.
pub fn plan_status_to_wayfinder(status: &PlanStatus) -> (&'static str, Option<&'static str>) {
    match status {
        PlanStatus::Pending => (STATUS_OPEN, None),
        PlanStatus::InProgress => (STATUS_IN_PROGRESS, None),
        PlanStatus::Done => (STATUS_DONE, None),
        PlanStatus::Deferred => (STATUS_CANCELLED, None),
    }
}

/// Reverse of `plan_status_to_wayfinder`, for reading a gate task's status
/// back into TA's phase-tracking view (`list_phases`/`next_ready_phases`).
/// `on_hold` — a status TA itself never writes for a gate task — means a
/// human paused it directly in the Wayfinder UI; treated conservatively as
/// `InProgress` (not newly-`Pending`-and-ready, not `Done`) rather than
/// guessing at intent.
pub fn wayfinder_status_to_plan_status(status: &str) -> PlanStatus {
    match status {
        STATUS_OPEN => PlanStatus::Pending,
        STATUS_DONE => PlanStatus::Done,
        STATUS_CANCELLED => PlanStatus::Deferred,
        STATUS_IN_PROGRESS | STATUS_ON_HOLD => PlanStatus::InProgress,
        _ => PlanStatus::InProgress,
    }
}

/// `GoalRunState` -> Wayfinder `(status, hold_reason)` for a goal task.
/// TA-owned per §7.2 — TA's local value always wins on push; see the
/// module doc for why the reverse direction isn't attempted.
pub fn goal_state_to_wayfinder(state: &GoalRunState) -> (&'static str, Option<String>) {
    match state {
        GoalRunState::Created | GoalRunState::Configured => (STATUS_OPEN, None),
        GoalRunState::Running
        | GoalRunState::PrReady
        | GoalRunState::UnderReview
        | GoalRunState::Approved { .. }
        | GoalRunState::Applied
        | GoalRunState::Finalizing { .. }
        | GoalRunState::DraftPending { .. } => (STATUS_IN_PROGRESS, None),
        GoalRunState::Merged | GoalRunState::Completed => (STATUS_DONE, None),
        GoalRunState::Closed {
            applied_externally_ref: Some(_),
            ..
        } => (STATUS_DONE, None),
        GoalRunState::Closed { .. } => (STATUS_CANCELLED, None),
        GoalRunState::AwaitingInput {
            question_preview, ..
        } => (
            STATUS_ON_HOLD,
            Some(format!("ta_awaiting_input: {question_preview}")),
        ),
        GoalRunState::Failed { reason } => (STATUS_ON_HOLD, Some(format!("ta_failed: {reason}"))),
        GoalRunState::Custom { tag } => (STATUS_ON_HOLD, Some(format!("ta_custom: {tag}"))),
        // `GoalRunState` is `#[non_exhaustive]` (future variants can be
        // added without a semver break) — an unrecognized future variant
        // is treated the same conservative way as an unrecognized
        // Wayfinder status in the reverse mapping: on_hold, not silently
        // dropped or misrepresented as done/open.
        other => (STATUS_ON_HOLD, Some(format!("ta_unmapped_state: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_gate_ids_round_trip() {
        let external_id = phase_gate_external_id("v0.17.11.3");
        assert_eq!(external_id, "ta-phase-gate:v0.17.11.3");
        assert_eq!(phase_id_from_external_id(&external_id), Some("v0.17.11.3"));
        assert_eq!(goal_id_from_external_id(&external_id), None);
    }

    #[test]
    fn goal_ids_round_trip() {
        let id = Uuid::new_v4();
        let external_id = goal_external_id(id);
        assert_eq!(goal_id_from_external_id(&external_id), Some(id));
        assert_eq!(phase_id_from_external_id(&external_id), None);
    }

    #[test]
    fn plan_status_mapping_is_stable_for_ordinary_statuses() {
        assert_eq!(
            plan_status_to_wayfinder(&PlanStatus::Pending),
            (STATUS_OPEN, None)
        );
        assert_eq!(
            plan_status_to_wayfinder(&PlanStatus::InProgress),
            (STATUS_IN_PROGRESS, None)
        );
        assert_eq!(
            plan_status_to_wayfinder(&PlanStatus::Done),
            (STATUS_DONE, None)
        );
        assert_eq!(
            plan_status_to_wayfinder(&PlanStatus::Deferred),
            (STATUS_CANCELLED, None)
        );
    }

    #[test]
    fn wayfinder_status_reverse_mapping_treats_on_hold_conservatively() {
        assert_eq!(
            wayfinder_status_to_plan_status(STATUS_OPEN),
            PlanStatus::Pending
        );
        assert_eq!(
            wayfinder_status_to_plan_status(STATUS_DONE),
            PlanStatus::Done
        );
        assert_eq!(
            wayfinder_status_to_plan_status(STATUS_CANCELLED),
            PlanStatus::Deferred
        );
        assert_eq!(
            wayfinder_status_to_plan_status(STATUS_ON_HOLD),
            PlanStatus::InProgress
        );
    }

    #[test]
    fn awaiting_input_state_preserves_the_question_in_hold_reason() {
        let (status, hold_reason) = goal_state_to_wayfinder(&GoalRunState::AwaitingInput {
            interaction_id: Uuid::new_v4(),
            question_preview: "approve this?".to_string(),
        });
        assert_eq!(status, STATUS_ON_HOLD);
        assert_eq!(
            hold_reason.as_deref(),
            Some("ta_awaiting_input: approve this?")
        );
    }

    #[test]
    fn custom_state_preserves_the_tag_in_hold_reason() {
        let (status, hold_reason) = goal_state_to_wayfinder(&GoalRunState::Custom {
            tag: "waiting_for_ci".to_string(),
        });
        assert_eq!(status, STATUS_ON_HOLD);
        assert_eq!(hold_reason.as_deref(), Some("ta_custom: waiting_for_ci"));
    }

    #[test]
    fn closed_with_external_ref_is_done_not_cancelled() {
        let (status, _) = goal_state_to_wayfinder(&GoalRunState::Closed {
            reason: None,
            applied_externally_ref: Some("gh:PR#123".to_string()),
        });
        assert_eq!(status, STATUS_DONE);
    }

    #[test]
    fn closed_without_external_ref_is_cancelled() {
        let (status, _) = goal_state_to_wayfinder(&GoalRunState::Closed {
            reason: Some("abandoned".to_string()),
            applied_externally_ref: None,
        });
        assert_eq!(status, STATUS_CANCELLED);
    }

    #[test]
    fn terminal_success_states_map_to_done() {
        assert_eq!(
            goal_state_to_wayfinder(&GoalRunState::Merged).0,
            STATUS_DONE
        );
        assert_eq!(
            goal_state_to_wayfinder(&GoalRunState::Completed).0,
            STATUS_DONE
        );
    }
}
