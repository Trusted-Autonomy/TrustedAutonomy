// config.rs — Daemon configuration from `.ta/daemon.toml`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ta_policy::AccessFilter;

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
    /// Sandbox configuration for command validation (v0.10.16, item 3).
    /// When set, the orchestrator validates commands against the sandbox
    /// allowlist before execution.
    #[serde(default)]
    pub sandbox: Option<SandboxSection>,
    /// Operations configuration (v0.11.2.3): heartbeat, watchdog, etc.
    #[serde(default)]
    pub operations: Option<OperationsConfig>,
}

/// Operations configuration section (v0.11.2.3+).
///
/// ```toml
/// [operations]
/// heartbeat_interval_secs = 10
/// watchdog_interval_secs = 30
/// zombie_transition_delay_secs = 60
/// stale_question_threshold_secs = 3600
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationsConfig {
    /// Heartbeat interval in seconds for long-running commands (default: 10).
    #[serde(default)]
    pub heartbeat_interval_secs: Option<u32>,

    /// Watchdog check cycle in seconds (default: 30). Set to 0 to disable.
    #[serde(default = "default_watchdog_interval")]
    pub watchdog_interval_secs: u32,

    /// Seconds to wait after detecting a dead process before transitioning
    /// the goal state (default: 60). Prevents false positives from brief
    /// process restarts.
    #[serde(default = "default_zombie_delay")]
    pub zombie_transition_delay_secs: u64,

    /// Seconds a question can be pending before it's flagged as stale (default: 3600 = 1h).
    #[serde(default = "default_stale_question_threshold")]
    pub stale_question_threshold_secs: u64,
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

impl Default for OperationsConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: None,
            watchdog_interval_secs: default_watchdog_interval(),
            zombie_transition_delay_secs: default_zombie_delay(),
            stale_question_threshold_secs: default_stale_question_threshold(),
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: 7700,
            cors_origins: vec!["*".to_string()],
            socket_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub require_token: bool,
    pub local_bypass: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            require_token: false,
            local_bypass: true,
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
}

fn default_qa_agent() -> String {
    "claude-code".to_string()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_sessions: 3,
            idle_timeout_secs: 3600,
            default_agent: "claude-code".to_string(),
            qa_agent: "claude-code".to_string(),
            timeout_secs: 300,
            tool_access: AgentToolAccess::default(),
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
                },
                ShortcutEntry {
                    r#match: "deny".to_string(),
                    expand: "ta draft deny".to_string(),
                },
                ShortcutEntry {
                    r#match: "view".to_string(),
                    expand: "ta draft view".to_string(),
                },
                ShortcutEntry {
                    r#match: "apply".to_string(),
                    expand: "ta draft apply".to_string(),
                },
                ShortcutEntry {
                    r#match: "status".to_string(),
                    expand: "ta status".to_string(),
                },
                ShortcutEntry {
                    r#match: "plan".to_string(),
                    expand: "ta plan list".to_string(),
                },
                ShortcutEntry {
                    r#match: "goals".to_string(),
                    expand: "ta goal list".to_string(),
                },
                ShortcutEntry {
                    r#match: "drafts".to_string(),
                    expand: "ta draft list".to_string(),
                },
                ShortcutEntry {
                    r#match: "release".to_string(),
                    expand: "ta release run --yes".to_string(),
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
    fn server_config_no_socket_path_omitted() {
        let config = ServerConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        // socket_path should be omitted when None.
        assert!(!toml_str.contains("socket_path"));
    }
}
