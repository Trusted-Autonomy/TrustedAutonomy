//! AuthMiddleware — plugin trait for request authentication and identity (v0.14.4/v0.14.5).
//!
//! ## Built-in implementations (v0.14.5)
//!
//! | Middleware | Config | Use case |
//! |---|---|---|
//! | [`NoopAuthMiddleware`] | (none) | Local single-user, no credentials |
//! | [`LocalIdentityMiddleware`] | `[[auth.users]]` in `daemon.toml` | Small teams without SSO |
//! | [`ApiKeyMiddleware`] | `[[auth.api_keys]]` in `daemon.toml` | CI pipelines, service accounts |
//!
//! Enterprise identity providers (OIDC, SAML, SCIM) are implemented by external
//! plugins that register against the [`AuthMiddleware`] trait. See `docs/plugin-traits.md`.
//!
//! ## Plugin registration
//!
//! ```toml
//! [plugins]
//! auth = "ta-auth-oidc"
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hex-encode bytes as a lowercase string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Opaque identity returned by a successful authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    /// Stable user identifier (e.g. email, UUID, username).
    pub user_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Role memberships (used for authorization decisions).
    #[serde(default)]
    pub roles: Vec<String>,
    /// Whether this identity was authenticated via a local bypass
    /// (i.e., no actual credential check was performed).
    #[serde(default)]
    pub local_bypass: bool,
}

impl Identity {
    /// Create a local-bypass identity (used by [`NoopAuthMiddleware`]).
    pub fn local() -> Self {
        Self {
            user_id: "local".to_string(),
            display_name: "Local User".to_string(),
            roles: vec!["admin".to_string()],
            local_bypass: true,
        }
    }
}

/// Minimal request metadata passed to the auth middleware.
///
/// Intentionally transport-agnostic: works for HTTP headers, MCP handshake
/// fields, or any other request type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthRequest {
    /// `Authorization` header value (e.g. `"Bearer ta_abc123"`).
    pub authorization: Option<String>,
    /// Remote IP address (for logging and IP allowlisting).
    pub remote_addr: Option<String>,
    /// The action being attempted (e.g. `"draft.approve"`, `"goal.start"`).
    pub action: String,
}

/// Session metadata returned alongside an `Identity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// How this session was authenticated.
    pub auth_method: String,
    /// When this session expires (ISO-8601), if applicable.
    pub expires_at: Option<String>,
}

/// Authentication errors.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Missing credentials: {0}")]
    MissingCredentials(String),

    #[error("Invalid credentials: {0}")]
    InvalidCredentials(String),

    #[error("Access denied: {0}")]
    AccessDenied(String),

    #[error("Auth backend unavailable: {0}")]
    BackendUnavailable(String),
}

/// Plugin trait for request authentication and identity.
///
/// Implement this to integrate TA with an external identity provider.
///
/// The daemon calls [`authenticate`](AuthMiddleware::authenticate) on every
/// incoming request. If authentication succeeds, the returned [`Identity`] is
/// attached to the request context and made available to policy decisions.
///
/// # Stability contract (v0.14.4)
///
/// This interface is **stable**. SA plugins implement this trait against the
/// v0.14.4 release and expect no breaking changes without a major version bump.
/// Additions (new optional methods) are permitted via default impls.
#[async_trait]
pub trait AuthMiddleware: Send + Sync {
    /// Name for logging and diagnostics (e.g., `"oidc"`, `"api-key"`, `"noop"`).
    fn name(&self) -> &str;

    /// Authenticate the request and return an identity.
    ///
    /// Return `Err(AuthError::MissingCredentials)` when no credentials are
    /// present (e.g., no `Authorization` header). The daemon may treat this
    /// as a local-bypass situation depending on its `[auth]` config.
    async fn authenticate(&self, request: &AuthRequest) -> Result<Identity, AuthError>;

    /// Authorize an already-authenticated identity to perform an action.
    ///
    /// `action` is a dot-separated string like `"draft.approve"` or
    /// `"goal.start"`. Return `Ok(false)` to deny without an error.
    async fn authorize(&self, identity: &Identity, action: &str) -> Result<bool, AuthError>;

    /// Return session metadata for an identity (for `/api/status` responses).
    fn session_info(&self, identity: &Identity) -> SessionInfo {
        SessionInfo {
            auth_method: self.name().to_string(),
            expires_at: identity.local_bypass.then(|| "never".to_string()),
        }
    }
}

/// Default auth middleware — no-op for local single-user deployments.
///
/// Every request is granted the `local` identity with full admin rights.
/// Appropriate when the daemon is only accessible on `127.0.0.1` and
/// no token is required.
pub struct NoopAuthMiddleware;

#[async_trait]
impl AuthMiddleware for NoopAuthMiddleware {
    fn name(&self) -> &str {
        "noop"
    }

    async fn authenticate(&self, _request: &AuthRequest) -> Result<Identity, AuthError> {
        Ok(Identity::local())
    }

    async fn authorize(&self, _identity: &Identity, _action: &str) -> Result<bool, AuthError> {
        Ok(true)
    }

    fn session_info(&self, identity: &Identity) -> SessionInfo {
        SessionInfo {
            auth_method: "noop".to_string(),
            expires_at: identity.local_bypass.then(|| "never".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// LocalIdentityMiddleware — v0.14.5
// ---------------------------------------------------------------------------

/// A user entry defined in `daemon.toml` under `[[auth.users]]`.
///
/// ```toml
/// [[auth.users]]
/// user_id = "alice"
/// display_name = "Alice Smith"
/// roles = ["admin"]
/// token_hash = "sha256:<hex-encoded SHA-256 of the bearer token>"
/// ```
///
/// Generate a `token_hash` with: `echo -n "ta_mytoken" | sha256sum`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalUserEntry {
    /// Stable identifier (e.g., email, username).
    pub user_id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Role memberships (e.g., `["admin"]`, `["read", "write"]`).
    #[serde(default)]
    pub roles: Vec<String>,
    /// SHA-256 hash of the bearer token, prefixed with `"sha256:"`.
    ///
    /// The user presents the raw token as `Authorization: Bearer <token>`.
    /// TA hashes the raw token and compares it to this stored hash.
    /// Leave empty to disable token-based login for this user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_hash: Option<String>,
}

/// Auth middleware that validates identities against `[[auth.users]]` in
/// `daemon.toml`. No network calls — suitable for single-user and small-team
/// setups without an external SSO provider.
///
/// Authentication:
/// - If `Authorization: Bearer <token>` is present, the token is SHA-256
///   hashed and compared against each user's `token_hash` entry.
/// - If no authorization header is present, `Err(MissingCredentials)` is
///   returned so the daemon can apply its configured `local_bypass` policy.
pub struct LocalIdentityMiddleware {
    users: Vec<LocalUserEntry>,
}

impl LocalIdentityMiddleware {
    /// Create middleware from a list of user entries (parsed from `daemon.toml`).
    pub fn new(users: Vec<LocalUserEntry>) -> Self {
        Self { users }
    }

    /// Hash a raw token string using SHA-256 and return `"sha256:<hex>"`.
    pub fn hash_token(raw: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(raw.as_bytes());
        format!("sha256:{}", hex_encode(&hasher.finalize()))
    }
}

#[async_trait]
impl AuthMiddleware for LocalIdentityMiddleware {
    fn name(&self) -> &str {
        "local-identity"
    }

    async fn authenticate(&self, request: &AuthRequest) -> Result<Identity, AuthError> {
        let bearer = match &request.authorization {
            Some(h) if h.starts_with("Bearer ") => h[7..].trim(),
            Some(_) => {
                return Err(AuthError::InvalidCredentials(
                    "Authorization header must use Bearer scheme".to_string(),
                ));
            }
            None => {
                return Err(AuthError::MissingCredentials(
                    "No Authorization header present".to_string(),
                ));
            }
        };

        let token_hash = Self::hash_token(bearer);

        for user in &self.users {
            if user.token_hash.as_deref() == Some(&token_hash) {
                return Ok(Identity {
                    user_id: user.user_id.clone(),
                    display_name: user.display_name.clone(),
                    roles: user.roles.clone(),
                    local_bypass: false,
                });
            }
        }

        Err(AuthError::InvalidCredentials(
            "Bearer token does not match any configured user".to_string(),
        ))
    }

    async fn authorize(&self, identity: &Identity, _action: &str) -> Result<bool, AuthError> {
        // Admin role can perform any action; non-admin can read but not write.
        // Fine-grained action-level ACL is a future extension.
        Ok(identity.roles.contains(&"admin".to_string()) || !identity.roles.is_empty())
    }

    fn session_info(&self, _identity: &Identity) -> SessionInfo {
        SessionInfo {
            auth_method: "local-identity".to_string(),
            expires_at: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ApiKeyMiddleware — v0.14.5
// ---------------------------------------------------------------------------

/// An API key entry defined in `daemon.toml` under `[[auth.api_keys]]`.
///
/// ```toml
/// [[auth.api_keys]]
/// label = "ci-pipeline"
/// user_id = "ci"
/// roles = ["read", "write"]
/// key_hash = "sha256:<hex-encoded SHA-256 of the raw key>"
/// ```
///
/// Keys must start with the prefix `ta_key_` (e.g., `ta_key_abc123xyz`).
/// Generate a `key_hash` with: `echo -n "ta_key_yourkey" | sha256sum`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyEntry {
    /// Human-readable label for this key (e.g., `"ci-pipeline"`, `"monitoring"`).
    pub label: String,
    /// User identity to assign when this key authenticates.
    pub user_id: String,
    /// Role memberships granted to this key.
    #[serde(default)]
    pub roles: Vec<String>,
    /// SHA-256 hash of the raw API key, prefixed with `"sha256:"`.
    pub key_hash: String,
}

/// Auth middleware that validates `Authorization: Bearer ta_key_...` tokens
/// against hashed API key entries in `daemon.toml`. Suitable for CI pipelines
/// and service accounts that don't participate in interactive SSO flows.
///
/// Keys must have the `ta_key_` prefix to be considered by this middleware.
/// Requests with non-`ta_key_` tokens return `MissingCredentials` so other
/// middleware in a chain can handle them.
pub struct ApiKeyMiddleware {
    keys: Vec<ApiKeyEntry>,
}

impl ApiKeyMiddleware {
    /// Create middleware from a list of API key entries (parsed from `daemon.toml`).
    pub fn new(keys: Vec<ApiKeyEntry>) -> Self {
        Self { keys }
    }

    /// Hash a raw API key using SHA-256 and return `"sha256:<hex>"`.
    pub fn hash_key(raw: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(raw.as_bytes());
        format!("sha256:{}", hex_encode(&hasher.finalize()))
    }

    /// Verify whether `raw_key` hashes to `stored_hash`.
    pub fn verify_key(raw_key: &str, stored_hash: &str) -> bool {
        Self::hash_key(raw_key) == stored_hash
    }
}

#[async_trait]
impl AuthMiddleware for ApiKeyMiddleware {
    fn name(&self) -> &str {
        "api-key"
    }

    async fn authenticate(&self, request: &AuthRequest) -> Result<Identity, AuthError> {
        let bearer = match &request.authorization {
            Some(h) if h.starts_with("Bearer ") => h[7..].trim(),
            _ => {
                return Err(AuthError::MissingCredentials(
                    "No Bearer token present".to_string(),
                ));
            }
        };

        // Only handle ta_key_ prefixed tokens.
        if !bearer.starts_with("ta_key_") {
            return Err(AuthError::MissingCredentials(
                "Not an API key (must start with ta_key_)".to_string(),
            ));
        }

        let key_hash = Self::hash_key(bearer);

        for entry in &self.keys {
            if entry.key_hash == key_hash {
                return Ok(Identity {
                    user_id: entry.user_id.clone(),
                    display_name: entry.label.clone(),
                    roles: entry.roles.clone(),
                    local_bypass: false,
                });
            }
        }

        Err(AuthError::InvalidCredentials(
            "API key does not match any configured entry".to_string(),
        ))
    }

    async fn authorize(&self, identity: &Identity, _action: &str) -> Result<bool, AuthError> {
        Ok(!identity.roles.is_empty())
    }

    fn session_info(&self, _identity: &Identity) -> SessionInfo {
        SessionInfo {
            auth_method: "api-key".to_string(),
            expires_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_middleware_name() {
        assert_eq!(NoopAuthMiddleware.name(), "noop");
    }

    #[tokio::test]
    async fn noop_authenticate_returns_local_identity() {
        let req = AuthRequest {
            authorization: None,
            remote_addr: None,
            action: "goal.start".to_string(),
        };
        let identity = NoopAuthMiddleware.authenticate(&req).await.unwrap();
        assert_eq!(identity.user_id, "local");
        assert!(identity.local_bypass);
        assert!(identity.roles.contains(&"admin".to_string()));
    }

    #[tokio::test]
    async fn noop_authorize_always_true() {
        let identity = Identity::local();
        let result = NoopAuthMiddleware
            .authorize(&identity, "draft.approve")
            .await
            .unwrap();
        assert!(result);
    }

    #[test]
    fn session_info_noop_never_expires() {
        let identity = Identity::local();
        let info = NoopAuthMiddleware.session_info(&identity);
        assert_eq!(info.auth_method, "noop");
        assert_eq!(info.expires_at.as_deref(), Some("never"));
    }

    // --- LocalIdentityMiddleware tests ---

    fn make_local_middleware() -> LocalIdentityMiddleware {
        let token = "ta_testtoken123";
        let hash = LocalIdentityMiddleware::hash_token(token);
        LocalIdentityMiddleware::new(vec![LocalUserEntry {
            user_id: "alice".to_string(),
            display_name: "Alice Smith".to_string(),
            roles: vec!["admin".to_string()],
            token_hash: Some(hash),
        }])
    }

    #[tokio::test]
    async fn local_identity_valid_token_authenticates() {
        let mw = make_local_middleware();
        let req = AuthRequest {
            authorization: Some("Bearer ta_testtoken123".to_string()),
            remote_addr: None,
            action: "goal.start".to_string(),
        };
        let identity = mw.authenticate(&req).await.unwrap();
        assert_eq!(identity.user_id, "alice");
        assert_eq!(identity.display_name, "Alice Smith");
        assert!(identity.roles.contains(&"admin".to_string()));
        assert!(!identity.local_bypass);
    }

    #[tokio::test]
    async fn local_identity_invalid_token_rejected() {
        let mw = make_local_middleware();
        let req = AuthRequest {
            authorization: Some("Bearer ta_wrongtoken".to_string()),
            remote_addr: None,
            action: "goal.start".to_string(),
        };
        let result = mw.authenticate(&req).await;
        assert!(matches!(result, Err(AuthError::InvalidCredentials(_))));
    }

    #[tokio::test]
    async fn local_identity_no_header_returns_missing_credentials() {
        let mw = make_local_middleware();
        let req = AuthRequest {
            authorization: None,
            remote_addr: None,
            action: "goal.start".to_string(),
        };
        let result = mw.authenticate(&req).await;
        assert!(matches!(result, Err(AuthError::MissingCredentials(_))));
    }

    #[test]
    fn local_identity_hash_token_is_deterministic() {
        let h1 = LocalIdentityMiddleware::hash_token("ta_foo");
        let h2 = LocalIdentityMiddleware::hash_token("ta_foo");
        assert_eq!(h1, h2);
        assert!(h1.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn local_identity_authorize_admin_role() {
        let mw = make_local_middleware();
        let identity = Identity {
            user_id: "alice".to_string(),
            display_name: "Alice Smith".to_string(),
            roles: vec!["admin".to_string()],
            local_bypass: false,
        };
        let result = mw.authorize(&identity, "draft.approve").await.unwrap();
        assert!(result);
    }

    #[test]
    fn local_identity_session_info() {
        let mw = make_local_middleware();
        let identity = Identity::local();
        let info = mw.session_info(&identity);
        assert_eq!(info.auth_method, "local-identity");
        assert!(info.expires_at.is_none());
    }

    // --- ApiKeyMiddleware tests ---

    fn make_api_key_middleware() -> ApiKeyMiddleware {
        let raw_key = "ta_key_citoken456";
        let hash = ApiKeyMiddleware::hash_key(raw_key);
        ApiKeyMiddleware::new(vec![ApiKeyEntry {
            label: "ci-pipeline".to_string(),
            user_id: "ci".to_string(),
            roles: vec!["read".to_string(), "write".to_string()],
            key_hash: hash,
        }])
    }

    #[tokio::test]
    async fn api_key_valid_key_authenticates() {
        let mw = make_api_key_middleware();
        let req = AuthRequest {
            authorization: Some("Bearer ta_key_citoken456".to_string()),
            remote_addr: None,
            action: "draft.approve".to_string(),
        };
        let identity = mw.authenticate(&req).await.unwrap();
        assert_eq!(identity.user_id, "ci");
        assert_eq!(identity.display_name, "ci-pipeline");
        assert!(identity.roles.contains(&"read".to_string()));
        assert!(!identity.local_bypass);
    }

    #[tokio::test]
    async fn api_key_invalid_key_rejected() {
        let mw = make_api_key_middleware();
        let req = AuthRequest {
            authorization: Some("Bearer ta_key_wrongkey".to_string()),
            remote_addr: None,
            action: "draft.approve".to_string(),
        };
        let result = mw.authenticate(&req).await;
        assert!(matches!(result, Err(AuthError::InvalidCredentials(_))));
    }

    #[tokio::test]
    async fn api_key_non_ta_key_returns_missing() {
        let mw = make_api_key_middleware();
        let req = AuthRequest {
            authorization: Some("Bearer usertoken999".to_string()),
            remote_addr: None,
            action: "goal.start".to_string(),
        };
        let result = mw.authenticate(&req).await;
        assert!(matches!(result, Err(AuthError::MissingCredentials(_))));
    }

    #[test]
    fn api_key_verify_key_matches() {
        let raw = "ta_key_mykey";
        let hash = ApiKeyMiddleware::hash_key(raw);
        assert!(ApiKeyMiddleware::verify_key(raw, &hash));
        assert!(!ApiKeyMiddleware::verify_key("ta_key_other", &hash));
    }

    #[test]
    fn api_key_session_info() {
        let mw = make_api_key_middleware();
        let identity = Identity::local();
        let info = mw.session_info(&identity);
        assert_eq!(info.auth_method, "api-key");
        assert!(info.expires_at.is_none());
    }
}
