// config.rs — validated runtime config for `WayfinderPlanStore`, built from
// `ta-submit`'s plain-data `WayfinderPlanBackendConfig` plus the secret
// resolved from the credential vault. Validation lives here (not on the
// plain-data struct) because it needs network/security judgment calls
// (scheme + host checks), not just shape checks.

use std::path::Path;

use anyhow::{bail, Context};
use ta_credentials::{CredentialVault, CredentialsConfig, FileVault};
use ta_submit::config::WayfinderPlanBackendConfig;
use url::Url;

use crate::secret::RedactedSecret;

/// Validated connection settings for `WayfinderPlanStore`. `Debug`-safe:
/// `secret`'s own `Debug` impl never prints the real value.
#[derive(Debug, Clone)]
pub struct WayfinderPlanConfig {
    pub base_url: Url,
    pub org_id: String,
    pub project_id: String,
    pub secret: RedactedSecret,
}

impl WayfinderPlanConfig {
    /// Validates `raw.base_url` and resolves the service-account secret
    /// from the project's credential vault by name.
    ///
    /// **Security-critical check**: a `service_account_token` is a
    /// long-lived bearer credential (valid until explicitly revoked, unlike
    /// a 24h session) — sending it over plaintext HTTP to anything but the
    /// local machine hands it to every network hop in between. `base_url`
    /// must be `https://`, or `http://` only when the host is a loopback
    /// address (`localhost`/`127.0.0.1`/`::1`), matching how local
    /// development against an unencrypted `wayfinder-api` instance is the
    /// one legitimate case for plaintext.
    pub fn load(project_root: &Path, raw: &WayfinderPlanBackendConfig) -> anyhow::Result<Self> {
        Self::load_with_credentials_config(raw, &CredentialsConfig::for_project(project_root))
    }

    /// Core validation logic, taking an explicit `CredentialsConfig` so
    /// tests can force `use_keychain: false` (the OS keychain is a
    /// process/OS-global resource — touching it from a test would make
    /// tests interfere with each other and the developer's real keychain,
    /// and can hang on macOS waiting for a permission prompt in a headless
    /// run, the same reason `ta_credentials`' own tests avoid it).
    pub fn load_with_credentials_config(
        raw: &WayfinderPlanBackendConfig,
        credentials_config: &CredentialsConfig,
    ) -> anyhow::Result<Self> {
        let base_url = Url::parse(&raw.base_url).with_context(|| {
            format!(
                "[plan.wayfinder] base_url '{}' is not a valid URL",
                raw.base_url
            )
        })?;
        validate_scheme_and_host(&base_url)?;

        if raw.org_id.trim().is_empty() {
            bail!("[plan.wayfinder] org_id must not be empty");
        }
        if raw.project_id.trim().is_empty() {
            bail!("[plan.wayfinder] project_id must not be empty");
        }
        if raw.credential_name.trim().is_empty() {
            bail!("[plan.wayfinder] credential_name must not be empty");
        }

        let secret = load_secret(credentials_config, &raw.credential_name)?;

        Ok(Self {
            base_url,
            org_id: raw.org_id.clone(),
            project_id: raw.project_id.clone(),
            secret,
        })
    }
}

fn validate_scheme_and_host(url: &Url) -> anyhow::Result<()> {
    let is_loopback_host = matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1")
    );
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host => Ok(()),
        "http" => bail!(
            "[plan.wayfinder] base_url '{url}' uses plain http:// against a non-loopback host. \
             The service-account token is a long-lived bearer credential — sending it over \
             unencrypted HTTP would expose it to every network hop in between. Use https://, or \
             http://localhost (or 127.0.0.1/::1) for local development only."
        ),
        other => bail!(
            "[plan.wayfinder] base_url '{url}' has unsupported scheme '{other}' — must be \
             https:// (or http:// for localhost only)."
        ),
    }
}

/// Looks up `credential_name` in the project's `ta_credentials` vault.
/// Never falls back to an environment variable or plaintext config field —
/// the vault (age-encrypted at rest, OS-keychain key custody) is the only
/// sanctioned place this secret lives at rest.
fn load_secret(
    credentials_config: &CredentialsConfig,
    credential_name: &str,
) -> anyhow::Result<RedactedSecret> {
    let vault = FileVault::open(credentials_config).with_context(|| {
        "failed to open credential vault while loading the Wayfinder service-account secret"
    })?;

    let summary = vault
        .list()
        .with_context(|| "failed to list credentials in the vault")?
        .into_iter()
        .find(|c| c.name == credential_name)
        .with_context(|| {
            format!(
                "no credential named '{credential_name}' found in the vault. Set it with: \
                 `ta credential add {credential_name} wayfinder <secret>` (the secret shown \
                 once when the service account was created in Wayfinder's Settings)."
            )
        })?;

    let credential = vault
        .get(summary.id)
        .with_context(|| format!("failed to read credential '{credential_name}' from the vault"))?;

    Ok(RedactedSecret::new(credential.secret))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ta_credentials::CredentialVault;
    use tempfile::TempDir;

    fn backend_config(base_url: &str) -> WayfinderPlanBackendConfig {
        WayfinderPlanBackendConfig {
            base_url: base_url.to_string(),
            org_id: "org-1".to_string(),
            project_id: "proj-1".to_string(),
            credential_name: "wayfinder:org-1:proj-1".to_string(),
        }
    }

    fn test_credentials_config(dir: &TempDir) -> CredentialsConfig {
        CredentialsConfig {
            vault_path: dir.path().join(".ta").join("credentials.json"),
            use_keychain: false,
        }
    }

    fn seed_credential(dir: &TempDir, name: &str, secret: &str) {
        let mut vault = FileVault::open(&test_credentials_config(dir)).unwrap();
        vault.add(name, "wayfinder", secret, vec![]).unwrap();
    }

    #[test]
    fn plaintext_http_against_a_public_host_is_rejected() {
        let dir = TempDir::new().unwrap();
        let raw = backend_config("http://wayfinder.example.com");
        let err =
            WayfinderPlanConfig::load_with_credentials_config(&raw, &test_credentials_config(&dir))
                .unwrap_err();
        assert!(err.to_string().contains("unencrypted HTTP"));
    }

    #[test]
    fn plaintext_http_against_localhost_is_allowed() {
        let dir = TempDir::new().unwrap();
        seed_credential(&dir, "wayfinder:org-1:proj-1", "wfsa_test_secret");
        let raw = backend_config("http://localhost:8080");
        let config =
            WayfinderPlanConfig::load_with_credentials_config(&raw, &test_credentials_config(&dir))
                .unwrap();
        assert_eq!(config.secret.expose_secret(), "wfsa_test_secret");
    }

    #[test]
    fn https_against_a_public_host_is_allowed() {
        let dir = TempDir::new().unwrap();
        seed_credential(&dir, "wayfinder:org-1:proj-1", "wfsa_test_secret");
        let raw = backend_config("https://wayfinder.example.com");
        assert!(WayfinderPlanConfig::load_with_credentials_config(
            &raw,
            &test_credentials_config(&dir)
        )
        .is_ok());
    }

    #[test]
    fn missing_credential_produces_an_actionable_error() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let raw = backend_config("https://wayfinder.example.com");
        let err =
            WayfinderPlanConfig::load_with_credentials_config(&raw, &test_credentials_config(&dir))
                .unwrap_err();
        assert!(err.to_string().contains("ta credential add"));
    }

    #[test]
    fn empty_org_id_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut raw = backend_config("https://wayfinder.example.com");
        raw.org_id = String::new();
        let err =
            WayfinderPlanConfig::load_with_credentials_config(&raw, &test_credentials_config(&dir))
                .unwrap_err();
        assert!(err.to_string().contains("org_id"));
    }
}
