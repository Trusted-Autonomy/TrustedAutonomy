//! # ta-task-intake
//!
//! The store behind the task intake funnel: how a goal agent *asks* for a
//! Wayfinder task to be created or a KPI datapoint to be recorded without
//! being able to write either itself.
//!
//! A goal agent calls the `ta_request_task` MCP tool. The gateway validates
//! the request, derives the requester's goal, role and Wayfinder project from
//! its own state (never from tool arguments) and appends the request through
//! [`IntakeStore::enqueue`]. A later consumer (the virtual-team project
//! manager, outside this crate) drains the queue in batches with
//! [`IntakeStore::drain`] and reports outcomes with
//! [`IntakeStore::acknowledge`]. Nothing here executes a request.
//!
//! **Not to be confused with `ta-intake`**, the unrelated trigger layer that
//! turns external events into goals.
//!
//! * [`model`] — documented, serde-stable record, ledger and view types.
//! * [`store`] — the [`IntakeStore`] trait, [`FileIntakeStore`], and the full
//!   on-disk format and crash-recovery rules.
//! * [`settings`] — caps, timing defaults and [`IntakeError`].
//! * [`fence`] — [`sanitize`](fence::sanitize) and fenced human rendering of
//!   untrusted request text.
//!
//! Request text is UNTRUSTED. Everything that leaves the store towards a
//! reader is sanitised, and every renderer here quotes it through
//! `ta_goal::provenance::fence_untrusted`. Batch records returned by `drain`
//! are raw: a consumer MUST fence them before a model reads them.

pub mod fence;
mod fs;
pub mod funnel;
pub mod model;
pub mod settings;
pub mod store;

pub use funnel::{funnel_requests_for_draft, is_funnel_draft};
pub use model::{
    normalize_text, Duplicate, IntakeRecord, LedgerEvent, ListReport, Origin, RequestBody,
    RequestKind, RequestState, RequestView, Submission, Transition, MAX_BODY_BYTES, MAX_LINE_BYTES,
    SCHEMA_VERSION,
};
pub use settings::{
    CapKind, IntakeCaps, IntakeError, IntakeSettings, TaskIntakeConfig, DEFAULT_CLAIM_LEASE,
    DEFAULT_DRAIN_MAX, DEFAULT_MAX_CLAIM_ATTEMPTS, DEFAULT_PER_GOAL,
    DEFAULT_PER_PROJECT_OUTSTANDING, DEFAULT_PER_ROLE_PER_HOUR, DEFAULT_QUEUED_TTL,
};
pub use store::{
    Ack, AckRequest, Batch, DrainRequest, EnqueueOutcome, FileIntakeStore, IntakeStore, ListFilter,
    RecoveryReport,
};
