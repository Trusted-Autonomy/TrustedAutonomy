use std::collections::HashMap;
use std::net::SocketAddr;

use crate::error::UnrealConnectorError;

/// Describes a running MCP backend process.
pub struct BackendHandle {
    pub pid: u32,
    pub socket_addr: SocketAddr,
}

/// Supported Unreal Engine MCP tools that any backend must advertise.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UnrealTool {
    /// Execute a Python script in UE5 Editor context.
    PythonExec,
    /// Query actors and metadata from a level.
    SceneQuery,
    /// List assets under a Content Browser path.
    AssetList,
    /// Submit an MRQ render job.
    MrqSubmit,
    /// Poll MRQ job completion.
    MrqStatus,
    /// Query Level Sequences available in the level.
    SequencerQuery,
    /// List available lighting presets in the level.
    LightingPresetList,
}

impl UnrealTool {
    pub fn mcp_name(&self) -> &'static str {
        match self {
            Self::PythonExec => "ue5_python_exec",
            Self::SceneQuery => "ue5_scene_query",
            Self::AssetList => "ue5_asset_list",
            Self::MrqSubmit => "ue5_mrq_submit",
            Self::MrqStatus => "ue5_mrq_status",
            Self::SequencerQuery => "ue5_sequencer_query",
            Self::LightingPresetList => "ue5_lighting_preset_list",
        }
    }
}

/// Trait implemented by each UE5 MCP backend (kvick, flopperam, special-agent).
/// All methods are synchronous — backends are local processes.
pub trait UnrealBackend: Send + Sync {
    /// Human-readable backend name (e.g., "kvick", "flopperam", "special-agent").
    fn name(&self) -> &str;

    /// Spawn the backend MCP server process and return a handle.
    /// Returns an error if the install_path is missing or the process fails.
    fn spawn(&self) -> Result<BackendHandle, UnrealConnectorError>;

    /// Return the set of MCP tools this backend supports.
    fn supported_tools(&self) -> Vec<UnrealTool>;

    /// Return the socket address this backend listens on.
    fn socket_addr(&self) -> SocketAddr;

    /// Return metadata about this backend for `ta connector list`.
    fn metadata(&self) -> HashMap<String, String>;
}
