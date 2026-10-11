// api/auth.rs — Bearer token authentication middleware.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::AppState;
use crate::config::{AuthConfig, TokenScope, TokenStore};
use crate::human_gate::{self, HostPolicy, HumanGate, HumanProof, Refusal};

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
    authenticate_with(auth, token_store, peer_ip, headers, true)
}

/// `authenticate` with control over the loopback bypass. Gated (mutating)
/// routes pass `honor_local_bypass = false` so a loopback caller is judged like
/// any other: by token or by human credential, never just by being local.
pub(crate) fn authenticate_with(
    auth: &AuthConfig,
    token_store: &TokenStore,
    peer_ip: Option<IpAddr>,
    headers: &HeaderMap,
    honor_local_bypass: bool,
) -> Result<CallerIdentity, Box<Response>> {
    // Determine the caller's real address (see `resolve_caller_ip`), then
    // whether that's loopback. No connect info at all (e.g. some test
    // harnesses) is treated as local, matching pre-existing behavior.
    let is_local = match peer_ip {
        Some(peer) => resolve_caller_ip(peer, headers, &auth.trusted_proxies).is_loopback(),
        None => true,
    };

    // Local bypass: skip auth for loopback connections.
    if is_local && auth.local_bypass && honor_local_bypass {
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

/// Everything one request's auth decision needs. Borrowed from `AppState` (or
/// the legacy listener's smaller state) so both middlewares share one
/// implementation instead of two drifting copies.
pub(crate) struct RequestAuth<'a> {
    pub auth: &'a AuthConfig,
    pub tokens: &'a TokenStore,
    pub gate: &'a HumanGate,
    pub hosts: &'a HostPolicy,
}

fn refusal_response(status: StatusCode, r: &Refusal, setting: Option<&str>) -> Response {
    let mut body = serde_json::json!({ "error": r.error, "hint": r.hint });
    if let Some(s) = setting {
        body["setting"] = serde_json::Value::String(s.to_string());
    }
    (status, axum::Json(body)).into_response()
}

/// The full per-request decision: Host allowlist, then bearer/local auth, then
/// (for gated routes) proof of a human.
pub(crate) fn authenticate_request(
    ra: &RequestAuth<'_>,
    method: &axum::http::Method,
    path: &str,
    peer_ip: Option<IpAddr>,
    headers: &HeaderMap,
) -> Result<(CallerIdentity, Option<HumanProof>), Box<Response>> {
    // CR-36: refuse Host headers that are not the daemon's own names.
    let host = headers.get("host").and_then(|v| v.to_str().ok());
    if let Err(r) = ra.hosts.check(host) {
        tracing::warn!(host = ?host, path, "request refused: Host header not allowed");
        return Err(Box::new(refusal_response(
            StatusCode::FORBIDDEN,
            &r,
            Some("[server] allowed_hosts"),
        )));
    }

    let route = match human_gate::classify(method, path) {
        // Reads, agent-coordination routes and self-authenticating routes keep
        // normal bearer / loopback authentication. The self-authenticating
        // handler then demands the credential or a signed assertion itself.
        human_gate::RouteClass::Safe
        | human_gate::RouteClass::AgentAllowed(_)
        | human_gate::RouteClass::SelfAuthenticating(_) => {
            return authenticate(ra.auth, ra.tokens, peer_ip, headers).map(|id| (id, None));
        }
        human_gate::RouteClass::Gated(route) => route,
    };

    // A gated route: a valid human credential is enough by itself (no bearer
    // token needed on loopback).
    if let Some(proof) = ra.gate.credential_proof(headers) {
        let is_local = peer_ip
            .map(|p| resolve_caller_ip(p, headers, &ra.auth.trusted_proxies).is_loopback())
            .unwrap_or(true);
        let identity = CallerIdentity {
            scope: TokenScope::Admin,
            label: Some("human-credential".to_string()),
            is_local,
        };
        return Ok((identity, Some(proof)));
    }

    let identity = match authenticate_with(
        ra.auth,
        ra.tokens,
        peer_ip,
        headers,
        ra.auth.local_bypass_mutating,
    ) {
        Ok(identity) => identity,
        // No bearer token and no human credential: say what to do (`ta login`)
        // instead of a bare "Missing Authorization header".
        Err(_) if !headers.contains_key("authorization") => CallerIdentity {
            scope: TokenScope::Read,
            label: None,
            is_local: peer_ip
                .map(|p| resolve_caller_ip(p, headers, &ra.auth.trusted_proxies).is_loopback())
                .unwrap_or(true),
        },
        Err(response) => return Err(response),
    };
    match ra.gate.decide(
        &route,
        headers,
        ra.auth,
        &human_gate::GatedCaller {
            is_local: identity.is_local,
            scope: identity.scope,
            method,
            path,
        },
    ) {
        Ok(proof) => Ok((identity, Some(proof))),
        Err(r) => {
            tracing::warn!(
                method = %method,
                path,
                local = identity.is_local,
                "state-changing request refused: no human credential (run `ta login`)"
            );
            Err(Box::new(refusal_response(
                StatusCode::FORBIDDEN,
                &r,
                Some("[auth] local_bypass_mutating"),
            )))
        }
    }
}

/// Authentication middleware: Host allowlist, bearer token or local bypass for
/// read-only routes, and proof of a human for the gated mutating routes.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer_ip = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());
    let ra = RequestAuth {
        auth: &state.daemon_config.auth,
        tokens: &state.token_store,
        gate: &state.human_gate,
        hosts: &state.host_policy,
    };
    match authenticate_request(
        &ra,
        request.method(),
        request.uri().path(),
        peer_ip,
        request.headers(),
    ) {
        Ok((identity, proof)) => {
            request.extensions_mut().insert(identity);
            if let Some(p) = proof {
                request.extensions_mut().insert(p);
            }
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

    // ── Human-credential gate (CR-02) through the real router ──────────────

    use crate::api::{build_api_router, AppState};
    use crate::config::DaemonConfig;
    use crate::human_gate::{HumanGate, CREDENTIAL_HEADER};
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    struct Harness {
        _project: tempfile::TempDir,
        _state_dir: tempfile::TempDir,
        state_path: std::path::PathBuf,
        router: axum::Router,
        state: Arc<AppState>,
    }

    fn harness_with(config: DaemonConfig) -> Harness {
        let project = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(project.path().to_path_buf(), config);
        state.human_gate = HumanGate::new(state_dir.path().to_path_buf());
        let state = Arc::new(state);
        Harness {
            router: build_api_router(state.clone()),
            state_path: state_dir.path().to_path_buf(),
            state,
            _project: project,
            _state_dir: state_dir,
        }
    }

    fn harness() -> Harness {
        harness_with(DaemonConfig::default())
    }

    fn req(method: Method, path: &str, peer: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut r = b
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        r.extensions_mut()
            .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        r
    }

    async fn body_text(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Gated routes a credential is exercised against (handlers that would do
    /// real work with an empty body are left to the refusal loop below).
    const GATED: &[(&str, &str)] = &[
        ("POST", "/api/cmd"),
        ("POST", "/api/input"),
        ("POST", "/api/drafts/abc/approve"),
        ("POST", "/api/drafts/abc/deny"),
        ("POST", "/api/drafts/abc/apply"),
        (
            "POST",
            "/api/interactions/00000000-0000-0000-0000-000000000001/respond",
        ),
        ("POST", "/api/goals/g1/input"),
        ("POST", "/api/persona/save"),
        ("POST", "/api/team/assign"),
        ("POST", "/api/workflow/save"),
        ("POST", "/api/plan/phase/add"),
        ("POST", "/api/advisor/inject"),
        ("DELETE", "/api/agent/a1"),
    ];

    /// A concrete path for a gated-route pattern.
    fn concrete(pattern: &str) -> String {
        pattern
            .replace("{id}", "00000000-0000-0000-0000-000000000001")
            .replace("{name}", "n1")
            .replace("{section}", "s1")
            .replace("{role}", "r1")
            .replace("{key}", "k1")
    }

    #[tokio::test]
    async fn every_listed_gated_route_is_refused_without_a_credential() {
        let h = harness();
        for (m, pattern, _) in human_gate::GATED_ROUTES {
            let p = concrete(pattern);
            let resp = h
                .router
                .clone()
                .oneshot(req(m.parse().unwrap(), &p, "127.0.0.1:50000", &[]))
                .await
                .unwrap();
            // Routes served only by the legacy web router are 404 here, which
            // is also not a pass; everything the API router serves is 403.
            assert_ne!(resp.status(), StatusCode::OK, "{m} {p}");
            if resp.status() == StatusCode::FORBIDDEN {
                let body = body_text(resp).await;
                assert!(body.contains("ta login"), "{m} {p}: {body}");
            }
        }
    }

    #[tokio::test]
    async fn an_unlisted_mutating_route_is_refused_before_routing() {
        // Fail closed: even a path no handler serves is refused (403), not
        // routed (404), when the caller is not a human.
        let h = harness();
        for (m, p) in [
            (Method::POST, "/api/new-feature"),
            (Method::PUT, "/api/new-feature"),
            (Method::PATCH, "/api/new-feature"),
            (Method::DELETE, "/api/new-feature/x"),
        ] {
            let resp = h
                .router
                .clone()
                .oneshot(req(m.clone(), p, "127.0.0.1:50000", &[]))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{m} {p}");
        }
    }

    #[tokio::test]
    async fn agent_style_loopback_call_without_credential_is_refused_on_every_gated_route() {
        let h = harness();
        for (m, p) in GATED {
            let resp = h
                .router
                .clone()
                .oneshot(req(m.parse().unwrap(), p, "127.0.0.1:50000", &[]))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{m} {p}");
            let body = body_text(resp).await;
            assert!(
                body.contains("ta login"),
                "{p}: refusal must say what to do: {body}"
            );
            assert!(
                body.contains("local_bypass_mutating"),
                "{p}: must name the setting: {body}"
            );
        }
    }

    #[tokio::test]
    async fn read_only_routes_keep_the_loopback_bypass() {
        let h = harness();
        let resp = h
            .router
            .clone()
            .oneshot(req(
                Method::GET,
                "/api/interactions/pending",
                "127.0.0.1:50000",
                &[],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_valid_credential_passes_the_gate_on_loopback() {
        let h = harness();
        ta_credentials::human::login(&h.state_path, false).unwrap();
        let secret = ta_credentials::human::load_secret(&h.state_path, false).unwrap();
        for (m, p) in GATED {
            let resp = h
                .router
                .clone()
                .oneshot(req(
                    m.parse().unwrap(),
                    p,
                    "127.0.0.1:50000",
                    &[(CREDENTIAL_HEADER, &secret)],
                ))
                .await
                .unwrap();
            assert_ne!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "{m} {p} should pass the gate"
            );
            assert_ne!(resp.status(), StatusCode::UNAUTHORIZED, "{m} {p}");
        }
    }

    #[tokio::test]
    async fn a_wrong_or_revoked_credential_is_refused() {
        let h = harness();
        ta_credentials::human::login(&h.state_path, false).unwrap();
        let secret = ta_credentials::human::load_secret(&h.state_path, false).unwrap();
        let resp = h
            .router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/cmd",
                "127.0.0.1:1",
                &[(CREDENTIAL_HEADER, "tahc_nope")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        ta_credentials::human::logout(&h.state_path, false);
        let resp = h
            .router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/cmd",
                "127.0.0.1:1",
                &[(CREDENTIAL_HEADER, &secret)],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_loopback_bearer_token_alone_is_not_a_human() {
        // The token file lives in the project tree, which an agent can read.
        let mut cfg = DaemonConfig::default();
        cfg.auth.require_token = true;
        let h = harness_with(cfg);
        let tok = h.state.token_store.create(TokenScope::Admin, None).unwrap();
        let resp = h
            .router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/drafts/abc/approve",
                "127.0.0.1:1",
                &[("authorization", &format!("Bearer {}", tok.token))],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_remote_write_token_still_works_and_a_relay_token_does_not() {
        // (The relay path with an assertion is tested at /api/attention/{id}/act.)
        let mut cfg = DaemonConfig::default();
        cfg.auth.require_token = true;
        let h = harness_with(cfg);
        let write = h.state.token_store.create(TokenScope::Write, None).unwrap();
        let relay = h
            .state
            .token_store
            .create(TokenScope::Relay, Some("poller".into()))
            .unwrap();
        let send = |tok: &str, extra: Vec<(String, String)>| {
            let mut hdrs = vec![("authorization".to_string(), format!("Bearer {tok}"))];
            hdrs.extend(extra);
            let refs: Vec<(&str, &str)> =
                hdrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            req(
                Method::POST,
                "/api/drafts/abc/approve",
                "203.0.113.9:443",
                &refs,
            )
        };
        let resp = h
            .router
            .clone()
            .oneshot(send(&write.token, vec![]))
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "remote Write token keeps working"
        );

        let resp = h
            .router
            .clone()
            .oneshot(send(&relay.token, vec![]))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_text(resp).await;
        assert!(
            body.contains("relay") && body.contains("assertion"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_assertion_header_is_not_proof_on_the_direct_routes() {
        // Assertions are accepted only at /api/attention/{id}/act.
        let h = harness();
        let resp = h
            .router
            .clone()
            .oneshot(req(
                Method::POST,
                "/api/drafts/abc/approve",
                "127.0.0.1:1",
                &[("x-ta-human-assertion", "a.b.c")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn local_bypass_mutating_is_an_explicit_opt_out() {
        let mut cfg = DaemonConfig::default();
        cfg.auth.local_bypass_mutating = true;
        let h = harness_with(cfg);
        let resp = h
            .router
            .clone()
            .oneshot(req(Method::POST, "/api/cmd", "127.0.0.1:1", &[]))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn agent_coordination_routes_are_not_gated() {
        let h = harness();
        for (_, pattern, _) in human_gate::AGENT_ALLOWED {
            // Webhooks are served outside this router (their own HMAC auth).
            if pattern.starts_with("/api/webhooks/") {
                continue;
            }
            let p: &str = pattern;
            let resp = h
                .router
                .clone()
                .oneshot(req(Method::POST, p, "127.0.0.1:1", &[]))
                .await
                .unwrap();
            let status = resp.status();
            let body = body_text(resp).await;
            assert!(
                !body.contains("ta login"),
                "{p} must stay open: {status} {body}"
            );
        }
    }

    #[tokio::test]
    async fn rebinding_host_header_is_refused_with_actionable_error() {
        let h = harness();
        let resp = h
            .router
            .clone()
            .oneshot(req(
                Method::GET,
                "/api/status",
                "127.0.0.1:1",
                &[("host", "evil.example.com:7700")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_text(resp).await;
        assert!(body.contains("allowed_hosts"), "{body}");
        let resp = h
            .router
            .clone()
            .oneshot(req(
                Method::GET,
                "/api/status",
                "127.0.0.1:1",
                &[("host", "localhost:7700")],
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
