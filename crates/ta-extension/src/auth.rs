//! AuthMiddleware — plugin trait for request authentication and identity (v0.14.4).
//!
//! The default [`NoopAuthMiddleware`] treats every request as coming from
//! the local single user, which is correct for `ta-daemon` running on a
//! developer's laptop with no network exposure.
//!
//! Enterprise identity providers (OIDC, SAML, API keys, SCIM) are implemented
//! by external plugins that register against this trait. See v0.14.5 for
//! hardened built-in identity middleware (`ApiKeyMiddleware`,
//! `LocalIdentityMiddleware`).
//!
//! ## Plugin registration
//!
//! ```toml
//! [plugins]
//! auth = "ta-auth-oidc"
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
}
