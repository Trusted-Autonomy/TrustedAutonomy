//! Smart Advisor — intent classification and conversational routing for
//! Studio's dashboard advisor dialog and per-draft Q&A dialogs (v0.17.0.12.6).
//!
//! This crate owns the 4-way intent taxonomy (`queue_goal` / `info_request` /
//! `draft_action` / `ambiguous`) on top of `ta_session`'s existing heuristic
//! classifier, plus the confirmation-card and clarification-round state
//! machines the Studio backend needs. It intentionally does not depend on
//! `ta-daemon` — "answer from daemon state" (item 16) stays in the daemon,
//! which already aggregates that state for the existing Advisor tab.

pub mod classify;

pub use classify::{
    build_confirmation_card, classify as classify_advisor_intent, next_clarify_step,
    AdvisorClassification, AdvisorIntent, ClarifyOutcome, ClarifyState, ConfirmationCard,
    DraftActionKind, AMBIGUOUS_CONFIDENCE_THRESHOLD, MAX_CLARIFY_ROUNDS,
};
