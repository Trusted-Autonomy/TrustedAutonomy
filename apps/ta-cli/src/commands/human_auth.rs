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
    let mut headers =
        credential_headers_from(&human::user_state_dir(), human::default_use_keychain());
    add_actor_context(&mut headers, human::detect_agent_context().as_ref());
    headers
}

/// Add the advisory `X-TA-Actor-Context` header when an agent context was
/// detected, so the daemon can label approve, deny and apply as
/// `agent_delegated` (CR-03 item 6). Absent means a direct human action. It is
/// for the audit trail, not a security control: an agent can omit it.
fn add_actor_context(headers: &mut reqwest::header::HeaderMap, ctx: Option<&human::AgentContext>) {
    let Some(ctx) = ctx else {
        return;
    };
    match reqwest::header::HeaderValue::from_str(&human::actor_context_header_value(ctx)) {
        Ok(value) => {
            headers.insert(
                reqwest::header::HeaderName::from_static(human::ACTOR_CONTEXT_HEADER),
                value,
            );
        }
        Err(e) => tracing::debug!(
            error = %e,
            "could not build the {} header; the daemon will treat this action as direct",
            human::ACTOR_CONTEXT_HEADER
        ),
    }
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

/// A delegated action: accepted from a detected agent context because the
/// credential holder's credential was present (CR-03 item 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Delegation {
    pub action: String,
    /// The credential holder, named as the delegating owner.
    pub holder: String,
    /// Why the context was judged to be an agent.
    pub signals: Vec<String>,
}

thread_local! {
    /// The action most recently accepted by `require_human_for` on this thread.
    /// Cleared at the start of every call, so a refused call proves nothing.
    static LAST_PROVEN: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Did `require_human_for(action)` succeed earlier on this thread? This is the
/// only source of "a person is present" for the rules table (v0.17.12.4): it
/// is never read from an environment variable or a goal's origin tag. In unit
/// tests, which call the apply internals directly, it is always true.
pub(crate) fn proven_human_for(action: &str) -> bool {
    #[cfg(test)]
    {
        let _ = action;
        true
    }
    #[cfg(not(test))]
    {
        LAST_PROVEN.with(|p| p.borrow().as_deref() == Some(action))
    }
}

thread_local! {
    /// The delegation accepted by the most recent `require_human_for` on this
    /// thread; cleared by every call so a direct action never inherits a label.
    static LAST_DELEGATION: std::cell::RefCell<Option<Delegation>> =
        const { std::cell::RefCell::new(None) };
}

/// The delegation accepted by the latest `require_human_for(action)` on this
/// thread, if that action was accepted from an agent context.
pub(crate) fn current_delegation(action: &str) -> Option<Delegation> {
    LAST_DELEGATION.with(|d| d.borrow().clone().filter(|d| d.action == action))
}

/// Refuse `action` (approve, deny or apply) when this looks like an agent and
/// no human credential is present. `Ok` for a person at a terminal, and for
/// anyone who has run `ta login` (see the module note on what that proves).
/// An agent context accepted on the credential is remembered as a
/// [`Delegation`] (see [`current_delegation`]) so the action is labeled.
pub(crate) fn require_human_for(action: &str) -> anyhow::Result<()> {
    LAST_DELEGATION.with(|d| *d.borrow_mut() = None);
    LAST_PROVEN.with(|p| *p.borrow_mut() = None);
    // Unit tests drive approve/apply in-process from whatever environment the
    // developer runs them in (often an agent session, often headless), so the
    // real detection is replaced by a per-thread setting: "no agent" unless a
    // test sets one with `test_support_set_gate`.
    #[cfg(test)]
    let delegation = {
        let (ctx, present) = TEST_GATE.with(|g| g.borrow().clone());
        require_human_with(action, ctx, || present, Path::new("<test>"), false)?
    };
    #[cfg(not(test))]
    let delegation = require_human_with(
        action,
        human::detect_agent_context(),
        credential_present,
        &human::user_state_dir(),
        true,
    )?;
    LAST_DELEGATION.with(|d| *d.borrow_mut() = delegation);
    LAST_PROVEN.with(|p| *p.borrow_mut() = Some(action.to_string()));
    Ok(())
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
    record_seen: bool,
) -> anyhow::Result<Option<Delegation>> {
    let Some(ctx) = context else {
        return Ok(None);
    };
    // Remember that an agent was seen, whether or not it is accepted: this
    // feeds the 'agent delegation active' line in `ta login status`/`ta status`.
    if record_seen && !daemon_already_recorded() {
        if let Err(e) = human::record_agent_context_seen(state_dir, &ctx) {
            tracing::warn!(
                path = %state_dir.display(),
                error = %e,
                "could not record that an agent context was seen; the 'agent delegation active' status line may be missing"
            );
        }
    }
    if credential_present() {
        let holder = human::credential_holder();
        tracing::info!(
            action,
            signals = ?ctx.signals,
            delegating_owner = %holder,
            "agent context detected; allowing {action} as agent_delegated because a human credential is present"
        );
        return Ok(Some(Delegation {
            action: action.to_string(),
            holder,
            signals: ctx.signals,
        }));
    }
    anyhow::bail!(
        "{} (credential directory checked: {})",
        human::refusal_message(action, &ctx),
        state_dir.display()
    )
}

/// True when the daemon spawned this process for a request it already labeled
/// (see `human::DAEMON_RECORDED_ENV`). A daemon-spawned
/// child is headless and would look like an agent, so it must not label or log
/// a sighting for an action a person took in Studio. Labels are advisory, so
/// this does not touch the approve/deny/apply gate itself.
fn daemon_already_recorded() -> bool {
    std::env::var(human::DAEMON_RECORDED_ENV).is_ok_and(|v| v == "1")
}

/// Stamp the delegation label onto the draft record and the audit log after an
/// approve, deny or apply that was accepted from an agent context. No-op for a
/// direct human action. Failures are reported but never undo the action.
pub(crate) fn record_delegation(
    config: &ta_mcp_gateway::GatewayConfig,
    draft_id: &str,
    action: &str,
) {
    let Some(d) = current_delegation(action) else {
        return;
    };
    if daemon_already_recorded() {
        tracing::debug!(
            action,
            "the daemon already recorded actor_context for this action; not labeling it again"
        );
        return;
    }
    let package_id = match super::draft::resolve_draft_id(draft_id, config) {
        Ok(id) => id,
        Err(e) => {
            eprintln!(
                "warning: {action} succeeded but the agent_delegated label could not be recorded: \
                 could not resolve draft '{draft_id}': {e}. The audit log entry is still written."
            );
            write_delegation_audit(config, &d, draft_id);
            return;
        }
    };
    match super::draft::load_package(config, package_id) {
        Ok(mut pkg) => {
            pkg.action_contexts.push(ta_changeset::ActionContextRecord {
                action: action.to_string(),
                actor_context: human::ACTOR_AGENT_DELEGATED.to_string(),
                delegating_owner: d.holder.clone(),
                delegated_signals: d.signals.clone(),
                at: chrono::Utc::now(),
            });
            if let Err(e) = super::draft::save_package(config, &pkg) {
                eprintln!(
                    "warning: {action} succeeded but the agent_delegated label could not be saved on draft {package_id}: {e}. \
                     Check write access to {} and re-run if the label matters.",
                    config.pr_packages_dir.display()
                );
            }
        }
        Err(e) => eprintln!(
            "warning: {action} succeeded but draft {package_id} could not be reloaded to record the agent_delegated label: {e}."
        ),
    }
    write_delegation_audit(config, &d, &package_id.to_string());
    println!(
        "  Recorded as agent_delegated: {} is the delegating owner ({}).",
        d.holder,
        d.signals.join("; ")
    );
}

fn write_delegation_audit(config: &ta_mcp_gateway::GatewayConfig, d: &Delegation, draft_id: &str) {
    let action = if d.action == "apply" {
        ta_audit::AuditAction::Apply
    } else {
        ta_audit::AuditAction::Approval
    };
    match ta_audit::AuditLog::open(&config.audit_log) {
        Ok(mut log) => {
            let mut event = ta_audit::AuditEvent::new(d.holder.clone(), action)
                .with_target(format!("draft://{draft_id}"))
                .with_metadata(delegation_metadata(d));
            if let Err(e) = log.append(&mut event) {
                tracing::warn!(path = %config.audit_log.display(), error = %e, "could not append the agent_delegated audit event");
            }
        }
        Err(e) => tracing::warn!(
            path = %config.audit_log.display(), error = %e,
            "could not open the audit log to record an agent_delegated action"
        ),
    }
}

/// Audit metadata for a delegated action, next to the credential holder.
pub(crate) fn delegation_metadata(d: &Delegation) -> serde_json::Value {
    serde_json::json!({
        "action": d.action,
        "actor_context": human::ACTOR_AGENT_DELEGATED,
        "delegating_owner": d.holder,
        "delegated_signals": d.signals,
        "credential": "human credential present",
    })
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
        let err = require_human_with("approve", ctx(), || false, dir.path(), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ta login"), "{err}");
        assert!(err.contains("! ta draft approve <id>"), "{err}");
        assert!(err.contains("CLAUDECODE is set"), "{err}");
    }

    #[test]
    fn agent_with_credential_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(require_human_with("apply", ctx(), || true, dir.path(), false).is_ok());
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
    fn agent_context_adds_the_advisory_actor_header_and_a_person_adds_none() {
        let mut headers = reqwest::header::HeaderMap::new();
        add_actor_context(&mut headers, None);
        assert!(headers.get(human::ACTOR_CONTEXT_HEADER).is_none());

        add_actor_context(&mut headers, ctx().as_ref());
        let sent = headers
            .get(human::ACTOR_CONTEXT_HEADER)
            .expect("header present")
            .to_str()
            .unwrap();
        assert!(sent.starts_with("agent_delegated; reasons="), "{sent}");
        assert_eq!(
            human::parse_actor_context_header(sent).unwrap(),
            vec!["CLAUDECODE is set"]
        );
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
        assert!(require_human_with("deny", None, || false, dir.path(), false).is_ok());
    }
}
