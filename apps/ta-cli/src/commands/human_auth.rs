//! The CLI side of the human credential: presenting it to the daemon and
//! refusing approve/deny/apply under an agent without it.
//!
//! The credential is minted by `ta login` and lives outside every project tree
//! (see `ta_credentials::human`). While agents run as the same OS user this is
//! a speed bump, not a wall; the OS sandbox phases make it a wall.

use std::path::Path;

use ta_credentials::human;

/// Whether a usable credential exists (secret loads and verifies).
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn credential_present() -> bool {
    human::credential_present(&human::user_state_dir(), human::default_use_keychain())
}

/// Default headers carrying the credential (empty when not logged in).
fn credential_headers() -> reqwest::header::HeaderMap {
    credential_headers_from(&human::user_state_dir(), human::default_use_keychain())
}

fn credential_headers_from(state_dir: &Path, use_keychain: bool) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(secret) = human::load_secret(state_dir, use_keychain) {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&secret) {
            let mut value = value;
            value.set_sensitive(true);
            headers.insert(
                reqwest::header::HeaderName::from_static(human::CREDENTIAL_HEADER),
                value,
            );
        }
    }
    headers
}

/// Async client that presents the human credential on every request.
pub(crate) fn async_client() -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers(credential_headers())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Blocking client builder that presents the human credential.
pub(crate) fn blocking_client_builder() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder().default_headers(credential_headers())
}

/// Refuse `action` (approve, deny or apply) when this looks like an agent and
/// no human credential is present. `Ok` for a person at a terminal, and for
/// anyone who has run `ta login` (see the module note on what that proves).
pub(crate) fn require_human_for(action: &str) -> anyhow::Result<()> {
    // Unit tests drive approve/apply in-process from whatever environment the
    // developer runs them in (often an agent session, often headless), so the
    // real detection is replaced by a per-thread setting: "no agent" unless a
    // test sets one with `test_support_set_gate`.
    #[cfg(test)]
    {
        let (ctx, present) = TEST_GATE.with(|g| g.borrow().clone());
        require_human_with(action, ctx, || present, Path::new("<test>"))
    }
    #[cfg(not(test))]
    require_human_with(
        action,
        human::detect_agent_context(),
        credential_present,
        &human::user_state_dir(),
    )
}

#[cfg(test)]
thread_local! {
    static TEST_GATE: std::cell::RefCell<(Option<human::AgentContext>, bool)> =
        const { std::cell::RefCell::new((None, false)) };
}

/// Test hook: the agent context and credential presence `require_human_for`
/// sees on this thread.
#[cfg(test)]
pub(crate) fn test_support_set_gate(ctx: Option<human::AgentContext>, credential_present: bool) {
    TEST_GATE.with(|g| *g.borrow_mut() = (ctx, credential_present));
}

fn require_human_with(
    action: &str,
    context: Option<human::AgentContext>,
    credential_present: impl Fn() -> bool,
    state_dir: &Path,
) -> anyhow::Result<()> {
    let Some(ctx) = context else {
        return Ok(());
    };
    if credential_present() {
        tracing::info!(
            action,
            signals = ?ctx.signals,
            "agent context detected; allowing {action} because a human credential is present"
        );
        return Ok(());
    }
    anyhow::bail!(
        "{} (credential directory checked: {})",
        human::refusal_message(action, &ctx),
        state_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Option<human::AgentContext> {
        Some(human::AgentContext {
            signals: vec!["CLAUDECODE is set".into()],
        })
    }

    #[test]
    fn agent_without_credential_is_refused_with_the_human_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = require_human_with("approve", ctx(), || false, dir.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("ta login"), "{err}");
        assert!(err.contains("! ta draft approve <id>"), "{err}");
        assert!(err.contains("CLAUDECODE is set"), "{err}");
    }

    #[test]
    fn agent_with_credential_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(require_human_with("apply", ctx(), || true, dir.path()).is_ok());
    }

    #[test]
    fn logged_in_cli_presents_the_credential_header_the_daemon_checks() {
        let dir = tempfile::tempdir().unwrap();
        assert!(credential_headers_from(dir.path(), false).is_empty());
        human::login(dir.path(), false).unwrap();
        let headers = credential_headers_from(dir.path(), false);
        let sent = headers
            .get(human::CREDENTIAL_HEADER)
            .expect("credential header present after login")
            .to_str()
            .unwrap()
            .to_string();
        assert!(human::verify_secret(dir.path(), &sent));
        assert!(headers
            .get(human::CREDENTIAL_HEADER)
            .unwrap()
            .is_sensitive());
    }

    #[test]
    fn the_dispatch_level_guard_follows_the_test_hook() {
        test_support_set_gate(ctx(), false);
        assert!(require_human_for("approve").is_err());
        test_support_set_gate(ctx(), true);
        assert!(require_human_for("approve").is_ok());
        test_support_set_gate(None, false);
        assert!(require_human_for("approve").is_ok());
    }

    #[test]
    fn a_person_at_a_terminal_needs_no_credential() {
        let dir = tempfile::tempdir().unwrap();
        assert!(require_human_with("deny", None, || false, dir.path()).is_ok());
    }
}
