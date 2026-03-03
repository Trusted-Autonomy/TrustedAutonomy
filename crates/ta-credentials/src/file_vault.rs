// file_vault.rs — Filesystem-backed credential vault.
//
// Stores credentials as JSON at the configured vault path.
// File permissions are set to owner-only (0600) for basic protection.
// Future: age encryption layer for at-rest encryption.

use std::fs;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use uuid::Uuid;

use crate::config::CredentialsConfig;
use crate::error::VaultError;
use crate::vault::{Credential, CredentialSummary, CredentialVault, SessionToken};

/// Persistent state stored in the vault file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct VaultData {
    credentials: Vec<Credential>,
    tokens: Vec<SessionToken>,
}

/// Filesystem-backed credential vault.
///
/// Credentials are stored as JSON. File permissions restrict access to the
/// current user. Session tokens are stored alongside credentials and cleaned
/// up on validation.
pub struct FileVault {
    vault_path: PathBuf,
    data: VaultData,
}

impl FileVault {
    /// Open or create a vault at the configured path.
    pub fn open(config: &CredentialsConfig) -> Result<Self, VaultError> {
        let vault_path = config.vault_path.clone();
        let data = if vault_path.exists() {
            debug!(?vault_path, "loading existing vault");
            let content = fs::read_to_string(&vault_path)?;
            serde_json::from_str(&content)?
        } else {
            debug!(?vault_path, "creating new empty vault");
            VaultData::default()
        };

        Ok(Self { vault_path, data })
    }

    /// Persist vault state to disk.
    fn save(&self) -> Result<(), VaultError> {
        if let Some(parent) = self.vault_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.vault_path, &content)?;

        // Set restrictive permissions (Unix only).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.vault_path, perms)?;
        }

        Ok(())
    }

    /// Remove expired tokens from the vault.
    fn gc_expired_tokens(&mut self) {
        let before = self.data.tokens.len();
        self.data.tokens.retain(|t| !t.is_expired());
        let removed = before - self.data.tokens.len();
        if removed > 0 {
            debug!(removed, "garbage-collected expired session tokens");
        }
    }
}

impl CredentialVault for FileVault {
    fn add(
        &mut self,
        name: &str,
        service: &str,
        secret: &str,
        scopes: Vec<String>,
    ) -> Result<Credential, VaultError> {
        // Check for duplicate name.
        if self.data.credentials.iter().any(|c| c.name == name) {
            return Err(VaultError::DuplicateName(name.to_string()));
        }

        let cred = Credential {
            id: Uuid::new_v4(),
            name: name.to_string(),
            service: service.to_string(),
            secret: secret.to_string(),
            scopes,
            created_at: Utc::now(),
            expires_at: None,
        };

        self.data.credentials.push(cred.clone());
        self.save()?;
        info!(name, service, "credential added to vault");
        Ok(cred)
    }

    fn list(&self) -> Result<Vec<CredentialSummary>, VaultError> {
        Ok(self
            .data
            .credentials
            .iter()
            .map(CredentialSummary::from)
            .collect())
    }

    fn get(&self, id: Uuid) -> Result<Credential, VaultError> {
        self.data
            .credentials
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or(VaultError::NotFound(id))
    }

    fn revoke(&mut self, id: Uuid) -> Result<(), VaultError> {
        let before = self.data.credentials.len();
        self.data.credentials.retain(|c| c.id != id);
        if self.data.credentials.len() == before {
            return Err(VaultError::NotFound(id));
        }
        // Also revoke any tokens for this credential.
        self.data.tokens.retain(|t| t.credential_id != id);
        self.save()?;
        info!(%id, "credential revoked");
        Ok(())
    }

    fn issue_token(
        &mut self,
        credential_id: Uuid,
        agent_id: &str,
        scopes: Vec<String>,
        ttl_secs: u64,
    ) -> Result<SessionToken, VaultError> {
        // Verify the credential exists.
        if !self.data.credentials.iter().any(|c| c.id == credential_id) {
            return Err(VaultError::NotFound(credential_id));
        }

        self.gc_expired_tokens();

        let now = Utc::now();
        let token = SessionToken {
            token_id: Uuid::new_v4(),
            credential_id,
            agent_id: agent_id.to_string(),
            allowed_scopes: scopes,
            issued_at: now,
            expires_at: now + Duration::seconds(ttl_secs as i64),
        };

        self.data.tokens.push(token.clone());
        self.save()?;
        info!(%credential_id, agent_id, ttl_secs, "session token issued");
        Ok(token)
    }

    fn validate_token(&self, token_id: Uuid) -> Result<SessionToken, VaultError> {
        let token = self
            .data
            .tokens
            .iter()
            .find(|t| t.token_id == token_id)
            .cloned()
            .ok_or(VaultError::TokenNotFound(token_id))?;

        if token.is_expired() {
            return Err(VaultError::TokenExpired(token_id));
        }

        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> CredentialsConfig {
        CredentialsConfig {
            vault_path: dir.path().join("vault.json"),
        }
    }

    #[test]
    fn add_and_list_credential() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let cred = vault
            .add("test-api", "test-service", "secret123", vec!["read".into()])
            .unwrap();
        assert_eq!(cred.name, "test-api");
        assert_eq!(cred.service, "test-service");

        let list = vault.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test-api");
    }

    #[test]
    fn get_credential_includes_secret() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let cred = vault.add("test", "svc", "my-secret", vec![]).unwrap();
        let retrieved = vault.get(cred.id).unwrap();
        assert_eq!(retrieved.secret, "my-secret");
    }

    #[test]
    fn get_nonexistent_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let vault = FileVault::open(&test_config(&dir)).unwrap();

        let result = vault.get(Uuid::new_v4());
        assert!(matches!(result, Err(VaultError::NotFound(_))));
    }

    #[test]
    fn revoke_credential() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let cred = vault.add("test", "svc", "secret", vec![]).unwrap();
        vault.revoke(cred.id).unwrap();
        assert!(vault.list().unwrap().is_empty());
    }

    #[test]
    fn revoke_nonexistent_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let result = vault.revoke(Uuid::new_v4());
        assert!(matches!(result, Err(VaultError::NotFound(_))));
    }

    #[test]
    fn duplicate_name_rejected() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        vault.add("dup", "svc", "secret1", vec![]).unwrap();
        let result = vault.add("dup", "svc", "secret2", vec![]);
        assert!(matches!(result, Err(VaultError::DuplicateName(_))));
    }

    #[test]
    fn issue_and_validate_token() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let cred = vault
            .add("test", "svc", "secret", vec!["read".into()])
            .unwrap();
        let token = vault
            .issue_token(cred.id, "agent-1", vec!["read".into()], 3600)
            .unwrap();

        let validated = vault.validate_token(token.token_id).unwrap();
        assert_eq!(validated.agent_id, "agent-1");
        assert_eq!(validated.credential_id, cred.id);
    }

    #[test]
    fn expired_token_rejected() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let cred = vault.add("test", "svc", "secret", vec![]).unwrap();
        // Issue with 0 TTL — immediately expired.
        let token = vault.issue_token(cred.id, "agent-1", vec![], 0).unwrap();

        let result = vault.validate_token(token.token_id);
        assert!(matches!(result, Err(VaultError::TokenExpired(_))));
    }

    #[test]
    fn token_for_nonexistent_credential_fails() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let result = vault.issue_token(Uuid::new_v4(), "agent-1", vec![], 3600);
        assert!(matches!(result, Err(VaultError::NotFound(_))));
    }

    #[test]
    fn vault_persists_across_opens() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        {
            let mut vault = FileVault::open(&config).unwrap();
            vault.add("persist-test", "svc", "secret", vec![]).unwrap();
        }

        let vault = FileVault::open(&config).unwrap();
        let list = vault.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "persist-test");
    }

    #[test]
    fn revoke_also_removes_tokens() {
        let dir = TempDir::new().unwrap();
        let mut vault = FileVault::open(&test_config(&dir)).unwrap();

        let cred = vault.add("test", "svc", "secret", vec![]).unwrap();
        let token = vault.issue_token(cred.id, "agent-1", vec![], 3600).unwrap();
        vault.revoke(cred.id).unwrap();

        let result = vault.validate_token(token.token_id);
        assert!(matches!(result, Err(VaultError::TokenNotFound(_))));
    }
}
