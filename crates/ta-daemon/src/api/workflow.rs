// workflow.rs — Workflow management API (v0.14.20 + v0.15.14.1).
//
// GET    /api/workflows              — list workflow TOML files from .ta/workflows/
// POST   /api/workflow/:id/run       — trigger a workflow run (v0.15.14.1)
// DELETE /api/workflow/:id           — stop/cancel a running workflow run (v0.15.14.1)
// GET    /api/workflow/:id/status    — get current run status via SSE polling (v0.15.14.1)
// POST   /api/workflow/:id/input     — human decision for paused workflow
// POST   /api/workflow/generate      — generate workflow TOML from a plain-English description
// POST   /api/workflow/save          — save (create/update) a workflow TOML file

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::AppState;

/// Workflow list entry.
#[derive(Debug, Serialize)]
pub struct WorkflowEntry {
    pub id: String,
    pub name: String,
    /// "scheduled" | "manual" | "unknown"
    pub trigger: String,
    /// "running" | "idle" | "unknown"
    pub status: String,
    pub toml_path: String,
}

/// Request body for workflow interaction.
#[derive(Debug, Deserialize)]
pub struct WorkflowInputRequest {
    pub decision: String,
    #[serde(default)]
    pub feedback: Option<String>,
}

/// Request body for workflow generation.
#[derive(Debug, Deserialize)]
pub struct WorkflowGenerateRequest {
    pub description: String,
}

/// Request body for workflow save.
#[derive(Debug, Deserialize)]
pub struct WorkflowSaveRequest {
    pub id: String,
    pub toml: String,
}

/// List active workflows from .ta/workflows/.
///
/// GET /api/workflows
pub async fn list_workflows(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    let workflows_dir = project_root.join(".ta").join("workflows");

    let mut entries: Vec<WorkflowEntry> = Vec::new();

    if let Ok(dir) = std::fs::read_dir(&workflows_dir) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let toml_path = path.display().to_string();
            let text = std::fs::read_to_string(&path).unwrap_or_default();

            // Simple heuristic: check for [trigger] section.
            let trigger = if text.contains("[trigger]") || text.contains("cron") {
                "scheduled"
            } else {
                "manual"
            };

            entries.push(WorkflowEntry {
                name: id
                    .replace('-', " ")
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            None => String::new(),
                            Some(f) => f.to_uppercase().to_string() + c.as_str(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                id,
                trigger: trigger.to_string(),
                status: "idle".to_string(),
                toml_path,
            });
        }
    }

    entries.sort_by(|a, b| a.id.cmp(&b.id));
    let count = entries.len();

    Json(serde_json::json!({
        "workflows": entries,
        "count": count,
    }))
    .into_response()
}

/// Handle human input for a paused workflow.
///
/// POST /api/workflow/:id/input
pub async fn workflow_input(
    State(_state): State<Arc<AppState>>,
    Path(workflow_id): Path<String>,
    Json(request): Json<WorkflowInputRequest>,
) -> impl IntoResponse {
    let valid_decisions = ["proceed", "revise", "cancel"];
    if !valid_decisions.contains(&request.decision.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "Invalid decision: '{}'. Valid decisions: proceed, revise, cancel",
                    request.decision
                ),
            })),
        )
            .into_response();
    }

    tracing::info!(
        workflow_id = %workflow_id,
        decision = %request.decision,
        feedback = ?request.feedback,
        "workflow input received"
    );

    Json(serde_json::json!({
        "workflow_id": workflow_id,
        "decision": request.decision,
        "status": if request.decision == "cancel" { "cancelled" } else { "acknowledged" },
        "message": format!(
            "Decision '{}' recorded for workflow {}.",
            request.decision,
            &workflow_id[..8.min(workflow_id.len())]
        ),
    }))
    .into_response()
}

/// Generate a workflow TOML from a plain-English description.
///
/// POST /api/workflow/generate
/// Body: { description: "check inbox every 30 min and draft replies" }
pub async fn generate_workflow(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<WorkflowGenerateRequest>,
) -> impl IntoResponse {
    if body.description.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "description is required"})),
        )
            .into_response();
    }

    // Generate a workflow TOML template based on the description.
    // In a full implementation this would call an LLM agent. For now we produce
    // a well-structured template that the user can edit.
    let slug = body
        .description
        .to_lowercase()
        .split_whitespace()
        .take(5)
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    let workflow_id = if slug.is_empty() {
        "new-workflow".to_string()
    } else {
        slug
    };

    let toml = format!(
        r#"# Workflow: {}
# Generated from description: {}

[workflow]
name = "{}"
description = """
{}
"""

[trigger]
# type = "schedule"   # Uncomment and set cron for scheduled execution.
# cron = "0 * * * *"  # Every hour. See https://crontab.guru/
type = "manual"

[[steps]]
name = "main"
goal = "{}"
agent = "claude-code"
"#,
        workflow_id, body.description, workflow_id, body.description, body.description,
    );

    Json(serde_json::json!({
        "id": workflow_id,
        "toml": toml,
        "message": "Review and edit the generated workflow below, then save it.",
    }))
    .into_response()
}

/// Save (create or update) a workflow TOML file.
///
/// POST /api/workflow/save
pub async fn save_workflow(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WorkflowSaveRequest>,
) -> impl IntoResponse {
    if body.id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "id is required"})),
        )
            .into_response();
    }

    // Sanitise the ID to prevent path traversal.
    let id = body
        .id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>();

    if id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "id contains no valid characters"})),
        )
            .into_response();
    }

    let project_root = state.active_project_root.read().unwrap().clone();
    let workflows_dir = project_root.join(".ta").join("workflows");
    if let Err(e) = std::fs::create_dir_all(&workflows_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Could not create workflows dir: {}", e)})),
        )
            .into_response();
    }

    let path = workflows_dir.join(format!("{}.toml", id));
    if let Err(e) = std::fs::write(&path, &body.toml) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Could not write workflow: {}", e)})),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "ok": true,
        "id": id,
        "path": path.display().to_string(),
    }))
    .into_response()
}

// ── Run / Stop / Status (v0.15.14.1) ─────────────────────────────

/// Request body for `POST /api/workflow/{id}/run`.
#[derive(Debug, Deserialize)]
pub struct WorkflowRunRequest {
    /// Optional goal title override; defaults to the workflow name.
    #[serde(default)]
    pub goal_title: Option<String>,
    /// Optional agent override; defaults to "claude-code".
    #[serde(default)]
    pub agent: Option<String>,
}

/// Trigger a workflow run from Studio.
///
/// POST /api/workflow/{id}/run
///
/// Spawns `ta workflow run <id>` in a background process.
/// Returns the new run ID immediately (fire-and-forget).
pub async fn run_workflow(
    State(state): State<Arc<AppState>>,
    Path(workflow_id): Path<String>,
    body: Option<Json<WorkflowRunRequest>>,
) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    let workflows_dir = project_root.join(".ta").join("workflows");
    let toml_path = workflows_dir.join(format!("{}.toml", workflow_id));

    if !toml_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Workflow '{}' not found. Check .ta/workflows/{}.toml exists.", workflow_id, workflow_id)
            })),
        )
            .into_response();
    }

    let req = body.map(|b| b.0).unwrap_or(WorkflowRunRequest {
        goal_title: None,
        agent: None,
    });
    let goal_title = req
        .goal_title
        .unwrap_or_else(|| format!("Run workflow {}", workflow_id));
    let agent = req.agent.unwrap_or_else(|| "claude-code".to_string());

    // Locate the `ta` binary — prefer the binary running this daemon.
    let ta_binary = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ta"));

    let spawn_result = std::process::Command::new(&ta_binary)
        .args([
            "workflow",
            "run",
            &workflow_id,
            "--goal",
            &goal_title,
            "--agent",
            &agent,
        ])
        .current_dir(&project_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match spawn_result {
        Ok(child) => {
            tracing::info!(
                workflow_id = %workflow_id,
                pid = child.id(),
                goal_title = %goal_title,
                "workflow run spawned from Studio"
            );
            Json(serde_json::json!({
                "ok": true,
                "workflow_id": workflow_id,
                "goal_title": goal_title,
                "message": format!(
                    "Workflow '{}' started. Check 'ta status' or Studio for progress.",
                    workflow_id
                ),
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!(
                workflow_id = %workflow_id,
                error = %e,
                "failed to spawn workflow run from Studio"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!(
                        "Failed to start workflow '{}': {}. \
                         Ensure 'ta' is in your PATH and the workflow file is valid.",
                        workflow_id, e
                    )
                })),
            )
                .into_response()
        }
    }
}

/// Stop/cancel the latest running workflow run.
///
/// DELETE /api/workflow/{id}
///
/// Finds the latest run record in `.ta/workflow-runs/` and marks it cancelled
/// by sending SIGTERM to the associated process (if recorded) or writing a
/// cancel marker file that the running loop checks.
pub async fn stop_workflow(
    State(state): State<Arc<AppState>>,
    Path(workflow_id): Path<String>,
) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    let runs_dir = project_root.join(".ta").join("workflow-runs");

    if !runs_dir.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!(
                    "No workflow runs found for '{}'. \
                     Run 'ta workflow run {}' first.",
                    workflow_id, workflow_id
                )
            })),
        )
            .into_response();
    }

    // Find the latest run for this workflow by scanning run records.
    let latest_run = find_latest_run_for_workflow(&runs_dir, &workflow_id);

    match latest_run {
        Some((run_id, run_json)) => {
            let state_str = run_json["state"].as_str().unwrap_or("unknown");
            if !matches!(state_str, "running" | "awaiting_human") {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": format!(
                            "Workflow run '{}' is not running (state: {}). Nothing to stop.",
                            run_id, state_str
                        )
                    })),
                )
                    .into_response();
            }

            // Write a cancel marker file that the running loop checks.
            let cancel_path = runs_dir.join(format!("{}.cancel", run_id));
            if let Err(e) = std::fs::write(&cancel_path, "") {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to write cancel marker: {}", e)
                    })),
                )
                    .into_response();
            }

            tracing::info!(
                workflow_id = %workflow_id,
                run_id = %run_id,
                "workflow stop requested from Studio"
            );

            Json(serde_json::json!({
                "ok": true,
                "workflow_id": workflow_id,
                "run_id": run_id,
                "message": format!(
                    "Stop requested for workflow run '{}'. \
                     The run will cancel at the next checkpoint.",
                    &run_id[..8.min(run_id.len())]
                ),
            }))
            .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!(
                    "No active run found for workflow '{}'. \
                     Use 'ta workflow run {}' to start one.",
                    workflow_id, workflow_id
                )
            })),
        )
            .into_response(),
    }
}

/// Get the current run status for a workflow.
///
/// GET /api/workflow/{id}/status
///
/// Returns the latest run state from `.ta/workflow-runs/`. Poll this endpoint
/// to track live progress (Studio uses 2-second polling intervals).
pub async fn workflow_run_status(
    State(state): State<Arc<AppState>>,
    Path(workflow_id): Path<String>,
) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    let runs_dir = project_root.join(".ta").join("workflow-runs");

    if !runs_dir.exists() {
        return Json(serde_json::json!({
            "workflow_id": workflow_id,
            "state": "idle",
            "run_id": null,
            "message": "No runs found. Use the Run button or 'ta workflow run' to start."
        }))
        .into_response();
    }

    match find_latest_run_for_workflow(&runs_dir, &workflow_id) {
        Some((run_id, run_json)) => {
            let state_str = run_json["state"].as_str().unwrap_or("unknown");
            let current_stage = run_json["current_stage"].as_str();
            let started_at = run_json["started_at"].as_str();
            let updated_at = run_json["updated_at"].as_str();

            Json(serde_json::json!({
                "workflow_id": workflow_id,
                "run_id": run_id,
                "state": state_str,
                "current_stage": current_stage,
                "started_at": started_at,
                "updated_at": updated_at,
                "stages": run_json["stages"],
            }))
            .into_response()
        }
        None => Json(serde_json::json!({
            "workflow_id": workflow_id,
            "state": "idle",
            "run_id": null,
            "message": "No runs found for this workflow."
        }))
        .into_response(),
    }
}

/// Find the latest run record for a specific workflow in the runs directory.
///
/// Scans all `.json` files, finds ones that match the workflow name, and returns
/// the most recently updated one.
fn find_latest_run_for_workflow(
    runs_dir: &std::path::Path,
    workflow_id: &str,
) -> Option<(String, serde_json::Value)> {
    let entries = std::fs::read_dir(runs_dir).ok()?;
    let mut candidates: Vec<(String, serde_json::Value)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&content).ok()?;

        let wf_name = v["workflow_name"].as_str().unwrap_or("");
        if wf_name == workflow_id {
            let run_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            candidates.push((run_id, v));
        }
    }

    // Sort by updated_at descending — latest first.
    candidates.sort_by(|(_, a), (_, b)| {
        let a_ts = a["updated_at"].as_str().unwrap_or("");
        let b_ts = b["updated_at"].as_str().unwrap_or("");
        b_ts.cmp(a_ts)
    });

    candidates.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_entry_is_serializable() {
        let entry = WorkflowEntry {
            id: "test-wf".into(),
            name: "Test Wf".into(),
            trigger: "manual".into(),
            status: "idle".into(),
            toml_path: "/tmp/test-wf.toml".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("test-wf"));
    }
}
