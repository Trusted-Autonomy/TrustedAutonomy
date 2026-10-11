// phase_claim.rs — Atomic in-memory phase claim registry (v0.15.24.2).
//
// Serialises concurrent `ta run --phase <id>` requests through a Mutex so
// two workflow workers cannot claim the same phase simultaneously.
//
// Usage:
//   POST /api/plan/phase/claim  { "phase_id": "v0.5.0", "goal_id": "<opt>" }
//     → 200 { "status": "claimed" }    phase was pending and is now in_progress
//     → 409 { "error": "..." }         phase already claimed or not pending
//
// The claim is released automatically when the daemon learns the phase
// transitioned to `done` (via `ta draft apply`). It can also be explicitly
// released by calling `release()` (used in deny/delete flows).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

/// A single phase claim record held in memory.
#[derive(Debug, Clone)]
pub struct ClaimRecord {
    /// The goal ID that claimed this phase, if known at claim time.
    pub goal_id: Option<String>,
    /// Wall-clock time the claim was acquired.
    pub claimed_at: SystemTime,
}

/// In-memory claim registry for plan phases.
///
/// One instance lives in `AppState` for the lifetime of the daemon.
/// All mutations go through the `Mutex`, making claim/release atomic
/// even when multiple HTTP requests arrive concurrently.
#[derive(Default)]
pub struct PhaseClaims {
    inner: Mutex<HashMap<String, ClaimRecord>>,
}

impl PhaseClaims {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to record an in-memory claim for `phase_id`.
    ///
    /// Returns `Ok(())` if the claim succeeded (phase was not previously
    /// claimed). Returns `Err(message)` if the phase is already held by
    /// another caller — the caller should return HTTP 409 to the client.
    pub fn try_claim(&self, phase_id: &str, goal_id: Option<&str>) -> Result<(), String> {
        let mut map = self.inner.lock().expect("phase_claims mutex poisoned");
        if let Some(existing) = map.get(phase_id) {
            return Err(format!(
                "Phase {} is already claimed (goal: {})",
                phase_id,
                existing.goal_id.as_deref().unwrap_or("unknown")
            ));
        }
        map.insert(
            phase_id.to_string(),
            ClaimRecord {
                goal_id: goal_id.map(|s| s.to_string()),
                claimed_at: SystemTime::now(),
            },
        );
        Ok(())
    }

    /// Release the claim for `phase_id` (e.g., on deny or completion).
    ///
    /// No-op if the phase was not claimed.
    pub fn release(&self, phase_id: &str) {
        let mut map = self.inner.lock().expect("phase_claims mutex poisoned");
        map.remove(phase_id);
    }

    /// Return true if `phase_id` is currently claimed.
    pub fn is_claimed(&self, phase_id: &str) -> bool {
        let map = self.inner.lock().expect("phase_claims mutex poisoned");
        map.contains_key(phase_id)
    }

    /// Return a snapshot of all current claims (for introspection / status endpoints).
    pub fn snapshot(&self) -> Vec<(String, Option<String>)> {
        let map = self.inner.lock().expect("phase_claims mutex poisoned");
        map.iter()
            .map(|(k, v)| (k.clone(), v.goal_id.clone()))
            .collect()
    }
}

/// Releases the daemon's in-memory claim when `GoalRunStore` reports that a
/// goal stopped holding its phase (v0.17.11.30), and resets PLAN.md when no
/// work landed. Registered once per daemon process.
pub struct DaemonPhaseReleaser {
    pub claims: std::sync::Arc<PhaseClaims>,
}

impl ta_goal::phase_release::PhaseClaimReleaser for DaemonPhaseReleaser {
    fn release(&self, release: &ta_goal::phase_release::PhaseRelease) {
        if release.reset_plan {
            if let Some(root) = release.source_dir.as_deref() {
                let note = format!("phase reset to pending ({})", release.trigger);
                match ta_plan::reset_phase_if_in_progress(root, &release.phase_id, &note) {
                    Ok(true) => tracing::info!(
                        goal_id = %release.goal_id,
                        phase = %release.phase_id,
                        trigger = %release.trigger,
                        "phase reset to pending in PLAN.md"
                    ),
                    Ok(false) => {}
                    Err(e) => tracing::warn!(
                        goal_id = %release.goal_id,
                        phase = %release.phase_id,
                        error = %e,
                        "could not reset the plan phase in PLAN.md; run `ta plan reset {}`",
                        release.phase_id
                    ),
                }
            }
        }
        self.claims.release(&release.phase_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_succeeds_for_new_phase() {
        let claims = PhaseClaims::new();
        assert!(claims.try_claim("v0.1.0", None).is_ok());
    }

    #[test]
    fn duplicate_claim_returns_error() {
        let claims = PhaseClaims::new();
        claims.try_claim("v0.1.0", Some("goal-a")).unwrap();
        let err = claims.try_claim("v0.1.0", Some("goal-b")).unwrap_err();
        assert!(err.contains("already claimed"));
        assert!(err.contains("goal-a"));
    }

    #[test]
    fn release_allows_reclaim() {
        let claims = PhaseClaims::new();
        claims.try_claim("v0.1.0", None).unwrap();
        claims.release("v0.1.0");
        assert!(claims.try_claim("v0.1.0", None).is_ok());
    }

    #[test]
    fn is_claimed_reflects_state() {
        let claims = PhaseClaims::new();
        assert!(!claims.is_claimed("v0.2.0"));
        claims.try_claim("v0.2.0", None).unwrap();
        assert!(claims.is_claimed("v0.2.0"));
        claims.release("v0.2.0");
        assert!(!claims.is_claimed("v0.2.0"));
    }

    #[test]
    fn different_phases_are_independent() {
        let claims = PhaseClaims::new();
        claims.try_claim("v0.1.0", None).unwrap();
        // v0.2.0 is a different phase — should not conflict.
        assert!(claims.try_claim("v0.2.0", None).is_ok());
    }
}
