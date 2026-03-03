// web.rs — Minimal web review UI for Trusted Autonomy (v0.5.2+).
//
// Serves a single-page HTML app and JSON API for reviewing draft packages
// and browsing the memory store (v0.5.7).
//
// Routes:
//   GET  /                         → embedded HTML review UI
//   GET  /api/drafts               → list drafts (JSON array)
//   GET  /api/drafts/:id           → draft detail (DraftPackage JSON)
//   POST /api/drafts/:id/approve   → approve a draft
//   POST /api/drafts/:id/deny      → deny a draft { reason }
//   GET  /api/memory               → list memory entries (v0.5.7)
//   GET  /api/memory/search        → semantic search (?q=query) (v0.5.7)
//   GET  /api/memory/stats         → memory statistics (v0.5.7)
//   POST /api/memory               → create memory entry (v0.5.7)
//   DELETE /api/memory/:key        → delete memory entry (v0.5.7)

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use chrono::Utc;
use ta_changeset::draft_package::{DraftPackage, DraftStatus};
use ta_memory::{FsMemoryStore, MemoryStore};

// ── State ────────────────────────────────────────────────────────

/// Shared state for the web server.
#[derive(Clone)]
struct WebState {
    pr_packages_dir: PathBuf,
    memory_dir: PathBuf,
}

// ── API types ────────────────────────────────────────────────────

/// Summary of a draft for list responses.
#[derive(Serialize, Deserialize)]
struct DraftSummary {
    package_id: Uuid,
    title: String,
    status: String,
    created_at: String,
    artifact_count: usize,
}

/// Request body for the deny endpoint.
#[derive(Deserialize)]
struct DenyRequest {
    #[serde(default = "default_deny_reason")]
    reason: String,
}

fn default_deny_reason() -> String {
    "denied via web UI".to_string()
}

/// Response for approve/deny actions.
#[derive(Serialize)]
struct ActionResponse {
    package_id: String,
    status: String,
    message: String,
}

/// Query parameters for memory search.
#[derive(Deserialize)]
struct MemorySearchQuery {
    q: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

/// Request body for creating a memory entry via the web UI.
#[derive(Deserialize)]
struct CreateMemoryRequest {
    key: String,
    value: Option<serde_json::Value>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    category: Option<String>,
}

/// API representation of a memory entry.
#[derive(Serialize, Deserialize)]
struct MemoryEntryResponse {
    entry_id: String,
    key: String,
    value: serde_json::Value,
    tags: Vec<String>,
    source: String,
    category: Option<String>,
    goal_id: Option<String>,
    confidence: f64,
    created_at: String,
    updated_at: String,
    expires_at: Option<String>,
}

impl From<ta_memory::MemoryEntry> for MemoryEntryResponse {
    fn from(e: ta_memory::MemoryEntry) -> Self {
        Self {
            entry_id: e.entry_id.to_string(),
            key: e.key,
            value: e.value,
            tags: e.tags,
            source: e.source,
            category: e.category.as_ref().map(|c| c.to_string()),
            goal_id: e.goal_id.map(|id| id.to_string()),
            confidence: e.confidence,
            created_at: e.created_at.to_rfc3339(),
            updated_at: e.updated_at.to_rfc3339(),
            expires_at: e.expires_at.map(|t| t.to_rfc3339()),
        }
    }
}

// ── Draft handlers ───────────────────────────────────────────────

async fn index() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn list_drafts(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    match load_all_drafts(&state.pr_packages_dir) {
        Ok(drafts) => {
            let summaries: Vec<DraftSummary> = drafts
                .iter()
                .map(|d| DraftSummary {
                    package_id: d.package_id,
                    title: d.goal.title.clone(),
                    status: format!("{:?}", d.status),
                    created_at: d.created_at.to_rfc3339(),
                    artifact_count: d.changes.artifacts.len(),
                })
                .collect();
            Json(summaries).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to load drafts: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn get_draft(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UUID").into_response(),
    };

    match load_draft(&state.pr_packages_dir, uuid) {
        Ok(Some(draft)) => Json(draft).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "draft not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn approve_draft(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UUID").into_response(),
    };

    let status = DraftStatus::Approved {
        approved_by: "web-ui".into(),
        approved_at: Utc::now(),
    };
    match update_draft_status(&state.pr_packages_dir, uuid, status) {
        Ok(true) => Json(ActionResponse {
            package_id: id,
            status: "Approved".into(),
            message: "Draft approved via web UI".into(),
        })
        .into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "draft not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn deny_draft(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
    Json(body): Json<DenyRequest>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UUID").into_response(),
    };

    let status = DraftStatus::Denied {
        reason: body.reason,
        denied_by: "web-ui".into(),
    };
    match update_draft_status(&state.pr_packages_dir, uuid, status) {
        Ok(true) => Json(ActionResponse {
            package_id: id,
            status: "Denied".into(),
            message: "Draft denied via web UI".into(),
        })
        .into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "draft not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Memory handlers (v0.5.7) ─────────────────────────────────────

async fn list_memory(
    State(state): State<Arc<WebState>>,
    Query(params): Query<MemorySearchQuery>,
) -> impl IntoResponse {
    let store = FsMemoryStore::new(&state.memory_dir);
    let entries = match store.list(Some(params.limit)) {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let items: Vec<MemoryEntryResponse> = entries.into_iter().map(Into::into).collect();
    Json(items).into_response()
}

async fn search_memory(
    State(state): State<Arc<WebState>>,
    Query(params): Query<MemorySearchQuery>,
) -> impl IntoResponse {
    let query = params.q.unwrap_or_default();
    if query.is_empty() {
        return (StatusCode::BAD_REQUEST, "query parameter 'q' is required").into_response();
    }
    let store = FsMemoryStore::new(&state.memory_dir);
    // Semantic search is only available with ruvector; fall back to prefix search.
    let entries = match store.lookup(ta_memory::MemoryQuery {
        key_prefix: Some(query.clone()),
        limit: Some(params.limit),
        ..Default::default()
    }) {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let items: Vec<MemoryEntryResponse> = entries.into_iter().map(Into::into).collect();
    Json(items).into_response()
}

async fn memory_stats(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let store = FsMemoryStore::new(&state.memory_dir);
    match store.stats() {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn create_memory(
    State(state): State<Arc<WebState>>,
    Json(body): Json<CreateMemoryRequest>,
) -> impl IntoResponse {
    let mut store = FsMemoryStore::new(&state.memory_dir);
    let value = body
        .value
        .unwrap_or(serde_json::Value::String(body.key.clone()));
    let params = ta_memory::StoreParams {
        category: body
            .category
            .as_deref()
            .map(ta_memory::MemoryCategory::from_str_lossy),
        ..Default::default()
    };
    match store.store_with_params(&body.key, value, body.tags, "web-ui", params) {
        Ok(entry) => (StatusCode::CREATED, Json(MemoryEntryResponse::from(entry))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn delete_memory(
    State(state): State<Arc<WebState>>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    let mut store = FsMemoryStore::new(&state.memory_dir);
    match store.forget(&key) {
        Ok(true) => Json(serde_json::json!({"status": "deleted", "key": key})).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "entry not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Filesystem helpers ──────────────────────────────────────────

fn load_all_drafts(dir: &std::path::Path) -> Result<Vec<DraftPackage>, std::io::Error> {
    let mut drafts = Vec::new();
    if !dir.exists() {
        return Ok(drafts);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<DraftPackage>(&content) {
                    Ok(draft) => drafts.push(draft),
                    Err(e) => tracing::warn!("Skipping invalid draft {}: {}", path.display(), e),
                },
                Err(e) => tracing::warn!("Cannot read {}: {}", path.display(), e),
            }
        }
    }
    // Most recent first.
    drafts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(drafts)
}

fn load_draft(dir: &std::path::Path, id: Uuid) -> Result<Option<DraftPackage>, std::io::Error> {
    let path = dir.join(format!("{}.json", id));
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let draft: DraftPackage = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Some(draft))
}

fn update_draft_status(
    dir: &std::path::Path,
    id: Uuid,
    status: DraftStatus,
) -> Result<bool, std::io::Error> {
    let path = dir.join(format!("{}.json", id));
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&path)?;
    let mut draft: DraftPackage = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    draft.status = status;
    let updated =
        serde_json::to_string_pretty(&draft).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(&path, updated)?;
    Ok(true)
}

// ── Router and server ───────────────────────────────────────────

/// Build the Axum router for the web review UI.
pub fn build_router(pr_packages_dir: PathBuf) -> Router {
    // Derive memory_dir from pr_packages_dir: sibling directory under .ta/
    let memory_dir = pr_packages_dir
        .parent()
        .unwrap_or(&pr_packages_dir)
        .join("memory");

    let state = Arc::new(WebState {
        pr_packages_dir,
        memory_dir,
    });

    Router::new()
        .route("/", get(index))
        // Draft routes
        .route("/api/drafts", get(list_drafts))
        .route("/api/drafts/{id}", get(get_draft))
        .route("/api/drafts/{id}/approve", post(approve_draft))
        .route("/api/drafts/{id}/deny", post(deny_draft))
        // Memory routes (v0.5.7)
        .route("/api/memory", get(list_memory).post(create_memory))
        .route("/api/memory/search", get(search_memory))
        .route("/api/memory/stats", get(memory_stats))
        .route("/api/memory/{key}", delete(delete_memory))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Start the web review UI server.
pub async fn serve_web_ui(pr_packages_dir: PathBuf, port: u16) -> anyhow::Result<()> {
    let app = build_router(pr_packages_dir);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
    tracing::info!("Web review UI listening on http://127.0.0.1:{}", port);
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_router(dir: PathBuf) -> Router {
        // Pass a subdirectory as pr_packages_dir so memory_dir resolves
        // to a sibling within the same temp dir (avoiding cross-test pollution).
        let packages_dir = dir.join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        build_router(packages_dir)
    }

    #[tokio::test]
    async fn index_serves_html() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Trusted Autonomy"));
    }

    #[tokio::test]
    async fn list_drafts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(Request::get("/api/drafts").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let drafts: Vec<DraftSummary> = serde_json::from_slice(&body).unwrap();
        assert!(drafts.is_empty());
    }

    #[tokio::test]
    async fn get_draft_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let fake_id = Uuid::new_v4();
        let resp = app
            .oneshot(
                Request::get(format!("/api/drafts/{}", fake_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn approve_draft_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let fake_id = Uuid::new_v4();
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/approve", fake_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn memory_list_empty() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(Request::get("/api/memory").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let entries: Vec<MemoryEntryResponse> = serde_json::from_slice(&body).unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn memory_stats_empty() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(
                Request::get("/api/memory/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let stats: ta_memory::MemoryStats = serde_json::from_slice(&body).unwrap();
        assert_eq!(stats.total_entries, 0);
    }

    #[tokio::test]
    async fn memory_create_and_list() {
        let dir = tempfile::tempdir().unwrap();
        // Create memory directory (build_router derives it from pr_packages_dir parent)
        let memory_dir = dir.path().join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();

        let app = test_router(dir.path().to_path_buf());

        // Create an entry
        let create_body = serde_json::json!({
            "key": "test-entry",
            "value": "hello world",
            "tags": ["test"],
            "category": "convention"
        });
        let resp = app
            .clone()
            .oneshot(
                Request::post("/api/memory")
                    .header("content-type", "application/json")
                    .body(Body::from(create_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // List should now have 1 entry
        let resp = app
            .oneshot(Request::get("/api/memory").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let entries: Vec<MemoryEntryResponse> = serde_json::from_slice(&body).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "test-entry");
        assert_eq!(entries[0].category.as_deref(), Some("convention"));
    }
}
