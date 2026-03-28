// config.rs — Daemon configuration from `.ta/daemon.toml`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ta_policy::AccessFilter;

pub use ta_extension::{ApiKeyEntry, LocalUserEntry, PluginsConfig};

/// Top-level daemon configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub commands: CommandConfig,
    pub agent: AgentConfig,
    pub routing: RoutingConfig,
    pub channels: ChannelsConfig,
    /// MCP transport configuration (v0.13.2).
    /// Controls how agents connect to the TA MCP server.
    /// Default: stdio (backward-compatible with existing setups).
    #[serde(default)]
    pub transport: TransportConfig,
    /// Shell Q&A agent configuration (v0.11.4.2).
    #[serde(default)]
    pub shell: ShellQaConfig,
    /// Sandbox configuration for command validation (v0.10.16, item 3).
    /// When set, the orchestrator validates commands against the sandbox
    /// allowlist before execution.
    #[serde(default)]
    pub sandbox: Option<SandboxSection>,
    /// Operations configuration (v0.11.2.3): heartbeat, watchdog, etc.
    #[serde(default)]
    pub operations: Option<OperationsConfig>,
    /// Lifecycle management configuration (v0.13.1).
    #[serde(default)]
    pub lifecycle: LifecycleConfig,
    /// Power & sleep management configuration (v0.13.1.1).
    #[serde(default)]
    pub power: PowerConfig,
    /// Experimental feature flags (v0.13.17).
    ///
    /// Features gated here are incomplete, unstable, or untested in production.
    /// All flags default to `false` — opt in explicitly in `.ta/config.toml`:
    ///
    /// ```toml
    /// [experimental]
    /// ollama_agent = true   # ta-agent-ollama (preview — untested against real models)
    /// sandbox = true        # macOS sandbox-exec / Linux bwrap (preview)
    /// ```
    #[serde(default)]
    pub experimental: ExperimentalConfig,
    /// Plugin registration (v0.14.4).
    ///
    /// Maps each daemon extension point to a plugin binary name or path.
    /// Unset slots use TA's built-in local defaults. See `docs/PLUGIN-AUTHORING.md`.
    ///
    /// ```toml
    /// [plugins]
    /// # auth = "ta-auth-oidc"
    /// # workspace = "ta-workspace-s3"
    /// # review_queue = "ta-review-jira"
    /// # audit_storage = "ta-audit-splunk"
    /// ```
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// Inbound webhook configuration (v0.14.8.3).
    ///
    /// Configure endpoints for receiving VCS events from GitHub, Perforce, and
    /// custom git hooks. Triggers matched workflow steps when events arrive.
    ///
    /// ```toml
    /// [webhooks.github]
    /// secret = "your-webhook-secret"
    ///
    /// [webhooks.vcs]
    /// secret = ""   # optional HMAC secret for remote senders
    ///
    /// [webhooks.relay]
    /// endpoint = "https://relay.secureautonomy.dev"
    /// secret = "your-relay-secret"
    /// ```
    #[serde(default)]
    pub webhooks: WebhooksConfig,
}

/// Top-level inbound webhook configuration (v0.14.8.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebhooksConfig {
    /// GitHub webhook configuration.
    pub github: GitHubWebhookConfig,
    /// Generic VCS webhook configuration.
    pub vcs: VcsWebhookConfig,
    /// SA cloud webhook relay configuration (design + stub).
    pub relay: Option<WebhookRelayConfig>,
}

/// GitHub inbound webhook configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubWebhookConfig {
    /// HMAC secret configured in the GitHub webhook settings.
    /// Required for `X-Hub-Signature-256` validation.
    /// If empty, signature validation is skipped (NOT recommended for production).
    pub secret: String,
}

/// Generic VCS inbound webhook configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VcsWebhookConfig {
    /// Optional HMAC secret for authenticating requests from non-localhost senders.
    /// If empty and the request originates from a non-loopback address, the
    /// request is rejected with 401.
    pub secret: String,
}

/// SA cloud webhook relay configuration (v0.14.8.3 — design + stub).
///
/// The relay is an SA-hosted publicly-accessible HTTPS endpoint that
/// tunnels VCS events to the local TA daemon. The local daemon registers
/// with the relay at startup (when `endpoint` is non-empty) and receives
/// forwarded events over a long-poll connection.
///
/// Implementation is SA's responsibility; this struct defines the protocol
/// contract so SA can build against it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRelayConfig {
    /// SA relay endpoint URL.
    pub endpoint: String,
    /// Shared secret for authenticating relay → daemon messages.
    pub secret: String,
    /// How long (in seconds) the daemon keeps the long-poll connection alive
    /// before reconnecting. Default: 30.
    #[serde(default = "default_relay_poll_secs")]
    pub poll_secs: u64,
}

fn default_relay_poll_secs() -> u64 {
    30
}

/// Shell Q&A agent configuration (v0.11.4.2).
///
/// Controls the persistent agent subprocess that handles natural language
/// questions in `ta shell`. Instead of spawning a new agent per question,
/// this keeps a single long-running process for the shell session's lifetime.
///
/// ```toml
/// [shell.qa_agent]
/// auto_start = true
/// agent = "claude-code"
/// idle_timeout_secs = 300
/// inject_memory = true
///
/// [shell.ui]
/// cursor_color = "#ffffff"
/// cursor_style = "block"
/// no_heartbeat_alert_secs = 30
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellQaConfig {
    /// Nested Q&A agent config.
    #[serde(default)]
    pub qa_agent: QaAgentConfig,
    /// Web shell UI configuration (v0.11.7).
    #[serde(default)]
    pub ui: ShellUiConfig,
}

/// Web shell UI configuration (v0.11.7).
///
/// Controls visual elements of the web shell (cursor style, alert thresholds).
///
/// ```toml
/// [shell.ui]
/// cursor_color = "#ffffff"
/// cursor_style = "block"
/// no_heartbeat_alert_secs = 30
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellUiConfig {
    /// CSS color for the shell input cursor (default: "#ffffff").
    #[serde(default = "default_cursor_color")]
    pub cursor_color: String,
    /// Cursor style: "block", "bar", or "underline" (default: "block").
    #[serde(default = "default_cursor_style")]
    pub cursor_style: String,
    /// Seconds without a heartbeat before showing the no-heartbeat alert
    /// in the working indicator (default: 30).
    #[serde(default = "default_no_heartbeat_alert_secs")]
    pub no_heartbeat_alert_secs: u32,
}

fn default_cursor_color() -> String {
    "#ffffff".to_string()
}

fn default_cursor_style() -> String {
    "block".to_string()
}

fn default_no_heartbeat_alert_secs() -> u32 {
    30
}

impl Default for ShellUiConfig {
    fn default() -> Self {
        Self {
            cursor_color: default_cursor_color(),
            cursor_style: default_cursor_style(),
            no_heartbeat_alert_secs: default_no_heartbeat_alert_secs(),
        }
    }
}

/// Persistent Q&A agent subprocess configuration (v0.11.4.2 item 6-10).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QaAgentConfig {
    /// Start agent on shell launch (default: true). If false, starts on first question.
    pub auto_start: bool,
    /// Which agent binary to use (default: "claude-code").
    pub agent: String,
    /// Kill after N seconds idle, restart on next question (default: 300 = 5 min).
    pub idle_timeout_secs: u64,
    /// Inject project memory context on start (default: true).
    pub inject_memory: bool,
    /// Maximum restart attempts per session before giving up (default: 3).
    pub max_restarts: u32,
    /// Graceful shutdown timeout in seconds (default: 5).
    pub shutdown_timeout_secs: u64,
}

impl Default for QaAgentConfig {
    fn default() -> Self {
        Self {
            auto_start: true,
            agent: "claude-code".to_string(),
            idle_timeout_secs: 300,
            inject_memory: true,
            max_restarts: 3,
            shutdown_timeout_secs: 5,
        }
    }
}

/// Operations configuration section (v0.11.2.3+).
///
/// ```toml
/// [operations]
/// heartbeat_interval_secs = 10
/// watchdog_interval_secs = 30
/// zombie_transition_delay_secs = 60
/// stale_question_threshold_secs = 3600
/// prompt_dismiss_after_output_secs = 5
/// prompt_verify_timeout_secs = 10
/// goal_log_interval_secs = 300
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationsConfig {
    /// Heartbeat interval in seconds for long-running commands (default: 10).
    #[serde(default)]
    pub heartbeat_interval_secs: Option<u32>,

    /// How often (in seconds) to emit a structured "still running" log for in-flight goals
    /// (v0.12.6). Helps diagnose stuck agents from logs alone. Default: 300 (5 minutes).
    #[serde(default)]
    pub goal_log_interval_secs: Option<u32>,

    /// Watchdog check cycle in seconds (default: 30). Set to 0 to disable.
    #[serde(default = "default_watchdog_interval")]
    pub watchdog_interval_secs: u32,

    /// Auto-heal policy configuration (v0.13.1).
    #[serde(default)]
    pub auto_heal: AutoHealConfig,

    /// Seconds to wait after detecting a dead process before transitioning
    /// the goal state (default: 60). Prevents false positives from brief
    /// process restarts.
    #[serde(default = "default_zombie_delay")]
    pub zombie_transition_delay_secs: u64,

    /// Seconds a question can be pending before it's flagged as stale (default: 3600 = 1h).
    #[serde(default = "default_stale_question_threshold")]
    pub stale_question_threshold_secs: u64,

    /// Seconds to wait after a detected prompt before auto-dismissing it if
    /// the agent continues producing output (default: 5). A real prompt means
    /// the agent stopped — continued output means false positive.
    #[serde(default = "default_prompt_dismiss_after_output")]
    pub prompt_dismiss_after_output_secs: u64,

    /// Seconds to wait for Q&A agent prompt verification before giving up
    /// (default: 10). If the Q&A agent doesn't respond in time, the prompt
    /// stays visible (fail-open).
    #[serde(default = "default_prompt_verify_timeout")]
    pub prompt_verify_timeout_secs: u64,

    /// Maximum seconds a goal may spend in `Finalizing` state with a dead
    /// `ta run` process before the watchdog declares it stuck and transitions
    /// to `Failed` (v0.13.17).
    ///
    /// Default: 1800s (30 min). If the `ta run` process is still alive, the
    /// watchdog never fires — this timeout only catches interrupted builds.
    /// Increase for very large workspaces where the file diff takes longer.
    #[serde(default = "default_finalize_timeout")]
    pub finalize_timeout_secs: u64,
}

/// Auto-heal policy: which low-risk corrective actions the daemon can take without asking.
///
/// ```toml
/// [operations.auto_heal]
/// enabled = true
/// allowed = [
///   "restart_crashed_plugin",
///   "transition_zombie_to_failed",
///   "clean_applied_staging",
/// ]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoHealConfig {
    /// Whether auto-heal is enabled (default: false — require explicit opt-in).
    pub enabled: bool,
    /// Action keys the daemon can execute without human approval.
    /// All other corrective actions are surfaced for human review.
    ///
    /// Well-known keys:
    /// - `"restart_crashed_plugin"` — restart a plugin that exited unexpectedly
    /// - `"transition_zombie_to_failed"` — mark dead-process goals as failed
    /// - `"clean_applied_staging"` — remove staging for successfully applied goals
    pub allowed: Vec<String>,
}

fn default_watchdog_interval() -> u32 {
    30
}

fn default_zombie_delay() -> u64 {
    60
}

fn default_stale_question_threshold() -> u64 {
    3600
}

fn default_prompt_dismiss_after_output() -> u64 {
    5
}

fn default_prompt_verify_timeout() -> u64 {
    10
}

fn default_finalize_timeout() -> u64 {
    1800
}

impl Default for OperationsConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: None,
            goal_log_interval_secs: None,
            watchdog_interval_secs: default_watchdog_interval(),
            zombie_transition_delay_secs: default_zombie_delay(),
            stale_question_threshold_secs: default_stale_question_threshold(),
            prompt_dismiss_after_output_secs: default_prompt_dismiss_after_output(),
            prompt_verify_timeout_secs: default_prompt_verify_timeout(),
            finalize_timeout_secs: default_finalize_timeout(),
            auto_heal: AutoHealConfig::default(),
        }
    }
}

/// Sandbox configuration section in daemon.toml (v0.10.16).
///
/// ```toml
/// [sandbox]
/// enabled = true
/// config_path = ".ta/sandbox.toml"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxSection {
    /// Whether sandbox command validation is active.
    pub enabled: bool,
    /// Path to a sandbox config file (relative to project root).
    /// If not set, uses the built-in `SandboxConfig::default()`.
    pub config_path: Option<String>,
}

/// Lifecycle compaction policy (v0.13.1).
///
/// Compaction ages applied/closed goals from "fat" storage (staging copies,
/// draft packages) down to slim audit-safe summaries, while the
/// `goal-history.jsonl` ledger preserves the essential facts.
///
/// ```toml
/// [lifecycle.compaction]
/// enabled = true
/// compact_after_days = 30
/// discard = ["staging_copy", "draft_package"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    /// Whether automatic compaction is enabled (default: true).
    pub enabled: bool,
    /// Age (days) after which applied/closed goals are eligible for compaction (default: 30).
    pub compact_after_days: u32,
    /// What to discard on compaction. Valid values: "staging_copy", "draft_package".
    pub discard: Vec<String>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            compact_after_days: 30,
            discard: vec!["staging_copy".to_string(), "draft_package".to_string()],
        }
    }
}

/// Lifecycle management configuration section (v0.13.1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LifecycleConfig {
    /// Compaction policy for applied/closed goals.
    #[serde(default)]
    pub compaction: CompactionConfig,
}

/// Power & sleep management configuration (v0.13.1.1).
///
/// Controls how the daemon behaves when the host machine sleeps or enters
/// low-power mode. Prevents false no-heartbeat alerts on wake and keeps
/// agent goals running through sleep/wake cycles.
///
/// ```toml
/// [power]
/// wake_grace_secs = 60
/// prevent_sleep_during_active_goals = true
/// prevent_app_nap = true
/// connectivity_check_url = "https://api.anthropic.com"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerConfig {
    /// Seconds to extend heartbeat deadlines after waking from sleep (default: 60).
    /// Prevents false no-heartbeat alerts before the agent has had a chance to
    /// resume and emit its first post-wake heartbeat.
    #[serde(default = "default_wake_grace_secs")]
    pub wake_grace_secs: u64,

    /// Whether to hold a power assertion while goals are running (default: true).
    ///
    /// macOS: holds `IOPMAssertionTypePreventUserIdleSystemSleep` via caffeinate.
    /// Linux: holds `systemd-inhibit --what=idle:sleep`.
    /// Windows: calls `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`.
    #[serde(default = "default_true")]
    pub prevent_sleep_during_active_goals: bool,

    /// Whether to suppress macOS App Nap for the daemon process (default: true).
    /// App Nap throttles background processes not doing visible UI work.
    /// When `prevent_sleep_during_active_goals` is true, App Nap is also suppressed
    /// while goals are running via the same caffeinate invocation.
    #[serde(default = "default_true")]
    pub prevent_app_nap: bool,

    /// URL to ping for connectivity check after waking from sleep (default: Anthropic API).
    /// If unreachable, `ApiConnectionLost` event is emitted and the shell shows a
    /// "waiting for network..." indicator. Restored when reachable again.
    #[serde(default = "default_connectivity_check_url")]
    pub connectivity_check_url: String,
}

fn default_wake_grace_secs() -> u64 {
    60
}

fn default_true() -> bool {
    true
}

fn default_connectivity_check_url() -> String {
    "https://api.anthropic.com".to_string()
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            wake_grace_secs: default_wake_grace_secs(),
            prevent_sleep_during_active_goals: true,
            prevent_app_nap: true,
            connectivity_check_url: default_connectivity_check_url(),
        }
    }
}

/// MCP transport mode (v0.13.2).
///
/// Controls how agents connect to the TA MCP server.
///
/// ```toml
/// [transport]
/// mode = "stdio"   # default — backward-compatible, agent spawned as child process
///
/// # Unix socket (faster IPC, works across container boundaries via mount):
/// mode = "unix"
/// unix_socket_path = ".ta/mcp.sock"
///
/// # TCP (remote agents, optional TLS):
/// mode = "tcp"
/// tcp_addr = "127.0.0.1:7800"
/// auth_token = "secret-bearer-token"
///
/// # Optional TLS for TCP transport:
/// [transport.tls]
/// cert_path = "certs/server.pem"
/// key_path = "certs/server.key"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransportMode {
    /// Standard I/O (default). The daemon is launched by the MCP client
    /// as a child process; communication is over stdin/stdout.
    #[default]
    Stdio,
    /// Unix domain socket. The daemon creates a socket file and listens
    /// for MCP clients to connect. Useful for container boundary crossing
    /// (mount the socket path into the container).
    Unix,
    /// TCP socket. The daemon listens on a configurable address/port.
    /// Supports optional TLS and bearer token authentication.
    Tcp,
}

/// TLS certificate configuration for TCP transport (v0.13.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    /// Path to PEM-encoded TLS certificate file (server certificate + chain).
    pub cert_path: std::path::PathBuf,
    /// Path to PEM-encoded private key file.
    pub key_path: std::path::PathBuf,
}

/// MCP transport configuration (v0.13.2).
///
/// Selects and configures the transport layer used by the TA MCP server.
/// The default is `stdio` for full backward compatibility.
///
/// Place under `[transport]` in `.ta/daemon.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TransportConfig {
    /// Which transport to use (default: "stdio").
    pub mode: TransportMode,

    /// Unix socket path for `mode = "unix"` (relative to project root or absolute).
    /// Default: `.ta/mcp.sock`
    #[serde(default = "default_unix_socket_path")]
    pub unix_socket_path: String,

    /// TCP listen address for `mode = "tcp"`.
    /// Default: `127.0.0.1:7800`
    #[serde(default = "default_tcp_addr")]
    pub tcp_addr: String,

    /// TLS certificate configuration for `mode = "tcp"`.
    /// When set, the TCP listener uses TLS (encrypted transport).
    /// When absent, TCP is plaintext (suitable for localhost/VPN-protected deployments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,

    /// Bearer token for connection authentication.
    ///
    /// When set, the MCP client must send `Bearer <token>\n` as the first
    /// line of the connection before any MCP messages. The server verifies
    /// the token before proceeding.
    ///
    /// - For `mode = "tcp"`: strongly recommended (especially without TLS).
    /// - For `mode = "unix"`: optional; filesystem permissions provide isolation.
    /// - For `mode = "stdio"`: ignored (process isolation provides security).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}

fn default_unix_socket_path() -> String {
    ".ta/mcp.sock".to_string()
}

fn default_tcp_addr() -> String {
    "127.0.0.1:7800".to_string()
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            mode: TransportMode::Stdio,
            unix_socket_path: default_unix_socket_path(),
            tcp_addr: default_tcp_addr(),
            tls: None,
            auth_token: None,
        }
    }
}

impl DaemonConfig {
    /// Load config from `.ta/daemon.toml`, falling back to defaults.
    pub fn load(project_root: &Path) -> Self {
        let config_path = project_root.join(".ta").join("daemon.toml");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(content) => match toml::from_str(&content) {
                    Ok(config) => return config,
                    Err(e) => {
                        tracing::warn!("Invalid daemon.toml, using defaults: {}", e);
                    }
                },
                Err(e) => {
                    tracing::warn!("Cannot read daemon.toml, using defaults: {}", e);
                }
            }
        }
        Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub port: u16,
    pub cors_origins: Vec<String>,
    /// Optional Unix domain socket path for local IPC (v0.10.16).
    /// Set to `""` to use the default `.ta/daemon.sock`, or provide a full path.
    /// Only used on Unix platforms; ignored on Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<String>,
    /// Optional TLS certificate path for the HTTP API server (v0.14.4).
    ///
    /// When set together with `tls_key_path`, the HTTP API is served over
    /// HTTPS. This is a no-op stub until a transport plugin activates it.
    /// Plugins read this field from `DaemonConfig::server` at startup.
    ///
    /// ```toml
    /// [server]
    /// tls_cert_path = "certs/server.pem"
    /// tls_key_path = "certs/server.key"
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_cert_path: Option<String>,
    /// Optional TLS private key path for the HTTP API server (v0.14.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_key_path: Option<String>,
    /// Serve the web UI at /ui (default: true). Set to false to disable the dashboard (v0.14.8).
    #[serde(default = "default_true")]
    pub web_ui: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: 7700,
            cors_origins: vec!["*".to_string()],
            socket_path: None,
            tls_cert_path: None,
            tls_key_path: None,
            web_ui: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub require_token: bool,
    pub local_bypass: bool,
    /// Named users for `LocalIdentityMiddleware` (v0.14.5).
    ///
    /// When non-empty, authenticated requests are matched against these entries
    /// by hashed bearer token. Use `LocalIdentityMiddleware` when you want
    /// user-level identity without an external SSO provider.
    ///
    /// ```toml
    /// [[auth.users]]
    /// user_id = "alice"
    /// display_name = "Alice Smith"
    /// roles = ["admin"]
    /// token_hash = "sha256:<hex SHA-256 of the bearer token>"
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<LocalUserEntry>,
    /// API key entries for `ApiKeyMiddleware` (v0.14.5).
    ///
    /// Suitable for CI pipelines and service accounts that present a
    /// `ta_key_`-prefixed bearer token.
    ///
    /// ```toml
    /// [[auth.api_keys]]
    /// label = "ci-pipeline"
    /// user_id = "ci"
    /// roles = ["read", "write"]
    /// key_hash = "sha256:<hex SHA-256 of the raw API key>"
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_keys: Vec<ApiKeyEntry>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            require_token: false,
            local_bypass: true,
            users: Vec::new(),
            api_keys: Vec::new(),
        }
    }
}

impl AuthConfig {
    /// Build the appropriate `AuthMiddleware` for the current config (v0.14.5).
    ///
    /// Selection order:
    /// 1. If `[[auth.api_keys]]` are configured, return `ApiKeyMiddleware`.
    /// 2. If `[[auth.users]]` are configured, return `LocalIdentityMiddleware`.
    /// 3. Otherwise, return `NoopAuthMiddleware` (local single-user default).
    pub fn build_middleware(&self) -> Box<dyn ta_extension::AuthMiddleware> {
        if !self.api_keys.is_empty() {
            Box::new(ta_extension::ApiKeyMiddleware::new(self.api_keys.clone()))
        } else if !self.users.is_empty() {
            Box::new(ta_extension::LocalIdentityMiddleware::new(
                self.users.clone(),
            ))
        } else {
            Box::new(ta_extension::NoopAuthMiddleware)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandConfig {
    /// Allowed command patterns. Empty = allow all.
    /// Use `denied` to explicitly block specific commands (deny takes precedence).
    pub allowed: Vec<String>,
    /// Denied command patterns. Deny always takes precedence over allowed.
    #[serde(default)]
    pub denied: Vec<String>,
    pub write_commands: Vec<String>,
    pub timeout_secs: u64,
    /// Commands that launch long-running processes (agents, dev loops).
    /// These get `long_timeout_secs` instead of `timeout_secs`.
    pub long_running: Vec<String>,
    /// Timeout for long-running commands like `ta run` and `ta dev` (default: 1 hour).
    pub long_timeout_secs: u64,
}

impl CommandConfig {
    /// Get the unified AccessFilter for this command config.
    pub fn access_filter(&self) -> AccessFilter {
        AccessFilter::new(self.allowed.clone(), self.denied.clone())
    }
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            allowed: vec![
                "*".to_string(), // All commands allowed by default for local human operator.
            ],
            denied: vec![],
            write_commands: vec![
                "ta draft approve *".to_string(),
                "ta draft deny *".to_string(),
                "ta draft apply *".to_string(),
                "ta goal start *".to_string(),
                "ta run *".to_string(),
                "ta dev *".to_string(),
                "ta init *".to_string(),
                "ta release run *".to_string(),
            ],
            timeout_secs: 120,
            long_running: vec![
                "ta run *".to_string(),
                "run *".to_string(),
                "ta dev *".to_string(),
                "dev *".to_string(),
                "ta plan from *".to_string(),
                "plan from *".to_string(),
                "ta release run *".to_string(),
                "release *".to_string(),
                "ta draft apply *".to_string(),
                "draft apply *".to_string(),
            ],
            long_timeout_secs: 3600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_sessions: usize,
    pub idle_timeout_secs: u64,
    pub default_agent: String,
    /// Default agent framework for goal execution (`ta run`).
    ///
    /// Used when no `--agent` flag is provided and no workflow specifies a framework.
    /// Resolution order: goal --agent flag → workflow agent_framework → this field → "claude-code".
    ///
    /// Example in daemon.toml:
    ///   [agent]
    ///   default_framework = "codex"
    #[serde(default = "default_framework")]
    pub default_framework: String,
    /// Framework to use for automated QA / review goals (e.g., v0.13.7 workflow steps).
    ///
    /// When a workflow's stage specifies `role.type = "qa"` and no explicit framework is
    /// configured for that role, this framework is used.
    ///
    /// Example: `qa_framework = "qwen-coder"` runs QA checks on a fast local model.
    #[serde(default = "default_qa_framework")]
    pub qa_framework: String,
    /// Agent for shell Q&A sessions (natural language questions in `ta shell`).
    /// Defaults to `"claude-code"` — must be a prompt-capable agent, not a framework.
    /// Separate from `default_agent` which is used for goal execution (`ta run`).
    #[serde(default = "default_qa_agent")]
    pub qa_agent: String,
    /// Timeout for agent prompt responses (default: 300s / 5 minutes).
    /// Agent LLM calls can take minutes — this is separate from the command timeout.
    pub timeout_secs: u64,
    /// Agent-level tool access control (v0.10.16).
    /// Restricts which MCP tools agents can invoke.
    #[serde(default)]
    pub tool_access: AgentToolAccess,
    /// Maximum concurrent parallel agent sessions in the web shell (v0.11.5 item 16).
    /// Each /parallel command spawns one session. Default: 3.
    #[serde(default = "default_max_parallel_sessions")]
    pub max_parallel_sessions: usize,
    /// Idle timeout for parallel sessions in seconds (v0.11.5 item 16).
    /// Sessions exceeding this idle time are auto-closed. Default: 1800 (30 min).
    #[serde(default = "default_parallel_idle_timeout_secs")]
    pub parallel_idle_timeout_secs: u64,
}

fn default_framework() -> String {
    "claude-code".to_string()
}

fn default_qa_framework() -> String {
    "claude-code".to_string()
}

fn default_qa_agent() -> String {
    "claude-code".to_string()
}

fn default_max_parallel_sessions() -> usize {
    3
}

fn default_parallel_idle_timeout_secs() -> u64 {
    1800
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_sessions: 3,
            idle_timeout_secs: 3600,
            default_agent: "claude-code".to_string(),
            default_framework: "claude-code".to_string(),
            qa_framework: "claude-code".to_string(),
            qa_agent: "claude-code".to_string(),
            timeout_secs: 300,
            tool_access: AgentToolAccess::default(),
            max_parallel_sessions: default_max_parallel_sessions(),
            parallel_idle_timeout_secs: default_parallel_idle_timeout_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingConfig {
    pub use_shell_config: bool,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            use_shell_config: true,
        }
    }
}

/// Routing configuration from `.ta/shell.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    #[serde(default)]
    pub routes: Vec<RouteEntry>,
    #[serde(default)]
    pub shortcuts: Vec<ShortcutEntry>,
    /// Known `ta` subcommands. Bare words matching these are auto-prefixed
    /// with `ta ` so users can type `run ...` instead of `ta run ...`.
    #[serde(default = "default_ta_subcommands")]
    pub ta_subcommands: Vec<String>,
}

fn default_ta_subcommands() -> Vec<String> {
    [
        "goal",
        "draft",
        "audit",
        "run",
        "session",
        "plan",
        "context",
        "credentials",
        "events",
        "token",
        "dev",
        "setup",
        "init",
        "agent",
        "adapter",
        "release",
        "office",
        "plugin",
        "workflow",
        "policy",
        "config",
        "gc",
        "status",
        "serve",
        "sync",
        "verify",
        "conversation",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            routes: vec![
                RouteEntry {
                    prefix: "ta ".to_string(),
                    command: "ta".to_string(),
                    args: vec![],
                    strip_prefix: true,
                },
                RouteEntry {
                    prefix: "git ".to_string(),
                    command: "git".to_string(),
                    args: vec![],
                    strip_prefix: true,
                },
                RouteEntry {
                    prefix: "vcs ".to_string(),
                    command: "git".to_string(),
                    args: vec![],
                    strip_prefix: true,
                },
                RouteEntry {
                    prefix: "!".to_string(),
                    command: "sh".to_string(),
                    args: vec!["-c".to_string()],
                    strip_prefix: true,
                },
            ],
            shortcuts: vec![
                ShortcutEntry {
                    r#match: "approve".to_string(),
                    expand: "ta draft approve".to_string(),
                    bare_only: false,
                },
                ShortcutEntry {
                    r#match: "deny".to_string(),
                    expand: "ta draft deny".to_string(),
                    bare_only: false,
                },
                ShortcutEntry {
                    r#match: "view".to_string(),
                    expand: "ta draft view".to_string(),
                    bare_only: false,
                },
                ShortcutEntry {
                    r#match: "apply".to_string(),
                    expand: "ta draft apply".to_string(),
                    bare_only: false,
                },
                ShortcutEntry {
                    r#match: "status".to_string(),
                    expand: "ta status".to_string(),
                    bare_only: false,
                },
                ShortcutEntry {
                    r#match: "plan".to_string(),
                    expand: "ta plan list".to_string(),
                    bare_only: true,
                },
                ShortcutEntry {
                    r#match: "goals".to_string(),
                    expand: "ta goal list".to_string(),
                    bare_only: true,
                },
                ShortcutEntry {
                    r#match: "drafts".to_string(),
                    expand: "ta draft list".to_string(),
                    bare_only: true,
                },
                ShortcutEntry {
                    r#match: "release".to_string(),
                    expand: "ta release run --yes".to_string(),
                    bare_only: true,
                },
            ],
            ta_subcommands: default_ta_subcommands(),
        }
    }
}

impl ShellConfig {
    /// Load from `.ta/shell.toml`, falling back to defaults.
    pub fn load(project_root: &Path) -> Self {
        let config_path = project_root.join(".ta").join("shell.toml");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(content) => match toml::from_str(&content) {
                    Ok(config) => return config,
                    Err(e) => {
                        tracing::warn!("Invalid shell.toml, using defaults: {}", e);
                    }
                },
                Err(e) => {
                    tracing::warn!("Cannot read shell.toml, using defaults: {}", e);
                }
            }
        }
        Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    pub prefix: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub strip_prefix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortcutEntry {
    pub r#match: String,
    pub expand: String,
    /// When true, only fire for a bare word with no trailing text.
    /// If the user types `<match> <rest>`, routing falls through to
    /// `ta_subcommands`, building `ta <match> <rest>` instead.
    /// Use for shortcuts that expand to a terminal command (e.g. `ta plan list`)
    /// where the user may also type explicit subcommands (`plan status`).
    #[serde(default)]
    pub bare_only: bool,
}

/// Channel delivery configuration for external question routing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelsConfig {
    /// Default channels to deliver questions to when no routing hints are specified.
    /// Empty means questions are only available via the HTTP API / ta shell.
    pub default_channels: Vec<String>,
    pub slack: Option<SlackChannelConfig>,
    pub discord: Option<DiscordChannelConfig>,
    pub email: Option<EmailChannelConfig>,
    /// External channel plugins (v0.10.2).
    /// Each entry registers an out-of-process plugin via JSON-over-stdio or HTTP.
    #[serde(default)]
    pub external: Vec<ExternalChannelEntry>,
    /// Global channel access control (v0.10.16).
    /// Restricts which users/roles can interact with TA through channels.
    #[serde(default)]
    pub access_control: ChannelAccessControl,
    /// Discord listener auto-launch configuration (v0.12.1).
    ///
    /// When set, the daemon auto-starts `ta-channel-discord --listen` and
    /// monitors its health, restarting on crash.
    ///
    /// ```toml
    /// [channels.discord_listener]
    /// enabled = true
    /// # Optional: override the binary name (default: "ta-channel-discord")
    /// # binary = "ta-channel-discord"
    /// # Optional: extra env vars for the listener process
    /// # env = { TA_DISCORD_PREFIX = "bot " }
    /// ```
    #[serde(default)]
    pub discord_listener: DiscordListenerConfig,
}

/// Configuration for the daemon-managed Discord listener process (v0.12.1).
///
/// When `enabled = true` and `"discord"` is in `default_channels`, the daemon
/// spawns `ta-channel-discord --listen` and restarts it if it crashes.
///
/// The listener process inherits the daemon's environment, so
/// `TA_DISCORD_TOKEN` and `TA_DISCORD_CHANNEL_ID` must be set in the
/// daemon's environment (or in the system service unit file).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordListenerConfig {
    /// Whether to auto-start the Discord listener. Default: false (opt-in).
    pub enabled: bool,
    /// Binary name or path. Defaults to `"ta-channel-discord"` (must be on PATH
    /// or in `.ta/plugins/channels/discord/`).
    #[serde(default = "default_discord_binary")]
    pub binary: String,
    /// Seconds between restart attempts after a crash. Default: 10.
    #[serde(default = "default_restart_delay_secs")]
    pub restart_delay_secs: u64,
    /// Maximum consecutive restarts before giving up. 0 = unlimited.
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
}

fn default_discord_binary() -> String {
    "ta-channel-discord".to_string()
}

fn default_restart_delay_secs() -> u64 {
    10
}

fn default_max_restarts() -> u32 {
    0 // unlimited by default
}

impl Default for DiscordListenerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            binary: default_discord_binary(),
            restart_delay_secs: default_restart_delay_secs(),
            max_restarts: default_max_restarts(),
        }
    }
}

/// Inline external channel plugin configuration in daemon.toml.
///
/// ```toml
/// [[channels.external]]
/// name = "teams"
/// command = "ta-channel-teams"
/// protocol = "json-stdio"
///
/// [[channels.external]]
/// name = "pagerduty"
/// protocol = "http"
/// deliver_url = "https://my-service.com/ta/deliver"
/// auth_token_env = "TA_PAGERDUTY_TOKEN"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalChannelEntry {
    /// Plugin name (used for routing, e.g., "teams", "pagerduty").
    pub name: String,
    /// Command to spawn (for json-stdio protocol).
    #[serde(default)]
    pub command: Option<String>,
    /// Additional arguments for the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Protocol: "json-stdio" or "http".
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// URL to POST questions to (for http protocol).
    #[serde(default)]
    pub deliver_url: Option<String>,
    /// Environment variable name containing an auth token.
    #[serde(default)]
    pub auth_token_env: Option<String>,
    /// Timeout in seconds (default: 30).
    #[serde(default = "default_plugin_timeout")]
    pub timeout_secs: u64,
    /// Per-channel access control (v0.10.16). Overrides global channel access control.
    #[serde(default)]
    pub access_control: Option<ChannelAccessControl>,
}

fn default_protocol() -> String {
    "json-stdio".to_string()
}

fn default_plugin_timeout() -> u64 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackChannelConfig {
    pub bot_token: String,
    pub channel_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordChannelConfig {
    pub bot_token: String,
    pub channel_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailChannelConfig {
    pub send_endpoint: String,
    pub api_key: String,
    pub from_address: String,
    pub to_address: String,
}

/// Channel-level access control (v0.10.16, item 12).
///
/// Restricts which users and roles can interact with TA through external
/// channels (Slack, Discord, email, etc.). Uses the same deny-takes-precedence
/// semantics as `AccessFilter`.
///
/// ```toml
/// [channels.access_control]
/// allowed_users = ["U12345", "U67890"]
/// denied_users = ["U99999"]
/// allowed_roles = ["admin", "reviewer"]
/// denied_roles = ["guest"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelAccessControl {
    /// Users explicitly allowed. Empty = allow all (subject to deny list).
    pub allowed_users: Vec<String>,
    /// Users explicitly denied (takes precedence over allowed).
    pub denied_users: Vec<String>,
    /// Roles explicitly allowed. Empty = allow all (subject to deny list).
    pub allowed_roles: Vec<String>,
    /// Roles explicitly denied (takes precedence over allowed).
    pub denied_roles: Vec<String>,
}

impl ChannelAccessControl {
    /// Check whether a user is permitted based on their user_id and roles.
    ///
    /// Logic (deny takes precedence):
    ///   1. If user_id matches any denied_users → false
    ///   2. If any role matches denied_roles → false
    ///   3. If allowed_users is non-empty and user_id is not in it → false
    ///   4. If allowed_roles is non-empty and no role matches → false
    ///   5. Otherwise → true
    pub fn permits(&self, user_id: &str, roles: &[String]) -> bool {
        // Deny takes precedence.
        if self.denied_users.iter().any(|u| u == user_id) {
            return false;
        }
        if roles.iter().any(|r| self.denied_roles.contains(r)) {
            return false;
        }
        // Check allowed users.
        if !self.allowed_users.is_empty() && !self.allowed_users.iter().any(|u| u == user_id) {
            return false;
        }
        // Check allowed roles.
        if !self.allowed_roles.is_empty() && !roles.iter().any(|r| self.allowed_roles.contains(r)) {
            return false;
        }
        true
    }

    /// Returns true if no access control is configured (all users/roles permitted).
    pub fn is_unrestricted(&self) -> bool {
        self.allowed_users.is_empty()
            && self.denied_users.is_empty()
            && self.allowed_roles.is_empty()
            && self.denied_roles.is_empty()
    }
}

/// Agent-level tool access control (v0.10.16, item 13).
///
/// Configures which MCP tools an agent is allowed or denied from using.
/// Applied per-agent via agent config YAML or daemon.toml.
///
/// ```toml
/// [agent.tool_access]
/// allowed_tools = ["ta_fs_read", "ta_fs_list", "ta_context"]
/// denied_tools = ["ta_fs_write"]
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentToolAccess {
    /// MCP tools the agent is allowed to call. Empty = allow all.
    pub allowed_tools: Vec<String>,
    /// MCP tools the agent is denied from calling. Deny takes precedence.
    pub denied_tools: Vec<String>,
}

impl AgentToolAccess {
    /// Create an access filter from the tool access configuration.
    #[allow(dead_code)] // Public API for gateway tool validation wiring (v0.11+).
    pub fn as_filter(&self) -> AccessFilter {
        AccessFilter::new(self.allowed_tools.clone(), self.denied_tools.clone())
    }
}

/// Token record stored in `.ta/daemon-tokens.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    pub token: String,
    pub scope: TokenScope,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub label: Option<String>,
}

/// Token permission scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    Read,
    Write,
    Admin,
}

impl TokenScope {
    pub fn allows_write(&self) -> bool {
        matches!(self, Self::Write | Self::Admin)
    }

    #[allow(dead_code)]
    pub fn allows_admin(&self) -> bool {
        matches!(self, Self::Admin)
    }
}

/// Manages bearer tokens stored in `.ta/daemon-tokens.json`.
pub struct TokenStore {
    tokens_path: PathBuf,
}

impl TokenStore {
    pub fn new(project_root: &Path) -> Self {
        Self {
            tokens_path: project_root.join(".ta").join("daemon-tokens.json"),
        }
    }

    /// Load all tokens from disk.
    pub fn load(&self) -> Vec<TokenRecord> {
        if !self.tokens_path.exists() {
            return vec![];
        }
        match std::fs::read_to_string(&self.tokens_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    #[allow(dead_code)]
    pub fn save(&self, tokens: &[TokenRecord]) -> std::io::Result<()> {
        if let Some(parent) = self.tokens_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(tokens)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(&self.tokens_path, json)
    }

    #[allow(dead_code)]
    pub fn create(&self, scope: TokenScope, label: Option<String>) -> std::io::Result<TokenRecord> {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let bytes: [u8; 32] = rng.gen();
        let token = format!("ta_{}", hex_encode(&bytes));

        let record = TokenRecord {
            token,
            scope,
            created_at: chrono::Utc::now(),
            label,
        };

        let mut tokens = self.load();
        tokens.push(record.clone());
        self.save(&tokens)?;

        Ok(record)
    }

    /// Validate a token string and return its record if valid.
    pub fn validate(&self, token: &str) -> Option<TokenRecord> {
        self.load().into_iter().find(|t| t.token == token)
    }

    #[allow(dead_code)]
    pub fn revoke(&self, token: &str) -> std::io::Result<bool> {
        let mut tokens = self.load();
        let len_before = tokens.len();
        tokens.retain(|t| t.token != token);
        if tokens.len() < len_before {
            self.save(&tokens)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Experimental feature flags — all default `false`.
///
/// Add `[experimental]` to `.ta/config.toml` to opt in:
///
/// ```toml
/// [experimental]
/// ollama_agent = true
/// sandbox = true
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    /// Enable the `ta-agent-ollama` agent backend (v0.13.16 preview).
    ///
    /// ta-agent-ollama is an experimental local model agent. Tool call
    /// parsing and CoT fallback behavior have not been validated against
    /// real Ollama instances. Expect rough edges.
    pub ollama_agent: bool,

    /// Enable macOS sandbox-exec / Linux bwrap sandbox enforcement (v0.14.0 preview).
    ///
    /// Sandbox profiles are coarse and may block legitimate agent operations
    /// (e.g., network to localhost:11434 for Ollama). Test thoroughly before
    /// enabling in workflows where agent actions must reach local services.
    pub sandbox: bool,
}

impl ExperimentalConfig {
    /// Returns true if any experimental feature is enabled.
    pub fn any_enabled(&self) -> bool {
        self.ollama_agent || self.sandbox
    }
}

#[allow(dead_code)]
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_daemon_config() {
        let config = DaemonConfig::default();
        assert_eq!(config.server.port, 7700);
        assert_eq!(config.server.bind, "127.0.0.1");
        assert!(!config.auth.require_token);
        assert!(config.auth.local_bypass);
        assert_eq!(config.commands.timeout_secs, 120);
        assert_eq!(config.agent.max_sessions, 3);
    }

    #[test]
    fn default_shell_config() {
        let config = ShellConfig::default();
        assert!(!config.routes.is_empty());
        assert!(!config.shortcuts.is_empty());
        assert_eq!(config.routes[0].prefix, "ta ");
        assert_eq!(config.shortcuts[0].r#match, "approve");
        assert!(config.ta_subcommands.contains(&"run".to_string()));
        assert!(config.ta_subcommands.contains(&"dev".to_string()));
        assert!(config.ta_subcommands.contains(&"goal".to_string()));
    }

    #[test]
    fn daemon_config_roundtrip() {
        let config = DaemonConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: DaemonConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.server.port, config.server.port);
    }

    #[test]
    fn shell_config_roundtrip() {
        let config = ShellConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: ShellConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.routes.len(), config.routes.len());
    }

    #[test]
    fn token_scope_permissions() {
        assert!(!TokenScope::Read.allows_write());
        assert!(TokenScope::Write.allows_write());
        assert!(TokenScope::Admin.allows_write());
        assert!(!TokenScope::Read.allows_admin());
        assert!(!TokenScope::Write.allows_admin());
        assert!(TokenScope::Admin.allows_admin());
    }

    #[test]
    fn token_store_create_validate_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        let store = TokenStore::new(dir.path());

        // Create a token.
        let record = store.create(TokenScope::Read, Some("test".into())).unwrap();
        assert!(record.token.starts_with("ta_"));

        // Validate it.
        let found = store.validate(&record.token);
        assert!(found.is_some());
        assert_eq!(found.unwrap().scope, TokenScope::Read);

        // Validate unknown token.
        assert!(store.validate("bogus").is_none());

        // Revoke it.
        assert!(store.revoke(&record.token).unwrap());
        assert!(store.validate(&record.token).is_none());
    }

    #[test]
    fn load_nonexistent_config() {
        let config = DaemonConfig::load(Path::new("/nonexistent"));
        assert_eq!(config.server.port, 7700);
    }

    #[test]
    fn operations_config_prompt_detection_defaults() {
        let config = OperationsConfig::default();
        assert_eq!(config.prompt_dismiss_after_output_secs, 5);
        assert_eq!(config.prompt_verify_timeout_secs, 10);
    }

    #[test]
    fn operations_config_prompt_detection_roundtrip() {
        let toml_str = r#"
            prompt_dismiss_after_output_secs = 3
            prompt_verify_timeout_secs = 15
        "#;
        let config: OperationsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.prompt_dismiss_after_output_secs, 3);
        assert_eq!(config.prompt_verify_timeout_secs, 15);
    }

    #[test]
    fn channel_access_control_unrestricted() {
        let acl = ChannelAccessControl::default();
        assert!(acl.is_unrestricted());
        assert!(acl.permits("anyone", &[]));
        assert!(acl.permits("user1", &["admin".to_string()]));
    }

    #[test]
    fn channel_access_control_denied_user() {
        let acl = ChannelAccessControl {
            denied_users: vec!["banned".to_string()],
            ..Default::default()
        };
        assert!(!acl.permits("banned", &[]));
        assert!(acl.permits("allowed", &[]));
    }

    #[test]
    fn channel_access_control_denied_role() {
        let acl = ChannelAccessControl {
            denied_roles: vec!["guest".to_string()],
            ..Default::default()
        };
        assert!(!acl.permits("user1", &["guest".to_string()]));
        assert!(acl.permits("user1", &["admin".to_string()]));
    }

    #[test]
    fn channel_access_control_allowed_users_only() {
        let acl = ChannelAccessControl {
            allowed_users: vec!["alice".to_string(), "bob".to_string()],
            ..Default::default()
        };
        assert!(acl.permits("alice", &[]));
        assert!(acl.permits("bob", &[]));
        assert!(!acl.permits("charlie", &[]));
    }

    #[test]
    fn channel_access_control_allowed_roles() {
        let acl = ChannelAccessControl {
            allowed_roles: vec!["reviewer".to_string()],
            ..Default::default()
        };
        assert!(acl.permits("user1", &["reviewer".to_string()]));
        assert!(!acl.permits("user1", &["viewer".to_string()]));
    }

    #[test]
    fn channel_access_deny_takes_precedence() {
        let acl = ChannelAccessControl {
            allowed_users: vec!["alice".to_string()],
            denied_users: vec!["alice".to_string()],
            ..Default::default()
        };
        // Deny takes precedence even when user is in allowed list.
        assert!(!acl.permits("alice", &[]));
    }

    #[test]
    fn agent_tool_access_default_unrestricted() {
        let ata = AgentToolAccess::default();
        let filter = ata.as_filter();
        assert!(filter.permits("ta_fs_read"));
        assert!(filter.permits("ta_fs_write"));
    }

    #[test]
    fn agent_tool_access_denied_tools() {
        let ata = AgentToolAccess {
            allowed_tools: vec![],
            denied_tools: vec!["ta_fs_write".to_string()],
        };
        let filter = ata.as_filter();
        assert!(filter.permits("ta_fs_read"));
        assert!(!filter.permits("ta_fs_write"));
    }

    #[test]
    fn agent_tool_access_allowed_only() {
        let ata = AgentToolAccess {
            allowed_tools: vec!["ta_fs_read".to_string(), "ta_context".to_string()],
            denied_tools: vec![],
        };
        let filter = ata.as_filter();
        assert!(filter.permits("ta_fs_read"));
        assert!(filter.permits("ta_context"));
        assert!(!filter.permits("ta_fs_write"));
    }

    #[test]
    fn sandbox_section_defaults() {
        let sandbox = SandboxSection::default();
        assert!(!sandbox.enabled);
        assert!(sandbox.config_path.is_none());
    }

    #[test]
    fn server_config_socket_path_serialization() {
        let config = ServerConfig {
            socket_path: Some(".ta/daemon.sock".to_string()),
            ..Default::default()
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("socket_path"));
        let parsed: ServerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.socket_path.as_deref(), Some(".ta/daemon.sock"));
    }

    #[test]
    fn power_config_defaults() {
        let config = PowerConfig::default();
        assert_eq!(config.wake_grace_secs, 60);
        assert!(config.prevent_sleep_during_active_goals);
        assert!(config.prevent_app_nap);
        assert_eq!(config.connectivity_check_url, "https://api.anthropic.com");
    }

    #[test]
    fn power_config_roundtrip() {
        let toml_str = r#"
            [power]
            wake_grace_secs = 120
            prevent_sleep_during_active_goals = false
            prevent_app_nap = true
            connectivity_check_url = "https://example.com"
        "#;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.power.wake_grace_secs, 120);
        assert!(!config.power.prevent_sleep_during_active_goals);
        assert!(config.power.prevent_app_nap);
        assert_eq!(config.power.connectivity_check_url, "https://example.com");
    }

    #[test]
    fn power_config_partial_override() {
        let toml_str = r#"
            [power]
            wake_grace_secs = 30
        "#;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.power.wake_grace_secs, 30);
        assert!(config.power.prevent_sleep_during_active_goals); // default
        assert!(config.power.prevent_app_nap); // default
    }

    #[test]
    fn server_config_no_socket_path_omitted() {
        let config = ServerConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        // socket_path should be omitted when None.
        assert!(!toml_str.contains("socket_path"));
    }

    #[test]
    fn shell_ui_config_defaults() {
        let config = ShellUiConfig::default();
        assert_eq!(config.cursor_color, "#ffffff");
        assert_eq!(config.cursor_style, "block");
        assert_eq!(config.no_heartbeat_alert_secs, 30);
    }

    #[test]
    fn shell_ui_config_roundtrip() {
        let toml_str = r##"
            [shell.ui]
            cursor_color = "#00d2ff"
            cursor_style = "bar"
            no_heartbeat_alert_secs = 60
        "##;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.shell.ui.cursor_color, "#00d2ff");
        assert_eq!(config.shell.ui.cursor_style, "bar");
        assert_eq!(config.shell.ui.no_heartbeat_alert_secs, 60);
    }

    #[test]
    fn shell_qa_config_defaults() {
        // v0.11.4.2 item 8: Default QA agent config.
        let config = ShellQaConfig::default();
        assert!(config.qa_agent.auto_start);
        assert_eq!(config.qa_agent.agent, "claude-code");
        assert_eq!(config.qa_agent.idle_timeout_secs, 300);
        assert!(config.qa_agent.inject_memory);
        assert_eq!(config.qa_agent.max_restarts, 3);
        assert_eq!(config.qa_agent.shutdown_timeout_secs, 5);
    }

    #[test]
    fn shell_qa_config_roundtrip() {
        // v0.11.4.2 item 8: Config survives TOML serialization.
        let toml_str = r#"
            [shell.qa_agent]
            auto_start = false
            agent = "codex"
            idle_timeout_secs = 600
            inject_memory = false
            max_restarts = 5
            shutdown_timeout_secs = 10
        "#;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.shell.qa_agent.auto_start);
        assert_eq!(config.shell.qa_agent.agent, "codex");
        assert_eq!(config.shell.qa_agent.idle_timeout_secs, 600);
        assert!(!config.shell.qa_agent.inject_memory);
        assert_eq!(config.shell.qa_agent.max_restarts, 5);
        assert_eq!(config.shell.qa_agent.shutdown_timeout_secs, 10);
    }

    #[test]
    fn shell_qa_config_partial_override() {
        // v0.11.4.2: Partial config should fill defaults for missing fields.
        let toml_str = r#"
            [shell.qa_agent]
            idle_timeout_secs = 120
        "#;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert!(config.shell.qa_agent.auto_start); // default
        assert_eq!(config.shell.qa_agent.agent, "claude-code"); // default
        assert_eq!(config.shell.qa_agent.idle_timeout_secs, 120); // overridden
    }

    #[test]
    fn webhook_config_roundtrip() {
        let toml_str = r#"
[webhooks.github]
secret = "gh-secret-123"

[webhooks.vcs]
secret = "vcs-secret-456"

[webhooks.relay]
endpoint = "https://relay.example.com"
secret = "relay-secret"
"#;
        let config: DaemonConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.webhooks.github.secret, "gh-secret-123");
        assert_eq!(config.webhooks.vcs.secret, "vcs-secret-456");
        let relay = config.webhooks.relay.as_ref().unwrap();
        assert_eq!(relay.endpoint, "https://relay.example.com");
        assert_eq!(relay.poll_secs, 30);
    }
}
