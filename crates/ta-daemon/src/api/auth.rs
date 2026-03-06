// api/auth.rs — Bearer token authentication middleware.

use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::config::TokenScope;

/// Authenticated caller identity attached to request extensions.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CallerIdentity {
    pub scope: TokenScope,
    pub label: Option<String>,
    pub is_local: bool,
}

/// Authentication middleware: checks Bearer token or local bypass.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let config = &state.daemon_config.auth;

    // Check if connection is local (127.0.0.1).
    let is_local = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().is_loopback())
        .unwrap_or(true); // Default to local if no connect info (e.g., tests).

    // Local bypass: skip auth for loopback connections.
    if is_local && config.local_bypass {
        request.extensions_mut().insert(CallerIdentity {
            scope: TokenScope::Admin,
            label: Some("local".to_string()),
            is_local: true,
        });
        return next.run(request).await;
    }

    // If auth is not required, grant read access.
    if !config.require_token {
        request.extensions_mut().insert(CallerIdentity {
            scope: TokenScope::Admin,
            label: None,
            is_local,
        });
        return next.run(request).await;
    }

    // Extract Bearer token from Authorization header.
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                [("WWW-Authenticate", "Bearer")],
                "Missing or invalid Authorization header",
            )
                .into_response();
        }
    };

    // Validate token.
    match state.token_store.validate(token) {
        Some(record) => {
            request.extensions_mut().insert(CallerIdentity {
                scope: record.scope,
                label: record.label,
                is_local,
            });
            next.run(request).await
        }
        None => (StatusCode::UNAUTHORIZED, "Invalid token").into_response(),
    }
}

/// Helper to require write scope on a handler.
pub fn require_write(identity: &CallerIdentity) -> Result<(), (StatusCode, &'static str)> {
    if identity.scope.allows_write() {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "Write scope required"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_identity_local_admin() {
        let id = CallerIdentity {
            scope: TokenScope::Admin,
            label: Some("local".into()),
            is_local: true,
        };
        assert!(id.scope.allows_write());
        assert!(id.scope.allows_admin());
    }

    #[test]
    fn require_write_read_scope_fails() {
        let id = CallerIdentity {
            scope: TokenScope::Read,
            label: None,
            is_local: false,
        };
        assert!(require_write(&id).is_err());
    }

    #[test]
    fn require_write_write_scope_ok() {
        let id = CallerIdentity {
            scope: TokenScope::Write,
            label: None,
            is_local: false,
        };
        assert!(require_write(&id).is_ok());
    }
}
