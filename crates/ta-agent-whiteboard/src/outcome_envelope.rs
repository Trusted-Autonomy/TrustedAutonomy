//! Stamped, signed outcome envelopes (red-team CR-04 / CR-32, plan phase
//! v0.17.12.2).
//!
//! The daemon authenticates the role behind an outcome, stamps
//! `{team_session, role, goal_id, target, msg_id, issued_at}` next to the
//! outcome fields and signs the unit with an Ed25519 key only the daemon
//! holds. The envelope is ADDITIVE: the four original fields keep their names
//! and meanings, and `v`, `stamp` and `sig` are new. A consumer that ignores
//! them keeps working; a consumer that verifies them can prove which role
//! posted the outcome and refuse forgeries, tampering, replays and stale
//! messages, even though the message bus itself is untrusted.
//!
//! This module is dependency-light on purpose (serde, ed25519-dalek, sha2,
//! base64) so the poller can copy or depend on it. The authoritative wire
//! contract is `docs/superpowers/specs/2026-10-11-outcome-envelope-contract.md`.
//!
//! Canonical signing input (version 1), UTF-8, every line ends with `\n`:
//!
//! ```text
//! ta-outcome-envelope/v1
//! kid:<kid>
//! team_session:<team_session>
//! role:<role>
//! goal_id:<goal_id>
//! target:<target>
//! msg_id:<msg_id>
//! issued_at:<decimal unix seconds>
//! body_sha256:<lowercase hex>
//! ```
//!
//! `body_sha256` is SHA-256 over, for each of `candidate_id`, `outcome`,
//! `detail`, `new_task_title` in that order, an 8-byte big-endian length
//! followed by the UTF-8 bytes. An absent `new_task_title` is the length
//! `u64::MAX` with no bytes (distinct from the empty string).

use std::collections::HashMap;

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Envelope schema version this module emits and accepts.
pub const ENVELOPE_VERSION: u32 = 1;
/// First line of the canonical signing input.
pub const SIGNING_PREFIX: &str = "ta-outcome-envelope/v1";
/// The only signature algorithm in version 1.
pub const SIG_ALG: &str = "ed25519";

/// Hard cap on a whole serialized envelope.
pub const MAX_ENVELOPE_BYTES: usize = 65_536;
pub const MAX_CANDIDATE_ID_BYTES: usize = 256;
pub const MAX_OUTCOME_BYTES: usize = 32;
pub const MAX_DETAIL_BYTES: usize = 16_384;
pub const MAX_TITLE_BYTES: usize = 512;
pub const MAX_TEAM_SESSION_BYTES: usize = 128;
pub const MAX_ROLE_BYTES: usize = 64;
pub const MAX_GOAL_ID_BYTES: usize = 64;
pub const MAX_TARGET_BYTES: usize = 128;
pub const MAX_MSG_ID_BYTES: usize = 64;
pub const MAX_KID_BYTES: usize = 64;

/// Outcome kinds the poller understands.
pub const ALLOWED_OUTCOMES: [&str; 5] = ["done", "blocked", "new_work", "reply", "delegate"];

/// An envelope more than this many seconds in the future is refused (clock
/// skew allowance).
pub const DEFAULT_MAX_FUTURE_SKEW_SECS: i64 = 30;
/// Default staleness window, matching `[whiteboard] outcome_max_age_secs`.
pub const DEFAULT_MAX_AGE_SECS: i64 = 300;

/// The four original outcome fields, unchanged in name and meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeBody {
    pub candidate_id: String,
    pub outcome: String,
    pub detail: String,
    #[serde(default)]
    pub new_task_title: Option<String>,
}

/// What the daemon asserts about the sender. Never taken from the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub team_session: String,
    pub role: String,
    pub goal_id: String,
    /// Execution target identity: `local` for this machine, otherwise an id
    /// such as `runner:build-7`, so a remote runner's outcomes are
    /// attributable.
    pub target: String,
    pub msg_id: String,
    /// Unix seconds.
    pub issued_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigBlock {
    pub kid: String,
    pub alg: String,
    /// base64url, no padding, of the 64-byte Ed25519 signature.
    pub value: String,
}

/// The version 1 wire object: original fields plus `v`, `stamp`, `sig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeEnvelope {
    pub candidate_id: String,
    pub outcome: String,
    pub detail: String,
    #[serde(default)]
    pub new_task_title: Option<String>,
    pub v: u32,
    pub stamp: Stamp,
    pub sig: SigBlock,
}

impl OutcomeEnvelope {
    pub fn body(&self) -> OutcomeBody {
        OutcomeBody {
            candidate_id: self.candidate_id.clone(),
            outcome: self.outcome.clone(),
            detail: self.detail.clone(),
            new_task_title: self.new_task_title.clone(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an OutcomeEnvelope always serializes")
    }
}

/// Why an envelope was refused. Every message names the problem and what to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("outcome field {field} is invalid: {why}. Fix the field and send the outcome again.")]
    InvalidField { field: &'static str, why: String },
    #[error("outcome envelope is {size} bytes, over the {max} byte cap. Shorten `detail` and send again.")]
    TooLarge { size: usize, max: usize },
    #[error("outcome envelope is malformed: {0}")]
    Malformed(String),
    #[error("outcome envelope has no signature (v/stamp/sig absent) and this verifier requires stamped outcomes (`[whiteboard] require_stamped_outcomes`). Only daemon-stamped envelopes are accepted; send the outcome through ta_whiteboard_outcome_send.")]
    Unstamped,
    #[error("outcome envelope version {0} is not supported (this verifier speaks version 1). Upgrade the verifier.")]
    UnsupportedVersion(u32),
    #[error("this verifier has no outcome verification keys, so it cannot verify any envelope. Load the registry (`ta show outcome-key` prints its path) before consuming outcomes.")]
    NoKeys,
    #[error("outcome signed by unknown key id {0}. Reload the key registry; if it is still missing the envelope did not come from this daemon.")]
    UnknownKey(String),
    #[error(
        "outcome signed by revoked key id {0}. It was not produced by a trusted key; discard it."
    )]
    KeyRevoked(String),
    #[error("outcome signed by retired key id {kid} after it retired (issued_at {issued_at}, retired_at {retired_at}). Discard it.")]
    SignedAfterRetirement {
        kid: String,
        issued_at: i64,
        retired_at: i64,
    },
    #[error("outcome signature does not verify (forged or tampered). Discard it.")]
    BadSignature,
    #[error("outcome is stale: issued {age_secs}s ago, the window is {max_secs}s. Discard it (or raise `[whiteboard] outcome_max_age_secs` if the poller was down).")]
    Stale { age_secs: i64, max_secs: i64 },
    #[error("outcome is dated {ahead_secs}s in the future (allowed skew {max_secs}s). Check the clocks of the daemon and this verifier.")]
    FromTheFuture { ahead_secs: i64, max_secs: i64 },
    #[error("outcome msg_id {0} was already processed (replay). Discard it.")]
    Replay(String),
    #[error("outcome came from role {role:?}, which may not post outcomes (allowed: {allowed}). Discard it.")]
    RoleNotPermitted { role: String, allowed: String },
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

fn has_control_chars(s: &str) -> bool {
    s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
}

/// Identifier-ish stamp values: ASCII letters, digits and `._:-`. This keeps
/// the canonical signing input free of newlines and ambiguity.
fn check_ident(field: &'static str, value: &str, max: usize) -> Result<(), EnvelopeError> {
    let bad = |why: String| EnvelopeError::InvalidField { field, why };
    if value.is_empty() {
        return Err(bad("must not be empty".into()));
    }
    if value.len() > max {
        return Err(bad(format!("{} bytes, cap is {max}", value.len())));
    }
    if let Some(c) = value
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')))
    {
        return Err(bad(format!(
            "contains {c:?}; only letters, digits and . _ : - are allowed"
        )));
    }
    Ok(())
}

/// Strict schema and size limits for the four outcome fields.
pub fn validate_body(body: &OutcomeBody) -> Result<(), EnvelopeError> {
    let bad = |field: &'static str, why: String| EnvelopeError::InvalidField { field, why };
    if body.candidate_id.is_empty() {
        return Err(bad("candidate_id", "must not be empty".into()));
    }
    if body.candidate_id.len() > MAX_CANDIDATE_ID_BYTES {
        return Err(bad(
            "candidate_id",
            format!(
                "{} bytes, cap is {MAX_CANDIDATE_ID_BYTES}",
                body.candidate_id.len()
            ),
        ));
    }
    if has_control_chars(&body.candidate_id) || body.candidate_id.contains('\n') {
        return Err(bad("candidate_id", "contains control characters".into()));
    }
    if !ALLOWED_OUTCOMES.contains(&body.outcome.as_str()) {
        return Err(bad(
            "outcome",
            format!(
                "{:?} is not one of {}",
                body.outcome,
                ALLOWED_OUTCOMES.join(", ")
            ),
        ));
    }
    if body.detail.len() > MAX_DETAIL_BYTES {
        return Err(bad(
            "detail",
            format!("{} bytes, cap is {MAX_DETAIL_BYTES}", body.detail.len()),
        ));
    }
    if let Some(t) = &body.new_task_title {
        if t.len() > MAX_TITLE_BYTES {
            return Err(bad(
                "new_task_title",
                format!("{} bytes, cap is {MAX_TITLE_BYTES}", t.len()),
            ));
        }
        if has_control_chars(t) || t.contains('\n') {
            return Err(bad("new_task_title", "contains control characters".into()));
        }
    }
    Ok(())
}

fn validate_stamp(stamp: &Stamp) -> Result<(), EnvelopeError> {
    check_ident("team_session", &stamp.team_session, MAX_TEAM_SESSION_BYTES)?;
    check_ident("role", &stamp.role, MAX_ROLE_BYTES)?;
    check_ident("goal_id", &stamp.goal_id, MAX_GOAL_ID_BYTES)?;
    check_ident("target", &stamp.target, MAX_TARGET_BYTES)?;
    check_ident("msg_id", &stamp.msg_id, MAX_MSG_ID_BYTES)?;
    if stamp.issued_at <= 0 {
        return Err(EnvelopeError::InvalidField {
            field: "issued_at",
            why: "must be positive unix seconds".into(),
        });
    }
    Ok(())
}

fn push_len_prefixed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_be_bytes());
    h.update(bytes);
}

/// Lowercase hex SHA-256 of the length-prefixed body fields.
pub fn body_sha256(body: &OutcomeBody) -> String {
    let mut h = Sha256::new();
    push_len_prefixed(&mut h, body.candidate_id.as_bytes());
    push_len_prefixed(&mut h, body.outcome.as_bytes());
    push_len_prefixed(&mut h, body.detail.as_bytes());
    match &body.new_task_title {
        Some(t) => push_len_prefixed(&mut h, t.as_bytes()),
        None => h.update(u64::MAX.to_be_bytes()),
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The exact bytes that are signed.
pub fn canonical_signing_input(kid: &str, stamp: &Stamp, body: &OutcomeBody) -> Vec<u8> {
    format!(
        "{SIGNING_PREFIX}\nkid:{kid}\nteam_session:{}\nrole:{}\ngoal_id:{}\ntarget:{}\nmsg_id:{}\nissued_at:{}\nbody_sha256:{}\n",
        stamp.team_session,
        stamp.role,
        stamp.goal_id,
        stamp.target,
        stamp.msg_id,
        stamp.issued_at,
        body_sha256(body)
    )
    .into_bytes()
}

/// Validate, stamp and sign. The daemon is the only caller.
pub fn sign_outcome(
    body: OutcomeBody,
    stamp: Stamp,
    kid: &str,
    key: &SigningKey,
) -> Result<OutcomeEnvelope, EnvelopeError> {
    validate_body(&body)?;
    validate_stamp(&stamp)?;
    check_ident("kid", kid, MAX_KID_BYTES)?;
    let sig = key.sign(&canonical_signing_input(kid, &stamp, &body));
    let env = OutcomeEnvelope {
        candidate_id: body.candidate_id,
        outcome: body.outcome,
        detail: body.detail,
        new_task_title: body.new_task_title,
        v: ENVELOPE_VERSION,
        stamp,
        sig: SigBlock {
            kid: kid.to_string(),
            alg: SIG_ALG.to_string(),
            value: b64().encode(sig.to_bytes()),
        },
    };
    let size = env.to_bytes().len();
    if size > MAX_ENVELOPE_BYTES {
        return Err(EnvelopeError::TooLarge {
            size,
            max: MAX_ENVELOPE_BYTES,
        });
    }
    Ok(env)
}

// ── Verification ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingStatus {
    Pending,
    Active,
    Retired,
    Revoked,
}

/// One entry of the public key registry. Deserializes straight from an entry
/// of `public-keys.json`; unknown fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct RingKey {
    pub kid: String,
    #[serde(default)]
    pub alg: String,
    /// base64url (no padding) of the 32-byte public key.
    pub public_key: String,
    pub status: RingStatus,
    #[serde(default)]
    pub retired_at: Option<i64>,
}

/// The verification keys a consumer holds. Empty means "cannot verify".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeyRing {
    #[serde(default)]
    pub keys: Vec<RingKey>,
}

impl KeyRing {
    /// Parse the registry file contents (`public-keys.json`).
    pub fn from_registry_json(raw: &str) -> Result<Self, EnvelopeError> {
        serde_json::from_str(raw)
            .map_err(|e| EnvelopeError::Malformed(format!("key registry is not valid JSON ({e})")))
    }
}

/// Remembers recently seen `msg_id`s so a replay is refused.
#[derive(Debug, Default, Clone)]
pub struct ReplayCache {
    seen: HashMap<String, i64>,
}

impl ReplayCache {
    pub fn new() -> Self {
        Self::default()
    }
    fn check_and_insert(&mut self, msg_id: &str, issued_at: i64, now: i64, keep_secs: i64) -> bool {
        // Forget entries that are stale anyway: a replay of those is refused
        // by the staleness check, so the cache stays bounded by the window.
        self.seen.retain(|_, at| now - *at <= keep_secs);
        if self.seen.contains_key(msg_id) {
            return false;
        }
        self.seen.insert(msg_id.to_string(), issued_at);
        true
    }
    pub fn len(&self) -> usize {
        self.seen.len()
    }
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct VerifyPolicy {
    /// Refuse envelopes with no `v`/`stamp`/`sig` (`require_stamped_outcomes`).
    pub require_stamped: bool,
    pub max_age_secs: i64,
    pub max_future_skew_secs: i64,
    /// Roles allowed to post outcomes. Empty means any authenticated role.
    pub allowed_roles: Vec<String>,
}

impl Default for VerifyPolicy {
    fn default() -> Self {
        Self {
            require_stamped: false,
            max_age_secs: DEFAULT_MAX_AGE_SECS,
            max_future_skew_secs: DEFAULT_MAX_FUTURE_SKEW_SECS,
            allowed_roles: Vec::new(),
        }
    }
}

/// A verified envelope or, when allowed, a legacy unstamped one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    Stamped {
        body: OutcomeBody,
        stamp: Stamp,
        kid: String,
    },
    /// No stamp. Only returned when `require_stamped` is off. Carries no
    /// proof of who sent it.
    Legacy { body: OutcomeBody },
}

/// Verify one stream payload. Order: size, shape, version, keys, key status,
/// signature, staleness, role, replay. Only a fully verified envelope enters
/// the replay cache.
pub fn verify_payload(
    payload: &[u8],
    ring: &KeyRing,
    policy: &VerifyPolicy,
    cache: &mut ReplayCache,
    now: i64,
) -> Result<Verified, EnvelopeError> {
    if payload.len() > MAX_ENVELOPE_BYTES {
        return Err(EnvelopeError::TooLarge {
            size: payload.len(),
            max: MAX_ENVELOPE_BYTES,
        });
    }
    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| EnvelopeError::Malformed(format!("not JSON ({e})")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| EnvelopeError::Malformed("not a JSON object".into()))?;

    let has = |k: &str| obj.contains_key(k);
    let stamped_keys = [has("v"), has("stamp"), has("sig")];
    if stamped_keys.iter().all(|b| !b) {
        if policy.require_stamped {
            return Err(EnvelopeError::Unstamped);
        }
        // Legacy shape: the four original fields, leniently (older senders
        // and the poller's own rules govern extras).
        let body = OutcomeBody {
            candidate_id: obj
                .get("candidate_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            outcome: obj
                .get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            detail: obj
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            new_task_title: obj
                .get("new_task_title")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        };
        return Ok(Verified::Legacy { body });
    }
    if stamped_keys.iter().any(|b| !b) {
        // Some but not all of v/stamp/sig: a downgrade attempt, never legacy.
        return Err(EnvelopeError::Malformed(
            "has some but not all of v, stamp and sig; a stamped envelope carries all three".into(),
        ));
    }
    if let Some(v) = obj.get("v").and_then(|v| v.as_u64()) {
        if v != ENVELOPE_VERSION as u64 {
            return Err(EnvelopeError::UnsupportedVersion(v as u32));
        }
    }
    let env: OutcomeEnvelope = serde_json::from_value(value.clone())
        .map_err(|e| EnvelopeError::Malformed(format!("does not match the v1 schema ({e})")))?;
    if env.v != ENVELOPE_VERSION {
        return Err(EnvelopeError::UnsupportedVersion(env.v));
    }
    let body = env.body();
    validate_body(&body)?;
    validate_stamp(&env.stamp)?;
    check_ident("kid", &env.sig.kid, MAX_KID_BYTES)?;
    if env.sig.alg != SIG_ALG {
        return Err(EnvelopeError::Malformed(format!(
            "unsupported signature algorithm {:?}; version 1 uses {SIG_ALG}",
            env.sig.alg
        )));
    }

    if ring.keys.is_empty() {
        return Err(EnvelopeError::NoKeys);
    }
    let key = ring
        .keys
        .iter()
        .find(|k| k.kid == env.sig.kid)
        .ok_or_else(|| EnvelopeError::UnknownKey(env.sig.kid.clone()))?;
    if key.status == RingStatus::Revoked {
        return Err(EnvelopeError::KeyRevoked(key.kid.clone()));
    }
    if key.status == RingStatus::Retired {
        if let Some(retired_at) = key.retired_at {
            if env.stamp.issued_at > retired_at + policy.max_future_skew_secs {
                return Err(EnvelopeError::SignedAfterRetirement {
                    kid: key.kid.clone(),
                    issued_at: env.stamp.issued_at,
                    retired_at,
                });
            }
        }
    }
    let pk_bytes: [u8; 32] = b64()
        .decode(&key.public_key)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| {
            EnvelopeError::Malformed(format!(
                "registry key {} is not a 32-byte base64url public key",
                key.kid
            ))
        })?;
    let vk = VerifyingKey::from_bytes(&pk_bytes).map_err(|_| {
        EnvelopeError::Malformed(format!(
            "registry key {} is not a valid public key",
            key.kid
        ))
    })?;
    let sig_bytes: [u8; 64] = b64()
        .decode(&env.sig.value)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(EnvelopeError::BadSignature)?;
    let sig = Signature::from_bytes(&sig_bytes);
    vk.verify(
        &canonical_signing_input(&env.sig.kid, &env.stamp, &body),
        &sig,
    )
    .map_err(|_| EnvelopeError::BadSignature)?;

    let age = now - env.stamp.issued_at;
    if age > policy.max_age_secs {
        return Err(EnvelopeError::Stale {
            age_secs: age,
            max_secs: policy.max_age_secs,
        });
    }
    if -age > policy.max_future_skew_secs {
        return Err(EnvelopeError::FromTheFuture {
            ahead_secs: -age,
            max_secs: policy.max_future_skew_secs,
        });
    }
    if !policy.allowed_roles.is_empty() && !policy.allowed_roles.contains(&env.stamp.role) {
        return Err(EnvelopeError::RoleNotPermitted {
            role: env.stamp.role.clone(),
            allowed: policy.allowed_roles.join(", "),
        });
    }
    if !cache.check_and_insert(
        &env.stamp.msg_id,
        env.stamp.issued_at,
        now,
        policy.max_age_secs + policy.max_future_skew_secs,
    ) {
        return Err(EnvelopeError::Replay(env.stamp.msg_id.clone()));
    }
    Ok(Verified::Stamped {
        body,
        stamp: env.stamp,
        kid: env.sig.kid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn ring_for(kid: &str, k: &SigningKey, status: RingStatus) -> KeyRing {
        KeyRing {
            keys: vec![RingKey {
                kid: kid.into(),
                alg: SIG_ALG.into(),
                public_key: b64().encode(k.verifying_key().to_bytes()),
                status,
                retired_at: None,
            }],
        }
    }

    fn body() -> OutcomeBody {
        OutcomeBody {
            candidate_id: "wayfinder-task:t1#abc".into(),
            outcome: "reply".into(),
            detail: "hello".into(),
            new_task_title: None,
        }
    }

    fn stamp(msg: &str, at: i64) -> Stamp {
        Stamp {
            team_session: "sess-1".into(),
            role: "chief-of-staff".into(),
            goal_id: "11111111-2222-3333-4444-555555555555".into(),
            target: "local".into(),
            msg_id: msg.into(),
            issued_at: at,
        }
    }

    fn signed(k: &SigningKey, msg: &str, at: i64) -> OutcomeEnvelope {
        sign_outcome(body(), stamp(msg, at), "k1", k).unwrap()
    }

    fn policy() -> VerifyPolicy {
        VerifyPolicy {
            allowed_roles: vec!["chief-of-staff".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_stamped_envelope_verifies_and_carries_the_stamp() {
        let k = key(1);
        let env = signed(&k, "m1", NOW);
        let v = verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW + 1,
        )
        .unwrap();
        match v {
            Verified::Stamped { body, stamp, kid } => {
                assert_eq!(body.outcome, "reply");
                assert_eq!(stamp.role, "chief-of-staff");
                assert_eq!(stamp.target, "local");
                assert_eq!(kid, "k1");
            }
            other => panic!("expected stamped, got {other:?}"),
        }
    }

    #[test]
    fn original_fields_keep_their_names_so_an_old_poller_still_parses_it() {
        let env = signed(&key(1), "m1", NOW);
        let v: serde_json::Value = serde_json::from_slice(&env.to_bytes()).unwrap();
        assert_eq!(v["candidate_id"], "wayfinder-task:t1#abc");
        assert_eq!(v["outcome"], "reply");
        assert_eq!(v["detail"], "hello");
        assert!(v["new_task_title"].is_null());
        assert_eq!(v["v"], 1);
        assert!(v.get("tag").is_none() && v.get("objective").is_none());
    }

    #[test]
    fn tampered_body_is_refused() {
        let k = key(1);
        let mut env = signed(&k, "m1", NOW);
        env.detail = "something else".into();
        let err = verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::BadSignature);
    }

    #[test]
    fn tampered_role_is_refused() {
        let k = key(1);
        let mut env = sign_outcome(
            body(),
            Stamp {
                role: "implementer".into(),
                ..stamp("m1", NOW)
            },
            "k1",
            &k,
        )
        .unwrap();
        env.stamp.role = "chief-of-staff".into();
        let err = verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::BadSignature);
    }

    #[test]
    fn forged_with_another_key_is_refused() {
        let real = key(1);
        let attacker = key(2);
        let env = signed(&attacker, "m1", NOW);
        let err = verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &real, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::BadSignature);
    }

    #[test]
    fn replay_is_refused() {
        let k = key(1);
        let env = signed(&k, "m1", NOW);
        let ring = ring_for("k1", &k, RingStatus::Active);
        let mut cache = ReplayCache::new();
        verify_payload(&env.to_bytes(), &ring, &policy(), &mut cache, NOW).unwrap();
        let err =
            verify_payload(&env.to_bytes(), &ring, &policy(), &mut cache, NOW + 5).unwrap_err();
        assert_eq!(err, EnvelopeError::Replay("m1".into()));
    }

    #[test]
    fn stale_envelope_is_refused_and_does_not_enter_the_cache() {
        let k = key(1);
        let env = signed(&k, "m1", NOW);
        let ring = ring_for("k1", &k, RingStatus::Active);
        let mut cache = ReplayCache::new();
        let err =
            verify_payload(&env.to_bytes(), &ring, &policy(), &mut cache, NOW + 301).unwrap_err();
        assert!(
            matches!(err, EnvelopeError::Stale { age_secs: 301, .. }),
            "{err}"
        );
        assert!(cache.is_empty());
    }

    #[test]
    fn envelope_from_the_future_is_refused() {
        let k = key(1);
        let env = signed(&k, "m1", NOW + 120);
        let err = verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert!(matches!(err, EnvelopeError::FromTheFuture { .. }), "{err}");
    }

    #[test]
    fn verifier_without_the_key_refuses() {
        let env = signed(&key(1), "m1", NOW);
        let err = verify_payload(
            &env.to_bytes(),
            &KeyRing::default(),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::NoKeys);
        assert!(err.to_string().contains("ta show outcome-key"));
    }

    #[test]
    fn unknown_kid_is_refused() {
        let k = key(1);
        let env = signed(&k, "m1", NOW);
        let err = verify_payload(
            &env.to_bytes(),
            &ring_for("other", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::UnknownKey("k1".into()));
    }

    #[test]
    fn revoked_key_is_refused_and_retired_key_only_before_retirement() {
        let k = key(1);
        let env = signed(&k, "m1", NOW);
        let err = verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &k, RingStatus::Revoked),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::KeyRevoked("k1".into()));

        let mut ring = ring_for("k1", &k, RingStatus::Retired);
        ring.keys[0].retired_at = Some(NOW + 100);
        verify_payload(
            &env.to_bytes(),
            &ring,
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap();
        ring.keys[0].retired_at = Some(NOW - 100);
        let err = verify_payload(
            &env.to_bytes(),
            &ring,
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert!(
            matches!(err, EnvelopeError::SignedAfterRetirement { .. }),
            "{err}"
        );
    }

    #[test]
    fn pending_key_verifies_so_pollers_can_preload_it() {
        let k = key(1);
        let env = signed(&k, "m1", NOW);
        verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &k, RingStatus::Pending),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap();
    }

    #[test]
    fn wrong_role_is_refused() {
        let k = key(1);
        let env = sign_outcome(
            body(),
            Stamp {
                role: "implementer".into(),
                ..stamp("m1", NOW)
            },
            "k1",
            &k,
        )
        .unwrap();
        let err = verify_payload(
            &env.to_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert!(
            matches!(err, EnvelopeError::RoleNotPermitted { .. }),
            "{err}"
        );
    }

    #[test]
    fn legacy_unstamped_flows_when_not_required_and_is_refused_when_required() {
        let legacy = serde_json::json!({
            "candidate_id": "wayfinder-task:t1",
            "outcome": "done",
            "detail": "closed",
            "new_task_title": null,
        })
        .to_string();
        let ring = KeyRing::default();
        let v = verify_payload(
            legacy.as_bytes(),
            &ring,
            &VerifyPolicy::default(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap();
        assert!(matches!(v, Verified::Legacy { .. }));

        let strict = VerifyPolicy {
            require_stamped: true,
            ..Default::default()
        };
        let err = verify_payload(
            legacy.as_bytes(),
            &ring,
            &strict,
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::Unstamped);
        assert!(err.to_string().contains("require_stamped_outcomes"));
    }

    #[test]
    fn partially_stamped_is_never_treated_as_legacy() {
        let k = key(1);
        let mut v: serde_json::Value =
            serde_json::from_slice(&signed(&k, "m1", NOW).to_bytes()).unwrap();
        v.as_object_mut().unwrap().remove("sig");
        let err = verify_payload(
            v.to_string().as_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &VerifyPolicy::default(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert!(matches!(err, EnvelopeError::Malformed(_)), "{err}");
    }

    #[test]
    fn extra_top_level_field_on_a_stamped_envelope_is_refused() {
        let k = key(1);
        let mut v: serde_json::Value =
            serde_json::from_slice(&signed(&k, "m1", NOW).to_bytes()).unwrap();
        v.as_object_mut()
            .unwrap()
            .insert("tag".into(), "evil".into());
        let err = verify_payload(
            v.to_string().as_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert!(matches!(err, EnvelopeError::Malformed(_)), "{err}");
    }

    #[test]
    fn unsupported_version_is_refused() {
        let k = key(1);
        let mut v: serde_json::Value =
            serde_json::from_slice(&signed(&k, "m1", NOW).to_bytes()).unwrap();
        v["v"] = 2.into();
        let err = verify_payload(
            v.to_string().as_bytes(),
            &ring_for("k1", &k, RingStatus::Active),
            &policy(),
            &mut ReplayCache::new(),
            NOW,
        )
        .unwrap_err();
        assert_eq!(err, EnvelopeError::UnsupportedVersion(2));
    }

    #[test]
    fn size_and_schema_limits_apply_at_signing_time() {
        let k = key(1);
        let mut b = body();
        b.detail = "x".repeat(MAX_DETAIL_BYTES + 1);
        assert!(matches!(
            sign_outcome(b, stamp("m", NOW), "k1", &k),
            Err(EnvelopeError::InvalidField {
                field: "detail",
                ..
            })
        ));
        let mut b = body();
        b.outcome = "approve".into();
        assert!(matches!(
            sign_outcome(b, stamp("m", NOW), "k1", &k),
            Err(EnvelopeError::InvalidField {
                field: "outcome",
                ..
            })
        ));
        let mut b = body();
        b.candidate_id = "x".repeat(MAX_CANDIDATE_ID_BYTES + 1);
        assert!(sign_outcome(b, stamp("m", NOW), "k1", &k).is_err());
        let mut b = body();
        b.new_task_title = Some("a\nb".into());
        assert!(sign_outcome(b, stamp("m", NOW), "k1", &k).is_err());
        let mut s = stamp("m", NOW);
        s.role = "chief of staff".into();
        assert!(matches!(
            sign_outcome(body(), s, "k1", &k),
            Err(EnvelopeError::InvalidField { field: "role", .. })
        ));
        let oversized = vec![b' '; MAX_ENVELOPE_BYTES + 1];
        assert!(matches!(
            verify_payload(
                &oversized,
                &KeyRing::default(),
                &policy(),
                &mut ReplayCache::new(),
                NOW
            ),
            Err(EnvelopeError::TooLarge { .. })
        ));
    }

    #[test]
    fn absent_title_and_empty_title_sign_differently() {
        let mut a = body();
        a.new_task_title = None;
        let mut b = body();
        b.new_task_title = Some(String::new());
        assert_ne!(body_sha256(&a), body_sha256(&b));
    }

    /// Pins the canonical signing input so a poller implementing the contract
    /// from the document alone can check itself against these exact bytes.
    #[test]
    fn canonical_signing_input_is_pinned() {
        let input = canonical_signing_input(
            "k1",
            &Stamp {
                team_session: "s".into(),
                role: "chief-of-staff".into(),
                goal_id: "g".into(),
                target: "local".into(),
                msg_id: "m".into(),
                issued_at: 1_800_000_000,
            },
            &OutcomeBody {
                candidate_id: "c".into(),
                outcome: "done".into(),
                detail: "d".into(),
                new_task_title: None,
            },
        );
        let text = String::from_utf8(input).unwrap();
        assert!(text.starts_with(
            "ta-outcome-envelope/v1\nkid:k1\nteam_session:s\nrole:chief-of-staff\ngoal_id:g\ntarget:local\nmsg_id:m\nissued_at:1800000000\nbody_sha256:"
        ));
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 9);
    }

    #[test]
    fn registry_json_deserializes_into_a_key_ring() {
        let raw = r#"{"v":1,"keys":[{"kid":"k1","alg":"ed25519","public_key":"AAAA","status":"active","created_at":1}]}"#;
        let ring = KeyRing::from_registry_json(raw).unwrap();
        assert_eq!(ring.keys[0].status, RingStatus::Active);
    }

    /// The published known-answer vector from the contract document. Ed25519
    /// signatures are deterministic, so this pins the exact signing input.
    #[test]
    fn contract_test_vector_verifies_and_reproduces() {
        let k = SigningKey::from_bytes(&[7u8; 32]);
        assert_eq!(
            b64().encode(k.verifying_key().to_bytes()),
            "6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw"
        );
        let wire = r#"{"candidate_id":"wayfinder-task:t1#abc","outcome":"reply","detail":"hello","new_task_title":null,"v":1,"stamp":{"team_session":"sess-1","role":"chief-of-staff","goal_id":"11111111-2222-3333-4444-555555555555","target":"local","msg_id":"00000000-0000-4000-8000-000000000001","issued_at":1800000000},"sig":{"kid":"k20261011-0a0b0c0d","alg":"ed25519","value":"IjA7ZDT1qGUSpXNd5N1ZqWphJIcxkEt9ERr8-kA9qz2mf3ZHlM5Yi1WxM2jYqt0wzi4iiFqCQiXlLgKZjoO0Bw"}}"#;
        let ring = KeyRing::from_registry_json(
            r#"{"v":1,"keys":[{"kid":"k20261011-0a0b0c0d","alg":"ed25519","public_key":"6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw","status":"active","created_at":1}]}"#,
        )
        .unwrap();
        let v = verify_payload(
            wire.as_bytes(),
            &ring,
            &VerifyPolicy::default(),
            &mut ReplayCache::new(),
            1_800_000_010,
        )
        .unwrap();
        assert!(matches!(v, Verified::Stamped { .. }));
        let env: OutcomeEnvelope = serde_json::from_str(wire).unwrap();
        assert_eq!(
            body_sha256(&env.body()),
            "676e7fc106d2ade491bd99c08a8bff3acd183891c64950e67a5aa1af86202530"
        );
        // Re-signing reproduces the published signature byte for byte.
        let again = sign_outcome(env.body(), env.stamp.clone(), &env.sig.kid, &k).unwrap();
        assert_eq!(again.sig.value, env.sig.value);
    }
}
