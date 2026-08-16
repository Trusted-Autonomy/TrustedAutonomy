// credential.rs — Scoped credential type for runtime injection.
//
// When TA injects credentials into a runtime, it doesn't give the agent the
// raw vault key. Instead, it issues a ScopedCredential: a short-lived token
// or value that is valid only for the declared scopes (operations the agent
// is allowed to perform with this credential).
//
// The RuntimeAdapter is responsible for delivering these into the agent's
// environment in a backend-specific way:
//   - BareProcess: environment variables at spawn time
//   - OCI: mounted secrets file or container env (set during container start)
//   - VM: secure channel post-boot (e.g., virtio-vsock or MMIO region)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A scoped, short-lived credential to be injected into an agent runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedCredential {
    /// Human-readable name (e.g., "ANTHROPIC_API_KEY", "GITHUB_TOKEN").
    pub name: String,

    /// The credential value (token, password, certificate, etc.).
    pub value: String,

    /// Capability scopes this credential authorises (e.g., ["gmail.send"]).
    ///
    /// The agent sees the credential but TA's policy layer limits what it
    /// can do with it to these declared scopes.
    pub scopes: Vec<String>,

    /// Whether this credential is declared `broker_mediated` by a
    /// `ConnectorRegistry` entry (v0.17.6.3). When `true`,
    /// `apply_credentials_to_env` withholds `value` from the agent's direct
    /// environment entirely — the agent must instead present
    /// `session_token_id` to `ta_external_action`, and the gateway broker
    /// resolves the real secret server-side. Defaults to `false`
    /// (unchanged, reduced-security env-injection behavior) so declaring a
    /// connector doesn't silently change any existing credential's
    /// delivery path.
    #[serde(default)]
    pub broker_mediated: bool,

    /// The `SessionToken.token_id` (as a string) minted for this
    /// credential, present only when `broker_mediated` is `true`. This is
    /// the only credential-shaped value the agent ever receives for a
    /// broker-mediated connector — an opaque, independently-verifiable
    /// reference, never `value` itself.
    #[serde(default)]
    pub session_token_id: Option<String>,

    /// The moment `session_token_id` stops being cryptographically valid
    /// (v0.17.6.5), present only when known. Informational, not enforced
    /// here: the real bound is the `check if time(...)` clause embedded in
    /// the token itself, evaluated by `CredentialBroker`. Carried alongside
    /// the token purely so a downstream spawn that inherits this credential
    /// (e.g. a nested swarm sub-goal attenuating it further) can compute
    /// `min(parent_remaining_ttl, ...)` without decoding the token, via the
    /// `TA_SESSION_TOKEN_<name>_EXPIRES_AT` sibling env var
    /// `apply_credentials_to_env` injects when this is `Some`.
    #[serde(default)]
    pub session_token_expires_at: Option<DateTime<Utc>>,
}

impl ScopedCredential {
    /// Construct a minimal credential with no scope restrictions.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            scopes: Vec::new(),
            broker_mediated: false,
            session_token_id: None,
            session_token_expires_at: None,
        }
    }

    /// Construct a credential with explicit scopes.
    pub fn with_scopes(
        name: impl Into<String>,
        value: impl Into<String>,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            scopes,
            broker_mediated: false,
            session_token_id: None,
            session_token_expires_at: None,
        }
    }

    /// Mark this credential broker-mediated (v0.17.6.3): `value` is
    /// withheld from the agent's direct environment by
    /// `apply_credentials_to_env`, and `session_token_id` is delivered
    /// instead.
    pub fn with_broker_mediation(mut self, session_token_id: impl Into<String>) -> Self {
        self.broker_mediated = true;
        self.session_token_id = Some(session_token_id.into());
        self
    }

    /// Attach the token's expiry (v0.17.6.5) so a downstream inheritor can
    /// compute its own attenuation TTL. No effect unless
    /// `with_broker_mediation` was also called.
    pub fn with_expiry(mut self, expires_at: DateTime<Utc>) -> Self {
        self.session_token_expires_at = Some(expires_at);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_empty_scopes() {
        let cred = ScopedCredential::new("TOKEN", "abc123");
        assert_eq!(cred.name, "TOKEN");
        assert_eq!(cred.value, "abc123");
        assert!(cred.scopes.is_empty());
    }

    #[test]
    fn with_scopes_retains_scopes() {
        let cred = ScopedCredential::with_scopes(
            "GITHUB_TOKEN",
            "ghp_xyz",
            vec!["repo.read".into(), "issues.write".into()],
        );
        assert_eq!(cred.scopes.len(), 2);
        assert_eq!(cred.scopes[0], "repo.read");
        assert!(!cred.broker_mediated);
        assert!(cred.session_token_id.is_none());
    }

    #[test]
    fn with_broker_mediation_sets_flag_and_token_clears_no_value_change() {
        let cred = ScopedCredential::new("GITHUB_TOKEN", "ghp_real_secret")
            .with_broker_mediation("11111111-1111-1111-1111-111111111111");
        assert!(cred.broker_mediated);
        assert_eq!(
            cred.session_token_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        // `value` is untouched by this call -- callers that mark a
        // credential broker-mediated are expected to also blank/ignore
        // `value` themselves before handing it to `apply_credentials_to_env`,
        // which is what actually enforces the withholding.
        assert_eq!(cred.value, "ghp_real_secret");
    }
}
