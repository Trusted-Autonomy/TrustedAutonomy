// web.rs — Minimal web review UI for Trusted Autonomy (v0.5.2+).
//
// Serves a single-page HTML app and JSON API for reviewing draft packages
// and browsing the memory store (v0.5.7).
//
// Routes:
//   GET  /                         → embedded HTML review UI
//   GET  /api/drafts               → list drafts (JSON array)
//   GET  /api/drafts/:id           → draft detail (DraftPackage JSON)
//   GET  /api/drafts/:id/artifact  → raw bytes of one artifact, for image/video preview (?uri=<resource_uri>) (v0.17.0.12.17)
//   POST /api/drafts/:id/approve   → approve a draft
//   POST /api/drafts/:id/deny      → deny a draft { reason }
//   POST /api/drafts/:id/apply     → apply a draft in the background, returns { status, job_id } (v0.17.0.12.5)
//   GET  /api/apply-jobs/:job_id   → poll a background apply job (v0.17.0.12.5)
//   GET  /api/memory               → list memory entries (v0.5.7)
//   GET  /api/memory/search        → semantic search (?q=query) (v0.5.7)
//   GET  /api/memory/stats         → memory statistics (v0.5.7)
//   POST /api/memory               → create memory entry (v0.5.7)
//   DELETE /api/memory/:key        → delete memory entry (v0.5.7)

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowOrigin, CorsLayer};
use uuid::Uuid;

use chrono::Utc;
use ta_changeset::draft_package::{ArtifactDisposition, DraftPackage, DraftStatus};
use ta_memory::{FsMemoryStore, MemoryStore};

// ── State ────────────────────────────────────────────────────────

/// Shared state for the web server.
#[derive(Clone)]
struct WebState {
    pr_packages_dir: PathBuf,
    memory_dir: PathBuf,
    /// In-memory tracking of background `ta draft apply` jobs, keyed by job id
    /// (v0.17.0.12.5). Cleared on daemon restart — job status is also durably
    /// recorded in the log file at `ApplyJobRecord::log_path`.
    apply_jobs: Arc<Mutex<HashMap<String, ApplyJobRecord>>>,
}

// ── Apply jobs (v0.17.0.12.5) ───────────────────────────────────────

/// Status of a background `ta draft apply` job.
#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ApplyJobStatus {
    Running,
    Done,
    Failed,
}

/// Tracked state of a background `ta draft apply` job, polled via
/// `GET /api/apply-jobs/:job_id`.
#[derive(Clone, Serialize)]
struct ApplyJobRecord {
    status: ApplyJobStatus,
    /// Last N lines of combined stdout+stderr. Full output is always in `log_path`.
    output: String,
    log_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_sha: Option<String>,
}

/// Keep only the last `n` lines of `s` (apply job output can be long; the poll
/// response should stay small — the full output is always on disk at `log_path`).
fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        s.to_string()
    } else {
        lines[lines.len() - n..].join("\n")
    }
}

/// Directory where `ta draft apply` job logs are written.
fn apply_logs_dir(project_root: &std::path::Path) -> PathBuf {
    project_root.join(".ta").join("logs")
}

/// Ensure `.ta/logs/` exists and prune log files older than `retention_days`.
///
/// Called on daemon startup (v0.17.0.12.5 item 3) so logs don't accumulate
/// indefinitely. Failures are logged but non-fatal — a missing/unprunable
/// logs dir should never block daemon startup.
fn ensure_and_prune_logs_dir(project_root: &std::path::Path, retention_days: i64) {
    let dir = apply_logs_dir(project_root);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "Failed to create apply logs directory"
        );
        return;
    }

    let cutoff = Utc::now() - chrono::Duration::days(retention_days);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e, "Failed to read apply logs directory");
            return;
        }
    };

    let mut pruned = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let modified = match entry.metadata().and_then(|m| m.modified()) {
            Ok(m) => chrono::DateTime::<Utc>::from(m),
            Err(_) => continue,
        };
        if modified < cutoff && std::fs::remove_file(&path).is_ok() {
            pruned += 1;
        }
    }
    if pruned > 0 {
        tracing::info!(pruned, path = %dir.display(), "Pruned old apply logs");
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
    /// Artifact `resource_uri`s left checked in the Studio "Changes" panel
    /// (v0.17.0.12.9 item 7). When present, stamps each artifact's
    /// disposition the same way approve does, before marking the whole
    /// draft Denied — a record of which files the human actually wanted,
    /// even though none of them get applied from a denied draft.
    #[serde(default)]
    selected_uris: Option<Vec<String>>,
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

/// `GET /api/drafts/:id` response: the draft itself, plus (v0.17.0.12.6 item
/// 7) the *first* draft's supervisor review for this goal when it differs
/// from the current draft's own review — e.g. a follow-up draft that didn't
/// re-run the supervisor still lets Studio show "the initial supervisor
/// review output from the goal's audit trail."
#[derive(Debug, Serialize)]
struct DraftDetailResponse {
    #[serde(flatten)]
    draft: DraftPackage,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_supervisor_review: Option<ta_changeset::supervisor_review::SupervisorReview>,
    /// Unresolved shared-file merge conflicts from this goal's most recent
    /// apply attempt (v0.17.0.12.7). Read-only in this phase — resolve the
    /// listed file manually, then re-run `ta draft apply`. Full interactive
    /// resolution is deferred to v0.18 per the phase's own scope note.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    conflicts: Vec<ConflictInfo>,
    /// Advisor-triggered edits to shared files queued while a goal was
    /// running, not yet replayed onto the project (v0.17.0.12.7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_advisor_patches: Vec<PendingAdvisorPatchInfo>,
}

/// A shared file with unresolved conflict-marked content, surfaced read-only
/// in the draft review panel (v0.17.0.12.7).
#[derive(Debug, Serialize)]
struct ConflictInfo {
    /// Relative path from the project root (e.g. "PLAN.md").
    path: String,
    /// Conflict-marked content (`<<<<<<<`/`=======`/`>>>>>>>`), as text.
    content: String,
}

/// A queued advisor patch not yet replayed onto the project (v0.17.0.12.7).
#[derive(Debug, Serialize)]
struct PendingAdvisorPatchInfo {
    /// Relative path from the project root (e.g. "PLAN.md").
    path: String,
    description: String,
    queued_at: u64,
}

/// Derive the project root from `pr_packages_dir`: `.ta/pr_packages` → `.ta` → root.
fn project_root_from_pr_packages_dir(pr_packages_dir: &std::path::Path) -> std::path::PathBuf {
    pr_packages_dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(pr_packages_dir)
        .to_path_buf()
}

/// Read unresolved shared-file conflicts for the goal owning `package_id`
/// (from `.ta/goals/<goal_id>/conflicts/`), and any advisor patches still
/// queued project-wide (from `.ta/advisor-patches/`) — v0.17.0.12.7.
fn find_conflicts_and_patches(
    project_root: &std::path::Path,
    package_id: Uuid,
) -> (Vec<ConflictInfo>, Vec<PendingAdvisorPatchInfo>) {
    let goals_dir = project_root.join(".ta").join("goals");

    let mut conflicts = Vec::new();
    if let Ok(store) = ta_goal::store::GoalRunStore::new(&goals_dir) {
        if let Ok(goals) = store.list() {
            if let Some(goal) = goals.iter().find(|g| g.pr_package_id == Some(package_id)) {
                let conflicts_dir = goals_dir
                    .join(goal.goal_run_id.to_string())
                    .join("conflicts");
                if let Ok(entries) = std::fs::read_dir(&conflicts_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("conflict") {
                            continue;
                        }
                        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                            continue;
                        };
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            conflicts.push(ConflictInfo {
                                path: stem.replace("__", "/"),
                                content,
                            });
                        }
                    }
                }
            }
        }
    }

    let mut pending_advisor_patches = Vec::new();
    let patches_dir = ta_workspace::advisor_patch::patches_dir(project_root);
    if let Ok(entries) = std::fs::read_dir(&patches_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("patch") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(patch) = serde_json::from_str::<ta_workspace::AdvisorPatch>(&content) {
                    pending_advisor_patches.push(PendingAdvisorPatchInfo {
                        path: patch.path,
                        description: patch.description,
                        queued_at: patch.queued_at,
                    });
                }
            }
        }
    }

    (conflicts, pending_advisor_patches)
}

/// Find the first draft (`draft_seq == 1`) sharing the same `goal_shortref`
/// as `draft` and return its `supervisor_review`, if any. Returns `None` when
/// `draft` has no `goal_shortref` (older drafts, or single-draft goals where
/// `draft` itself is already the first) or the first draft can't be found.
fn find_initial_supervisor_review(
    pr_packages_dir: &std::path::Path,
    draft: &DraftPackage,
) -> Option<ta_changeset::supervisor_review::SupervisorReview> {
    let shortref = draft.goal_shortref.as_deref()?;
    if draft.draft_seq <= 1 {
        // This draft already is the first — its own supervisor_review (if
        // any) is the initial one; no separate lookup needed.
        return None;
    }
    let all = load_all_drafts(pr_packages_dir).ok()?;
    all.into_iter()
        .find(|d| d.goal_shortref.as_deref() == Some(shortref) && d.draft_seq == 1)
        .and_then(|d| d.supervisor_review)
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
        Ok(Some(draft)) => {
            let initial_supervisor_review =
                find_initial_supervisor_review(&state.pr_packages_dir, &draft);
            let project_root = project_root_from_pr_packages_dir(&state.pr_packages_dir);
            let (conflicts, pending_advisor_patches) =
                find_conflicts_and_patches(&project_root, uuid);
            Json(DraftDetailResponse {
                draft,
                initial_supervisor_review,
                conflicts,
                pending_advisor_patches,
            })
            .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "draft not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Query params for `GET /api/drafts/:id/artifact`.
#[derive(Debug, Deserialize)]
struct ArtifactContentQuery {
    uri: String,
}

/// `GET /api/drafts/:id/artifact?uri=<resource_uri>` — serve the raw bytes of
/// one artifact already listed on the draft, for inline image/video preview
/// in the draft review panel (v0.17.0.12.17 item 5: Studio previously showed
/// `ArtifactKind::Image`/`Video` artifacts as a bare file path despite the
/// backend already carrying width/height/format/duration metadata for them).
///
/// Resolves `resource_uri` (`fs://workspace/<path>`) against the goal's
/// staging workspace first (pre-apply — what the agent actually produced),
/// falling back to the project root (post-apply). Both the artifact-uri
/// membership check and a canonicalized-path prefix check guard against
/// serving anything outside those two directories.
async fn get_artifact_content(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
    Query(q): Query<ArtifactContentQuery>,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UUID").into_response(),
    };
    let draft = match load_draft(&state.pr_packages_dir, uuid) {
        Ok(Some(d)) => d,
        Ok(None) => return (StatusCode::NOT_FOUND, "draft not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let Some(artifact) = draft
        .changes
        .artifacts
        .iter()
        .find(|a| a.resource_uri == q.uri)
    else {
        return (StatusCode::NOT_FOUND, "artifact not found on this draft").into_response();
    };

    let Some(relative) = artifact.resource_uri.strip_prefix("fs://workspace/") else {
        return (StatusCode::BAD_REQUEST, "unsupported resource_uri scheme").into_response();
    };
    let relative_path = std::path::Path::new(relative);
    if relative_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    }

    let project_root = project_root_from_pr_packages_dir(&state.pr_packages_dir);

    let mut candidate_bases = Vec::new();
    let goals_dir = project_root.join(".ta").join("goals");
    if let Ok(store) = ta_goal::store::GoalRunStore::new(&goals_dir) {
        if let Ok(goals) = store.list() {
            if let Some(goal) = goals.iter().find(|g| g.pr_package_id == Some(uuid)) {
                candidate_bases.push(
                    project_root
                        .join(".ta")
                        .join("staging")
                        .join(goal.goal_run_id.to_string()),
                );
            }
        }
    }
    candidate_bases.push(project_root.clone());

    for base in &candidate_bases {
        let full_path = base.join(relative_path);
        let Ok(canonical) = full_path.canonicalize() else {
            continue;
        };
        let Ok(canonical_base) = base.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(&canonical_base) {
            continue; // Path traversal guard.
        }
        if let Ok(bytes) = std::fs::read(&canonical) {
            let content_type = artifact_content_type(artifact, relative_path);
            return ([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response();
        }
    }

    (StatusCode::NOT_FOUND, "artifact file not found on disk").into_response()
}

/// Best-effort MIME type for an artifact preview: prefer the declared
/// `ArtifactKind::Image`/`Video` format, fall back to a file-extension guess.
fn artifact_content_type(
    artifact: &ta_changeset::draft_package::Artifact,
    path: &std::path::Path,
) -> String {
    use ta_changeset::artifact_kind::ArtifactKind;
    if let Some(kind) = &artifact.kind {
        match kind {
            ArtifactKind::Image {
                format: Some(fmt), ..
            } => return format!("image/{}", fmt.to_lowercase()),
            ArtifactKind::Video {
                format: Some(fmt), ..
            } => return format!("video/{}", fmt.to_lowercase()),
            _ => {}
        }
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("png") => "image/png".to_string(),
        Some("jpg") | Some("jpeg") => "image/jpeg".to_string(),
        Some("gif") => "image/gif".to_string(),
        Some("webp") => "image/webp".to_string(),
        Some("svg") => "image/svg+xml".to_string(),
        Some("mp4") => "video/mp4".to_string(),
        Some("mov") => "video/quicktime".to_string(),
        Some("webm") => "video/webm".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Request body for `POST /api/drafts/:id/approve` (v0.17.0.12.9 item 7).
///
/// Mirrors `ApplyDraftRequest`: `selected_uris`, when present, is the set of
/// artifact `resource_uri`s left checked in the Studio "Changes" panel.
/// Omitting the field (or sending an empty body, as older callers do)
/// approves the draft without touching any artifact's disposition.
#[derive(Debug, Default, Deserialize)]
struct ApproveDraftRequest {
    #[serde(default)]
    selected_uris: Option<Vec<String>>,
}

async fn approve_draft(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UUID").into_response(),
    };

    let approve_request: ApproveDraftRequest = if body.is_empty() {
        ApproveDraftRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("Invalid JSON body for approve request: {}", e)
                    })),
                )
                    .into_response();
            }
        }
    };

    let status = DraftStatus::Approved {
        approved_by: "web-ui".into(),
        approved_at: Utc::now(),
    };
    match update_draft_status(
        &state.pr_packages_dir,
        uuid,
        status,
        approve_request.selected_uris.as_deref(),
    ) {
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

/// Request body for `POST /api/drafts/:id/apply` (v0.17.0.12.6 item 9).
///
/// `selected_uris`, when present, is the set of artifact `resource_uri`s the
/// human left checked in the Studio "Changes" panel. Artifacts *not* in this
/// list are excluded from the apply via `--reject <uri>`; everything in the
/// list (and anything unmentioned, when the list is absent) is approved via
/// `--approve rest`. Omitting the field (or sending an empty body, as older
/// callers do) preserves the original "apply everything" behavior.
#[derive(Debug, Default, Deserialize)]
struct ApplyDraftRequest {
    #[serde(default)]
    selected_uris: Option<Vec<String>>,
}

/// `POST /api/drafts/:id/apply` — Apply an approved or pending draft to the workspace.
///
/// Spawns `ta draft apply <short_id> --git-commit` as a background task and
/// returns immediately with `{"status": "pending", "job_id": "<uuid>"}`
/// (v0.17.0.12.5 item 1 — previously this blocked the HTTP response for up to
/// 120 seconds, which made Studio look hung on slow applies). Poll
/// `GET /api/apply-jobs/:job_id` for progress. Full stdout+stderr is written to
/// `.ta/logs/apply-<draft-short-id>-<timestamp>.log` regardless of outcome.
async fn apply_draft_endpoint(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let uuid = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid UUID").into_response(),
    };

    let apply_request: ApplyDraftRequest = if body.is_empty() {
        ApplyDraftRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": format!("Invalid JSON body for apply request: {}", e)
                    })),
                )
                    .into_response();
            }
        }
    };

    // Verify the draft exists and is in an appliable state.
    let draft = match load_draft(&state.pr_packages_dir, uuid) {
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
            draft
        }
    };

    // Translate the checkbox selection into --approve/--reject URI patterns
    // (item 9/10). Deselected files are excluded from the apply; everything
    // else (selected + unmentioned) is approved.
    let mut approve_patterns: Vec<String> = Vec::new();
    let mut reject_patterns: Vec<String> = Vec::new();
    if let Some(selected) = &apply_request.selected_uris {
        let selected: std::collections::HashSet<&str> =
            selected.iter().map(|s| s.as_str()).collect();
        for artifact in &draft.changes.artifacts {
            if !selected.contains(artifact.resource_uri.as_str()) {
                reject_patterns.push(artifact.resource_uri.clone());
            }
        }
        approve_patterns.push("rest".to_string());
    }

    let project_root = project_root_from_pr_packages_dir(&state.pr_packages_dir);

    let ta_bin = find_ta_binary_web();
    let short_id = id[..8.min(id.len())].to_string();

    let logs_dir = apply_logs_dir(&project_root);
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to create apply logs directory {}: {}", logs_dir.display(), e)
            })),
        )
            .into_response();
    }
    let log_path = logs_dir.join(format!(
        "apply-{}-{}.log",
        short_id,
        Utc::now().format("%Y%m%dT%H%M%SZ")
    ));

    let job_id = Uuid::new_v4().to_string();
    state.apply_jobs.lock().unwrap().insert(
        job_id.clone(),
        ApplyJobRecord {
            status: ApplyJobStatus::Running,
            output: String::new(),
            log_path: log_path.display().to_string(),
            commit_sha: None,
        },
    );

    let jobs = state.apply_jobs.clone();
    let job_id_task = job_id.clone();
    let log_path_task = log_path.clone();
    tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new(&ta_bin);
        cmd.arg("--project-root")
            .arg(&project_root)
            .arg("draft")
            .arg("apply")
            .arg(&short_id)
            .arg("--git-commit");
        for pattern in &reject_patterns {
            cmd.arg("--reject").arg(pattern);
        }
        for pattern in &approve_patterns {
            cmd.arg("--approve").arg(pattern);
        }
        let result = cmd.current_dir(&project_root).output().await;

        let (status, combined) = match result {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                let combined = format!("{}{}", stdout, stderr);
                let status = if out.status.success() {
                    ApplyJobStatus::Done
                } else {
                    ApplyJobStatus::Failed
                };
                (status, combined)
            }
            Err(e) => (
                ApplyJobStatus::Failed,
                format!("Failed to spawn ta: {}. Is `ta` on PATH?", e),
            ),
        };

        if let Err(e) = std::fs::write(&log_path_task, &combined) {
            tracing::warn!(
                path = %log_path_task.display(),
                error = %e,
                "Failed to write apply job log"
            );
        }

        let commit_sha = parse_commit_sha(&combined);
        if let Ok(mut map) = jobs.lock() {
            if let Some(job) = map.get_mut(&job_id_task) {
                job.status = status;
                job.output = tail_lines(&combined, 200);
                job.commit_sha = commit_sha;
            }
        }
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

/// `GET /api/apply-jobs/:job_id` — Poll the status of a background apply job
/// started via `POST /api/drafts/:id/apply` (v0.17.0.12.5 item 1).
async fn get_apply_job(
    State(state): State<Arc<WebState>>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let map = state.apply_jobs.lock().unwrap();
    match map.get(&job_id) {
        Some(job) => Json(job).into_response(),
        None => (StatusCode::NOT_FOUND, "apply job not found").into_response(),
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

    let selected_uris = body.selected_uris;
    let status = DraftStatus::Denied {
        reason: body.reason,
        denied_by: "web-ui".into(),
    };
    match update_draft_status(
        &state.pr_packages_dir,
        uuid,
        status,
        selected_uris.as_deref(),
    ) {
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

/// Update a draft's status and, when `selected_uris` is given, stamp each
/// artifact's `disposition` from the human's checkbox selection in Studio
/// (v0.17.0.12.9 item 7): artifacts named in `selected_uris` are marked
/// `Approved`, everything else `Rejected`. `None` leaves existing
/// dispositions untouched, preserving old callers that approve/deny a whole
/// draft without a per-file selection.
fn update_draft_status(
    dir: &std::path::Path,
    id: Uuid,
    status: DraftStatus,
    selected_uris: Option<&[String]>,
) -> Result<bool, std::io::Error> {
    let path = dir.join(format!("{}.json", id));
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&path)?;
    let mut draft: DraftPackage = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if let Some(selected) = selected_uris {
        let selected: std::collections::HashSet<&str> =
            selected.iter().map(|s| s.as_str()).collect();
        for artifact in &mut draft.changes.artifacts {
            artifact.disposition = if selected.contains(artifact.resource_uri.as_str()) {
                ArtifactDisposition::Approved
            } else {
                ArtifactDisposition::Rejected
            };
        }
    }
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
        apply_jobs: Arc::new(Mutex::new(HashMap::new())),
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
        .route("/api/drafts/{id}/artifact", get(get_artifact_content))
        .route("/api/drafts/{id}/approve", post(approve_draft))
        .route("/api/drafts/{id}/deny", post(deny_draft))
        .route("/api/drafts/{id}/apply", post(apply_draft_endpoint))
        .route("/api/apply-jobs/{job_id}", get(get_apply_job))
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
        apply_jobs: Arc::new(Mutex::new(HashMap::new())),
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

    // Ensure `.ta/logs/` exists and prune apply job logs older than 30 days
    // (v0.17.0.12.5 item 3).
    ensure_and_prune_logs_dir(&project_root, 30);

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

    /// v0.17.0.12.7: `find_conflicts_and_patches` must surface a conflict
    /// sidecar for the goal owning `package_id` and any project-wide queued
    /// advisor patches.
    #[test]
    fn find_conflicts_and_patches_reads_sidecars_and_patches() {
        let project = tempfile::tempdir().unwrap();
        let goals_dir = project.path().join(".ta").join("goals");
        let store = ta_goal::store::GoalRunStore::new(&goals_dir).unwrap();

        let package_id = Uuid::new_v4();
        let mut goal = ta_goal::GoalRun::new(
            "Test goal",
            "test",
            "test-agent",
            PathBuf::from("/tmp/staging"),
            goals_dir.join("placeholder"),
        );
        goal.pr_package_id = Some(package_id);
        store.save(&goal).unwrap();

        let conflicts_dir = goals_dir
            .join(goal.goal_run_id.to_string())
            .join("conflicts");
        std::fs::create_dir_all(&conflicts_dir).unwrap();
        std::fs::write(
            conflicts_dir.join("memory__notes.md.conflict"),
            "<<<<<<< ours\nfoo\n=======\nbar\n>>>>>>> theirs\n",
        )
        .unwrap();

        let patches_dir = ta_workspace::advisor_patch::patches_dir(project.path());
        std::fs::create_dir_all(&patches_dir).unwrap();
        let patch = ta_workspace::AdvisorPatch {
            path: "PLAN.md".to_string(),
            old_content_b64: String::new(),
            new_content_b64: String::new(),
            description: "add plan phase v0.18.0".to_string(),
            queued_at: 1735900000,
        };
        std::fs::write(
            patches_dir.join("1735900000-add-plan-phase.patch"),
            serde_json::to_string(&patch).unwrap(),
        )
        .unwrap();

        let (conflicts, patches) = find_conflicts_and_patches(project.path(), package_id);

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "memory/notes.md");
        assert!(conflicts[0].content.contains("<<<<<<<"));

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, "PLAN.md");
        assert_eq!(patches[0].description, "add plan phase v0.18.0");
        assert_eq!(patches[0].queued_at, 1735900000);
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
                Request::get("/api/apply-jobs/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// v0.17.0.12.5 item 1: apply must return immediately with a job id instead
    /// of blocking the HTTP response on the subprocess.
    #[tokio::test]
    async fn apply_draft_returns_pending_job_id_immediately() {
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
        let job_id = json["job_id"].as_str().expect("job_id present");
        Uuid::parse_str(job_id).expect("job_id is a valid UUID");
    }

    #[tokio::test]
    async fn apply_draft_accepts_selected_uris_body() {
        // v0.17.0.12.6 item 9: per-file selection body is accepted and still
        // returns the same pending-job response shape.
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        write_draft_json(
            &packages_dir,
            id,
            serde_json::json!({
                "changes": {
                    "artifacts": [
                        {"resource_uri": "fs://workspace/a.rs", "change_type": "modify", "diff_ref": "diff-a"},
                        {"resource_uri": "fs://workspace/b.rs", "change_type": "modify", "diff_ref": "diff-b"}
                    ],
                    "patch_sets": [],
                    "pending_actions": []
                }
            }),
        );

        let app = build_router(packages_dir);
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/apply", id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"selected_uris": ["fs://workspace/a.rs"]}).to_string(),
                    ))
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
            StatusCode::ACCEPTED,
            "body: {}",
            String::from_utf8_lossy(&body_bytes)
        );
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["status"], "pending");
    }

    #[test]
    fn tail_lines_keeps_short_output_unchanged() {
        let s = "a\nb\nc";
        assert_eq!(super::tail_lines(s, 10), s);
    }

    #[test]
    fn tail_lines_truncates_to_last_n_lines() {
        let s = (0..10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(super::tail_lines(&s, 3), "7\n8\n9");
    }

    #[test]
    fn ensure_and_prune_logs_dir_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        super::ensure_and_prune_logs_dir(dir.path(), 30);
        assert!(dir.path().join(".ta").join("logs").is_dir());
    }

    #[test]
    fn ensure_and_prune_logs_dir_keeps_recent_logs() {
        let dir = tempfile::tempdir().unwrap();
        let logs_dir = dir.path().join(".ta").join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();

        let recent_log = logs_dir.join("apply-recent-20260101T000000Z.log");
        std::fs::write(&recent_log, "recent").unwrap();
        // Non-`.log` files must never be touched by pruning.
        let other_file = logs_dir.join("notes.txt");
        std::fs::write(&other_file, "notes").unwrap();

        super::ensure_and_prune_logs_dir(dir.path(), 30);

        assert!(recent_log.exists());
        assert!(other_file.exists());
    }

    #[test]
    fn ensure_and_prune_logs_dir_removes_logs_older_than_retention() {
        let dir = tempfile::tempdir().unwrap();
        let logs_dir = dir.path().join(".ta").join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();

        let old_log = logs_dir.join("apply-old-20200101T000000Z.log");
        std::fs::write(&old_log, "old").unwrap();
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86400);
        // Windows requires write-access on the file handle to call SetFileTime.
        // File::open (read-only) returns PermissionDenied (error 5) on Windows CI.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&old_log)
            .unwrap()
            .set_modified(old_time)
            .expect("platform supports setting mtime");

        super::ensure_and_prune_logs_dir(dir.path(), 30);

        assert!(
            !old_log.exists(),
            "log older than retention should be pruned"
        );
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

    // ── Artifact content preview (v0.17.0.12.17 item 5) ───────────────────

    /// project_root = pr_packages_dir.parent().parent(), so tests that need
    /// project-root-relative file resolution must nest packages under
    /// `<root>/.ta/pr_packages` (matching the real on-disk layout), not the
    /// flat `<root>/packages` used by tests that don't touch project_root.
    fn test_router_with_project_root(project_root: &std::path::Path) -> Router {
        let packages_dir = project_root.join(".ta").join("pr_packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        build_router(packages_dir)
    }

    fn image_artifact_draft_json(id: Uuid, uri: &str) -> serde_json::Value {
        let mut v = minimal_draft_json(id);
        v.as_object_mut().unwrap().insert(
            "changes".to_string(),
            serde_json::json!({
                "artifacts": [{
                    "resource_uri": uri,
                    "change_type": "add",
                    "diff_ref": "changeset:0",
                    "kind": {"type": "image", "format": "PNG"}
                }],
                "patch_sets": [],
                "pending_actions": []
            }),
        );
        v
    }

    #[tokio::test]
    async fn get_artifact_content_serves_bytes_from_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        let draft = image_artifact_draft_json(id, "fs://workspace/output.png");
        std::fs::write(
            packages_dir.join(format!("{}.json", id)),
            serde_json::to_string_pretty(&draft).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join("output.png"), b"fake-png-bytes").unwrap();

        let app = test_router_with_project_root(dir.path());
        let resp = app
            .oneshot(
                Request::get(format!(
                    "/api/drafts/{}/artifact?uri=fs%3A%2F%2Fworkspace%2Foutput.png",
                    id
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "image/png");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"fake-png-bytes");
    }

    #[tokio::test]
    async fn get_artifact_content_prefers_goal_staging_dir_over_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        let draft = image_artifact_draft_json(id, "fs://workspace/output.png");
        std::fs::write(
            packages_dir.join(format!("{}.json", id)),
            serde_json::to_string_pretty(&draft).unwrap(),
        )
        .unwrap();

        // Register the owning goal, pointed at this package.
        let goals_dir = dir.path().join(".ta").join("goals");
        let store = ta_goal::store::GoalRunStore::new(&goals_dir).unwrap();
        let mut goal = ta_goal::GoalRun::new(
            "Test goal",
            "test",
            "test-agent",
            dir.path().join(".ta/staging/placeholder"),
            goals_dir.join("placeholder"),
        );
        goal.pr_package_id = Some(id);
        store.save(&goal).unwrap();

        let staging_dir = dir
            .path()
            .join(".ta")
            .join("staging")
            .join(goal.goal_run_id.to_string());
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::write(staging_dir.join("output.png"), b"staging-bytes").unwrap();
        // Different content at the project root — staging must win.
        std::fs::write(dir.path().join("output.png"), b"applied-bytes").unwrap();

        let app = test_router_with_project_root(dir.path());
        let resp = app
            .oneshot(
                Request::get(format!(
                    "/api/drafts/{}/artifact?uri=fs%3A%2F%2Fworkspace%2Foutput.png",
                    id
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"staging-bytes");
    }

    #[tokio::test]
    async fn get_artifact_content_rejects_uri_not_on_draft() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        let draft = image_artifact_draft_json(id, "fs://workspace/output.png");
        std::fs::write(
            packages_dir.join(format!("{}.json", id)),
            serde_json::to_string_pretty(&draft).unwrap(),
        )
        .unwrap();
        // A file that exists on disk but was never listed as an artifact on
        // this draft must not be servable through it.
        std::fs::write(dir.path().join("secret.txt"), b"nope").unwrap();

        let app = test_router_with_project_root(dir.path());
        let resp = app
            .oneshot(
                Request::get(format!(
                    "/api/drafts/{}/artifact?uri=fs%3A%2F%2Fworkspace%2Fsecret.txt",
                    id
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_artifact_content_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join(".ta").join("pr_packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        let draft = image_artifact_draft_json(id, "fs://workspace/../../etc/passwd");
        std::fs::write(
            packages_dir.join(format!("{}.json", id)),
            serde_json::to_string_pretty(&draft).unwrap(),
        )
        .unwrap();

        let app = test_router_with_project_root(dir.path());
        let resp = app
            .oneshot(
                Request::get(format!(
                    "/api/drafts/{}/artifact?uri=fs%3A%2F%2Fworkspace%2F..%2F..%2Fetc%2Fpasswd",
                    id
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
    async fn draft_detail_returns_initial_supervisor_review_for_followup_draft() {
        // v0.17.0.12.6 item 7: a follow-up draft (draft_seq 2) that didn't
        // re-run the supervisor should still surface the *first* draft's
        // supervisor review via `initial_supervisor_review`.
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();

        let first_id = Uuid::new_v4();
        write_draft_json(
            &packages_dir,
            first_id,
            serde_json::json!({
                "goal_shortref": "abc123",
                "draft_seq": 1,
                "supervisor_review": {
                    "verdict": "pass",
                    "scope_ok": true,
                    "findings": ["Initial review"],
                    "summary": "First draft looked fine",
                    "agent": "builtin",
                    "duration_secs": 1.0
                }
            }),
        );

        let followup_id = Uuid::new_v4();
        write_draft_json(
            &packages_dir,
            followup_id,
            serde_json::json!({
                "goal_shortref": "abc123",
                "draft_seq": 2,
            }),
        );

        let app = build_router(packages_dir);
        let resp = app
            .oneshot(
                Request::get(format!("/api/drafts/{}", followup_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let val: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        // The follow-up draft has no supervisor_review of its own.
        assert!(val["supervisor_review"].is_null());
        // But the first draft's review is surfaced separately.
        assert_eq!(
            val["initial_supervisor_review"]["summary"],
            serde_json::json!("First draft looked fine")
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
    async fn approve_draft_stamps_disposition_from_selected_uris() {
        // v0.17.0.12.9 item 7: Approve now acts on the same per-file checkbox
        // selection Apply already respects — selected artifacts are marked
        // Approved, deselected ones Rejected.
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        write_draft_json(
            &packages_dir,
            id,
            serde_json::json!({
                "changes": {
                    "artifacts": [
                        {"resource_uri": "fs://workspace/a.rs", "change_type": "modify", "diff_ref": "diff-a"},
                        {"resource_uri": "fs://workspace/b.rs", "change_type": "modify", "diff_ref": "diff-b"}
                    ],
                    "patch_sets": [],
                    "pending_actions": []
                }
            }),
        );

        let app = build_router(packages_dir.clone());
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/approve", id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"selected_uris": ["fs://workspace/a.rs"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let pkg_path = packages_dir.join(format!("{}.json", id));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(pkg_path).unwrap()).unwrap();
        let artifacts = on_disk["changes"]["artifacts"].as_array().unwrap();
        let a = artifacts
            .iter()
            .find(|a| a["resource_uri"] == "fs://workspace/a.rs")
            .unwrap();
        let b = artifacts
            .iter()
            .find(|a| a["resource_uri"] == "fs://workspace/b.rs")
            .unwrap();
        assert_eq!(a["disposition"], serde_json::json!("approved"));
        assert_eq!(b["disposition"], serde_json::json!("rejected"));
    }

    #[tokio::test]
    async fn approve_draft_without_body_leaves_dispositions_untouched() {
        // Back-compat: older callers (and the "approve everything" button)
        // send no selection at all — dispositions stay at their default.
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        write_draft_json(
            &packages_dir,
            id,
            serde_json::json!({
                "changes": {
                    "artifacts": [
                        {"resource_uri": "fs://workspace/a.rs", "change_type": "modify", "diff_ref": "diff-a"}
                    ],
                    "patch_sets": [],
                    "pending_actions": []
                }
            }),
        );

        let app = build_router(packages_dir.clone());
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/approve", id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let pkg_path = packages_dir.join(format!("{}.json", id));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(pkg_path).unwrap()).unwrap();
        assert_eq!(
            on_disk["changes"]["artifacts"][0]["disposition"],
            serde_json::json!("pending")
        );
    }

    #[tokio::test]
    async fn deny_draft_stamps_disposition_from_selected_uris() {
        let dir = tempfile::tempdir().unwrap();
        let packages_dir = dir.path().join("packages");
        std::fs::create_dir_all(&packages_dir).unwrap();
        let id = Uuid::new_v4();
        write_draft_json(
            &packages_dir,
            id,
            serde_json::json!({
                "changes": {
                    "artifacts": [
                        {"resource_uri": "fs://workspace/a.rs", "change_type": "modify", "diff_ref": "diff-a"},
                        {"resource_uri": "fs://workspace/b.rs", "change_type": "modify", "diff_ref": "diff-b"}
                    ],
                    "patch_sets": [],
                    "pending_actions": []
                }
            }),
        );

        let app = build_router(packages_dir.clone());
        let resp = app
            .oneshot(
                Request::post(format!("/api/drafts/{}/deny", id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "reason": "not what I asked for",
                            "selected_uris": ["fs://workspace/b.rs"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let pkg_path = packages_dir.join(format!("{}.json", id));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(pkg_path).unwrap()).unwrap();
        let artifacts = on_disk["changes"]["artifacts"].as_array().unwrap();
        let a = artifacts
            .iter()
            .find(|a| a["resource_uri"] == "fs://workspace/a.rs")
            .unwrap();
        let b = artifacts
            .iter()
            .find(|a| a["resource_uri"] == "fs://workspace/b.rs")
            .unwrap();
        assert_eq!(a["disposition"], serde_json::json!("rejected"));
        assert_eq!(b["disposition"], serde_json::json!("approved"));
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
