// server.rs — MCP gateway server for Trusted Autonomy.
//
// TaGatewayServer implements the rmcp ServerHandler trait, exposing TA's
// staging, policy, and goal lifecycle as MCP tools. Every file operation
// flows through policy -> staging -> changeset -> audit, ensuring the core
// thesis holds: all agent actions are mediated.
//
// v0.9.4: Refactored — tool handlers are in tools/ modules.
// This file contains state, config, CallerMode, and ServerHandler dispatch.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ta_audit::AuditLog;
use ta_changeset::channel_registry;
use ta_changeset::interaction::{InteractionRequest, Notification};
use ta_changeset::multi_channel::MultiChannelStrategy;
use ta_changeset::pr_package::PRPackage;
use ta_changeset::review_channel::{ReviewChannel, ReviewChannelError};
use ta_connector_fs::FsConnector;
use ta_goal::{EventDispatcher, GoalRun, GoalRunState, GoalRunStore, LogSink, TaEvent};
use ta_memory::FsMemoryStore;
use ta_policy::{
    AlignmentProfile, CompilerOptions, PolicyCompiler, PolicyDecision, PolicyEngine, PolicyRequest,
};
use ta_workspace::{JsonFileStore, StagingWorkspace};

use ta_changeset::draft_package::PendingAction;

use crate::config::GatewayConfig;
use crate::error::GatewayError;
use crate::interceptor::ToolCallInterceptor;
use crate::tools;

// ── Tool parameter types ─────────────────────────────────────────

/// Parameters for `ta_goal_start`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalStartParams {
    /// Human-readable title for the goal (e.g., "Fix authentication bug").
    pub title: String,
    /// Detailed objective describing what needs to be accomplished.
    pub objective: String,
    /// Agent identifier. Defaults to "claude-code" if not provided.
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    /// Source directory to use for the overlay workspace. Defaults to the
    /// project root. Required for launch mode to create a proper staging copy.
    #[serde(default)]
    pub source: Option<String>,
    /// Plan phase ID to link this goal to (e.g., "v0.9.4.1").
    #[serde(default)]
    pub phase: Option<String>,
    /// Goal IDs whose output should feed into this goal's context (v0.10.18).
    /// The gateway will retrieve summaries from completed goals and inject them
    /// into the new goal's context.
    #[serde(default)]
    pub context_from: Vec<String>,
    /// External thread ID for cross-channel context tracking (v0.10.18).
    /// When set, replies in this thread auto-route to the same project.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Project name to scope this goal to (v0.10.18, multi-project).
    #[serde(default)]
    pub project_name: Option<String>,
}

fn default_agent_id() -> String {
    "claude-code".to_string()
}

/// Parameters for tools that take only a goal_run_id.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalIdParams {
    /// The UUID of the goal run.
    pub goal_run_id: String,
}

/// Parameters for `ta_goal_list`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalListParams {
    /// Optional state filter (e.g., "running", "pr_ready").
    pub state: Option<String>,
}

/// Parameters for `ta_fs_write`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FsWriteParams {
    /// The UUID of the goal run.
    pub goal_run_id: String,
    /// Relative path within the workspace (e.g., "src/main.rs").
    pub path: String,
    /// File content to write.
    pub content: String,
}

/// Parameters for `ta_fs_read`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FsReadParams {
    /// The UUID of the goal run.
    pub goal_run_id: String,
    /// Relative path within the workspace.
    pub path: String,
}

/// Parameters for `ta_fs_list`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FsListParams {
    /// The UUID of the goal run.
    pub goal_run_id: String,
}

/// Parameters for `ta_fs_diff`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FsDiffParams {
    /// The UUID of the goal run.
    pub goal_run_id: String,
    /// Relative path within the workspace.
    pub path: String,
}

/// Parameters for `ta_pr_build`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PrBuildParams {
    /// The UUID of the goal run.
    pub goal_run_id: String,
    /// Title for the PR package.
    pub title: String,
    /// Summary of what changed and why.
    pub summary: String,
    /// Design alternatives considered (v0.9.5). Each entry has `option`, `rationale`, `chosen`.
    #[serde(default)]
    pub alternatives: Option<Vec<PrBuildAlternative>>,
}

/// A design alternative for the `ta_pr_build` MCP tool (v0.9.5).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PrBuildAlternative {
    /// The option that was considered.
    pub option: String,
    /// Why this option was chosen or rejected.
    pub rationale: String,
    /// Whether this was the chosen approach.
    #[serde(default)]
    pub chosen: bool,
}

// ── Macro goal / inner-loop parameter types ─────────────────────

/// Parameters for `ta_draft` (inner-loop agent tool).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DraftToolParams {
    /// Action: "build", "submit", "status", or "list".
    pub action: String,
    /// The UUID of the goal run (required for build, submit, status).
    #[serde(default)]
    pub goal_run_id: Option<String>,
    /// Summary of changes (used with "build" action).
    #[serde(default)]
    pub summary: Option<String>,
    /// Draft package ID (used with "status" action).
    #[serde(default)]
    pub draft_id: Option<String>,
    /// Force human review even when auto-approve is configured (v0.10.15).
    #[serde(default)]
    pub require_review: Option<bool>,
}

/// Parameters for `ta_goal` (inner-loop agent tool).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GoalToolParams {
    /// Action: "start" or "status".
    pub action: String,
    /// Title for the new sub-goal (required for "start").
    #[serde(default)]
    pub title: Option<String>,
    /// Objective for the sub-goal (used with "start").
    #[serde(default)]
    pub objective: Option<String>,
    /// The macro goal ID that this sub-goal belongs to (required for "start").
    #[serde(default)]
    pub macro_goal_id: Option<String>,
    /// Goal run ID to check status of (used with "status").
    #[serde(default)]
    pub goal_run_id: Option<String>,
    /// Whether to launch the implementation agent asynchronously (default: false).
    #[serde(default)]
    pub launch: Option<bool>,
    /// Agent to use for the sub-goal (default: inherits from macro goal).
    #[serde(default)]
    pub agent: Option<String>,
    /// Plan phase ID for the sub-goal (default: inherits from macro goal).
    #[serde(default)]
    pub phase: Option<String>,
}

/// Parameters for `ta_plan` (inner-loop agent tool).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlanToolParams {
    /// Action: "read" or "update".
    pub action: String,
    /// The UUID of the goal run (used to locate source dir with plan).
    #[serde(default)]
    pub goal_run_id: Option<String>,
    /// Phase ID to update (used with "update" action).
    #[serde(default)]
    pub phase: Option<String>,
    /// Proposed status update (used with "update" action).
    #[serde(default)]
    pub status_note: Option<String>,
}

/// Parameters for `ta_context` (persistent memory tool, v0.5.4+).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ContextToolParams {
    /// Action: "store", "recall", "list", "forget", "search", "stats", or "similar".
    pub action: String,
    /// Key for the memory entry (required for store, recall, forget).
    #[serde(default)]
    pub key: Option<String>,
    /// Value to store (JSON, used with "store" action).
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// Tags for the entry (used with "store" action).
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Maximum entries to return (used with "list" and "search" actions).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Source framework identifier (v0.5.6).
    #[serde(default)]
    pub source: Option<String>,
    /// Associate this entry with a specific goal (v0.5.6).
    #[serde(default)]
    pub goal_id: Option<String>,
    /// Knowledge category (v0.5.6).
    #[serde(default)]
    pub category: Option<String>,
    /// Search query text (used with "search" action, v0.5.6).
    #[serde(default)]
    pub query: Option<String>,
}

/// Parameters for `ta_agent_status` (v0.9.6).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AgentStatusParams {
    /// Action: "list" (all active agents) or "status" (specific agent).
    pub action: String,
    /// Agent ID to query (required for "status" action).
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// Tracks an active agent session within the gateway (v0.9.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    /// Unique per session (e.g., PID or UUID).
    pub agent_id: String,
    /// Agent framework type: "claude-code", "codex", "custom".
    pub agent_type: String,
    /// Goal this agent is working on (None for orchestrator).
    pub goal_run_id: Option<Uuid>,
    /// Caller mode for this session.
    pub caller_mode: String,
    /// When this session started.
    pub started_at: DateTime<Utc>,
    /// Last heartbeat (updated on each tool call).
    pub last_heartbeat: DateTime<Utc>,
}

// ── Gateway state ────────────────────────────────────────────────

/// Per-project isolated state for multi-project daemon support (v0.10.18).
///
/// Each project gets its own goal store, connectors, PR packages, event
/// dispatcher, and memory store. This prevents cross-project leakage
/// and allows per-project policy/review configuration.
pub struct ProjectState {
    /// Goal store scoped to this project.
    pub goal_store: GoalRunStore,
    /// Connectors for active goals within this project.
    pub connectors: HashMap<Uuid, FsConnector<JsonFileStore>>,
    /// PR packages for this project.
    pub pr_packages: HashMap<Uuid, PRPackage>,
    /// Event dispatcher for this project.
    pub event_dispatcher: EventDispatcher,
    /// Memory store for this project.
    pub memory_store: FsMemoryStore,
    /// Review channel for this project (can differ from default).
    pub review_channel: Option<Box<dyn ReviewChannel>>,
    /// Pending actions for this project.
    pub pending_actions: HashMap<Uuid, Vec<PendingAction>>,
}

impl ProjectState {
    /// Create a new per-project state from a project root path.
    pub fn new(project_root: &std::path::Path) -> Result<Self, GatewayError> {
        let ta_dir = project_root.join(".ta");
        let goals_dir = ta_dir.join("goals");
        let events_log = ta_dir.join("events.log");
        let memory_dir = ta_dir.join("memory");

        let goal_store = GoalRunStore::new(&goals_dir)?;
        let mut event_dispatcher = EventDispatcher::new();
        event_dispatcher.add_sink(Box::new(LogSink::new(&events_log)));
        let memory_store = FsMemoryStore::new(memory_dir);

        Ok(Self {
            goal_store,
            connectors: HashMap::new(),
            pr_packages: HashMap::new(),
            event_dispatcher,
            memory_store,
            review_channel: None,
            pending_actions: HashMap::new(),
        })
    }
}

/// Shared mutable state for the gateway server.
pub struct GatewayState {
    pub config: GatewayConfig,
    pub policy_engine: PolicyEngine,
    pub goal_store: GoalRunStore,
    pub connectors: HashMap<Uuid, FsConnector<JsonFileStore>>,
    pub pr_packages: HashMap<Uuid, PRPackage>,
    pub audit_log: AuditLog,
    pub event_dispatcher: EventDispatcher,
    pub review_channel: Box<dyn ReviewChannel>,
    pub memory_store: FsMemoryStore,
    pub auto_capture_config: ta_memory::AutoCaptureConfig,
    pub interceptor: ToolCallInterceptor,
    pub pending_actions: HashMap<Uuid, Vec<PendingAction>>,
    /// v0.9.3: Caller mode.
    pub caller_mode: CallerMode,
    /// v0.9.3: Dev session ID for audit correlation.
    pub dev_session_id: Option<String>,
    /// v0.9.6: Active agent sessions keyed by agent_id.
    pub active_agents: HashMap<String, AgentSession>,
    /// v0.10.18: Per-project isolated state for multi-project support.
    /// Keyed by project name. When empty, uses the top-level stores
    /// (single-project backward compatibility).
    pub projects: HashMap<String, ProjectState>,
    /// v0.10.18: Currently active project name for this session.
    pub active_project: Option<String>,
}

/// Caller mode determines what operations the MCP gateway allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerMode {
    Normal,
    Orchestrator,
    Unrestricted,
}

/// File patterns that orchestrator-mode agents may write without being blocked.
/// These are release artifacts that the release pipeline agent needs to create
/// directly, rather than delegating through a sub-goal.
const ORCHESTRATOR_WRITE_WHITELIST: &[&str] = &[
    ".release-draft.md",
    "CHANGELOG.md",
    "version.json",
    ".press-release-draft.md",
];

impl CallerMode {
    pub fn from_env() -> Self {
        match std::env::var("TA_CALLER_MODE").as_deref() {
            Ok("orchestrator") => CallerMode::Orchestrator,
            Ok("unrestricted") => CallerMode::Unrestricted,
            _ => CallerMode::Normal,
        }
    }

    pub fn is_tool_forbidden(&self, tool_name: &str) -> bool {
        match self {
            CallerMode::Normal | CallerMode::Unrestricted => false,
            CallerMode::Orchestrator => {
                matches!(tool_name, "ta_fs_write" | "ta_pr_build" | "ta_fs_diff")
            }
        }
    }

    /// Check if a specific file path is whitelisted for orchestrator writes.
    /// Release artifact files can be written even in orchestrator mode.
    pub fn is_write_whitelisted(&self, path: &str) -> bool {
        match self {
            CallerMode::Orchestrator => {
                let filename = std::path::Path::new(path)
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or(path);
                ORCHESTRATOR_WRITE_WHITELIST
                    .iter()
                    .any(|pattern| filename == *pattern || path == *pattern)
            }
            _ => true, // Non-orchestrator modes allow all writes.
        }
    }

    /// Returns true if the tool requires an active goal (mutation tools).
    pub fn requires_goal(&self, tool_name: &str) -> bool {
        match self {
            CallerMode::Orchestrator => matches!(
                tool_name,
                "ta_fs_write" | "ta_pr_build" | "ta_fs_diff" | "ta_fs_read" | "ta_fs_list"
            ),
            _ => false,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            CallerMode::Normal => "normal",
            CallerMode::Orchestrator => "orchestrator",
            CallerMode::Unrestricted => "unrestricted",
        }
    }
}

impl GatewayState {
    /// Initialize gateway state from config.
    ///
    /// Loads `.ta/config.yaml` to resolve channel configuration. If the config
    /// specifies multiple review channels, they are wrapped in a
    /// `MultiReviewChannel` (v0.10.0). Falls back to `TerminalChannel` if
    /// the config is missing or the channel type is unknown.
    pub fn new(config: GatewayConfig) -> Result<Self, GatewayError> {
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let audit_log = AuditLog::open(&config.audit_log)?;

        let mut event_dispatcher = EventDispatcher::new();
        event_dispatcher.add_sink(Box::new(LogSink::new(&config.events_log)));
        let memory_store = FsMemoryStore::new(config.workspace_root.join(".ta").join("memory"));

        let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
        let auto_capture_config = ta_memory::auto_capture::load_config(&workflow_toml);

        // v0.10.0: Load channel routing from .ta/config.yaml and build review
        // channel(s) via ChannelRegistry instead of hardcoding AutoApproveChannel.
        let review_channel = Self::build_review_channel(&config);

        Ok(Self {
            config,
            policy_engine: PolicyEngine::new(),
            goal_store,
            connectors: HashMap::new(),
            pr_packages: HashMap::new(),
            audit_log,
            event_dispatcher,
            review_channel,
            memory_store,
            auto_capture_config,
            interceptor: ToolCallInterceptor::new(),
            pending_actions: HashMap::new(),
            caller_mode: CallerMode::from_env(),
            dev_session_id: std::env::var("TA_DEV_SESSION_ID").ok(),
            active_agents: HashMap::new(),
            projects: HashMap::new(),
            active_project: None,
        })
    }

    /// Build the review channel from `.ta/config.yaml` using the ChannelRegistry.
    ///
    /// Resolution order:
    /// 1. Load `.ta/config.yaml` → `TaConfig.channels.review`
    /// 2. Build `ChannelRegistry` with all built-in factories
    /// 3. Resolve each channel type via factory → `ReviewChannel`
    /// 4. Wrap multiple channels in `MultiReviewChannel` if needed
    /// 5. Fallback: `TerminalChannel` if config missing or type unknown
    fn build_review_channel(config: &GatewayConfig) -> Box<dyn ReviewChannel> {
        let ta_config = channel_registry::load_config(&config.workspace_root);
        let registry = channel_registry::default_registry();
        let routing = &ta_config.channels;

        // Parse strategy from config (default: first_response).
        let strategy = match routing.strategy.as_deref() {
            Some("quorum") => MultiChannelStrategy::Quorum { quorum_size: 2 },
            _ => MultiChannelStrategy::FirstResponse,
        };

        match registry.build_review_from_route(&routing.review, &strategy) {
            Ok(channel) => {
                let configs = routing.review.configs();
                let types: Vec<&str> = configs.iter().map(|c| c.channel_type.as_str()).collect();
                tracing::info!(
                    channel_types = ?types,
                    multi = routing.review.is_multi(),
                    "gateway: resolved review channel(s) from .ta/config.yaml"
                );
                channel
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "gateway: failed to build review channel from config, falling back to terminal"
                );
                Box::new(ta_changeset::terminal_channel::TerminalChannel::stdio())
            }
        }
    }

    /// Register a project for multi-project support (v0.10.18).
    ///
    /// Creates per-project isolated state (goal store, connectors, events).
    /// Once at least one project is registered, goal operations can be
    /// scoped to a specific project via `active_project`.
    pub fn register_project(
        &mut self,
        name: &str,
        project_root: &std::path::Path,
    ) -> Result<(), GatewayError> {
        let state = ProjectState::new(project_root)?;
        tracing::info!(
            project = %name,
            path = %project_root.display(),
            "Registered project with per-project state isolation"
        );
        self.projects.insert(name.to_string(), state);
        Ok(())
    }

    /// Set the active project for this session (v0.10.18).
    pub fn set_active_project(&mut self, name: Option<String>) {
        self.active_project = name;
    }

    /// Get the goal store for the active project, falling back to the
    /// global store in single-project mode (v0.10.18).
    pub fn active_goal_store(&self) -> &GoalRunStore {
        if let Some(ref project_name) = self.active_project {
            if let Some(project) = self.projects.get(project_name) {
                return &project.goal_store;
            }
        }
        &self.goal_store
    }

    /// List registered project names (v0.10.18).
    pub fn project_names(&self) -> Vec<String> {
        self.projects.keys().cloned().collect()
    }

    /// Start a new goal: create GoalRun, issue manifest, set up connector.
    pub fn start_goal(
        &mut self,
        title: &str,
        objective: &str,
        agent_id: &str,
    ) -> Result<GoalRun, GatewayError> {
        let goal_run_id = Uuid::new_v4();
        let staging_path = self.config.staging_dir.join(goal_run_id.to_string());
        let store_path = self.config.store_dir.join(goal_run_id.to_string());

        let mut goal_run = GoalRun::new(title, objective, agent_id, staging_path, store_path);
        goal_run.goal_run_id = goal_run_id;

        let profile = AlignmentProfile::default_developer();
        let options = CompilerOptions::default();
        let manifest =
            PolicyCompiler::compile_with_id(goal_run.manifest_id, agent_id, &profile, &options)
                .map_err(|e| GatewayError::Other(format!("policy compilation failed: {}", e)))?;
        self.policy_engine.load_manifest(manifest);

        let staging = StagingWorkspace::new(goal_run_id.to_string(), &self.config.staging_dir)?;
        let store = JsonFileStore::new(self.config.store_dir.join(goal_run_id.to_string()))?;
        let connector = FsConnector::new(goal_run_id.to_string(), staging, store, agent_id);
        self.connectors.insert(goal_run_id, connector);

        goal_run.transition(GoalRunState::Configured)?;
        goal_run.transition(GoalRunState::Running)?;
        self.goal_store.save(&goal_run)?;

        self.event_dispatcher
            .dispatch(&TaEvent::goal_created(goal_run_id, title, agent_id));

        Ok(goal_run)
    }

    /// Start a new goal with a custom alignment profile (v0.4.0).
    pub fn start_goal_with_profile(
        &mut self,
        title: &str,
        objective: &str,
        agent_id: &str,
        profile: &AlignmentProfile,
        resource_scope: Option<Vec<String>>,
    ) -> Result<GoalRun, GatewayError> {
        let goal_run_id = Uuid::new_v4();
        let staging_path = self.config.staging_dir.join(goal_run_id.to_string());
        let store_path = self.config.store_dir.join(goal_run_id.to_string());

        let mut goal_run = GoalRun::new(title, objective, agent_id, staging_path, store_path);
        goal_run.goal_run_id = goal_run_id;

        let options = CompilerOptions {
            resource_scope: resource_scope.unwrap_or_else(|| vec!["fs://workspace/**".to_string()]),
            validity_hours: 8,
        };
        let manifest =
            PolicyCompiler::compile_with_id(goal_run.manifest_id, agent_id, profile, &options)
                .map_err(|e| GatewayError::Other(format!("policy compilation failed: {}", e)))?;
        self.policy_engine.load_manifest(manifest);

        let staging = StagingWorkspace::new(goal_run_id.to_string(), &self.config.staging_dir)?;
        let store = JsonFileStore::new(self.config.store_dir.join(goal_run_id.to_string()))?;
        let connector = FsConnector::new(goal_run_id.to_string(), staging, store, agent_id);
        self.connectors.insert(goal_run_id, connector);

        goal_run.transition(GoalRunState::Configured)?;
        goal_run.transition(GoalRunState::Running)?;
        self.goal_store.save(&goal_run)?;

        self.event_dispatcher
            .dispatch(&TaEvent::goal_created(goal_run_id, title, agent_id));

        Ok(goal_run)
    }

    /// Check policy for a filesystem operation.
    pub fn check_policy(
        &self,
        agent_id: &str,
        verb: &str,
        path: &str,
    ) -> Result<PolicyDecision, GatewayError> {
        let request = PolicyRequest {
            agent_id: agent_id.to_string(),
            tool: "fs".to_string(),
            verb: verb.to_string(),
            target_uri: format!("fs://workspace/{}", path),
        };
        Ok(self.policy_engine.evaluate(&request))
    }

    /// Save a PR package to both in-memory cache and disk.
    pub fn save_pr_package(&mut self, pkg: PRPackage) -> Result<(), GatewayError> {
        let package_id = pkg.package_id;
        std::fs::create_dir_all(&self.config.pr_packages_dir)?;
        let path = self
            .config
            .pr_packages_dir
            .join(format!("{}.json", package_id));
        let json =
            serde_json::to_string_pretty(&pkg).map_err(|e| GatewayError::Other(e.to_string()))?;
        std::fs::write(&path, json)?;
        self.pr_packages.insert(package_id, pkg);
        Ok(())
    }

    /// Set a custom ReviewChannel.
    pub fn set_review_channel(&mut self, channel: Box<dyn ReviewChannel>) {
        self.review_channel = channel;
    }

    /// Route an interaction request through the configured ReviewChannel.
    pub fn request_review(
        &self,
        request: &InteractionRequest,
    ) -> Result<ta_changeset::interaction::InteractionResponse, ReviewChannelError> {
        let response = self.review_channel.request_interaction(request)?;
        tracing::info!(
            interaction_id = %request.interaction_id,
            kind = %request.kind,
            decision = %response.decision,
            "review channel interaction"
        );
        Ok(response)
    }

    /// Send a non-blocking notification through the ReviewChannel.
    pub fn notify_reviewer(&self, notification: &Notification) -> Result<(), ReviewChannelError> {
        self.review_channel.notify(notification)
    }

    /// Register or update an agent session (v0.9.6).
    ///
    /// Called on each tool invocation to track active agents. If the agent_id
    /// is already known, updates the heartbeat. Otherwise creates a new session.
    pub fn touch_agent_session(
        &mut self,
        agent_id: &str,
        agent_type: &str,
        goal_run_id: Option<Uuid>,
    ) {
        let now = Utc::now();
        if let Some(session) = self.active_agents.get_mut(agent_id) {
            session.last_heartbeat = now;
            // Update goal association if it changed.
            if goal_run_id.is_some() {
                session.goal_run_id = goal_run_id;
            }
        } else {
            let session = AgentSession {
                agent_id: agent_id.to_string(),
                agent_type: agent_type.to_string(),
                goal_run_id,
                caller_mode: self.caller_mode.as_str().to_string(),
                started_at: now,
                last_heartbeat: now,
            };
            self.active_agents.insert(agent_id.to_string(), session);
            self.event_dispatcher
                .dispatch(&TaEvent::agent_session_started(
                    agent_id,
                    agent_type,
                    goal_run_id,
                    self.caller_mode.as_str(),
                ));
        }
    }

    /// Remove an agent session (v0.9.6).
    pub fn end_agent_session(&mut self, agent_id: &str) {
        if let Some(session) = self.active_agents.remove(agent_id) {
            self.event_dispatcher
                .dispatch(&TaEvent::agent_session_ended(agent_id, session.goal_run_id));
        }
    }

    /// Get the agent_id for a goal run.
    pub fn agent_for_goal(&self, goal_run_id: Uuid) -> Result<String, GatewayError> {
        let goal = self
            .goal_store
            .get(goal_run_id)?
            .ok_or(GatewayError::GoalNotFound(goal_run_id))?;
        Ok(goal.agent_id)
    }

    /// Resolve the active agent_id from `TA_AGENT_ID` env var,
    /// falling back to the dev session or "unknown" (v0.10.15).
    pub fn resolve_agent_id(&self) -> String {
        std::env::var("TA_AGENT_ID")
            .ok()
            .or_else(|| self.dev_session_id.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Record a per-tool-call audit entry with caller_mode and agent_id (v0.10.15).
    ///
    /// Called from each tool handler to produce a fine-grained audit trail.
    pub fn audit_tool_call(
        &mut self,
        tool_name: &str,
        target_uri: Option<&str>,
        goal_run_id: Option<Uuid>,
    ) {
        let agent_id = self.resolve_agent_id();
        let mut event = ta_audit::AuditEvent::new(&agent_id, ta_audit::AuditAction::ToolCall)
            .with_caller_mode(self.caller_mode.as_str())
            .with_tool_name(tool_name);
        if let Some(uri) = target_uri {
            event = event.with_target(uri);
        }
        if let Some(gid) = goal_run_id {
            event = event.with_goal_run_id(gid);
        }
        if let Err(e) = self.audit_log.append(&mut event) {
            tracing::warn!(
                tool = tool_name,
                error = %e,
                "failed to write tool-call audit entry"
            );
        }
    }
}

// ── MCP Server ───────────────────────────────────────────────────

/// The MCP gateway server. Holds shared state and the tool router.
pub struct TaGatewayServer {
    state: Arc<Mutex<GatewayState>>,
    tool_router: ToolRouter<Self>,
}

// Tool definitions. Each `#[tool]` method delegates to a handler in tools/.
#[tool_router]
impl TaGatewayServer {
    pub fn new(config: GatewayConfig) -> Result<Self, GatewayError> {
        let state = GatewayState::new(config)?;
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            tool_router: Self::tool_router(),
        })
    }

    pub fn with_state(state: GatewayState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            tool_router: Self::tool_router(),
        }
    }

    pub fn state(&self) -> &Arc<Mutex<GatewayState>> {
        &self.state
    }

    // ── Goal tools ───────────────────────────────────────────

    /// Log a tool call to the audit log (v0.10.15).
    fn audit(&self, tool_name: &str, target_uri: Option<&str>, goal_run_id: Option<Uuid>) {
        if let Ok(mut state) = self.state.lock() {
            state.audit_tool_call(tool_name, target_uri, goal_run_id);
        }
    }

    #[tool(
        description = "Start a new goal run and launch an implementation agent. Performs the full lifecycle: creates an overlay workspace copy, injects CLAUDE.md context, spawns the agent in the background, and emits lifecycle events. The agent runs headlessly and builds a draft on exit. Track progress via ta_event_subscribe. Returns the goal_run_id."
    )]
    fn ta_goal_start(
        &self,
        Parameters(params): Parameters<GoalStartParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_goal_start", None, None);
        tools::goal::handle_goal_start(&self.state, params)
    }

    #[tool(description = "Get the current status of a goal run, including its state and metadata.")]
    fn ta_goal_status(
        &self,
        Parameters(params): Parameters<GoalIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_goal_status", None, params.goal_run_id.parse().ok());
        tools::goal::handle_goal_status(&self.state, &params.goal_run_id)
    }

    #[tool(
        description = "List all goal runs, optionally filtered by state (e.g., 'running', 'pr_ready', 'completed')."
    )]
    fn ta_goal_list(
        &self,
        Parameters(params): Parameters<GoalListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_goal_list", None, None);
        tools::goal::handle_goal_list(&self.state, params)
    }

    // ── Filesystem tools ─────────────────────────────────────

    #[tool(
        description = "Read a file from the project source directory. The file is snapshotted for later diff generation."
    )]
    fn ta_fs_read(
        &self,
        Parameters(params): Parameters<FsReadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_fs_read",
            Some(&format!("fs://workspace/{}", params.path)),
            params.goal_run_id.parse().ok(),
        );
        tools::fs::handle_fs_read(&self.state, params)
    }

    #[tool(
        description = "Write a file to the staging workspace. Creates a ChangeSet tracking the modification. Nothing touches the real filesystem until approved and applied."
    )]
    fn ta_fs_write(
        &self,
        Parameters(params): Parameters<FsWriteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_fs_write",
            Some(&format!("fs://workspace/{}", params.path)),
            params.goal_run_id.parse().ok(),
        );
        tools::fs::handle_fs_write(&self.state, params)
    }

    #[tool(description = "List all files currently staged for a goal run.")]
    fn ta_fs_list(
        &self,
        Parameters(params): Parameters<FsListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_fs_list", None, params.goal_run_id.parse().ok());
        tools::fs::handle_fs_list(&self.state, params)
    }

    #[tool(description = "Show the diff for a staged file compared to the original source.")]
    fn ta_fs_diff(
        &self,
        Parameters(params): Parameters<FsDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_fs_diff", None, params.goal_run_id.parse().ok());
        tools::fs::handle_fs_diff(&self.state, params)
    }

    // ── PR tools ─────────────────────────────────────────────

    #[tool(
        description = "Bundle all staged changes for a goal into a PR package for human review. Transitions the goal to PrReady state."
    )]
    fn ta_pr_build(
        &self,
        Parameters(params): Parameters<PrBuildParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_pr_build", None, params.goal_run_id.parse().ok());
        tools::draft::handle_pr_build(&self.state, params)
    }

    #[tool(description = "Check the review status of a goal's PR package.")]
    fn ta_pr_status(
        &self,
        Parameters(params): Parameters<GoalIdParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_pr_status", None, params.goal_run_id.parse().ok());
        tools::draft::handle_pr_status(&self.state, params)
    }

    // ── Inner-loop tools (v0.4.1 — macro goals) ────────────

    #[tool(
        description = "Manage draft packages within a macro goal session. Actions: build (package changes), submit (send for human review), status (check review status), list (list drafts)."
    )]
    fn ta_draft(
        &self,
        Parameters(params): Parameters<DraftToolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_draft",
            None,
            params.goal_run_id.as_deref().and_then(|id| id.parse().ok()),
        );
        tools::draft::handle_draft(&self.state, params)
    }

    #[tool(
        description = "Manage sub-goals within a macro goal session. Actions: start (create a sub-goal), status (check sub-goal progress). Set launch:true to spawn the implementation agent in the background."
    )]
    fn ta_goal_inner(
        &self,
        Parameters(params): Parameters<GoalToolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_goal_inner",
            None,
            params
                .macro_goal_id
                .as_deref()
                .and_then(|id| id.parse().ok()),
        );
        tools::goal::handle_goal_inner(&self.state, params)
    }

    #[tool(
        description = "Read or propose updates to the project development plan. Actions: read (view plan), update (propose a status note for a phase)."
    )]
    fn ta_plan(
        &self,
        Parameters(params): Parameters<PlanToolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_plan", None, None);
        tools::plan::handle_plan(&self.state, params)
    }

    /// Persistent memory store for cross-agent context.
    #[tool]
    fn ta_context(
        &self,
        Parameters(params): Parameters<ContextToolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_context", None, None);
        tools::context::handle_context(&self.state, params)
    }

    // ── Agent status tool (v0.9.6) ─────────────────────────────

    #[tool(
        description = "Query active agent sessions for orchestration diagnostics. Actions: list (all active agents), status (specific agent by agent_id)."
    )]
    fn ta_agent_status(
        &self,
        Parameters(params): Parameters<AgentStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_agent_status", None, None);
        tools::agent::handle_agent_status(&self.state, params)
    }

    // ── Event subscription tool (v0.9.4) ─────────────────────

    #[tool(
        description = "Query TA events for orchestration. Actions: query (events matching filter), watch (events since timestamp — cursor-based), latest (most recent events). Use event_types filter for specific events like goal_completed, goal_failed, draft_built. Pass the returned cursor as 'since' to get only new events without polling."
    )]
    fn ta_event_subscribe(
        &self,
        Parameters(params): Parameters<tools::event::EventSubscribeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_event_subscribe", None, None);
        tools::event::handle_event_subscribe(&self.state, params)
    }

    // ── Workflow tools (v0.9.8.2) ─────────────────────────────

    #[tool(
        description = "Manage multi-stage workflows. Actions: start (begin a workflow from a YAML definition), status (get workflow status), list (list active/completed workflows), cancel (cancel a running workflow), history (show stage transitions and verdicts)."
    )]
    fn ta_workflow(
        &self,
        Parameters(params): Parameters<tools::workflow::WorkflowToolParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_workflow", None, None);
        tools::workflow::handle_workflow(&self.state, params)
    }

    // ── Interactive tools (v0.9.9.1) ─────────────────────────

    #[tool(
        description = "Ask the human a question and wait for their response. Use this when you need clarification, want to propose options, or need human input before proceeding. The question is delivered through whatever channel the human is using (terminal, Slack, Discord, web UI). Your execution pauses until they respond or the timeout expires."
    )]
    fn ta_ask_human(
        &self,
        Parameters(params): Parameters<tools::human::AskHumanParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_ask_human", None, None);
        tools::human::handle_ask_human(&self.state, params)
    }
}

// ── ServerHandler implementation ─────────────────────────────────

#[tool_handler]
impl ServerHandler for TaGatewayServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "trusted-autonomy".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: Some("Trusted Autonomy".into()),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Trusted Autonomy MCP server. All file operations are staged \
                 and require human review before being applied to the real \
                 filesystem. Start with ta_goal_start, then use ta_fs_write \
                 to stage changes, and ta_pr_build when ready for review."
                    .into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_changeset::PRStatus;
    use tempfile::tempdir;

    fn test_server() -> (TaGatewayServer, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let server = TaGatewayServer::new(config).unwrap();
        (server, dir)
    }

    fn test_server_with_source(
        source_content: &[(&str, &[u8])],
    ) -> (TaGatewayServer, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        for (path, content) in source_content {
            let full_path = dir.path().join(path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full_path, content).unwrap();
        }
        let config = GatewayConfig::for_project(dir.path());
        let server = TaGatewayServer::new(config).unwrap();
        (server, dir)
    }

    fn start_goal(server: &TaGatewayServer) -> Uuid {
        let mut state = server.state.lock().unwrap();
        let goal = state
            .start_goal("Test Goal", "Testing the system", "test-agent")
            .unwrap();
        goal.goal_run_id
    }

    #[test]
    fn tool_count_matches_expected() {
        let (server, _dir) = test_server();
        let tools = server.tool_router.list_all();
        // 17 tools: goal_start, goal_status, goal_list,
        //           fs_read, fs_write, fs_list, fs_diff,
        //           pr_build, pr_status,
        //           ta_draft, ta_goal_inner, ta_plan, ta_context,
        //           ta_agent_status (v0.9.6), ta_event_subscribe (v0.9.4),
        //           ta_workflow (v0.9.8.2), ta_ask_human (v0.9.9.1)
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(tools.len(), 17, "expected 17 tools, got: {:?}", names);
    }

    #[test]
    fn tool_names_are_prefixed() {
        let (server, _dir) = test_server();
        let tools = server.tool_router.list_all();
        for tool in &tools {
            assert!(
                tool.name.starts_with("ta_"),
                "tool '{}' should be prefixed with 'ta_'",
                tool.name
            );
        }
    }

    #[test]
    fn start_goal_creates_running_goal() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        let state = server.state.lock().unwrap();
        let goal = state.goal_store.get(goal_id).unwrap().unwrap();
        assert_eq!(goal.state, GoalRunState::Running);
        assert_eq!(goal.title, "Test Goal");
    }

    #[test]
    fn start_goal_issues_manifest() {
        let (server, _dir) = test_server();
        let _goal_id = start_goal(&server);

        let state = server.state.lock().unwrap();
        let decision = state.policy_engine.evaluate(&PolicyRequest {
            agent_id: "test-agent".to_string(),
            tool: "fs".to_string(),
            verb: "read".to_string(),
            target_uri: "fs://workspace/src/main.rs".to_string(),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn start_goal_creates_connector() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        let state = server.state.lock().unwrap();
        assert!(state.connectors.contains_key(&goal_id));
    }

    #[test]
    fn fs_write_stages_file() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        let mut state = server.state.lock().unwrap();
        let connector = state.connectors.get_mut(&goal_id).unwrap();
        let cs = connector
            .write_patch("hello.txt", b"Hello from TA!")
            .unwrap();
        assert_eq!(cs.target_uri, "fs://workspace/hello.txt");

        let content = connector.read_staged("hello.txt").unwrap();
        assert_eq!(content, b"Hello from TA!");
    }

    #[test]
    fn fs_write_accumulates_changesets() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        let mut state = server.state.lock().unwrap();
        let connector = state.connectors.get_mut(&goal_id).unwrap();
        connector.write_patch("a.txt", b"aaa").unwrap();
        connector.write_patch("b.txt", b"bbb").unwrap();

        let changesets = connector.list_changesets().unwrap();
        assert_eq!(changesets.len(), 2);
    }

    #[test]
    fn fs_list_shows_staged_files() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        let mut state = server.state.lock().unwrap();
        let connector = state.connectors.get_mut(&goal_id).unwrap();
        connector
            .write_patch("src/lib.rs", b"pub fn main() {}")
            .unwrap();
        connector.write_patch("README.md", b"# Hello").unwrap();

        let files = connector.list_staged().unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn fs_read_snapshots_source() {
        let (server, _dir) = test_server_with_source(&[("existing.txt", b"original content")]);
        let goal_id = start_goal(&server);

        let mut state = server.state.lock().unwrap();
        let source_dir = state.config.workspace_root.clone();
        let connector = state.connectors.get_mut(&goal_id).unwrap();

        let content = connector.read_source(&source_dir, "existing.txt").unwrap();
        assert_eq!(content, b"original content");
    }

    #[test]
    fn pr_build_creates_package() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        {
            let mut state = server.state.lock().unwrap();
            let connector = state.connectors.get_mut(&goal_id).unwrap();
            connector.write_patch("file.txt", b"content").unwrap();
        }

        let state = server.state.lock().unwrap();
        let goal = state.goal_store.get(goal_id).unwrap().unwrap();
        let connector = state.connectors.get(&goal_id).unwrap();
        let pkg = connector
            .build_pr_package(&goal.title, &goal.objective, "Added file", "Test PR")
            .unwrap();

        assert_eq!(pkg.changes.artifacts.len(), 1);
        assert_eq!(pkg.status, PRStatus::PendingReview);
    }

    #[test]
    fn pr_build_transitions_to_pr_ready() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        {
            let mut state = server.state.lock().unwrap();
            let connector = state.connectors.get_mut(&goal_id).unwrap();
            connector.write_patch("file.txt", b"content").unwrap();
        }

        let mut state = server.state.lock().unwrap();
        let goal = state.goal_store.get(goal_id).unwrap().unwrap();
        let connector = state.connectors.get(&goal_id).unwrap();
        let pkg = connector
            .build_pr_package(&goal.title, &goal.objective, "what", "why")
            .unwrap();
        let package_id = pkg.package_id;
        state.pr_packages.insert(package_id, pkg);

        let mut updated = goal;
        updated.pr_package_id = Some(package_id);
        updated.transition(GoalRunState::PrReady).unwrap();
        state.goal_store.save(&updated).unwrap();

        let reloaded = state.goal_store.get(goal_id).unwrap().unwrap();
        assert_eq!(reloaded.state, GoalRunState::PrReady);
        assert_eq!(reloaded.pr_package_id, Some(package_id));
    }

    #[test]
    fn policy_denies_unknown_agent() {
        let (server, _dir) = test_server();
        let state = server.state.lock().unwrap();

        let decision = state.policy_engine.evaluate(&PolicyRequest {
            agent_id: "unknown".to_string(),
            tool: "fs".to_string(),
            verb: "read".to_string(),
            target_uri: "fs://workspace/test.txt".to_string(),
        });
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn policy_allows_after_goal_start() {
        let (server, _dir) = test_server();
        let _goal_id = start_goal(&server);
        let state = server.state.lock().unwrap();

        let decision = state.policy_engine.evaluate(&PolicyRequest {
            agent_id: "test-agent".to_string(),
            tool: "fs".to_string(),
            verb: "write_patch".to_string(),
            target_uri: "fs://workspace/src/main.rs".to_string(),
        });
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn events_logged_on_goal_start() {
        let dir = tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let events_path = config.events_log.clone();
        let server = TaGatewayServer::new(config).unwrap();
        let _goal_id = start_goal(&server);

        let content = std::fs::read_to_string(&events_path).unwrap();
        assert!(content.contains("goal_created"));
    }

    #[test]
    fn multiple_goals_are_isolated() {
        let (server, _dir) = test_server();
        let id1 = start_goal(&server);
        let id2 = {
            let mut state = server.state.lock().unwrap();
            let goal = state
                .start_goal("Goal 2", "Second goal", "agent-2")
                .unwrap();
            goal.goal_run_id
        };

        assert_ne!(id1, id2);

        let mut state = server.state.lock().unwrap();
        state
            .connectors
            .get_mut(&id1)
            .unwrap()
            .write_patch("g1.txt", b"goal 1")
            .unwrap();
        state
            .connectors
            .get_mut(&id2)
            .unwrap()
            .write_patch("g2.txt", b"goal 2")
            .unwrap();

        let files1 = state.connectors.get(&id1).unwrap().list_staged().unwrap();
        let files2 = state.connectors.get(&id2).unwrap().list_staged().unwrap();
        assert_eq!(files1.len(), 1);
        assert_eq!(files2.len(), 1);
        assert!(files1.contains(&"g1.txt".to_string()));
        assert!(files2.contains(&"g2.txt".to_string()));
    }

    // v0.9.3: CallerMode tests.

    #[test]
    fn caller_mode_normal_allows_all_tools() {
        let mode = CallerMode::Normal;
        assert!(!mode.is_tool_forbidden("ta_fs_write"));
        assert!(!mode.is_tool_forbidden("ta_goal_start"));
        assert!(!mode.is_tool_forbidden("ta_draft"));
    }

    #[test]
    fn caller_mode_orchestrator_blocks_mutation_tools() {
        let mode = CallerMode::Orchestrator;
        assert!(mode.is_tool_forbidden("ta_fs_write"));
        assert!(mode.is_tool_forbidden("ta_pr_build"));
        assert!(mode.is_tool_forbidden("ta_fs_diff"));
        assert!(!mode.is_tool_forbidden("ta_plan"));
        assert!(!mode.is_tool_forbidden("ta_goal_start"));
        assert!(!mode.is_tool_forbidden("ta_draft"));
        assert!(!mode.is_tool_forbidden("ta_context"));
        assert!(!mode.is_tool_forbidden("ta_agent_status"));
        assert!(!mode.is_tool_forbidden("ta_goal_list"));
    }

    #[test]
    fn caller_mode_unrestricted_allows_all_tools() {
        let mode = CallerMode::Unrestricted;
        assert!(!mode.is_tool_forbidden("ta_fs_write"));
        assert!(!mode.is_tool_forbidden("ta_goal_start"));
    }

    #[test]
    fn caller_mode_from_env_defaults_to_normal() {
        std::env::remove_var("TA_CALLER_MODE");
        assert_eq!(CallerMode::from_env(), CallerMode::Normal);
    }

    #[test]
    fn validate_goal_exists_rejects_fake_id() {
        let (server, _dir) = test_server();
        let state = server.state.lock().unwrap();
        let fake_id = Uuid::parse_str("00000000-0000-0000-0000-000000000000").unwrap();
        let result = crate::validation::validate_goal_exists(&state.goal_store, fake_id);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("goal_run_id not found"),
            "expected 'goal_run_id not found' error, got: {}",
            err.message
        );
    }

    #[test]
    fn validate_goal_exists_accepts_real_id() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);
        let state = server.state.lock().unwrap();
        let result = crate::validation::validate_goal_exists(&state.goal_store, goal_id);
        assert!(result.is_ok());
    }

    // v0.9.6: Agent session tracking tests.

    #[test]
    fn agent_session_tracking() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);

        {
            let mut state = server.state.lock().unwrap();
            state.touch_agent_session("agent-1", "claude-code", Some(goal_id));
            assert_eq!(state.active_agents.len(), 1);
            assert_eq!(state.active_agents["agent-1"].agent_type, "claude-code");
            assert_eq!(state.active_agents["agent-1"].goal_run_id, Some(goal_id));
        }

        {
            let mut state = server.state.lock().unwrap();
            // Touch again — should update heartbeat, not duplicate.
            state.touch_agent_session("agent-1", "claude-code", Some(goal_id));
            assert_eq!(state.active_agents.len(), 1);
        }

        {
            let mut state = server.state.lock().unwrap();
            state.end_agent_session("agent-1");
            assert!(state.active_agents.is_empty());
        }
    }

    #[test]
    fn agent_status_tool_exists() {
        let (server, _dir) = test_server();
        let tools = server.tool_router.list_all();
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert!(
            names.contains(&"ta_agent_status".to_string()),
            "ta_agent_status tool not found in: {:?}",
            names
        );
    }

    #[test]
    fn caller_mode_as_str() {
        assert_eq!(CallerMode::Normal.as_str(), "normal");
        assert_eq!(CallerMode::Orchestrator.as_str(), "orchestrator");
        assert_eq!(CallerMode::Unrestricted.as_str(), "unrestricted");
    }

    // v0.9.4: Event subscription tool test.
    #[test]
    fn event_subscribe_tool_exists() {
        let (server, _dir) = test_server();
        let tools = server.tool_router.list_all();
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert!(
            names.contains(&"ta_event_subscribe".to_string()),
            "ta_event_subscribe tool not found in: {:?}",
            names
        );
    }

    // v0.10.6: Orchestrator write whitelist tests.
    #[test]
    fn orchestrator_blocks_arbitrary_writes() {
        let mode = CallerMode::Orchestrator;
        assert!(mode.is_tool_forbidden("ta_fs_write"));
        assert!(!mode.is_write_whitelisted("src/main.rs"));
        assert!(!mode.is_write_whitelisted("Cargo.toml"));
    }

    #[test]
    fn orchestrator_allows_release_artifact_writes() {
        let mode = CallerMode::Orchestrator;
        assert!(mode.is_write_whitelisted(".release-draft.md"));
        assert!(mode.is_write_whitelisted("CHANGELOG.md"));
        assert!(mode.is_write_whitelisted("version.json"));
        assert!(mode.is_write_whitelisted(".press-release-draft.md"));
    }

    #[test]
    fn orchestrator_whitelist_matches_filename() {
        let mode = CallerMode::Orchestrator;
        // Paths with directories should still match by filename.
        assert!(mode.is_write_whitelisted("some/path/.release-draft.md"));
        assert!(mode.is_write_whitelisted("docs/CHANGELOG.md"));
    }

    #[test]
    fn normal_mode_allows_all_writes() {
        let mode = CallerMode::Normal;
        assert!(mode.is_write_whitelisted("anything.rs"));
        assert!(mode.is_write_whitelisted("src/main.rs"));
    }

    #[test]
    fn unrestricted_mode_allows_all_writes() {
        let mode = CallerMode::Unrestricted;
        assert!(mode.is_write_whitelisted("anything.rs"));
    }

    // v0.10.15: Audit tool-call and agent_id resolution tests.

    #[test]
    fn resolve_agent_id_from_env() {
        let (server, _dir) = test_server();
        std::env::set_var("TA_AGENT_ID", "env-agent-42");
        let state = server.state.lock().unwrap();
        let agent_id = state.resolve_agent_id();
        assert_eq!(agent_id, "env-agent-42");
        std::env::remove_var("TA_AGENT_ID");
    }

    #[test]
    fn resolve_agent_id_falls_back_to_dev_session() {
        let (server, _dir) = test_server();
        std::env::remove_var("TA_AGENT_ID");
        let mut state = server.state.lock().unwrap();
        state.dev_session_id = Some("dev-session-99".to_string());
        assert_eq!(state.resolve_agent_id(), "dev-session-99");
    }

    #[test]
    fn resolve_agent_id_falls_back_to_unknown() {
        let (server, _dir) = test_server();
        std::env::remove_var("TA_AGENT_ID");
        let mut state = server.state.lock().unwrap();
        state.dev_session_id = None;
        assert_eq!(state.resolve_agent_id(), "unknown");
    }

    #[test]
    fn audit_tool_call_writes_to_log() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);
        {
            let mut state = server.state.lock().unwrap();
            state.audit_tool_call("ta_fs_write", Some("fs://workspace/foo.rs"), Some(goal_id));
        }
        let state = server.state.lock().unwrap();
        let events = ta_audit::AuditLog::read_all(state.audit_log.path()).unwrap();
        assert!(!events.is_empty());
        let last = events.last().unwrap();
        assert_eq!(last.action, ta_audit::AuditAction::ToolCall);
        assert_eq!(last.tool_name.as_deref(), Some("ta_fs_write"));
        assert_eq!(last.caller_mode.as_deref(), Some("normal"));
        assert_eq!(last.goal_run_id, Some(goal_id));
        assert_eq!(last.target_uri.as_deref(), Some("fs://workspace/foo.rs"));
    }
}
