use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

use crate::{
    backend::{BackendHandle, UnrealBackend, UnrealTool},
    config::UnrealConnectorConfig,
    error::UnrealConnectorError,
};

/// Backend wrapping `kvick-games/UnrealMCP` (Python server, simple scene ops).
/// Default for POC/development.
pub struct KvickBackend {
    install_path: String,
    socket: String,
}

impl KvickBackend {
    pub fn new(config: &UnrealConnectorConfig) -> Self {
        Self {
            install_path: config.backends.kvick.install_path.clone(),
            socket: config.socket.clone(),
        }
    }
}

impl UnrealBackend for KvickBackend {
    fn name(&self) -> &str {
        "kvick"
    }

    fn spawn(&self) -> Result<BackendHandle, UnrealConnectorError> {
        if self.install_path.is_empty() {
            return Err(UnrealConnectorError::NotInstalled("kvick".to_string()));
        }
        let install = std::path::Path::new(&self.install_path);
        if !install.exists() {
            return Err(UnrealConnectorError::NotInstalled("kvick".to_string()));
        }
        let server_script = install.join("server.py");
        let child = std::process::Command::new("python3")
            .arg(&server_script)
            .spawn()
            .map_err(|e| UnrealConnectorError::SpawnFailed(e.to_string()))?;
        let addr = SocketAddr::from_str(&self.socket)
            .map_err(|e| UnrealConnectorError::Config(e.to_string()))?;
        Ok(BackendHandle {
            pid: child.id(),
            socket_addr: addr,
        })
    }

    fn supported_tools(&self) -> Vec<UnrealTool> {
        vec![
            UnrealTool::PythonExec,
            UnrealTool::SceneQuery,
            UnrealTool::AssetList,
        ]
    }

    fn socket_addr(&self) -> SocketAddr {
        SocketAddr::from_str(&self.socket).unwrap_or_else(|_| "127.0.0.1:30100".parse().unwrap())
    }

    fn metadata(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(
            "source".to_string(),
            "github.com/kvick-games/UnrealMCP".to_string(),
        );
        m.insert("runtime".to_string(), "python3".to_string());
        m.insert("use_case".to_string(), "POC / simple scene ops".to_string());
        m.insert("install_path".to_string(), self.install_path.clone());
        m
    }
}
