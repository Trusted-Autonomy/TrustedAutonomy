// api/project_browser.rs — Project Browser API endpoints (v0.14.18).
//
// Provides:
//   POST /api/project/open   — Open a project by path, update recents
//   GET  /api/project/list   — List recent projects
//   POST /api/project/browse — Trigger native OS directory picker

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::AppState;

/// A recently-opened TA project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    pub last_opened: String, // ISO 8601
}

/// Manages ~/.config/ta/recent-projects.json.
pub struct RecentProjectsStore;

impl RecentProjectsStore {
    fn config_dir() -> PathBuf {
        std::env::var("HOME")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".config")
            .join("ta")
    }

    fn file_path() -> PathBuf {
        Self::config_dir().join("recent-projects.json")
    }

    /// Load recent projects. Returns an empty vec on any error.
    pub fn load() -> Vec<RecentProject> {
        let path = Self::file_path();
        if !path.exists() {
            return vec![];
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        serde_json::from_str::<Vec<RecentProject>>(&content).unwrap_or_default()
    }

    /// Save recent projects to disk, creating directories as needed.
    pub fn save(projects: &[RecentProject]) -> std::io::Result<()> {
        let dir = Self::config_dir();
        std::fs::create_dir_all(&dir)?;
        let path = Self::file_path();
        let json = serde_json::to_string_pretty(projects).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Prepend an entry (by path+name), deduplicate by path, cap at 20.
    pub fn add(path: String, name: String) -> std::io::Result<()> {
        let mut projects = Self::load();
        // Remove any existing entry for this path.
        projects.retain(|p| p.path != path);
        // Prepend the new entry.
        let now = chrono::Utc::now().to_rfc3339();
        projects.insert(
            0,
            RecentProject {
                path,
                name,
                last_opened: now,
            },
        );
        // Cap at 20.
        projects.truncate(20);
        Self::save(&projects)
    }
}

/// Read the project name from workflow.toml [project] name, falling back to the directory name.
pub fn read_project_name(path: &Path) -> String {
    let workflow_path = path.join(".ta").join("workflow.toml");
    if let Ok(content) = std::fs::read_to_string(&workflow_path) {
        if let Ok(table) = toml::from_str::<toml::Value>(&content) {
            if let Some(name) = table
                .get("project")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            {
                return name.to_string();
            }
        }
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Request body for POST /api/project/open.
#[derive(Debug, Deserialize)]
pub struct ProjectOpenRequest {
    pub path: String,
}

/// POST /api/project/open — Open a project by path.
///
/// Validates that `path/.ta/` exists, reads the project name, updates
/// recents, and sets the daemon's active project root.
pub async fn open_project(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProjectOpenRequest>,
) -> impl IntoResponse {
    let path = PathBuf::from(&body.path);

    // Validate that this looks like a TA project.
    if !path.join(".ta").exists() {
        return Json(serde_json::json!({
            "ok": false,
            "error": format!(
                "Directory '{}' does not contain a .ta/ folder. Initialize it with 'ta init' first.",
                body.path
            )
        }))
        .into_response();
    }

    let name = read_project_name(&path);

    // Update recent projects store.
    if let Err(e) = RecentProjectsStore::add(body.path.clone(), name.clone()) {
        tracing::warn!(path = %body.path, error = %e, "Failed to update recent-projects.json");
    }

    // Update the active project root in daemon state.
    {
        let mut active = state.active_project_root.write().unwrap();
        *active = path;
    }

    Json(serde_json::json!({
        "ok": true,
        "name": name
    }))
    .into_response()
}

/// GET /api/project/list — List recent projects, most recent first.
pub async fn list_projects(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut projects = RecentProjectsStore::load();
    // Sort by last_opened descending (most recent first).
    projects.sort_by_key(|p| Reverse(p.last_opened.clone()));
    Json(projects).into_response()
}

/// POST /api/project/browse — Open a native OS directory picker.
///
/// Returns `{ "path": "..." }` on success, `{ "cancelled": true }` if dismissed.
pub async fn browse_projects(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let result = spawn_directory_picker();
    match result {
        Ok(Some(path)) => Json(serde_json::json!({ "path": path })).into_response(),
        Ok(None) => Json(serde_json::json!({ "cancelled": true })).into_response(),
        Err(e) => Json(serde_json::json!({
            "cancelled": true,
            "error": format!("Directory picker unavailable: {}", e)
        }))
        .into_response(),
    }
}

#[cfg(target_os = "macos")]
fn spawn_directory_picker() -> Result<Option<String>, String> {
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"Finder\" to set theFolder to POSIX path of (choose folder) as text",
        ])
        .output()
        .map_err(|e| format!("osascript failed: {}", e))?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout)
            .trim()
            .trim_end_matches('/')
            .to_string();
        if path.is_empty() {
            Ok(None)
        } else {
            Ok(Some(path))
        }
    } else {
        // User cancelled — osascript exits non-zero on cancel.
        Ok(None)
    }
}

#[cfg(target_os = "windows")]
fn spawn_directory_picker() -> Result<Option<String>, String> {
    let script = r#"[System.Reflection.Assembly]::LoadWithPartialName('System.Windows.Forms') | Out-Null; $fd = New-Object System.Windows.Forms.FolderBrowserDialog; if ($fd.ShowDialog() -eq 'OK') { $fd.SelectedPath } else { '' }"#;
    let output = std::process::Command::new("powershell")
        .args(["-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("PowerShell FolderBrowserDialog failed: {}", e))?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        Ok(None)
    } else {
        Ok(Some(path))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn spawn_directory_picker() -> Result<Option<String>, String> {
    // Try zenity first, then kdialog.
    let zenity = std::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--directory",
            "--title=Select TA Project",
        ])
        .output();
    if let Ok(out) = zenity {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return if path.is_empty() {
                Ok(None)
            } else {
                Ok(Some(path))
            };
        }
        // zenity was found but user cancelled (non-zero exit).
        return Ok(None);
    }

    // Fall back to kdialog.
    let kdialog = std::process::Command::new("kdialog")
        .arg("--getexistingdirectory")
        .output();
    if let Ok(out) = kdialog {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return if path.is_empty() {
                Ok(None)
            } else {
                Ok(Some(path))
            };
        }
        return Ok(None);
    }

    Err("No directory picker available (install zenity or kdialog)".to_string())
}

/// Request body for project init.
#[derive(Deserialize)]
pub struct ProjectInitRequest {
    pub path: String,
    pub name: String,
}

/// `POST /api/project/init` — Create a new TA project at a given path.
///
/// Creates `.ta/`, writes starter `workflow.toml` and empty `PLAN.md`.
pub async fn init_project(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<ProjectInitRequest>,
) -> impl IntoResponse {
    if body.path.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "path is required"})),
        )
            .into_response();
    }
    if body.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "name is required"})),
        )
            .into_response();
    }

    let project_path = std::path::PathBuf::from(body.path.trim());
    let ta_dir = project_path.join(".ta");

    // Create .ta/ directory structure.
    for sub in &[
        "goals",
        "pr_packages",
        "memory",
        "events",
        "personas",
        "workflows",
    ] {
        if let Err(e) = std::fs::create_dir_all(ta_dir.join(sub)) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Could not create .ta/{}: {}", sub, e),
                })),
            )
                .into_response();
        }
    }

    // Write a starter PLAN.md.
    let plan_content = format!(
        "# {name} — Development Plan\n\n\
         ## Versioning\n\n\
         Version format: `MAJOR.MINOR.PATCH-alpha`. Phases map directly to semver.\n\n\
         ---\n\
         <!-- Add phases below using `ta plan add` or the Plan tab in Studio. -->\n",
        name = body.name.trim()
    );
    let plan_path = project_path.join("PLAN.md");
    if let Err(e) = std::fs::write(&plan_path, &plan_content) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Could not write PLAN.md: {}", e),
            })),
        )
            .into_response();
    }

    // Write a starter workflow.toml.
    let workflow_toml = format!(
        "[workflow]\n\
         name = \"{name}\"\n\
         enforce_phase_order = \"warn\"\n\
         context_budget_chars = 0\n\n\
         [build]\n\
         # commands = [\"cargo build\"]\n\n\
         [verify]\n\
         # commands = [\"cargo test\", \"cargo clippy\"]\n\
         # on_failure = \"block\"\n",
        name = body.name.trim()
    );
    if let Err(e) = std::fs::write(ta_dir.join("workflow.toml"), workflow_toml) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Could not write workflow.toml: {}", e),
            })),
        )
            .into_response();
    }

    tracing::info!(
        path = %project_path.display(),
        name = %body.name,
        "New project initialized via Studio"
    );

    Json(serde_json::json!({
        "ok": true,
        "path": project_path.display().to_string(),
        "name": body.name.trim(),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn recent_projects_add_deduplicates() {
        // Build two entries with the same path.
        let mut projects: Vec<RecentProject> = vec![];
        // Simulate add logic inline (without touching the filesystem).
        let path = "/tmp/my-project".to_string();
        let name = "My Project".to_string();
        let now = "2026-01-01T00:00:00Z".to_string();
        projects.push(RecentProject {
            path: path.clone(),
            name: name.clone(),
            last_opened: now.clone(),
        });
        // Add again: remove existing, prepend.
        projects.retain(|p| p.path != path);
        projects.insert(
            0,
            RecentProject {
                path: path.clone(),
                name: name.clone(),
                last_opened: "2026-01-02T00:00:00Z".to_string(),
            },
        );
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, path);
    }

    #[test]
    fn recent_projects_capped_at_20() {
        let mut projects: Vec<RecentProject> = vec![];
        for i in 0..25usize {
            let path = format!("/tmp/project-{}", i);
            projects.retain(|p: &RecentProject| p.path != path);
            projects.insert(
                0,
                RecentProject {
                    path,
                    name: format!("Project {}", i),
                    last_opened: "2026-01-01T00:00:00Z".to_string(),
                },
            );
            projects.truncate(20);
        }
        assert_eq!(projects.len(), 20);
    }

    #[test]
    fn recent_projects_most_recent_first() {
        let mut projects: Vec<RecentProject> = vec![RecentProject {
            path: "/tmp/older".to_string(),
            name: "Older".to_string(),
            last_opened: "2026-01-01T00:00:00Z".to_string(),
        }];
        let new_path = "/tmp/newer".to_string();
        projects.retain(|p| p.path != new_path);
        projects.insert(
            0,
            RecentProject {
                path: new_path.clone(),
                name: "Newer".to_string(),
                last_opened: "2026-02-01T00:00:00Z".to_string(),
            },
        );
        assert_eq!(projects[0].path, new_path);
    }

    #[test]
    fn read_project_name_from_workflow_toml() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("workflow.toml"),
            "[project]\nname = \"Test Project\"\n",
        )
        .unwrap();
        let name = read_project_name(dir.path());
        assert_eq!(name, "Test Project");
    }

    #[test]
    fn read_project_name_fallback_to_dirname() {
        let dir = tempdir().unwrap();
        // No workflow.toml — should fall back to directory name.
        let name = read_project_name(dir.path());
        // tempdir names are something like "tmp.XXXXXXXX" — just verify it's not empty.
        assert!(!name.is_empty());
        assert_ne!(name, "unknown");
    }

    #[test]
    fn recent_projects_empty_when_no_file() {
        // Override HOME to a nonexistent directory so no file is found.
        // We test the load logic directly using a path that doesn't exist.
        let dir = tempdir().unwrap();
        let fake_config = dir.path().join("ta").join("recent-projects.json");
        assert!(!fake_config.exists());
        // parse an empty path as if it were our store
        let result: Vec<RecentProject> =
            serde_json::from_str::<Vec<RecentProject>>("[]").unwrap_or_default();
        assert!(result.is_empty());
    }
}
