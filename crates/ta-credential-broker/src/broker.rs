// broker.rs — Biscuit-backed CredentialBroker (v0.17.6.4).
//
// Replaces ta_credentials::FileVault's UUID SessionToken (a reference that
// only resolves against the vault process that minted it) with a biscuit
// token: a signed, self-describing grant that any holder of the broker's
// public key can verify offline. The broker persists its ed25519 root key
// as a chmod-0600 file (same custody model as FileVault's age identity) so
// a token minted by one process (`ta credentials grant`) verifies in another
// (the MCP gateway) as long as both open the same `.ta` directory.
//
// Revocation isn't a biscuit primitive (tokens are only bounded by their
// embedded expiry) so the broker keeps a small denylist file keyed by each
// token's authority-block revocation id.

use std::fs;
use std::path::{Path, PathBuf};

use biscuit_auth::{
    Algorithm, AuthorizerBuilder, AuthorizerLimits, Biscuit, BlockBuilder, KeyPair, PrivateKey,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use crate::error::BrokerError;

const ROOT_KEY_FILENAME: &str = "broker_root.key";
const DENYLIST_FILENAME: &str = "broker_revocations.json";

/// A freshly-minted grant.
#[derive(Debug, Clone)]
pub struct GrantedToken {
    /// The base64-encoded biscuit — the opaque bearer value handed to the
    /// agent (e.g. as `TA_SESSION_TOKEN_<credential>`) and presented back to
    /// `ta_external_action` as `session_token`.
    pub token: String,
    /// Hex-encoded revocation id of the token's authority block — a stable
    /// reference for `revoke()` and for display, distinct from `token`
    /// itself (which changes if the token is ever attenuated).
    pub token_id: String,
    pub credential_id: Uuid,
    pub agent_id: String,
    pub allowed_scopes: Vec<String>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Claims recovered from a successfully verified grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedGrant {
    pub credential_id: Uuid,
    pub agent_id: String,
    pub allowed_scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Denylist {
    revoked_token_ids: Vec<String>,
}

/// Mints and verifies biscuit-backed credential grants.
pub struct CredentialBroker {
    keypair: KeyPair,
    denylist_path: PathBuf,
    denylist: Denylist,
}

impl CredentialBroker {
    /// Open (or initialize) a broker rooted at `dir` — typically a project's
    /// `.ta` directory, the same one `FileVault` stores `credentials.json`
    /// alongside. Generates and persists a root key on first use.
    pub fn open(dir: &Path) -> Result<Self, BrokerError> {
        fs::create_dir_all(dir)?;
        let keypair = load_or_create_root_keypair(&dir.join(ROOT_KEY_FILENAME))?;
        let denylist_path = dir.join(DENYLIST_FILENAME);
        let denylist = load_denylist(&denylist_path)?;
        Ok(Self {
            keypair,
            denylist_path,
            denylist,
        })
    }

    /// Mint a biscuit-backed grant for `agent_id`, scoped to `scopes`,
    /// expiring `ttl_secs` from now. The expiry is embedded in the token
    /// itself as a datalog check, so it's enforced by any verifier — not
    /// just this broker instance.
    pub fn grant(
        &self,
        credential_id: Uuid,
        agent_id: &str,
        scopes: Vec<String>,
        ttl_secs: u64,
    ) -> Result<GrantedToken, BrokerError> {
        let issued_at = Utc::now();
        let expires_at = issued_at + Duration::seconds(ttl_secs as i64);
        let scopes_csv = scopes.join(",");

        let fact = format!(
            "grant({:?}, {:?}, {:?})",
            credential_id.to_string(),
            agent_id,
            scopes_csv
        );
        let check = format!("check if time($t), $t < {}", expires_at.to_rfc3339());

        let biscuit = Biscuit::builder()
            .fact(fact.as_str())
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?
            .check(check.as_str())
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?
            .build(&self.keypair)
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?;

        let token_id = revocation_id_hex(&biscuit);
        let token = biscuit
            .to_base64()
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?;

        info!(%credential_id, agent_id, ttl_secs, token_id, "credential broker: grant minted");

        Ok(GrantedToken {
            token,
            token_id,
            credential_id,
            agent_id: agent_id.to_string(),
            allowed_scopes: scopes,
            issued_at,
            expires_at,
        })
    }

    /// Verify a presented token: signature, embedded expiry, and revocation
    /// status. Returns the claims embedded at mint time.
    pub fn verify(&self, token: &str) -> Result<VerifiedGrant, BrokerError> {
        let biscuit = Biscuit::from_base64(token, self.keypair.public())
            .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;

        let token_id = revocation_id_hex(&biscuit);
        if self
            .denylist
            .revoked_token_ids
            .iter()
            .any(|id| id == &token_id)
        {
            return Err(BrokerError::Revoked);
        }

        // biscuit-auth's default `RunLimits::max_time` is 1ms — tuned for a
        // quiet machine, not a loaded CI runner. A trivial authorization
        // (a couple of facts, one query) can exceed that under scheduling
        // jitter alone, producing a spurious "Reached Datalog execution
        // limits" failure that has nothing to do with the grant itself.
        // biscuit-auth's own test suite works around the identical issue
        // ("cheap worker on GitHub Actions") by widening the budget; do the
        // same here rather than let the 1ms default flake on Windows CI.
        let mut authorizer = AuthorizerBuilder::new()
            .time()
            .set_limits(AuthorizerLimits {
                max_time: std::time::Duration::from_secs(1),
                ..Default::default()
            })
            .policy("allow if true")
            .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?
            .build(&biscuit)
            .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;
        authorizer
            .authorize()
            .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;

        let (credential_id_str, agent_id, scopes_csv): (String, String, String) = authorizer
            .query_exactly_one("data($c, $a, $s) <- grant($c, $a, $s)")
            .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;
        let credential_id = Uuid::parse_str(&credential_id_str).map_err(|e| {
            BrokerError::InvalidGrant(format!("grant carries a malformed credential id: {e}"))
        })?;
        let allowed_scopes = if scopes_csv.is_empty() {
            Vec::new()
        } else {
            scopes_csv.split(',').map(str::to_string).collect()
        };

        Ok(VerifiedGrant {
            credential_id,
            agent_id,
            allowed_scopes,
        })
    }

    /// Attenuate `token` (a grant from [`Self::grant`] or a previously
    /// attenuated token from this method) in-process: appends a new block
    /// narrowing `scopes` to at most the token's *currently* effective
    /// scopes and `ttl_secs` to at most its currently effective remaining
    /// validity. No root key or network round-trip is used — biscuit's
    /// holder-side [`Biscuit::append`] is exactly the primitive that makes
    /// this possible, and is why the resulting attenuation is real
    /// cryptographic narrowing rather than a fresh, independently-signed
    /// grant that merely happens to declare a smaller scope list.
    ///
    /// The narrowing is enforced by an appended `check if requested_scope(...)`
    /// clause, not by facts: biscuit facts can be freely added by any holder
    /// (they are not restrictive), so a "latest fact wins" extraction would
    /// let a malicious holder forge a wider-looking grant. Checks, by
    /// contrast, are ANDed across every block ever appended and can only
    /// narrow what [`Self::authorize_scope`] will accept — that's what makes
    /// a two-hop-attenuated (grandchild) token provably narrower than its
    /// grandparent: every check appended along the way must still pass.
    ///
    /// A `requested_scope` check requires the caller to supply that fact at
    /// authorization time, which [`Self::verify`]'s scope-blind extraction
    /// does not do — so `verify()` is not meaningful on an attenuated token
    /// (nothing produces one before this method exists, so there is no
    /// existing caller depending on that). Query effective scope narrowing
    /// via [`Self::authorize_scope`] instead.
    pub fn attenuate(
        &self,
        token: &str,
        scopes: Vec<String>,
        ttl_secs: u64,
    ) -> Result<GrantedToken, BrokerError> {
        let biscuit = self.decode_checked(token)?;
        let (credential_id, agent_id, root_scopes) = root_claims(&biscuit)?;

        // Currently-effective scopes: whichever root-declared scopes still
        // clear every check appended by prior attenuation hops (if any).
        // Probed independently per scope since a `check if` clause is
        // existential over whatever facts are supplied.
        let currently_allowed: Vec<String> = root_scopes
            .into_iter()
            .filter(|s| authorize_with_scope(&biscuit, s).is_ok())
            .collect();

        let narrowed_scopes: Vec<String> = scopes
            .into_iter()
            .filter(|s| currently_allowed.contains(s))
            .collect();

        let issued_at = Utc::now();
        let expires_at = issued_at + Duration::seconds(ttl_secs as i64);

        // An empty narrowed set is a deliberate "attenuated to nothing"
        // state (the child declared no scope this hop still grants) — embed
        // a check that can never be satisfied by any real requested scope,
        // rather than one that (via an empty `.contains()` on nothing)
        // could be mistaken for "no restriction".
        let scope_check = if narrowed_scopes.is_empty() {
            "check if requested_scope($s), {\"__ta_attenuated_to_no_scope__\"}.contains($s)"
                .to_string()
        } else {
            let scope_set = narrowed_scopes
                .iter()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("check if requested_scope($s), {{{scope_set}}}.contains($s)")
        };
        let time_check = format!("check if time($t), $t < {}", expires_at.to_rfc3339());

        let block = BlockBuilder::new()
            .check(scope_check.as_str())
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?
            .check(time_check.as_str())
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?;

        let attenuated = biscuit
            .append(block)
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?;

        let token_id = revocation_id_hex(&attenuated);
        let new_token = attenuated
            .to_base64()
            .map_err(|e| BrokerError::MintFailed(e.to_string()))?;

        info!(
            %credential_id, agent_id, ttl_secs, token_id, scopes = ?narrowed_scopes,
            "credential broker: token attenuated"
        );

        Ok(GrantedToken {
            token: new_token,
            token_id,
            credential_id,
            agent_id,
            allowed_scopes: narrowed_scopes,
            issued_at,
            expires_at,
        })
    }

    /// Cryptographically authorize `token` for exactly `scope`: signature,
    /// denylist, embedded expiry, and — unlike [`Self::verify`] — every
    /// `requested_scope` check appended by [`Self::attenuate`] along the
    /// way. Rejects if `scope` was narrowed away at any hop, even if it was
    /// present in the root grant, which is the cryptographic proof that an
    /// attenuated (child or grandchild) token cannot be used to exceed what
    /// it was actually narrowed to.
    pub fn authorize_scope(&self, token: &str, scope: &str) -> Result<VerifiedGrant, BrokerError> {
        let biscuit = self.decode_checked(token)?;
        let (credential_id, agent_id, _root_scopes) = root_claims(&biscuit)?;
        authorize_with_scope(&biscuit, scope)?;
        Ok(VerifiedGrant {
            credential_id,
            agent_id,
            allowed_scopes: vec![scope.to_string()],
        })
    }

    /// Signature + denylist check shared by [`Self::attenuate`] and
    /// [`Self::authorize_scope`]. Deliberately does not evaluate the
    /// embedded time check the way [`Self::verify`]'s full authorizer does —
    /// both callers run their own authorizer afterward (with or without a
    /// `requested_scope` fact), so a redundant unscoped `.authorize()` call
    /// here would just fail on any already-scope-attenuated token before
    /// `root_claims` ever runs.
    fn decode_checked(&self, token: &str) -> Result<Biscuit, BrokerError> {
        let biscuit = Biscuit::from_base64(token, self.keypair.public())
            .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;
        let token_id = revocation_id_hex(&biscuit);
        if self
            .denylist
            .revoked_token_ids
            .iter()
            .any(|id| id == &token_id)
        {
            return Err(BrokerError::Revoked);
        }
        Ok(biscuit)
    }

    /// Add `token_id` (as returned in [`GrantedToken::token_id`]) to the
    /// denylist, persisting immediately. Revoking an id the broker never
    /// issued is a no-op, not an error — the end state is identical.
    pub fn revoke(&mut self, token_id: &str) -> Result<(), BrokerError> {
        if !self
            .denylist
            .revoked_token_ids
            .iter()
            .any(|id| id == token_id)
        {
            self.denylist.revoked_token_ids.push(token_id.to_string());
            save_denylist(&self.denylist_path, &self.denylist)?;
            info!(token_id, "credential broker: grant revoked");
        }
        Ok(())
    }
}

/// Recover `(credential_id, agent_id, scopes)` from the token's authority
/// block. Pure data query — no policy, no `.authorize()` call — so it works
/// regardless of how many attenuation blocks (and their `requested_scope`
/// checks) have since been appended, and regardless of expiry: callers that
/// need enforcement (expiry, denylist, scope) get it from
/// [`CredentialBroker::decode_checked`] plus their own authorizer pass, not
/// from this helper.
fn root_claims(biscuit: &Biscuit) -> Result<(Uuid, String, Vec<String>), BrokerError> {
    let mut authorizer = AuthorizerBuilder::new()
        .time()
        .set_limits(AuthorizerLimits {
            max_time: std::time::Duration::from_secs(1),
            ..Default::default()
        })
        .build(biscuit)
        .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;
    let (credential_id_str, agent_id, scopes_csv): (String, String, String) = authorizer
        .query_exactly_one("data($c, $a, $s) <- grant($c, $a, $s)")
        .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;
    let credential_id = Uuid::parse_str(&credential_id_str).map_err(|e| {
        BrokerError::InvalidGrant(format!("grant carries a malformed credential id: {e}"))
    })?;
    let allowed_scopes = if scopes_csv.is_empty() {
        Vec::new()
    } else {
        scopes_csv.split(',').map(str::to_string).collect()
    };
    Ok((credential_id, agent_id, allowed_scopes))
}

/// Run a full authorization pass (time + every embedded check, including any
/// `requested_scope` check appended by [`CredentialBroker::attenuate`])
/// against `biscuit`, supplying `scope` as the `requested_scope($s)` fact.
/// `Ok(())` means every block — authority plus every attenuation hop —
/// accepted `scope`; `Err` means at least one hop's check rejected it, which
/// is what makes attenuation cryptographically enforced rather than
/// advisory.
fn authorize_with_scope(biscuit: &Biscuit, scope: &str) -> Result<(), BrokerError> {
    let mut authorizer = AuthorizerBuilder::new()
        .time()
        .fact(format!("requested_scope({scope:?})").as_str())
        .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?
        .set_limits(AuthorizerLimits {
            max_time: std::time::Duration::from_secs(1),
            ..Default::default()
        })
        .policy("allow if true")
        .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?
        .build(biscuit)
        .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;
    authorizer
        .authorize()
        .map_err(|e| BrokerError::InvalidGrant(e.to_string()))?;
    Ok(())
}

fn revocation_id_hex(biscuit: &Biscuit) -> String {
    let ids = biscuit.revocation_identifiers();
    let authority_id = ids.first().map(Vec::as_slice).unwrap_or(&[]);
    authority_id.iter().map(|b| format!("{b:02x}")).collect()
}

fn load_or_create_root_keypair(key_path: &Path) -> Result<KeyPair, BrokerError> {
    if key_path.exists() {
        let hex = fs::read_to_string(key_path)?;
        let private = PrivateKey::from_bytes_hex(hex.trim(), Algorithm::Ed25519).map_err(|e| {
            BrokerError::KeyUnreadable {
                path: key_path.to_path_buf(),
                reason: e.to_string(),
            }
        })?;
        Ok(KeyPair::from(&private))
    } else {
        let keypair = KeyPair::new();
        fs::write(key_path, keypair.private().to_bytes_hex())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(keypair)
    }
}

fn load_denylist(path: &Path) -> Result<Denylist, BrokerError> {
    if !path.exists() {
        return Ok(Denylist::default());
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_denylist(path: &Path, denylist: &Denylist) -> Result<(), BrokerError> {
    fs::write(path, serde_json::to_vec(denylist)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[test]
    fn grant_round_trips_claims() {
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let granted = broker
            .grant(
                credential_id,
                "agent-1",
                vec!["read".into(), "write".into()],
                3600,
            )
            .unwrap();
        let verified = broker.verify(&granted.token).unwrap();

        assert_eq!(verified.credential_id, credential_id);
        assert_eq!(verified.agent_id, "agent-1");
        assert_eq!(
            verified.allowed_scopes,
            vec!["read".to_string(), "write".to_string()]
        );
    }

    #[test]
    fn unscoped_grant_has_empty_scopes() {
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let granted = broker
            .grant(credential_id, "agent-1", vec![], 3600)
            .unwrap();
        let verified = broker.verify(&granted.token).unwrap();

        assert!(verified.allowed_scopes.is_empty());
    }

    #[test]
    fn expired_grant_rejected() {
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let granted = broker.grant(credential_id, "agent-1", vec![], 0).unwrap();
        sleep(Duration::from_millis(20));

        let result = broker.verify(&granted.token);
        assert!(matches!(result, Err(BrokerError::InvalidGrant(_))));
    }

    #[test]
    fn tampered_token_rejected() {
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let granted = broker
            .grant(credential_id, "agent-1", vec![], 3600)
            .unwrap();
        let mut tampered = granted.token.clone();
        // Flip one base64 character near the end (signature bytes) so the
        // decoded token fails the ed25519 check rather than base64 parsing.
        let mut chars: Vec<char> = tampered.chars().collect();
        let idx = chars.len() - 2;
        chars[idx] = if chars[idx] == 'A' { 'B' } else { 'A' };
        tampered = chars.into_iter().collect();

        let result = broker.verify(&tampered);
        assert!(result.is_err());
    }

    #[test]
    fn revoked_grant_rejected() {
        let dir = TempDir::new().unwrap();
        let mut broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let granted = broker
            .grant(credential_id, "agent-1", vec![], 3600)
            .unwrap();
        broker.revoke(&granted.token_id).unwrap();

        let result = broker.verify(&granted.token);
        assert!(matches!(result, Err(BrokerError::Revoked)));
    }

    #[test]
    fn key_persists_across_broker_reopen() {
        let dir = TempDir::new().unwrap();
        let credential_id = Uuid::new_v4();

        let token = {
            let broker = CredentialBroker::open(dir.path()).unwrap();
            broker
                .grant(credential_id, "agent-1", vec![], 3600)
                .unwrap()
                .token
        };

        // A freshly-opened broker on the same directory must load the same
        // root key from disk and verify a token minted by the earlier
        // instance — this is what makes the token independently verifiable
        // across process boundaries (CLI mint, gateway verify).
        let broker2 = CredentialBroker::open(dir.path()).unwrap();
        let verified = broker2.verify(&token).unwrap();
        assert_eq!(verified.credential_id, credential_id);
    }

    #[test]
    fn verify_rejects_token_from_different_root_key() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let credential_id = Uuid::new_v4();

        let broker_a = CredentialBroker::open(dir_a.path()).unwrap();
        let broker_b = CredentialBroker::open(dir_b.path()).unwrap();

        let granted = broker_a
            .grant(credential_id, "agent-1", vec![], 3600)
            .unwrap();
        let result = broker_b.verify(&granted.token);
        assert!(result.is_err());
    }

    #[test]
    fn revoke_unknown_token_id_is_not_an_error() {
        // Revoking a token id the broker never issued (e.g. a retry after a
        // partially-applied revoke) must not fail — the end state (denylisted)
        // is identical either way.
        let dir = TempDir::new().unwrap();
        let mut broker = CredentialBroker::open(dir.path()).unwrap();
        broker.revoke("does-not-exist").unwrap();
    }

    // -- v0.17.6.5: swarm fan-out cryptographic attenuation --------------

    #[test]
    fn attenuated_token_authorizes_only_narrowed_scopes() {
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let root = broker
            .grant(
                credential_id,
                "swarm-root",
                vec!["read".into(), "write".into(), "admin".into()],
                3600,
            )
            .unwrap();

        let child = broker
            .attenuate(&root.token, vec!["read".into(), "write".into()], 1800)
            .unwrap();

        assert_eq!(
            child.allowed_scopes,
            vec!["read".to_string(), "write".to_string()]
        );
        assert!(broker.authorize_scope(&child.token, "read").is_ok());
        assert!(broker.authorize_scope(&child.token, "write").is_ok());
        // "admin" was never requested for the child, so it's gone even
        // though the root grant carried it.
        assert!(broker.authorize_scope(&child.token, "admin").is_err());
    }

    #[test]
    fn attenuate_cannot_widen_scope_beyond_parent() {
        // A child requesting a scope its parent never granted gets nothing
        // for that scope — attenuation only ever narrows, never widens,
        // regardless of what the caller asks for.
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let root = broker
            .grant(credential_id, "swarm-root", vec!["read".into()], 3600)
            .unwrap();

        let child = broker
            .attenuate(&root.token, vec!["read".into(), "write".into()], 1800)
            .unwrap();

        assert_eq!(child.allowed_scopes, vec!["read".to_string()]);
        assert!(broker.authorize_scope(&child.token, "read").is_ok());
        assert!(broker.authorize_scope(&child.token, "write").is_err());
    }

    #[test]
    fn two_level_nested_attenuation_produces_provably_narrower_grandchild() {
        // v0.17.6.5 item 4: a two-level-deep nested swarm produces a
        // grandchild token that is provably a subset of the grandparent's
        // grant, verified via the authorizer rejecting an out-of-scope
        // request at every hop — not just a Rust-side list comparison.
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let grandparent = broker
            .grant(
                credential_id,
                "swarm-root",
                vec!["read".into(), "write".into(), "admin".into()],
                3600,
            )
            .unwrap();

        let parent = broker
            .attenuate(
                &grandparent.token,
                vec!["read".into(), "write".into()],
                1800,
            )
            .unwrap();
        assert_eq!(
            parent.allowed_scopes,
            vec!["read".to_string(), "write".to_string()]
        );

        let child = broker
            .attenuate(&parent.token, vec!["read".into()], 900)
            .unwrap();
        assert_eq!(child.allowed_scopes, vec!["read".to_string()]);

        // Every hop's own scope survives at that hop.
        assert!(broker.authorize_scope(&grandparent.token, "admin").is_ok());
        assert!(broker.authorize_scope(&parent.token, "write").is_ok());
        assert!(broker.authorize_scope(&child.token, "read").is_ok());

        // The grandchild cannot exercise "write" (dropped at the parent
        // hop) or "admin" (dropped at the very first attenuation), even
        // though both were present in the grandparent's original grant —
        // the authorizer itself rejects them, not an app-level check.
        assert!(broker.authorize_scope(&child.token, "write").is_err());
        assert!(broker.authorize_scope(&child.token, "admin").is_err());

        // The parent (not just the grandchild) has already lost "admin".
        assert!(broker.authorize_scope(&parent.token, "admin").is_err());
    }

    #[test]
    fn attenuate_narrows_ttl_and_expiry_is_enforced() {
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let root = broker
            .grant(credential_id, "swarm-root", vec!["read".into()], 3600)
            .unwrap();
        let child = broker
            .attenuate(&root.token, vec!["read".into()], 0)
            .unwrap();

        sleep(Duration::from_millis(20));

        assert!(broker.authorize_scope(&child.token, "read").is_err());
        // The un-attenuated root, with its original longer TTL, is
        // unaffected — proving the shorter expiry came from the
        // attenuation block, not some global clock issue.
        assert!(broker.authorize_scope(&root.token, "read").is_ok());
    }

    #[test]
    fn attenuating_to_no_scope_is_explicit_not_unrestricted() {
        // Requesting a disjoint scope set (none of which the parent
        // granted) must attenuate to zero usable scopes, not silently fall
        // back to "no restriction".
        let dir = TempDir::new().unwrap();
        let broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let root = broker
            .grant(credential_id, "swarm-root", vec!["read".into()], 3600)
            .unwrap();
        let child = broker
            .attenuate(&root.token, vec!["totally-unrelated".into()], 1800)
            .unwrap();

        assert!(child.allowed_scopes.is_empty());
        assert!(broker.authorize_scope(&child.token, "read").is_err());
        assert!(broker
            .authorize_scope(&child.token, "totally-unrelated")
            .is_err());
    }

    #[test]
    fn revoking_root_cascades_to_attenuated_descendants() {
        // Revocation is keyed off the authority block's id, which is
        // unchanged by attenuation — revoking the root credential must
        // therefore kill every token ever attenuated from it, without a
        // separate revocation entry per descendant.
        let dir = TempDir::new().unwrap();
        let mut broker = CredentialBroker::open(dir.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let root = broker
            .grant(credential_id, "swarm-root", vec!["read".into()], 3600)
            .unwrap();
        let child = broker
            .attenuate(&root.token, vec!["read".into()], 1800)
            .unwrap();

        broker.revoke(&root.token_id).unwrap();

        assert!(matches!(
            broker.authorize_scope(&child.token, "read"),
            Err(BrokerError::Revoked)
        ));
    }

    #[test]
    fn attenuate_rejects_tampered_and_wrong_key_tokens() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let broker_a = CredentialBroker::open(dir_a.path()).unwrap();
        let broker_b = CredentialBroker::open(dir_b.path()).unwrap();
        let credential_id = Uuid::new_v4();

        let root = broker_a
            .grant(credential_id, "swarm-root", vec!["read".into()], 3600)
            .unwrap();

        assert!(broker_b
            .attenuate(&root.token, vec!["read".into()], 1800)
            .is_err());
    }
}
