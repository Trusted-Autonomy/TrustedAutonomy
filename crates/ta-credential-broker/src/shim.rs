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
    let broker = CredentialBroker::open_for_project(project_root).map_err(|e| {
        ShimError::BrokerUnavailable {
            path: broker_dir.display().to_string(),
            reason: e.to_string(),
        }
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

// ── Out-of-sandbox resolution (v0.17.12.6) ─────────────────────────────────
//
// A sandboxed agent cannot read `.ta/` (the broker key, the vault, the
// connector registry), so the shims running inside its box ask the
// out-of-sandbox gateway socket instead. The wire form is one handshake line
// `Bearer <token> credential` followed by one `host=<host>` line; the reply is
// `OK` plus `key=value` lines, `NONE` (no connector for the host), or
// `ERR <reason>`. Only the shim for that one operation ever sees the secret.

/// Purpose word in the handshake line that selects credential resolution.
pub const CREDENTIAL_PURPOSE: &str = "credential";

/// Encode the gateway's reply for a resolution outcome.
pub fn encode_endpoint_reply(result: &Result<ShimResolution, ShimError>) -> String {
    match result {
        Ok(r) => format!(
            "OK\nsecret={}\ncredential_name={}\nconnector_id={}\n",
            r.secret, r.credential_name, r.connector_id
        ),
        Err(ShimError::NoConnector(_)) | Err(ShimError::NoSessionToken { .. }) => {
            "NONE\n".to_string()
        }
        Err(e) => format!("ERR {}\n", e.to_string().replace('\n', " ")),
    }
}

/// Parse the gateway's reply. `Ok(None)` means "no connector for this host".
pub fn parse_endpoint_reply(reply: &str) -> Result<Option<ShimResolution>, ShimError> {
    let mut lines = reply.lines();
    match lines.next() {
        Some("OK") => {
            let mut secret = None;
            let mut name = None;
            let mut id = None;
            for l in lines {
                match l.split_once('=') {
                    Some(("secret", v)) => secret = Some(v.to_string()),
                    Some(("credential_name", v)) => name = Some(v.to_string()),
                    Some(("connector_id", v)) => id = Some(v.to_string()),
                    _ => {}
                }
            }
            match (secret, name, id) {
                (Some(secret), Some(credential_name), Some(connector_id)) => {
                    Ok(Some(ShimResolution {
                        secret,
                        credential_name,
                        connector_id,
                    }))
                }
                _ => Err(ShimError::CredentialLookupFailed {
                    connector_id: "(gateway)".into(),
                    reason: "the TA gateway returned an incomplete credential reply".into(),
                }),
            }
        }
        Some("NONE") => Ok(None),
        Some(other) => Err(ShimError::CredentialLookupFailed {
            connector_id: "(gateway)".into(),
            reason: other.strip_prefix("ERR ").unwrap_or(other).to_string(),
        }),
        None => Err(ShimError::CredentialLookupFailed {
            connector_id: "(gateway)".into(),
            reason: "the TA gateway closed the connection without replying; is the goal's \
                     gateway still running?"
                .into(),
        }),
    }
}

/// Ask the out-of-sandbox gateway at `endpoint` for the secret backing `host`.
///
/// `endpoint` is a Unix socket path, or a `\\.\pipe\...` name on Windows.
pub fn resolve_via_endpoint(
    endpoint: &Path,
    token: &str,
    host: &str,
) -> Result<Option<ShimResolution>, ShimError> {
    let fail = |reason: String| ShimError::CredentialLookupFailed {
        connector_id: "(gateway)".into(),
        reason: format!(
            "{reason} (endpoint {}). The goal's gateway may have exited; re-run the goal",
            endpoint.display()
        ),
    };
    let request = format!("Bearer {token} {CREDENTIAL_PURPOSE}\nhost={host}\n");
    let reply = exchange(endpoint, &request).map_err(fail)?;
    parse_endpoint_reply(&reply)
}

#[cfg(unix)]
fn exchange(endpoint: &Path, request: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(endpoint)
        .map_err(|e| format!("could not connect: {e}"))?;
    let timeout = Some(std::time::Duration::from_secs(30));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("could not send the request: {e}"))?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .map_err(|e| format!("could not read the reply: {e}"))?;
    Ok(reply)
}

/// Named pipe client. A byte pipe has no half-close, so the gateway answers as
/// soon as it has read the request line and then closes its end; the read below
/// ends there. A file handle has no read timeout, so the exchange runs on a
/// thread that is abandoned after 30 seconds.
#[cfg(windows)]
fn exchange(endpoint: &Path, request: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    const ERROR_PIPE_BUSY: i32 = 231;
    let deadline = std::time::Duration::from_secs(30);
    let endpoint = endpoint.to_path_buf();
    let request = request.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> Result<String, String> {
            let start = std::time::Instant::now();
            let mut pipe = loop {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&endpoint)
                {
                    Ok(f) => break f,
                    Err(e)
                        if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                            && start.elapsed() < std::time::Duration::from_secs(5) =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    Err(e) => return Err(format!("could not connect: {e}")),
                }
            };
            pipe.write_all(request.as_bytes())
                .map_err(|e| format!("could not send the request: {e}"))?;
            let mut reply = String::new();
            pipe.read_to_string(&mut reply)
                .map_err(|e| format!("could not read the reply: {e}"))?;
            Ok(reply)
        };
        let _ = tx.send(run());
    });
    rx.recv_timeout(deadline)
        .map_err(|_| format!("no reply within {}s", deadline.as_secs()))?
}

#[cfg(not(any(unix, windows)))]
fn exchange(_endpoint: &Path, _request: &str) -> Result<String, String> {
    Err("this platform has no gateway transport".to_string())
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn reply_roundtrips_and_none_is_silent() {
        let ok = Ok(ShimResolution {
            secret: "s3".into(),
            credential_name: "gh".into(),
            connector_id: "github".into(),
        });
        let parsed = parse_endpoint_reply(&encode_endpoint_reply(&ok)).unwrap();
        assert_eq!(parsed.unwrap().secret, "s3");
        let none = Err(ShimError::NoConnector("x".into()));
        assert_eq!(
            parse_endpoint_reply(&encode_endpoint_reply(&none)).unwrap(),
            None
        );
    }

    #[test]
    fn errors_and_empty_replies_are_actionable() {
        let e = parse_endpoint_reply("ERR vault locked\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("vault locked"), "{e}");
        let e = parse_endpoint_reply("").unwrap_err().to_string();
        assert!(e.contains("still running"), "{e}");
    }
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
        let broker = CredentialBroker::open_for_project(project_root).unwrap();
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
