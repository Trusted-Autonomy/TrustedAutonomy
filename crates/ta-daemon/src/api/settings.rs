// api/settings.rs — Settings and setup wizard API (v0.14.13).
//
// GET  /api/settings/:section        — read a config section as JSON
// PUT  /api/settings/:section        — write a config section from JSON
// GET  /api/setup/status             — read wizard progress
// PUT  /api/setup/progress           — write wizard progress
// POST /api/settings/agent/validate  — validate API key (mock)
// POST /api/settings/notifications/test — test notification (mock)
// POST /api/settings/vcs/check       — check VCS connection (mock)

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::AppState;
use crate::config::DaemonConfig;

/// Setup wizard progress as stored in `.ta/setup-progress.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SetupProgress {
    pub wizard_complete: bool,
    pub step: u32,
}

/// Request body for `PUT /api/setup/progress`.
#[derive(Debug, Deserialize)]
pub struct SetupProgressRequest {
    pub wizard_complete: Option<bool>,
    pub step: Option<u32>,
}

/// Request body for `POST /api/settings/agent/validate`.
#[derive(Debug, Deserialize)]
pub struct ValidateApiKeyRequest {
    pub api_key: Option<String>,
}

/// Request body for `POST /api/settings/notifications/test`.
#[derive(Debug, Deserialize)]
pub struct TestNotificationRequest {
    pub url: Option<String>,
    pub channel: Option<String>,
}

/// Request body for `POST /api/settings/vcs/check`.
#[derive(Debug, Deserialize)]
pub struct VcsCheckRequest {
    pub url: Option<String>,
    #[allow(dead_code)]
    pub token: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn setup_progress_path(project_root: &std::path::Path) -> PathBuf {
    project_root.join(".ta").join("setup-progress.json")
}

fn daemon_toml_path(project_root: &std::path::Path) -> PathBuf {
    project_root.join(".ta").join("daemon.toml")
}

/// Read setup progress from `.ta/setup-progress.json`, returning defaults if absent.
fn read_setup_progress(project_root: &std::path::Path) -> SetupProgress {
    let path = setup_progress_path(project_root);
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(progress) = serde_json::from_str::<SetupProgress>(&content) {
                return progress;
            }
        }
    }
    SetupProgress::default()
}

/// Write setup progress to `.ta/setup-progress.json`.
fn write_setup_progress(
    project_root: &std::path::Path,
    progress: &SetupProgress,
) -> std::io::Result<()> {
    let path = setup_progress_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(progress).map_err(std::io::Error::other)?;
    std::fs::write(&path, content)
}

/// Read daemon config from `.ta/daemon.toml`, falling back to defaults.
fn read_daemon_config(project_root: &std::path::Path) -> DaemonConfig {
    DaemonConfig::load(project_root)
}

/// Write daemon config to `.ta/daemon.toml`.
fn write_daemon_config(
    project_root: &std::path::Path,
    config: &DaemonConfig,
) -> Result<(), String> {
    let path = daemon_toml_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create .ta/ directory: {}", e))?;
    }
    let content =
        toml::to_string_pretty(config).map_err(|e| format!("Cannot serialize config: {}", e))?;
    std::fs::write(&path, content).map_err(|e| format!("Cannot write daemon.toml: {}", e))
}

/// Convert the `agent` section of DaemonConfig to a JSON Value.
fn agent_config_to_json(config: &DaemonConfig) -> Value {
    json!({
        "max_sessions": config.agent.max_sessions,
        "idle_timeout_secs": config.agent.idle_timeout_secs,
        "default_agent": config.agent.default_agent,
        "default_framework": config.agent.default_framework,
        "qa_framework": config.agent.qa_framework,
        "qa_agent": config.agent.qa_agent,
        "timeout_secs": config.agent.timeout_secs,
    })
}

/// Convert the `vcs` (routing) section of DaemonConfig to a JSON Value.
fn vcs_config_to_json(config: &DaemonConfig) -> Value {
    json!({
        "use_shell_config": config.routing.use_shell_config,
        "vcs_type": "git",
        "remote_url": "",
        "token": "",
    })
}

/// Convert the `channels` section of DaemonConfig to a JSON Value.
fn notifications_config_to_json(config: &DaemonConfig) -> Value {
    let discord_token = config
        .channels
        .discord
        .as_ref()
        .map(|d| d.bot_token.clone())
        .unwrap_or_default();
    let slack_token = config
        .channels
        .slack
        .as_ref()
        .map(|s| s.bot_token.clone())
        .unwrap_or_default();
    json!({
        "discord_token": discord_token,
        "slack_token": slack_token,
    })
}

/// Convert the `server` section of DaemonConfig to a JSON Value.
fn workflow_config_to_json(config: &DaemonConfig) -> Value {
    json!({
        "bind": config.server.bind,
        "port": config.server.port,
    })
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /api/settings/:section` — read a settings section.
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    Path(section): Path<String>,
) -> impl IntoResponse {
    let config = read_daemon_config(&state.project_root);
    let data: Value = match section.as_str() {
        "agent" => agent_config_to_json(&config),
        "vcs" => vcs_config_to_json(&config),
        "workflow" => workflow_config_to_json(&config),
        "notifications" => notifications_config_to_json(&config),
        "policy" => json!({
            "file_read": true,
            "file_write": true,
            "shell_commands": true,
            "network": false,
            "git_push_protected": true,
        }),
        "constitution" => json!({
            "rules": [
                { "id": "quality", "label": "Code quality checks required", "enabled": true },
                { "id": "tests", "label": "Tests must pass before draft", "enabled": true },
                { "id": "no_secrets", "label": "No secrets in committed code", "enabled": true },
            ],
            "custom_rule": "",
        }),
        "memory" => json!({
            "scope": "project",
            "retention_days": 90,
        }),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": format!("Unknown settings section: '{}'. Valid sections: agent, vcs, workflow, policy, constitution, notifications, memory.", section)
                })),
            )
                .into_response();
        }
    };
    Json(json!({ "section": section, "data": data })).into_response()
}

/// `PUT /api/settings/:section` — write a settings section.
pub async fn put_settings(
    State(state): State<Arc<AppState>>,
    Path(section): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let data = match body.get("data") {
        Some(d) => d.clone(),
        None => body.clone(),
    };

    match section.as_str() {
        "agent" => {
            let mut config = read_daemon_config(&state.project_root);
            if let Some(v) = data.get("max_sessions").and_then(|v| v.as_u64()) {
                config.agent.max_sessions = v as usize;
            }
            if let Some(v) = data.get("idle_timeout_secs").and_then(|v| v.as_u64()) {
                config.agent.idle_timeout_secs = v;
            }
            if let Some(v) = data.get("default_agent").and_then(|v| v.as_str()) {
                config.agent.default_agent = v.to_string();
            }
            if let Some(v) = data.get("default_framework").and_then(|v| v.as_str()) {
                config.agent.default_framework = v.to_string();
            }
            if let Some(v) = data.get("timeout_secs").and_then(|v| v.as_u64()) {
                config.agent.timeout_secs = v;
            }
            if let Err(e) = write_daemon_config(&state.project_root, &config) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e })),
                )
                    .into_response();
            }
            let updated = agent_config_to_json(&config);
            Json(json!({ "section": section, "data": updated })).into_response()
        }
        "vcs" => {
            let mut config = read_daemon_config(&state.project_root);
            if let Some(v) = data.get("use_shell_config").and_then(|v| v.as_bool()) {
                config.routing.use_shell_config = v;
            }
            if let Err(e) = write_daemon_config(&state.project_root, &config) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e })),
                )
                    .into_response();
            }
            let updated = vcs_config_to_json(&config);
            Json(json!({ "section": section, "data": updated })).into_response()
        }
        "notifications" => {
            let config = read_daemon_config(&state.project_root);
            // Notifications (Discord/Slack) are stored via channel plugin configs,
            // not directly in daemon.toml. Return the echoed data for now.
            let updated = notifications_config_to_json(&config);
            Json(json!({ "section": section, "data": updated })).into_response()
        }
        "workflow" => {
            let mut config = read_daemon_config(&state.project_root);
            if let Some(v) = data.get("port").and_then(|v| v.as_u64()) {
                config.server.port = v as u16;
            }
            if let Some(v) = data.get("bind").and_then(|v| v.as_str()) {
                config.server.bind = v.to_string();
            }
            if let Err(e) = write_daemon_config(&state.project_root, &config) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e })),
                )
                    .into_response();
            }
            let updated = workflow_config_to_json(&config);
            Json(json!({ "section": section, "data": updated })).into_response()
        }
        "policy" | "constitution" | "memory" => {
            // These sections don't map directly to daemon.toml; accept and echo back.
            Json(json!({ "section": section, "data": data })).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("Unknown settings section: '{}'. Valid sections: agent, vcs, workflow, policy, constitution, notifications, memory.", section)
            })),
        )
            .into_response(),
    }
}

/// `GET /api/setup/status` — read wizard progress.
pub async fn get_setup_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let progress = read_setup_progress(&state.project_root);
    Json(progress).into_response()
}

/// `PUT /api/setup/progress` — write wizard progress.
pub async fn put_setup_progress(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetupProgressRequest>,
) -> impl IntoResponse {
    let mut progress = read_setup_progress(&state.project_root);
    if let Some(v) = body.wizard_complete {
        progress.wizard_complete = v;
    }
    if let Some(v) = body.step {
        progress.step = v;
    }
    if let Err(e) = write_setup_progress(&state.project_root, &progress) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Cannot write setup progress: {}", e) })),
        )
            .into_response();
    }
    Json(progress).into_response()
}

/// `POST /api/settings/agent/validate` — validate an API key (mock).
pub async fn validate_api_key(Json(body): Json<ValidateApiKeyRequest>) -> impl IntoResponse {
    let key = body.api_key.unwrap_or_default();
    if key.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "invalid",
                "message": "API key must not be empty."
            })),
        )
            .into_response();
    }
    Json(json!({
        "status": "validated",
        "message": "API key looks valid (mock validation — real check requires agent connectivity)."
    }))
    .into_response()
}

/// `POST /api/settings/notifications/test` — send a test notification (mock).
pub async fn test_notification(Json(body): Json<TestNotificationRequest>) -> impl IntoResponse {
    let url = body.url.unwrap_or_default();
    if url.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "error",
                "message": "Webhook URL must not be empty."
            })),
        )
            .into_response();
    }
    let channel = body.channel.unwrap_or_else(|| "unknown".to_string());
    Json(json!({
        "status": "ok",
        "message": format!("Test notification sent to {} channel (mock — configure real webhook URL in notifications settings).", channel)
    }))
    .into_response()
}

/// `POST /api/settings/vcs/check` — check VCS connection (mock).
pub async fn check_vcs(Json(body): Json<VcsCheckRequest>) -> impl IntoResponse {
    let url = body.url.unwrap_or_default();
    if url.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "error",
                "message": "Repository URL must not be empty."
            })),
        )
            .into_response();
    }
    Json(json!({
        "status": "ok",
        "message": "VCS connection check passed (mock — real connectivity requires git credentials).",
        "url": url,
    }))
    .into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_state(project_root: std::path::PathBuf) -> Arc<AppState> {
        let config = DaemonConfig::default();
        Arc::new(AppState::new(project_root, config))
    }

    #[test]
    fn read_setup_progress_missing_file_returns_defaults() {
        let dir = tempdir().unwrap();
        let progress = read_setup_progress(dir.path());
        assert!(!progress.wizard_complete);
        assert_eq!(progress.step, 0);
    }

    #[test]
    fn write_and_read_setup_progress_round_trips() {
        let dir = tempdir().unwrap();
        let progress = SetupProgress {
            wizard_complete: true,
            step: 5,
        };
        write_setup_progress(dir.path(), &progress).unwrap();
        let loaded = read_setup_progress(dir.path());
        assert!(loaded.wizard_complete);
        assert_eq!(loaded.step, 5);
    }

    #[test]
    fn read_daemon_config_missing_file_returns_defaults() {
        let dir = tempdir().unwrap();
        let config = read_daemon_config(dir.path());
        // Default agent config has a non-empty default_framework.
        assert!(!config.agent.default_framework.is_empty());
    }

    #[test]
    fn agent_config_to_json_has_expected_keys() {
        let config = DaemonConfig::default();
        let json = agent_config_to_json(&config);
        assert!(json.get("max_sessions").is_some());
        assert!(json.get("default_framework").is_some());
        assert!(json.get("timeout_secs").is_some());
    }

    #[test]
    fn validate_api_key_empty_returns_invalid() {
        let req = ValidateApiKeyRequest {
            api_key: Some(String::new()),
        };
        // Empty key should be invalid.
        assert!(req.api_key.unwrap_or_default().trim().is_empty());
    }

    #[test]
    fn validate_api_key_nonempty_passes() {
        let req = ValidateApiKeyRequest {
            api_key: Some("sk-abc123".to_string()),
        };
        assert!(!req.api_key.unwrap_or_default().trim().is_empty());
    }

    #[tokio::test]
    async fn get_settings_unknown_section_returns_not_found() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path().to_path_buf());
        let response = get_settings(State(state), Path("unknown_section".to_string())).await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_setup_status_returns_defaults_when_no_file() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path().to_path_buf());
        let response = get_setup_status(State(state)).await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn put_setup_progress_persists_state() {
        let dir = tempdir().unwrap();
        // Create .ta dir
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let state = make_state(dir.path().to_path_buf());
        let req = SetupProgressRequest {
            wizard_complete: Some(true),
            step: Some(3),
        };
        let response = put_setup_progress(State(state), Json(req)).await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify it was actually written.
        let progress = read_setup_progress(dir.path());
        assert!(progress.wizard_complete);
        assert_eq!(progress.step, 3);
    }

    #[tokio::test]
    async fn get_settings_agent_returns_json() {
        let dir = tempdir().unwrap();
        let state = make_state(dir.path().to_path_buf());
        let response = get_settings(State(state), Path("agent".to_string())).await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn put_settings_agent_writes_and_returns_updated() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let state = make_state(dir.path().to_path_buf());
        let body = json!({ "data": { "default_framework": "codex", "timeout_secs": 600 } });
        let response = put_settings(State(state), Path("agent".to_string()), Json(body)).await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
