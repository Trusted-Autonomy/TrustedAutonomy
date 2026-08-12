// file_vault.rs — Filesystem-backed credential vault.
//
// Stores credentials as an age-encrypted blob at the configured vault path
// (v0.17.6.2). The age identity is held in the OS keychain where available,
// falling back to a chmod-0600 key file (see `encryption::load_or_create_identity`).
// File permissions on the vault itself remain owner-only (0600) as a second
// layer of protection. Vaults written by pre-v0.17.6.2 builds (plaintext
// JSON) are transparently migrated to encrypted-at-rest on first open.

use std::fs;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use uuid::Uuid;

use crate::config::CredentialsConfig;
use crate::encryption::{self, KeyCustody};
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
/// Credentials are stored as an age-encrypted blob (v0.17.6.2). File
/// permissions on the vault file additionally restrict access to the current
/// user. Session tokens are stored alongside credentials and cleaned up on
/// validation.
pub struct FileVault {
    vault_path: PathBuf,
    data: VaultData,
    identity: age::x25519::Identity,
    key_custody: KeyCustody,
}

impl FileVault {
    /// Open or create a vault at the configured path.
    ///
    /// A pre-v0.17.6.2 plaintext-JSON vault is transparently decoded and
    /// re-saved as encrypted-at-rest on this call.
    pub fn open(config: &CredentialsConfig) -> Result<Self, VaultError> {
        let vault_path = config.vault_path.clone();
        let vault_dir = vault_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let (identity, key_custody) =
            encryption::load_or_create_identity(&vault_dir, config.use_keychain)?;

        let (data, needs_migration) = if vault_path.exists() {
            debug!(?vault_path, "loading existing vault");
            let raw = fs::read(&vault_path)?;
            if encryption::looks_like_plaintext_json(&raw) {
                info!(
                    ?vault_path,
                    "migrating legacy plaintext vault to encrypted-at-rest"
                );
                let content = String::from_utf8(raw).map_err(|e| {
                    VaultError::Other(format!(
                        "legacy plaintext vault at {} is not valid UTF-8: {e}",
                        vault_path.display()
                    ))
                })?;
                (serde_json::from_str(&content)?, true)
            } else {
                let plaintext = encryption::decrypt(&identity, &vault_path, &raw)?;
                (serde_json::from_slice(&plaintext)?, false)
            }
        } else {
            debug!(?vault_path, "creating new empty vault");
            (VaultData::default(), false)
        };

        let vault = Self {
            vault_path,
            data,
            identity,
            key_custody,
        };
        if needs_migration {
            vault.save()?;
        }
        Ok(vault)
    }

    /// Which key custody backend is protecting this vault's encryption key.
    pub fn key_custody(&self) -> &KeyCustody {
        &self.key_custody
    }

    /// Persist vault state to disk, encrypted to the vault's age identity.
    fn save(&self) -> Result<(), VaultError> {
        if let Some(parent) = self.vault_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let plaintext = serde_json::to_vec(&self.data)?;
        let ciphertext = encryption::encrypt(&self.identity, &plaintext)?;
        fs::write(&self.vault_path, &ciphertext)?;

        // Set restrictive permissions (Unix only) — defense in depth on top
        // of the encryption itself.
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
            // Never touch the real OS keychain from tests — it's a
            // process/OS-global resource, not scoped to this tempdir, so
            // using it here would make tests interfere with each other and
            // with the developer's real keychain (and can hang on macOS
            // waiting for a permission prompt in a headless run).
            use_keychain: false,
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
    fn vault_file_is_encrypted_at_rest() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        let mut vault = FileVault::open(&config).unwrap();
        vault
            .add("test", "svc", "super-secret-value", vec![])
            .unwrap();

        let raw = fs::read(&config.vault_path).unwrap();
        assert!(!encryption::looks_like_plaintext_json(&raw));
        let raw_str = String::from_utf8_lossy(&raw);
        assert!(!raw_str.contains("super-secret-value"));
    }

    #[test]
    fn encrypted_vault_round_trips_across_opens() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        {
            let mut vault = FileVault::open(&config).unwrap();
            vault
                .add("test", "svc", "super-secret-value", vec!["read".into()])
                .unwrap();
        }

        let vault = FileVault::open(&config).unwrap();
        let creds = vault.list().unwrap();
        assert_eq!(creds.len(), 1);
        let full = vault.get(creds[0].id).unwrap();
        assert_eq!(full.secret, "super-secret-value");
        assert_eq!(
            *vault.key_custody(),
            KeyCustody::FallbackFile(dir.path().join(encryption::FALLBACK_KEY_FILENAME))
        );
    }

    #[test]
    fn legacy_plaintext_vault_is_migrated_to_encrypted() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        // Simulate a pre-v0.17.6.2 plaintext vault written directly to disk.
        let legacy = VaultData {
            credentials: vec![Credential {
                id: Uuid::new_v4(),
                name: "legacy".to_string(),
                service: "svc".to_string(),
                secret: "legacy-secret".to_string(),
                scopes: vec![],
                created_at: Utc::now(),
                expires_at: None,
            }],
            tokens: vec![],
        };
        fs::write(
            &config.vault_path,
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();
        assert!(encryption::looks_like_plaintext_json(
            &fs::read(&config.vault_path).unwrap()
        ));

        // Opening migrates it in place.
        let vault = FileVault::open(&config).unwrap();
        let creds = vault.list().unwrap();
        assert_eq!(creds.len(), 1);
        assert_eq!(vault.get(creds[0].id).unwrap().secret, "legacy-secret");

        let raw_after = fs::read(&config.vault_path).unwrap();
        assert!(!encryption::looks_like_plaintext_json(&raw_after));

        // And it's still readable (and still encrypted) on a subsequent open.
        let vault2 = FileVault::open(&config).unwrap();
        assert_eq!(vault2.list().unwrap().len(), 1);
    }

    #[test]
    fn missing_key_with_existing_encrypted_vault_produces_actionable_error_not_data_loss() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        {
            let mut vault = FileVault::open(&config).unwrap();
            vault.add("test", "svc", "secret", vec![]).unwrap();
        }
        let encrypted_bytes_before = fs::read(&config.vault_path).unwrap();

        // Simulate the key being lost (e.g. fallback key file deleted).
        let key_path = dir.path().join(encryption::FALLBACK_KEY_FILENAME);
        assert!(key_path.exists());
        fs::remove_file(&key_path).unwrap();

        // Re-opening generates a *new* key (file custody has no way to know
        // the old one is "missing" vs "never created"), which cannot decrypt
        // the existing ciphertext — this must fail loudly, not silently
        // return an empty vault or overwrite the encrypted file.
        let result = FileVault::open(&config);
        assert!(matches!(result, Err(VaultError::DecryptionFailed { .. })));

        // The original encrypted vault file must be untouched.
        let encrypted_bytes_after = fs::read(&config.vault_path).unwrap();
        assert_eq!(encrypted_bytes_before, encrypted_bytes_after);
    }

    #[test]
    fn corrupt_key_file_produces_actionable_error_not_data_loss() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        {
            let mut vault = FileVault::open(&config).unwrap();
            vault.add("test", "svc", "secret", vec![]).unwrap();
        }
        let encrypted_bytes_before = fs::read(&config.vault_path).unwrap();

        let key_path = dir.path().join(encryption::FALLBACK_KEY_FILENAME);
        fs::write(&key_path, "not-a-valid-age-identity").unwrap();

        let result = FileVault::open(&config);
        assert!(matches!(result, Err(VaultError::KeyUnreadable { .. })));

        let encrypted_bytes_after = fs::read(&config.vault_path).unwrap();
        assert_eq!(encrypted_bytes_before, encrypted_bytes_after);
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
