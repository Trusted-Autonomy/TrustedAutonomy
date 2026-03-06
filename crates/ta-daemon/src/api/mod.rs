// api/mod.rs — Daemon HTTP API module organization (v0.9.7).
//
// Provides the full API surface for any interface to connect:
//   /api/cmd      — command execution
//   /api/agent    — agent session management
//   /api/events   — SSE event stream
//   /api/status   — project dashboard
//   /api/input    — unified input with routing
//   /api/routes   — routing table for tab completion
//   /api/drafts   — draft review (existing, from web.rs)
//   /api/memory   — memory store (existing, from web.rs)

pub mod agent;
pub mod auth;
pub mod cmd;
pub mod events;
pub mod input;
pub mod status;

use std::path::PathBuf;
use std::sync::Arc;

use axum::middleware;
use axum::routing::{delete, get, post};
use axum::Router;

use crate::config::{DaemonConfig, ShellConfig, TokenStore};

/// Shared application state for all API handlers.
pub struct AppState {
    pub project_root: PathBuf,
    pub pr_packages_dir: PathBuf,
    pub memory_dir: PathBuf,
    pub events_dir: PathBuf,
    pub goals_dir: PathBuf,
    pub daemon_config: DaemonConfig,
    pub shell_config: ShellConfig,
    pub token_store: TokenStore,
    pub agent_sessions: agent::AgentSessionManager,
}

impl AppState {
    pub fn new(project_root: PathBuf, daemon_config: DaemonConfig) -> Self {
        let ta_dir = project_root.join(".ta");
        let shell_config = ShellConfig::load(&project_root);
        let max_sessions = daemon_config.agent.max_sessions;

        Self {
            pr_packages_dir: ta_dir.join("pr_packages"),
            memory_dir: ta_dir.join("memory"),
            events_dir: ta_dir.join("events"),
            goals_dir: ta_dir.join("goals"),
            token_store: TokenStore::new(&project_root),
            shell_config,
            agent_sessions: agent::AgentSessionManager::new(max_sessions),
            project_root,
            daemon_config,
        }
    }
}

/// Build the full API router with auth middleware.
pub fn build_api_router(state: Arc<AppState>) -> Router {
    Router::new()
        // New v0.9.7 API routes.
        .route("/api/cmd", post(cmd::execute_command))
        .route("/api/status", get(status::project_status))
        .route("/api/events", get(events::event_stream))
        .route("/api/input", post(input::handle_input))
        .route("/api/routes", get(input::list_routes))
        // Agent session routes.
        .route("/api/agent/start", post(agent::start_session))
        .route("/api/agent/ask", post(agent::ask_agent))
        .route("/api/agent/sessions", get(agent::list_sessions))
        .route("/api/agent/{id}", delete(agent::stop_session))
        // Auth middleware on all API routes.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .with_state(state)
}
