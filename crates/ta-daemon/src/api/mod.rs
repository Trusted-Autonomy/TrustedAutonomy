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

pub mod active;
pub mod advisor;
pub mod agent;
pub mod agent_profiles;
pub mod auth;
pub mod cmd;
pub mod context_upload;
pub mod dashboard_advisor;
pub mod draft_dialog;
pub mod drain;
pub mod events;
pub mod goal_output;
pub mod health;
pub mod health_signals;
pub mod input;
pub mod interactions;
pub mod links;
pub mod notifications;
pub mod persona;
pub mod plan;
pub mod project_browser;
pub mod project_new;
pub mod settings;
pub mod stats;
pub mod status;
pub mod team;
pub mod webhooks;
pub mod workflow;

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::HeaderMap;
use axum::middleware;
use axum::routing::{delete, get, post, put};
use axum::Router;
use tokio::sync::Semaphore;

use crate::config::{DaemonConfig, ShellConfig, TokenStore};
use crate::office::ProjectRegistry;
use crate::phase_claim::PhaseClaims;
use crate::project_context::ProjectStatusSummary;
use crate::question_registry::QuestionRegistry;

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
    pub goal_output: goal_output::GoalOutputManager,
    /// Stdin handles for background agent processes (v0.10.18.5).
    pub goal_input: goal_output::GoalInputManager,
    pub question_registry: Arc<QuestionRegistry>,
    /// Multi-project registry (single-project mode has exactly one entry).
    pub project_registry: Arc<ProjectRegistry>,
    /// Bootstrap session manager for conversational project creation (v0.10.17).
    pub bootstrap_sessions: project_new::BootstrapSessionManager,
    /// Persistent QA agent for shell sessions (v0.11.4.2).
    pub persistent_qa: Arc<agent::PersistentQaAgent>,
    /// Currently active project root for TA Studio multi-project support (v0.14.18).
    pub active_project_root: Arc<std::sync::RwLock<PathBuf>>,
    /// Atomic in-memory claim registry for plan phases (v0.15.24.2).
    pub phase_claims: Arc<PhaseClaims>,
    /// Cached health signals for `/health/signals` endpoint (v0.15.30.6).
    pub signals_cache: Arc<health_signals::SignalsCache>,
    /// 5-second cache for `/api/status` responses (v0.17.0.6).
    /// Prevents the status endpoint from scanning all goals+drafts on every call.
    pub status_cache: Arc<status::StatusCache>,
    /// Semaphore bounding concurrent background tasks from `POST /api/cmd` (v0.17.0.9).
    /// Size comes from `commands.max_background_tasks` in daemon.toml (default 4).
    pub cmd_semaphore: Arc<Semaphore>,
    /// Rate-limits `POST /api/shutdown` to at most 1 call per 60 seconds (v0.17.0.9).
    pub last_shutdown_attempt: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
}

impl AppState {
    pub fn new(project_root: PathBuf, daemon_config: DaemonConfig) -> Self {
        let ta_dir = project_root.join(".ta");
        let shell_config = ShellConfig::load(&project_root);
        let max_sessions = daemon_config.agent.max_sessions;
        let registry = ProjectRegistry::single_project(project_root.clone());
        let qa_config = daemon_config.shell.qa_agent.clone();
        let trusted_binaries = daemon_config.agent.trusted_binaries.clone();
        let persistent_qa = Arc::new(
            agent::PersistentQaAgent::new(qa_config, project_root.clone())
                .with_trusted_binaries(trusted_binaries),
        );

        let cmd_max = daemon_config.commands.max_background_tasks;
        Self {
            pr_packages_dir: ta_dir.join("pr_packages"),
            memory_dir: ta_dir.join("memory"),
            events_dir: ta_dir.join("events"),
            goals_dir: ta_dir.join("goals"),
            token_store: TokenStore::new(&project_root),
            shell_config,
            agent_sessions: agent::AgentSessionManager::new(max_sessions),
            goal_output: goal_output::GoalOutputManager::new(),
            goal_input: goal_output::GoalInputManager::new(),
            question_registry: Arc::new(QuestionRegistry::new()),
            project_registry: Arc::new(registry),
            bootstrap_sessions: project_new::BootstrapSessionManager::new(),
            persistent_qa,
            active_project_root: Arc::new(std::sync::RwLock::new(project_root.clone())),
            phase_claims: Arc::new(PhaseClaims::new()),
            signals_cache: Arc::new(health_signals::SignalsCache::default()),
            status_cache: Arc::new(status::StatusCache::new()),
            cmd_semaphore: Arc::new(Semaphore::new(cmd_max)),
            last_shutdown_attempt: Arc::new(std::sync::Mutex::new(None)),
            project_root,
            daemon_config,
        }
    }

    /// Create with a multi-project registry from office config.
    #[allow(dead_code)]
    pub fn with_registry(
        project_root: PathBuf,
        daemon_config: DaemonConfig,
        registry: ProjectRegistry,
    ) -> Self {
        let mut state = Self::new(project_root, daemon_config);
        state.project_registry = Arc::new(registry);
        state
    }

    /// Resolve a project root from an optional `?project=` query parameter.
    /// In single-project mode, always returns the default project root.
    /// In multi-project mode, requires the project parameter.
    #[allow(dead_code)]
    pub fn resolve_project_root(&self, project_name: Option<&str>) -> Result<PathBuf, String> {
        match project_name {
            Some(name) => self
                .project_registry
                .get(name)
                .map(|ctx| ctx.path)
                .ok_or_else(|| {
                    format!(
                        "Project '{}' not found. Available: {:?}",
                        name,
                        self.project_registry.names()
                    )
                }),
            None => self
                .project_registry
                .default_project()
                .map(|ctx| ctx.path)
                .ok_or_else(|| {
                    format!(
                        "Multiple projects available. Specify ?project=<name>. Available: {:?}",
                        self.project_registry.names()
                    )
                }),
        }
    }
}

// ── Project API handlers (v0.9.10) ──────────────────────────────

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

/// List all managed projects.
async fn list_projects(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> impl IntoResponse {
    let projects: Vec<ProjectStatusSummary> = state
        .project_registry
        .list()
        .iter()
        .map(|ctx| ctx.status_summary())
        .collect();
    Json(projects).into_response()
}

/// Get a specific project's status.
async fn get_project(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.project_registry.get(&name) {
        Some(ctx) => Json(ctx.status_summary()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            format!(
                "Project '{}' not found. Available: {:?}",
                name,
                state.project_registry.names()
            ),
        )
            .into_response(),
    }
}

/// Request body for adding a project at runtime.
#[derive(Deserialize)]
struct AddProjectRequest {
    name: String,
    path: String,
    plan: Option<String>,
    default_branch: Option<String>,
}

/// Add a project at runtime.
async fn add_project(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Json(body): Json<AddProjectRequest>,
) -> impl IntoResponse {
    let ctx = crate::project_context::ProjectContext::from_config(
        body.name.clone(),
        std::path::PathBuf::from(&body.path),
        body.plan,
        body.default_branch,
    );

    if let Err(e) = ctx.validate() {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }

    match state.project_registry.add(ctx) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "status": "added",
                "name": body.name,
                "path": body.path,
            })),
        )
            .into_response(),
        Err(e) => (StatusCode::CONFLICT, e).into_response(),
    }
}

/// Remove a project at runtime.
async fn remove_project(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.project_registry.remove(&name) {
        Ok(_) => Json(serde_json::json!({
            "status": "removed",
            "name": name,
        }))
        .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

/// Reload office configuration.
async fn reload_office(
    axum::extract::State(_state): axum::extract::State<Arc<AppState>>,
) -> impl IntoResponse {
    // The office config path is stored in TA_OFFICE_CONFIG env var.
    let config_path = match std::env::var("TA_OFFICE_CONFIG") {
        Ok(path) => std::path::PathBuf::from(path),
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "No TA_OFFICE_CONFIG set. Cannot reload without a config path.",
            )
                .into_response();
        }
    };

    match crate::office::OfficeConfig::load(&config_path) {
        Ok(config) => {
            let project_count = config.projects.len();
            Json(serde_json::json!({
                "status": "reloaded",
                "config": config_path.display().to_string(),
                "projects": project_count,
            }))
            .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// `POST /api/shutdown` — Graceful daemon shutdown (v0.10.10 / v0.17.0.9).
///
/// Requires the `X-TA-Admin-Confirm: shutdown` header to prevent accidental or
/// malicious shutdown from agent subprocesses. Rate-limited to 1 call per 60 s.
async fn shutdown_daemon(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Guard: require explicit confirmation header.
    let confirm = headers
        .get("x-ta-admin-confirm")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if confirm != "shutdown" {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Shutdown requires the X-TA-Admin-Confirm: shutdown header.",
                "hint": "Add the header to your request: X-TA-Admin-Confirm: shutdown"
            })),
        )
            .into_response();
    }

    // Rate-limit: at most 1 shutdown attempt per 60 seconds.
    {
        let mut last = state
            .last_shutdown_attempt
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        if let Some(prev) = *last {
            let elapsed = now.duration_since(prev);
            if elapsed < std::time::Duration::from_secs(60) {
                let remaining = 60u64.saturating_sub(elapsed.as_secs());
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({
                        "error": format!(
                            "Shutdown rate-limited. Try again in {} second(s).",
                            remaining
                        )
                    })),
                )
                    .into_response();
            }
        }
        *last = Some(now);
    }

    tracing::info!("Shutdown requested via POST /api/shutdown");
    // Spawn the exit on a short delay so the response is sent first.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({
        "status": "shutting_down",
        "message": "Daemon is shutting down gracefully."
    }))
    .into_response()
}

/// Build the full API router with auth middleware.
pub fn build_api_router(state: Arc<AppState>) -> Router {
    // Health endpoint is auth-free (load balancers, readiness probes, Studio ambient bar).
    let health_routes = Router::new()
        .route("/health", get(health::health))
        .route("/metrics", get(health::metrics))
        .route("/health/signals", get(health_signals::health_signals))
        .with_state(state.clone());

    // Webhook endpoints use their own HMAC-based authentication — no Bearer token required.
    // The vcs_webhook handler uses ConnectInfo<SocketAddr> to allow localhost without a secret.
    let webhook_routes = Router::new()
        .route("/api/webhooks/github", post(webhooks::github_webhook))
        .route("/api/webhooks/vcs", post(webhooks::vcs_webhook))
        .with_state(state.clone());

    // All other routes go through the auth middleware.
    let api_routes = Router::new()
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
        // Goal output streaming.
        .route(
            "/api/goals/active-output",
            get(goal_output::list_active_output),
        )
        .route(
            "/api/goals/{id}/output",
            get(goal_output::goal_output_stream),
        )
        // Stdin relay for background agent processes (v0.10.18.5).
        .route(
            "/api/goals/{id}/input",
            post(goal_output::goal_input_handler),
        )
        // "Send info / ask this agent" from the Studio Active tab (v0.17.0.12.6 item 5).
        .route("/api/goals/{id}/message", post(active::handle_goal_message))
        // Active tab: Running/Configured goals with elapsed time + last event.
        .route("/api/active/goals", get(active::list_active_goals))
        // Workflow routes (v0.9.8.2).
        .route("/api/workflows", get(workflow::list_workflows))
        .route("/api/workflow/{id}/input", post(workflow::workflow_input))
        // Interaction routes — human responses to agent questions.
        .route("/api/interactions/pending", get(interactions::list_pending))
        .route(
            "/api/interactions/{id}/respond",
            post(interactions::respond),
        )
        // Project management routes (v0.9.10).
        .route("/api/projects", get(list_projects).post(add_project))
        .route(
            "/api/projects/{name}",
            get(get_project).delete(remove_project),
        )
        .route("/api/office/reload", post(reload_office))
        // Project bootstrapping routes (v0.10.17).
        .route("/api/project/new", post(project_new::create_project))
        // Plan phase browser routes (v0.14.19).
        .route("/api/plan/phases", get(plan::get_plan_phases))
        .route("/api/plan/phase/add", post(plan::add_plan_phase))
        .route("/api/plan/phase/claim", post(plan::claim_phase))
        .route("/api/plan/phase/release", post(plan::release_phase))
        .route("/api/goal/start", post(plan::start_goal))
        // Plan generation (v0.14.20).
        .route("/api/plan/generate", post(plan::generate_plan_phases))
        // Plan new — agent-based PLAN.md generation (v0.14.21).
        .route("/api/plan/new", post(plan::plan_new))
        // Project browser routes (v0.14.18).
        .route("/api/project/open", post(project_browser::open_project))
        .route("/api/project/list", get(project_browser::list_projects))
        .route(
            "/api/project/browse",
            post(project_browser::browse_projects),
        )
        // New project init (v0.14.20).
        .route("/api/project/init", post(project_browser::init_project))
        // Persona routes (v0.14.20).
        .route("/api/personas", get(persona::list_personas))
        .route("/api/persona/save", post(persona::save_persona))
        // Workflow generate/save (v0.14.20).
        .route("/api/workflow/generate", post(workflow::generate_workflow))
        .route("/api/workflow/save", post(workflow::save_workflow))
        // Workflow template catalog (v0.16.1.3).
        .route(
            "/api/workflow/templates",
            get(workflow::list_workflow_templates),
        )
        // Workflow run/stop/status from Studio (v0.15.14.1).
        .route("/api/workflow/{id}/run", post(workflow::run_workflow))
        .route("/api/workflow/{id}", delete(workflow::stop_workflow))
        .route(
            "/api/workflow/{id}/status",
            get(workflow::workflow_run_status),
        )
        // Proactive notifications (v0.13.1.6).
        .route("/api/notifications", get(notifications::get_notifications))
        // Settings API (v0.14.13).
        .route(
            "/api/settings/{section}",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route("/api/setup/status", get(settings::get_setup_status))
        .route("/api/setup/progress", put(settings::put_setup_progress))
        .route(
            "/api/settings/agent/validate",
            post(settings::validate_api_key),
        )
        .route(
            "/api/settings/notifications/test",
            post(settings::test_notification),
        )
        .route("/api/settings/vcs/check", post(settings::check_vcs))
        // Velocity stats API (v0.15.14.2).
        .route("/api/stats/velocity", get(stats::velocity_aggregate))
        .route("/api/stats/velocity-detail", get(stats::velocity_detail))
        // Studio "Stats" tab: goal stats + velocity + Meridian KPIs (v0.17.0.12.6 item 6).
        .route("/api/stats/summary", get(stats::summary))
        // Studio Advisor API (v0.15.21 + v0.15.28 + v0.16.1.3).
        .route("/api/advisor/message", post(advisor::handle_message))
        .route("/api/advisor/tools", get(advisor::get_tools))
        .route("/api/advisor/config", get(advisor::get_config))
        .route("/api/advisor/inject", post(advisor::handle_inject))
        .route(
            "/api/advisor/history",
            get(advisor::get_history).post(advisor::append_history),
        )
        .route("/api/advisor/suggestions", get(advisor::get_suggestions))
        .route("/api/advisor/context", get(advisor::get_context))
        // Dashboard Advisor dialog (v0.17.0.12.6 item 4): .ta/advisor-history.jsonl.
        .route(
            "/api/advisor/dialog",
            get(dashboard_advisor::get_dialog).post(dashboard_advisor::post_dialog),
        )
        // Per-draft Q&A dialog (v0.17.0.12.6 items 11, 12): .ta/drafts/<id>-dialog.jsonl.
        .route(
            "/api/drafts/{id}/dialog",
            get(draft_dialog::get_dialog).post(draft_dialog::post_dialog),
        )
        // Context file upload for Studio --context flag (v0.15.30.7).
        .route("/api/context/upload", post(context_upload::upload_context))
        // Cross-project links (v0.16.1.5).
        .route("/api/links", get(links::get_links))
        // Agent profile inventory (v0.16.3).
        .route("/api/agents/profiles", get(agent_profiles::list_profiles))
        // Team/role assignment (v0.17.0.12.17 item 3): `.ta/team.toml` had no
        // Studio UI at all before this — see api/team.rs.
        .route("/api/team", get(team::list_team))
        .route("/api/team/assign", post(team::assign_team_member))
        .route("/api/team/{role}", delete(team::remove_team_member))
        // Daemon lifecycle routes (v0.10.10 / v0.17.0.12.2).
        .route("/api/shutdown", post(shutdown_daemon))
        .route("/api/drain/status", get(drain::drain_status))
        // Auth middleware on all API routes.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .with_state(state);

    // Merge: health (no auth), webhooks (HMAC auth), api (Bearer auth).
    health_routes.merge(webhook_routes).merge(api_routes)
}
