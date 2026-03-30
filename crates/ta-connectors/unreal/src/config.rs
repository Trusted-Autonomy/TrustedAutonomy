use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Config for a single named backend (kvick or flopperam).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendConfig {
    /// Path where the backend server is installed.
    #[serde(default)]
    pub install_path: String,
}

/// Per-backend overrides map (backend name → BackendConfig).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendsConfig {
    #[serde(default)]
    pub kvick: BackendConfig,
    #[serde(default)]
    pub flopperam: BackendConfig,
    #[serde(rename = "special-agent", default)]
    pub special_agent: BackendConfig,
}

/// Top-level `[connectors.unreal]` config block.
///
/// Example:
/// ```toml
/// [connectors.unreal]
/// enabled = true
/// backend = "flopperam"
/// ue_project_path = "/path/to/MyGame.uproject"
/// editor_path = ""          # auto-detect if empty
/// socket = "localhost:30100"
///
/// [connectors.unreal.backends.kvick]
/// install_path = "~/.ta/mcp-servers/unreal-kvick"
///
/// [connectors.unreal.backends.flopperam]
/// install_path = "~/.ta/mcp-servers/unreal-flopperam"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnrealConnectorConfig {
    /// Whether the Unreal connector is active.
    #[serde(default)]
    pub enabled: bool,

    /// Which backend MCP server to use: "kvick", "flopperam", or "special-agent".
    #[serde(default = "default_backend")]
    pub backend: String,

    /// Absolute path to the `.uproject` file.
    /// Used by backends that need to know the project location.
    #[serde(default)]
    pub ue_project_path: String,

    /// Path to the Unreal Editor executable.
    /// If empty, auto-detection is attempted via common install locations.
    #[serde(default)]
    pub editor_path: String,

    /// Socket address the MCP backend listens on.
    #[serde(default = "default_socket")]
    pub socket: String,

    /// Per-backend installation paths and options.
    #[serde(default)]
    pub backends: BackendsConfig,
}

fn default_backend() -> String {
    "flopperam".to_string()
}

fn default_socket() -> String {
    "localhost:30100".to_string()
}

impl Default for UnrealConnectorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: default_backend(),
            ue_project_path: String::new(),
            editor_path: String::new(),
            socket: default_socket(),
            backends: BackendsConfig::default(),
        }
    }
}

impl UnrealConnectorConfig {
    /// Parse from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self, crate::error::UnrealConnectorError> {
        toml::from_str(s).map_err(|e| crate::error::UnrealConnectorError::Config(e.to_string()))
    }

    /// Resolve the install_path for the active backend (expands leading `~`).
    pub fn install_path_for_active_backend(&self) -> PathBuf {
        let raw = match self.backend.as_str() {
            "kvick" => &self.backends.kvick.install_path,
            "flopperam" => &self.backends.flopperam.install_path,
            "special-agent" => &self.backends.special_agent.install_path,
            _ => return PathBuf::new(),
        };
        expand_tilde(raw)
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_or_fallback() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn dirs_or_fallback() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}
