// assertion.rs: Human assertions, the proof that a named person decided.
//
// A person acting through another surface (Wayfinder, Studio, a channel) never
// holds the local credential. Their surface signs a short-lived assertion and
// the poller relays it verbatim to `POST /api/attention/{id}/act`. TA is the
// only verifier, so a leaked relay token cannot approve, deny or answer
// anything.
//
// Compact form: base64url(header) . base64url(claims) . base64url(signature),
// Ed25519 over the first two segments joined by '.'. The contract (claims, the
// twelve rules, key registration) is in
// docs/superpowers/specs/2026-10-11-attention-feed-and-human-assertions-contract.md
// and the how-to is in docs/USAGE.md ("Human-asserted actions").
//
// The routes only see the `AssertionVerifier` trait, so the mechanism can
// change without touching them.
//
// HONEST LIMIT: the replay cache is persisted under the per-user state
// directory, so a daemon restart does not reopen the replay window. The cache
// keeps each jti for REPLAY_RETENTION_SECS (more than twice the maximum
// lifetime). An assertion is verified against what the person saw, but a
// local process that can read the key registry or the install id learns
// nothing it can sign with: only public keys are stored here.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// An assertion may be valid for at most this long (`exp - iat`).
pub const MAX_LIFETIME_SECS: i64 = 60;
/// `iat` may be at most this far in the future (clock skew allowance).
pub const MAX_IAT_SKEW_SECS: i64 = 30;
/// How long a used jti is remembered: well over twice the maximum lifetime.
pub const REPLAY_RETENTION_SECS: i64 = 300;
/// Cap on remembered jtis (only verified assertions can add to it).
const MAX_REMEMBERED_JTIS: usize = 50_000;
const REPLAY_FILE: &str = "assertion-replay.jsonl";
const CONTRACT_DOC: &str = "docs/USAGE.md, 'Human-asserted actions'";

const KNOWN_CLAIMS: &[&str] = &[
    "user_id",
    "email",
    "action",
    "item_id",
    "project_id",
    "aud",
    "role",
    "escalated",
    "self_approval_permitted",
    "item_digest",
    "answer_sha256",
    "jti",
    "iat",
    "exp",
];

// ── Hashing helpers shared with the feed ────────────────────────────────────

/// `sha256:<lowercase hex>` of `bytes`.
pub fn sha256_tagged(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Canonical JSON: object keys sorted, no whitespace, UTF-8. Two systems that
/// agree on the value agree on these bytes, which is what a digest needs.
pub fn canonical_json(v: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let sorted: BTreeMap<&String, &Value> = map.iter().collect();
                out.push('{');
                for (i, (k, val)) in sorted.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    write(val, out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, val) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(val, out);
                }
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

// ── The request being authorized ────────────────────────────────────────────

/// What TA knows about the item an assertion is being used on.
#[derive(Debug, Clone)]
pub struct ItemFacts {
    pub id: String,
    pub project: String,
    /// The item's current digest (`sha256:...`).
    pub digest: String,
    /// `user`, `role` or `any_maintainer`.
    pub waiting_on_type: String,
    pub waiting_on_value: Option<String>,
    /// The requester recorded in the item's provenance, if any.
    pub requester_id: Option<String>,
    /// True for items that came from agent-originated work (origin `cos` or
    /// `chat`), which are the ones with provenance.
    pub agent_originated: bool,
}

pub struct AssertionRequest<'a> {
    /// `approve`, `deny` or `answer`.
    pub action: &'a str,
    pub item: &'a ItemFacts,
    /// The relayed answer text, for `answer`.
    pub answer: Option<&'a str>,
}

/// A fully verified assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAssertion {
    pub kid: String,
    pub jti: String,
    pub user_id: String,
    pub email: String,
    pub action: String,
    pub role: Option<String>,
    pub escalated: bool,
    /// Set when rule 7's reroute exception accepted a signer other than the
    /// user the item names: the original `waiting_on.value`, for the audit log.
    pub rerouted_from: Option<String>,
    /// The signer is the requester of an agent-originated item and the
    /// signer's system said that is permitted.
    pub self_approved: bool,
    pub at: i64,
}

/// A signature-checked assertion whose claims were NOT checked against an item.
/// Used only to answer "already resolved" safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticAssertion {
    pub jti: String,
    pub user_id: String,
}

/// Why an assertion was refused. Every message says what to do next. The
/// key-related failures (unknown kid, wrong algorithm, bad signature) are one
/// indistinguishable `Rejected`, and the same work is done for each, so a probe
/// learns nothing about which keys exist. Everything else is only reachable by
/// a holder of a registered key, so it is specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssertionError {
    Malformed(String),
    Rejected,
    WrongAudience,
    Mismatch(&'static str),
    Expired,
    NotYetValid,
    BadLifetime,
    Replayed,
    NotPermitted(String),
    SelfApproval,
    ItemChanged,
    AnswerChanged,
}

impl std::fmt::Display for AssertionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssertionError::Malformed(why) => write!(
                f,
                "the assertion is malformed ({why}). Send base64url(header).base64url(claims).\
                 base64url(signature) with header {{\"alg\":\"EdDSA\",\"kid\":...}}; see {CONTRACT_DOC}."
            ),
            AssertionError::Rejected => write!(
                f,
                "the assertion was not accepted: its key id is not registered with this install, \
                 its algorithm is not EdDSA, or its signature does not match. Register the signer's \
                 public key with `ta keys add --kid <kid> --pub <file>` (`ta keys list` shows what is \
                 registered) and sign with that key."
            ),
            AssertionError::WrongAudience => write!(
                f,
                "the assertion is for a different TA install (aud does not match). Read this \
                 install's id from `ta show install` or the install_id field of GET /api/attention \
                 and sign with that as aud."
            ),
            AssertionError::Mismatch(what) => write!(
                f,
                "the assertion is for a different {what} than this request. Sign one for exactly \
                 the action, item and project you are acting on."
            ),
            AssertionError::Expired => write!(
                f,
                "the assertion has expired. Sign a new one (exp at most {MAX_LIFETIME_SECS} seconds \
                 after iat) and check the signer's clock."
            ),
            AssertionError::NotYetValid => write!(
                f,
                "the assertion's iat is more than {MAX_IAT_SKEW_SECS} seconds in the future. Check \
                 the signer's clock and sign a new assertion."
            ),
            AssertionError::BadLifetime => write!(
                f,
                "the assertion lives longer than {MAX_LIFETIME_SECS} seconds (or exp is not after \
                 iat). Sign one with exp at most {MAX_LIFETIME_SECS} seconds after iat."
            ),
            AssertionError::Replayed => write!(
                f,
                "this assertion was already used. Each assertion is single-use; sign a new one \
                 with a fresh jti."
            ),
            AssertionError::NotPermitted(why) => write!(
                f,
                "the signer may not act on this item: {why}. Ask someone the item is waiting on \
                 to act, or have the signing system assert the role the person actually holds."
            ),
            AssertionError::SelfApproval => write!(
                f,
                "the signer is the person who requested this agent-originated work, and \
                 self-approval is not permitted for it. Ask a second approver, or have the signing \
                 system set self_approval_permitted (a sole approver, or the project owner enabled \
                 allow_self_approval)."
            ),
            AssertionError::ItemChanged => write!(
                f,
                "item changed, refresh and review again. The assertion's item_digest is not the \
                 item's current digest, so nothing was done."
            ),
            AssertionError::AnswerChanged => write!(
                f,
                "the relayed answer is not the answer the person signed (answer_sha256 does not \
                 match). Nothing was delivered; relay the answer exactly as signed."
            ),
        }
    }
}

impl std::error::Error for AssertionError {}

/// The pluggable verification mechanism. The routes only ever see this trait.
pub trait AssertionVerifier: Send + Sync {
    /// All twelve contract rules for `req`, at unix time `now`. A success
    /// consumes the assertion's jti.
    fn verify(
        &self,
        token: &str,
        req: &AssertionRequest<'_>,
        now: i64,
    ) -> Result<VerifiedAssertion, AssertionError>;

    /// Signature, algorithm and audience only (no time, item or replay checks,
    /// nothing consumed). Lets TA tell a real signer "already resolved" without
    /// telling an anonymous caller anything.
    fn authenticate(&self, token: &str) -> Result<AuthenticAssertion, AssertionError>;
}

// ── Replay cache ────────────────────────────────────────────────────────────

struct ReplayCache {
    path: Option<PathBuf>,
    seen: Mutex<HashMap<String, i64>>,
}

impl ReplayCache {
    fn load(path: Option<PathBuf>, now: i64) -> Self {
        let mut seen = HashMap::new();
        if let Some(p) = &path {
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        if let (Some(j), Some(t)) = (v["jti"].as_str(), v["seen_at"].as_i64()) {
                            if t + REPLAY_RETENTION_SECS >= now {
                                seen.insert(j.to_string(), t);
                            }
                        }
                    }
                }
            }
        }
        Self {
            path,
            seen: Mutex::new(seen),
        }
    }

    /// Record `jti`; false when it was already seen or the cache is full.
    fn insert(&self, jti: &str, now: i64) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, t| *t + REPLAY_RETENTION_SECS >= now);
        if seen.contains_key(jti) || seen.len() >= MAX_REMEMBERED_JTIS {
            return false;
        }
        seen.insert(jti.to_string(), now);
        if let Some(p) = &self.path {
            use std::io::Write;
            let line = serde_json::json!({ "jti": jti, "seen_at": now }).to_string();
            let written = p
                .parent()
                .map(std::fs::create_dir_all)
                .transpose()
                .and_then(|_| {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(p)
                })
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                tracing::warn!(
                    path = %p.display(),
                    error = %e,
                    "could not persist the assertion replay cache; replays are still refused until \
                     the daemon restarts"
                );
            }
        }
        true
    }

    fn contains(&self, jti: &str, now: i64) -> bool {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.get(jti)
            .is_some_and(|t| *t + REPLAY_RETENTION_SECS >= now)
    }
}

// ── The Ed25519 registry verifier ───────────────────────────────────────────

pub struct Ed25519RegistryVerifier {
    state_dir: PathBuf,
    replay: ReplayCache,
}

struct Parsed {
    kid: String,
    claims: serde_json::Map<String, Value>,
}

fn b64url_decode(what: &str, s: &str) -> Result<Vec<u8>, AssertionError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|_| AssertionError::Malformed(format!("{what} is not base64url")))
}

impl Ed25519RegistryVerifier {
    pub fn new(state_dir: PathBuf) -> Self {
        let now = chrono::Utc::now().timestamp();
        let replay = ReplayCache::load(Some(state_dir.join(REPLAY_FILE)), now);
        Self { state_dir, replay }
    }

    /// A key that never matches any real signer; verified against when the kid
    /// is unknown so both paths do the same work.
    fn dummy_key() -> VerifyingKey {
        SigningKey::from_bytes(&[7u8; 32]).verifying_key()
    }

    /// Steps 1 and 2: parse, find the key, check the signature. The claims are
    /// returned only if the signature is valid.
    fn authenticate_parsed(
        &self,
        token: &str,
    ) -> Result<(Parsed, ta_credentials::human::AssertionKey), AssertionError> {
        let mut parts = token.trim().split('.');
        let (Some(h), Some(c), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(AssertionError::Malformed(
                "expected three dot-separated segments".into(),
            ));
        };
        let header: Value = serde_json::from_slice(&b64url_decode("the header", h)?)
            .map_err(|e| AssertionError::Malformed(format!("the header is not JSON: {e}")))?;
        let claims: Value = serde_json::from_slice(&b64url_decode("the claims", c)?)
            .map_err(|e| AssertionError::Malformed(format!("the claims are not JSON: {e}")))?;
        let Value::Object(claims) = claims else {
            return Err(AssertionError::Malformed(
                "the claims must be a JSON object".into(),
            ));
        };
        let sig_bytes = b64url_decode("the signature", s)?;
        let sig_arr: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| AssertionError::Malformed("the signature must be 64 bytes".into()))?;
        let signature = Signature::from_bytes(&sig_arr);
        let kid = header["kid"]
            .as_str()
            .ok_or_else(|| AssertionError::Malformed("the header has no kid".into()))?
            .to_string();
        let alg_ok = header["alg"].as_str() == Some("EdDSA");

        // A corrupt registry fails closed as a plain rejection.
        let registered = ta_credentials::human::load_assertion_keys(&self.state_dir)
            .unwrap_or_default()
            .into_iter()
            .find(|k| k.kid == kid);
        let verifying = registered
            .as_ref()
            .and_then(|k| ta_credentials::human::base64_decode(&k.public_key))
            .and_then(|raw| <[u8; 32]>::try_from(raw).ok())
            .and_then(|arr| VerifyingKey::from_bytes(&arr).ok());
        let key_known = verifying.is_some();
        let key = verifying.unwrap_or_else(Self::dummy_key);
        let signing_input = format!("{h}.{c}");
        let sig_ok = key
            .verify_strict(signing_input.as_bytes(), &signature)
            .is_ok();
        match (registered, alg_ok & key_known & sig_ok) {
            (Some(reg), true) => Ok((Parsed { kid, claims }, reg)),
            _ => Err(AssertionError::Rejected),
        }
    }

    fn check_aud(&self, claims: &serde_json::Map<String, Value>) -> Result<(), AssertionError> {
        let install = ta_credentials::human::install_id(&self.state_dir)
            .map_err(|e| AssertionError::Malformed(format!("cannot read install id: {e}")))?;
        let aud = claims.get("aud").and_then(Value::as_str).unwrap_or("");
        if ta_credentials::human::constant_time_eq(aud.as_bytes(), install.as_bytes()) {
            Ok(())
        } else {
            Err(AssertionError::WrongAudience)
        }
    }
}

fn req_str<'a>(
    claims: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a str, AssertionError> {
    claims
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AssertionError::Malformed(format!("claim '{name}' is required")))
}

fn req_int(claims: &serde_json::Map<String, Value>, name: &str) -> Result<i64, AssertionError> {
    claims
        .get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| AssertionError::Malformed(format!("claim '{name}' must be an integer")))
}

fn opt_bool(claims: &serde_json::Map<String, Value>, name: &str) -> bool {
    claims.get(name).and_then(Value::as_bool).unwrap_or(false)
}

impl AssertionVerifier for Ed25519RegistryVerifier {
    fn authenticate(&self, token: &str) -> Result<AuthenticAssertion, AssertionError> {
        let (parsed, _key) = self.authenticate_parsed(token)?;
        self.check_aud(&parsed.claims)?;
        Ok(AuthenticAssertion {
            jti: req_str(&parsed.claims, "jti")?.to_string(),
            user_id: req_str(&parsed.claims, "user_id")?.to_string(),
        })
    }

    fn verify(
        &self,
        token: &str,
        req: &AssertionRequest<'_>,
        now: i64,
    ) -> Result<VerifiedAssertion, AssertionError> {
        // Rules 1 and 2: registered kid, exact alg, valid signature.
        let (parsed, key) = self.authenticate_parsed(token)?;
        let claims = &parsed.claims;

        let unknown: Vec<&str> = claims
            .keys()
            .map(String::as_str)
            .filter(|k| !KNOWN_CLAIMS.contains(k))
            .collect();
        if !unknown.is_empty() {
            // Rule 12: ignored (the signature covers them) but visible.
            tracing::debug!(claims = ?unknown, kid = %parsed.kid, "assertion carries claims this TA does not know; ignoring them");
        }

        // Rule 3: audience.
        self.check_aud(claims)?;

        let user_id = req_str(claims, "user_id")?;
        let email = req_str(claims, "email")?;
        let jti = req_str(claims, "jti")?;
        if jti.len() < 8 || jti.len() > 128 {
            return Err(AssertionError::Malformed(
                "claim 'jti' must be 8 to 128 characters".into(),
            ));
        }

        // Rule 4: action, item, project.
        if req_str(claims, "action")? != req.action {
            return Err(AssertionError::Mismatch("action"));
        }
        if req_str(claims, "item_id")? != req.item.id {
            return Err(AssertionError::Mismatch("item"));
        }
        if req_str(claims, "project_id")? != req.item.project {
            return Err(AssertionError::Mismatch("project"));
        }

        // Rule 5: lifetime and freshness.
        let iat = req_int(claims, "iat")?;
        let exp = req_int(claims, "exp")?;
        if exp <= iat || exp - iat > MAX_LIFETIME_SECS {
            return Err(AssertionError::BadLifetime);
        }
        if exp < now {
            return Err(AssertionError::Expired);
        }
        if iat > now + MAX_IAT_SKEW_SECS {
            return Err(AssertionError::NotYetValid);
        }

        // Rule 6 (read half): a used jti is refused before anything else is
        // learned. The cache is only ever written after every check passes.
        if self.replay.contains(jti, now) {
            return Err(AssertionError::Replayed);
        }

        // Rules 7, 10, 11: the role floor.
        let role = claims
            .get("role")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty());
        let escalated = opt_bool(claims, "escalated");
        let item = req.item;
        let mut rerouted_from = None;
        match item.waiting_on_type.as_str() {
            "user" => {
                let want = item.waiting_on_value.as_deref().unwrap_or("");
                if user_id != want {
                    // Rule 7 exception: a viewer's item rerouted by Wayfinder is
                    // acted on by someone else under the `escalated` claim.
                    check_reroute(&key, req.action, escalated, role, want)?;
                    rerouted_from = Some(want.to_string());
                }
            }
            "role" => {
                let want = item.waiting_on_value.as_deref().unwrap_or("");
                let Some(claimed) = role else {
                    return Err(AssertionError::NotPermitted(format!(
                        "this item is waiting on role '{want}' and the assertion carries no role claim"
                    )));
                };
                if !key.role_satisfies(claimed, want) {
                    return Err(AssertionError::NotPermitted(format!(
                        "role '{claimed}' does not satisfy role '{want}' (rank order {})",
                        key.role_order.join(" < ")
                    )));
                }
            }
            _ => {
                let Some(claimed) = role else {
                    return Err(AssertionError::NotPermitted(
                        "this item is waiting on any maintainer and the assertion carries no role claim"
                            .into(),
                    ));
                };
                if !key.is_maintainer(claimed) {
                    return Err(AssertionError::NotPermitted(format!(
                        "role '{claimed}' is not a maintainer role for key '{}' ({})",
                        key.kid,
                        key.maintainer_roles.join(", ")
                    )));
                }
            }
        }

        // Rule 8: no self-approval of agent-originated work, unless permitted.
        let decides = matches!(req.action, "approve" | "deny");
        let is_requester = item.requester_id.as_deref() == Some(user_id);
        let mut self_approved = false;
        if decides && item.agent_originated && is_requester {
            if opt_bool(claims, "self_approval_permitted") {
                self_approved = true;
            } else {
                return Err(AssertionError::SelfApproval);
            }
        }

        // Rule 9: bound to what the person saw.
        let tagged_eq = |claim: &str, want: &str| {
            claims
                .get(claim)
                .and_then(Value::as_str)
                .is_some_and(|got| {
                    ta_credentials::human::constant_time_eq(
                        got.to_ascii_lowercase().as_bytes(),
                        want.as_bytes(),
                    )
                })
        };
        if decides && !tagged_eq("item_digest", &item.digest) {
            return Err(AssertionError::ItemChanged);
        }
        if req.action == "answer" {
            let answer = req.answer.ok_or_else(|| {
                AssertionError::Malformed("an answer assertion needs the answer text".into())
            })?;
            if !tagged_eq("answer_sha256", &sha256_tagged(answer.as_bytes())) {
                return Err(AssertionError::AnswerChanged);
            }
        }

        // Rule 6 (write half): consume the jti now that nothing else can refuse.
        if !self.replay.insert(jti, now) {
            return Err(AssertionError::Replayed);
        }
        Ok(VerifiedAssertion {
            kid: parsed.kid,
            jti: jti.to_string(),
            user_id: user_id.to_string(),
            email: email.to_string(),
            action: req.action.to_string(),
            role: role.map(str::to_string),
            escalated,
            rerouted_from,
            self_approved,
            at: now,
        })
    }
}

/// Rule 7's exception for an item rerouted away from its named user. Accepted
/// only for approve, deny and answer, with `escalated: true` and a `role`
/// claim that is in the key's role order and ranks at or above the key's
/// `escalation_min_role`. Every refusal says which condition failed.
fn check_reroute(
    key: &ta_credentials::human::AssertionKey,
    action: &str,
    escalated: bool,
    role: Option<&str>,
    named_user: &str,
) -> Result<(), AssertionError> {
    let not_permitted = |why: String| AssertionError::NotPermitted(why);
    if !escalated {
        return Err(not_permitted(format!(
            "this item is waiting on a specific other user ('{named_user}'); only an assertion \
             with escalated: true can act on an item that was rerouted away from its named user"
        )));
    }
    if !matches!(action, "approve" | "deny" | "answer") {
        return Err(not_permitted(format!(
            "the escalated claim covers approve, deny and answer only, not '{action}'"
        )));
    }
    let Some(claimed) = role else {
        return Err(not_permitted(
            "the escalated claim needs a role claim and the assertion carries none".into(),
        ));
    };
    let floor = key.escalation_min_role.as_str();
    match (key.rank(claimed), key.rank(floor)) {
        (Some(c), Some(f)) if c >= f => Ok(()),
        (Some(_), Some(_)) => Err(not_permitted(format!(
            "role '{claimed}' ranks below this key's escalation floor '{floor}' (rank order {}); \
             change the floor with `ta keys add --escalation-min-role`",
            key.role_order.join(" < ")
        ))),
        _ => Err(not_permitted(format!(
            "role '{claimed}' or the escalation floor '{floor}' is not in key '{}' role order ({})",
            key.kid,
            key.role_order.join(" < ")
        ))),
    }
}

/// Where the replay cache lives for a state directory (tests, docs).
pub fn replay_file(state_dir: &Path) -> PathBuf {
    state_dir.join(REPLAY_FILE)
}

#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use ed25519_dalek::Signer;

    pub const KID: &str = "org-1";

    pub struct TestSigner {
        key: SigningKey,
    }

    impl TestSigner {
        pub fn new(seed: u8) -> Self {
            Self {
                key: SigningKey::from_bytes(&[seed; 32]),
            }
        }

        /// A signer from an explicit 32-byte seed (the published test key).
        pub fn from_seed(seed: [u8; 32]) -> Self {
            Self {
                key: SigningKey::from_bytes(&seed),
            }
        }

        pub fn public_hex(&self) -> String {
            self.key
                .verifying_key()
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }

        pub fn public_b64(&self) -> String {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .encode(self.key.verifying_key().as_bytes())
        }

        pub fn register(&self, state_dir: &Path, kid: &str) {
            ta_credentials::human::add_assertion_key(
                state_dir,
                kid,
                &self.public_b64(),
                None,
                None,
                None,
            )
            .unwrap();
        }

        pub fn sign_with(&self, header: &Value, claims: &Value) -> String {
            let enc = |v: &Value| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(v).unwrap())
            };
            let input = format!("{}.{}", enc(header), enc(claims));
            let sig = self.key.sign(input.as_bytes());
            format!(
                "{input}.{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_bytes())
            )
        }

        pub fn sign(&self, kid: &str, claims: &Value) -> String {
            self.sign_with(&serde_json::json!({"alg": "EdDSA", "kid": kid}), claims)
        }
    }

    /// Claims for approving `item` as `user` in `role`, valid at `now`.
    pub fn claims_for(
        install: &str,
        action: &str,
        item: &ItemFacts,
        user: &str,
        role: &str,
        now: i64,
        jti: &str,
    ) -> Value {
        serde_json::json!({
            "user_id": user,
            "email": format!("{user}@example.com"),
            "action": action,
            "item_id": item.id,
            "project_id": item.project,
            "aud": install,
            "role": role,
            "escalated": false,
            "self_approval_permitted": false,
            "item_digest": item.digest,
            "jti": jti,
            "iat": now,
            "exp": now + 30,
        })
    }

    pub fn item(waiting_on_type: &str, value: Option<&str>) -> ItemFacts {
        ItemFacts {
            id: "att_draft_d1".into(),
            project: "proj".into(),
            digest: sha256_tagged(b"what the person saw"),
            waiting_on_type: waiting_on_type.into(),
            waiting_on_value: value.map(str::to_string),
            requester_id: Some("requester-1".into()),
            agent_originated: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    const NOW: i64 = 1_800_000_000;

    struct Rig {
        _dir: tempfile::TempDir,
        verifier: Ed25519RegistryVerifier,
        signer: TestSigner,
        install: String,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let signer = TestSigner::new(1);
        signer.register(dir.path(), KID);
        let install = ta_credentials::human::install_id(dir.path()).unwrap();
        let verifier = Ed25519RegistryVerifier::new(dir.path().to_path_buf());
        Rig {
            _dir: dir,
            verifier,
            signer,
            install,
        }
    }

    impl Rig {
        fn verify(
            &self,
            claims: &Value,
            action: &str,
            item: &ItemFacts,
        ) -> Result<VerifiedAssertion, AssertionError> {
            let token = self.signer.sign(KID, claims);
            self.verifier.verify(
                &token,
                &AssertionRequest {
                    action,
                    item,
                    answer: None,
                },
                NOW,
            )
        }
        fn claims(
            &self,
            action: &str,
            item: &ItemFacts,
            user: &str,
            role: &str,
            jti: &str,
        ) -> Value {
            claims_for(&self.install, action, item, user, role, NOW, jti)
        }
    }

    #[test]
    fn a_valid_assertion_is_accepted_once_and_replay_is_refused() {
        let r = rig();
        let item = item("any_maintainer", None);
        let c = r.claims("approve", &item, "alice", "admin", "jti-00000001");
        let v = r.verify(&c, "approve", &item).unwrap();
        assert_eq!(
            (v.user_id.as_str(), v.jti.as_str()),
            ("alice", "jti-00000001")
        );
        assert_eq!(v.role.as_deref(), Some("admin"));
        assert_eq!(
            r.verify(&c, "approve", &item),
            Err(AssertionError::Replayed)
        );
    }

    #[test]
    fn replay_survives_a_daemon_restart() {
        let r = rig();
        let item = item("any_maintainer", None);
        let c = r.claims("approve", &item, "alice", "owner", "jti-00000002");
        r.verify(&c, "approve", &item).unwrap();
        let restarted = Ed25519RegistryVerifier::new(r._dir.path().to_path_buf());
        let token = r.signer.sign(KID, &c);
        let req = AssertionRequest {
            action: "approve",
            item: &item,
            answer: None,
        };
        assert_eq!(
            restarted.verify(&token, &req, NOW + 5),
            Err(AssertionError::Replayed)
        );
    }

    #[test]
    fn a_refused_assertion_does_not_burn_its_jti() {
        let r = rig();
        let item = item("any_maintainer", None);
        let mut stale = r.claims("approve", &item, "alice", "admin", "jti-00000003");
        stale["item_digest"] = "sha256:stale".into();
        assert_eq!(
            r.verify(&stale, "approve", &item),
            Err(AssertionError::ItemChanged)
        );
        let good = r.claims("approve", &item, "alice", "admin", "jti-00000003");
        assert!(r.verify(&good, "approve", &item).is_ok());
    }

    #[test]
    fn unknown_kid_wrong_alg_and_bad_signature_are_one_indistinguishable_failure() {
        let r = rig();
        let item = item("any_maintainer", None);
        let c = r.claims("approve", &item, "alice", "admin", "jti-00000004");
        let attacker = TestSigner::new(2);
        let req = AssertionRequest {
            action: "approve",
            item: &item,
            answer: None,
        };
        let tokens = [
            attacker.sign(KID, &c),
            r.signer.sign("nope", &c),
            r.signer
                .sign_with(&serde_json::json!({"alg": "HS256", "kid": KID}), &c),
            r.signer
                .sign_with(&serde_json::json!({"alg": "none", "kid": KID}), &c),
        ];
        for t in tokens {
            assert_eq!(
                r.verifier.verify(&t, &req, NOW),
                Err(AssertionError::Rejected)
            );
        }
        let msg = AssertionError::Rejected.to_string();
        assert!(msg.contains("ta keys add") && !msg.contains('\u{2014}'));
    }

    #[test]
    fn a_tampered_claim_fails_the_signature() {
        let r = rig();
        let item = item("any_maintainer", None);
        let token = r.signer.sign(
            KID,
            &r.claims("approve", &item, "alice", "admin", "jti-00000005"),
        );
        let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
        let mut forged: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&parts[1])
                .unwrap(),
        )
        .unwrap();
        forged["role"] = "owner".into();
        parts[1] = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&forged).unwrap());
        let req = AssertionRequest {
            action: "approve",
            item: &item,
            answer: None,
        };
        assert_eq!(
            r.verifier.verify(&parts.join("."), &req, NOW),
            Err(AssertionError::Rejected)
        );
    }

    #[test]
    fn audience_action_item_and_project_must_match() {
        let r = rig();
        let item = item("any_maintainer", None);
        let mut c = r.claims("approve", &item, "alice", "admin", "jti-00000006");
        c["aud"] = "ta_install_other".into();
        assert_eq!(
            r.verify(&c, "approve", &item),
            Err(AssertionError::WrongAudience)
        );

        let c = r.claims("approve", &item, "alice", "admin", "jti-00000007");
        assert_eq!(
            r.verify(&c, "deny", &item),
            Err(AssertionError::Mismatch("action"))
        );
        let mut other = item.clone();
        other.id = "att_draft_d2".into();
        assert_eq!(
            r.verify(&c, "approve", &other),
            Err(AssertionError::Mismatch("item"))
        );
        let mut other = item.clone();
        other.project = "elsewhere".into();
        assert_eq!(
            r.verify(&c, "approve", &other),
            Err(AssertionError::Mismatch("project"))
        );
    }

    #[test]
    fn lifetime_expiry_and_future_iat_are_enforced() {
        let r = rig();
        let item = item("any_maintainer", None);
        let mk = |iat: i64, exp: i64, jti: &str| {
            let mut c = r.claims("approve", &item, "alice", "admin", jti);
            c["iat"] = iat.into();
            c["exp"] = exp.into();
            c
        };
        // exactly 60 s is fine; 61 is not.
        assert!(r
            .verify(&mk(NOW, NOW + 60, "jti-00000008"), "approve", &item)
            .is_ok());
        assert_eq!(
            r.verify(&mk(NOW, NOW + 61, "jti-00000009"), "approve", &item),
            Err(AssertionError::BadLifetime)
        );
        assert_eq!(
            r.verify(&mk(NOW, NOW, "jti-00000010"), "approve", &item),
            Err(AssertionError::BadLifetime)
        );
        // expired: exp before now.
        assert_eq!(
            r.verify(&mk(NOW - 50, NOW - 1, "jti-00000011"), "approve", &item),
            Err(AssertionError::Expired)
        );
        // iat 30 s ahead is the edge; 31 is refused.
        assert!(r
            .verify(&mk(NOW + 30, NOW + 60, "jti-00000012"), "approve", &item)
            .is_ok());
        assert_eq!(
            r.verify(&mk(NOW + 31, NOW + 60, "jti-00000013"), "approve", &item),
            Err(AssertionError::NotYetValid)
        );
    }

    #[test]
    fn role_floor_for_user_role_rank_and_any_maintainer() {
        let r = rig();
        // user: only that user.
        let mut it = item("user", Some("alice"));
        it.requester_id = None;
        let c = r.claims("approve", &it, "alice", "member", "jti-00000020");
        assert!(r.verify(&c, "approve", &it).is_ok());
        let c = r.claims("approve", &it, "bob", "owner", "jti-00000021");
        assert!(matches!(
            r.verify(&c, "approve", &it),
            Err(AssertionError::NotPermitted(_))
        ));

        // role: rank at least. owner satisfies admin; member does not.
        let mut it = item("role", Some("admin"));
        it.requester_id = None;
        for (role, ok, jti) in [
            ("admin", true, "jti-00000022"),
            ("owner", true, "jti-00000023"),
            ("member", false, "jti-00000024"),
        ] {
            let c = r.claims("approve", &it, "carol", role, jti);
            assert_eq!(r.verify(&c, "approve", &it).is_ok(), ok, "{role}");
        }
        // role claim missing.
        let mut c = r.claims("approve", &it, "carol", "admin", "jti-00000025");
        c.as_object_mut().unwrap().remove("role");
        assert!(matches!(
            r.verify(&c, "approve", &it),
            Err(AssertionError::NotPermitted(_))
        ));

        // any_maintainer: admin and owner, never member, even if signed.
        let mut it = item("any_maintainer", None);
        it.requester_id = None;
        for (role, ok, jti) in [
            ("admin", true, "jti-00000026"),
            ("owner", true, "jti-00000027"),
            ("member", false, "jti-00000028"),
        ] {
            let c = r.claims("approve", &it, "dave", role, jti);
            assert_eq!(r.verify(&c, "approve", &it).is_ok(), ok, "{role}");
        }
    }

    #[test]
    fn maintainer_roles_and_rank_follow_the_registered_key() {
        let dir = tempfile::tempdir().unwrap();
        let signer = TestSigner::new(3);
        ta_credentials::human::add_assertion_key(
            dir.path(),
            KID,
            &signer.public_b64(),
            Some(&["lead".to_string()]),
            Some(&["dev".to_string(), "lead".to_string()]),
            None,
        )
        .unwrap();
        let install = ta_credentials::human::install_id(dir.path()).unwrap();
        let v = Ed25519RegistryVerifier::new(dir.path().to_path_buf());
        let mut it = item("any_maintainer", None);
        it.requester_id = None;
        let req = AssertionRequest {
            action: "approve",
            item: &it,
            answer: None,
        };
        let lead = signer.sign(
            KID,
            &claims_for(&install, "approve", &it, "e", "lead", NOW, "jti-00000030"),
        );
        assert!(v.verify(&lead, &req, NOW).is_ok());
        let admin = signer.sign(
            KID,
            &claims_for(&install, "approve", &it, "e", "admin", NOW, "jti-00000031"),
        );
        assert!(matches!(
            v.verify(&admin, &req, NOW),
            Err(AssertionError::NotPermitted(_))
        ));
    }

    #[test]
    fn escalation_is_recorded_and_accepted_under_the_role_actually_held() {
        let r = rig();
        let mut it = item("role", Some("admin"));
        it.requester_id = None;
        let mut c = r.claims("approve", &it, "olivia", "owner", "jti-00000040");
        c["escalated"] = true.into();
        let v = r.verify(&c, "approve", &it).unwrap();
        assert!(v.escalated);
        assert_eq!(v.role.as_deref(), Some("owner"));
    }

    /// A verifier whose key has the default order (member < admin < owner)
    /// and the given escalation floor.
    fn rig_with_floor(floor: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let signer = TestSigner::new(1);
        ta_credentials::human::add_assertion_key_full(
            dir.path(),
            KID,
            &signer.public_b64(),
            None,
            None,
            Some(floor),
            None,
        )
        .unwrap();
        let install = ta_credentials::human::install_id(dir.path()).unwrap();
        let verifier = Ed25519RegistryVerifier::new(dir.path().to_path_buf());
        Rig {
            _dir: dir,
            verifier,
            signer,
            install,
        }
    }

    fn rerouted_claims(r: &Rig, action: &str, it: &ItemFacts, role: &str, jti: &str) -> Value {
        let mut c = r.claims(action, it, "mia", role, jti);
        c["escalated"] = true.into();
        c
    }

    #[test]
    fn rerouted_user_item_is_accepted_under_escalated_claim_for_approve_deny_and_answer() {
        let r = rig();
        let mut it = item("user", Some("viewer-9"));
        it.requester_id = None;
        for (action, jti) in [
            ("approve", "jti-00000080"),
            ("deny", "jti-00000081"),
            ("answer", "jti-00000082"),
        ] {
            let mut c = rerouted_claims(&r, action, &it, "member", jti);
            let answer = "use postgres";
            c["answer_sha256"] = sha256_tagged(answer.as_bytes()).into();
            let token = r.signer.sign(KID, &c);
            let v = r
                .verifier
                .verify(
                    &token,
                    &AssertionRequest {
                        action,
                        item: &it,
                        answer: (action == "answer").then_some(answer),
                    },
                    NOW,
                )
                .unwrap_or_else(|e| panic!("{action}: {e}"));
            assert!(v.escalated, "{action}");
            assert_eq!(v.user_id, "mia");
            assert_eq!(v.rerouted_from.as_deref(), Some("viewer-9"), "{action}");
        }
    }

    #[test]
    fn the_named_user_acting_on_their_own_item_is_not_marked_rerouted() {
        let r = rig();
        let mut it = item("user", Some("viewer-9"));
        it.requester_id = None;
        let mut c = r.claims("approve", &it, "viewer-9", "member", "jti-00000083");
        c["escalated"] = true.into();
        let v = r.verify(&c, "approve", &it).unwrap();
        assert_eq!(v.rerouted_from, None);
    }

    #[test]
    fn rerouted_user_item_is_refused_without_the_escalated_claim() {
        let r = rig();
        let mut it = item("user", Some("viewer-9"));
        it.requester_id = None;
        let c = r.claims("approve", &it, "mia", "owner", "jti-00000084");
        let err = r.verify(&c, "approve", &it).unwrap_err();
        assert!(
            matches!(&err, AssertionError::NotPermitted(w) if w.contains("escalated: true")),
            "{err:?}"
        );
    }

    #[test]
    fn rerouted_user_item_is_refused_when_role_is_below_the_keys_floor() {
        let r = rig_with_floor("admin");
        let mut it = item("user", Some("viewer-9"));
        it.requester_id = None;
        let c = rerouted_claims(&r, "approve", &it, "member", "jti-00000085");
        let err = r.verify(&c, "approve", &it).unwrap_err();
        assert!(
            matches!(&err, AssertionError::NotPermitted(w) if w.contains("below") && w.contains("'admin'")),
            "{err:?}"
        );
        // At the floor it is accepted, and a refusal burnt no jti.
        let ok = rerouted_claims(&r, "approve", &it, "admin", "jti-00000086");
        assert!(r.verify(&ok, "approve", &it).is_ok());
    }

    #[test]
    fn rerouted_user_item_is_refused_when_the_role_claim_is_absent() {
        let r = rig();
        let mut it = item("user", Some("viewer-9"));
        it.requester_id = None;
        let mut c = rerouted_claims(&r, "approve", &it, "owner", "jti-00000087");
        c.as_object_mut().unwrap().remove("role");
        let err = r.verify(&c, "approve", &it).unwrap_err();
        assert!(
            matches!(&err, AssertionError::NotPermitted(w) if w.contains("no role claim") || w.contains("carries none")),
            "{err:?}"
        );
        // A role the key does not know cannot meet the floor either.
        let c = rerouted_claims(&r, "approve", &it, "superuser", "jti-00000088");
        assert!(matches!(
            r.verify(&c, "approve", &it),
            Err(AssertionError::NotPermitted(_))
        ));
    }

    #[test]
    fn rerouted_user_item_is_refused_for_actions_other_than_approve_deny_answer() {
        let r = rig();
        let mut it = item("user", Some("viewer-9"));
        it.requester_id = None;
        for (action, jti) in [
            ("apply", "jti-00000089"),
            ("commands", "jti-00000090"),
            ("shutdown", "jti-00000091"),
        ] {
            let c = rerouted_claims(&r, action, &it, "owner", jti);
            let err = r.verify(&c, action, &it).unwrap_err();
            assert!(
                matches!(&err, AssertionError::NotPermitted(w) if w.contains("approve, deny and answer only")),
                "{action}: {err:?}"
            );
        }
    }

    #[test]
    fn rule_seven_does_not_widen_role_items() {
        // The escalated exception is for user-addressed items only: a role
        // item still needs a role that satisfies it.
        let r = rig();
        let mut it = item("role", Some("owner"));
        it.requester_id = None;
        let c = rerouted_claims(&r, "approve", &it, "member", "jti-00000092");
        assert!(matches!(
            r.verify(&c, "approve", &it),
            Err(AssertionError::NotPermitted(_))
        ));
    }

    #[test]
    fn self_approval_is_refused_by_default_and_audited_when_permitted() {
        let r = rig();
        let it = item("any_maintainer", None); // requester-1, agent originated
        let c = r.claims("approve", &it, "requester-1", "owner", "jti-00000050");
        assert_eq!(
            r.verify(&c, "approve", &it),
            Err(AssertionError::SelfApproval)
        );
        let c = r.claims("deny", &it, "requester-1", "owner", "jti-00000051");
        assert_eq!(r.verify(&c, "deny", &it), Err(AssertionError::SelfApproval));

        let mut ok = r.claims("approve", &it, "requester-1", "owner", "jti-00000052");
        ok["self_approval_permitted"] = true.into();
        let v = r.verify(&ok, "approve", &it).unwrap();
        assert!(v.self_approved);

        // A different approver is not a self-approval.
        let c = r.claims("approve", &it, "someone-else", "owner", "jti-00000053");
        assert!(!r.verify(&c, "approve", &it).unwrap().self_approved);

        // Items that did not come from agent work are out of scope.
        let mut human_item = it.clone();
        human_item.agent_originated = false;
        let c = r.claims(
            "approve",
            &human_item,
            "requester-1",
            "owner",
            "jti-00000054",
        );
        let v = r.verify(&c, "approve", &human_item).unwrap();
        assert!(!v.self_approved);
    }

    #[test]
    fn item_changed_is_refused_with_the_refresh_message() {
        let r = rig();
        let it = item("any_maintainer", None);
        let mut c = r.claims("approve", &it, "alice", "admin", "jti-00000060");
        c["item_digest"] = sha256_tagged(b"a different summary").into();
        let err = r.verify(&c, "approve", &it).unwrap_err();
        assert_eq!(err, AssertionError::ItemChanged);
        assert!(err
            .to_string()
            .contains("item changed, refresh and review again"));
        // Missing digest is also refused on approve/deny.
        let mut c = r.claims("deny", &it, "alice", "admin", "jti-00000061");
        c.as_object_mut().unwrap().remove("item_digest");
        assert_eq!(r.verify(&c, "deny", &it), Err(AssertionError::ItemChanged));
    }

    #[test]
    fn answer_is_bound_to_the_signed_answer_hash() {
        let r = rig();
        let mut it = item("any_maintainer", None);
        it.id = "att_question_q1".into();
        let mut c = r.claims("answer", &it, "alice", "admin", "jti-00000070");
        c["answer_sha256"] = sha256_tagged(b"use postgres").into();
        let token = r.signer.sign(KID, &c);
        let mk = |answer: &'static str| AssertionRequest {
            action: "answer",
            item: &it,
            answer: Some(answer),
        };
        // Swapped answer: refused, and the jti is not burnt.
        assert_eq!(
            r.verifier.verify(&token, &mk("use mysql"), NOW),
            Err(AssertionError::AnswerChanged)
        );
        assert!(r.verifier.verify(&token, &mk("use postgres"), NOW).is_ok());
        // No answer_sha256 at all is refused.
        let mut bare = r.claims("answer", &it, "alice", "admin", "jti-00000071");
        bare.as_object_mut().unwrap().remove("answer_sha256");
        let token = r.signer.sign(KID, &bare);
        assert_eq!(
            r.verifier.verify(&token, &mk("use postgres"), NOW),
            Err(AssertionError::AnswerChanged)
        );
    }

    #[test]
    fn unknown_claims_are_tolerated() {
        let r = rig();
        let it = item("any_maintainer", None);
        let mut c = r.claims("approve", &it, "alice", "admin", "jti-00000080");
        c["future_claim"] = serde_json::json!({"anything": [1, 2, 3]});
        assert!(r.verify(&c, "approve", &it).is_ok());
    }

    #[test]
    fn malformed_tokens_are_actionable() {
        let r = rig();
        let it = item("any_maintainer", None);
        let req = AssertionRequest {
            action: "approve",
            item: &it,
            answer: None,
        };
        for t in ["", "a.b", "a.b.c.d", "!!!.???.***", "e30.e30.AAAA"] {
            let err = r.verifier.verify(t, &req, NOW).unwrap_err();
            assert!(matches!(err, AssertionError::Malformed(_)), "{t}: {err:?}");
            assert!(err.to_string().contains("docs/USAGE.md"));
        }
    }

    #[test]
    fn authenticate_checks_signature_and_audience_but_consumes_nothing() {
        let r = rig();
        let it = item("any_maintainer", None);
        let c = r.claims("approve", &it, "alice", "admin", "jti-00000090");
        let token = r.signer.sign(KID, &c);
        let a = r.verifier.authenticate(&token).unwrap();
        assert_eq!(a.jti, "jti-00000090");
        // Still usable afterwards.
        let req = AssertionRequest {
            action: "approve",
            item: &it,
            answer: None,
        };
        assert!(r.verifier.verify(&token, &req, NOW).is_ok());
        let forged = TestSigner::new(9).sign(KID, &c);
        assert_eq!(
            r.verifier.authenticate(&forged),
            Err(AssertionError::Rejected)
        );
    }

    #[test]
    fn canonical_json_sorts_keys_and_has_no_whitespace() {
        let v = serde_json::json!({"b": [1, {"z": null, "a": "x"}], "a": true});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":true,"b":[1,{"a":"x","z":null}]}"#
        );
        assert!(sha256_tagged(b"").starts_with("sha256:e3b0c442"));
    }
}
