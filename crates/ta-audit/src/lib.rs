//! # ta-audit
//!
//! Append-only event log and artifact hashing for Trusted Autonomy.
//!
//! Every tool call, policy decision, approval, and apply action in the system
//! is recorded as an [`AuditEvent`] in a JSONL (JSON Lines) log file.
//! Each event includes SHA-256 hashes of its inputs and outputs for
//! tamper detection and replay verification.
//!
//! ## Quick Example
//!
//! ```rust,no_run
//! use ta_audit::{AuditLog, AuditEvent, AuditAction};
//!
//! let mut log = AuditLog::open("/tmp/audit.jsonl").unwrap();
//! let mut event = AuditEvent::new("agent-1", AuditAction::ToolCall)
//!     .with_target("fs://workspace/src/main.rs");
//! log.append(&mut event).unwrap();
//! ```

// Module declarations — each `mod foo;` tells Rust to look for `foo.rs`
// in the same directory and include it as a submodule.
pub mod attestation;
pub mod drift;
pub mod error;
pub mod event;
pub mod hasher;
pub mod ledger;
pub mod log;

// Re-export the main types at the crate root for convenience.
// Users can write `use ta_audit::AuditLog` instead of `use ta_audit::log::AuditLog`.
pub use attestation::{
    AttestationBackend, AttestationError, AttestationRecord, SoftwareAttestationBackend,
};
pub use drift::{
    constitution_violation_finding, BaselineStore, BehavioralBaseline, DraftSummary, DriftFinding,
    DriftReport, DriftSeverity, DriftSignal,
};
pub use error::AuditError;
pub use event::{Alternative, AuditAction, AuditEvent, DecisionReasoning};
pub use ledger::{
    migrate_from_history, ArtifactRecord, AuditDisposition, AuditEntry, GoalAuditLedger,
    LedgerFilter,
};
pub use log::AuditLog;
