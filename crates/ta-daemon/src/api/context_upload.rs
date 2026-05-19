// api/context_upload.rs — Context file upload endpoint for Studio (v0.15.30.7).
//
// POST /api/context/upload
//   Accepts a plain-text body, writes it to a temporary file in .ta/tmp/,
//   and returns the server-side path so the caller can pass it as
//   `--context <path>` when dispatching a `run` command.
//
// The temp files are not automatically cleaned up — they live until the next
// daemon restart or until the user's OS tmp-cleanup runs.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

use crate::api::AppState;

/// Response from a successful context upload.
#[derive(Debug, Serialize)]
pub struct ContextUploadResponse {
    /// Absolute path on the server that can be passed as `--context <path>`.
    pub path: String,
    /// Byte count of the uploaded content.
    pub size: usize,
}

/// `POST /api/context/upload` — Upload context text and receive a server-side path.
///
/// Body: raw UTF-8 text (build logs, stack traces, test output, etc.)
/// Returns: `{ "path": "/abs/path/to/tmp/ctx-<uuid>.txt", "size": <N> }`
pub async fn upload_context(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let content = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "context body must be valid UTF-8" })),
            )
                .into_response();
        }
    };

    // Write to .ta/tmp/ inside the project root so the agent can read it from
    // staging without any path translation.
    let tmp_dir = state.project_root.join(".ta").join("tmp");
    if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
        tracing::warn!(dir = %tmp_dir.display(), error = %e, "Failed to create .ta/tmp");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("failed to create tmp dir: {}", e) })),
        )
            .into_response();
    }

    let filename = format!("ctx-{}.txt", uuid::Uuid::new_v4());
    let path = tmp_dir.join(&filename);

    if let Err(e) = std::fs::write(&path, content) {
        tracing::warn!(path = %path.display(), error = %e, "Failed to write context file");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("failed to write context file: {}", e) })),
        )
            .into_response();
    }

    let size = content.len();
    tracing::debug!(path = %path.display(), size, "Context file uploaded");

    Json(ContextUploadResponse {
        path: path.display().to_string(),
        size,
    })
    .into_response()
}
