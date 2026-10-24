// api/identity.rs — Every daemon response names its project.
//
// One daemon per project is the launch model, but "the daemon I reached is the
// project" must not be a client assumption: a mis-resolved port, a copied
// `.ta/daemon.json` or a future shared daemon would all break it. So every
// response carries `project_id` (stable, from `.ta/project-id`) and
// `project_root` (canonical absolute path), added here in ONE middleware rather
// than in each handler:
//
// * JSON object bodies get `project_id` and `project_root` added when absent
//   (never overwritten; no field is removed or renamed, so old clients that
//   ignore unknown fields keep working).
// * Every response, whatever its body (SSE stream, HTML, binary, a JSON array
//   or scalar, an error), gets the headers `X-TA-Project-Id` and
//   `X-TA-Project-Root` (the root percent-encoded, see
//   `ta_events::identity::header_encode`).
//
// Exposure. The middleware sits INSIDE the auth layer for authenticated route
// groups, so a 401/403 carries nothing and `project_root` only reaches callers
// the API already serves (the same callers who get `GET /api/status`).
// Unauthenticated groups (`/health`, `/metrics`, `/health/signals` and the
// HMAC-authenticated webhooks) use `Exposure::IdOnly`: `project_id` (a
// non-secret opaque id) but not the filesystem path. `/health` already
// reports `project_root` in its own body; that is unchanged.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderName, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use ta_events::identity::{header_encode, ProjectIdentity, PROJECT_ID_HEADER, PROJECT_ROOT_HEADER};

use crate::api::AppState;

/// Largest JSON body the middleware will buffer to add fields. Bigger bodies
/// (and bodies of unknown length) still get the headers.
const MAX_REWRITE_BYTES: usize = 8 * 1024 * 1024;

/// What a route group reveals about its project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// `project_id` and `project_root` (authenticated callers).
    Full,
    /// `project_id` only (unauthenticated routes).
    IdOnly,
}

#[derive(Clone)]
struct IdentityState {
    root: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    exposure: Exposure,
}

/// Stamp the identity of the project `state` currently serves (follows the
/// Studio project switch, like `/api/status`).
pub fn layer_for_state(router: Router, state: &Arc<AppState>, exposure: Exposure) -> Router {
    let state = state.clone();
    layer(
        router,
        Arc::new(move || state.active_project_root.read().unwrap().clone()),
        exposure,
    )
}

/// Stamp the identity of the fixed project at `root`.
pub fn layer_for_root(router: Router, root: PathBuf, exposure: Exposure) -> Router {
    layer(router, Arc::new(move || root.clone()), exposure)
}

fn layer(
    router: Router,
    root: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    exposure: Exposure,
) -> Router {
    router.layer(middleware::from_fn_with_state(
        IdentityState { root, exposure },
        stamp_identity,
    ))
}

async fn stamp_identity(State(s): State<IdentityState>, request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let identity = ProjectIdentity::read(&(s.root)());
    apply(response, &identity, s.exposure).await
}

/// Add `identity` to `response` as described in the module comment.
pub async fn apply(response: Response, identity: &ProjectIdentity, exposure: Exposure) -> Response {
    let (mut parts, body) = response.into_parts();

    let is_json = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            ct.trim_start()
                .to_ascii_lowercase()
                .starts_with("application/json")
        });
    // Axum sets Content-Length only when hyper serves the response, so the
    // buffering decision uses the body's own exact size, not the header.
    let known_len = axum::body::HttpBody::size_hint(&body)
        .exact()
        .and_then(|n| usize::try_from(n).ok());

    let body = if is_json && known_len.is_some_and(|n| n <= MAX_REWRITE_BYTES) {
        match axum::body::to_bytes(body, MAX_REWRITE_BYTES).await {
            Ok(bytes) => {
                let out = add_fields(&bytes, identity, exposure).unwrap_or(bytes.to_vec());
                parts
                    .headers
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(out.len()));
                Body::from(out)
            }
            Err(e) => {
                // Cannot happen for a body whose exact size we just checked;
                // say so loudly rather than send an empty body silently.
                tracing::error!(error = %e, "identity middleware could not read a JSON response body");
                Body::empty()
            }
        }
    } else {
        body
    };

    if let Some(id) = &identity.project_id {
        if let Ok(v) = HeaderValue::from_str(id) {
            parts
                .headers
                .insert(HeaderName::from_static(PROJECT_ID_HEADER), v);
        }
    }
    if exposure == Exposure::Full {
        if let Ok(v) = HeaderValue::from_str(&header_encode(&identity.project_root)) {
            parts
                .headers
                .insert(HeaderName::from_static(PROJECT_ROOT_HEADER), v);
        }
    }
    Response::from_parts(parts, body)
}

/// `bytes` with the identity fields added when it is a JSON object; `None`
/// (leave the body alone) for arrays, scalars and anything unparsable.
fn add_fields(bytes: &[u8], identity: &ProjectIdentity, exposure: Exposure) -> Option<Vec<u8>> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = value.as_object_mut()?;
    obj.entry("project_id").or_insert_with(|| {
        identity
            .project_id
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null)
    });
    if exposure == Exposure::Full {
        obj.entry("project_root")
            .or_insert_with(|| serde_json::Value::String(identity.project_root.clone()));
    }
    serde_json::to_vec(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Json;
    use tower::ServiceExt;

    const ID: &str = "p_0123456789abcdef0123456789abcdef";

    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        std::fs::write(d.path().join(".ta/project-id"), ID).unwrap();
        d
    }

    fn router(root: &std::path::Path, exposure: Exposure) -> Router {
        let r = Router::new()
            .route("/obj", get(|| async { Json(serde_json::json!({"a": 1})) }))
            .route(
                "/has-root",
                get(|| async { Json(serde_json::json!({"project_root": "mine"})) }),
            )
            .route("/arr", get(|| async { Json(serde_json::json!([1, 2])) }))
            .route("/text", get(|| async { "plain" }))
            .route(
                "/err",
                get(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": "x"})),
                    )
                        .into_response()
                }),
            );
        layer_for_root(r, root.to_path_buf(), exposure)
    }

    async fn get_path(app: Router, path: &str) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let resp = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    #[tokio::test]
    async fn json_objects_gain_the_fields_without_losing_any() {
        let d = project();
        let (_, h, body) = get_path(router(d.path(), Exposure::Full), "/obj").await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["a"], 1, "existing fields are kept");
        assert_eq!(v["project_id"], ID);
        assert_eq!(
            v["project_root"],
            std::fs::canonicalize(d.path())
                .unwrap()
                .display()
                .to_string()
        );
        assert_eq!(h["x-ta-project-id"], ID);
        assert_eq!(
            h["content-length"].to_str().unwrap(),
            body.len().to_string(),
            "content-length must match the rewritten body"
        );
    }

    #[tokio::test]
    async fn a_field_the_handler_already_set_is_not_overwritten() {
        let d = project();
        let (_, _, body) = get_path(router(d.path(), Exposure::Full), "/has-root").await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["project_root"], "mine");
        assert_eq!(v["project_id"], ID);
    }

    #[tokio::test]
    async fn non_object_bodies_are_untouched_and_carry_the_headers() {
        let d = project();
        for path in ["/arr", "/text"] {
            let (_, h, body) = get_path(router(d.path(), Exposure::Full), path).await;
            assert_eq!(h["x-ta-project-id"], ID, "{path}");
            assert!(h.contains_key("x-ta-project-root"), "{path}");
            if path == "/arr" {
                assert_eq!(body, b"[1,2]");
            } else {
                assert_eq!(body, b"plain");
            }
        }
    }

    #[tokio::test]
    async fn error_responses_keep_their_status_and_get_the_identity() {
        let d = project();
        let (status, h, body) = get_path(router(d.path(), Exposure::Full), "/err").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(h["x-ta-project-id"], ID);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "x");
        assert_eq!(v["project_id"], ID);
    }

    #[tokio::test]
    async fn id_only_never_reveals_the_path() {
        let d = project();
        let (_, h, body) = get_path(router(d.path(), Exposure::IdOnly), "/obj").await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["project_id"], ID);
        assert!(v.get("project_root").is_none(), "{v}");
        assert!(!h.contains_key("x-ta-project-root"));
        assert_eq!(h["x-ta-project-id"], ID);
    }

    #[tokio::test]
    async fn a_project_without_an_id_says_null_and_sends_no_id_header() {
        let d = tempfile::tempdir().unwrap();
        let (_, h, body) = get_path(router(d.path(), Exposure::Full), "/obj").await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["project_id"].is_null());
        assert!(!h.contains_key("x-ta-project-id"));
        assert!(h.contains_key("x-ta-project-root"));
    }

    #[tokio::test]
    async fn a_non_ascii_root_is_percent_encoded_in_the_header() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("projé");
        std::fs::create_dir_all(root.join(".ta")).unwrap();
        let (_, h, body) = get_path(router(&root, Exposure::Full), "/obj").await;
        let hv = h["x-ta-project-root"].to_str().unwrap();
        assert!(hv.contains("%C3%A9"), "{hv}");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["project_root"].as_str().unwrap().ends_with("projé"));
    }
}

/// Every route the daemon registers answers with its project's identity, and
/// the route list is read from the source, so a handler added tomorrow is
/// covered without touching this test.
#[cfg(test)]
mod route_table {
    use super::*;
    use crate::config::DaemonConfig;
    use axum::extract::ConnectInfo;
    use axum::http::StatusCode;
    use ta_events::EventStore;
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    /// A fresh id per test: tests run in parallel and the credential state
    /// treats two live project directories sharing an id as a copy.
    fn fresh_id() -> String {
        format!("p_{}", uuid::Uuid::new_v4().simple())
    }

    fn project(id: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        std::fs::write(d.path().join(".ta/project-id"), id).unwrap();
        d
    }

    fn router(root: &std::path::Path, config: DaemonConfig) -> (Router, Arc<AppState>) {
        crate::web::build_full_router(
            root.to_path_buf(),
            config,
            crate::startup::DaemonHealth::ready(),
        )
    }

    fn req(method: &str, uri: &str) -> Request {
        let mut r = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap();
        r.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap(),
        ));
        r
    }

    /// Route paths registered in `source` (production code only).
    fn routes_in(source: &str) -> Vec<String> {
        let prod = source.split("\n#[cfg(test)]\nmod ").next().unwrap();
        let mut out = Vec::new();
        let mut rest = prod;
        while let Some(i) = rest.find(".route(") {
            rest = &rest[i + ".route(".len()..];
            let t = rest.trim_start();
            if let Some(t) = t.strip_prefix('"') {
                if let Some(end) = t.find('"') {
                    out.push(t[..end].to_string());
                }
            }
        }
        out
    }

    fn concrete(path: &str) -> String {
        let mut out = String::new();
        let mut in_param = false;
        for c in path.chars() {
            match c {
                '{' => {
                    in_param = true;
                    out.push('x');
                }
                '}' => in_param = false,
                _ if in_param => {}
                c => out.push(c),
            }
        }
        out
    }

    /// Paths that must not reveal the project root (no authentication).
    fn is_unauthenticated(path: &str) -> bool {
        matches!(path, "/health" | "/metrics" | "/health/signals")
            || path.starts_with("/api/webhooks")
    }

    #[tokio::test]
    async fn every_registered_route_carries_the_project_identity() {
        let id_a = fresh_id();
        let d = project(&id_a);
        let (app, _state) = router(d.path(), DaemonConfig::default());
        let root = std::fs::canonicalize(d.path())
            .unwrap()
            .display()
            .to_string();

        let mut paths: Vec<String> = routes_in(include_str!("mod.rs"))
            .into_iter()
            .chain(routes_in(include_str!("../web.rs")))
            .collect();
        paths.sort();
        paths.dedup();
        assert!(
            paths.len() > 40,
            "route scanner found only {} routes; the registration style changed",
            paths.len()
        );

        let mut failures = Vec::new();
        for path in &paths {
            // GET runs the read-only handlers of GET routes and answers 405
            // from the method router on POST-only ones (no mutation either
            // way). Only the headers are inspected; streaming bodies are
            // dropped unread. OPTIONS is not used: the outer CORS layer
            // answers it before any route group.
            let resp = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                app.clone().oneshot(req("GET", &concrete(path))),
            )
            .await
            .unwrap_or_else(|_| panic!("GET {path} did not answer within 20s"))
            .unwrap();
            let h = resp.headers();
            if h.get("x-ta-project-id").and_then(|v| v.to_str().ok()) != Some(id_a.as_str()) {
                failures.push(format!("{path}: no X-TA-Project-Id ({})", resp.status()));
            }
            let has_root = h.get("x-ta-project-root").and_then(|v| v.to_str().ok()) == Some(&root);
            if is_unauthenticated(path) == has_root {
                failures.push(format!(
                    "{path}: X-TA-Project-Root exposure wrong (unauthenticated={}, present={has_root})",
                    is_unauthenticated(path)
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "routes missing identity (a new route must live in a group wrapped by api::identity):\n{}",
            failures.join("\n")
        );
    }

    #[tokio::test]
    async fn json_endpoints_carry_both_fields_in_the_body_and_keep_their_own() {
        let id_a = fresh_id();
        let d = project(&id_a);
        let (app, _s) = router(d.path(), DaemonConfig::default());
        let root = std::fs::canonicalize(d.path())
            .unwrap()
            .display()
            .to_string();
        let resp = app
            .clone()
            .oneshot(req("GET", "/api/status"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["project_id"], id_a.as_str());
        assert_eq!(v["project_root"], root.as_str());
        assert!(v["version"].is_string(), "existing status fields stay: {v}");

        let resp = app.clone().oneshot(req("GET", "/health")).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["project_id"], id_a.as_str());
    }

    #[tokio::test]
    async fn two_projects_never_answer_for_each_other() {
        let (id_a, id_b) = (fresh_id(), fresh_id());
        let a = project(&id_a);
        let b = project(&id_b);
        let (app_a, _sa) = router(a.path(), DaemonConfig::default());
        let (app_b, _sb) = router(b.path(), DaemonConfig::default());
        for (app, id, dir) in [(&app_a, id_a.as_str(), &a), (&app_b, id_b.as_str(), &b)] {
            let resp = app
                .clone()
                .oneshot(req("GET", "/api/status"))
                .await
                .unwrap();
            assert_eq!(resp.headers()["x-ta-project-id"], id);
            let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["project_id"], id);
            assert_eq!(
                v["project_root"],
                std::fs::canonicalize(dir.path())
                    .unwrap()
                    .display()
                    .to_string()
                    .as_str()
            );
        }
    }

    #[tokio::test]
    async fn a_refused_request_reveals_nothing_about_the_project() {
        let id_a = fresh_id();
        let d = project(&id_a);
        let mut cfg = DaemonConfig::default();
        cfg.auth.require_token = true;
        cfg.auth.local_bypass = false;
        let (app, _s) = router(d.path(), cfg);
        let resp = app.oneshot(req("GET", "/api/status")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(!resp.headers().contains_key("x-ta-project-root"));
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&body).contains("project_root"),
            "{}",
            String::from_utf8_lossy(&body)
        );
    }

    #[tokio::test]
    async fn the_event_stream_and_html_carry_headers_and_events_carry_identity() {
        let id_a = fresh_id();
        let d = project(&id_a);
        let (app, state) = router(d.path(), DaemonConfig::default());
        let root = std::fs::canonicalize(d.path())
            .unwrap()
            .display()
            .to_string();

        // An event emitted the way every emitter does: through the store.
        let store = ta_events::FsEventStore::new(&state.events_dir);
        store
            .append(&ta_events::EventEnvelope::new(
                ta_events::SessionEvent::GoalStarted {
                    goal_id: uuid::Uuid::new_v4(),
                    title: "t".into(),
                    agent_id: "a".into(),
                    phase: None,
                },
            ))
            .unwrap();

        let resp = app
            .clone()
            .oneshot(req("GET", "/api/events?since=1970-01-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["x-ta-project-id"], id_a.as_str());
        let mut stream = resp.into_body().into_data_stream();
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("an event within 5s")
            .expect("stream open")
            .expect("chunk");
        let text = String::from_utf8_lossy(&chunk).to_string();
        let data = text
            .lines()
            .find_map(|l| l.strip_prefix("data:"))
            .unwrap_or_else(|| panic!("no data line in {text}"));
        let v: serde_json::Value = serde_json::from_str(data.trim()).unwrap();
        assert_eq!(v["project_id"], id_a.as_str(), "{v}");
        assert_eq!(v["project_root"], root.as_str(), "{v}");

        let resp = app.oneshot(req("GET", "/ui")).await.unwrap();
        assert_eq!(resp.headers()["x-ta-project-id"], id_a.as_str());
    }
}
