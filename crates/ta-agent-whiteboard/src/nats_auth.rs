//! NATS connection credentials and the "no unauthenticated remote NATS" rule
//! (v0.17.12.2, CR-05).
//!
//! The pure logic lives here and takes the environment value and the vault
//! value as arguments, so it is testable without touching the process
//! environment, the real Keychain or the network. Nothing in this module
//! uses a unix-only API: a `nats-server.exe` on Windows listening on
//! `127.0.0.1` or `localhost` is treated exactly like the unix case.
//!
//! Resolution order:
//! 1. the environment variable named by `nats_credentials_env`
//!    (default `TA_NATS_CREDENTIALS`);
//! 2. the TA credential vault entry named [`VAULT_ENTRY_NAME`].
//!
//! The value (from either source) is one of: a path to an existing NATS
//! `.creds` file, `user:password`, or a bare token. The secret itself is
//! never logged, never part of an error message, and redacted in `Debug`.

use std::fmt;
use std::path::{Path, PathBuf};

/// Name of the TA credential vault entry consulted when the environment
/// variable is unset.
pub const VAULT_ENTRY_NAME: &str = "nats-credentials";

/// Default name of the environment variable holding NATS credentials.
pub const DEFAULT_CREDENTIALS_ENV: &str = "TA_NATS_CREDENTIALS";

/// How to authenticate to NATS. `Debug` is redacted by hand.
#[derive(Clone, PartialEq, Eq)]
pub enum NatsAuth {
    None,
    CredsFile(PathBuf),
    UserPassword { user: String, password: String },
    Token(String),
}

impl fmt::Debug for NatsAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NatsAuth::None => write!(f, "NatsAuth::None"),
            NatsAuth::CredsFile(p) => write!(f, "NatsAuth::CredsFile({})", p.display()),
            NatsAuth::UserPassword { user, .. } => {
                write!(
                    f,
                    "NatsAuth::UserPassword {{ user: {user:?}, password: <redacted> }}"
                )
            }
            NatsAuth::Token(_) => write!(f, "NatsAuth::Token(<redacted>)"),
        }
    }
}

impl NatsAuth {
    pub fn is_none(&self) -> bool {
        matches!(self, NatsAuth::None)
    }

    /// Short, secret-free description of the auth kind, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            NatsAuth::None => "none",
            NatsAuth::CredsFile(_) => "creds-file",
            NatsAuth::UserPassword { .. } => "user-password",
            NatsAuth::Token(_) => "token",
        }
    }
}

/// Where the credentials came from (never the value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsAuthSource {
    Env(String),
    Vault(&'static str),
    NotConfigured,
}

impl fmt::Display for NatsAuthSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NatsAuthSource::Env(name) => write!(f, "environment variable {name}"),
            NatsAuthSource::Vault(name) => write!(f, "vault entry {name:?}"),
            NatsAuthSource::NotConfigured => write!(f, "not configured"),
        }
    }
}

/// Failure to resolve usable NATS credentials. Messages never contain the
/// secret value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsAuthError {
    #[error(
        "refusing to connect to NATS at {url:?}: the host is not a loopback address and no \
         credentials are configured, so anyone who can reach it could read and forge \
         coordination messages. To fix: set the {env} environment variable (a path to a \
         .creds file, `user:password`, or a token), or store a vault entry named \
         `{vault}` with `ta credentials add`, or bind NATS to loopback (for example \
         `nats-server -a 127.0.0.1`) and point nats_url at it."
    )]
    UnauthenticatedRemote {
        url: String,
        env: String,
        vault: &'static str,
    },

    #[error(
        "NATS credentials from {origin} name a .creds file that does not exist: {path}. \
         To fix: create the file, correct the path, or unset {source_hint} to use a different \
         credential source."
    )]
    CredsFileMissing {
        origin: NatsAuthSource,
        source_hint: String,
        path: String,
    },

    #[error(
        "NATS credentials from {origin} are not usable: {reason}. Expected a path to an \
         existing .creds file, `user:password`, or a bare token. To fix: correct the value \
         in {origin}."
    )]
    Invalid {
        origin: NatsAuthSource,
        reason: String,
    },

    #[error("cannot determine the host of NATS url {url:?}. To fix: set nats_url in the [whiteboard] section of .ta/workflow.toml to host:port, for example \"127.0.0.1:4222\".")]
    BadUrl { url: String },
}

/// Extract the host from a NATS url such as `nats://localhost:4222`,
/// `tls://[::1]:4222`, `127.0.0.1:4222`, `user:pw@host:4222`. Tolerates
/// surrounding whitespace (including a stray `\r` from CRLF config files).
pub fn parse_host(url: &str) -> Option<String> {
    let mut s = url.trim();
    if let Some(idx) = s.find("://") {
        s = &s[idx + 3..];
    }
    // Drop path/query, then userinfo.
    if let Some(idx) = s.find(['/', '?']) {
        s = &s[..idx];
    }
    if let Some(idx) = s.rfind('@') {
        s = &s[idx + 1..];
    }
    let host = if let Some(rest) = s.strip_prefix('[') {
        rest.split(']').next()?
    } else if s.matches(':').count() > 1 {
        // Bare IPv6 without brackets and without a port.
        s
    } else {
        s.split(':').next()?
    };
    let host = host.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Whether `host` is `localhost`, a `127.0.0.0/8` address or `::1`.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    false
}

/// Whether the NATS url points at a loopback host.
pub fn is_loopback_url(url: &str) -> bool {
    parse_host(url)
        .map(|h| is_loopback_host(&h))
        .unwrap_or(false)
}

fn looks_like_path(v: &str) -> bool {
    let b = v.as_bytes();
    v.to_ascii_lowercase().ends_with(".creds")
        || v.starts_with('/')
        || v.starts_with("~/")
        || v.starts_with("./")
        || v.starts_with("../")
        || v.starts_with(".\\")
        || v.starts_with("\\\\")
        || (b.len() > 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
}

/// Parse one credential value (from `source`) into a [`NatsAuth`].
pub fn parse_credential_value(
    raw: &str,
    source: &NatsAuthSource,
    source_hint: &str,
) -> Result<NatsAuth, NatsAuthError> {
    let v = raw.trim();
    if v.is_empty() {
        return Ok(NatsAuth::None);
    }
    if Path::new(v).is_file() {
        return Ok(NatsAuth::CredsFile(PathBuf::from(v)));
    }
    if looks_like_path(v) {
        return Err(NatsAuthError::CredsFileMissing {
            origin: source.clone(),
            source_hint: source_hint.to_string(),
            path: v.to_string(),
        });
    }
    if let Some((user, password)) = v.split_once(':') {
        if user.is_empty() || password.is_empty() {
            return Err(NatsAuthError::Invalid {
                origin: source.clone(),
                reason: "user:password needs both a user and a password".to_string(),
            });
        }
        return Ok(NatsAuth::UserPassword {
            user: user.to_string(),
            password: password.to_string(),
        });
    }
    Ok(NatsAuth::Token(v.to_string()))
}

/// Resolve credentials: the environment value wins, then the vault value.
/// An empty/whitespace value counts as unset.
pub fn resolve_auth(
    env_name: &str,
    env_value: Option<&str>,
    vault_value: Option<&str>,
) -> Result<(NatsAuth, NatsAuthSource), NatsAuthError> {
    if let Some(v) = env_value.filter(|v| !v.trim().is_empty()) {
        let src = NatsAuthSource::Env(env_name.to_string());
        let auth = parse_credential_value(v, &src, env_name)?;
        return Ok((auth, src));
    }
    if let Some(v) = vault_value.filter(|v| !v.trim().is_empty()) {
        let src = NatsAuthSource::Vault(VAULT_ENTRY_NAME);
        let hint = format!("the vault entry {VAULT_ENTRY_NAME:?}");
        let auth = parse_credential_value(v, &src, &hint)?;
        return Ok((auth, src));
    }
    Ok((NatsAuth::None, NatsAuthSource::NotConfigured))
}

/// The full decision: resolve credentials, then refuse an unauthenticated
/// non-loopback url. Loopback with no credentials stays allowed.
pub fn decide(
    url: &str,
    env_name: &str,
    env_value: Option<&str>,
    vault_value: Option<&str>,
) -> Result<(NatsAuth, NatsAuthSource), NatsAuthError> {
    let host = parse_host(url).ok_or_else(|| NatsAuthError::BadUrl {
        url: url.trim().to_string(),
    })?;
    let (auth, src) = resolve_auth(env_name, env_value, vault_value)?;
    if auth.is_none() && !is_loopback_host(&host) {
        return Err(NatsAuthError::UnauthenticatedRemote {
            url: url.trim().to_string(),
            env: env_name.to_string(),
            vault: VAULT_ENTRY_NAME,
        });
    }
    Ok((auth, src))
}

/// Read the `nats-credentials` secret from the project's credential vault,
/// if the vault file exists. Never creates a vault. Honors `TA_NO_KEYCHAIN`
/// (and the test keychain guard) through `CredentialsConfig::for_project`.
pub fn read_vault_secret(project_root: &Path) -> Option<String> {
    use ta_credentials::{CredentialVault, CredentialsConfig, FileVault};
    let config = CredentialsConfig::for_project(project_root);
    if !config.vault_path.exists() {
        return None;
    }
    let vault = match FileVault::open(&config) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                vault = %config.vault_path.display(),
                entry = VAULT_ENTRY_NAME,
                error = %e,
                "could not open the credential vault to look for NATS credentials; \
                 set the credentials environment variable instead or fix the vault"
            );
            return None;
        }
    };
    let id = vault
        .list()
        .ok()?
        .into_iter()
        .find(|c| c.name == VAULT_ENTRY_NAME)?
        .id;
    vault.get(id).ok().map(|c| c.secret)
}

/// Resolve and enforce the policy for `url` using the real environment and
/// (when `project_root` is given and the env var is unset) the vault.
pub fn resolve_for_url(
    url: &str,
    env_name: &str,
    project_root: Option<&Path>,
) -> Result<NatsAuth, NatsAuthError> {
    let env_value = std::env::var(env_name).ok();
    let env_set = env_value.as_deref().is_some_and(|v| !v.trim().is_empty());
    let vault_value = if env_set {
        None
    } else {
        project_root.and_then(read_vault_secret)
    };
    let (auth, src) = decide(url, env_name, env_value.as_deref(), vault_value.as_deref())?;
    tracing::info!(
        url = %url.trim(),
        auth = auth.kind(),
        source = %src,
        "whiteboard: NATS authentication resolved"
    );
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: &str = "TA_NATS_CREDENTIALS";

    #[test]
    fn loopback_without_creds_is_ok() {
        for url in [
            "localhost:4222",
            "nats://localhost:4222",
            "127.0.0.1:4222",
            "nats://127.0.0.2:4222",
            "[::1]:4222",
            "nats://[::1]:4222",
            "tls://localhost",
            "LOCALHOST:4222",
        ] {
            let (auth, _) = decide(url, ENV, None, None).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert!(auth.is_none(), "{url}");
        }
    }

    #[test]
    fn windows_style_and_crlf_inputs_parse() {
        assert_eq!(
            parse_host("nats://127.0.0.1:4222\r\n").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            parse_host("  localhost:4222\r").as_deref(),
            Some("localhost")
        );
        assert!(is_loopback_url("nats://localhost:4222\r\n"));
        assert!(!is_loopback_url("nats://10.0.0.5:4222\r\n"));
        assert_eq!(
            parse_host("nats://user:pw@host.example:4222").as_deref(),
            Some("host.example")
        );
        assert_eq!(parse_host(""), None);
    }

    #[test]
    fn remote_without_creds_is_refused_with_actionable_message() {
        for url in [
            "10.0.0.5:4222",
            "nats://example.com:4222",
            "tls://[2001:db8::1]:4222",
        ] {
            let err = decide(url, ENV, None, None).unwrap_err();
            let msg = err.to_string();
            assert!(matches!(err, NatsAuthError::UnauthenticatedRemote { .. }));
            assert!(msg.contains(url), "{msg}");
            assert!(msg.contains("TA_NATS_CREDENTIALS"), "{msg}");
            assert!(msg.contains("nats-credentials"), "{msg}");
            assert!(msg.contains("127.0.0.1"), "{msg}");
        }
    }

    #[test]
    fn remote_with_env_creds_is_accepted() {
        let (auth, src) = decide("example.com:4222", ENV, Some("alice:s3cret"), None).unwrap();
        assert!(matches!(auth, NatsAuth::UserPassword { .. }));
        assert_eq!(src, NatsAuthSource::Env(ENV.to_string()));
        let (auth, _) = decide("example.com:4222", ENV, Some("tok-abc123"), None).unwrap();
        assert_eq!(auth, NatsAuth::Token("tok-abc123".to_string()));
    }

    #[test]
    fn creds_file_path_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("user.creds");
        std::fs::write(&file, "-----BEGIN NATS USER JWT-----\n").unwrap();
        let (auth, _) =
            decide("example.com:4222", ENV, Some(file.to_str().unwrap()), None).unwrap();
        assert_eq!(auth, NatsAuth::CredsFile(file.clone()));

        let missing = dir.path().join("nope.creds");
        let err = decide(
            "example.com:4222",
            ENV,
            Some(missing.to_str().unwrap()),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, NatsAuthError::CredsFileMissing { .. }));
        let msg = err.to_string();
        assert!(
            msg.contains("nope.creds") && msg.contains("does not exist"),
            "{msg}"
        );
    }

    #[test]
    fn windows_drive_path_is_treated_as_missing_file_not_user_password() {
        let err = decide(
            "example.com:4222",
            ENV,
            Some(r"C:\nats\missing.creds"),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, NatsAuthError::CredsFileMissing { .. }));
        let err = decide("example.com:4222", ENV, Some(r"C:\nats\x"), None).unwrap_err();
        assert!(matches!(err, NatsAuthError::CredsFileMissing { .. }));
    }

    #[test]
    fn vault_fallback_used_when_env_unset_and_env_wins() {
        let (auth, src) = decide("example.com:4222", ENV, None, Some("vaultuser:vaultpw")).unwrap();
        assert!(matches!(auth, NatsAuth::UserPassword { ref user, .. } if user == "vaultuser"));
        assert_eq!(src, NatsAuthSource::Vault(VAULT_ENTRY_NAME));

        let (auth, src) = decide(
            "example.com:4222",
            ENV,
            Some("envtoken"),
            Some("vaultuser:vaultpw"),
        )
        .unwrap();
        assert_eq!(auth, NatsAuth::Token("envtoken".to_string()));
        assert_eq!(src, NatsAuthSource::Env(ENV.to_string()));

        // Blank env counts as unset.
        let (_, src) = decide("example.com:4222", ENV, Some("  "), Some("tok")).unwrap();
        assert_eq!(src, NatsAuthSource::Vault(VAULT_ENTRY_NAME));
    }

    #[test]
    fn secrets_never_appear_in_debug_or_errors() {
        let secret = "SUPERSECRETVALUE";
        for auth in [
            NatsAuth::Token(secret.to_string()),
            NatsAuth::UserPassword {
                user: "bob".into(),
                password: secret.to_string(),
            },
        ] {
            let dbg = format!("{auth:?}");
            assert!(!dbg.contains(secret), "{dbg}");
        }
        let err = decide("example.com:4222", ENV, Some("bob:"), None).unwrap_err();
        assert!(!format!("{err} {err:?}").contains(secret));
        let err = decide("example.com:4222", ENV, None, None).unwrap_err();
        assert!(!format!("{err:?}").contains(secret));
    }

    #[test]
    fn read_vault_secret_without_vault_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_vault_secret(dir.path()), None);
    }
}
