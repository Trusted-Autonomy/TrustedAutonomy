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

use ta_audit::{AttestationBackend, AuditLog, SoftwareAttestationBackend};
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

use ta_actions::RateLimiter;
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

/// Parameters for `ta_plan_status` (lazy plan checklist tool, v0.14.3.2).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlanStatusParams {
    /// Optional phase ID to use as the "current" anchor for windowed output.
    /// When omitted, the full plan is returned without windowing.
    #[serde(default)]
    pub phase: Option<String>,
    /// Number of completed phases to show individually before the current phase.
    /// Default: 5.
    #[serde(default)]
    pub done_window: Option<u8>,
    /// Number of pending phases to show individually after the current phase.
    /// Default: 5.
    #[serde(default)]
    pub pending_window: Option<u8>,
    /// Output format: "text" (default) or "json".
    #[serde(default)]
    pub format: Option<String>,
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

/// Parameters for `ta_external_action` (v0.13.4).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExternalActionParams {
    /// The action type to request (e.g., "email", "api_call", "social_post", "db_query").
    pub action_type: String,
    /// The action payload. Fields vary by action type — use the schema returned
    /// by querying the action type's registry entry.
    pub payload: serde_json::Value,
    /// The UUID of the goal run this action is associated with (optional).
    /// When provided, rate limits and pending actions are scoped to this goal.
    #[serde(default)]
    pub goal_run_id: Option<String>,
    /// Optional URI identifying the resource this action targets
    /// (e.g., "mailto://alice@example.com", "https://api.stripe.com/charges").
    #[serde(default)]
    pub target_uri: Option<String>,
    /// Dry-run mode: log the action but do not execute or capture for review.
    /// Use this to test workflow definitions without side effects.
    #[serde(default)]
    pub dry_run: bool,
    /// A business-metric budget guardrail check to run against this action
    /// before it may auto-execute (v0.17.5.3, reusing `ta_human_verify`'s
    /// v0.17.5.2 budget mechanism). Only consulted for `ActionPolicy::Auto`
    /// dispatch — absent for actions with no cost/quantity dimension.
    #[serde(default)]
    pub budget: Option<tools::human_verify::BudgetActionParams>,
    /// Symbolic connector id (e.g. `"github"`, `"slack-ops"`) declared in
    /// `.ta/connectors.toml` (v0.17.6.3). This — never a raw credential
    /// value — is the only credential-shaped thing this schema exposes to
    /// the calling agent/LLM. When the connector is `broker_mediated`, the
    /// gateway resolves and attaches the real secret itself, only to its
    /// own outbound call, and never returns it here.
    #[serde(default)]
    pub connector: Option<String>,
    /// Session token id minted for a broker-mediated `connector` (the
    /// `TA_SESSION_TOKEN_<name>` value the agent received in its own
    /// environment at spawn time — see `ta_runtime::apply_credentials_to_env`).
    /// Required, and independently re-validated against the credential
    /// vault, whenever `connector` names a `broker_mediated` entry.
    #[serde(default)]
    pub session_token: Option<String>,
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
    /// Rate limiter for external actions scoped to this project (v0.13.4).
    pub action_rate_limiter: RateLimiter,
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
            action_rate_limiter: RateLimiter::new(),
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
    /// v0.13.4: Rate limiter for external actions (ta_external_action).
    pub action_rate_limiter: RateLimiter,
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
    /// When set, this MCP server process was started for a chat-mode launch
    /// and every `ta_fs_*` call must target exactly this chat session.
    /// See `lock_to_chat_session`.
    pub chat_lock: Option<Uuid>,
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

        let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
        let wf = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);

        // Optionally attach Ed25519 attestation backend when enabled in workflow.toml.
        let audit_log = {
            let log = AuditLog::open(&config.audit_log)?;
            if wf.audit.attestation {
                let keys_dir = if wf.audit.keys_dir.starts_with('/') {
                    std::path::PathBuf::from(&wf.audit.keys_dir)
                } else {
                    config.workspace_root.join(&wf.audit.keys_dir)
                };
                match SoftwareAttestationBackend::load_or_generate(&keys_dir) {
                    Ok(backend) => {
                        tracing::info!(
                            keys_dir = ?keys_dir,
                            fingerprint = %backend.public_key_fingerprint(),
                            "Audit attestation enabled"
                        );
                        log.with_attestation(Box::new(backend))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to load attestation key — audit events will not be signed");
                        log
                    }
                }
            } else {
                log
            }
        };

        let mut event_dispatcher = EventDispatcher::new();
        event_dispatcher.add_sink(Box::new(LogSink::new(&config.events_log)));
        let memory_store = FsMemoryStore::new(config.workspace_root.join(".ta").join("memory"));

        let auto_capture_config = ta_memory::auto_capture::load_config(&workflow_toml);

        // v0.10.0: Load channel routing from .ta/config.yaml and build review
        // channel(s) via ChannelRegistry instead of hardcoding AutoApproveChannel.
        let review_channel = Self::build_review_channel(&config);

        // Any application implementing the Commit contract registers its own
        // verb here instead of requiring a core-crate edit to a hardcoded
        // array (v0.17.0.12.15). "publish" is Commit for the social endpoint.
        let mut policy_engine = PolicyEngine::new();
        policy_engine.register_commit_verb("publish"); // social
        policy_engine.register_commit_verb("apply_mutation"); // DB proxy

        Ok(Self {
            config,
            policy_engine,
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
            action_rate_limiter: RateLimiter::new(),
            caller_mode: CallerMode::from_env(),
            dev_session_id: std::env::var("TA_DEV_SESSION_ID").ok(),
            active_agents: HashMap::new(),
            projects: HashMap::new(),
            active_project: None,
            chat_lock: None,
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
        // Guard against caller-supplied agent_id containing the reserved ":chat:"
        // marker. This prevents deliberately colliding with a live chat session's
        // derived policy identity and overwriting its narrow manifest.
        if agent_id.contains(":chat:") {
            return Err(GatewayError::Other(
                format!(
                    "agent_id '{}' is not permitted to contain ':chat:' - that substring is reserved for chat-session-derived policy identities and using it here could overwrite a live chat session's manifest",
                    agent_id
                )
            ));
        }

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
        // Guard against caller-supplied agent_id containing the reserved ":chat:"
        // marker. This prevents deliberately colliding with a live chat session's
        // derived policy identity and overwriting its narrow manifest.
        if agent_id.contains(":chat:") {
            return Err(GatewayError::Other(
                format!(
                    "agent_id '{}' is not permitted to contain ':chat:' - that substring is reserved for chat-session-derived policy identities and using it here could overwrite a live chat session's manifest",
                    agent_id
                )
            ));
        }

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

    /// Start a chat-mode session: a `GoalRun` whose capability manifest
    /// comes from `ta_policy::compile_chat_manifest()` (broad `fs_read`,
    /// `fs_write_patch` scoped only to `ta_policy::CHAT_SCRATCH_DIR`)
    /// instead of a developer profile. This deliberately reuses the exact
    /// same `GoalRun`/`StagingWorkspace`/`FsConnector` machinery as
    /// `start_goal`/`start_goal_with_profile` so that `tools/fs.rs`'s
    /// already-tested, already-enforced `ta_fs_read`/`ta_fs_write`/
    /// `ta_fs_diff`/`ta_fs_list` handlers work against a chat session's
    /// `goal_run_id` with zero changes to that file.
    ///
    /// A chat session's `GoalRun` is transitioned to `Running` and left
    /// there permanently: no draft/PR lifecycle (`PrReady`/`Approved`/
    /// `Applied`) is ever invoked against it, since chat mode's intended
    /// tool surface never includes `ta_pr_build`. This is a deliberate
    /// design choice (see
    /// `docs/superpowers/specs/2026-10-05-chat-mode-secure-launch-design.md`),
    /// not a bug or an unfinished state machine.
    ///
    /// The caller-supplied `agent_id` is NOT used directly as the policy
    /// identity (the manifest map key, and the value stored on the
    /// `GoalRun`). `PolicyEngine::load_manifest()` keys manifests by a
    /// plain `agent_id` string in a shared `HashMap`, so if a later
    /// `start_goal`/`start_goal_with_profile` call reuses the same raw
    /// `agent_id` (e.g. a poller reusing a stable id like `"cos"` for its
    /// orchestrator), that call's broad developer-profile manifest would
    /// silently overwrite this session's narrow chat manifest in the map,
    /// and this still-live session's `goal_run_id` would resolve back to
    /// the same `agent_id` via `agent_for_goal()`, inheriting full write
    /// access. To prevent that collision, each chat session gets its own
    /// derived policy identity (`agent_id` plus the session's own unique
    /// `goal_run_id`), used everywhere a policy key is needed: the
    /// manifest compiled for it, and the `agent_id` stored on its
    /// `GoalRun`. No other `start_goal*` call can ever produce the same
    /// derived identity, so it can never be overwritten by one.
    pub fn start_chat_session(
        &mut self,
        agent_id: &str,
        resource_scope: &str,
        validity_hours: i64,
    ) -> Result<GoalRun, GatewayError> {
        self.start_chat_session_with_id(agent_id, Uuid::new_v4(), resource_scope, validity_hours)
    }

    /// `start_chat_session` with a caller-chosen session id. Used by the
    /// `ta run` chat-mode launch path, which pre-generates the id so it can
    /// tell the agent which `goal_run_id` to pass to the `ta_fs_*` tools
    /// before the agent's MCP server process even starts.
    ///
    /// Rejects (H6) an `agent_id` containing the reserved `:chat:` marker,
    /// and refuses to reuse a `session_id` that already belongs to a
    /// different goal/session in the goal store, so this can never
    /// overwrite a real goal's record or another session's identity.
    pub fn start_chat_session_with_id(
        &mut self,
        agent_id: &str,
        goal_run_id: Uuid,
        resource_scope: &str,
        validity_hours: i64,
    ) -> Result<GoalRun, GatewayError> {
        ta_goal::chat_mode::validate_chat_agent_id(agent_id)
            .map_err(|msg| GatewayError::Other(format!("cannot start chat session: {}", msg)))?;
        let expected_policy_id = format!(
            "{}{}{}",
            agent_id,
            ta_goal::chat_mode::CHAT_POLICY_ID_MARKER,
            goal_run_id
        );
        if let Some(existing) = self.goal_store.get(goal_run_id)? {
            if existing.agent_id != expected_policy_id {
                return Err(GatewayError::Other(format!(
                    "cannot start chat session {}: that id already belongs to goal '{}' \
                     (agent '{}') in {}. Chat sessions never reuse another goal's id. \
                     Re-launch without a fixed session id so a fresh one is generated.",
                    goal_run_id,
                    existing.title,
                    existing.agent_id,
                    self.config.goals_dir.display()
                )));
            }
        }
        // Each chat session gets its own policy identity, distinct from the
        // caller-supplied agent_id, so a later start_goal/start_goal_with_profile
        // call reusing the same agent_id (e.g. a poller's stable "cos" id)
        // cannot silently overwrite this session's manifest in the shared
        // agent_id -> manifest map and widen a live chat session's access.
        let policy_agent_id = format!("{}:chat:{}", agent_id, goal_run_id);
        let staging_path = self.config.staging_dir.join(goal_run_id.to_string());
        let store_path = self.config.store_dir.join(goal_run_id.to_string());

        let mut goal_run = GoalRun::new(
            "chat session",
            "answer a chat-mode question using project context",
            &policy_agent_id,
            staging_path,
            store_path,
        );
        goal_run.goal_run_id = goal_run_id;
        // H9: anything a chat session produces is chat-originated and is
        // never auto-approved.
        goal_run.origin = Some(ta_goal::origin::CHAT_ORIGIN.to_string());

        // Unlike compile_with_id, compile_chat_manifest generates its own
        // manifest_id internally: keep GoalRun's own manifest_id field
        // consistent with what's actually loaded, rather than leaving it
        // at the placeholder value GoalRun::new() assigned.
        let manifest =
            ta_policy::compile_chat_manifest(&policy_agent_id, resource_scope, validity_hours)
                .map_err(|e| {
                    let msg = format!(
                "chat manifest compilation failed for agent '{}' with resource scope '{}': {}",
                agent_id, resource_scope, e
            );
                    GatewayError::Other(msg)
                })?;
        goal_run.manifest_id = manifest.manifest_id;
        self.policy_engine.load_manifest(manifest);

        let staging = StagingWorkspace::new(goal_run_id.to_string(), &self.config.staging_dir)?;
        let store = JsonFileStore::new(self.config.store_dir.join(goal_run_id.to_string()))?;
        let connector = FsConnector::new(goal_run_id.to_string(), staging, store, &policy_agent_id);
        self.connectors.insert(goal_run_id, connector);

        goal_run.transition(GoalRunState::Configured)?;
        goal_run.transition(GoalRunState::Running)?;
        self.goal_store.save(&goal_run)?;

        self.event_dispatcher.dispatch(&TaEvent::goal_created(
            goal_run_id,
            &goal_run.title,
            &policy_agent_id,
        ));

        Ok(goal_run)
    }

    /// Start the chat session described by `launch` and lock this gateway
    /// to it: from then on every `ta_fs_*` call must name exactly this
    /// session's `goal_run_id` (see `check_chat_lock`), so a chat-mode agent
    /// cannot reach any other goal's broader manifest through this server.
    pub fn lock_to_chat_session(
        &mut self,
        launch: &crate::chat_launch::ChatLaunch,
    ) -> Result<GoalRun, GatewayError> {
        if let Some(existing) = self.chat_lock {
            return Err(GatewayError::Other(format!(
                "this TA MCP server is already locked to chat session {}; a server is locked \
                 to exactly one chat session for its lifetime",
                existing
            )));
        }
        let goal = self.start_chat_session_with_id(
            &launch.agent_id,
            launch.session_id,
            crate::chat_launch::CHAT_RESOURCE_SCOPE,
            crate::chat_launch::CHAT_VALIDITY_HOURS,
        )?;
        self.chat_lock = Some(goal.goal_run_id);
        tracing::info!(
            session_id = %goal.goal_run_id,
            agent_id = %launch.agent_id,
            policy_agent_id = %goal.agent_id,
            workspace_root = %self.config.workspace_root.display(),
            "TA MCP server locked to chat-mode session (read-only manifest, scratch-only writes)"
        );
        Ok(goal)
    }

    /// When this server is chat-locked, reject any `goal_run_id` other than
    /// the locked chat session's. No-op for a normal (unlocked) server.
    pub fn check_chat_lock(&self, goal_run_id: Uuid) -> Result<(), GatewayError> {
        match self.chat_lock {
            Some(locked) if locked != goal_run_id => Err(GatewayError::PolicyDenied(format!(
                "this TA MCP server runs in chat mode and is locked to chat session {}; \
                 goal_run_id {} was rejected. Pass goal_run_id \"{}\" to the ta_fs_* tools. \
                 Chat mode can read the workspace and write only under {}/.",
                locked,
                goal_run_id,
                locked,
                ta_policy::CHAT_SCRATCH_DIR
            ))),
            _ => Ok(()),
        }
    }

    /// Check policy for a filesystem operation.
    pub fn check_policy(
        &self,
        agent_id: &str,
        verb: &str,
        path: &str,
    ) -> Result<PolicyDecision, GatewayError> {
        // H5: `format!("fs://workspace/{}", path)` with an absolute `path`
        // yields `fs://workspace//abs/...`, which still matches a
        // `fs://workspace/**` grant. Reject anything that is not a plain
        // workspace-relative path (absolute in POSIX or Windows form on any
        // host, `~`, or with a `..` component) before building the URI, so
        // no grant can ever be matched by an out-of-workspace target.
        if let Err(reason) = ta_workspace::path_safety::validate_relative_path(path) {
            tracing::warn!(
                agent_id = %agent_id,
                verb = %verb,
                path = %path,
                reason = %reason,
                "fs policy check rejected a non-workspace-relative path"
            );
            return Ok(PolicyDecision::Deny {
                reason: format!(
                    "path '{}' is not a workspace-relative path ({}). Pass a path relative \
                     to the workspace root, e.g. 'src/main.rs'.",
                    path, reason
                ),
            });
        }
        let request = PolicyRequest {
            agent_id: agent_id.to_string(),
            tool: "fs".to_string(),
            verb: verb.to_string(),
            target_uri: format!("fs://workspace/{}", normalize_workspace_relative(path)),
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

    /// Resolve the current goal run via `TA_AGENT_ID` and `active_agents`,
    /// the same generic resolution `resolve_agent_id` already provides one
    /// step of. Returns `None` when there's no matching active agent
    /// session (a dev/manual call, or a caller mode with no goal context).
    pub fn resolve_current_goal_run_id(&self) -> Option<Uuid> {
        let agent_id = self.resolve_agent_id();
        self.active_agents.get(&agent_id)?.goal_run_id
    }

    /// Look up the current goal's resolved experiment-override value for
    /// `key`, if any. Generic: this method (and every caller of it) never
    /// hardcodes what a key means, only that some tool handler decided to
    /// check one.
    pub fn experiment_override(&self, key: &str) -> Option<serde_json::Value> {
        let goal_run_id = self.resolve_current_goal_run_id()?;
        let goal = self.goal_store.get(goal_run_id).ok()??;
        let overrides = goal.experiment_overrides?;
        overrides.get(key).cloned()
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

    /// Build an MCP server for a chat-mode launch: the gateway is locked to
    /// one chat session whose manifest is the compiled chat manifest, and
    /// every tool outside `ta_goal::chat_mode::CHAT_MODE_MCP_TOOLS` is
    /// removed from the router, so it is neither listed to the agent nor
    /// callable, independently of whatever the agent harness's own settings
    /// allow.
    pub fn new_chat_mode(
        config: GatewayConfig,
        launch: &crate::chat_launch::ChatLaunch,
    ) -> Result<Self, GatewayError> {
        let mut state = GatewayState::new(config)?;
        state.lock_to_chat_session(launch)?;
        let mut tool_router = Self::tool_router();
        let removed: Vec<String> = tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .filter(|name| !ta_goal::chat_mode::is_chat_mode_mcp_tool(name))
            .collect();
        for name in &removed {
            tool_router.remove_route(name);
        }
        // The Chief-of-Staff's outcome vocabulary is reply/delegate/done:
        // advertise exactly that (the handler also enforces it).
        if let Some(route) = tool_router.map.get_mut("ta_whiteboard_outcome_send") {
            tools::whiteboard::advertise_chat_outcome_vocabulary(&mut route.attr);
        }
        tracing::info!(
            session_id = %launch.session_id,
            removed_tools = removed.len(),
            remaining_tools = tool_router.list_all().len(),
            "chat mode: removed every non-chat-mode tool from the TA MCP server's tool router"
        );
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            tool_router,
        })
    }

    /// Names of the tools this server exposes (and will dispatch).
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
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

    #[tool(
        description = "Return the windowed plan checklist on demand (v0.14.3.2). \
            Provides the same output as the injected plan context, but fetched lazily \
            so agents in mcp or hybrid context_mode can retrieve plan state without it \
            being pre-loaded into CLAUDE.md. Parameters: phase (optional anchor phase ID), \
            done_window (default 5), pending_window (default 5), format (\"text\" | \"json\")."
    )]
    fn ta_plan_status(
        &self,
        Parameters(params): Parameters<PlanStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_plan_status", None, None);
        tools::plan::handle_plan_status(&self.state, params)
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

    // ── Interactive tools (v0.9.9.1, superseded by ta_human_verify in v0.17.0.12.26) ──

    #[tool(
        description = "Deprecated: use ta_human_verify instead (identical parameters, now confidence-gated). Kept registered so existing agent prompts/docs referencing this name keep working unchanged."
    )]
    fn ta_ask_human(
        &self,
        Parameters(params): Parameters<tools::human::AskHumanParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_ask_human", None, None);
        tools::human_verify::handle_ask_human_deprecated(&self.state, params)
    }

    #[tool(
        description = "Verify/ask the human a question through TA's two-stage confidence-gated pipeline (v0.17.0.12.26). A synthetic opinion pass (a headless agent answers like a human reviewer would) and an independent validator pass (critiques that reasoning) feed the shared ta_decision gate against per-workload thresholds; only a high-confidence/low-risk pair auto-confirms, fully documented in .ta/human-verify-audit.jsonl. Anything uncertain, high-risk, or from a non-'auto' security tier falls through to a real blocking human question exactly like the deprecated ta_ask_human. Your execution pauses only on that fallback path, until the human responds or the timeout expires."
    )]
    fn ta_human_verify(
        &self,
        Parameters(params): Parameters<tools::human_verify::HumanVerifyParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_human_verify", None, None);
        tools::human_verify::handle_human_verify(&self.state, params)
    }

    // ── Daemon-hosted whiteboard tools (v0.17.11.8) ───────────────────────
    //
    // Live team-session coordination tools, backed by the daemon's HTTP
    // whiteboard API (`crate::daemon_client::WhiteboardDaemonClient`).
    // Authenticate via `.ta/whiteboard-session.json` in this goal's staging
    // workspace (written by `ta run --team-session-id`), never via an
    // LLM-supplied token/team_session argument — see
    // `tools::whiteboard`'s module doc for the full rationale.

    #[tool(
        description = "Publish (or refresh) this agent's presence on the team-session whiteboard: what it's doing right now, for other roles/agents to see. Only available for a goal launched as part of a team session with whiteboard coordination enabled."
    )]
    fn ta_whiteboard_presence_register(
        &self,
        Parameters(params): Parameters<tools::whiteboard::PresenceRegisterParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_whiteboard_presence_register", None, None);
        tools::whiteboard::handle_presence_register(&self.state, params)
    }

    #[tool(
        description = "List the current presence records for this team session — which agents/roles are active and what they're working on. Only available for a goal launched as part of a team session with whiteboard coordination enabled."
    )]
    fn ta_whiteboard_presence_list(
        &self,
        Parameters(params): Parameters<tools::whiteboard::PresenceListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_whiteboard_presence_list", None, None);
        tools::whiteboard::handle_presence_list(&self.state, params)
    }

    #[tool(
        description = "Send a handoff payload to another role or agent in this team session. Only available for a goal launched as part of a team session with whiteboard coordination enabled."
    )]
    fn ta_whiteboard_handoff_send(
        &self,
        Parameters(params): Parameters<tools::whiteboard::HandoffSendParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_whiteboard_handoff_send", None, None);
        tools::whiteboard::handle_handoff_send(&self.state, params)
    }

    #[tool(
        description = "Check for a pending handoff addressed to this role or agent in this team session. Returns null if none is pending. Only available for a goal launched as part of a team session with whiteboard coordination enabled."
    )]
    fn ta_whiteboard_handoff_receive(
        &self,
        Parameters(params): Parameters<tools::whiteboard::HandoffReceiveParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_whiteboard_handoff_receive", None, None);
        tools::whiteboard::handle_handoff_receive(&self.state, params)
    }

    #[tool(
        description = "Attempt to claim a task on the team-session whiteboard — returns whether the claim succeeded (false if another agent already claimed it). Only available for a goal launched as part of a team session with whiteboard coordination enabled."
    )]
    fn ta_whiteboard_task_claim(
        &self,
        Parameters(params): Parameters<tools::whiteboard::TaskClaimParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_whiteboard_task_claim", None, None);
        tools::whiteboard::handle_task_claim(&self.state, params)
    }

    #[tool(
        description = "Mark a previously claimed task complete on the team-session whiteboard. Only available for a goal launched as part of a team session with whiteboard coordination enabled."
    )]
    fn ta_whiteboard_task_complete(
        &self,
        Parameters(params): Parameters<tools::whiteboard::TaskCompleteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_whiteboard_task_complete", None, None);
        tools::whiteboard::handle_task_complete(&self.state, params)
    }

    #[tool(
        description = "Report an outcome (done/blocked/new_work; a chat-mode server accepts only reply/delegate/done) for Wayfinder-sourced work back onto the report-back stream, so the Wayfinder poller can PATCH/POST it back to Wayfinder's task API. Only available for a goal launched as part of a team session with whiteboard coordination enabled."
    )]
    fn ta_whiteboard_outcome_send(
        &self,
        Parameters(params): Parameters<tools::whiteboard::OutcomeSendParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_whiteboard_outcome_send", None, None);
        tools::whiteboard::handle_outcome_send(&self.state, params)
    }

    // ── Wayfinder wiki tools (v0.17.11.15) ────────────────────────────
    //
    // Read/write Wayfinder's org/project wiki via a gateway-held
    // credential: the agent never talks to Wayfinder directly. See
    // `tools::wiki`'s module doc for the full rationale and
    // `docs/superpowers/specs/2026-09-15-virtual-team-wiki-retrieval-design.md`
    // (`ta-virtual-team` repo) for the design.

    #[tool(
        description = "Search Wayfinder's org or project wiki for pages matching a query. Always live (never cached); ranking will improve transparently over time. scope is \"project\" or \"org\"; id is that scope's Wayfinder id."
    )]
    fn ta_wiki_search(
        &self,
        Parameters(params): Parameters<tools::wiki::WikiSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_wiki_search", None, None);
        if let Ok(state) = self.state.lock() {
            if state
                .experiment_override("wiki.disabled")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                return Ok(CallToolResult::success(vec![Content::text(
                    r#"{"disabled": true, "reason": "wiki access disabled for this cost experiment"}"#,
                )]));
            }
        }
        tools::wiki::handle_wiki_search(&self.state, params)
    }

    #[tool(
        description = "Fetch one wiki page by id (scope: \"project\" or \"org\", plus that scope's id). Cache-first: served from the local cache when present, which is deliberately stale-tolerant (freshness depends on the periodic background sync, not a check on every call)."
    )]
    fn ta_wiki_get(
        &self,
        Parameters(params): Parameters<tools::wiki::WikiGetParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_wiki_get", None, None);
        if let Ok(state) = self.state.lock() {
            if state
                .experiment_override("wiki.disabled")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                return Ok(CallToolResult::success(vec![Content::text(
                    r#"{"disabled": true, "reason": "wiki access disabled for this cost experiment"}"#,
                )]));
            }
        }
        tools::wiki::handle_wiki_get(&self.state, params)
    }

    #[tool(
        description = "List the current wiki type taxonomy for a scope (\"project\" or \"org\"): call before ta_wiki_create/ta_wiki_update to reuse an existing type rather than minting a near-duplicate. Always live, never cached."
    )]
    fn ta_wiki_types(
        &self,
        Parameters(params): Parameters<tools::wiki::WikiTypesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_wiki_types", None, None);
        if let Ok(state) = self.state.lock() {
            if state
                .experiment_override("wiki.disabled")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                return Ok(CallToolResult::success(vec![Content::text(
                    r#"{"disabled": true, "reason": "wiki access disabled for this cost experiment"}"#,
                )]));
            }
        }
        tools::wiki::handle_wiki_types(&self.state, params)
    }

    #[tool(
        description = "Create a new wiki page in the given scope (\"project\" or \"org\"). Uses an elevated, write-specific credential distinct from the one ta_wiki_search/ta_wiki_get use."
    )]
    fn ta_wiki_create(
        &self,
        Parameters(params): Parameters<tools::wiki::WikiCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_wiki_create", None, None);
        if let Ok(state) = self.state.lock() {
            if state
                .experiment_override("wiki.disabled")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                return Ok(CallToolResult::success(vec![Content::text(
                    r#"{"disabled": true, "reason": "wiki access disabled for this cost experiment"}"#,
                )]));
            }
        }
        tools::wiki::handle_wiki_create(&self.state, params)
    }

    #[tool(
        description = "Update an existing wiki page. Pass if_sha (the sha last read) to reject the write on a conflicting concurrent edit instead of silently overwriting it; omit it to accept last-write-wins."
    )]
    fn ta_wiki_update(
        &self,
        Parameters(params): Parameters<tools::wiki::WikiUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ta_wiki_update", None, None);
        if let Ok(state) = self.state.lock() {
            if state
                .experiment_override("wiki.disabled")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                return Ok(CallToolResult::success(vec![Content::text(
                    r#"{"disabled": true, "reason": "wiki access disabled for this cost experiment"}"#,
                )]));
            }
        }
        tools::wiki::handle_wiki_update(&self.state, params)
    }

    // ── Unreal Engine 5 tools (v0.14.14) ─────────────────────────────

    #[tool(
        description = "Execute a Python script in the UE5 Editor context via the active backend (kvick/flopperam/special-agent). Gated behind `unreal://script/**` capability. Returns connector_not_running when the Editor is not open."
    )]
    fn ue5_python_exec(
        &self,
        Parameters(params): Parameters<tools::unreal::Ue5PythonExecParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("ue5_python_exec", Some("unreal://script/python_exec"), None);
        tools::unreal::handle_ue5_python_exec(&self.state, params)
    }

    #[tool(
        description = "Query actors and metadata from an Unreal Engine level. Returns actor list, transform data, and scene metadata. Gated behind `unreal://scene/**` capability."
    )]
    fn ue5_scene_query(
        &self,
        Parameters(params): Parameters<tools::unreal::Ue5SceneQueryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ue5_scene_query",
            Some(&format!(
                "unreal://scene/{}",
                params.level_path.trim_start_matches('/')
            )),
            None,
        );
        tools::unreal::handle_ue5_scene_query(&self.state, params)
    }

    #[tool(
        description = "List assets under a Content Browser path in an Unreal Engine project. Gated behind `unreal://assets/**` capability."
    )]
    fn ue5_asset_list(
        &self,
        Parameters(params): Parameters<tools::unreal::Ue5AssetListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ue5_asset_list",
            Some(&format!(
                "unreal://assets/{}",
                params.path.trim_start_matches('/')
            )),
            None,
        );
        tools::unreal::handle_ue5_asset_list(&self.state, params)
    }

    #[tool(
        description = "Submit a Movie Render Queue (MRQ) render job in Unreal Engine. Requires `unreal://render/**` capability grant (human approval gated). Returns a job_id for polling with ue5_mrq_status."
    )]
    fn ue5_mrq_submit(
        &self,
        Parameters(params): Parameters<tools::unreal::Ue5MrqSubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ue5_mrq_submit",
            Some(&format!(
                "unreal://render/{}",
                params.sequence_path.trim_start_matches('/')
            )),
            None,
        );
        tools::unreal::handle_ue5_mrq_submit(&self.state, params)
    }

    #[tool(
        description = "Poll the status of an MRQ render job submitted via ue5_mrq_submit. Returns job state (queued/running/complete/failed) and frame progress."
    )]
    fn ue5_mrq_status(
        &self,
        Parameters(params): Parameters<tools::unreal::Ue5MrqStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ue5_mrq_status",
            Some(&format!("unreal://render/status/{}", params.job_id)),
            None,
        );
        tools::unreal::handle_ue5_mrq_status(&self.state, params)
    }

    #[tool(
        description = "Query Level Sequences available in a UE5 level. Returns sequence names, content-browser paths, and frame ranges. Requires `unreal://scene/**` capability grant."
    )]
    fn ue5_sequencer_query(
        &self,
        Parameters(params): Parameters<tools::unreal::Ue5SequencerQueryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ue5_sequencer_query",
            Some(&format!(
                "unreal://scene/{}",
                params.level_path.trim_start_matches('/')
            )),
            None,
        );
        tools::unreal::handle_ue5_sequencer_query(&self.state, params)
    }

    #[tool(
        description = "List available lighting presets in a UE5 level (time-of-day, HDRI, static). Used to select a tod_preset before calling ue5_mrq_submit. Requires `unreal://scene/**` capability grant."
    )]
    fn ue5_lighting_preset_list(
        &self,
        Parameters(params): Parameters<tools::unreal::Ue5LightingPresetListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ue5_lighting_preset_list",
            Some(&format!(
                "unreal://scene/{}",
                params.level_path.trim_start_matches('/')
            )),
            None,
        );
        tools::unreal::handle_ue5_lighting_preset_list(&self.state, params)
    }

    // ── ComfyUI Inference Connector (v0.15.2) ─────────────────────

    #[tool(
        description = "Submit a ComfyUI workflow for inference (e.g., Wan2.1 video-to-video). Gated behind `comfyui://workflow/**` capability. Returns a job_id to poll with comfyui_job_status."
    )]
    fn comfyui_workflow_submit(
        &self,
        Parameters(params): Parameters<tools::comfyui::ComfyUiWorkflowSubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "comfyui_workflow_submit",
            Some("comfyui://workflow/submit"),
            None,
        );
        tools::comfyui::handle_comfyui_workflow_submit(&self.state, params)
    }

    #[tool(
        description = "Poll the status of a ComfyUI inference job. Returns job state (queued/running/complete/failed) and output file paths."
    )]
    fn comfyui_job_status(
        &self,
        Parameters(params): Parameters<tools::comfyui::ComfyUiJobStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "comfyui_job_status",
            Some(&format!("comfyui://workflow/status/{}", params.job_id)),
            None,
        );
        tools::comfyui::handle_comfyui_job_status(&self.state, params)
    }

    #[tool(
        description = "Cancel a queued or running ComfyUI inference job. Requires `comfyui://workflow/**` capability."
    )]
    fn comfyui_job_cancel(
        &self,
        Parameters(params): Parameters<tools::comfyui::ComfyUiJobCancelParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "comfyui_job_cancel",
            Some(&format!("comfyui://workflow/cancel/{}", params.job_id)),
            None,
        );
        tools::comfyui::handle_comfyui_job_cancel(&self.state, params)
    }

    #[tool(
        description = "List models available in the connected ComfyUI instance (checkpoints, LoRAs, VAEs). Gated behind `comfyui://model/**` capability."
    )]
    fn comfyui_model_list(
        &self,
        Parameters(params): Parameters<tools::comfyui::ComfyUiModelListParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("comfyui_model_list", Some("comfyui://model/list"), None);
        tools::comfyui::handle_comfyui_model_list(&self.state, params)
    }

    // ── Community Knowledge Hub tools (v0.17.0.12.4) ──────────────────────────
    // Routes to the `ta-community-hub` plugin binary via its JSON-over-stdio
    // protocol. Returns `not_configured` if the binary isn't installed.

    #[tool(
        description = "Search across configured community knowledge resources by query, optionally filtered by intent or resource name. Returns `not_configured` if `ta-community-hub` is not installed."
    )]
    fn community_search(
        &self,
        Parameters(params): Parameters<tools::community::CommunitySearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("community_search", None, None);
        tools::community::handle_community_search(&self.state, params)
    }

    #[tool(
        description = "Fetch a community knowledge document by ID (`<resource-name>/<path>`). Returns `not_configured` if `ta-community-hub` is not installed."
    )]
    fn community_get(
        &self,
        Parameters(params): Parameters<tools::community::CommunityGetParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("community_get", None, None);
        tools::community::handle_community_get(&self.state, params)
    }

    #[tool(
        description = "Stage a gap annotation on a community knowledge document for human review. Requires the resource to be configured read-write. Returns `not_configured` if `ta-community-hub` is not installed."
    )]
    fn community_annotate(
        &self,
        Parameters(params): Parameters<tools::community::CommunityAnnotateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("community_annotate", None, None);
        tools::community::handle_community_annotate(&self.state, params)
    }

    #[tool(
        description = "Stage an upvote/downvote quality rating on a community knowledge document for batched upstream submission. Returns `not_configured` if `ta-community-hub` is not installed."
    )]
    fn community_feedback(
        &self,
        Parameters(params): Parameters<tools::community::CommunityFeedbackParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("community_feedback", None, None);
        tools::community::handle_community_feedback(&self.state, params)
    }

    #[tool(
        description = "Stage a new document proposal under a community knowledge resource for human review. Requires the resource to be configured read-write. Returns `not_configured` if `ta-community-hub` is not installed."
    )]
    fn community_suggest(
        &self,
        Parameters(params): Parameters<tools::community::CommunitySuggestParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("community_suggest", None, None);
        tools::community::handle_community_suggest(&self.state, params)
    }

    // ── Unity Engine tools (v0.15.3) ─────────────────────────────────────────

    #[tool(description = "Trigger a Unity Player or AssetBundle build. \
        Specify the build target (e.g. \"StandaloneOSX\", \"StandaloneWindows64\", \"WebGL\", \"AssetBundle\") \
        and optional config (\"Debug\" or \"Release\"). \
        Gated behind `unity://build/**` capability. Returns build success status and output path.")]
    fn unity_build_trigger(
        &self,
        Parameters(params): Parameters<tools::unity::UnityBuildTriggerParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "unity_build_trigger",
            Some(&format!("unity://build/{}", params.target)),
            None,
        );
        tools::unity::handle_unity_build_trigger(&self.state, params)
    }

    #[tool(
        description = "Query the GameObject hierarchy and component summary of a Unity scene. \
        Pass a scene asset path (e.g. \"Assets/Scenes/Main.unity\") or an empty string for the currently-open scene. \
        Gated behind `unity://scene/**` capability."
    )]
    fn unity_scene_query(
        &self,
        Parameters(params): Parameters<tools::unity::UnitySceneQueryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "unity_scene_query",
            Some(&format!(
                "unity://scene/{}",
                if params.scene_path.is_empty() {
                    "active"
                } else {
                    &params.scene_path
                }
            )),
            None,
        );
        tools::unity::handle_unity_scene_query(&self.state, params)
    }

    #[tool(
        description = "Run Unity EditMode or PlayMode tests and return pass/fail counts. \
        Optionally supply a filter string to run only matching tests. \
        Gated behind `unity://test/**` capability."
    )]
    fn unity_test_run(
        &self,
        Parameters(params): Parameters<tools::unity::UnityTestRunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit("unity_test_run", Some("unity://test/run"), None);
        tools::unity::handle_unity_test_run(&self.state, params)
    }

    #[tool(description = "Trigger a Unity Addressables content build. \
        Requires the Addressables package to be installed in the project. \
        Gated behind `unity://build/**` capability.")]
    fn unity_addressables_build(
        &self,
        Parameters(params): Parameters<tools::unity::UnityAddressablesBuildParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "unity_addressables_build",
            Some("unity://build/addressables"),
            None,
        );
        tools::unity::handle_unity_addressables_build(&self.state, params)
    }

    #[tool(
        description = "Capture a screenshot from a Unity scene camera and save it as a PNG. \
        Provide the GameObject path to the camera (e.g. \"/Main Camera\") and the output file path \
        relative to the Unity project root. \
        Gated behind `unity://render/**` capability."
    )]
    fn unity_render_capture(
        &self,
        Parameters(params): Parameters<tools::unity::UnityRenderCaptureParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "unity_render_capture",
            Some(&format!("unity://render/capture/{}", params.camera_path)),
            None,
        );
        tools::unity::handle_unity_render_capture(&self.state, params)
    }

    // ── External Action Governance (v0.13.4) ─────────────────────

    #[tool(
        description = "Request an external action (email, API call, social post, DB query) through TA's governance pipeline. \
        TA applies the action policy from .ta/workflow.toml: 'review' captures the action for human approval before execution, \
        'auto' executes immediately, 'block' rejects outright. Rate limits are enforced per goal. \
        Set dry_run=true to test workflows without side effects. \
        Built-in action types: email, social_post, api_call, db_query. \
        Plugins can register additional types."
    )]
    fn ta_external_action(
        &self,
        Parameters(params): Parameters<ExternalActionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_external_action",
            params.target_uri.as_deref(),
            params.goal_run_id.as_deref().and_then(|id| id.parse().ok()),
        );
        tools::action::handle_external_action(&self.state, params)
    }

    // ── CoS read-only chat-mode design, item 4 (v0.17.11.12) ──────

    #[tool(
        description = "Propose a title/description revision to the Wayfinder task this goal is working on. \
        Captured for human review alongside this goal's normal draft -- not executed immediately and not a \
        separate automated report. The actual Wayfinder update happens only if the draft containing this \
        proposal is approved and applied. At least one of title/description must be set."
    )]
    fn ta_propose_task_update(
        &self,
        Parameters(params): Parameters<tools::wayfinder_task::ProposeTaskUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_propose_task_update",
            None,
            params.goal_run_id.parse().ok(),
        );
        tools::wayfinder_task::handle_propose_task_update(&self.state, params)
    }

    #[tool(
        description = "Propose creating a new Wayfinder task. Captured for human review in this goal's draft, \
        never executed immediately; the task is created only if the draft is approved and applied. `verb` is \
        required by Wayfinder. If `external_id` is omitted a deterministic one is derived so re-applying the \
        draft can never create a duplicate."
    )]
    fn ta_propose_task_create(
        &self,
        Parameters(params): Parameters<tools::wayfinder_task::ProposeTaskCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_propose_task_create",
            None,
            params.goal_run_id.parse().ok(),
        );
        tools::wayfinder_task::handle_propose_task_create(&self.state, params)
    }

    #[tool(
        description = "Propose reassigning a Wayfinder task. `assignee_id` is required: a Wayfinder roster/team-role \
        id (not a name), or null to clear. Captured for human review in this goal's draft; applied only if \
        the draft is approved and applied."
    )]
    fn ta_propose_task_reassign(
        &self,
        Parameters(params): Parameters<tools::wayfinder_task::ProposeTaskReassignParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_propose_task_reassign",
            None,
            params.goal_run_id.parse().ok(),
        );
        tools::wayfinder_task::handle_propose_task_reassign(&self.state, params)
    }

    #[tool(
        description = "Propose that a Wayfinder task needs revision: the delivered work is incorrect and the task \
        goes back to `open` in the work queue. Distinct from on-hold (blocked). Captured for human review in \
        this goal's draft; applied only if the draft is approved and applied."
    )]
    fn ta_propose_task_needs_revision(
        &self,
        Parameters(params): Parameters<tools::wayfinder_task::ProposeTaskNeedsRevisionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_propose_task_needs_revision",
            None,
            params.goal_run_id.parse().ok(),
        );
        tools::wayfinder_task::handle_propose_task_needs_revision(&self.state, params)
    }

    #[tool(
        description = "Propose putting a Wayfinder task on hold because it is blocked (business reason or a \
        dependency). `hold_reason` is required. Optionally pass `blocking_task` {title, verb, description?} to \
        create a precursor task the held task depends on. Distinct from needs-revision. Captured for human \
        review in this goal's draft; applied only if the draft is approved and applied."
    )]
    fn ta_propose_task_on_hold(
        &self,
        Parameters(params): Parameters<tools::wayfinder_task::ProposeTaskOnHoldParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_propose_task_on_hold",
            None,
            params.goal_run_id.parse().ok(),
        );
        tools::wayfinder_task::handle_propose_task_on_hold(&self.state, params)
    }

    #[tool(
        description = "Propose marking a Wayfinder task done. Captured for human review in this goal's draft; \
        the status changes only if the draft is approved and applied."
    )]
    fn ta_propose_task_complete(
        &self,
        Parameters(params): Parameters<tools::wayfinder_task::ProposeTaskCompleteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.audit(
            "ta_propose_task_complete",
            None,
            params.goal_run_id.parse().ok(),
        );
        tools::wayfinder_task::handle_propose_task_complete(&self.state, params)
    }
}

// ── ServerHandler implementation ─────────────────────────────────

#[tool_handler]
impl ServerHandler for TaGatewayServer {
    fn get_info(&self) -> ServerInfo {
        let chat_lock = self.state.lock().ok().and_then(|s| s.chat_lock);
        if let Some(session_id) = chat_lock {
            return ServerInfo {
                protocol_version: ProtocolVersion::V_2024_11_05,
                capabilities: ServerCapabilities::builder().enable_tools().build(),
                server_info: Implementation {
                    name: "trusted-autonomy".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    title: Some("Trusted Autonomy (chat mode)".into()),
                    icons: None,
                    website_url: None,
                },
                instructions: Some(format!(
                    "Trusted Autonomy MCP server in read-only chat mode. Pass goal_run_id \
                     \"{}\" to ta_fs_read/ta_fs_list/ta_fs_diff/ta_fs_write. You can read the \
                     workspace; writes are allowed only under {}/ and are discarded. No goal, \
                     draft, plan, wiki-write, task or external-action tools are available.",
                    session_id,
                    ta_policy::CHAT_SCRATCH_DIR
                )),
            };
        }
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

/// Canonical form of an already-validated workspace-relative path for the
/// policy target URI: drops empty and `.` components so `./a//b` and `a/b`
/// evaluate identically (they name the same file on every host).
///
/// Backslashes are deliberately NOT treated as separators here: on POSIX a
/// backslash is an ordinary file-name byte, so rewriting `scratch\x` to
/// `scratch/x` could let a write grant scoped to `scratch/**` match a file
/// that actually lands at the workspace root. Leaving them alone fails
/// closed (no grant matches). `..` under either separator is rejected
/// earlier by `validate_relative_path`.
fn normalize_workspace_relative(path: &str) -> String {
    path.split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_changeset::PRStatus;
    use tempfile::tempdir;

    /// Guards every test that sets/removes the process-global `TA_AGENT_ID`
    /// env var. Rust's test runner executes tests in parallel threads within
    /// one process by default, so without this lock one test's
    /// `remove_var` can land between another's `set_var` and its use,
    /// producing flaky failures. Mirrors the same pattern used elsewhere in
    /// this codebase for env-var test isolation (e.g.
    /// `apps/ta-cli/src/commands/credential_helper.rs`,
    /// `crates/ta-credential-broker/src/shim.rs`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        // 26 tools: goal_start, goal_status, goal_list,
        //           fs_read, fs_write, fs_list, fs_diff,
        //           pr_build, pr_status,
        //           ta_draft, ta_goal_inner, ta_plan, ta_plan_status (v0.14.3.2),
        //           ta_context, ta_agent_status (v0.9.6), ta_event_subscribe (v0.9.4),
        //           ta_workflow (v0.9.8.2), ta_ask_human (v0.9.9.1, deprecated alias
        //           for ta_human_verify as of v0.17.0.12.26),
        //           ta_external_action (v0.13.4),
        //           ue5_python_exec, ue5_scene_query, ue5_asset_list,
        //           ue5_mrq_submit, ue5_mrq_status (v0.14.14),
        //           ue5_sequencer_query, ue5_lighting_preset_list (v0.14.15.1)
        //           unity_build_trigger, unity_scene_query, unity_test_run,
        //           unity_addressables_build, unity_render_capture (v0.15.3)
        //           community_search, community_get, community_annotate,
        //           community_feedback, community_suggest (v0.17.0.12.4)
        //           ta_human_verify (v0.17.0.12.26)
        //           ta_whiteboard_presence_register, ta_whiteboard_presence_list,
        //           ta_whiteboard_handoff_send, ta_whiteboard_handoff_receive,
        //           ta_whiteboard_task_claim, ta_whiteboard_task_complete (v0.17.11.8)
        //           ta_whiteboard_outcome_send (v0.17.11.11)
        //           ta_wiki_search, ta_wiki_get, ta_wiki_types, ta_wiki_create,
        //           ta_wiki_update (v0.17.11.15)
        //           ta_propose_task_update (v0.17.11.12)
        //           ta_propose_task_create, ta_propose_task_reassign,
        //           ta_propose_task_needs_revision, ta_propose_task_on_hold,
        //           ta_propose_task_complete
        let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(tools.len(), 59, "expected 59 tools, got: {:?}", names);
        for kind in crate::tools::wayfinder_task::ProposeKind::ALL {
            assert!(
                names.iter().any(|n| n == kind.tool_name()),
                "{} is not registered",
                kind.tool_name()
            );
        }
    }

    // ── H7: every registered tool is classified; CoS surface is read-only ──

    /// Fails when a NEW tool is added to the MCP registry without being
    /// classified read-only or mutating in `ta_goal::tool_surface`, so a
    /// future tool can never silently enter a read-only (CoS) surface.
    #[test]
    fn h7_every_registered_mcp_tool_is_classified() {
        let (server, _dir) = test_server();
        let unclassified: Vec<String> = server
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .filter(|name| ta_goal::tool_surface::classify_mcp_tool(name).is_none())
            .collect();
        assert!(
            unclassified.is_empty(),
            "MCP tools registered without a read-only/mutating classification: {:?}. Add each \
             to ta_goal::tool_surface::MCP_TOOL_EFFECTS (mutating unless it provably only reads).",
            unclassified
        );
    }

    /// The classification table must not list tools that no longer exist
    /// (a stale "read-only" entry could later be reused by a different,
    /// mutating tool of the same name).
    #[test]
    fn h7_classification_table_has_no_stale_entries() {
        let (server, _dir) = test_server();
        let registered: std::collections::HashSet<String> = server
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        for (name, _) in ta_goal::tool_surface::MCP_TOOL_EFFECTS {
            assert!(
                registered.contains(*name),
                "classified tool '{}' is not registered",
                name
            );
        }
    }

    /// The default read-only (CoS) surface only names real, registered,
    /// read-only tools, and none of the tools a CoS must never hold.
    #[test]
    fn h7_read_only_surface_contains_no_mutating_registered_tool() {
        use ta_goal::tool_surface::{
            classify_mcp_tool, ToolEffect, READ_ONLY_PERSONA_ALLOWED_TOOLS,
        };
        let (server, _dir) = test_server();
        let registered: std::collections::HashSet<String> = server
            .tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        for entry in READ_ONLY_PERSONA_ALLOWED_TOOLS {
            let tool = entry.strip_prefix("mcp__ta__").expect("TA MCP tool");
            assert!(registered.contains(tool), "{} is not registered", tool);
            assert_eq!(
                classify_mcp_tool(tool),
                Some(ToolEffect::ReadOnly),
                "{}",
                tool
            );
        }
        for must_be_mutating in [
            "ta_fs_write",
            "ta_wiki_create",
            "ta_wiki_update",
            "ta_external_action",
            "ta_propose_task_update",
            "ta_draft",
            "ta_pr_build",
            "ta_goal_start",
            "ta_goal_inner",
            "ta_plan",
            "ta_workflow",
            "ta_whiteboard_presence_register",
            "ta_whiteboard_handoff_send",
            "ta_whiteboard_handoff_receive",
            "ta_whiteboard_task_claim",
            "ta_whiteboard_task_complete",
        ] {
            assert_eq!(
                classify_mcp_tool(must_be_mutating),
                Some(ToolEffect::Mutating),
                "{} must be classified mutating",
                must_be_mutating
            );
            assert!(
                !READ_ONLY_PERSONA_ALLOWED_TOOLS
                    .contains(&format!("mcp__ta__{}", must_be_mutating).as_str()),
                "{} is in the read-only surface",
                must_be_mutating
            );
        }
    }

    /// The chat manifest's policy grants (the other half of the chat
    /// surface) contain only fs reads and scratch-only writes: no git,
    /// email, external, or other tool grants.
    #[test]
    fn h7_chat_manifest_grants_only_fs_read_and_scratch_write() {
        let manifest =
            ta_policy::compile_chat_manifest("cos:chat:x", "fs://workspace/**", 1).unwrap();
        assert!(!manifest.grants.is_empty());
        for grant in &manifest.grants {
            assert_eq!(grant.tool, "fs", "unexpected tool grant: {:?}", grant);
            match grant.verb.as_str() {
                "read" => {}
                "write_patch" => assert!(
                    grant.resource_pattern.contains(ta_policy::CHAT_SCRATCH_DIR),
                    "write grant outside scratch: {:?}",
                    grant
                ),
                other => panic!("unexpected verb '{}' in chat manifest: {:?}", other, grant),
            }
        }
    }

    #[test]
    fn tool_names_are_prefixed() {
        let (server, _dir) = test_server();
        let tools = server.tool_router.list_all();
        for tool in &tools {
            assert!(
                tool.name.starts_with("ta_")
                    || tool.name.starts_with("ue5_")
                    || tool.name.starts_with("comfyui_")
                    || tool.name.starts_with("unity_")
                    || tool.name.starts_with("community_"),
                "tool '{}' should be prefixed with 'ta_', 'ue5_', 'comfyui_', 'unity_', or 'community_'",
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
    fn start_goal_rejects_agent_id_containing_chat_marker() {
        let (server, _dir) = test_server();
        let mut state = server.state.lock().unwrap();

        // start_goal should reject agent_id containing ":chat:"
        let result = state.start_goal("Test Goal", "Testing the system", "someagent:chat:deadbeef");
        assert!(
            result.is_err(),
            "start_goal should reject agent_id with ':chat:' marker"
        );
        if let Err(GatewayError::Other(msg)) = result {
            assert!(
                msg.contains(":chat:"),
                "error message should mention ':chat:' marker"
            );
        }
    }

    #[test]
    fn start_goal_with_profile_rejects_agent_id_containing_chat_marker() {
        let (server, _dir) = test_server();
        let mut state = server.state.lock().unwrap();

        // start_goal_with_profile should reject agent_id containing ":chat:"
        let profile = AlignmentProfile::default_developer();
        let result = state.start_goal_with_profile(
            "Test Goal",
            "Testing the system",
            "someagent:chat:deadbeef",
            &profile,
            None,
        );
        assert!(
            result.is_err(),
            "start_goal_with_profile should reject agent_id with ':chat:' marker"
        );
        if let Err(GatewayError::Other(msg)) = result {
            assert!(
                msg.contains(":chat:"),
                "error message should mention ':chat:' marker"
            );
        }
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
    fn chat_session_fs_access_is_enforced_through_the_real_mcp_tool_handlers() {
        use crate::server::{FsReadParams, FsWriteParams};
        use crate::tools::fs::{handle_fs_read, handle_fs_write};

        let (server, _dir) =
            test_server_with_source(&[("notes.txt", b"hello from the real workspace\n")]);
        let goal_run_id = {
            let mut state = server.state.lock().unwrap();
            state
                .start_chat_session("chat-agent", "fs://workspace/**", 1)
                .unwrap()
                .goal_run_id
                .to_string()
        };

        // Read anywhere in the workspace: the real handler succeeds.
        let read_result = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id: goal_run_id.clone(),
                path: "notes.txt".to_string(),
            },
        );
        assert!(
            read_result.is_ok(),
            "expected chat session to read workspace files, got {:?}",
            read_result.err()
        );

        // Write inside the chat scratch dir: the real handler succeeds.
        let scratch_path = format!("{}/notes.md", ta_policy::CHAT_SCRATCH_DIR);
        let write_ok = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: goal_run_id.clone(),
                path: scratch_path,
                content: "scratch notes".to_string(),
            },
        );
        assert!(
            write_ok.is_ok(),
            "expected chat session to write inside chat-scratch, got {:?}",
            write_ok.err()
        );

        // Write outside the scratch dir: the real handler rejects it,
        // propagated from PolicyDecision::Deny through enforce_policy()
        // into a real McpError.
        let write_denied = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: goal_run_id.clone(),
                path: "src/main.rs".to_string(),
                content: "malicious".to_string(),
            },
        );
        assert!(
            write_denied.is_err(),
            "expected chat session write outside scratch to be denied"
        );

        // Reading a secrets-path file: denied the same way, proving Task 1's
        // backstop applies to chat sessions too (chat mode's broad fs_read
        // grant does not bypass it).
        let secret_read_denied = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id,
                path: ".env".to_string(),
            },
        );
        // Assert on the actual error message, not just is_err(): the test
        // fixture never creates a real `.env` file, so a missing-backstop
        // read would ALSO fail (with an I/O "file not found" error),
        // proving nothing about the backstop. Checking for both
        // "Policy denied" (enforce_policy's wrapping of a Deny decision)
        // and "secrets path" (the backstop's own deny reason) confirms the
        // denial actually came from the secrets backstop.
        let err = secret_read_denied.expect_err("expected chat session to be denied reading .env");
        assert!(
            err.message.contains("Policy denied") && err.message.contains("secrets path"),
            "expected a policy-denial error mentioning the secrets-path backstop, got: {}",
            err.message
        );
    }

    // ── H5: absolute-path / traversal workspace escape ──────────────────
    //
    // These go through the real MCP tool handlers (handle_fs_read /
    // handle_fs_write), the real PolicyEngine, the real FsConnector and the
    // real StagingWorkspace: nothing under test is mocked.

    /// A file that lives OUTSIDE the gateway's workspace root, standing in
    /// for something like `~/.aws/credentials`. Returns the guard (keeps the
    /// tempdir alive) and the absolute path as a string.
    fn outside_workspace_file(content: &[u8]) -> (tempfile::TempDir, String) {
        let outside = tempdir().unwrap();
        let path = outside.path().join("outside-secret.txt");
        std::fs::write(&path, content).unwrap();
        let abs = path.to_string_lossy().to_string();
        (outside, abs)
    }

    #[test]
    fn h5_chat_session_cannot_read_outside_workspace_via_absolute_path() {
        use crate::server::FsReadParams;
        use crate::tools::fs::handle_fs_read;

        let (server, _dir) = test_server_with_source(&[("notes.txt", b"in workspace\n")]);
        let (_outside, abs_path) = outside_workspace_file(b"TOP-SECRET-OUTSIDE-WORKSPACE");
        let goal_run_id = {
            let mut state = server.state.lock().unwrap();
            state
                .start_chat_session("chat-agent", "fs://workspace/**", 1)
                .unwrap()
                .goal_run_id
                .to_string()
        };

        let result = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id,
                path: abs_path.clone(),
            },
        );
        match result {
            Ok(ok) => panic!(
                "H5 regression: absolute path '{}' outside the workspace was readable \
                 through ta_fs_read: {:?}",
                abs_path, ok
            ),
            Err(err) => {
                assert!(
                    !err.message.contains("TOP-SECRET"),
                    "error must not leak file content: {}",
                    err.message
                );
                assert!(
                    err.message.contains("Policy denied"),
                    "expected the policy layer to deny the absolute path before any I/O, got: {}",
                    err.message
                );
            }
        }
    }

    #[test]
    fn h5_normal_goal_cannot_read_outside_workspace_via_absolute_path() {
        use crate::server::FsReadParams;
        use crate::tools::fs::handle_fs_read;

        let (server, _dir) = test_server();
        let (_outside, abs_path) = outside_workspace_file(b"TOP-SECRET-OUTSIDE-WORKSPACE");
        let goal_run_id = start_goal(&server).to_string();

        let result = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id,
                path: abs_path,
            },
        );
        let err = result.expect_err("absolute path outside workspace must be rejected");
        assert!(!err.message.contains("TOP-SECRET"));
    }

    #[test]
    fn h5_chat_session_cannot_read_via_parent_traversal() {
        use crate::server::FsReadParams;
        use crate::tools::fs::handle_fs_read;

        let (server, _dir) = test_server_with_source(&[("notes.txt", b"in workspace\n")]);
        let goal_run_id = {
            let mut state = server.state.lock().unwrap();
            state
                .start_chat_session("chat-agent", "fs://workspace/**", 1)
                .unwrap()
                .goal_run_id
                .to_string()
        };
        for path in [
            "../outside.txt",
            "sub/../../outside.txt",
            "sub\\..\\..\\outside.txt",
        ] {
            let result = handle_fs_read(
                &server.state,
                FsReadParams {
                    goal_run_id: goal_run_id.clone(),
                    path: path.to_string(),
                },
            );
            let err = result.expect_err("parent traversal must be rejected");
            assert!(
                err.message.contains("Policy denied"),
                "expected policy denial for '{}', got: {}",
                path,
                err.message
            );
        }
    }

    #[test]
    fn h5_chat_session_cannot_write_outside_workspace_via_absolute_path() {
        use crate::server::FsWriteParams;
        use crate::tools::fs::handle_fs_write;

        let (server, _dir) = test_server();
        let outside = tempdir().unwrap();
        // An absolute path whose tail even looks like the chat scratch dir:
        // the scratch-only write grant must not be satisfiable by an
        // absolute path anywhere on disk.
        let target = outside
            .path()
            .join(ta_policy::CHAT_SCRATCH_DIR)
            .join("pwned.txt");
        let goal_run_id = {
            let mut state = server.state.lock().unwrap();
            state
                .start_chat_session("chat-agent", "fs://workspace/**", 1)
                .unwrap()
                .goal_run_id
                .to_string()
        };
        let result = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id,
                path: target.to_string_lossy().to_string(),
                content: "pwned".to_string(),
            },
        );
        assert!(result.is_err(), "absolute-path write must be rejected");
        assert!(
            !target.exists(),
            "nothing may be written outside the workspace"
        );
    }

    #[test]
    fn h5_check_policy_denies_absolute_and_traversal_paths_in_every_os_form() {
        let (server, _dir) = test_server();
        let goal_id = start_goal(&server);
        let state = server.state.lock().unwrap();
        let agent_id = state.agent_for_goal(goal_id).unwrap();
        for path in [
            "/etc/passwd",
            "//etc/passwd",
            "\\etc\\passwd",
            "C:\\Users\\me\\.aws\\credentials",
            "c:/Users/me/.aws/credentials",
            "\\\\server\\share\\secret.txt",
            "../x",
            "a/../../x",
            "a\\..\\x",
            "",
        ] {
            let decision = state.check_policy(&agent_id, "read", path).unwrap();
            assert!(
                matches!(decision, PolicyDecision::Deny { .. }),
                "expected Deny for {:?}, got {:?}",
                path,
                decision
            );
        }
    }

    #[test]
    fn h5_legitimate_workspace_relative_reads_and_writes_still_work() {
        use crate::server::{FsReadParams, FsWriteParams};
        use crate::tools::fs::{handle_fs_read, handle_fs_write};

        let (server, _dir) = test_server_with_source(&[
            ("notes.txt", b"top level\n"),
            ("src/deep/nested/mod.rs", b"nested\n"),
        ]);
        let goal_run_id = start_goal(&server).to_string();

        for (path, expected) in [
            ("notes.txt", "top level\n"),
            ("./notes.txt", "top level\n"),
            ("src/deep/nested/mod.rs", "nested\n"),
            ("src//deep/./nested/mod.rs", "nested\n"),
        ] {
            let result = handle_fs_read(
                &server.state,
                FsReadParams {
                    goal_run_id: goal_run_id.clone(),
                    path: path.to_string(),
                },
            )
            .unwrap_or_else(|e| panic!("legit read of '{}' failed: {}", path, e.message));
            let text = format!("{:?}", result.content);
            assert!(
                text.contains(expected.trim_end()),
                "read of '{}' returned unexpected content: {}",
                path,
                text
            );
        }

        handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: goal_run_id.clone(),
                path: "src/deep/new_file.rs".to_string(),
                content: "pub fn f() {}".to_string(),
            },
        )
        .unwrap_or_else(|e| panic!("legit staged write failed: {}", e.message));
    }

    // ── H9: no auto-approve shortcut for CoS/chat-originated goals ──────

    /// Build + submit a draft for `goal_run_id` through the real `ta_draft`
    /// MCP handler, with project policy auto-approving every draft and a
    /// non-blocking review channel standing in for the human. Returns the
    /// submit response JSON.
    fn submit_draft_with_auto_approve_policy(
        server: &TaGatewayServer,
        workspace: &std::path::Path,
        goal_run_id: Uuid,
        path: &str,
    ) -> serde_json::Value {
        use crate::server::DraftToolParams;
        use crate::tools::draft::handle_draft;

        let mut doc = ta_policy::PolicyDocument::default();
        doc.defaults.auto_approve.drafts.enabled = true;
        std::fs::create_dir_all(workspace.join(".ta")).unwrap();
        std::fs::write(
            workspace.join(".ta/policy.yaml"),
            serde_yaml::to_string(&doc).unwrap(),
        )
        .unwrap();
        {
            let mut state = server.state.lock().unwrap();
            state.set_review_channel(Box::new(ta_changeset::AutoApproveChannel::new()));
            state
                .connectors
                .get_mut(&goal_run_id)
                .unwrap()
                .write_patch(path, b"content")
                .unwrap();
        }
        for action in ["build", "submit"] {
            let result = handle_draft(
                &server.state,
                DraftToolParams {
                    action: action.to_string(),
                    goal_run_id: Some(goal_run_id.to_string()),
                    summary: Some("h9 test".to_string()),
                    draft_id: None,
                    require_review: None,
                },
            )
            .unwrap_or_else(|e| panic!("ta_draft {} failed: {}", action, e.message));
            if action == "submit" {
                let text = match &result.content[0].raw {
                    rmcp::model::RawContent::Text(t) => t.text.clone(),
                    other => panic!("unexpected content: {:?}", other),
                };
                return serde_json::from_str(&text).unwrap();
            }
        }
        unreachable!()
    }

    fn set_goal_origin(server: &TaGatewayServer, goal_run_id: Uuid, origin: Option<&str>) {
        let state = server.state.lock().unwrap();
        let mut goal = state.goal_store.get(goal_run_id).unwrap().unwrap();
        goal.origin = origin.map(str::to_string);
        state.goal_store.save(&goal).unwrap();
    }

    #[test]
    fn h9_cos_origin_goal_is_not_auto_approved_by_mcp_submit() {
        let (server, dir) = test_server();
        let goal_id = start_goal(&server);
        set_goal_origin(&server, goal_id, Some("cos"));
        let response = submit_draft_with_auto_approve_policy(&server, dir.path(), goal_id, "a.txt");
        assert_ne!(response["status"], "auto_approved", "{}", response);
        assert_ne!(response["approved_by"], "policy:auto", "{}", response);
        let blockers = response["auto_approve_blockers"].to_string();
        assert!(
            blockers.contains("auto-approve refused: origin=cos"),
            "refusal must be visible in the response: {}",
            response
        );
    }

    #[test]
    fn h9_chat_session_is_stamped_chat_origin_and_not_auto_approved() {
        let (server, dir) = test_server();
        let goal_id = {
            let mut state = server.state.lock().unwrap();
            let goal = state
                .start_chat_session("chat-agent", "fs://workspace/**", 1)
                .unwrap();
            assert_eq!(goal.origin.as_deref(), Some("chat"));
            goal.goal_run_id
        };
        let path = format!("{}/notes.md", ta_policy::CHAT_SCRATCH_DIR);
        let response = submit_draft_with_auto_approve_policy(&server, dir.path(), goal_id, &path);
        assert_ne!(response["status"], "auto_approved", "{}", response);
        assert!(response["auto_approve_blockers"]
            .to_string()
            .contains("auto-approve refused: origin=chat"));
    }

    #[test]
    fn h9_goal_without_origin_or_with_other_origin_is_auto_approved_as_before() {
        for origin in [None, Some("cli")] {
            let (server, dir) = test_server();
            let goal_id = start_goal(&server);
            set_goal_origin(&server, goal_id, origin);
            let response =
                submit_draft_with_auto_approve_policy(&server, dir.path(), goal_id, "a.txt");
            assert_eq!(
                response["status"], "auto_approved",
                "origin {:?}: {}",
                origin, response
            );
        }
    }

    #[test]
    fn h5_chat_session_scratch_write_still_works_and_reads_inside_workspace_still_work() {
        use crate::server::{FsReadParams, FsWriteParams};
        use crate::tools::fs::{handle_fs_read, handle_fs_write};

        let (server, _dir) = test_server_with_source(&[("a/b.txt", b"hello\n")]);
        let goal_run_id = {
            let mut state = server.state.lock().unwrap();
            state
                .start_chat_session("chat-agent", "fs://workspace/**", 1)
                .unwrap()
                .goal_run_id
                .to_string()
        };
        handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id: goal_run_id.clone(),
                path: "a/b.txt".to_string(),
            },
        )
        .unwrap_or_else(|e| panic!("chat read failed: {}", e.message));
        handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id,
                path: format!("./{}/scratch.md", ta_policy::CHAT_SCRATCH_DIR),
                content: "notes".to_string(),
            },
        )
        .unwrap_or_else(|e| panic!("chat scratch write failed: {}", e.message));
    }

    #[test]
    fn chat_session_manifest_is_not_widened_by_a_later_start_goal_sharing_its_raw_agent_id() {
        // Regression test for the vulnerability in Finding 2 of the final
        // whole-branch review: PolicyEngine::load_manifest() keys manifests
        // by a plain agent_id string. If a chat session's manifest were
        // keyed by the raw, caller-supplied agent_id, a later start_goal
        // call reusing that SAME agent_id string (e.g. a poller reusing a
        // stable id like "cos") would silently overwrite the chat
        // session's narrow manifest with a broad developer-profile one,
        // widening a still-live chat session's access. start_chat_session
        // now derives its own internal policy identity, so this must not
        // happen.
        use crate::server::{FsReadParams, FsWriteParams};
        use crate::tools::fs::{handle_fs_read, handle_fs_write};

        let (server, _dir) =
            test_server_with_source(&[("notes.txt", b"hello from the real workspace\n")]);

        let shared_agent_id = "shared-agent";
        let chat_goal_run_id = {
            let mut state = server.state.lock().unwrap();
            state
                .start_chat_session(shared_agent_id, "fs://workspace/**", 1)
                .unwrap()
                .goal_run_id
                .to_string()
        };

        // A later call reuses the exact same raw agent_id for a normal,
        // broad-access goal.
        {
            let mut state = server.state.lock().unwrap();
            state
                .start_goal("Some Goal", "unrelated objective", shared_agent_id)
                .unwrap();
        }

        // The original chat session must still be denied a write outside
        // its chat-scratch directory: its manifest must not have been
        // widened by the later start_goal call.
        let write_denied = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: chat_goal_run_id.clone(),
                path: "src/main.rs".to_string(),
                content: "malicious".to_string(),
            },
        );
        assert!(
            write_denied.is_err(),
            "expected chat session write outside scratch to still be denied after a later \
             start_goal call reused its raw agent_id, got: {:?}",
            write_denied
        );

        // Sanity: the chat session can still read the workspace (its own
        // manifest is intact, not merely broken).
        let read_ok = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id: chat_goal_run_id,
                path: "notes.txt".to_string(),
            },
        );
        assert!(
            read_ok.is_ok(),
            "expected chat session to still read workspace files, got {:?}",
            read_ok.err()
        );
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

    /// All resolve_agent_id tests run in a single test to avoid env var races.
    /// `set_var`/`remove_var` on `TA_AGENT_ID` is not thread-safe — parallel
    /// tests that touch the same env var produce flaky results.
    #[test]
    fn resolve_agent_id_priority_order() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (server, _dir) = test_server();

        // 1. Env var takes priority.
        std::env::set_var("TA_AGENT_ID", "env-agent-42");
        {
            let state = server.state.lock().unwrap();
            assert_eq!(state.resolve_agent_id(), "env-agent-42");
        }
        std::env::remove_var("TA_AGENT_ID");

        // 2. Falls back to dev_session_id when env var is absent.
        {
            let mut state = server.state.lock().unwrap();
            state.dev_session_id = Some("dev-session-99".to_string());
            assert_eq!(state.resolve_agent_id(), "dev-session-99");
        }

        // 3. Falls back to "unknown" when both are absent.
        {
            let mut state = server.state.lock().unwrap();
            state.dev_session_id = None;
            assert_eq!(state.resolve_agent_id(), "unknown");
        }
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

    // v0.17.x: generic cost-experiment marker consultation.

    #[test]
    fn resolve_current_goal_run_id_uses_active_agents_map() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (server, _dir) = test_server();
        let goal_run_id = Uuid::new_v4();
        {
            let mut state = server.state.lock().unwrap();
            state.touch_agent_session("agent-1", "claude", Some(goal_run_id));
        }
        std::env::set_var("TA_AGENT_ID", "agent-1");

        {
            let state = server.state.lock().unwrap();
            assert_eq!(state.resolve_current_goal_run_id(), Some(goal_run_id));
        }

        std::env::remove_var("TA_AGENT_ID");
    }

    #[test]
    fn resolve_current_goal_run_id_is_none_when_no_agent_matches() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (server, _dir) = test_server();
        std::env::remove_var("TA_AGENT_ID");
        let state = server.state.lock().unwrap();
        assert_eq!(state.resolve_current_goal_run_id(), None);
    }

    #[test]
    fn ta_wiki_search_returns_disabled_stub_when_wiki_disabled_override_is_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (server, _dir) = test_server();
        {
            let mut state = server.state.lock().unwrap();
            let mut goal = GoalRun::new(
                "t",
                "o",
                "agent-1",
                std::path::PathBuf::from("/tmp/ws"),
                std::path::PathBuf::from("/tmp/store"),
            );
            goal.experiment_overrides = Some(serde_json::json!({"wiki.disabled": true}));
            let goal_run_id = goal.goal_run_id;
            state.goal_store.save(&goal).unwrap();
            state.touch_agent_session("agent-1", "claude", Some(goal_run_id));
        }
        std::env::set_var("TA_AGENT_ID", "agent-1");

        let result = server
            .ta_wiki_search(Parameters(tools::wiki::WikiSearchParams {
                scope: "project".to_string(),
                id: "proj-1".to_string(),
                query: "anything".to_string(),
            }))
            .unwrap();

        let text = result.content[0].raw.as_text().unwrap().text.clone();
        assert!(
            text.contains("disabled"),
            "expected disabled stub, got: {text}"
        );

        std::env::remove_var("TA_AGENT_ID");
    }

    // ── Chat-mode launch (persona chat_mode = true / ta run --chat-mode) ──

    fn chat_launch(agent_id: &str) -> crate::chat_launch::ChatLaunch {
        crate::chat_launch::ChatLaunch {
            agent_id: agent_id.to_string(),
            session_id: Uuid::new_v4(),
            workspace_root: None,
        }
    }

    fn chat_mode_server(
        source_content: &[(&str, &[u8])],
        launch: &crate::chat_launch::ChatLaunch,
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
        let server = TaGatewayServer::new_chat_mode(config, launch).unwrap();
        (server, dir)
    }

    /// H10: the manifest a chat-mode launch actually loads (looked up from
    /// the live PolicyEngine by the session's real policy identity, not
    /// recompiled in the test) grants fs read across the workspace and fs
    /// write_patch only under chat scratch, and nothing for git, email, or
    /// any other tool.
    #[test]
    fn chat_mode_launch_loads_compiled_chat_manifest_with_no_write_git_or_email_grants() {
        let launch = chat_launch("chief-of-staff");
        let (server, _dir) = chat_mode_server(&[], &launch);
        let state = server.state.lock().unwrap();

        assert_eq!(state.chat_lock, Some(launch.session_id));
        let policy_id = state.agent_for_goal(launch.session_id).unwrap();
        assert_eq!(
            policy_id,
            format!("chief-of-staff:chat:{}", launch.session_id)
        );
        let manifest = state
            .policy_engine
            .manifest_for(&policy_id)
            .expect("chat-mode launch must load a manifest for its policy identity");

        assert!(!manifest.grants.is_empty());
        for grant in &manifest.grants {
            assert_eq!(grant.tool, "fs", "unexpected non-fs grant: {:?}", grant);
            match grant.verb.as_str() {
                "read" => assert_eq!(grant.resource_pattern, "fs://workspace/**"),
                "write_patch" => assert_eq!(
                    grant.resource_pattern,
                    format!("fs://workspace/{}/**", ta_policy::CHAT_SCRATCH_DIR)
                ),
                other => panic!("unexpected verb '{}' in chat manifest: {:?}", other, grant),
            }
        }
        for (tool, verb, target) in [
            ("git", "commit", "git://workspace/main"),
            ("git", "push", "git://workspace/main"),
            ("email", "send", "email://someone@example.com"),
            ("fs", "write_patch", "fs://workspace/src/main.rs"),
            ("fs", "apply", "fs://workspace/src/main.rs"),
        ] {
            let decision = state.policy_engine.evaluate(&ta_policy::PolicyRequest {
                agent_id: policy_id.clone(),
                tool: tool.to_string(),
                verb: verb.to_string(),
                target_uri: target.to_string(),
            });
            assert!(
                matches!(decision, ta_policy::PolicyDecision::Deny { .. }),
                "chat manifest must deny {} {} {}, got {:?}",
                tool,
                verb,
                target,
                decision
            );
        }
    }

    /// H11: a chat-mode server exposes only the chat-mode tool profile.
    /// Mutating tools are gone from the router entirely (not listed, not
    /// dispatchable), however the agent harness is configured.
    #[test]
    fn chat_mode_server_router_exposes_only_chat_mode_tools() {
        let launch = chat_launch("chief-of-staff");
        let (server, _dir) = chat_mode_server(&[], &launch);
        let names = server.tool_names();
        assert!(!names.is_empty());
        for name in &names {
            assert!(
                ta_goal::chat_mode::is_chat_mode_mcp_tool(name),
                "non-chat tool '{}' still routed in chat mode",
                name
            );
        }
        for mutating in [
            "ta_goal_start",
            "ta_goal_inner",
            "ta_pr_build",
            "ta_draft",
            "ta_plan",
            "ta_workflow",
            "ta_external_action",
            "ta_wiki_create",
            "ta_wiki_update",
            "ta_propose_task_update",
            "ta_whiteboard_task_claim",
            "ta_context",
            "ta_human_verify",
        ] {
            assert!(
                !server.tool_router.has_route(mutating),
                "{} must not be routable in chat mode",
                mutating
            );
        }
        assert!(server.tool_router.has_route("ta_fs_read"));
        // The Chief-of-Staff's outbound channel and its human-question tool
        // are deliberately routable (see the profile's comments).
        assert!(server.tool_router.has_route("ta_whiteboard_outcome_send"));
        assert!(server.tool_router.has_route("ta_ask_human"));

        // Regression: a normal server still exposes the full surface and
        // is not chat-locked.
        let (normal, _d2) = test_server();
        assert!(normal.tool_router.has_route("ta_goal_start"));
        assert!(normal.state.lock().unwrap().chat_lock.is_none());

        // Typo guard: every profile entry names a real tool, so the chat
        // profile and the router cannot silently drift apart.
        for name in ta_goal::chat_mode::CHAT_MODE_MCP_TOOLS {
            assert!(
                server.tool_router.has_route(name),
                "chat-mode profile lists '{}' but the gateway has no such tool",
                name
            );
        }
    }

    fn outcome_params(outcome: &str) -> crate::tools::whiteboard::OutcomeSendParams {
        crate::tools::whiteboard::OutcomeSendParams {
            candidate_id: "wayfinder-task:t1#abc".to_string(),
            outcome: outcome.to_string(),
            detail: "d".to_string(),
            new_task_title: None,
        }
    }

    /// Chat mode accepts only the Chief-of-Staff vocabulary at the tool.
    /// There is no whiteboard session in this fixture, so an ACCEPTED value
    /// gets past the vocabulary check and fails later on the missing
    /// session; a REJECTED value fails on the vocabulary with an actionable
    /// error naming the allowed values.
    #[test]
    fn chat_mode_outcome_send_accepts_only_reply_delegate_done() {
        let launch = chat_launch("chief-of-staff");
        let (server, _dir) = chat_mode_server(&[], &launch);
        for ok in ["reply", "delegate", "done"] {
            let err =
                crate::tools::whiteboard::handle_outcome_send(server.state(), outcome_params(ok))
                    .unwrap_err()
                    .to_string();
            assert!(err.contains("no whiteboard session"), "{ok}: {err}");
        }
        for bad in ["new_work", "blocked", "Reply", "", "done "] {
            let err =
                crate::tools::whiteboard::handle_outcome_send(server.state(), outcome_params(bad))
                    .unwrap_err()
                    .to_string();
            assert!(err.contains("not allowed in chat mode"), "{bad}: {err}");
            for allowed in ["reply", "delegate", "done"] {
                assert!(err.contains(allowed), "{bad}: {err}");
            }
            assert!(!err.contains("no whiteboard session"), "{bad}: {err}");
        }
    }

    /// The chat-mode server advertises exactly reply/delegate/done; a normal
    /// server keeps the unchanged tool and does not enforce the vocabulary.
    #[test]
    fn chat_mode_outcome_tool_advertises_reply_delegate_done_and_normal_server_is_unchanged() {
        let launch = chat_launch("chief-of-staff");
        let (server, _dir) = chat_mode_server(&[], &launch);
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == "ta_whiteboard_outcome_send")
            .expect("outcome tool is in the chat profile");
        let desc = tool.description.clone().unwrap().to_string();
        for v in ["reply", "delegate", "done"] {
            assert!(desc.contains(v), "{desc}");
        }
        let outcome = &tool.input_schema["properties"]["outcome"];
        assert_eq!(
            outcome["enum"],
            serde_json::json!(["reply", "delegate", "done"])
        );

        let (normal, _d) = test_server();
        let ntool = normal
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == "ta_whiteboard_outcome_send")
            .unwrap();
        assert!(ntool.input_schema["properties"]["outcome"]
            .get("enum")
            .is_none());
        for any in ["new_work", "blocked", "done"] {
            let err =
                crate::tools::whiteboard::handle_outcome_send(normal.state(), outcome_params(any))
                    .unwrap_err()
                    .to_string();
            assert!(err.contains("no whiteboard session"), "{any}: {err}");
        }
    }

    /// H12: through the real MCP tool handlers, a launched chat-mode
    /// session can write only under chat scratch, cannot point the fs tools
    /// at any other goal (even a broad developer-profile goal living in the
    /// same gateway), and cannot read secrets.
    #[test]
    fn chat_mode_launch_cannot_write_outside_scratch_or_reach_another_goal() {
        use crate::server::{FsReadParams, FsWriteParams};
        use crate::tools::fs::{handle_fs_read, handle_fs_write};

        let launch = chat_launch("chief-of-staff");
        let (server, _dir) = chat_mode_server(&[("notes.txt", b"project notes\n")], &launch);
        let chat_id = launch.session_id.to_string();

        let read = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id: chat_id.clone(),
                path: "notes.txt".to_string(),
            },
        );
        assert!(read.is_ok(), "chat read failed: {:?}", read.err());

        let scratch = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: chat_id.clone(),
                path: format!("{}/answer.md", ta_policy::CHAT_SCRATCH_DIR),
                content: "draft answer".to_string(),
            },
        );
        assert!(scratch.is_ok(), "scratch write failed: {:?}", scratch.err());

        for path in [
            "src/main.rs",
            "PLAN.md",
            ".ta/chat-scratch-evil/x",
            "../escape.txt",
        ] {
            let denied = handle_fs_write(
                &server.state,
                FsWriteParams {
                    goal_run_id: chat_id.clone(),
                    path: path.to_string(),
                    content: "malicious".to_string(),
                },
            );
            assert!(denied.is_err(), "write to '{}' must be denied", path);
        }

        // A broad developer-profile goal exists in this same process. The
        // chat lock must stop the agent from borrowing its manifest by
        // passing its goal_run_id.
        let other_goal = {
            let mut state = server.state.lock().unwrap();
            state
                .start_goal("dev goal", "broad access", "implementer")
                .unwrap()
                .goal_run_id
        };
        let borrowed = handle_fs_write(
            &server.state,
            FsWriteParams {
                goal_run_id: other_goal.to_string(),
                path: "src/main.rs".to_string(),
                content: "malicious".to_string(),
            },
        );
        let err = borrowed.expect_err("chat-locked server must reject another goal's id");
        assert!(
            err.message.contains("locked to chat session") && err.message.contains(&chat_id),
            "lock error must tell the agent which id to use: {}",
            err.message
        );

        let secret = handle_fs_read(
            &server.state,
            FsReadParams {
                goal_run_id: chat_id,
                path: ".env".to_string(),
            },
        );
        assert!(secret
            .expect_err("secrets must stay denied")
            .message
            .contains("secrets path"));
    }

    /// H6 applies to the launch path too: the reserved ':chat:' marker is
    /// rejected for chat sessions, and a chat session can never take over
    /// an existing goal's id.
    #[test]
    fn chat_session_rejects_reserved_marker_and_existing_goal_ids() {
        let (server, _dir) = test_server();
        let mut state = server.state.lock().unwrap();
        let err = state
            .start_chat_session("cos:chat:deadbeef", "fs://workspace/**", 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains(":chat:"), "{}", err);

        let real_goal = state
            .start_goal("real goal", "objective", "implementer")
            .unwrap()
            .goal_run_id;
        let err = state
            .start_chat_session_with_id("chief-of-staff", real_goal, "fs://workspace/**", 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already belongs to goal"), "{}", err);
        assert_eq!(state.agent_for_goal(real_goal).unwrap(), "implementer");
    }

    #[test]
    fn chat_mode_server_instructions_name_the_session_id() {
        let launch = chat_launch("chief-of-staff");
        let (server, _dir) = chat_mode_server(&[], &launch);
        let text = server.get_info().instructions.unwrap_or_default();
        assert!(text.contains(&launch.session_id.to_string()), "{}", text);
        assert!(text.contains("chat mode"), "{}", text);
    }
}
