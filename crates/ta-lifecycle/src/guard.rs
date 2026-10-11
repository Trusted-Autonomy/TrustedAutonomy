//! Restart-loop guard.
//!
//! If a restart does not converge (the new binary still reports a build that
//! differs from the installed one, or it crashes on start), retrying forever
//! would restart the daemon every check interval. The guard persists an
//! attempt record per target build, backs off exponentially, and gives up
//! after a small cap until a different build is installed. State lives on
//! disk because the process that restarts is replaced by the restart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::version::BuildIdentity;

pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
pub const DEFAULT_BASE_BACKOFF_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardState {
    /// `BuildIdentity::describe()` of the build we are trying to reach.
    pub target: String,
    /// The running build when the last attempt was made.
    pub from: String,
    pub attempts: u32,
    pub last_attempt_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    /// Go ahead; this is attempt number `attempt`.
    Proceed { attempt: u32 },
    /// Too soon after the previous attempt.
    Backoff { attempts: u32, retry_at_unix: u64 },
    /// Cap reached for this target build.
    GaveUp { attempts: u32, last_from: String },
}

#[derive(Debug, Clone)]
pub struct RestartGuard {
    path: PathBuf,
    pub max_attempts: u32,
    pub base_backoff_secs: u64,
}

impl RestartGuard {
    /// Guard persisted at `<project>/.ta/daemon-update.json`.
    pub fn for_project(project_root: &Path) -> Self {
        Self::at(project_root.join(".ta").join("daemon-update.json"))
    }

    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_backoff_secs: DEFAULT_BASE_BACKOFF_SECS,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Missing or corrupt state counts as "no attempts yet".
    pub fn load(&self) -> GuardState {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn evaluate(&self, target: &BuildIdentity, now_unix: u64) -> GuardDecision {
        let state = self.load();
        if state.target != target.describe() || state.attempts == 0 {
            return GuardDecision::Proceed { attempt: 1 };
        }
        if state.attempts >= self.max_attempts {
            return GuardDecision::GaveUp {
                attempts: state.attempts,
                last_from: state.from,
            };
        }
        let wait = self
            .base_backoff_secs
            .saturating_mul(1u64 << (state.attempts - 1).min(16));
        let retry_at = state.last_attempt_unix.saturating_add(wait);
        if now_unix < retry_at {
            GuardDecision::Backoff {
                attempts: state.attempts,
                retry_at_unix: retry_at,
            }
        } else {
            GuardDecision::Proceed {
                attempt: state.attempts + 1,
            }
        }
    }

    /// Persist an attempt *before* restarting: the process may not survive
    /// to record it afterwards.
    pub fn record_attempt(
        &self,
        target: &BuildIdentity,
        running: &BuildIdentity,
        attempt: u32,
        now_unix: u64,
    ) -> Result<(), String> {
        let state = GuardState {
            target: target.describe(),
            from: running.describe(),
            attempts: attempt,
            last_attempt_unix: now_unix,
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let json = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, json).map_err(|e| {
            format!(
                "cannot record restart attempt in {}: {e}; refusing to restart without a loop guard",
                self.path.display()
            )
        })
    }

    /// The running build now equals the installed one: forget past attempts.
    /// Returns the cleared state when there was one (for a success log line).
    pub fn clear(&self) -> Option<GuardState> {
        let prior = self.load();
        if prior.attempts == 0 && !self.path.exists() {
            return None;
        }
        let _ = std::fs::remove_file(&self.path);
        (prior.attempts > 0).then_some(prior)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(h: &str) -> BuildIdentity {
        BuildIdentity::new("1.0.0", Some(h))
    }

    fn guard() -> (tempfile::TempDir, RestartGuard) {
        let d = tempfile::tempdir().unwrap();
        let g = RestartGuard::for_project(d.path());
        (d, g)
    }

    #[test]
    fn first_attempt_proceeds_then_backs_off_then_caps() {
        let (_d, g) = guard();
        let (old, new) = (id("aaa"), id("bbb"));
        assert_eq!(
            g.evaluate(&new, 1000),
            GuardDecision::Proceed { attempt: 1 }
        );
        g.record_attempt(&new, &old, 1, 1000).unwrap();
        assert_eq!(
            g.evaluate(&new, 1100),
            GuardDecision::Backoff {
                attempts: 1,
                retry_at_unix: 1300
            }
        );
        assert_eq!(
            g.evaluate(&new, 1300),
            GuardDecision::Proceed { attempt: 2 }
        );
        g.record_attempt(&new, &old, 2, 1300).unwrap();
        // Second backoff doubles.
        assert_eq!(
            g.evaluate(&new, 1400),
            GuardDecision::Backoff {
                attempts: 2,
                retry_at_unix: 1900
            }
        );
        g.record_attempt(&new, &old, 3, 1900).unwrap();
        assert_eq!(
            g.evaluate(&new, 999_999),
            GuardDecision::GaveUp {
                attempts: 3,
                last_from: "1.0.0 (aaa)".into()
            }
        );
    }

    #[test]
    fn a_different_target_build_resets_the_cap() {
        let (_d, g) = guard();
        g.record_attempt(&id("bbb"), &id("aaa"), 3, 10).unwrap();
        assert!(matches!(
            g.evaluate(&id("bbb"), 11),
            GuardDecision::GaveUp { .. }
        ));
        assert_eq!(
            g.evaluate(&id("ccc"), 11),
            GuardDecision::Proceed { attempt: 1 }
        );
    }

    #[test]
    fn clear_forgets_attempts_and_reports_them() {
        let (_d, g) = guard();
        assert!(g.clear().is_none());
        g.record_attempt(&id("bbb"), &id("aaa"), 2, 10).unwrap();
        let prior = g.clear().unwrap();
        assert_eq!(prior.attempts, 2);
        assert_eq!(
            g.evaluate(&id("bbb"), 11),
            GuardDecision::Proceed { attempt: 1 }
        );
    }

    #[test]
    fn corrupt_state_is_treated_as_no_attempts() {
        let (_d, g) = guard();
        std::fs::create_dir_all(g.path().parent().unwrap()).unwrap();
        std::fs::write(g.path(), "{{{").unwrap();
        assert_eq!(
            g.evaluate(&id("bbb"), 5),
            GuardDecision::Proceed { attempt: 1 }
        );
    }

    #[test]
    fn unwritable_state_refuses_to_restart_rather_than_loop() {
        let d = tempfile::tempdir().unwrap();
        // A directory where the file should be makes the write fail.
        let p = d.path().join("daemon-update.json");
        std::fs::create_dir_all(&p).unwrap();
        let g = RestartGuard::at(p);
        let e = g.record_attempt(&id("b"), &id("a"), 1, 1).unwrap_err();
        assert!(e.contains("loop guard"), "{e}");
    }
}
