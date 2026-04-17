// ratelimit.rs — Cross-session rate limiter for the External Action Governance Framework.
//
// Persists rate limit state to `.ta/action-ratelimit.json` so that limits are
// enforced across daemon restarts and multiple goal sessions. Each action type
// maintains a list of timestamps; checks are based on a sliding window.
//
// This is separate from `rate_limit.rs` (per-goal in-memory limiter). Use this
// module for `max_per_hour` / `max_per_day` limits in `[actions.email]`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

// ── Result ────────────────────────────────────────────────────────────────────

/// Outcome of a `SessionRateLimiter::check_and_record` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRateLimitResult {
    /// Under all limits — the action may proceed. The timestamp was recorded.
    Allowed,
    /// The hourly limit was exceeded.
    HourlyExceeded { limit: u32, count: u32 },
    /// The daily limit was exceeded.
    DailyExceeded { limit: u32, count: u32 },
}

// ── State ─────────────────────────────────────────────────────────────────────

/// A single timestamped record for an action type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitEntry {
    pub action_type: String,
    /// All timestamps for this action type, sorted ascending.
    /// Entries older than 24 hours are pruned on load and before each check.
    pub timestamps: Vec<DateTime<Utc>>,
}

/// Persisted state for the cross-session rate limiter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RateLimitState {
    pub entries: Vec<RateLimitEntry>,
}

impl RateLimitState {
    /// Return mutable reference to the entry for `action_type`, creating it
    /// if absent.
    fn entry_for_mut(&mut self, action_type: &str) -> &mut RateLimitEntry {
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.action_type == action_type)
        {
            &mut self.entries[pos]
        } else {
            self.entries.push(RateLimitEntry {
                action_type: action_type.to_owned(),
                timestamps: vec![],
            });
            self.entries.last_mut().unwrap()
        }
    }

    /// Return a reference to the entry for `action_type`, if present.
    fn entry_for(&self, action_type: &str) -> Option<&RateLimitEntry> {
        self.entries.iter().find(|e| e.action_type == action_type)
    }

    /// Prune timestamps older than 24 hours from all entries.
    fn prune(&mut self) {
        let cutoff = Utc::now() - Duration::hours(24);
        for entry in &mut self.entries {
            entry.timestamps.retain(|ts| *ts > cutoff);
        }
    }
}

// ── Limiter ───────────────────────────────────────────────────────────────────

/// Cross-session rate limiter. State is persisted to `.ta/action-ratelimit.json`.
pub struct SessionRateLimiter {
    state_path: PathBuf,
    state: RateLimitState,
}

impl SessionRateLimiter {
    /// Create a new limiter, loading state from `ta_dir/action-ratelimit.json`.
    pub fn new(ta_dir: &Path) -> Self {
        Self::load(ta_dir)
    }

    /// Load state from disk (or return empty state if file is absent).
    pub fn load(ta_dir: &Path) -> Self {
        let state_path = ta_dir.join("action-ratelimit.json");
        let mut state = if state_path.exists() {
            match std::fs::read_to_string(&state_path) {
                Ok(content) => {
                    serde_json::from_str::<RateLimitState>(&content).unwrap_or_else(|e| {
                        tracing::warn!(
                            path = %state_path.display(),
                            error = %e,
                            "failed to parse action-ratelimit.json; starting fresh"
                        );
                        RateLimitState::default()
                    })
                }
                Err(e) => {
                    tracing::warn!(
                        path = %state_path.display(),
                        error = %e,
                        "failed to read action-ratelimit.json; starting fresh"
                    );
                    RateLimitState::default()
                }
            }
        } else {
            RateLimitState::default()
        };

        // Prune stale entries on load.
        state.prune();

        Self { state_path, state }
    }

    /// Save state to disk.
    pub fn save(&self) -> Result<(), std::io::Error> {
        // Ensure parent directory exists.
        if let Some(parent) = self.state_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.state).map_err(std::io::Error::other)?;
        std::fs::write(&self.state_path, json)
    }

    /// Check whether recording a new action would exceed the limits.
    ///
    /// On `Allowed`: records the current timestamp and saves state to disk.
    /// On any exceeded result: does NOT record and does NOT save.
    ///
    /// Prunes entries older than 24h before checking.
    pub fn check_and_record(
        &mut self,
        action_type: &str,
        max_per_hour: Option<u32>,
        max_per_day: Option<u32>,
    ) -> SessionRateLimitResult {
        let now = Utc::now();
        let hour_ago = now - Duration::hours(1);

        // Prune old entries.
        self.state.prune();

        // Count existing events within each window.
        let (hourly_count, daily_count) = if let Some(entry) = self.state.entry_for(action_type) {
            let hourly = entry.timestamps.iter().filter(|ts| **ts > hour_ago).count() as u32;
            let daily = entry.timestamps.len() as u32; // already pruned to 24h
            (hourly, daily)
        } else {
            (0, 0)
        };

        // Check hourly limit first.
        if let Some(limit) = max_per_hour {
            if hourly_count >= limit {
                return SessionRateLimitResult::HourlyExceeded {
                    limit,
                    count: hourly_count,
                };
            }
        }

        // Check daily limit.
        if let Some(limit) = max_per_day {
            if daily_count >= limit {
                return SessionRateLimitResult::DailyExceeded {
                    limit,
                    count: daily_count,
                };
            }
        }

        // All checks passed — record the timestamp.
        let entry = self.state.entry_for_mut(action_type);
        entry.timestamps.push(now);
        // Keep sorted.
        entry.timestamps.sort();

        if let Err(e) = self.save() {
            tracing::warn!(
                action_type = %action_type,
                error = %e,
                "failed to save action-ratelimit.json after recording action"
            );
        }

        SessionRateLimitResult::Allowed
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_limiter(dir: &Path) -> SessionRateLimiter {
        let ta_dir = dir.join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        SessionRateLimiter::new(&ta_dir)
    }

    #[test]
    fn hourly_limit_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let mut limiter = make_limiter(dir.path());

        // First 3 allowed (limit = 3).
        for _ in 0..3 {
            assert_eq!(
                limiter.check_and_record("email", Some(3), None),
                SessionRateLimitResult::Allowed
            );
        }

        // 4th should be blocked.
        let result = limiter.check_and_record("email", Some(3), None);
        assert!(
            matches!(
                result,
                SessionRateLimitResult::HourlyExceeded { limit: 3, count: 3 }
            ),
            "expected HourlyExceeded, got {:?}",
            result
        );
    }

    #[test]
    fn daily_limit_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let mut limiter = make_limiter(dir.path());

        // Allow up to 5 per day, no hourly limit.
        for _ in 0..5 {
            assert_eq!(
                limiter.check_and_record("email", None, Some(5)),
                SessionRateLimitResult::Allowed
            );
        }

        let result = limiter.check_and_record("email", None, Some(5));
        assert!(
            matches!(
                result,
                SessionRateLimitResult::DailyExceeded { limit: 5, count: 5 }
            ),
            "expected DailyExceeded, got {:?}",
            result
        );
    }

    #[test]
    fn state_persists_across_load_save() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        {
            let mut limiter = SessionRateLimiter::new(&ta_dir);
            limiter.check_and_record("email", None, None);
            limiter.check_and_record("email", None, None);
        }

        // Reload and verify count is still 2.
        let mut limiter2 = SessionRateLimiter::load(&ta_dir);
        // With daily limit of 2, this should be exceeded on reload.
        let result = limiter2.check_and_record("email", None, Some(2));
        assert!(
            matches!(
                result,
                SessionRateLimitResult::DailyExceeded { limit: 2, count: 2 }
            ),
            "state not persisted: got {:?}",
            result
        );
    }

    #[test]
    fn old_entries_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        // Manually inject an old entry (25h ago).
        let old_ts = Utc::now() - Duration::hours(25);
        let state = RateLimitState {
            entries: vec![RateLimitEntry {
                action_type: "email".into(),
                timestamps: vec![old_ts],
            }],
        };
        let json = serde_json::to_string(&state).unwrap();
        std::fs::write(ta_dir.join("action-ratelimit.json"), json).unwrap();

        // Load should prune the old entry.
        let mut limiter = SessionRateLimiter::load(&ta_dir);
        // Daily limit of 1 — old entry was pruned, so this should be allowed.
        let result = limiter.check_and_record("email", None, Some(1));
        assert_eq!(
            result,
            SessionRateLimitResult::Allowed,
            "old entry should have been pruned: {:?}",
            result
        );
    }

    #[test]
    fn hourly_takes_precedence_over_daily_when_both_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let mut limiter = make_limiter(dir.path());

        // Add 3 entries within the last hour (and day).
        for _ in 0..3 {
            limiter.check_and_record("email", None, None);
        }

        // Both hourly (3) and daily (3) limits are met.
        let result = limiter.check_and_record("email", Some(3), Some(3));
        assert!(
            matches!(result, SessionRateLimitResult::HourlyExceeded { .. }),
            "hourly should take precedence: {:?}",
            result
        );
    }

    #[test]
    fn no_limits_always_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let mut limiter = make_limiter(dir.path());

        for _ in 0..100 {
            assert_eq!(
                limiter.check_and_record("email", None, None),
                SessionRateLimitResult::Allowed
            );
        }
    }
}
