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
}

impl FlopperamBackend {
    pub fn new(config: &UnrealConnectorConfig) -> Self {
        Self {
            install_path: config.backends.flopperam.install_path.clone(),
            socket: config.socket.clone(),
        }
    }
}

impl UnrealBackend for FlopperamBackend {
    fn name(&self) -> &str {
        "flopperam"
    }

    fn spawn(&self) -> Result<BackendHandle, UnrealConnectorError> {
        // The flopperam backend is a UE5 C++ plugin — it runs inside the Unreal Editor.
        // The MCP server starts automatically when the Editor loads the plugin.
        // TA does not spawn a process; it returns a synthetic pid=0 handle.
        // The plugin source at install_path is only needed to copy the plugin into a UE5
        // project — it is not required for TA to connect to a running Editor session.
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
            UnrealTool::SequencerQuery,
            UnrealTool::LightingPresetList,
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
