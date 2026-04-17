// dispatch.rs — Email dispatch guard for the External Action Governance Framework.
//
// Enforces that email actions are always routed to human review — never auto-sent.
// If an agent attempts to use `policy = auto` for email, the guard overrides it
// to `policy = review` and surfaces a `ForcedReview` result to the caller.
//
// This is a lightweight compile-time and runtime guard. The constitution_rules
// module provides the configurable rule layer on top.

use crate::ActionPolicy;

// ── DispatchResult ────────────────────────────────────────────────────────────

/// Outcome of an `EmailDispatchGuard::enforce` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResult {
    /// The action may proceed with its configured policy.
    Allowed,
    /// The action's policy was overridden to `Review`. The original policy was
    /// unsuitable (e.g., `auto` for email). Reason explains why.
    ForcedReview { reason: String },
    /// The action must be blocked. Message explains why.
    Blocked { message: String },
}

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Enforces draft-only policy for email actions at the dispatch layer.
///
/// Email is the most sensitive action type — a misconfigured or prompt-injected
/// `policy = "auto"` would send email without human review. This guard intercepts
/// all email dispatches and forces them to `Review` regardless of configuration.
///
/// ```rust
/// use ta_actions::{EmailDispatchGuard, ActionPolicy, DispatchResult};
///
/// let guard = EmailDispatchGuard::new();
/// match guard.enforce("email", &ActionPolicy::Auto) {
///     DispatchResult::ForcedReview { reason } => {
///         // Use ActionPolicy::Review instead
///     }
///     DispatchResult::Allowed => { /* proceed with configured policy */ }
///     DispatchResult::Blocked { message } => { /* return error to agent */ }
/// }
/// ```
#[derive(Debug, Default)]
pub struct EmailDispatchGuard;

impl EmailDispatchGuard {
    pub fn new() -> Self {
        Self
    }

    /// Check whether the action type + policy combination is allowed.
    ///
    /// Rules:
    /// - `email` + `auto` → `ForcedReview` (auto-send is never allowed)
    /// - `email` + `review` → `Allowed`
    /// - `email` + `block` → `Allowed` (block is more restrictive, that's fine)
    /// - Any other action type → `Allowed` (this guard only governs email)
    pub fn enforce(&self, action_type: &str, policy: &ActionPolicy) -> DispatchResult {
        if action_type != "email" {
            return DispatchResult::Allowed;
        }

        match policy {
            ActionPolicy::Auto => DispatchResult::ForcedReview {
                reason: "Email actions are always routed to review — TA never sends email \
                         autonomously. To create a draft, the policy must be 'review'."
                    .into(),
            },
            ActionPolicy::Review => DispatchResult::Allowed,
            ActionPolicy::Block => DispatchResult::Allowed,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_auto_policy_returns_forced_review() {
        let guard = EmailDispatchGuard::new();
        let result = guard.enforce("email", &ActionPolicy::Auto);
        assert!(
            matches!(result, DispatchResult::ForcedReview { .. }),
            "expected ForcedReview, got {:?}",
            result
        );
        if let DispatchResult::ForcedReview { reason } = result {
            assert!(reason.contains("review"), "reason should mention 'review'");
        }
    }

    #[test]
    fn email_review_policy_returns_allowed() {
        let guard = EmailDispatchGuard::new();
        let result = guard.enforce("email", &ActionPolicy::Review);
        assert_eq!(result, DispatchResult::Allowed);
    }

    #[test]
    fn email_block_policy_returns_allowed() {
        let guard = EmailDispatchGuard::new();
        let result = guard.enforce("email", &ActionPolicy::Block);
        assert_eq!(result, DispatchResult::Allowed);
    }

    #[test]
    fn non_email_actions_always_allowed() {
        let guard = EmailDispatchGuard::new();
        for (action_type, policy) in [
            ("api_call", ActionPolicy::Auto),
            ("social_post", ActionPolicy::Auto),
            ("db_query", ActionPolicy::Review),
            ("webhook", ActionPolicy::Auto),
        ] {
            let result = guard.enforce(action_type, &policy);
            assert_eq!(
                result,
                DispatchResult::Allowed,
                "non-email action '{}' should always be Allowed by EmailDispatchGuard",
                action_type
            );
        }
    }
}
