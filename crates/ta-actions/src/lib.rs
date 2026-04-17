// ta-actions — External Action Governance Framework (v0.13.4).
//
// Provides the policy, capture, and rate-limiting layer for external actions
// performed by agents (email, social posts, API calls, DB queries). TA does
// not implement the actions; plugins do. TA governs them.
//
// ## Architecture
//
// ```text
//  Agent → ta_external_action MCP tool
//               │
//               ▼
//         ActionPolicies.policy_for(action_type)
//               │
//         ┌─────┴──────────────────────────┐
//         │ Block   │  Review   │   Auto   │
//         │ reject  │ capture → │ execute  │
//         │         │  draft    │  plugin  │
//         └─────────┴───────────┴──────────┘
//               │
//               ▼
//         ActionCapture.append(CapturedAction)  ← ALL paths log here
// ```
//
// ## Usage
//
// ```rust
// use ta_actions::{ActionPolicies, ActionRegistry, ActionCapture, RateLimiter};
//
// let policies = ActionPolicies::load(workflow_toml_path);
// let registry = ActionRegistry::new();
// let capture = ActionCapture::new(ta_dir);
// let mut limiter = RateLimiter::new();
// ```

pub mod action;
pub mod capture;
pub mod constitution_rules;
pub mod dispatch;
pub mod policy;
pub mod rate_limit;
pub mod ratelimit;

// Re-export the most commonly used types.
pub use action::{ActionError, ActionRegistry, ActionTypeInfo, ExternalAction};
pub use capture::{ActionCapture, ActionOutcome, CaptureError, CapturedAction};
pub use constitution_rules::{ConstitutionViolation, PolicyConstitution};
pub use dispatch::{DispatchResult, EmailDispatchGuard};
pub use policy::{ActionPolicies, ActionPolicy, ActionPolicyConfig};
pub use rate_limit::{RateLimitResult, RateLimiter};
pub use ratelimit::{SessionRateLimitResult, SessionRateLimiter};
