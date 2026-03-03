// config.rs — Credential vault configuration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Configuration for the credential vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialsConfig {
    /// Path to the vault file (default: `.ta/credentials.json`).
    pub vault_path: PathBuf,
}

impl CredentialsConfig {
    /// Create config with standard `.ta/` layout for a project.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        let ta_dir = project_root.as_ref().join(".ta");
        Self {
            vault_path: ta_dir.join("credentials.json"),
        }
    }
}

impl Default for CredentialsConfig {
    fn default() -> Self {
        Self {
            vault_path: PathBuf::from(".ta/credentials.json"),
        }
    }
}
