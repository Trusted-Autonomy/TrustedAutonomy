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
//   POST /api/drafts/:id/apply     → apply a draft in the background, returns { job_id } (v0.17.0.12.5)
//   GET  /api/apply-jobs/:job_id   → poll apply job status (v0.17.0.12.5)
//   GET  /api/apply-jobs/:job_id/log → fetch full apply log text (v0.17.0.12.5)
//   GET  /api/memory               → list memory entries (v0.5.7)
//   GET  /api/memory/search        → semantic search (?q=query) (v0.5.7)
//   GET  /api/memory/stats         → memory statistics (v0.5.7)
//   POST /api/memory               → create memory entry (v0.5.7)
//   DELETE /api/memory/:key        → delete memory entry (v0.5.7)

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tower_http::cors::{AllowOrigin, CorsLayer};
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
    /// In-flight and completed `ta draft apply` jobs, keyed by job ID (v0.17.0.12.5).
    apply_jobs: Arc<AsyncMutex<HashMap<String, ApplyJobRecord>>>,
}

/// Status of a background `ta draft apply` job (v0.17.0.12.5).
///
/// Apply can take longer than an HTTP request should block for (build + test +
/// git commit), so the endpoint spawns it and returns a job ID immediately;
/// clients poll `GET /api/apply-jobs/:job_id` for progress.
#[derive(Debug, Clone, Serialize)]
struct ApplyJobRecord {
    /// "running" | "done" | "failed"
    status: String,
    /// Last N lines of combined stdout+stderr (full output is in `log_path`).
    output: String,
    /// Path to the full log file on disk.
    log_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_sha: Option<String>,
}

/// Cap the in-memory/API-response output so a noisy apply doesn't blow up the
/// job status payload; the full output always goes to the log file on disk.
const APPLY_JOB_MAX_OUTPUT_LINES: usize = 200;

/// Maximum time to let a background apply run before declaring it failed.
const APPLY_JOB_TIMEOUT_SECS: u64 = 600;

fn tail_lines(s: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        s.to_string()
    } else {
        lines[lines.len() - max_lines..].join("\n")
    }
}

/// Derive the project root from `pr_packages_dir`: `.ta/pr_packages` → `.ta` → project root.
fn derive_project_root(pr_packages_dir: &std::path::Path) -> PathBuf {
    pr_packages_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(pr_packages_dir)
        .to_path_buf()
}

/// Create `.ta/logs/` if missing and prune apply logs older than 30 days
/// (v0.17.0.12.5). Called once at daemon startup; cheap directory scan.
pub fn init_apply_logs_dir(project_root: &std::path::Path) {
    let logs_dir = project_root.join(".ta").join("logs");
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        tracing::warn!(
            "Failed to create apply logs dir {}: {}",
            logs_dir.display(),
            e
        );
        return;
    }

    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 24 * 3600);
    let entries = match std::fs::read_dir(&logs_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                "Failed to read apply logs dir {}: {}",
                logs_dir.display(),
                e
            );
            return;
        }
    };

    let mut pruned = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "log") {
            continue;
        }
        let is_old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|mtime| mtime < cutoff)
            .unwrap_or(false);
        if is_old {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("Failed to prune old apply log {}: {}", path.display(), e);
            } else {
                pruned += 1;
            }
        }
    }
    if pruned > 0 {
        tracing::info!(
            pruned_count = pruned,
            logs_dir = %logs_dir.display(),
            "Pruned apply logs older than 30 days"
        );
    }
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

/// Web shell — responsive terminal UI served as a single HTML page.
async fn shell_page() -> Html<&'static str> {
    Html(include_str!("../assets/shell.html"))
}

/// Serve the PWA manifest for mobile-responsive web UI (v0.9.0).
async fn manifest() -> (
    [(axum::http::header::HeaderName, &'static str); 1],
    &'static str,
) {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/manifest+json",
        )],
        include_str!("../assets/manifest.json"),
    )
}

/// Serve favicon.ico (32x32 PNG served as ICO content-type) (v0.10.18.7).
async fn favicon() -> (
    [(axum::http::header::HeaderName, &'static str); 1],
    &'static [u8],
) {
    (
        [(axum::http::header::CONTENT_TYPE, "image/x-icon")],
        include_bytes!("../assets/favicon.ico"),
    )
}

/// Serve a PNG icon at the given size (v0.10.18.7).
async fn icon_192() -> (
    [(axum::http::header::HeaderName, &'static str); 1],
    &'static [u8],
) {
    (
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        include_bytes!("../assets/icon-192.png"),
    )
}

async fn icon_512() -> (
    [(axum::http::header::HeaderName, &'static str); 1],
    &'static [u8],
) {
    (
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        include_bytes!("../assets/icon-512.png"),
    )
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

/// `POST /api/drafts/:id/apply` — Apply an approved or pending draft to the workspace.
///
/// Spawns `ta draft apply <short_id> --git-commit` as a background task instead
/// of blocking the HTTP response (draft applies can involve builds/tests and
/// regularly exceed what's reasonable for a synchronous request — v0.17.0.12.5).
/// Returns a job ID immediately; poll `GET /api/apply-jobs/:job_id` for progress.
async fn apply_draft_endpoint(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UUID").into_response(),
    };

    // Verify the draft exists and is in an appliable state.
    match load_draft(&state.pr_packages_dir, uuid) {
        Ok(None) => return (StatusCode::NOT_FOUND, "draft not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Ok(Some(draft)) => {
            let status = format!("{:?}", draft.status).to_lowercase();
            if status.contains("denied")
                || status.contains("applied")
                || status.contains("superseded")
                || status.contains("closed")
            {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": format!(
                            "Draft is in terminal state '{}' and cannot be applied.",
                            status
                        )
                    })),
                )
                    .into_response();
            }
        }
    }

    let project_root = derive_project_root(&state.pr_packages_dir);
    let ta_bin = find_ta_binary_web();
    let short_id = id[..8.min(id.len())].to_string();
    let job_id = Uuid::new_v4().to_string();

    let logs_dir = project_root.join(".ta").join("logs");
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        tracing::warn!(
            "Failed to create apply logs dir {}: {}",
            logs_dir.display(),
            e
        );
    }
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let log_path = logs_dir.join(format!("apply-{}-{}.log", short_id, timestamp));

    {
        let mut jobs = state.apply_jobs.lock().await;
        jobs.insert(
            job_id.clone(),
            ApplyJobRecord {
                status: "running".to_string(),
                output: String::new(),
                log_path: log_path.to_string_lossy().to_string(),
                commit_sha: None,
            },
        );
    }

    let state_clone = state.clone();
    let job_id_clone = job_id.clone();
    let log_path_clone = log_path.clone();

    tokio::spawn(async move {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(APPLY_JOB_TIMEOUT_SECS),
            tokio::process::Command::new(&ta_bin)
                .arg("--project-root")
                .arg(&project_root)
                .arg("draft")
                .arg("apply")
                .arg(&short_id)
                .arg("--git-commit")
                .current_dir(&project_root)
                .output(),
        )
        .await;

        let (status, combined, commit_sha) = match result {
            Err(_) => (
                "failed".to_string(),
                format!("apply timed out after {} seconds", APPLY_JOB_TIMEOUT_SECS),
                None,
            ),
            Ok(Err(e)) => (
                "failed".to_string(),
                format!("Failed to spawn ta: {}. Is `ta` on PATH?", e),
                None,
            ),
            Ok(Ok(out)) => {
                let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                let combined = format!("{}{}", stdout, stderr);
                let commit_sha = parse_commit_sha(&combined);
                let status = if out.status.success() {
                    "done"
                } else {
                    "failed"
                };
                (status.to_string(), combined, commit_sha)
            }
        };

        // Write the full output to disk regardless of outcome (v0.17.0.12.5 item 1).
        if let Err(e) = std::fs::write(&log_path_clone, &combined) {
            tracing::warn!(
                "Failed to write apply log {}: {}",
                log_path_clone.display(),
                e
            );
        }

        let mut jobs = state_clone.apply_jobs.lock().await;
        jobs.insert(
            job_id_clone,
            ApplyJobRecord {
                status,
                output: tail_lines(&combined, APPLY_JOB_MAX_OUTPUT_LINES),
                log_path: log_path_clone.to_string_lossy().to_string(),
                commit_sha,
            },
        );
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "pending",
            "job_id": job_id,
        })),
    )
        .into_response()
}

/// `GET /api/apply-jobs/:job_id` — Poll the status of a background apply job.
async fn get_apply_job(
    State(state): State<Arc<WebState>>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let jobs = state.apply_jobs.lock().await;
    match jobs.get(&job_id) {
        Some(job) => Json(job.clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("No apply job found with id '{}'.", job_id)
            })),
        )
            .into_response(),
    }
}

/// `GET /api/apply-jobs/:job_id/log` — Fetch the full apply log for a job.
async fn get_apply_job_log(
    State(state): State<Arc<WebState>>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let log_path = {
        let jobs = state.apply_jobs.lock().await;
        match jobs.get(&job_id) {
            Some(job) => job.log_path.clone(),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    format!("No apply job found with id '{}'.", job_id),
                )
                    .into_response()
            }
        }
    };

    match std::fs::read_to_string(&log_path) {
        Ok(content) => (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            content,
        )
            .into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!("Apply log not yet available at {}: {}", log_path, e),
        )
            .into_response(),
    }
}

/// Parse the first 7- or 40-char hex commit SHA from apply output lines
/// that contain the word "commit".
fn parse_commit_sha(output: &str) -> Option<String> {
    for line in output.lines() {
        if line.to_lowercase().contains("commit") {
            for word in line.split_whitespace() {
                let w = word.trim_matches(|c: char| !c.is_ascii_hexdigit());
                if (w.len() == 7 || w.len() == 40) && w.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Some(w.to_string());
                }
            }
        }
    }
    None
}

/// Locate the `ta` binary. Prefers the one adjacent to the running daemon.
fn find_ta_binary_web() -> String {
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let ta_path = dir.join("ta");
            if ta_path.exists() {
                return ta_path.to_string_lossy().to_string();
            }
        }
    }
    "ta".to_string()
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
    drafts.sort_by_key(|d| Reverse(d.created_at));
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

/// Build the legacy web review UI router (draft/memory routes only).
/// Used by tests that don't need the full daemon API.
pub fn build_router(pr_packages_dir: PathBuf) -> Router {
    // Derive memory_dir from pr_packages_dir: sibling directory under .ta/
    let memory_dir = pr_packages_dir
        .parent()
        .unwrap_or(&pr_packages_dir)
        .join("memory");

    let state = Arc::new(WebState {
        pr_packages_dir,
        memory_dir,
        apply_jobs: Arc::new(AsyncMutex::new(HashMap::new())),
    });

    build_web_routes(state)
}

/// Build a restrictive CORS layer that allows only Studio and localhost origins (v0.17.0.9).
///
/// Allows:
/// - `app://ta-studio`  — Electron app
/// - `http://localhost` (any port)
/// - `http://127.0.0.1` (any port)
/// - Any additional origins supplied in `extra_origins`
///
/// Replaces `CorsLayer::permissive()` which allowed any webpage to call the local
/// daemon API, enabling CSRF attacks from arbitrary web origins.
pub fn build_cors_layer(extra_origins: &[String]) -> CorsLayer {
    let mut allowed: Vec<HeaderValue> = vec![
        HeaderValue::from_static("app://ta-studio"),
        HeaderValue::from_static("http://localhost"),
        HeaderValue::from_static("http://127.0.0.1"),
    ];
    for origin in extra_origins {
        if let Ok(v) = HeaderValue::from_str(origin) {
            allowed.push(v);
        }
    }
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            let origin_str = origin.to_str().unwrap_or("");
            // Allow exact matches in the allowlist.
            if allowed.iter().any(|a| a == origin) {
                return true;
            }
            // Allow http://localhost:<port> and http://127.0.0.1:<port>.
            origin_str.starts_with("http://localhost:")
                || origin_str.starts_with("http://127.0.0.1:")
        }))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}

/// Build web UI routes with the given state.
fn build_web_routes(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ui", get(index))
        .route("/shell", get(shell_page))
        .route("/manifest.json", get(manifest))
        // Favicon and icon routes (v0.10.18.7)
        .route("/favicon.ico", get(favicon))
        .route("/icon-192.png", get(icon_192))
        .route("/icon-512.png", get(icon_512))
        // Draft routes
        .route("/api/drafts", get(list_drafts))
        .route("/api/drafts/{id}", get(get_draft))
        .route("/api/drafts/{id}/approve", post(approve_draft))
        .route("/api/drafts/{id}/deny", post(deny_draft))
        .route("/api/drafts/{id}/apply", post(apply_draft_endpoint))
        .route("/api/apply-jobs/{job_id}", get(get_apply_job))
        .route("/api/apply-jobs/{job_id}/log", get(get_apply_job_log))
        // Memory routes (v0.5.7)
        .route("/api/memory", get(list_memory).post(create_memory))
        .route("/api/memory/search", get(search_memory))
        .route("/api/memory/stats", get(memory_stats))
        .route("/api/memory/{key}", delete(delete_memory))
        .layer(build_cors_layer(&[]))
        .with_state(state)
}

/// Build the combined router: web UI routes + full daemon API (v0.9.7).
///
/// Returns the router and a shared `AppState` handle so callers (e.g. the
/// auto-spawn supervisor) can reuse the same state without creating duplicates.
pub fn build_full_router(
    project_root: std::path::PathBuf,
    daemon_config: crate::config::DaemonConfig,
) -> (Router, Arc<crate::api::AppState>) {
    let app_state = Arc::new(crate::api::AppState::new(project_root, daemon_config));

    // Web UI routes use their own state (legacy).
    let web_state = Arc::new(WebState {
        pr_packages_dir: app_state.pr_packages_dir.clone(),
        memory_dir: app_state.memory_dir.clone(),
        apply_jobs: Arc::new(AsyncMutex::new(HashMap::new())),
    });

    // Build CORS layer using any extra origins from config (Studio URL, custom UIs).
    // Filter out legacy wildcard "*" entries — the layer manages its own allow-list.
    let extra_origins: Vec<String> = app_state
        .daemon_config
        .server
        .cors_origins
        .iter()
        .filter(|o| o.as_str() != "*")
        .cloned()
        .collect();
    let cors = build_cors_layer(&extra_origins);

    let web_routes = build_web_routes(web_state);
    let api_routes = crate::api::build_api_router(app_state.clone());

    // Merge: API routes take precedence, web routes fill in the rest.
    // Apply the single restrictive CORS layer at the top level.
    (api_routes.merge(web_routes).layer(cors), app_state)
}

/// Start the web review UI server (legacy — draft/memory only).
pub async fn serve_web_ui(pr_packages_dir: PathBuf, port: u16) -> anyhow::Result<()> {
    let app = build_router(pr_packages_dir);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
    tracing::info!("Web review UI listening on http://127.0.0.1:{}", port);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Start the full daemon API server (v0.9.7).
///
/// Accepts a `shutdown` notifier (v0.10.16) for graceful termination on
/// SIGINT/SIGTERM. When notified, the server completes in-flight requests
/// and stops accepting new connections.
///
/// Writes a `.ta/daemon.pid` file so the CLI can detect a running daemon
/// and auto-start one if needed (v0.10.16 item 5).
pub async fn serve_daemon_api(
    project_root: std::path::PathBuf,
    daemon_config: crate::config::DaemonConfig,
    shutdown: std::sync::Arc<tokio::sync::Notify>,
) -> anyhow::Result<()> {
    let bind = format!(
        "{}:{}",
        daemon_config.server.bind, daemon_config.server.port
    );

    // Write PID file for daemon discovery (v0.10.16).
    let pid_path = project_root.join(".ta").join("daemon.pid");
    write_pid_file(&pid_path, &daemon_config.server);

    // Clean up PID file on shutdown.
    let pid_path_clone = pid_path.clone();
    let sd_cleanup = shutdown.clone();
    tokio::spawn(async move {
        sd_cleanup.notified().await;
        let _ = std::fs::remove_file(&pid_path_clone);
        tracing::debug!("Removed daemon PID file");
    });

    // Capture web_ui setting before daemon_config is moved.
    let web_ui_enabled = daemon_config.server.web_ui;
    let web_ui_port = daemon_config.server.port;
    let web_ui_bind = daemon_config.server.bind.clone();

    // Ensure `.ta/logs/` exists for apply job logs and prune anything older
    // than 30 days (v0.17.0.12.5 item 3).
    init_apply_logs_dir(&project_root);

    let (app, app_state) = build_full_router(project_root, daemon_config);

    // Startup recovery: resume state-poll tasks for any goals that were
    // in-flight when the daemon was last restarted (v0.12.6 item 11).
    start_goal_recovery_tasks(&app_state);

    // Auto-spawn agent supervisor (runs in background, shares the same AppState).
    let supervisor_shutdown = shutdown.clone();
    tokio::spawn(crate::api::agent::auto_spawn_supervisor(
        app_state,
        supervisor_shutdown,
    ));

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("Daemon API listening on http://{}", bind);
    if web_ui_enabled {
        tracing::info!(
            "Web UI available at http://{}:{}/ui",
            web_ui_bind,
            web_ui_port
        );
    }
    // Use into_make_service_with_connect_info so that ConnectInfo<SocketAddr> is
    // populated in request extensions (needed by webhook and auth handlers).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown.notified().await;
        tracing::info!("Daemon API shutting down gracefully");
    })
    .await?;

    // Clean up PID file on normal exit too.
    let _ = std::fs::remove_file(&pid_path);

    Ok(())
}

/// Write a PID file containing the daemon process ID and bind address.
///
/// Format: `pid=<PID>\nbind=<host>:<port>\n`
fn write_pid_file(path: &std::path::Path, server: &crate::config::ServerConfig) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = format!(
        "pid={}\nbind={}:{}\n",
        std::process::id(),
        server.bind,
        server.port
    );
    match std::fs::write(path, &content) {
        Ok(()) => tracing::debug!(path = %path.display(), "Wrote daemon PID file"),
        Err(e) => tracing::warn!(
            path = %path.display(),
            error = %e,
            "Failed to write daemon PID file — auto-start may not detect this instance"
        ),
    }
}

/// Spawn state-poll recovery tasks for any goals that were in-flight
/// (state: `running` or `pr_ready`) when the daemon last restarted (v0.12.6 item 11).
///
/// This prevents goals from silently stalling in the goal store when the daemon
/// is restarted mid-run. Each recovered goal gets a lightweight poll task that
/// emits SSE events as state transitions occur (or as the watchdog updates state).
fn start_goal_recovery_tasks(app_state: &std::sync::Arc<crate::api::AppState>) {
    let goal_dir = app_state.project_root.join(".ta/goals");
    let events_dir = app_state.events_dir.clone();
    let project_root = app_state.project_root.clone();

    let store = match ta_goal::store::GoalRunStore::new(&goal_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Startup recovery: failed to open GoalRunStore");
            return;
        }
    };

    let goals = match store.list() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "Startup recovery: failed to list goals");
            return;
        }
    };

    let in_flight: Vec<_> = goals
        .into_iter()
        .filter(|g| {
            let s = g.state.to_string();
            s == "running" || s == "pr_ready"
        })
        .collect();

    if in_flight.is_empty() {
        return;
    }

    tracing::info!(
        count = in_flight.len(),
        "Startup recovery: resuming state-poll tasks for in-flight goals"
    );

    for goal in in_flight {
        let goal_id = goal.goal_run_id;
        let goal_title = goal.title.clone();
        let events_dir = events_dir.clone();
        let goal_dir = project_root.join(".ta/goals");
        let pr_dir = project_root.join(".ta/pr_packages");

        tracing::info!(
            goal_id = %goal_id,
            title = %goal_title,
            state = %goal.state,
            "Startup recovery: restarting state-poll for goal"
        );

        let initial_state = goal.state.to_string();
        tokio::spawn(async move {
            let mut last_state: Option<String> = Some(initial_state);
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                let store = match ta_goal::store::GoalRunStore::new(&goal_dir) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let goal = match store.get(goal_id) {
                    Ok(Some(g)) => g,
                    _ => continue,
                };
                let state_str = goal.state.to_string();

                if last_state.as_deref() == Some(&state_str) {
                    continue;
                }

                if let Some(ref prev) = last_state {
                    tracing::info!(
                        goal_id = %goal_id,
                        from = %prev,
                        to = %state_str,
                        "Recovery goal state transition"
                    );
                }
                last_state = Some(state_str.clone());

                // Emit SSE events for the new state.
                use ta_events::schema::{EventEnvelope, SessionEvent};
                use ta_events::store::{EventStore, FsEventStore};
                let event_store = FsEventStore::new(&events_dir);

                match state_str.as_str() {
                    "completed" => {
                        let event = SessionEvent::GoalCompleted {
                            goal_id,
                            title: goal.title.clone(),
                            duration_secs: None,
                        };
                        let _ = event_store.append(&EventEnvelope::new(event));
                    }
                    "pr_ready" => {
                        // Emit draft-ready events if a draft package exists.
                        use ta_changeset::draft_package::DraftPackage;
                        let goal_str = goal_id.to_string();
                        let latest = std::fs::read_dir(&pr_dir)
                            .ok()
                            .into_iter()
                            .flatten()
                            .filter_map(|e| e.ok())
                            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                            .filter_map(|s| serde_json::from_str::<DraftPackage>(&s).ok())
                            .filter(|d| d.goal.goal_id == goal_str)
                            .max_by_key(|d| d.created_at);

                        if let Some(d) = latest {
                            tracing::info!(
                                goal_id = %goal_id,
                                draft_id = %d.package_id,
                                artifact_count = d.changes.artifacts.len(),
                                "Recovery: draft detected — emitting ReviewRequested"
                            );
                            let built = SessionEvent::DraftBuilt {
                                goal_id,
                                draft_id: d.package_id,
                                artifact_count: d.changes.artifacts.len(),
                                title: goal.title.clone(),
                            };
                            let _ = event_store.append(&EventEnvelope::new(built));
                            let review = SessionEvent::ReviewRequested {
                                goal_id,
                                draft_id: d.package_id,
                                title: goal.title.clone(),
                                summary: format!(
                                    "Draft ready for '{}' — {} file(s) changed.",
                                    goal.title,
                                    d.changes.artifacts.len()
                                ),
                            };
                            let _ = event_store.append(&EventEnvelope::new(review));
                        }
                    }
                    "failed" | "denied" => {
                        let event = SessionEvent::GoalFailed {
                            goal_id,
                            error: "Goal in terminal failure state at daemon restart".to_string(),
                            exit_code: None,
                        };
                        let _ = event_store.append(&EventEnvelope::new(event));
                    }
                    _ => {}
                }

                // Stop polling once the goal reaches a terminal state.
                if matches!(
                    state_str.as_str(),
                    "completed" | "failed" | "denied" | "applied"
                ) {
                    tracing::info!(
                        goal_id = %goal_id,
                        terminal_state = %state_str,
                        "Recovery state-poll task exiting (terminal state)"
                    );
                    break;
                }
            }
        });
    }
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
    async fn favicon_serves_icon() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(Request::get("/favicon.ico").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "image/x-icon");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!body.is_empty(), "favicon body should not be empty");
    }

    #[tokio::test]
    async fn icon_192_serves_png() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(Request::get("/icon-192.png").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "image/png");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        // PNG magic bytes
        assert_eq!(&body[..4], b"\x89PNG");
    }

    #[tokio::test]
    async fn apply_draft_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let fake_id = Uuid::new_v4();
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/apply", fake_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn apply_job_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(
                Request::get("/api/apply-jobs/nonexistent-job")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn apply_job_log_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(
                Request::get("/api/apply-jobs/nonexistent-job/log")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn apply_draft_returns_job_id_immediately() {
        // The endpoint must respond right away with a job_id rather than
        // blocking on the (potentially slow) `ta draft apply` subprocess
        // (v0.17.0.12.5 item 1).
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        write_draft_json(&packages_dir, id, serde_json::json!({}));

        let app = build_router(packages_dir);
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/apply", id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "pending");
        assert!(json["job_id"].as_str().is_some());
    }

    #[test]
    fn tail_lines_returns_full_string_when_under_limit() {
        let s = "line1\nline2\nline3";
        assert_eq!(tail_lines(s, 10), s);
    }

    #[test]
    fn tail_lines_truncates_to_last_n_lines() {
        let s = "1\n2\n3\n4\n5";
        assert_eq!(tail_lines(s, 2), "4\n5");
    }

    #[test]
    fn parse_commit_sha_finds_sha_in_commit_line() {
        let output = "Applying draft...\nApplied — commit abc1234 to feature/test\nDone.\n";
        let sha = super::parse_commit_sha(output);
        assert_eq!(sha.as_deref(), Some("abc1234"));
    }

    #[test]
    fn parse_commit_sha_returns_none_when_absent() {
        let output = "Build succeeded.\nTests passed.\n";
        let sha = super::parse_commit_sha(output);
        assert!(sha.is_none());
    }

    #[tokio::test]
    async fn icon_512_serves_png() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_router(dir.path().to_path_buf());
        let resp = app
            .oneshot(Request::get("/icon-512.png").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "image/png");
    }

    // ── v0.17.0.1: draft detail endpoint returns supervisor_review and agent_decision_log ──

    /// Build a minimal valid DraftPackage JSON with the given UUID.
    fn minimal_draft_json(id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "package_version": "1.0.0",
            "package_id": id.to_string(),
            "created_at": "2026-01-01T00:00:00Z",
            "goal": {
                "goal_id": "aabbccdd-0000-0000-0000-000000000000",
                "title": "Test goal",
                "objective": "test",
                "success_criteria": [],
                "constraints": []
            },
            "iteration": {
                "iteration_id": "iter-1",
                "sequence": 1,
                "workspace_ref": {"type": "staging_dir", "ref": "test"}
            },
            "agent_identity": {
                "agent_id": "test-agent",
                "agent_type": "test",
                "constitution_id": "default",
                "capability_manifest_hash": "abc"
            },
            "summary": {"what_changed": "test", "why": "test", "impact": "none", "rollback_plan": "none", "open_questions": [], "alternatives_considered": []},
            "plan": {"completed_steps": [], "next_steps": [], "decision_log": []},
            "changes": {"artifacts": [], "patch_sets": [], "pending_actions": []},
            "risk": {"risk_score": 0, "findings": [], "policy_decisions": []},
            "provenance": {"inputs": [], "tool_trace_hash": "test"},
            "review_requests": {"requested_actions": [], "reviewers": [], "required_approvals": 1},
            "signatures": {"package_hash": "test", "agent_signature": "test"}
        })
    }

    fn write_draft_json(packages_dir: &std::path::Path, id: Uuid, extra: serde_json::Value) {
        let mut v = minimal_draft_json(id);
        if let Some(map) = v.as_object_mut() {
            if let Some(extra_map) = extra.as_object() {
                for (k, val) in extra_map {
                    map.insert(k.clone(), val.clone());
                }
            }
        }
        std::fs::write(
            packages_dir.join(format!("{}.json", id)),
            serde_json::to_string_pretty(&v).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn draft_detail_returns_supervisor_review() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();

        write_draft_json(
            &packages_dir,
            id,
            serde_json::json!({
                "supervisor_review": {
                    "verdict": "pass",
                    "scope_ok": true,
                    "findings": ["All good"],
                    "summary": "Changes are aligned with goal",
                    "agent": "builtin",
                    "duration_secs": 1.2
                }
            }),
        );

        let app = build_router(packages_dir);
        let resp = app
            .oneshot(
                Request::get(format!("/api/drafts/{}", id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status_code = resp.status();
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            status_code,
            StatusCode::OK,
            "body: {}",
            String::from_utf8_lossy(&body_bytes)
        );
        let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(
            val["supervisor_review"]["verdict"],
            serde_json::json!("pass"),
            "supervisor verdict should be 'pass'"
        );
        assert_eq!(
            val["supervisor_review"]["summary"],
            serde_json::json!("Changes are aligned with goal"),
        );
    }

    #[tokio::test]
    async fn draft_detail_returns_agent_decision_log() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();

        write_draft_json(
            &packages_dir,
            id,
            serde_json::json!({
                "agent_decision_log": [
                    {
                        "decision": "Use JSON for serialization",
                        "rationale": "Widely supported and human-readable",
                        "alternatives": ["BSON", "MessagePack"],
                        "confidence": 0.95
                    }
                ]
            }),
        );

        let app = build_router(packages_dir);
        let resp = app
            .oneshot(
                Request::get(format!("/api/drafts/{}", id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let log = val["agent_decision_log"]
            .as_array()
            .expect("agent_decision_log should be an array");
        assert_eq!(log.len(), 1, "should have one decision log entry");
        assert_eq!(
            log[0]["decision"],
            serde_json::json!("Use JSON for serialization")
        );
        assert_eq!(log[0]["confidence"], serde_json::json!(0.95));
    }

    #[tokio::test]
    async fn deny_draft_updates_status() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        write_draft_json(&packages_dir, id, serde_json::json!({}));

        let app = build_router(packages_dir.clone());
        let deny_body = serde_json::json!({"reason": "not what I asked for"});
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/deny", id))
                    .header("content-type", "application/json")
                    .body(Body::from(deny_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp_val: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp_val["status"], serde_json::json!("Denied"));

        // Verify the package file on disk reflects the denied status.
        let pkg_path = packages_dir.join(format!("{}.json", id));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(pkg_path).unwrap()).unwrap();
        let status_str = serde_json::to_string(&on_disk["status"]).unwrap();
        assert!(
            status_str.contains("Denied") || status_str.contains("denied"),
            "on-disk status should reflect denied: {}",
            status_str
        );
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

    // ── v0.17.0.12.5: apply logs directory ──────────────────────

    #[test]
    fn init_apply_logs_dir_creates_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path();
        assert!(!project_root.join(".ta").join("logs").exists());
        init_apply_logs_dir(project_root);
        assert!(project_root.join(".ta").join("logs").is_dir());
    }

    #[test]
    fn init_apply_logs_dir_prunes_old_logs_only() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path();
        let logs_dir = project_root.join(".ta").join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();

        let old_log = logs_dir.join("apply-old-19990101-000000.log");
        std::fs::write(&old_log, "stale").unwrap();
        let old_time =
            std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 24 * 3600);
        let old_file = std::fs::File::open(&old_log).unwrap();
        old_file.set_modified(old_time).unwrap();

        let recent_log = logs_dir.join("apply-recent-20260101-000000.log");
        std::fs::write(&recent_log, "fresh").unwrap();

        init_apply_logs_dir(project_root);

        assert!(!old_log.exists(), "log older than 30 days should be pruned");
        assert!(recent_log.exists(), "recent log should be kept");
    }
}
