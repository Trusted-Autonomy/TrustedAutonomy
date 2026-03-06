//! # ta-policy
//!
//! Capability-based policy engine for Trusted Autonomy.
//!
//! Implements the "default deny" security boundary: agents can only perform
//! actions explicitly granted by a [`CapabilityManifest`]. The [`PolicyEngine`]
//! evaluates each tool call request against the agent's grants and returns
//! Allow, Deny, or RequireApproval.
//!
//! ## v0.4.0 additions
//!
//! - **Alignment profiles**: Structured declarations of agent capabilities,
//!   constraints, and coordination rules (`alignment` module).
//! - **Policy compiler**: Compiles alignment profiles into enforceable
//!   capability manifests (`compiler` module).
//! - **Exemption patterns**: Configurable summary exemption via `.ta/summary-exempt`
//!   file, replacing hardcoded patterns (`exemption` module).
//!
//! ## Key invariants
//!
//! - **Default deny**: no manifest → denied. No matching grant → denied.
//! - **Side effects gated**: verbs like "apply", "commit", "send", "post"
//!   always return RequireApproval, even when granted.
//! - **Path traversal blocked**: URIs containing ".." are always denied.

pub mod alignment;
pub mod capability;
pub mod cascade;
pub mod compiler;
pub mod constitution;
pub mod context;
pub mod document;
pub mod engine;
pub mod error;
pub mod exemption;

pub use alignment::{
    AgentSetupProposal, AlignmentProfile, AutonomyEnvelope, CoordinationConfig, Milestone,
    ProposedAgent,
};
pub use capability::{CapabilityGrant, CapabilityManifest};
pub use cascade::{CliOverrides, PolicyCascade};
pub use compiler::{CompilerError, CompilerOptions, PolicyCompiler};
pub use constitution::{
    AccessConstitution, ConstitutionEntry, ConstitutionError, ConstitutionStore,
    ConstitutionValidation, EnforcementMode,
};
pub use context::PolicyContext;
pub mod access_filter;
pub mod auto_approve;

pub use access_filter::AccessFilter;
pub use auto_approve::{AutoApproveDecision, DraftInfo};
pub use document::{
    AgentPolicyOverride, AutoApproveConditions, AutoApproveConfig, AutoApproveDraftConfig,
    BudgetConfig, EscalationConfig, PolicyDefaults, PolicyDocument, PolicyEnforcement,
    SchemePolicy, SecurityLevel,
};
pub use engine::{EvaluationStep, EvaluationTrace, PolicyDecision, PolicyEngine, PolicyRequest};
pub use error::PolicyError;
pub use exemption::ExemptionPatterns;
