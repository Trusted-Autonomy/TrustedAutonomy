// secret.rs — a small newtype so the Wayfinder service-account bearer
// secret can never leak through an accidental `{:?}`/`{}` in a log line,
// error message, or panic payload. The secret is a long-lived (until
// explicitly revoked) credential — unlike `session_token`'s 24h expiry, a
// leaked copy of this one stays valid until someone notices and revokes it,
// so the bar for "never printed" is higher here than for most strings this
// codebase handles.

use std::fmt;

/// A Wayfinder `service_account_token` secret (`wfsa_<64 hex>`). Holds the
/// real value only long enough to build an `Authorization: Bearer` header;
/// every `Debug`/`Display` impl prints a fixed placeholder instead of the
/// contents, so `tracing::debug!(?config)` or an `anyhow` context string
/// built from `{:?}` can't accidentally include it.
#[derive(Clone)]
pub struct RedactedSecret(String);

impl RedactedSecret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The real value, for building an auth header. Named `expose_` rather
    /// than a `Deref`/`AsRef` impl so every call site is a visible,
    /// grep-able moment where the secret leaves this type.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedactedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RedactedSecret(<redacted>)")
    }
}

impl fmt::Display for RedactedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_never_print_the_secret() {
        let secret = RedactedSecret::new("wfsa_super_secret_value".to_string());
        assert!(!format!("{:?}", secret).contains("super_secret_value"));
        assert!(!format!("{}", secret).contains("super_secret_value"));
    }

    #[test]
    fn expose_secret_returns_the_real_value() {
        let secret = RedactedSecret::new("wfsa_super_secret_value".to_string());
        assert_eq!(secret.expose_secret(), "wfsa_super_secret_value");
    }
}
