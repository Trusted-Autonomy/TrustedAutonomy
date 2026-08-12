// config.rs — Credential vault configuration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Configuration for the credential vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialsConfig {
    /// Path to the vault file (default: `.ta/credentials.json`).
    pub vault_path: PathBuf,

    /// Whether to try the OS keychain for the vault's encryption key before
    /// falling back to a chmod-0600 file (default: `true`). Set `false` to
    /// force file-based key custody — e.g. in tests (the keychain is a
    /// process/OS-global resource, not scoped to a test's tempdir, so using
    /// it there would make tests interfere with each other and with the
    /// developer's real keychain) or on headless servers with no keychain
    /// daemon available.
    #[serde(default = "default_use_keychain")]
    pub use_keychain: bool,
}

fn default_use_keychain() -> bool {
    true
}

impl CredentialsConfig {
    /// Create config with standard `.ta/` layout for a project.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        let ta_dir = project_root.as_ref().join(".ta");
        Self {
            vault_path: ta_dir.join("credentials.json"),
            use_keychain: true,
        }
    }
}

impl Default for CredentialsConfig {
    fn default() -> Self {
        Self {
            vault_path: PathBuf::from(".ta/credentials.json"),
            use_keychain: true,
        }
    }
}
