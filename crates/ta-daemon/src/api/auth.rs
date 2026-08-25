// api/auth.rs — Bearer token authentication middleware.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::config::{AuthConfig, TokenScope, TokenStore};

/// Authenticated caller identity attached to request extensions.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CallerIdentity {
    pub scope: TokenScope,
    pub label: Option<String>,
    pub is_local: bool,
}

/// Resolves the address `auth_middleware` should treat as "the caller".
///
/// `X-Forwarded-For` is trusted only when the raw TCP peer matches an entry
/// in `trusted_proxies` — otherwise any caller could self-report
/// `127.0.0.1` in the header and be treated as local from anywhere on the
/// internet. When `trusted_proxies` is empty (the default), this always
/// returns `peer` unchanged: zero behavior change for the common
/// no-reverse-proxy deployment (v0.17.11.4, TA-02).
///
/// This closes the specific bypass where a same-host reverse proxy — the
/// ordinary way to put TLS and a real domain in front of a plain-HTTP
/// service like this one — makes every request the daemon sees arrive from
/// loopback, defeating `local_bypass` for every request the proxy forwards
/// regardless of the real client's address.
pub(crate) fn resolve_caller_ip(
    peer: IpAddr,
    headers: &HeaderMap,
    trusted_proxies: &[String],
) -> IpAddr {
    let peer_is_trusted_proxy = trusted_proxies.iter().any(|p| {
        p.parse::<IpAddr>()
            .map(|trusted| trusted == peer)
            .unwrap_or(false)
    });
    if !peer_is_trusted_proxy {
        return peer;
    }
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .and_then(|s| s.parse::<IpAddr>().ok())
        // A trusted proxy that sent no (or a malformed) header fails safe
        // to its own address — a real, configured proxy IP, never silently
        // treated as "local" by falling through to some other default.
        .unwrap_or(peer)
}

/// Core auth decision, independent of `AppState` so any router with an
/// `AuthConfig` + `TokenStore` in hand can reuse the exact same logic
/// (v0.17.11.4, TA-07) rather than a second, drifted copy of it — the main
/// `auth_middleware` below and the legacy web UI's `serve_web_ui` both call
/// this.
pub(crate) fn authenticate(
    auth: &AuthConfig,
    token_store: &TokenStore,
    peer_ip: Option<IpAddr>,
    headers: &HeaderMap,
) -> Result<CallerIdentity, Box<Response>> {
    // Determine the caller's real address (see `resolve_caller_ip`), then
    // whether that's loopback. No connect info at all (e.g. some test
    // harnesses) is treated as local, matching pre-existing behavior.
    let is_local = match peer_ip {
        Some(peer) => resolve_caller_ip(peer, headers, &auth.trusted_proxies).is_loopback(),
        None => true,
    };

    // Local bypass: skip auth for loopback connections.
    if is_local && auth.local_bypass {
        return Ok(CallerIdentity {
            scope: TokenScope::Admin,
            label: Some("local".to_string()),
            is_local: true,
        });
    }

    // If auth is not required, grant read-only access — matching this
    // branch's own intent (v0.17.11.4, TA-01a: previously granted `Admin`
    // here, silently contradicting this comment and this codebase's own
    // documented `require_token = false` posture).
    if !auth.require_token {
        return Ok(CallerIdentity {
            scope: TokenScope::Read,
            label: None,
            is_local,
        });
    }

    // Extract Bearer token from Authorization header.
    let token = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return Err(Box::new(
                (
                    StatusCode::UNAUTHORIZED,
                    [("WWW-Authenticate", "Bearer")],
                    "Missing or invalid Authorization header",
                )
                    .into_response(),
            ));
        }
    };

    // Validate token.
    match token_store.validate(token) {
        Some(record) => Ok(CallerIdentity {
            scope: record.scope,
            label: record.label,
            is_local,
        }),
        None => Err(Box::new(
            (StatusCode::UNAUTHORIZED, "Invalid token").into_response(),
        )),
    }
}

/// Authentication middleware: checks Bearer token or local bypass.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    match authenticate(
        &state.daemon_config.auth,
        &state.token_store,
        peer_ip,
        request.headers(),
    ) {
        Ok(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(response) => *response,
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

    fn empty_token_store() -> (tempfile::TempDir, TokenStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path());
        (dir, store)
    }

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (k, v) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn require_token_false_and_not_local_grants_read_not_admin() {
        // v0.17.11.4, TA-01a regression test: this branch's own comment
        // says "grant read access" -- pin that the code actually does.
        let (_dir, store) = empty_token_store();
        let auth = AuthConfig {
            require_token: false,
            local_bypass: true,
            ..Default::default()
        };
        let identity = authenticate(
            &auth,
            &store,
            Some("203.0.113.9".parse().unwrap()),
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(identity.scope, TokenScope::Read);
        assert!(!identity.is_local);
    }

    #[test]
    fn local_bypass_still_grants_admin_for_a_direct_loopback_connection() {
        let (_dir, store) = empty_token_store();
        let auth = AuthConfig {
            require_token: false,
            local_bypass: true,
            ..Default::default()
        };
        let identity = authenticate(
            &auth,
            &store,
            Some("127.0.0.1".parse().unwrap()),
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(identity.scope, TokenScope::Admin);
        assert!(identity.is_local);
    }

    #[test]
    fn untrusted_peers_x_forwarded_for_is_ignored() {
        // A direct (non-proxy) caller claiming to be 127.0.0.1 via the
        // header must not be treated as local -- trusted_proxies is empty.
        let ip = resolve_caller_ip(
            "203.0.113.9".parse().unwrap(),
            &headers_with(&[("x-forwarded-for", "127.0.0.1")]),
            &[],
        );
        assert_eq!(ip, "203.0.113.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn trusted_proxys_x_forwarded_for_is_honored() {
        let trusted_proxies = vec!["127.0.0.1".to_string()];
        let ip = resolve_caller_ip(
            "127.0.0.1".parse().unwrap(),
            &headers_with(&[("x-forwarded-for", "198.51.100.7")]),
            &trusted_proxies,
        );
        assert_eq!(ip, "198.51.100.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn trusted_proxy_with_no_forwarded_header_fails_safe_to_its_own_address() {
        let trusted_proxies = vec!["127.0.0.1".to_string()];
        let ip = resolve_caller_ip(
            "127.0.0.1".parse().unwrap(),
            &HeaderMap::new(),
            &trusted_proxies,
        );
        assert_eq!(ip, "127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn a_same_host_reverse_proxy_no_longer_grants_blanket_local_bypass() {
        // The TA-02 scenario: without `trusted_proxies` configured, every
        // request forwarded by a same-host reverse proxy looks local and
        // gets the local_bypass Admin grant regardless of the real client.
        // With the proxy declared trusted, the real (remote) client's
        // address is used instead, and local_bypass no longer fires.
        let (_dir, store) = empty_token_store();
        let auth = AuthConfig {
            require_token: true,
            local_bypass: true,
            trusted_proxies: vec!["127.0.0.1".to_string()],
            ..Default::default()
        };
        let result = authenticate(
            &auth,
            &store,
            Some("127.0.0.1".parse().unwrap()),
            &headers_with(&[("x-forwarded-for", "198.51.100.7")]),
        );
        // Not local anymore, and no token was presented -> unauthorized,
        // not an automatic Admin bypass.
        assert!(result.is_err());
    }

    #[test]
    fn no_connect_info_defaults_to_local_unchanged() {
        let (_dir, store) = empty_token_store();
        let auth = AuthConfig::default();
        let identity = authenticate(&auth, &store, None, &HeaderMap::new()).unwrap();
        assert!(identity.is_local);
        assert_eq!(identity.scope, TokenScope::Admin);
    }
}
