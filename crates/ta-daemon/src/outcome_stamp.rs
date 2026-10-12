// outcome_stamp.rs: the daemon side of stamped, signed outcome envelopes
// (red-team CR-04 / CR-32, plan phase v0.17.12.2).
//
// Signing keys live in the per-user state directory (`ta-credentials`
// `outcome_keys`), never in the project tree. The wire format and the
// verifier live in `ta_agent_whiteboard::outcome_envelope`; the contract for
// consumers is docs/superpowers/specs/2026-10-11-outcome-envelope-contract.md.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ta_agent_whiteboard::outcome_envelope::{
    self, EnvelopeError, KeyRing, OutcomeBody, OutcomeEnvelope, ReplayCache, Stamp, Verified,
    VerifyPolicy,
};
use ta_agent_whiteboard::WhiteboardConfig;
use ta_credentials::outcome_keys::OutcomeKeyStore;
use ta_credentials::state::{Posture, ProjectState, StateLocator};

/// What the daemon asserts about the caller of the outcome endpoint. Every
/// field comes from the verified token or the daemon's own state.
#[derive(Debug, Clone)]
pub struct StampSubject {
    pub team_session: String,
    pub role: String,
    pub goal_id: String,
    pub target: String,
}

fn store_for(project_root: &Path) -> Result<(OutcomeKeyStore, ProjectState), String> {
    let locator = StateLocator::from_env();
    let ps = ProjectState::open(project_root, &locator).map_err(|e| {
        format!(
            "cannot open the TA state directory for {} ({e}). Run `ta show state` to see where it is and `ta doctor` to repair it.",
            project_root.display()
        )
    })?;
    Ok((OutcomeKeyStore::open(&ps), ps))
}

/// Validate, stamp and sign `body` for `subject`. The envelope is returned as
/// the exact bytes to append to the report-back stream.
pub fn stamp_and_sign(
    project_root: &Path,
    subject: &StampSubject,
    body: OutcomeBody,
) -> Result<OutcomeEnvelope, String> {
    let now = chrono::Utc::now().timestamp();
    let (store, _ps) = store_for(project_root)?;
    let (kid, key) = store
        .active_signing_key(now)
        .map_err(|e| format!("cannot sign the outcome: {e}"))?;
    let stamp = Stamp {
        team_session: subject.team_session.clone(),
        role: subject.role.clone(),
        goal_id: subject.goal_id.clone(),
        target: subject.target.clone(),
        msg_id: uuid::Uuid::new_v4().to_string(),
        issued_at: now,
    };
    outcome_envelope::sign_outcome(body, stamp, &kid, &key).map_err(|e| e.to_string())
}

/// Whether verification must refuse unstamped envelopes: the explicit
/// `[whiteboard] require_stamped_outcomes` wins, otherwise on only under the
/// `strict` posture.
pub fn effective_require_stamped(project_root: &Path) -> bool {
    let cfg = WhiteboardConfig::load(project_root);
    cfg.require_stamped_outcomes
        .unwrap_or_else(|| Posture::resolve(&StateLocator::from_env()) == Posture::Strict)
}

fn replay_caches() -> &'static Mutex<HashMap<PathBuf, ReplayCache>> {
    static CACHES: OnceLock<Mutex<HashMap<PathBuf, ReplayCache>>> = OnceLock::new();
    CACHES.get_or_init(Default::default)
}

/// Verify one report-back stream payload against this project's key registry
/// and `[whiteboard]` settings. With `consume` false the replay cache is not
/// updated (a dry run); with it true a verified `msg_id` is remembered and a
/// second presentation is refused.
pub fn check_payload(
    project_root: &Path,
    payload: &[u8],
    consume: bool,
) -> Result<Verified, EnvelopeError> {
    let cfg = WhiteboardConfig::load(project_root);
    let ring = match store_for(project_root) {
        Ok((store, _)) => match store.registry() {
            Ok(reg) => KeyRing::from_registry_json(
                &serde_json::to_string(&reg).unwrap_or_else(|_| "{}".into()),
            )?,
            Err(e) => return Err(EnvelopeError::Malformed(e.to_string())),
        },
        Err(e) => return Err(EnvelopeError::Malformed(e)),
    };
    let policy = VerifyPolicy {
        require_stamped: effective_require_stamped(project_root),
        max_age_secs: cfg.outcome_max_age_secs as i64,
        allowed_roles: cfg.outcome_roles.clone(),
        ..Default::default()
    };
    let now = chrono::Utc::now().timestamp();
    let mut caches = replay_caches().lock().unwrap_or_else(|p| p.into_inner());
    let cache = caches.entry(project_root.to_path_buf()).or_default();
    if consume {
        outcome_envelope::verify_payload(payload, &ring, &policy, cache, now)
    } else {
        let mut scratch = cache.clone();
        outcome_envelope::verify_payload(payload, &ring, &policy, &mut scratch, now)
    }
}
