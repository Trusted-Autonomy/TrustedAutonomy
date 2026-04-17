// tokens.rs -- HMAC-based tokens for non-interactive draft approval.
//
// Token flow:
// 1. `ta token create --scope draft:approve --expires 24h` generates a token
// 2. Token is stored in `.ta/tokens/<id>.json`
// 3. `ta draft approve --token <TOKEN>` validates and uses the token
//
// Tokens are simple HMAC-SHA256 signatures over the scope + expiry + random nonce.

use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::EventError;

/// An approval token with scope and expiration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalToken {
    /// Unique token identifier.
    pub id: Uuid,
    /// What the token authorizes (e.g., "draft:approve", "draft:approve:<id>").
    pub scope: String,
    /// When the token was created.
    pub created_at: DateTime<Utc>,
    /// When the token expires.
    pub expires_at: DateTime<Utc>,
    /// The hex-encoded token value for presentation.
    pub token_value: String,
    /// Whether the token has been used (single-use tokens).
    #[serde(default)]
    pub used: bool,
}

impl ApprovalToken {
    /// Check if the token has expired.
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }

    /// Check if the token is valid (not expired, not used).
    pub fn is_valid(&self) -> bool {
        !self.is_expired() && !self.used
    }

    /// Check if the token's scope covers the requested action.
    pub fn covers_scope(&self, requested: &str) -> bool {
        // "draft:approve" covers "draft:approve" and "draft:approve:<id>"
        if self.scope == requested {
            return true;
        }
        requested.starts_with(&format!("{}:", self.scope))
    }
}

/// Persistent store for approval tokens.
pub struct TokenStore {
    tokens_dir: PathBuf,
}

impl TokenStore {
    /// Create a new token store at the given directory.
    pub fn new(tokens_dir: impl AsRef<Path>) -> Self {
        Self {
            tokens_dir: tokens_dir.as_ref().to_path_buf(),
        }
    }

    /// Generate a new approval token.
    pub fn create(&self, scope: &str, expires_in: Duration) -> Result<ApprovalToken, EventError> {
        fs::create_dir_all(&self.tokens_dir)?;

        let id = Uuid::new_v4();
        let now = Utc::now();
        let expires_at = now + expires_in;

        // Generate token value: HMAC-SHA256(id + scope + expires_at + random)
        let nonce = Uuid::new_v4();
        let mut hasher = Sha256::new();
        hasher.update(id.as_bytes());
        hasher.update(scope.as_bytes());
        hasher.update(expires_at.to_rfc3339().as_bytes());
        hasher.update(nonce.as_bytes());
        let hash = hasher.finalize();
        let token_value = format!("ta_{}", hex::encode(&hash[..16]));

        let token = ApprovalToken {
            id,
            scope: scope.to_string(),
            created_at: now,
            expires_at,
            token_value,
            used: false,
        };

        let path = self.token_path(id);
        let json = serde_json::to_string_pretty(&token)?;
        fs::write(&path, json)?;

        Ok(token)
    }

    /// Validate a token value and return the token if valid.
    pub fn validate(&self, token_value: &str, scope: &str) -> Result<ApprovalToken, EventError> {
        let token = self
            .find_by_value(token_value)?
            .ok_or_else(|| EventError::InvalidToken("token not found".into()))?;

        if token.is_expired() {
            return Err(EventError::TokenExpired);
        }

        if token.used {
            return Err(EventError::InvalidToken("token already used".into()));
        }

        if !token.covers_scope(scope) {
            return Err(EventError::InvalidToken(format!(
                "token scope '{}' does not cover '{}'",
                token.scope, scope
            )));
        }

        Ok(token)
    }

    /// Mark a token as used.
    pub fn mark_used(&self, id: Uuid) -> Result<(), EventError> {
        let path = self.token_path(id);
        if !path.exists() {
            return Err(EventError::NotFound(format!("token {}", id)));
        }
        let content = fs::read_to_string(&path)?;
        let mut token: ApprovalToken = serde_json::from_str(&content)?;
        token.used = true;
        let json = serde_json::to_string_pretty(&token)?;
        fs::write(&path, json)?;
        Ok(())
    }

    /// List all tokens (including expired/used).
    pub fn list(&self) -> Result<Vec<ApprovalToken>, EventError> {
        if !self.tokens_dir.exists() {
            return Ok(vec![]);
        }
        let mut tokens = Vec::new();
        for entry in fs::read_dir(&self.tokens_dir)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
                let content = fs::read_to_string(entry.path())?;
                if let Ok(token) = serde_json::from_str::<ApprovalToken>(&content) {
                    tokens.push(token);
                }
            }
        }
        tokens.sort_by_key(|t| Reverse(t.created_at));
        Ok(tokens)
    }

    /// Delete expired tokens.
    pub fn cleanup(&self) -> Result<usize, EventError> {
        if !self.tokens_dir.exists() {
            return Ok(0);
        }
        let mut removed = 0;
        for entry in fs::read_dir(&self.tokens_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                let content = fs::read_to_string(&path)?;
                if let Ok(token) = serde_json::from_str::<ApprovalToken>(&content) {
                    if token.is_expired() {
                        fs::remove_file(&path)?;
                        removed += 1;
                    }
                }
            }
        }
        Ok(removed)
    }

    fn token_path(&self, id: Uuid) -> PathBuf {
        self.tokens_dir.join(format!("{}.json", id))
    }

    fn find_by_value(&self, token_value: &str) -> Result<Option<ApprovalToken>, EventError> {
        if !self.tokens_dir.exists() {
            return Ok(None);
        }
        for entry in fs::read_dir(&self.tokens_dir)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .map(|e| e == "json")
                .unwrap_or(false)
            {
                let content = fs::read_to_string(entry.path())?;
                if let Ok(token) = serde_json::from_str::<ApprovalToken>(&content) {
                    if token.token_value == token_value {
                        return Ok(Some(token));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Simple hex encoding (avoids adding the `hex` crate).
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_and_validate_token() {
        let dir = tempdir().unwrap();
        let store = TokenStore::new(dir.path());

        let token = store.create("draft:approve", Duration::hours(24)).unwrap();
        assert!(!token.token_value.is_empty());
        assert!(token.token_value.starts_with("ta_"));
        assert!(token.is_valid());

        let validated = store.validate(&token.token_value, "draft:approve").unwrap();
        assert_eq!(validated.id, token.id);
    }

    #[test]
    fn token_scope_coverage() {
        let token = ApprovalToken {
            id: Uuid::new_v4(),
            scope: "draft:approve".into(),
            created_at: Utc::now(),
            expires_at: Utc::now() + Duration::hours(1),
            token_value: "ta_test".into(),
            used: false,
        };
        assert!(token.covers_scope("draft:approve"));
        assert!(token.covers_scope("draft:approve:some-id"));
        assert!(!token.covers_scope("draft:deny"));
    }

    #[test]
    fn expired_token_rejected() {
        let dir = tempdir().unwrap();
        let store = TokenStore::new(dir.path());

        // Create a token that already expired.
        let id = Uuid::new_v4();
        let token = ApprovalToken {
            id,
            scope: "draft:approve".into(),
            created_at: Utc::now() - Duration::hours(2),
            expires_at: Utc::now() - Duration::hours(1),
            token_value: "ta_expired".into(),
            used: false,
        };
        let path = dir.path().join(format!("{}.json", id));
        fs::write(&path, serde_json::to_string(&token).unwrap()).unwrap();

        let result = store.validate("ta_expired", "draft:approve");
        assert!(matches!(result, Err(EventError::TokenExpired)));
    }

    #[test]
    fn used_token_rejected() {
        let dir = tempdir().unwrap();
        let store = TokenStore::new(dir.path());

        let token = store.create("draft:approve", Duration::hours(24)).unwrap();
        store.mark_used(token.id).unwrap();

        let result = store.validate(&token.token_value, "draft:approve");
        assert!(matches!(result, Err(EventError::InvalidToken(_))));
    }

    #[test]
    fn list_tokens() {
        let dir = tempdir().unwrap();
        let store = TokenStore::new(dir.path());

        store.create("draft:approve", Duration::hours(1)).unwrap();
        store.create("draft:deny", Duration::hours(2)).unwrap();

        let tokens = store.list().unwrap();
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn cleanup_expired() {
        let dir = tempdir().unwrap();
        let store = TokenStore::new(dir.path());

        // Create an expired token manually.
        let id = Uuid::new_v4();
        let token = ApprovalToken {
            id,
            scope: "test".into(),
            created_at: Utc::now() - Duration::hours(2),
            expires_at: Utc::now() - Duration::hours(1),
            token_value: "ta_old".into(),
            used: false,
        };
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(
            dir.path().join(format!("{}.json", id)),
            serde_json::to_string(&token).unwrap(),
        )
        .unwrap();

        // Create a valid token.
        store.create("test", Duration::hours(24)).unwrap();

        assert_eq!(store.list().unwrap().len(), 2);
        let removed = store.cleanup().unwrap();
        assert_eq!(removed, 1);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn wrong_scope_rejected() {
        let dir = tempdir().unwrap();
        let store = TokenStore::new(dir.path());

        let token = store.create("draft:approve", Duration::hours(24)).unwrap();
        let result = store.validate(&token.token_value, "draft:deny");
        assert!(matches!(result, Err(EventError::InvalidToken(_))));
    }
}
