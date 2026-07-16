// api/persona.rs — Agent persona management API (v0.14.20).
//
// GET  /api/personas           — list all personas from .ta/personas/
// POST /api/persona/save       — create or update a persona TOML file

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use ta_goal::PersonaConfig;

use super::AppState;

/// Request body for saving a persona.
#[derive(Debug, Deserialize)]
pub struct PersonaSaveRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub forbidden_tools: Vec<String>,
    #[serde(default)]
    pub output_format: String,
}

/// API response for a single persona.
#[derive(Debug, Serialize)]
pub struct PersonaApiEntry {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub forbidden_tools: Vec<String>,
    pub output_format: String,
}

/// `GET /api/personas` — List all personas from `.ta/personas/`.
pub async fn list_personas(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let project_root = state.active_project_root.read().unwrap().clone();
    let summaries = PersonaConfig::list_all(&project_root);

    // Load full configs to include output_format.
    let entries: Vec<PersonaApiEntry> = summaries
        .into_iter()
        .map(|s| {
            let output_format = PersonaConfig::load(&project_root, &s.name)
                .map(|c| c.style.output_format)
                .unwrap_or_default();
            PersonaApiEntry {
                name: s.name,
                description: s.description,
                allowed_tools: s.allowed_tools,
                forbidden_tools: s.forbidden_tools,
                output_format,
            }
        })
        .collect();

    let count = entries.len();
    Json(serde_json::json!({
        "personas": entries,
        "count": count,
    }))
    .into_response()
}

/// `POST /api/persona/save` — Create or update a persona TOML file.
pub async fn save_persona(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PersonaSaveRequest>,
) -> impl IntoResponse {
    if body.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "name is required"})),
        )
            .into_response();
    }

    // Sanitise name.
    let name = body
        .name
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>();

    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "name contains no valid characters"})),
        )
            .into_response();
    }

    let persona = PersonaConfig {
        persona: ta_goal::PersonaInner {
            name: name.clone(),
            description: body.description.clone(),
            system_prompt: body.system_prompt.clone(),
            constitution: None,
            agent: None,
        },
        capabilities: ta_goal::PersonaCapabilities {
            allowed_tools: body.allowed_tools.clone(),
            forbidden_tools: body.forbidden_tools.clone(),
        },
        style: ta_goal::PersonaStyle {
            output_format: body.output_format.clone(),
            max_response_length: String::new(),
        },
    };

    let project_root = state.active_project_root.read().unwrap().clone();
    match persona.save(&project_root) {
        Ok(path) => Json(serde_json::json!({
            "ok": true,
            "name": name,
            "path": path.display().to_string(),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Could not save persona: {}", e)})),
        )
            .into_response(),
    }
}
