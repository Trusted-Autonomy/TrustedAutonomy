// rate_limit.rs — Per-goal, per-action-type rate limiter (v0.13.4).
//
// Enforces the `rate_limit` setting from `.ta/workflow.toml`.  State is
// in-memory (resets when the daemon restarts), scoped to a single gateway
// session. Each goal gets its own counter per action type so different
// goals cannot consume each other's budget.

use std::collections::HashMap;

use uuid::Uuid;

// ── Result ────────────────────────────────────────────────────────────────────

/// Outcome of a rate limit check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitResult {
    /// Under the limit — the action may proceed.
    Allowed {
        /// How many times this action type has been used in this goal so far.
        current: u32,
        /// Maximum allowed per goal (the configured limit).
        limit: u32,
    },
    /// Limit reached — the action must be blocked.
    Exceeded { limit: u32, current: u32 },
    /// No limit configured for this action type — always allowed.
    Unlimited,
}

impl RateLimitResult {
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            RateLimitResult::Allowed { .. } | RateLimitResult::Unlimited
        )
    }
}

// ── RateLimiter ──────────────────────────────────────────────────────────────

/// In-memory rate limiter. Keyed by `(goal_id, action_type)`.
#[derive(Debug, Default)]
pub struct RateLimiter {
    counts: HashMap<(Uuid, String), u32>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check whether the action is within the configured limit for this goal.
    ///
    /// - `limit = None` → always allowed (`RateLimitResult::Unlimited`).
    /// - `limit = Some(n)` → allowed if `current < n`.
    pub fn check(&self, goal_id: Uuid, action_type: &str, limit: Option<u32>) -> RateLimitResult {
        let Some(limit) = limit else {
            return RateLimitResult::Unlimited;
        };

        let current = self
            .counts
            .get(&(goal_id, action_type.to_owned()))
            .copied()
            .unwrap_or(0);

        if current < limit {
            RateLimitResult::Allowed { current, limit }
        } else {
            RateLimitResult::Exceeded { limit, current }
        }
    }

    /// Increment the usage counter for the given goal + action type.
    ///
    /// Call this AFTER the action succeeds (execute) or is captured for review.
    /// Blocked and rate-limited actions do not consume budget.
    pub fn increment(&mut self, goal_id: Uuid, action_type: &str) {
        *self
            .counts
            .entry((goal_id, action_type.to_owned()))
            .or_default() += 1;
    }

    /// Return the current usage count for a goal + action type.
    pub fn count(&self, goal_id: Uuid, action_type: &str) -> u32 {
        self.counts
            .get(&(goal_id, action_type.to_owned()))
            .copied()
            .unwrap_or(0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_when_no_limit() {
        let limiter = RateLimiter::new();
        let goal = Uuid::new_v4();
        assert_eq!(
            limiter.check(goal, "email", None),
            RateLimitResult::Unlimited
        );
    }

    #[test]
    fn allowed_under_limit() {
        let mut limiter = RateLimiter::new();
        let goal = Uuid::new_v4();

        limiter.increment(goal, "email");
        limiter.increment(goal, "email");

        let result = limiter.check(goal, "email", Some(5));
        assert!(result.is_allowed());
        assert_eq!(
            result,
            RateLimitResult::Allowed {
                current: 2,
                limit: 5
            }
        );
    }

    #[test]
    fn blocked_at_limit() {
        let mut limiter = RateLimiter::new();
        let goal = Uuid::new_v4();

        for _ in 0..3 {
            limiter.increment(goal, "social_post");
        }

        let result = limiter.check(goal, "social_post", Some(3));
        assert!(!result.is_allowed());
        assert_eq!(
            result,
            RateLimitResult::Exceeded {
                limit: 3,
                current: 3
            }
        );
    }

    #[test]
    fn different_goals_are_independent() {
        let mut limiter = RateLimiter::new();
        let goal_a = Uuid::new_v4();
        let goal_b = Uuid::new_v4();

        limiter.increment(goal_a, "email");
        limiter.increment(goal_a, "email");

        // goal_b is untouched.
        assert_eq!(limiter.count(goal_b, "email"), 0);
        let r = limiter.check(goal_b, "email", Some(1));
        assert!(r.is_allowed());
    }

    #[test]
    fn different_action_types_are_independent() {
        let mut limiter = RateLimiter::new();
        let goal = Uuid::new_v4();

        for _ in 0..5 {
            limiter.increment(goal, "email");
        }

        // api_call counter should still be 0.
        assert_eq!(limiter.count(goal, "api_call"), 0);
        assert!(limiter.check(goal, "api_call", Some(2)).is_allowed());
    }

    #[test]
    fn count_reflects_increments() {
        let mut limiter = RateLimiter::new();
        let goal = Uuid::new_v4();

        assert_eq!(limiter.count(goal, "email"), 0);
        limiter.increment(goal, "email");
        assert_eq!(limiter.count(goal, "email"), 1);
        limiter.increment(goal, "email");
        assert_eq!(limiter.count(goal, "email"), 2);
    }
}
