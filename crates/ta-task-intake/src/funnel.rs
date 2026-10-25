//! Funnel draft detection.
//!
//! A draft that the intake ledger references (a request in state `drafted` or
//! `applied` carrying that `draft_id`) is a *funnel draft*: it was produced by
//! the project manager from queued task requests. Such a draft counts as
//! origin `project-manager` for the constitution evaluator even when its goal
//! record carries no origin, unless the goal origin is `cos` or `chat` (those
//! block auto-approve outright and keep winning, H9).
//!
//! The returned views are sanitised but still UNTRUSTED data: fence them
//! (see [`crate::fence`]) before a model or human reads them.

use chrono::{DateTime, Utc};

use crate::model::{RequestState, RequestView};
use crate::settings::IntakeError;
use crate::store::{IntakeStore, ListFilter};

/// The requests in `project_id`'s ledger that went into `draft_id`, in state
/// `drafted` or `applied`, oldest first.
pub fn funnel_requests_for_draft(
    store: &impl IntakeStore,
    project_id: &str,
    draft_id: &str,
    now: DateTime<Utc>,
) -> Result<Vec<RequestView>, IntakeError> {
    let mut out = Vec::new();
    for state in [RequestState::Drafted, RequestState::Applied] {
        let report = store.list(
            &ListFilter {
                project_id: Some(project_id.to_string()),
                state: Some(state),
                goal_id: None,
            },
            now,
        )?;
        out.extend(
            report
                .requests
                .into_iter()
                .filter(|r| r.draft_id.as_deref() == Some(draft_id)),
        );
    }
    out.sort_by_key(|r| r.created_at);
    Ok(out)
}

/// Whether the ledger references `draft_id` (see [`funnel_requests_for_draft`]).
pub fn is_funnel_draft(
    store: &impl IntakeStore,
    project_id: &str,
    draft_id: &str,
    now: DateTime<Utc>,
) -> Result<bool, IntakeError> {
    Ok(!funnel_requests_for_draft(store, project_id, draft_id, now)?.is_empty())
}
