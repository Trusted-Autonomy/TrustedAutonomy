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

use biscuit_auth::{Algorithm, AuthorizerBuilder, Biscuit, KeyPair, PrivateKey};
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

        let mut authorizer = AuthorizerBuilder::new()
            .time()
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
}
