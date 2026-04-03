use serde::{Deserialize, Serialize};

/// Top-level `[connectors.comfyui]` config block.
///
/// Example:
/// ```toml
/// [connectors.comfyui]
/// enabled = true
/// url = "http://localhost:8188"
/// output_dir = "/home/user/ComfyUI/output"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComfyUiConnectorConfig {
    /// Whether the ComfyUI connector is active.
    #[serde(default)]
    pub enabled: bool,

    /// Base URL of the ComfyUI REST API.
    #[serde(default = "default_url")]
    pub url: String,

    /// Filesystem path to ComfyUI's output directory (where generated files land).
    /// If empty, output watching is disabled.
    #[serde(default)]
    pub output_dir: String,
}

fn default_url() -> String {
    "http://localhost:8188".to_string()
}

impl Default for ComfyUiConnectorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: default_url(),
            output_dir: String::new(),
        }
    }
}

impl ComfyUiConnectorConfig {
    pub fn from_toml(s: &str) -> Result<Self, crate::error::ComfyUiError> {
        toml::from_str(s).map_err(|e| crate::error::ComfyUiError::Config(e.to_string()))
    }
}
