// vault.rs — Credential vault trait and core types.
//
// Agents must never hold raw credentials. TA acts as an identity broker —
// agents request access, TA provides scoped, short-lived session tokens.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::VaultError;

/// A stored credential (includes the secret — only returned by `get`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub service: String,
    pub secret: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Summary of a credential (no secret — used by `list`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialSummary {
    pub id: Uuid,
    pub name: String,
    pub service: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<&Credential> for CredentialSummary {
    fn from(cred: &Credential) -> Self {
        Self {
            id: cred.id,
            name: cred.name.clone(),
            service: cred.service.clone(),
            scopes: cred.scopes.clone(),
            created_at: cred.created_at,
            expires_at: cred.expires_at,
        }
    }
}

/// Scoped session token issued to an agent for time-limited access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionToken {
    pub token_id: Uuid,
    pub credential_id: Uuid,
    pub agent_id: String,
    pub allowed_scopes: Vec<String>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl SessionToken {
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

/// Pluggable credential storage backend.
pub trait CredentialVault: Send + Sync {
    /// Add a new credential to the vault.
    fn add(
        &mut self,
        name: &str,
        service: &str,
        secret: &str,
        scopes: Vec<String>,
    ) -> Result<Credential, VaultError>;

    /// List all credentials (without secrets).
    fn list(&self) -> Result<Vec<CredentialSummary>, VaultError>;

    /// Retrieve a credential by ID (includes secret).
    fn get(&self, id: Uuid) -> Result<Credential, VaultError>;

    /// Revoke (delete) a credential.
    fn revoke(&mut self, id: Uuid) -> Result<(), VaultError>;

    /// Issue a scoped, time-limited session token for an agent.
    fn issue_token(
        &mut self,
        credential_id: Uuid,
        agent_id: &str,
        scopes: Vec<String>,
        ttl_secs: u64,
    ) -> Result<SessionToken, VaultError>;

    /// Validate a session token (returns error if expired or not found).
    fn validate_token(&self, token_id: Uuid) -> Result<SessionToken, VaultError>;
}
