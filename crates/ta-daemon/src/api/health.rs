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
///   "plugins": ["auth"]
/// }
/// ```
pub async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let plugins = state.daemon_config.plugins.configured_slots();
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "timestamp": Utc::now().to_rfc3339(),
        "plugins": plugins,
    }))
    .into_response()
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
