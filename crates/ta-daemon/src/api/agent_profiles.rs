// api/agent_profiles.rs — GET /api/agents/profiles (v0.16.3).
//
// Returns a JSON inventory of all agent TOML profiles discovered from
// .ta/agents/ and ~/.config/ta/agents/, including manifest metadata,
// model extracted from args, inherit source, and context file count.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use ta_runtime::{build_workflow_agent_index, AgentFrameworkManifest};

use crate::api::AppState;

/// Metadata for a single agent profile.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AgentProfileEntry {
    /// Profile name (from the manifest's `name` field).
    pub name: String,
    /// Version string.
    pub version: String,
    /// Human-readable description.
    pub description: String,
    /// Agent command binary.
    pub command: String,
    /// Model extracted from `--model <value>` in args, if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Inherit path (resolved), if this manifest uses inheritance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherit: Option<String>,
    /// Number of context files declared in `[context].files`.
    pub context_file_count: usize,
    /// Whether this is a built-in framework manifest.
    pub builtin: bool,
    /// Source directory: "project", "user", or "builtin".
    pub source: String,
    /// Workflow names that reference this agent profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by_workflows: Vec<String>,
}

/// Response body for GET /api/agents/profiles.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentProfilesResponse {
    pub profiles: Vec<AgentProfileEntry>,
    pub total: usize,
}

/// GET /api/agents/profiles — enumerate all installed agent profiles.
pub async fn list_profiles(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let project_root = &state.project_root;

    let discovered = AgentFrameworkManifest::discover(project_root);
    let workflow_index = build_workflow_agent_index(project_root);

    let user_agents_dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".config")
        .join("ta")
        .join("agents");

    let entries: Vec<AgentProfileEntry> = discovered
        .into_iter()
        .map(|m| {
            let source = if m.builtin {
                "builtin"
            } else {
                let is_user = user_agents_dir.is_dir()
                    && std::fs::read_dir(&user_agents_dir)
                        .ok()
                        .into_iter()
                        .flat_map(|rd| rd.flatten())
                        .any(|e| {
                            e.path()
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(|s| s == m.name)
                                .unwrap_or(false)
                        });
                if is_user {
                    "user"
                } else {
                    "project"
                }
            };

            let used_by = workflow_index.get(&m.name).cloned().unwrap_or_default();

            AgentProfileEntry {
                model: m.extract_model().map(|s| s.to_string()),
                context_file_count: m.context.as_ref().map(|c| c.files.len()).unwrap_or(0),
                inherit: m.inherit.clone(),
                builtin: m.builtin,
                source: source.to_string(),
                name: m.name,
                version: m.version,
                description: m.description,
                command: m.command,
                used_by_workflows: used_by,
            }
        })
        .collect();

    let total = entries.len();
    (
        StatusCode::OK,
        Json(AgentProfilesResponse {
            profiles: entries,
            total,
        }),
    )
}
