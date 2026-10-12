//! Health and metrics endpoints (v0.14.4).
//!
//! - `GET /health` — minimal liveness check (local only, no auth required).
//! - `GET /metrics` — plugin hook for Prometheus-style metrics. Returns 501
//!   when no metrics plugin is registered.
//!
//! ## `/health`
//!
//! Returns 200 OK with a JSON body when the daemon is running:
//!
//! ```json
//! {
//!   "status": "ok",
//!   "version": "0.14.4-alpha",
//!   "plugins": ["auth", "audit_storage"]
//! }
//! ```
//!
//! Use this for load balancer health checks, systemd `ExecStartPost` readiness
//! probes, and CI "is the daemon up?" scripts.
//!
//! ## `/metrics`
//!
//! Reserved for a future metrics plugin. Until one is registered, returns
//! `501 Not Implemented` with a plain-text message.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;

use crate::api::AppState;

/// `GET /health` — daemon liveness check.
///
/// This endpoint intentionally bypasses the auth middleware so that health
/// checks from load balancers, Docker, and systemd can work without tokens.
///
/// # Response
///
/// `200 OK` with JSON:
/// ```json
/// {
///   "status": "ok",
///   "version": "0.14.4-alpha",
///   "timestamp": "2026-03-26T00:00:00Z",
///   "plugins": ["auth"],
///   "reasons": []
/// }
/// ```
///
/// `status` is `starting` or `degraded` (with `reasons`) while the daemon
/// waits for a keychain approval or cannot reach a part it needs; the
/// answer is still `200 OK` because the process is alive.
pub async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let plugins = state.daemon_config.plugins.configured_slots();
    let root = state.active_project_root.read().unwrap().clone();
    Json(health_body(&state.health, plugins, &root)).into_response()
}

/// The `/health` JSON body. `status` is `ok`, `starting` (keychain items are
/// still loading, for example waiting for a person to approve the macOS
/// dialog) or `degraded` (running, but a part is unavailable). `reasons`
/// explains any status other than `ok` and says what to do about it.
pub fn health_body(
    health: &crate::startup::DaemonHealth,
    plugins: Vec<&str>,
    root: &std::path::Path,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        // Identity of the answering daemon, so `ta daemon` commands can tell
        // their own project's daemon from another project's on the same port.
        "project_root": crate::api::status::canonical_root_string(root),
        "pid": std::process::id(),
        "timestamp": Utc::now().to_rfc3339(),
        "plugins": plugins,
    });
    health.apply_to(&mut body);
    body
}

/// `GET /metrics` — plugin hook for Prometheus/OpenMetrics scrape endpoint.
///
/// Returns `501 Not Implemented` until a metrics plugin is registered.
/// When the `[plugins].metrics` slot is set (defined in a future phase),
/// this handler will forward the scrape request to the plugin.
pub async fn metrics(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "metrics endpoint requires a metrics plugin — none registered.\n\
         Set [plugins].metrics in daemon.toml to enable Prometheus scraping.",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::{routing::get, Router};
    use std::path::PathBuf;
    use tower::ServiceExt;

    fn test_state() -> Arc<AppState> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(AppState::new(
            PathBuf::from(dir.path()),
            crate::config::DaemonConfig::default(),
        ))
    }

    #[tokio::test]
    async fn health_returns_200() {
        let state = test_state();
        let app = Router::new()
            .route("/health", get(health))
            .with_state(state);

        let resp = app
            .oneshot(
                Request::get("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["version"].is_string());
        assert!(json["timestamp"].is_string());
        assert!(json["plugins"].is_array());
        assert!(json["project_root"].is_string());
        assert_eq!(json["pid"].as_u64(), Some(u64::from(std::process::id())));
    }

    #[tokio::test]
    async fn health_reports_starting_with_a_keychain_reason_and_keeps_answering() {
        let mut state = Arc::try_unwrap(test_state()).ok().unwrap();
        let health = crate::startup::DaemonHealth::starting(std::time::Duration::from_secs(60));
        state.health = health.clone();
        let state = Arc::new(state);
        let app = Router::new()
            .route("/health", get(health_handler_for_test))
            .with_state(state);
        let resp = app
            .oneshot(
                Request::get("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Still 200: the process is alive, `status` carries the nuance.
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 8192).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "starting");
        assert_eq!(json["reasons"][0]["component"], "keychain");
        assert!(json["reasons"][0]["next_step"].is_string());
        // Existing contract fields are unchanged.
        assert!(json["version"].is_string());
        assert_eq!(json["pid"].as_u64(), Some(u64::from(std::process::id())));
        assert!(json["project_root"].is_string());
    }

    async fn health_handler_for_test(State(s): State<Arc<AppState>>) -> impl IntoResponse {
        health(State(s)).await
    }

    #[tokio::test]
    async fn metrics_returns_501() {
        let state = test_state();
        let app = Router::new()
            .route("/metrics", get(metrics))
            .with_state(state);

        let resp = app
            .oneshot(
                Request::get("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }
}
