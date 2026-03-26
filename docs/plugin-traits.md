# TA Plugin Trait Reference

This document describes the stable extension surface that TA exposes to external plugins. Plugin authors implement these traits against a specific TA release and can rely on them not breaking without a major version bump.

Plugin binaries communicate with TA through the `[plugins]` section of `daemon.toml`:

```toml
[plugins]
# transport = "ta-transport-websocket"
# auth      = "ta-auth-oidc"
# workspace = "ta-workspace-s3"
# review_queue   = "ta-review-jira"
# audit_storage  = "ta-audit-splunk"
```

Each value is either a binary name (resolved from `$PATH` and `.ta/plugins/`) or an absolute path to a plugin binary.

---

## `AuthMiddleware` (stable since v0.14.5)

**Crate**: `ta-extension::auth`
**File**: `crates/ta-extension/src/auth.rs`

Authenticates HTTP API requests and MCP connections. The daemon calls `authenticate` on every incoming request. The returned `Identity` is attached to the request context and used for policy decisions and audit records.

### Interface

```rust
#[async_trait]
pub trait AuthMiddleware: Send + Sync {
    /// Name for logging and diagnostics (e.g., "oidc", "api-key", "noop").
    fn name(&self) -> &str;

    /// Authenticate the request and return an identity.
    ///
    /// Return `Err(AuthError::MissingCredentials)` when no credentials are
    /// present. The daemon may treat this as a local-bypass situation
    /// depending on its `[auth]` config.
    async fn authenticate(&self, request: &AuthRequest) -> Result<Identity, AuthError>;

    /// Authorize an already-authenticated identity to perform an action.
    ///
    /// `action` is a dot-separated string like "draft.approve" or "goal.start".
    /// Return `Ok(false)` to deny without an error.
    async fn authorize(&self, identity: &Identity, action: &str) -> Result<bool, AuthError>;

    /// Return session metadata for an identity (for /api/status responses).
    /// Provides a default implementation — override to add expiry or method info.
    fn session_info(&self, identity: &Identity) -> SessionInfo { ... }
}
```

### Key types

```rust
/// Opaque identity returned by a successful authentication.
pub struct Identity {
    pub user_id: String,       // Stable identifier (email, UUID, username)
    pub display_name: String,  // Human-readable name
    pub roles: Vec<String>,    // Role memberships
    pub local_bypass: bool,    // True if no credential check was performed
}

/// Minimal request metadata passed to the auth middleware.
pub struct AuthRequest {
    pub authorization: Option<String>, // "Authorization" header value
    pub remote_addr: Option<String>,   // Client IP address
    pub action: String,                // Action being attempted
}

/// Session metadata returned alongside an Identity.
pub struct SessionInfo {
    pub auth_method: String,     // How this session was authenticated
    pub expires_at: Option<String>, // ISO-8601 expiry, if applicable
}

pub enum AuthError {
    MissingCredentials(String),  // No credentials present
    InvalidCredentials(String),  // Credentials present but invalid
    AccessDenied(String),        // Auth ok, action denied
    BackendUnavailable(String),  // Cannot reach auth backend
}
```

### Built-in implementations (v0.14.5)

| Type | When to use |
|---|---|
| `NoopAuthMiddleware` | Local single-user, no credentials required (default) |
| `LocalIdentityMiddleware` | Small teams; users defined in `[[auth.users]]` |
| `ApiKeyMiddleware` | CI pipelines; keys defined in `[[auth.api_keys]]` |

### `LocalIdentityMiddleware`

Reads user identities from `[[auth.users]]` entries in `daemon.toml`. No network calls.

```toml
[[auth.users]]
user_id = "alice"
display_name = "Alice Smith"
roles = ["admin"]
token_hash = "sha256:<hex SHA-256 of the bearer token>"
```

Generate a `token_hash`:

```bash
echo -n "ta_mysecrettoken" | sha256sum
# Output: abc123...  -
# Use: sha256:abc123...
```

Middleware selection: when `[[auth.users]]` is non-empty, `LocalIdentityMiddleware` is used automatically unless `[[auth.api_keys]]` is also configured (in which case `ApiKeyMiddleware` takes precedence).

### `ApiKeyMiddleware`

Validates `Authorization: Bearer ta_key_...` tokens against hashed entries in `daemon.toml`. Suitable for CI pipelines and service accounts.

```toml
[[auth.api_keys]]
label = "ci-pipeline"
user_id = "ci"
roles = ["read", "write"]
key_hash = "sha256:<hex SHA-256 of the raw API key>"
```

Keys must start with `ta_key_`. Tokens without this prefix are passed through (returning `MissingCredentials`) so another middleware in a chain can handle them.

Generate a `key_hash`:

```bash
echo -n "ta_key_yourkey" | sha256sum
```

### Middleware selection

The daemon selects a built-in middleware at startup based on `daemon.toml`:

1. `[[auth.api_keys]]` configured → `ApiKeyMiddleware`
2. `[[auth.users]]` configured → `LocalIdentityMiddleware`
3. Neither → `NoopAuthMiddleware`

Set `plugins.auth = "ta-auth-oidc"` to replace the built-in with an external plugin.

### Stability contract

- **No breaking changes** to `AuthMiddleware`, `Identity`, `AuthRequest`, `SessionInfo`, or `AuthError` without a TA major version bump.
- **Additive changes** (new optional methods with default implementations) are permitted in minor versions.
- `session_info` is optional — the default implementation returns `auth_method = self.name()` and `expires_at = None`.

---

## `TransportBackend` (stable since v0.14.4)

**Crate**: `ta-extension::transport`

Provides a custom network transport for the MCP server. The default is local stdio/Unix socket/TCP.

See `crates/ta-extension/src/transport.rs` for the full interface.

---

## `WorkspaceBackend` (stable since v0.14.4)

**Crate**: `ta-extension::workspace`

Stores staging workspace copies. Default is local filesystem (`.ta/staging/`).

See `crates/ta-extension/src/workspace.rs` for the full interface.

---

## `ReviewQueueBackend` (stable since v0.14.4)

**Crate**: `ta-extension::review_queue`

Routes drafts to external review systems. Default is local queue (`.ta/review_queue/`).

See `crates/ta-extension/src/review_queue.rs` for the full interface.

---

## `AuditStorageBackend` (stable since v0.14.4)

**Crate**: `ta-extension::audit`

Stores audit log records. Default is local JSONL file (`.ta/audit.jsonl`).

See `crates/ta-extension/src/audit.rs` for the full interface.
