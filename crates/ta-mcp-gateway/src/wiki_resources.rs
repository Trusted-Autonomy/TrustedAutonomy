//! Shared `.ta/wiki-resources.toml` config parsing, credential resolution,
//! and Wayfinder wiki client construction.
//!
//! Extracted out of `tools/wiki.rs` so the `ta_wiki_*` MCP tools and the
//! daemon-owned background sync task (`ta-daemon`'s `wiki_sync.rs`)
//! resolve config and credentials through exactly the same code path --
//! two independent implementations of "which credential backs a read vs a
//! write" would be a correctness bug waiting to happen the day only one of
//! them gets updated.
//!
//! Errors here are `anyhow`, not `McpError`: this module has no MCP
//! dependency, so a non-MCP caller (the daemon task) doesn't need to link
//! against `rmcp` error types just to read a TOML file.

use std::path::Path;

use crate::wiki_client::WikiMcpClient;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct WikiResourcesConfig {
    #[serde(default)]
    pub scopes: Vec<ScopeConfig>,
    pub wayfinder: WayfinderConfig,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ScopeConfig {
    /// Human-readable label, not otherwise used for addressing (callers
    /// address a scope by `scope`+`id`, matching Wayfinder's own tool
    /// signatures) -- kept for the config file's own readability and for
    /// the background sync task's log lines.
    pub name: String,
    /// `"project"` or `"org"`.
    pub scope: String,
    pub id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct WayfinderConfig {
    /// Wayfinder's wiki MCP endpoint, e.g. `https://wayfinder.example.com/mcp`.
    pub base_url: String,
    pub read_credential_name: String,
    pub write_credential_name: String,
}

pub fn load_wiki_resources_config(project_root: &Path) -> anyhow::Result<WikiResourcesConfig> {
    let path = project_root.join(".ta").join("wiki-resources.toml");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("no wiki configuration found at {} ({e})", path.display()))?;
    toml::from_str(&raw).map_err(|e| anyhow::anyhow!("malformed {} ({e})", path.display()))
}

/// True only when `.ta/wiki-resources.toml` exists at all -- used by the
/// background sync task to skip projects that never paired with Wayfinder,
/// the same "not configured, not an error" treatment
/// `token_refresh.rs::refresh_one_session` gives a session with no
/// whiteboard token.
pub fn wiki_resources_configured(project_root: &Path) -> bool {
    project_root
        .join(".ta")
        .join("wiki-resources.toml")
        .is_file()
}

/// Whether the credential vault should try the OS keychain first, per this
/// project's own default (honors `TA_NO_KEYCHAIN`). Exposed so callers
/// that don't otherwise depend on `ta-credentials` directly -- the daemon
/// background sync task, specifically -- don't need to add that
/// dependency just to compute this one bool.
pub fn default_use_keychain(project_root: &Path) -> bool {
    ta_credentials::CredentialsConfig::for_project(project_root).use_keychain
}

pub fn resolve_credential_secret(
    project_root: &Path,
    use_keychain: bool,
    credential_name: &str,
) -> anyhow::Result<String> {
    use ta_credentials::CredentialVault;

    let mut cred_config = ta_credentials::CredentialsConfig::for_project(project_root);
    cred_config.use_keychain = use_keychain;
    let vault = ta_credentials::FileVault::open(&cred_config)
        .map_err(|e| anyhow::anyhow!("could not open the credential vault: {e}"))?;
    let summaries = vault
        .list()
        .map_err(|e| anyhow::anyhow!("could not list credentials: {e}"))?;
    let summary = summaries
        .iter()
        .find(|c| c.name == credential_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no credential named '{credential_name}' found. Run `ta credentials list` to \
                 see what's stored, or re-run the pairing step if this project hasn't been \
                 paired with Wayfinder yet."
            )
        })?;
    let full = vault
        .get(summary.id)
        .map_err(|e| anyhow::anyhow!("could not resolve credential '{credential_name}': {e}"))?;
    Ok(full.secret)
}

pub fn read_client(
    project_root: &Path,
    use_keychain: bool,
    config: &WikiResourcesConfig,
) -> anyhow::Result<WikiMcpClient> {
    let token = resolve_credential_secret(
        project_root,
        use_keychain,
        &config.wayfinder.read_credential_name,
    )?;
    Ok(WikiMcpClient::new(config.wayfinder.base_url.clone(), token))
}

pub fn write_client(
    project_root: &Path,
    use_keychain: bool,
    config: &WikiResourcesConfig,
) -> anyhow::Result<WikiMcpClient> {
    let token = resolve_credential_secret(
        project_root,
        use_keychain,
        &config.wayfinder.write_credential_name,
    )?;
    Ok(WikiMcpClient::new(config.wayfinder.base_url.clone(), token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_file_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_wiki_resources_config(dir.path()).unwrap_err();
        assert!(err.to_string().contains("no wiki configuration found"));
    }

    #[test]
    fn wiki_resources_configured_false_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!wiki_resources_configured(dir.path()));
    }

    #[test]
    fn wiki_resources_configured_true_when_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("wiki-resources.toml"),
            "[wayfinder]\nbase_url = \"https://example.com/mcp\"\n\
             read_credential_name = \"r\"\nwrite_credential_name = \"w\"\n",
        )
        .unwrap();
        assert!(wiki_resources_configured(dir.path()));
    }

    #[test]
    fn resolve_credential_secret_errors_clearly_when_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            resolve_credential_secret(dir.path(), false, "wayfinder-service-account").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no credential named"));
        assert!(msg.contains("wayfinder-service-account"));
    }

    #[test]
    fn resolve_credential_secret_finds_a_stored_credential_by_name() {
        use ta_credentials::CredentialVault;

        let dir = tempfile::tempdir().unwrap();
        let mut cred_config = ta_credentials::CredentialsConfig::for_project(dir.path());
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        vault
            .add("wayfinder-service-account", "wayfinder", "tok-abc", vec![])
            .unwrap();

        let secret =
            resolve_credential_secret(dir.path(), false, "wayfinder-service-account").unwrap();
        assert_eq!(secret, "tok-abc");
    }

    #[test]
    fn resolve_credential_secret_distinguishes_read_and_write_credentials() {
        use ta_credentials::CredentialVault;

        let dir = tempfile::tempdir().unwrap();
        let mut cred_config = ta_credentials::CredentialsConfig::for_project(dir.path());
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        vault
            .add("wayfinder-service-account", "wayfinder", "read-tok", vec![])
            .unwrap();
        vault
            .add("wayfinder-wiki-writer", "wayfinder", "write-tok", vec![])
            .unwrap();

        assert_eq!(
            resolve_credential_secret(dir.path(), false, "wayfinder-service-account").unwrap(),
            "read-tok"
        );
        assert_eq!(
            resolve_credential_secret(dir.path(), false, "wayfinder-wiki-writer").unwrap(),
            "write-tok"
        );
    }

    #[test]
    fn default_use_keychain_honors_ta_no_keychain() {
        // TA_NO_KEYCHAIN is process-global env state shared across tests;
        // save/restore so this test can't leak into others run in the
        // same process (cargo test runs unit tests single-process,
        // multi-threaded).
        let dir = tempfile::tempdir().unwrap();
        let had_var = std::env::var("TA_NO_KEYCHAIN").ok();

        std::env::set_var("TA_NO_KEYCHAIN", "1");
        assert!(!default_use_keychain(dir.path()));
        std::env::remove_var("TA_NO_KEYCHAIN");
        assert!(default_use_keychain(dir.path()));

        match had_var {
            Some(v) => std::env::set_var("TA_NO_KEYCHAIN", v),
            None => std::env::remove_var("TA_NO_KEYCHAIN"),
        }
    }

    #[test]
    fn parses_scopes_and_wayfinder_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("wiki-resources.toml"),
            r#"
[[scopes]]
name = "project"
scope = "project"
id = "proj-1"

[wayfinder]
base_url = "https://example.com/mcp"
read_credential_name = "wayfinder-service-account"
write_credential_name = "wayfinder-wiki-writer"
"#,
        )
        .unwrap();
        let config = load_wiki_resources_config(dir.path()).unwrap();
        assert_eq!(config.scopes.len(), 1);
        assert_eq!(config.scopes[0].id, "proj-1");
        assert_eq!(
            config.wayfinder.read_credential_name,
            "wayfinder-service-account"
        );
    }
}
