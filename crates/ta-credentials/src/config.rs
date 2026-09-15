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
    /// daemon available. `for_project()` also reads this off the
    /// `TA_NO_KEYCHAIN` environment variable for callers that don't build
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
    /// `use_keychain` defaults to `true`, except when the `TA_NO_KEYCHAIN`
    /// environment variable is set (to any value) — the OS keychain can
    /// require an interactive GUI permission prompt on first use (observed
    /// live on macOS: a fresh vault's encryption-key lookup blocks
    /// indefinitely with no visible error when nothing can answer that
    /// prompt), which hangs forever in any headless, scripted, or CI
    /// invocation of `ta credentials add`/`update` with no way to opt out
    /// before this. `TA_NO_KEYCHAIN` mirrors the existing `TA_IS_STAGING`
    /// convention (`ta-mcp-gateway`'s `GatewayConfig::for_project`) —
    /// presence, not value, is what's checked.
    pub fn for_project(project_root: impl AsRef<Path>) -> Self {
        let ta_dir = project_root.as_ref().join(".ta");
        Self {
            vault_path: ta_dir.join("credentials.json"),
            use_keychain: std::env::var("TA_NO_KEYCHAIN").is_err(),
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

    /// `TA_NO_KEYCHAIN` isn't touched by any other test in this crate, and
    /// both assertions live in one test function (not split across two)
    /// specifically so a parallel test runner never interleaves this env
    /// var's set/remove with a read from another test.
    #[test]
    fn for_project_respects_ta_no_keychain_env_var() {
        std::env::remove_var("TA_NO_KEYCHAIN");
        let config = CredentialsConfig::for_project("/tmp/does-not-need-to-exist");
        assert!(
            config.use_keychain,
            "use_keychain must default to true when TA_NO_KEYCHAIN is unset"
        );

        std::env::set_var("TA_NO_KEYCHAIN", "1");
        let config = CredentialsConfig::for_project("/tmp/does-not-need-to-exist");
        assert!(
            !config.use_keychain,
            "TA_NO_KEYCHAIN being set must disable keychain use, regardless of its value"
        );
        std::env::remove_var("TA_NO_KEYCHAIN");
    }
}
