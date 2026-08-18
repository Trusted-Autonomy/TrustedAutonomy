// shim.rs — Shell/CLI credential shim resolution (v0.17.6.7).
//
// v0.17.6.3 built the gateway's live interception point for MCP tool calls,
// but a Bash-driven coding agent's *majority* credentialed actions (`git
// push`, `gh pr create`, a raw `curl` with a bearer header) never go through
// an MCP tool call at all. This module is the shared resolution logic behind
// the two concrete shims that close that gap for git and the GitHub CLI:
// `ta credential-helper` (git's pluggable `credential.helper` protocol) and
// the `gh` wrapper binary (PATH-shadows the real `gh`, injects the token
// only into that one child process's environment).
//
// Both shims run as short-lived child processes of the agent's own shell,
// inheriting `TA_SESSION_TOKEN_<credential>` (the biscuit grant — never the
// raw secret; see `ta_runtime::apply_credentials_to_env`) but never the raw
// secret itself. This function is what turns that grant back into the real
// secret, entirely offline, the same way the gateway's own interception
// point does for MCP tool calls.

use std::path::Path;

use ta_credentials::{ConnectorRegistry, CredentialVault, CredentialsConfig, FileVault};
use thiserror::Error;

use crate::broker::CredentialBroker;

/// A resolved secret, ready to hand to the calling tool (git, `gh`) for
/// exactly one operation. Never logged, never written to disk by the shim
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimResolution {
    pub secret: String,
    pub credential_name: String,
    pub connector_id: String,
}

/// Why a shim could not resolve a secret for `host`.
///
/// [`ShimError::NoConnector`] is the expected, silent-fallback case: `host`
/// isn't declared by any broker-mediated connector, so the calling shim
/// should behave as if it were never installed (git falls back to its own
/// prompt/other helpers; the `gh` wrapper execs the real `gh` unmodified).
/// Every other variant is an actionable misconfiguration or failure worth
/// surfacing to the operator.
#[derive(Debug, Error)]
pub enum ShimError {
    #[error("no broker-mediated connector declares host '{0}'")]
    NoConnector(String),

    #[error(
        "connector '{connector_id}' is broker-mediated for host '{host}' but this process has \
         no TA_SESSION_TOKEN_{credential_name} in its environment — the agent's own spawn must \
         not have granted this credential (see `ta credentials grant` / `docs/USAGE.md`)"
    )]
    NoSessionToken {
        connector_id: String,
        host: String,
        credential_name: String,
    },

    #[error("session token for connector '{connector_id}' failed verification: {reason}")]
    VerifyFailed {
        connector_id: String,
        reason: String,
    },

    #[error("credential vault at {path} could not be opened: {reason}")]
    VaultUnavailable { path: String, reason: String },

    #[error("credential broker at {path} could not be opened: {reason}")]
    BrokerUnavailable { path: String, reason: String },

    #[error("failed to resolve credential for connector '{connector_id}': {reason}")]
    CredentialLookupFailed {
        connector_id: String,
        reason: String,
    },

    #[error(
        "session token for connector '{connector_id}' does not back the credential that \
         connector declares ('{expected}' expected, token backs '{actual}')"
    )]
    CredentialMismatch {
        connector_id: String,
        expected: String,
        actual: String,
    },
}

/// Resolve the real secret backing `host` for a shell/CLI credential shim.
///
/// `project_root` is the directory containing `.ta/` (broker root key,
/// `credentials.json`, `connectors.toml`) — callers resolve this from
/// `TA_PROJECT_ROOT` (falling back to the current working directory), the
/// same convention `ta serve` already uses for the MCP stdio subprocess.
///
/// `use_keychain` should be `true` for every real invocation; `false` only
/// in tests, matching `CredentialsConfig::use_keychain`'s existing contract.
pub fn resolve_for_host(
    project_root: &Path,
    host: &str,
    use_keychain: bool,
) -> Result<ShimResolution, ShimError> {
    let registry = ConnectorRegistry::load(&project_root.join(".ta"));
    let Some((connector_id, entry)) = registry.find_by_host(host) else {
        return Err(ShimError::NoConnector(host.to_string()));
    };
    let connector_id = connector_id.to_string();

    let env_var = format!("TA_SESSION_TOKEN_{}", entry.credential_name);
    let Ok(session_token) = std::env::var(&env_var) else {
        return Err(ShimError::NoSessionToken {
            connector_id,
            host: host.to_string(),
            credential_name: entry.credential_name.clone(),
        });
    };

    let broker_dir = project_root.join(".ta");
    let broker = CredentialBroker::open(&broker_dir).map_err(|e| ShimError::BrokerUnavailable {
        path: broker_dir.display().to_string(),
        reason: e.to_string(),
    })?;
    let grant = broker
        .verify(&session_token)
        .map_err(|e| ShimError::VerifyFailed {
            connector_id: connector_id.clone(),
            reason: e.to_string(),
        })?;

    let mut cred_config = CredentialsConfig::for_project(project_root);
    cred_config.use_keychain = use_keychain;
    let vault = FileVault::open(&cred_config).map_err(|e| ShimError::VaultUnavailable {
        path: cred_config.vault_path.display().to_string(),
        reason: e.to_string(),
    })?;
    let credential =
        vault
            .get(grant.credential_id)
            .map_err(|e| ShimError::CredentialLookupFailed {
                connector_id: connector_id.clone(),
                reason: e.to_string(),
            })?;

    if credential.name != entry.credential_name {
        return Err(ShimError::CredentialMismatch {
            connector_id,
            expected: entry.credential_name.clone(),
            actual: credential.name,
        });
    }

    Ok(ShimResolution {
        secret: credential.secret,
        credential_name: credential.name,
        connector_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // `std::env::set_var`/`resolve_for_host` reads process-global env, so
    // tests that touch `TA_SESSION_TOKEN_*` must not run concurrently.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_connectors_toml(project_root: &Path, body: &str) {
        std::fs::create_dir_all(project_root.join(".ta")).unwrap();
        std::fs::write(project_root.join(".ta/connectors.toml"), body).unwrap();
    }

    fn seed_credential(
        project_root: &Path,
        name: &str,
        secret: &str,
    ) -> (uuid::Uuid, CredentialBroker) {
        let mut cred_config = CredentialsConfig::for_project(project_root);
        cred_config.use_keychain = false;
        let mut vault = FileVault::open(&cred_config).unwrap();
        let cred = vault.add(name, "github", secret, vec![]).unwrap();
        let broker = CredentialBroker::open(&project_root.join(".ta")).unwrap();
        (cred.id, broker)
    }

    #[test]
    fn resolves_real_secret_for_matching_host() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        write_connectors_toml(
            dir.path(),
            "[connectors.github]\ncredential_name = \"GITHUB_TOKEN\"\n\
             broker_mediated = true\nhosts = [\"github.com\"]\n",
        );
        let (cred_id, broker) = seed_credential(dir.path(), "GITHUB_TOKEN", "ghp_real_secret");
        let granted = broker.grant(cred_id, "agent-1", vec![], 3600).unwrap();

        std::env::set_var("TA_SESSION_TOKEN_GITHUB_TOKEN", &granted.token);
        let result = resolve_for_host(dir.path(), "github.com", false);
        std::env::remove_var("TA_SESSION_TOKEN_GITHUB_TOKEN");

        let resolution = result.unwrap();
        assert_eq!(resolution.secret, "ghp_real_secret");
        assert_eq!(resolution.credential_name, "GITHUB_TOKEN");
        assert_eq!(resolution.connector_id, "github");
    }

    #[test]
    fn unlisted_host_falls_through_silently() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        write_connectors_toml(
            dir.path(),
            "[connectors.github]\ncredential_name = \"GITHUB_TOKEN\"\n\
             broker_mediated = true\nhosts = [\"github.com\"]\n",
        );

        let result = resolve_for_host(dir.path(), "gitlab.com", false);
        assert!(matches!(result, Err(ShimError::NoConnector(host)) if host == "gitlab.com"));
    }

    #[test]
    fn missing_session_token_is_actionable_not_silent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        write_connectors_toml(
            dir.path(),
            "[connectors.github]\ncredential_name = \"GITHUB_TOKEN\"\n\
             broker_mediated = true\nhosts = [\"github.com\"]\n",
        );
        std::env::remove_var("TA_SESSION_TOKEN_GITHUB_TOKEN");

        let result = resolve_for_host(dir.path(), "github.com", false);
        assert!(matches!(
            result,
            Err(ShimError::NoSessionToken { credential_name, .. })
                if credential_name == "GITHUB_TOKEN"
        ));
    }

    #[test]
    fn revoked_token_is_rejected() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        write_connectors_toml(
            dir.path(),
            "[connectors.github]\ncredential_name = \"GITHUB_TOKEN\"\n\
             broker_mediated = true\nhosts = [\"github.com\"]\n",
        );
        let (cred_id, mut broker) = seed_credential(dir.path(), "GITHUB_TOKEN", "ghp_real_secret");
        let granted = broker.grant(cred_id, "agent-1", vec![], 3600).unwrap();
        broker.revoke(&granted.token_id).unwrap();

        std::env::set_var("TA_SESSION_TOKEN_GITHUB_TOKEN", &granted.token);
        let result = resolve_for_host(dir.path(), "github.com", false);
        std::env::remove_var("TA_SESSION_TOKEN_GITHUB_TOKEN");

        assert!(matches!(result, Err(ShimError::VerifyFailed { .. })));
    }
}
