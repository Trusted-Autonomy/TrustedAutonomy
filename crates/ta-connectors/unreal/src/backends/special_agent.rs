use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

use crate::{
    backend::{BackendHandle, UnrealBackend, UnrealTool},
    config::UnrealConnectorConfig,
    error::UnrealConnectorError,
};

/// Backend wrapping `ArtisanGameworks/SpecialAgentPlugin` (71+ tools, environment-building).
/// Opt-in via `backend = "special-agent"` in config.
pub struct SpecialAgentBackend {
    install_path: String,
    socket: String,
}

impl SpecialAgentBackend {
    pub fn new(config: &UnrealConnectorConfig) -> Self {
        Self {
            install_path: config.backends.special_agent.install_path.clone(),
            socket: config.socket.clone(),
        }
    }
}

impl UnrealBackend for SpecialAgentBackend {
    fn name(&self) -> &str {
        "special-agent"
    }

    fn spawn(&self) -> Result<BackendHandle, UnrealConnectorError> {
        if self.install_path.is_empty() {
            return Err(UnrealConnectorError::NotInstalled(
                "special-agent".to_string(),
            ));
        }
        let install = std::path::Path::new(&self.install_path);
        if !install.exists() {
            return Err(UnrealConnectorError::NotInstalled(
                "special-agent".to_string(),
            ));
        }
        // SpecialAgentPlugin is a UE5 plugin — starts inside the Editor.
        let addr = SocketAddr::from_str(&self.socket)
            .map_err(|e| UnrealConnectorError::Config(e.to_string()))?;
        Ok(BackendHandle {
            pid: 0,
            socket_addr: addr,
        })
    }

    fn supported_tools(&self) -> Vec<UnrealTool> {
        // SpecialAgentPlugin exposes 71+ tools; we model the core five here.
        vec![
            UnrealTool::PythonExec,
            UnrealTool::SceneQuery,
            UnrealTool::AssetList,
            UnrealTool::MrqSubmit,
            UnrealTool::MrqStatus,
        ]
    }

    fn socket_addr(&self) -> SocketAddr {
        SocketAddr::from_str(&self.socket).unwrap_or_else(|_| "127.0.0.1:30100".parse().unwrap())
    }

    fn metadata(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(
            "source".to_string(),
            "github.com/ArtisanGameworks/SpecialAgentPlugin".to_string(),
        );
        m.insert("runtime".to_string(), "ue5-plugin".to_string());
        m.insert(
            "use_case".to_string(),
            "opt-in / 71+ tools / environment-building".to_string(),
        );
        m.insert("install_path".to_string(), self.install_path.clone());
        m
    }
}
