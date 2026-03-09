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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".to_string(),
            port: 7700,
            cors_origins: vec!["*".to_string()],
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
            timeout_secs: 30,
            long_running: vec![
                "ta run *".to_string(),
                "run *".to_string(),
                "ta dev *".to_string(),
                "dev *".to_string(),
                "ta plan from *".to_string(),
                "plan from *".to_string(),
                "ta release run *".to_string(),
                "release *".to_string(),
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
    /// Timeout for agent prompt responses (default: 300s / 5 minutes).
    /// Agent LLM calls can take minutes — this is separate from the command timeout.
    pub timeout_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_sessions: 3,
            idle_timeout_secs: 3600,
            default_agent: "claude-code".to_string(),
            timeout_secs: 300,
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
                    expand: "ta release run".to_string(),
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
        assert_eq!(config.commands.timeout_secs, 30);
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
}
