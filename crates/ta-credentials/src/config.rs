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
    /// daemon available. `for_project()` resolves this through
    /// [`crate::custody::use_keychain`] for callers that don't build
    /// this struct by hand (e.g. the `ta credentials` CLI).
    #[serde(default = "default_use_keychain")]
    pub use_keychain: bool,
}

fn default_use_keychain() -> bool {
    true
}

impl CredentialsConfig {
    /// Create config with standard `.ta/` layout for a project.
    ///
    /// `use_keychain` comes from [`crate::custody::use_keychain`]: the
    /// `TA_NO_KEYCHAIN` environment variable (any value) forces file custody,
    /// then the per-user custody file (`ta credentials custody file|keychain`),
    /// then the keychain default. The OS keychain can
    /// require an interactive GUI permission prompt on first use (observed
    /// live on macOS: a fresh vault's encryption-key lookup blocks
    /// indefinitely with no visible error when nothing can answer that
    /// prompt), which hangs forever in any headless, scripted, or CI
    /// invocation of `ta credentials add`/`update` with no way to opt out
    /// before this. `TA_NO_KEYCHAIN` mirrors the existing `TA_IS_STAGING`
    /// convention (`ta-mcp-gateway`'s `GatewayConfig::for_project`) —
    /// presence, not value, is what's checked.
    ///
    /// Inside a cargo test binary the default is also `false` (see
    /// [`crate::encryption::keychain_guard_active`]): a test must never open
    /// the real OS keychain, whichever crate it lives in and whether or not
    /// `TA_NO_KEYCHAIN` happens to be set in the environment. Installed and
    /// `cargo run` binaries are unaffected.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        let ta_dir = project_root.as_ref().join(".ta");
        Self {
            vault_path: ta_dir.join("credentials.json"),
            use_keychain: crate::custody::use_keychain(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `for_project` caller in a test binary gets a file-key vault, so a
    /// dependent crate's test can never reach the real keychain through it.
    #[test]
    fn for_project_never_defaults_to_the_keychain_under_test() {
        let config = CredentialsConfig::for_project("/tmp/does-not-need-to-exist");
        assert!(
            !config.use_keychain,
            "a test binary must not default to the real OS keychain"
        );
    }
}
