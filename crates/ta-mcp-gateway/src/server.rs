// server.rs — MCP gateway server for Trusted Autonomy.
//
// TaGatewayServer implements the rmcp ServerHandler trait, exposing TA's
// staging, policy, and goal lifecycle as MCP tools. Every file operation
// flows through policy → staging → changeset → audit, ensuring the core
// thesis holds: all agent actions are mediated.
//
// Tools (prefixed `ta_` for namespacing):
//   ta_goal_start  — create a GoalRun, issue manifest, allocate workspace
//   ta_goal_status — get current state of a GoalRun
//   ta_goal_list   — list GoalRuns (optionally filtered by state)
//   ta_fs_read     — read a file from the source directory
//   ta_fs_write    — write a file to staging (creates ChangeSet)
//   ta_fs_list     — list staged files for a goal
//   ta_fs_diff     — show diff for a staged file
//   ta_pr_build    — bundle staged changes into a PR package
//   ta_pr_status   — check PR package review status

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use ta_audit::AuditLog;
use ta_changeset::interaction::{InteractionRequest, Notification};
use ta_changeset::pr_package::PRPackage;
use ta_changeset::review_channel::{ReviewChannel, ReviewChannelError};
use ta_changeset::terminal_channel::AutoApproveChannel;
use ta_connector_fs::FsConnector;
use ta_goal::{EventDispatcher, GoalRun, GoalRunState, GoalRunStore, LogSink, TaEvent};
use ta_memory::{AutoCapture, DraftRejectEvent, FsMemoryStore, MemoryStore};
use ta_policy::{
    AlignmentProfile, CompilerOptions, PolicyCompiler, PolicyDecision, PolicyEngine, PolicyRequest,
};
use ta_workspace::{JsonFileStore, StagingWorkspace};

use ta_changeset::draft_package::PendingAction;

use crate::config::GatewayConfig;
use crate::error::GatewayError;
use crate::interceptor::ToolCallInterceptor;

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
}

// ── Macro goal / inner-loop parameter types ─────────────────────

/// Parameters for `ta_draft` (inner-loop agent tool).
/// Mirrors `ta draft` CLI subcommands as a single MCP tool with action dispatch.
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
}

/// Parameters for `ta_goal` (inner-loop agent tool).
/// Mirrors `ta goal` CLI subcommands for sub-goal management within macro sessions.
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
    /// When true, spawns `ta run --headless` in the background and returns immediately.
    /// The orchestrator can poll status via action:"status" to track progress.
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
/// Mirrors `ta plan` CLI subcommands for reading/proposing plan updates.
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
    /// Source framework identifier (v0.5.6). Defaults to "agent".
    /// Examples: "claude-code", "codex", "cursor", "claude-flow".
    #[serde(default)]
    pub source: Option<String>,
    /// Associate this entry with a specific goal (v0.5.6).
    #[serde(default)]
    pub goal_id: Option<String>,
    /// Knowledge category (v0.5.6): "convention", "architecture",
    /// "history", "preference", "relationship".
    #[serde(default)]
    pub category: Option<String>,
    /// Search query text (used with "search" action, v0.5.6).
    #[serde(default)]
    pub query: Option<String>,
}

// ── Gateway state ────────────────────────────────────────────────

/// Shared mutable state for the gateway server.
///
/// Holds all the components that tool handlers need: policy engine,
/// goal store, active connectors, audit log, and event dispatcher.
pub struct GatewayState {
    pub config: GatewayConfig,
    pub policy_engine: PolicyEngine,
    pub goal_store: GoalRunStore,
    pub connectors: HashMap<Uuid, FsConnector<JsonFileStore>>,
    pub pr_packages: HashMap<Uuid, PRPackage>,
    pub audit_log: AuditLog,
    pub event_dispatcher: EventDispatcher,
    /// ReviewChannel for bidirectional human-agent communication (v0.4.1.1).
    /// Defaults to AutoApproveChannel when no interactive channel is configured.
    pub review_channel: Box<dyn ReviewChannel>,
    /// Persistent memory store for cross-agent context (v0.5.4).
    pub memory_store: FsMemoryStore,
    /// Auto-capture config for lifecycle events (v0.5.6).
    pub auto_capture_config: ta_memory::AutoCaptureConfig,
    /// MCP tool call interceptor for capturing external actions (v0.5.1).
    pub interceptor: ToolCallInterceptor,
    /// Pending actions captured by the interceptor, keyed by goal_run_id.
    pub pending_actions: HashMap<Uuid, Vec<PendingAction>>,
}

impl GatewayState {
    /// Initialize gateway state from config. Creates directories and opens logs.
    pub fn new(config: GatewayConfig) -> Result<Self, GatewayError> {
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let audit_log = AuditLog::open(&config.audit_log)?;

        let mut event_dispatcher = EventDispatcher::new();
        event_dispatcher.add_sink(Box::new(LogSink::new(&config.events_log)));
        let memory_store = FsMemoryStore::new(config.workspace_root.join(".ta").join("memory"));

        // Load auto-capture config from .ta/workflow.toml (v0.5.6).
        let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
        let auto_capture_config = ta_memory::auto_capture::load_config(&workflow_toml);

        Ok(Self {
            config,
            policy_engine: PolicyEngine::new(),
            goal_store,
            connectors: HashMap::new(),
            pr_packages: HashMap::new(),
            audit_log,
            event_dispatcher,
            review_channel: Box::new(AutoApproveChannel::new()),
            memory_store,
            auto_capture_config,
            interceptor: ToolCallInterceptor::new(),
            pending_actions: HashMap::new(),
        })
    }

    /// Start a new goal: create GoalRun, issue manifest, set up connector.
    pub fn start_goal(
        &mut self,
        title: &str,
        objective: &str,
        agent_id: &str,
    ) -> Result<GoalRun, GatewayError> {
        // Create GoalRun with paths derived from config.
        let goal_run_id = Uuid::new_v4();
        let staging_path = self.config.staging_dir.join(goal_run_id.to_string());
        let store_path = self.config.store_dir.join(goal_run_id.to_string());

        let mut goal_run = GoalRun::new(title, objective, agent_id, staging_path, store_path);
        // Override the random ID so we control it.
        goal_run.goal_run_id = goal_run_id;

        // Compile a default developer manifest via the Policy Compiler (v0.4.0).
        // Uses AlignmentProfile::default_developer() which grants fs read/write/apply
        // on workspace and denies network/credential access.
        let profile = AlignmentProfile::default_developer();
        let options = CompilerOptions::default();
        let manifest =
            PolicyCompiler::compile_with_id(goal_run.manifest_id, agent_id, &profile, &options)
                .map_err(|e| GatewayError::Other(format!("policy compilation failed: {}", e)))?;
        self.policy_engine.load_manifest(manifest);

        // Create staging workspace and change store for this goal.
        let staging = StagingWorkspace::new(goal_run_id.to_string(), &self.config.staging_dir)?;
        let store = JsonFileStore::new(self.config.store_dir.join(goal_run_id.to_string()))?;
        let connector = FsConnector::new(goal_run_id.to_string(), staging, store, agent_id);
        self.connectors.insert(goal_run_id, connector);

        // Transition Created → Configured → Running.
        goal_run.transition(GoalRunState::Configured)?;
        goal_run.transition(GoalRunState::Running)?;
        self.goal_store.save(&goal_run)?;

        // Emit events.
        self.event_dispatcher
            .dispatch(&TaEvent::goal_created(goal_run_id, title, agent_id));

        Ok(goal_run)
    }

    /// Start a new goal with a custom alignment profile (v0.4.0).
    ///
    /// Like `start_goal`, but compiles the provided AlignmentProfile into
    /// the capability manifest instead of using the default developer profile.
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
    fn check_policy(
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

        // Persist to disk so the CLI can read it.
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

    /// Set a custom ReviewChannel (e.g., TerminalChannel for interactive sessions).
    pub fn set_review_channel(&mut self, channel: Box<dyn ReviewChannel>) {
        self.review_channel = channel;
    }

    /// Route an interaction request through the configured ReviewChannel.
    /// Returns the human's response. Logs the interaction as an event.
    pub fn request_review(
        &self,
        request: &InteractionRequest,
    ) -> Result<ta_changeset::interaction::InteractionResponse, ReviewChannelError> {
        let response = self.review_channel.request_interaction(request)?;

        // Log the interaction for audit trail.
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

    /// Get the agent_id for a goal run.
    fn agent_for_goal(&self, goal_run_id: Uuid) -> Result<String, GatewayError> {
        let goal = self
            .goal_store
            .get(goal_run_id)?
            .ok_or(GatewayError::GoalNotFound(goal_run_id))?;
        Ok(goal.agent_id)
    }
}

// ── MCP Server ───────────────────────────────────────────────────

/// The MCP gateway server. Holds shared state and the tool router.
pub struct TaGatewayServer {
    state: Arc<Mutex<GatewayState>>,
    tool_router: ToolRouter<Self>,
}

// Tool definitions. Each `#[tool]` method becomes an MCP tool that
// Claude Code (or any MCP client) can call.
#[tool_router]
impl TaGatewayServer {
    /// Create a new gateway server from config.
    pub fn new(config: GatewayConfig) -> Result<Self, GatewayError> {
        let state = GatewayState::new(config)?;
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            tool_router: Self::tool_router(),
        })
    }

    /// Create a server wrapping existing state (for testing).
    pub fn with_state(state: GatewayState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            tool_router: Self::tool_router(),
        }
    }

    /// Get a reference to the shared state (for testing).
    pub fn state(&self) -> &Arc<Mutex<GatewayState>> {
        &self.state
    }

    // ── Goal tools ───────────────────────────────────────────

    #[tool(
        description = "Start a new goal run. Creates a workspace, issues a capability manifest, and transitions to Running state. Returns the goal_run_id to use with other tools."
    )]
    fn ta_goal_start(
        &self,
        Parameters(params): Parameters<GoalStartParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run = state
            .start_goal(&params.title, &params.objective, &params.agent_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = serde_json::json!({
            "goal_run_id": goal_run.goal_run_id.to_string(),
            "state": goal_run.state.to_string(),
            "title": goal_run.title,
            "agent_id": goal_run.agent_id,
            "manifest_id": goal_run.manifest_id.to_string(),
        });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    #[tool(description = "Get the current status of a goal run, including its state and metadata.")]
    fn ta_goal_status(
        &self,
        Parameters(params): Parameters<GoalIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run_id = parse_uuid(&params.goal_run_id)?;
        let goal = state
            .goal_store
            .get(goal_run_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
            })?;

        let response = serde_json::json!({
            "goal_run_id": goal.goal_run_id.to_string(),
            "title": goal.title,
            "objective": goal.objective,
            "state": goal.state.to_string(),
            "agent_id": goal.agent_id,
            "created_at": goal.created_at.to_rfc3339(),
            "updated_at": goal.updated_at.to_rfc3339(),
            "pr_package_id": goal.pr_package_id.map(|id| id.to_string()),
        });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    #[tool(
        description = "List all goal runs, optionally filtered by state (e.g., 'running', 'pr_ready', 'completed')."
    )]
    fn ta_goal_list(
        &self,
        Parameters(params): Parameters<GoalListParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goals = if let Some(ref state_filter) = params.state {
            state
                .goal_store
                .list_by_state(state_filter)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
        } else {
            state
                .goal_store
                .list()
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
        };

        let items: Vec<serde_json::Value> = goals
            .iter()
            .map(|g| {
                serde_json::json!({
                    "goal_run_id": g.goal_run_id.to_string(),
                    "title": g.title,
                    "state": g.state.to_string(),
                    "agent_id": g.agent_id,
                    "created_at": g.created_at.to_rfc3339(),
                })
            })
            .collect();

        let response = serde_json::json!({ "goals": items, "count": items.len() });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    // ── Filesystem tools ─────────────────────────────────────

    #[tool(
        description = "Read a file from the project source directory. The file is snapshotted for later diff generation."
    )]
    fn ta_fs_read(
        &self,
        Parameters(params): Parameters<FsReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run_id = parse_uuid(&params.goal_run_id)?;
        let agent_id = state
            .agent_for_goal(goal_run_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Check policy.
        let decision = state
            .check_policy(&agent_id, "read", &params.path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        enforce_policy(&decision)?;

        // Clone workspace_root before taking mutable borrow on connector.
        let workspace_root = state.config.workspace_root.clone();
        let connector = state.connectors.get_mut(&goal_run_id).ok_or_else(|| {
            McpError::invalid_params(
                format!("no active connector for goal: {}", goal_run_id),
                None,
            )
        })?;

        let content = connector
            .read_source(&workspace_root, &params.path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let text = String::from_utf8_lossy(&content).to_string();
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Write a file to the staging workspace. Creates a ChangeSet tracking the modification. Nothing touches the real filesystem until approved and applied."
    )]
    fn ta_fs_write(
        &self,
        Parameters(params): Parameters<FsWriteParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run_id = parse_uuid(&params.goal_run_id)?;
        let agent_id = state
            .agent_for_goal(goal_run_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Check policy.
        let decision = state
            .check_policy(&agent_id, "write_patch", &params.path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        enforce_policy(&decision)?;

        let connector = state.connectors.get_mut(&goal_run_id).ok_or_else(|| {
            McpError::invalid_params(
                format!("no active connector for goal: {}", goal_run_id),
                None,
            )
        })?;

        let changeset = connector
            .write_patch(&params.path, params.content.as_bytes())
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = serde_json::json!({
            "changeset_id": changeset.changeset_id.to_string(),
            "target_uri": changeset.target_uri,
            "status": "staged",
        });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    #[tool(description = "List all files currently staged for a goal run.")]
    fn ta_fs_list(
        &self,
        Parameters(params): Parameters<FsListParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run_id = parse_uuid(&params.goal_run_id)?;

        let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
            McpError::invalid_params(
                format!("no active connector for goal: {}", goal_run_id),
                None,
            )
        })?;

        let files = connector
            .list_staged()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let response = serde_json::json!({ "files": files, "count": files.len() });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    #[tool(description = "Show the diff for a staged file compared to the original source.")]
    fn ta_fs_diff(
        &self,
        Parameters(params): Parameters<FsDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run_id = parse_uuid(&params.goal_run_id)?;

        let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
            McpError::invalid_params(
                format!("no active connector for goal: {}", goal_run_id),
                None,
            )
        })?;

        let diff = connector
            .diff_file(&params.path)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        match diff {
            Some(diff_text) => Ok(CallToolResult::success(vec![Content::text(diff_text)])),
            None => Ok(CallToolResult::success(vec![Content::text(
                "No changes (file is identical to source).",
            )])),
        }
    }

    // ── PR tools ─────────────────────────────────────────────

    #[tool(
        description = "Bundle all staged changes for a goal into a PR package for human review. Transitions the goal to PrReady state."
    )]
    fn ta_pr_build(
        &self,
        Parameters(params): Parameters<PrBuildParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run_id = parse_uuid(&params.goal_run_id)?;

        // Get goal details for the PR package.
        let goal = state
            .goal_store
            .get(goal_run_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
            })?;

        let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
            McpError::invalid_params(
                format!("no active connector for goal: {}", goal_run_id),
                None,
            )
        })?;

        let pr_package = connector
            .build_pr_package(&goal.title, &goal.objective, &params.summary, &params.title)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let package_id = pr_package.package_id;
        state
            .save_pr_package(pr_package)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Transition goal to PrReady.
        let mut updated_goal = goal;
        updated_goal.pr_package_id = Some(package_id);
        updated_goal
            .transition(GoalRunState::PrReady)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        state
            .goal_store
            .save(&updated_goal)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Emit event.
        state.event_dispatcher.dispatch(&TaEvent::PrReady {
            goal_run_id,
            pr_package_id: package_id,
            summary: params.summary,
            timestamp: Utc::now(),
        });

        let response = serde_json::json!({
            "pr_package_id": package_id.to_string(),
            "goal_run_id": goal_run_id.to_string(),
            "state": "pr_ready",
            "message": "PR package built. Awaiting human review via `ta pr view` / `ta pr approve`.",
        });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    #[tool(description = "Check the review status of a goal's PR package.")]
    fn ta_pr_status(
        &self,
        Parameters(params): Parameters<GoalIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
        let goal_run_id = parse_uuid(&params.goal_run_id)?;

        let goal = state
            .goal_store
            .get(goal_run_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
            })?;

        let pr_status = if let Some(pkg_id) = goal.pr_package_id {
            if let Some(pkg) = state.pr_packages.get(&pkg_id) {
                serde_json::json!({
                    "pr_package_id": pkg_id.to_string(),
                    "status": format!("{:?}", pkg.status),
                    "artifacts": pkg.changes.artifacts.len(),
                })
            } else {
                serde_json::json!({
                    "pr_package_id": pkg_id.to_string(),
                    "status": "unknown",
                })
            }
        } else {
            serde_json::json!({
                "status": "no_pr_package",
                "message": "No PR package has been built yet. Use ta_pr_build first.",
            })
        };

        let response = serde_json::json!({
            "goal_run_id": goal_run_id.to_string(),
            "goal_state": goal.state.to_string(),
            "pr": pr_status,
        });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    }

    // ── Inner-loop tools (v0.4.1 — macro goals) ────────────

    #[tool(
        description = "Manage draft packages within a macro goal session. Actions: build (package changes), submit (send for human review — blocks until response), status (check review status), list (list drafts for this goal). Use during macro goal sessions for inner-loop iteration."
    )]
    fn ta_draft(
        &self,
        Parameters(params): Parameters<DraftToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

        match params.action.as_str() {
            "build" => {
                let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                    McpError::invalid_params("goal_run_id required for build", None)
                })?)?;
                let summary = params
                    .summary
                    .as_deref()
                    .unwrap_or("Changes from macro goal sub-task");

                let goal = state
                    .goal_store
                    .get(goal_run_id)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .ok_or_else(|| {
                        McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
                    })?;

                let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
                    McpError::invalid_params(
                        format!("no active connector for goal: {}", goal_run_id),
                        None,
                    )
                })?;

                // Use goal.objective for the "why" field (#76). Previously used
                // goal.title which just restated the summary.
                let summary_why = if goal.objective.is_empty() {
                    goal.title.clone()
                } else {
                    goal.objective.clone()
                };
                let pr_package = connector
                    .build_pr_package(&goal.title, &goal.objective, summary, &summary_why)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let package_id = pr_package.package_id;
                state
                    .save_pr_package(pr_package)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let response = serde_json::json!({
                    "draft_id": package_id.to_string(),
                    "goal_run_id": goal_run_id.to_string(),
                    "status": "built",
                    "message": "Draft built. Call ta_draft with action: 'submit' to send for review.",
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            "submit" => {
                let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                    McpError::invalid_params("goal_run_id required for submit", None)
                })?)?;

                let mut goal = state
                    .goal_store
                    .get(goal_run_id)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .ok_or_else(|| {
                        McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
                    })?;

                let goal_id_str = goal_run_id.to_string();
                let package_id = goal.pr_package_id.or_else(|| {
                    // Find the most recent package for this goal.
                    state
                        .pr_packages
                        .values()
                        .filter(|p| p.goal.goal_id == goal_id_str)
                        .max_by_key(|p| p.created_at)
                        .map(|p| p.package_id)
                });

                match package_id {
                    Some(pkg_id) => {
                        // Transition to PrReady to signal human review needed.
                        if goal.state == GoalRunState::Running {
                            goal.pr_package_id = Some(pkg_id);
                            goal.transition(GoalRunState::PrReady)
                                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                            state
                                .goal_store
                                .save(&goal)
                                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                        }

                        state.event_dispatcher.dispatch(&TaEvent::PrReady {
                            goal_run_id,
                            pr_package_id: pkg_id,
                            summary: "Macro goal draft submitted for review".to_string(),
                            timestamp: Utc::now(),
                        });

                        // Route through ReviewChannel for interactive review (v0.4.1.1).
                        let artifact_count = state
                            .pr_packages
                            .get(&pkg_id)
                            .map(|p| p.changes.artifacts.len())
                            .unwrap_or(0);
                        let interaction_req =
                            InteractionRequest::draft_review(pkg_id, &goal.title, artifact_count)
                                .with_goal_id(goal_run_id);

                        let review_result = state.request_review(&interaction_req);

                        let (review_status, review_decision) = match &review_result {
                            Ok(resp) => {
                                let decision_str = format!("{}", resp.decision);
                                if decision_str == "approved" {
                                    // Dispatch PrApproved event (v0.5.6).
                                    state.event_dispatcher.dispatch(&TaEvent::PrApproved {
                                        goal_run_id,
                                        pr_package_id: pkg_id,
                                        approved_by: "human".to_string(),
                                        timestamp: Utc::now(),
                                    });

                                    // If macro goal, transition back to Running.
                                    if goal.is_macro {
                                        if let Ok(Some(mut g)) = state.goal_store.get(goal_run_id) {
                                            if g.state == GoalRunState::PrReady {
                                                let _ = g.transition(GoalRunState::Running);
                                                let _ = state.goal_store.save(&g);
                                            }
                                        }
                                    }
                                } else {
                                    // Dispatch PrDenied event (v0.5.6).
                                    state.event_dispatcher.dispatch(&TaEvent::PrDenied {
                                        goal_run_id,
                                        pr_package_id: pkg_id,
                                        reason: resp
                                            .reasoning
                                            .clone()
                                            .unwrap_or_else(|| "denied".to_string()),
                                        denied_by: "human".to_string(),
                                        timestamp: Utc::now(),
                                    });

                                    // Auto-capture rejection in memory (v0.5.6).
                                    // Clone config to avoid borrow conflict on state.
                                    let reject_event = DraftRejectEvent {
                                        goal_id: goal_run_id,
                                        draft_id: pkg_id,
                                        agent_framework: goal.agent_id.clone(),
                                        attempted: goal.title.clone(),
                                        rejection_reason: resp
                                            .reasoning
                                            .clone()
                                            .unwrap_or_else(|| "denied".to_string()),
                                        phase_id: goal.plan_phase.clone(),
                                    };
                                    let capture =
                                        AutoCapture::new(state.auto_capture_config.clone());
                                    let _ = capture
                                        .on_draft_reject(&mut state.memory_store, &reject_event);
                                }
                                ("reviewed".to_string(), decision_str)
                            }
                            Err(_) => {
                                // Channel error — fall back to pending review.
                                ("submitted".to_string(), "pending".to_string())
                            }
                        };

                        let response = serde_json::json!({
                            "draft_id": pkg_id.to_string(),
                            "goal_run_id": goal_run_id.to_string(),
                            "status": review_status,
                            "decision": review_decision,
                            "message": if review_decision == "pending" {
                                "Draft submitted for human review. Use ta_draft with action: 'status' to check review progress."
                            } else {
                                "Draft reviewed through ReviewChannel."
                            },
                        });
                        Ok(CallToolResult::success(vec![Content::json(response)
                            .map_err(|e| {
                                McpError::internal_error(e.to_string(), None)
                            })?]))
                    }
                    None => {
                        let response = serde_json::json!({
                            "error": "no_draft",
                            "message": "No draft package found. Call ta_draft with action: 'build' first.",
                        });
                        Ok(CallToolResult::success(vec![Content::json(response)
                            .map_err(|e| {
                                McpError::internal_error(e.to_string(), None)
                            })?]))
                    }
                }
            }
            "status" => {
                let draft_id = parse_uuid(
                    params
                        .draft_id
                        .as_deref()
                        .or(params.goal_run_id.as_deref())
                        .ok_or_else(|| {
                            McpError::invalid_params(
                                "draft_id or goal_run_id required for status",
                                None,
                            )
                        })?,
                )?;

                // Try as draft ID first, then as goal ID.
                if let Some(pkg) = state.pr_packages.get(&draft_id) {
                    let response = serde_json::json!({
                        "draft_id": draft_id.to_string(),
                        "status": format!("{:?}", pkg.status),
                        "artifacts": pkg.changes.artifacts.len(),
                    });
                    Ok(CallToolResult::success(vec![Content::json(response)
                        .map_err(|e| {
                            McpError::internal_error(e.to_string(), None)
                        })?]))
                } else if let Ok(Some(goal)) = state.goal_store.get(draft_id) {
                    let pr_status = if let Some(pkg_id) = goal.pr_package_id {
                        if let Some(pkg) = state.pr_packages.get(&pkg_id) {
                            serde_json::json!({
                                "draft_id": pkg_id.to_string(),
                                "status": format!("{:?}", pkg.status),
                                "artifacts": pkg.changes.artifacts.len(),
                            })
                        } else {
                            serde_json::json!({ "status": "unknown" })
                        }
                    } else {
                        serde_json::json!({
                            "status": "no_draft",
                            "message": "No draft built yet.",
                        })
                    };
                    Ok(CallToolResult::success(vec![Content::json(pr_status)
                        .map_err(|e| {
                            McpError::internal_error(e.to_string(), None)
                        })?]))
                } else {
                    Err(McpError::invalid_params(
                        format!("not found: {}", draft_id),
                        None,
                    ))
                }
            }
            "list" => {
                let packages: Vec<serde_json::Value> = state
                    .pr_packages
                    .values()
                    .map(|pkg| {
                        serde_json::json!({
                            "draft_id": pkg.package_id.to_string(),
                            "status": format!("{:?}", pkg.status),
                            "artifacts": pkg.changes.artifacts.len(),
                            "goal_id": &pkg.goal.goal_id,
                        })
                    })
                    .collect();

                let response = serde_json::json!({ "drafts": packages, "count": packages.len() });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            _ => Err(McpError::invalid_params(
                format!(
                    "unknown action '{}'. Expected: build, submit, status, list",
                    params.action
                ),
                None,
            )),
        }
    }

    #[tool(
        description = "Manage sub-goals within a macro goal session. Actions: start (create a sub-goal that inherits the macro goal's context), status (check sub-goal progress). Sub-goals share the macro goal's workspace, plan phase, and agent config. For start: set launch:true to spawn the implementation agent in the background (headless mode). Use agent and phase params to override inherited values."
    )]
    fn ta_goal_inner(
        &self,
        Parameters(params): Parameters<GoalToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

        match params.action.as_str() {
            "start" => {
                let macro_goal_id =
                    parse_uuid(params.macro_goal_id.as_deref().ok_or_else(|| {
                        McpError::invalid_params("macro_goal_id required for start", None)
                    })?)?;
                let title = params
                    .title
                    .as_deref()
                    .ok_or_else(|| McpError::invalid_params("title required for start", None))?;
                let objective = params.objective.as_deref().unwrap_or(title);

                // Verify macro goal exists and is a macro goal.
                let macro_goal = state
                    .goal_store
                    .get(macro_goal_id)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            format!("macro goal not found: {}", macro_goal_id),
                            None,
                        )
                    })?;

                if !macro_goal.is_macro {
                    return Err(McpError::invalid_params(
                        "goal is not a macro goal. Use ta run --macro to start a macro session.",
                        None,
                    ));
                }

                // Create sub-goal inheriting from macro goal.
                let sub_goal_run = state
                    .start_goal(title, objective, &macro_goal.agent_id)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let sub_id = sub_goal_run.goal_run_id;

                // Set parent_macro_id on sub-goal and inherit plan phase.
                let mut updated_sub = state
                    .goal_store
                    .get(sub_id)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .ok_or_else(|| McpError::internal_error("sub-goal vanished", None))?;
                updated_sub.parent_macro_id = Some(macro_goal_id);
                // Phase: use explicit param, fall back to macro goal's phase.
                updated_sub.plan_phase = params
                    .phase
                    .clone()
                    .or_else(|| macro_goal.plan_phase.clone());
                updated_sub.source_dir = macro_goal.source_dir.clone();
                // Agent: use explicit param, fall back to macro goal's agent.
                let agent_id = params
                    .agent
                    .clone()
                    .unwrap_or_else(|| macro_goal.agent_id.clone());
                updated_sub.agent_id = agent_id.clone();
                state
                    .goal_store
                    .save(&updated_sub)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                // Add sub-goal ID to macro goal's list.
                let mut updated_macro = macro_goal.clone();
                updated_macro.sub_goal_ids.push(sub_id);
                state
                    .goal_store
                    .save(&updated_macro)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                // Optionally launch the implementation agent in the background (v0.8.2).
                let launched = if params.launch.unwrap_or(false) {
                    if let Some(ref source_dir) = macro_goal.source_dir {
                        let phase_arg = updated_sub.plan_phase.clone();
                        let source = source_dir.clone();
                        let sub_title = title.to_string();
                        let sub_objective = objective.to_string();

                        // Spawn `ta run --headless` as a background process.
                        let mut cmd = std::process::Command::new("ta");
                        cmd.arg("run")
                            .arg(&sub_title)
                            .arg("--source")
                            .arg(&source)
                            .arg("--agent")
                            .arg(&agent_id)
                            .arg("--objective")
                            .arg(&sub_objective)
                            .arg("--headless");

                        if let Some(ref phase) = phase_arg {
                            cmd.arg("--phase").arg(phase);
                        }

                        // Detach: redirect output to null, don't inherit stdin.
                        cmd.stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null());

                        match cmd.spawn() {
                            Ok(_child) => {
                                tracing::info!(
                                    "Launched headless agent for sub-goal {} ({})",
                                    sub_id,
                                    sub_title
                                );
                                true
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to launch agent for sub-goal {}: {}",
                                    sub_id,
                                    e
                                );
                                false
                            }
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                // Publish GoalStarted event to the file-based event store.
                {
                    use ta_events::{EventStore, FsEventStore};
                    let events_dir = state.config.workspace_root.join(".ta").join("events");
                    let event_store = FsEventStore::new(&events_dir);
                    let event = ta_events::SessionEvent::GoalStarted {
                        goal_id: sub_id,
                        title: title.to_string(),
                        agent_id: agent_id.clone(),
                        phase: updated_sub.plan_phase.clone(),
                    };
                    let envelope = ta_events::EventEnvelope::new(event);
                    if let Err(e) = event_store.append(&envelope) {
                        tracing::warn!("Failed to persist GoalStarted event: {}", e);
                    }
                }

                let response = serde_json::json!({
                    "sub_goal_id": sub_id.to_string(),
                    "macro_goal_id": macro_goal_id.to_string(),
                    "title": title,
                    "state": "running",
                    "launched": launched,
                    "agent": agent_id,
                    "phase": updated_sub.plan_phase,
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            "status" => {
                let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                    McpError::invalid_params("goal_run_id required for status", None)
                })?)?;

                let goal = state
                    .goal_store
                    .get(goal_run_id)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .ok_or_else(|| {
                        McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
                    })?;

                let mut response = serde_json::json!({
                    "goal_run_id": goal.goal_run_id.to_string(),
                    "title": goal.title,
                    "state": goal.state.to_string(),
                    "is_macro": goal.is_macro,
                });

                // Include sub-goal tree if this is a macro goal.
                if goal.is_macro && !goal.sub_goal_ids.is_empty() {
                    let sub_goals: Vec<serde_json::Value> = goal
                        .sub_goal_ids
                        .iter()
                        .filter_map(|id| state.goal_store.get(*id).ok().flatten())
                        .map(|sg| {
                            serde_json::json!({
                                "sub_goal_id": sg.goal_run_id.to_string(),
                                "title": sg.title,
                                "state": sg.state.to_string(),
                                "draft_id": sg.pr_package_id.map(|id| id.to_string()),
                            })
                        })
                        .collect();
                    response["sub_goals"] = serde_json::json!(sub_goals);
                }

                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            _ => Err(McpError::invalid_params(
                format!(
                    "unknown action '{}'. Expected: start, status",
                    params.action
                ),
                None,
            )),
        }
    }

    #[tool(
        description = "Read or propose updates to the project development plan. Actions: read (view current plan progress), update (propose a status note for a phase — held for human approval). Plan updates are governance-gated: agent proposes, human approves."
    )]
    fn ta_plan(
        &self,
        Parameters(params): Parameters<PlanToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

        match params.action.as_str() {
            "read" => {
                // Read plan progress from the goal's source directory.
                let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                    McpError::invalid_params("goal_run_id required for read", None)
                })?)?;

                let goal = state
                    .goal_store
                    .get(goal_run_id)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .ok_or_else(|| {
                        McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
                    })?;

                // Try to read PLAN.md from workspace.
                let plan_path = goal.workspace_path.join("PLAN.md");
                if plan_path.exists() {
                    let content = std::fs::read_to_string(&plan_path)
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                    Ok(CallToolResult::success(vec![Content::text(content)]))
                } else {
                    let response = serde_json::json!({
                        "message": "No PLAN.md found in workspace.",
                    });
                    Ok(CallToolResult::success(vec![Content::json(response)
                        .map_err(|e| {
                            McpError::internal_error(e.to_string(), None)
                        })?]))
                }
            }
            "update" => {
                // Propose a plan update — recorded as an event, held for human approval.
                let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                    McpError::invalid_params("goal_run_id required for update", None)
                })?)?;
                let phase = params.phase.as_deref().unwrap_or("unknown");
                let status_note = params
                    .status_note
                    .as_deref()
                    .unwrap_or("Agent proposes phase update");

                // Log the proposal as an event — human must approve via ta draft workflow.
                state
                    .event_dispatcher
                    .dispatch(&TaEvent::PlanUpdateProposed {
                        goal_run_id,
                        phase: phase.to_string(),
                        status_note: status_note.to_string(),
                        timestamp: Utc::now(),
                    });

                // Route through ReviewChannel for plan negotiation (v0.4.1.1).
                let interaction_req = InteractionRequest::plan_negotiation(phase, status_note)
                    .with_goal_id(goal_run_id);

                let review_result = state.request_review(&interaction_req);

                let (plan_status, plan_decision) = match &review_result {
                    Ok(resp) => {
                        let decision_str = format!("{}", resp.decision);
                        (
                            if decision_str == "approved" {
                                "approved"
                            } else {
                                "proposed"
                            },
                            decision_str,
                        )
                    }
                    Err(_) => ("proposed", "pending".to_string()),
                };

                let response = serde_json::json!({
                    "goal_run_id": goal_run_id.to_string(),
                    "phase": phase,
                    "status": plan_status,
                    "decision": plan_decision,
                    "message": if plan_decision == "pending" {
                        "Plan update proposed. Human must approve via `ta draft approve` before it takes effect."
                    } else {
                        "Plan update reviewed through ReviewChannel."
                    },
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            _ => Err(McpError::invalid_params(
                format!("unknown action '{}'. Expected: read, update", params.action),
                None,
            )),
        }
    }

    /// Persistent memory store — agents can store and recall context that
    /// persists across sessions and works across different agent frameworks.
    ///
    /// Actions:
    /// - "store": Save a memory entry (key + value + optional tags/source/goal_id/category).
    /// - "recall": Retrieve a single entry by exact key.
    /// - "list": List all entries (optionally limited).
    /// - "forget": Delete an entry by key.
    /// - "search": Semantic search by query text (v0.5.6).
    #[tool]
    fn ta_context(
        &self,
        Parameters(params): Parameters<ContextToolParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

        match params.action.as_str() {
            "store" => {
                let key = params
                    .key
                    .as_deref()
                    .ok_or_else(|| McpError::invalid_params("key required for store", None))?;
                let value = params.value.clone().unwrap_or(serde_json::Value::Null);
                let tags = params.tags.clone().unwrap_or_default();
                let source = params.source.as_deref().unwrap_or("agent");

                // Parse optional goal_id and category (v0.5.6).
                let goal_id = params
                    .goal_id
                    .as_deref()
                    .and_then(|s| s.parse::<Uuid>().ok());
                let category = params
                    .category
                    .as_deref()
                    .map(ta_memory::MemoryCategory::from_str_lossy);

                let store_params = ta_memory::StoreParams {
                    goal_id,
                    category,
                    ..Default::default()
                };
                let entry = state
                    .memory_store
                    .store_with_params(key, value, tags, source, store_params)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let response = serde_json::json!({
                    "status": "stored",
                    "entry_id": entry.entry_id.to_string(),
                    "key": entry.key,
                    "source": entry.source,
                    "category": entry.category.as_ref().map(|c| c.to_string()),
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            "recall" => {
                let key = params
                    .key
                    .as_deref()
                    .ok_or_else(|| McpError::invalid_params("key required for recall", None))?;

                let entry = state
                    .memory_store
                    .recall(key)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                match entry {
                    Some(e) => {
                        let response = serde_json::json!({
                            "key": e.key,
                            "value": e.value,
                            "tags": e.tags,
                            "source": e.source,
                            "category": e.category.as_ref().map(|c| c.to_string()),
                            "goal_id": e.goal_id.map(|id| id.to_string()),
                            "created_at": e.created_at.to_rfc3339(),
                            "updated_at": e.updated_at.to_rfc3339(),
                        });
                        Ok(CallToolResult::success(vec![Content::json(response)
                            .map_err(|e| {
                                McpError::internal_error(e.to_string(), None)
                            })?]))
                    }
                    None => {
                        let response = serde_json::json!({
                            "status": "not_found",
                            "key": key,
                        });
                        Ok(CallToolResult::success(vec![Content::json(response)
                            .map_err(|e| {
                                McpError::internal_error(e.to_string(), None)
                            })?]))
                    }
                }
            }
            "list" => {
                let entries = state
                    .memory_store
                    .list(params.limit)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let items: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "key": e.key,
                            "tags": e.tags,
                            "source": e.source,
                            "category": e.category.as_ref().map(|c| c.to_string()),
                            "updated_at": e.updated_at.to_rfc3339(),
                        })
                    })
                    .collect();

                let response = serde_json::json!({
                    "count": items.len(),
                    "entries": items,
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            "forget" => {
                let key = params
                    .key
                    .as_deref()
                    .ok_or_else(|| McpError::invalid_params("key required for forget", None))?;

                let existed = state
                    .memory_store
                    .forget(key)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let response = serde_json::json!({
                    "status": if existed { "forgotten" } else { "not_found" },
                    "key": key,
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            "search" => {
                let query = params
                    .query
                    .as_deref()
                    .or(params.key.as_deref())
                    .ok_or_else(|| {
                        McpError::invalid_params("query or key required for search", None)
                    })?;
                let k = params.limit.unwrap_or(5);

                let entries = state
                    .memory_store
                    .semantic_search(query, k)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let items: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "key": e.key,
                            "value": e.value,
                            "tags": e.tags,
                            "source": e.source,
                            "category": e.category.as_ref().map(|c| c.to_string()),
                        })
                    })
                    .collect();

                let response = serde_json::json!({
                    "count": items.len(),
                    "results": items,
                    "backend": if items.is_empty() { "no results (semantic search requires ruvector backend)" } else { "ok" },
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            "stats" => {
                let stats = state
                    .memory_store
                    .stats()
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let response = serde_json::json!({
                    "total_entries": stats.total_entries,
                    "by_category": stats.by_category,
                    "by_source": stats.by_source,
                    "expired_count": stats.expired_count,
                    "avg_confidence": stats.avg_confidence,
                    "oldest_entry": stats.oldest_entry.map(|t| t.to_rfc3339()),
                    "newest_entry": stats.newest_entry.map(|t| t.to_rfc3339()),
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            "similar" => {
                let entry_id = params
                    .key
                    .as_deref()
                    .ok_or_else(|| {
                        McpError::invalid_params("key (entry_id) required for similar", None)
                    })?;
                let uuid: uuid::Uuid = entry_id.parse().map_err(|_| {
                    McpError::invalid_params(
                        format!("invalid UUID '{}' for similar lookup", entry_id),
                        None,
                    )
                })?;
                let k = params.limit.unwrap_or(5);

                let entry = state
                    .memory_store
                    .find_by_id(uuid)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?
                    .ok_or_else(|| {
                        McpError::invalid_params(
                            format!("no entry found with ID '{}'", entry_id),
                            None,
                        )
                    })?;

                let query_text = match &entry.value {
                    serde_json::Value::String(s) => s.clone(),
                    v => v.to_string(),
                };

                let results = state
                    .memory_store
                    .semantic_search(&query_text, k + 1)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                let items: Vec<serde_json::Value> = results
                    .iter()
                    .filter(|e| e.entry_id != uuid)
                    .take(k)
                    .map(|e| {
                        serde_json::json!({
                            "key": e.key,
                            "value": e.value,
                            "tags": e.tags,
                            "source": e.source,
                            "category": e.category.as_ref().map(|c| c.to_string()),
                        })
                    })
                    .collect();

                let response = serde_json::json!({
                    "reference_key": entry.key,
                    "count": items.len(),
                    "similar": items,
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
            _ => Err(McpError::invalid_params(
                format!(
                    "unknown action '{}'. Expected: store, recall, list, forget, search, stats, similar",
                    params.action
                ),
                None,
            )),
        }
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

// ── Helpers ──────────────────────────────────────────────────────

/// Parse a UUID string, returning an MCP error on failure.
fn parse_uuid(s: &str) -> Result<Uuid, McpError> {
    Uuid::parse_str(s)
        .map_err(|e| McpError::invalid_params(format!("invalid UUID '{}': {}", s, e), None))
}

/// Enforce a policy decision, returning an MCP error if denied.
fn enforce_policy(decision: &PolicyDecision) -> Result<(), McpError> {
    match decision {
        PolicyDecision::Allow => Ok(()),
        PolicyDecision::Deny { reason } => Err(McpError::invalid_request(
            format!("Policy denied: {}", reason),
            None,
        )),
        PolicyDecision::RequireApproval { reason } => {
            // For now, RequireApproval is treated as allowed since the
            // CLI approval flow will handle the actual gating.
            tracing::info!("action requires approval: {}", reason);
            Ok(())
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
        // Create source files in the project root.
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

    /// Helper: start a goal and return its ID.
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
        // 12 tools: goal_start, goal_status, goal_list,
        //           fs_read, fs_write, fs_list, fs_diff,
        //           pr_build, pr_status,
        //           ta_draft, ta_goal_inner, ta_plan (v0.4.1 macro goal tools)
        //           ta_context (v0.5.4 memory store)
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(tools.len(), 13, "expected 13 tools, got: {:?}", names);
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

        // Policy engine should have a manifest for the agent.
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

        // Build PR through state methods (simulating the tool).
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

        // Events log should have at least one entry.
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
        // Write to goal 1.
        state
            .connectors
            .get_mut(&id1)
            .unwrap()
            .write_patch("g1.txt", b"goal 1")
            .unwrap();
        // Write to goal 2.
        state
            .connectors
            .get_mut(&id2)
            .unwrap()
            .write_patch("g2.txt", b"goal 2")
            .unwrap();

        // Each should only see its own files.
        let files1 = state.connectors.get(&id1).unwrap().list_staged().unwrap();
        let files2 = state.connectors.get(&id2).unwrap().list_staged().unwrap();
        assert_eq!(files1.len(), 1);
        assert_eq!(files2.len(), 1);
        assert!(files1.contains(&"g1.txt".to_string()));
        assert!(files2.contains(&"g2.txt".to_string()));
    }
}
