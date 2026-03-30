use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

use crate::{
    backend::{BackendHandle, UnrealBackend, UnrealTool},
    config::UnrealConnectorConfig,
    error::UnrealConnectorError,
};

/// Backend wrapping `flopperam/unreal-engine-mcp` (C++ UE5 plugin, full MRQ/Sequencer access).
/// Default for production.
pub struct FlopperamBackend {
    install_path: String,
    socket: String,
    ue_project_path: String,
}

impl FlopperamBackend {
    pub fn new(config: &UnrealConnectorConfig) -> Self {
        Self {
            install_path: config.backends.flopperam.install_path.clone(),
            socket: config.socket.clone(),
            ue_project_path: config.ue_project_path.clone(),
        }
    }
}

impl UnrealBackend for FlopperamBackend {
    fn name(&self) -> &str {
        "flopperam"
    }

    fn spawn(&self) -> Result<BackendHandle, UnrealConnectorError> {
        // The flopperam backend is a UE5 C++ plugin — it runs inside the Unreal Editor.
        // The MCP server is started when the Editor loads the plugin.
        // This spawn() call checks that the plugin is installed (install_path exists)
        // and that the UE project path is configured.
        if self.install_path.is_empty() {
            return Err(UnrealConnectorError::NotInstalled("flopperam".to_string()));
        }
        let install = std::path::Path::new(&self.install_path);
        if !install.exists() {
            return Err(UnrealConnectorError::NotInstalled("flopperam".to_string()));
        }
        if self.ue_project_path.is_empty() {
            return Err(UnrealConnectorError::Config(
                "ue_project_path must be set for flopperam backend".to_string(),
            ));
        }
        // The C++ plugin starts automatically when the Editor loads.
        // We return a synthetic "pid 0" handle — the Editor owns the process.
        let addr = SocketAddr::from_str(&self.socket)
            .map_err(|e| UnrealConnectorError::Config(e.to_string()))?;
        Ok(BackendHandle {
            pid: 0, // Editor-owned; no child process to track.
            socket_addr: addr,
        })
    }

    fn supported_tools(&self) -> Vec<UnrealTool> {
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
            "github.com/flopperam/unreal-engine-mcp".to_string(),
        );
        m.insert("runtime".to_string(), "ue5-plugin".to_string());
        m.insert(
            "use_case".to_string(),
            "production / MRQ + Sequencer".to_string(),
        );
        m.insert("install_path".to_string(), self.install_path.clone());
        m
    }
}
