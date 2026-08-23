//! `ta-plan` — PLAN.md schema, parsing, and phase-tracking logic.
//!
//! Extracted from `apps/ta-cli/src/commands/plan.rs` (v0.17.11.1, Sub-project
//! 0 of the TA-Wayfinder plan integration design) as a pure, no-behavior-change
//! extraction: `apps/ta-cli/src/commands/plan.rs` re-exports these items and
//! calls into them rather than duplicating the logic, and
//! `crates/ta-daemon/src/api/plan.rs`'s previously-independent second parser
//! is resolved against this crate too.
//!
//! This is TA's "plan/goal storage" library layer — the same architectural
//! role `ta-goal` already plays for `GoalRun` state — sitting below
//! `ta-daemon`/`ta-mcp-gateway`/`ta-cli`, not a CLI-specific module. A future
//! `PlanStore` trait (v0.17.11.1 item 3) is built on top of these functions,
//! with `FilePlanStore` (item 4) as its sole implementation for now.

pub mod history;
pub mod parse;
pub mod query;
pub mod schema;
pub mod store;

pub use history::{
    insert_adhoc_phase, load_history, mark_phase_in_source, record_history,
    reset_phase_if_in_progress,
};
pub use parse::{
    load_plan, parse_plan, parse_plan_with_schema, phase_ids_match, update_phase_status,
    update_phase_status_with_schema,
};
pub use query::{
    binary_version, candidate_waves, check_phase_order, check_version_sync,
    collect_dependency_warnings, detect_missing_status_markers, find_in_progress,
    find_next_pending, find_phases_needing_done_marker, is_sub_phase, last_completed_phase_id,
    next_actionable_phase_id, next_actionable_phases, parent_phase_id, parse_semver_id,
    phase_id_to_semver, warn_unparseable_phase_id_for_bump,
};
pub use schema::{
    default_doc_search_dirs, default_statuses, PhasePattern, PlanPhase, PlanSchema, PlanStatus,
};
pub use store::{FilePlanStore, PlanStore, PlanStoreCapabilities};
