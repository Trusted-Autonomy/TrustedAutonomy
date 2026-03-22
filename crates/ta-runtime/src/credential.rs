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
}

impl ScopedCredential {
    /// Construct a minimal credential with no scope restrictions.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            scopes: Vec::new(),
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
        }
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
    }
}
