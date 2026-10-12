//! `ta login`: mint, inspect and revoke the human credential. `ta keys` registers
//! the public keys other surfaces use to assert a human identity, and
//! `ta show install` prints this install's id.
//!
//! The credential is stored outside every project tree: in the OS keychain
//! where one is available (an item bound to the signed `ta` binary), otherwise
//! in a mode-0600 file in the per-user state directory. While agents run as
//! your OS user this is a speed bump, not a wall: an agent that can read that
//! file can use it. The OS sandbox phases make it a wall.

use clap::Subcommand;
use ta_credentials::human;

#[derive(Debug, Subcommand)]
pub enum LoginCommands {
    /// Open the web UI in your browser, signed in with the human credential.
    ///
    /// The credential travels in the URL fragment (never sent to the server),
    /// is kept for that browser tab only, and is attached to the page's
    /// requests so approve, deny and apply from the UI keep working.
    Open,
    /// Show whether a human credential exists and where it is held.
    Status,
    /// Remove the human credential: approve/deny/apply from agent contexts stop working and approvals are Wayfinder-only.
    Logout,
}

#[derive(Debug, Subcommand)]
pub enum KeyCommands {
    /// Register the public key a signing system uses for human assertions.
    ///
    /// One key id (`kid`) per organization. The key file holds the base64
    /// 32-byte Ed25519 public key (standard or URL-safe), or the 32 raw bytes.
    Add {
        /// The key id, the `kid` in the assertion header.
        #[arg(long)]
        kid: String,
        /// File containing the public key.
        #[arg(long = "pub", value_name = "FILE")]
        pub_file: std::path::PathBuf,
        /// Roles that may act on "any maintainer" items (default: admin,owner).
        #[arg(long, value_delimiter = ',')]
        maintainer_roles: Option<Vec<String>>,
        /// Role ranks, lowest first; a higher role satisfies a lower one
        /// (default: member,admin,owner).
        #[arg(long, value_delimiter = ',')]
        role_order: Option<Vec<String>>,
        /// Free-text label (which system holds the private key).
        #[arg(long)]
        label: Option<String>,
    },
    /// List registered keys (key id and fingerprint, never key material).
    List,
    /// Remove a registered key by key id.
    Remove {
        /// The key id to remove.
        #[arg(long)]
        kid: String,
    },
}

pub fn execute(
    command: Option<&LoginCommands>,
    file: bool,
    project_root: &std::path::Path,
) -> anyhow::Result<()> {
    let state_dir = human::user_state_dir();
    let use_keychain = human::default_use_keychain();
    match command {
        None => login(&state_dir, use_keychain && !file),
        Some(LoginCommands::Open) => open_ui(&state_dir, use_keychain, project_root),
        Some(LoginCommands::Status) => status(&state_dir, use_keychain, project_root),
        Some(LoginCommands::Logout) => {
            if human::logout(&state_dir, use_keychain) {
                println!("Human credential removed from {}.", state_dir.display());
                println!(
                    "Approve, deny and apply from agent contexts are refused again; approvals \
                     are Wayfinder-only (attention feed and signed assertions)."
                );
                println!("Run `ta login` to create a new one.");
            } else {
                println!(
                    "No human credential found under {} (nothing to remove).",
                    state_dir.display()
                );
            }
            Ok(())
        }
    }
}

fn login(state_dir: &std::path::Path, use_keychain: bool) -> anyhow::Result<()> {
    let out = human::login(state_dir, use_keychain)?;
    println!("Human credential created.");
    println!("  Held in:   {}", out.custody.describe());
    println!("  Verifier:  {}", out.digest_path.display());
    if let Some(reason) = &out.keychain_fallback_reason {
        println!("  Note: the OS keychain was not usable ({reason}); used a file instead.");
    }
    println!();
    if matches!(out.custody, human::Custody::Keychain) {
        println!(
            "Editor plugins and scripts cannot read the keychain. For them run `ta login --file`"
        );
        println!("instead, or export TA_HUMAN_CREDENTIAL.");
        println!();
    }
    println!("The daemon now accepts approve, deny, apply, command execution and answers only");
    println!(
        "from callers that present this credential (the ta CLI and shell do so automatically)."
    );
    println!("Any earlier credential no longer works.");
    println!();
    println!("Honest limit: while agents run as your OS user this is a speed bump, not a wall.");
    println!("The OS sandbox phases make it a wall.");
    Ok(())
}

/// The URL `ta login open` launches: the UI with the credential in the fragment.
pub(crate) fn ui_url(daemon_url: &str, secret: &str) -> String {
    format!("{}/ui#ta_cred={}", daemon_url.trim_end_matches('/'), secret)
}

fn open_ui(
    state_dir: &std::path::Path,
    use_keychain: bool,
    project_root: &std::path::Path,
) -> anyhow::Result<()> {
    let Some(secret) = human::load_secret(state_dir, use_keychain) else {
        anyhow::bail!(
            "no human credential found under {}. Run `ta login` first, then `ta login open`.",
            state_dir.display()
        );
    };
    let daemon_url = super::daemon::resolve_daemon_url(project_root, None);
    let url = ui_url(&daemon_url, &secret);
    let (program, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url.as_str()])
    } else if cfg!(windows) {
        ("cmd", vec!["/C", "start", "", url.as_str()])
    } else {
        ("xdg-open", vec![url.as_str()])
    };
    let status = std::process::Command::new(program).args(&args).status();
    match status {
        Ok(s) if s.success() => {
            println!("Opened the web UI at {daemon_url}/ui (signed in for that tab).");
            Ok(())
        }
        other => anyhow::bail!(
            "could not launch a browser with `{program}` ({other:?}). Open {daemon_url}/ui yourself; \
             the UI will then show actions as refused until it has the credential."
        ),
    }
}

fn status(
    state_dir: &std::path::Path,
    use_keychain: bool,
    project_root: &std::path::Path,
) -> anyhow::Result<()> {
    println!("Credential directory: {}", state_dir.display());
    if human::credential_present(state_dir, use_keychain) {
        println!("Human credential: present and valid");
    } else {
        println!("Human credential: not present. Run `ta login` in your own terminal.");
    }
    match human::detect_agent_context() {
        Some(ctx) => println!(
            "Agent context:    detected ({}). Approve/deny/apply need the credential.",
            ctx.signals.join("; ")
        ),
        None => println!("Agent context:    not detected"),
    }
    if let Some(line) =
        delegation_line_for_project(project_root, state_dir, use_keychain, chrono::Utc::now())
    {
        println!("Delegation:       {line}");
    }
    let keys = human::load_assertion_keys(state_dir)?;
    println!(
        "Assertion keys:   {} registered (`ta keys list`; install id: `ta show install`)",
        keys.len()
    );
    Ok(())
}

/// The 'agent delegation active' line, or `None`. Shared by `ta login status`
/// and `ta status`; the clock is supplied so tests can pin it.
pub(crate) fn delegation_line(
    state_dir: &std::path::Path,
    use_keychain: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let sighting = human::agent_context_seen_within(
        state_dir,
        chrono::Duration::hours(human::DELEGATION_ACTIVE_WINDOW_HOURS),
        now,
    );
    human::delegation_status_line(
        human::credential_present(state_dir, use_keychain),
        sighting.as_ref(),
        now,
    )
}

/// The `agent_delegation` object from the daemon's `GET /api/status`, or `None`
/// when the daemon is unreachable or too old to report it. Short timeout: this
/// runs inside `ta status` and `ta login status`.
pub(crate) fn fetch_daemon_delegation(base_url: &str) -> Option<human::DelegationStatus> {
    let client = super::human_auth::blocking_client_builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let url = format!("{}/api/status", base_url.trim_end_matches('/'));
    let resp = match client.get(&url).send() {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::debug!(url = %url, status = %r.status(), "daemon status unavailable; using the local agent sighting file");
            return None;
        }
        Err(e) => {
            tracing::debug!(url = %url, error = %e, "daemon not reachable; using the local agent sighting file");
            return None;
        }
    };
    let json: serde_json::Value = resp.json().ok()?;
    serde_json::from_value(json.get("agent_delegation")?.clone()).ok()
}

/// The 'agent delegation active' line. The daemon's answer wins when it gave
/// one; otherwise (daemon unreachable or older) the local sighting file decides.
pub(crate) fn delegation_line_with(
    daemon: Option<&human::DelegationStatus>,
    state_dir: &std::path::Path,
    use_keychain: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    match daemon {
        Some(d) => d.line(now),
        None => delegation_line(state_dir, use_keychain, now),
    }
}

/// As [`delegation_line_with`], asking the project's daemon first.
pub(crate) fn delegation_line_for_project(
    project_root: &std::path::Path,
    state_dir: &std::path::Path,
    use_keychain: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let url = super::daemon::resolve_daemon_url(project_root, None);
    let daemon = fetch_daemon_delegation(&url);
    delegation_line_with(daemon.as_ref(), state_dir, use_keychain, now)
}

/// Read a public key file: base64 text (standard or URL-safe), or 32 raw bytes.
/// Returns the standard-alphabet base64 the registry stores.
fn read_public_key_file(path: &std::path::Path) -> anyhow::Result<String> {
    use base64::Engine;
    let bytes = std::fs::read(path).map_err(|e| {
        anyhow::anyhow!(
            "could not read the public key file {}: {e}. Check the path and that you can read it.",
            path.display()
        )
    })?;
    if bytes.len() == 32 {
        return Ok(base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes));
    }
    let text = String::from_utf8_lossy(&bytes);
    let text: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    let text = text.trim().to_string();
    match human::base64_decode(&text) {
        Some(raw) if raw.len() == 32 => Ok(text),
        Some(raw) => anyhow::bail!(
            "the public key in {} decodes to {} bytes; an Ed25519 public key is 32 bytes. \
             Pass the raw public key (base64 of 32 bytes), not a private key or certificate.",
            path.display(),
            raw.len()
        ),
        None => anyhow::bail!(
            "{} is not a base64 Ed25519 public key. Pass a file holding the base64 of the 32-byte \
             public key (or the 32 raw bytes).",
            path.display()
        ),
    }
}

/// `ta keys add | list | remove`
pub fn keys(command: &KeyCommands) -> anyhow::Result<()> {
    keys_in(&human::user_state_dir(), command)
}

pub(crate) fn keys_in(state_dir: &std::path::Path, command: &KeyCommands) -> anyhow::Result<()> {
    match command {
        KeyCommands::Add {
            kid,
            pub_file,
            maintainer_roles,
            role_order,
            label,
        } => {
            let public_key = read_public_key_file(pub_file)?;
            let key = human::add_assertion_key(
                state_dir,
                kid,
                &public_key,
                maintainer_roles.as_deref(),
                role_order.as_deref(),
                label.as_deref(),
            )?;
            println!(
                "Registered assertion key '{}' in {}.",
                key.kid,
                human::assertion_keys_path(state_dir).display()
            );
            println!("  Fingerprint:      {}", key.fingerprint());
            println!("  Maintainer roles: {}", key.maintainer_roles.join(", "));
            println!("  Role order:       {}", key.role_order.join(" < "));
            println!("  Escalation floor: {}", key.escalation_min_role);
            println!("The daemon reads the registry on each request; no restart is needed.");
            println!(
                "Signed assertions must name this install as aud: run `ta show install` for its id."
            );
        }
        KeyCommands::List => {
            let keys = human::load_assertion_keys(state_dir)?;
            if keys.is_empty() {
                println!(
                    "No assertion keys registered. Add one with `ta keys add --kid <kid> --pub <file>`."
                );
            }
            for k in keys {
                println!(
                    "  {}  fingerprint={}  maintainer_roles={}  role_order={}  escalation_min_role={}  label={}  registered={}",
                    k.kid,
                    k.fingerprint(),
                    k.maintainer_roles.join(","),
                    k.role_order.join(","),
                    k.escalation_min_role,
                    k.label.as_deref().unwrap_or("-"),
                    k.registered_at.to_rfc3339()
                );
            }
        }
        KeyCommands::Remove { kid } => {
            if human::remove_assertion_key(state_dir, kid)? {
                println!("Removed assertion key '{kid}'.");
            } else {
                println!(
                    "No assertion key '{kid}' is registered (nothing removed). `ta keys list` shows what is."
                );
            }
        }
    }
    Ok(())
}

/// `ta show install`: the install id and registered key ids.
pub fn show_install() -> anyhow::Result<()> {
    show_install_in(&human::user_state_dir())
}

pub(crate) fn show_install_in(state_dir: &std::path::Path) -> anyhow::Result<()> {
    let id = human::install_id(state_dir)?;
    println!("Install id:     {id}");
    println!("State directory: {}", state_dir.display());
    let keys = human::load_assertion_keys(state_dir)?;
    println!("Assertion keys: {} registered", keys.len());
    for k in keys {
        println!("  {}  {}", k.kid, k.fingerprint());
    }
    println!();
    println!("Give the install id to the system that signs human assertions; it goes in the");
    println!("`aud` claim. The same id and the key ids are returned by GET /api/attention.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_file(dir: &std::path::Path, contents: &[u8]) -> std::path::PathBuf {
        let p = dir.join("wf.pub");
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn keys_add_list_remove_round_trip_with_defaults_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let file = key_file(dir.path(), "A".repeat(43).as_bytes());
        keys_in(
            &state,
            &KeyCommands::Add {
                kid: "org-1".into(),
                pub_file: file.clone(),
                maintainer_roles: None,
                role_order: None,
                label: Some("wayfinder".into()),
            },
        )
        .unwrap();
        let k = human::load_assertion_keys(&state).unwrap();
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].maintainer_roles, vec!["admin", "owner"]);
        assert_eq!(k[0].role_order, vec!["member", "admin", "owner"]);

        keys_in(
            &state,
            &KeyCommands::Add {
                kid: "org-1".into(),
                pub_file: file,
                maintainer_roles: Some(vec!["owner".into()]),
                role_order: Some(vec!["dev".into(), "owner".into()]),
                label: None,
            },
        )
        .unwrap();
        let k = human::load_assertion_keys(&state).unwrap();
        assert_eq!(k.len(), 1, "same kid replaces");
        assert_eq!(k[0].maintainer_roles, vec!["owner"]);
        assert_eq!(k[0].role_order, vec!["dev", "owner"]);

        keys_in(&state, &KeyCommands::List).unwrap();
        keys_in(
            &state,
            &KeyCommands::Remove {
                kid: "org-1".into(),
            },
        )
        .unwrap();
        assert!(human::load_assertion_keys(&state).unwrap().is_empty());
        // Removing again is not an error, it says so.
        keys_in(
            &state,
            &KeyCommands::Remove {
                kid: "org-1".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn key_files_accept_base64_raw_bytes_and_refuse_wrong_sizes_actionably() {
        let dir = tempfile::tempdir().unwrap();
        let b64 = read_public_key_file(&key_file(
            dir.path(),
            b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
        ))
        .unwrap();
        assert_eq!(b64.len(), 43);
        let raw = read_public_key_file(&key_file(dir.path(), &[7u8; 32])).unwrap();
        assert_eq!(human::base64_decode(&raw).unwrap(), vec![7u8; 32]);
        let err = read_public_key_file(&key_file(dir.path(), b"AAAA"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("32 bytes") && err.contains("raw public key"),
            "{err}"
        );
        let err = read_public_key_file(&dir.path().join("missing.pub"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Check the path"), "{err}");
    }

    #[test]
    fn show_install_creates_a_stable_id() {
        let dir = tempfile::tempdir().unwrap();
        show_install_in(dir.path()).unwrap();
        let id = human::install_id(dir.path()).unwrap();
        show_install_in(dir.path()).unwrap();
        assert_eq!(human::install_id(dir.path()).unwrap(), id);
    }

    #[test]
    fn ui_url_puts_the_credential_in_the_fragment_only() {
        let url = ui_url("http://127.0.0.1:7700/", "tahc_abc");
        assert_eq!(url, "http://127.0.0.1:7700/ui#ta_cred=tahc_abc");
        // The part a server (and its logs) sees has no secret in it.
        assert!(!url.split('#').next().unwrap().contains("tahc_"));
    }

    #[test]
    fn delegation_line_follows_credential_and_recency() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        let ctx = human::AgentContext {
            signals: vec!["CLAUDECODE is set".into()],
        };
        // No credential: never shown, even with a fresh sighting.
        human::record_agent_context_seen_at(dir.path(), &ctx, now - chrono::Duration::hours(1))
            .unwrap();
        assert!(delegation_line(dir.path(), false, now).is_none());
        // Credential present and agent seen 1h ago: shown.
        human::login(dir.path(), false).unwrap();
        let line = delegation_line(dir.path(), false, now).expect("line shown");
        assert!(line.contains("agent delegation active"), "{line}");
        assert!(line.contains("CLAUDECODE is set"), "{line}");
        assert!(line.contains("ta login logout"), "{line}");
        // Seen 25h ago: not shown.
        human::record_agent_context_seen_at(dir.path(), &ctx, now - chrono::Duration::hours(25))
            .unwrap();
        assert!(delegation_line(dir.path(), false, now).is_none());
        // Logout switches back: no credential, no line.
        human::record_agent_context_seen_at(dir.path(), &ctx, now).unwrap();
        assert!(delegation_line(dir.path(), false, now).is_some());
        human::logout(dir.path(), false);
        assert!(delegation_line(dir.path(), false, now).is_none());
    }

    /// Serve one canned `GET /api/status` response; returns the base URL.
    fn fake_daemon(body: serde_json::Value) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf);
                let payload = body.to_string();
                let _ = write!(
                    sock,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn delegation_line_uses_the_daemon_when_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        // Local state says nothing (no credential, no sighting) ...
        assert!(delegation_line(dir.path(), false, now).is_none());
        // ... but the daemon reports an active delegation, and its answer wins.
        let url = fake_daemon(serde_json::json!({
            "agent_delegation": {
                "active": true,
                "last_seen": (now - chrono::Duration::minutes(5)).to_rfc3339(),
                "credential_present": true,
                "reasons": ["TA_CHAT_MODE is set"]
            }
        }));
        let daemon = fetch_daemon_delegation(&url).expect("daemon answered");
        let line = delegation_line_with(Some(&daemon), dir.path(), false, now).unwrap();
        assert!(line.contains("agent delegation active"), "{line}");
        assert!(line.contains("TA_CHAT_MODE is set"), "{line}");
        assert!(line.contains("ta login logout"), "{line}");
    }

    #[test]
    fn an_inactive_daemon_answer_hides_the_line_even_if_a_local_file_says_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        human::login(dir.path(), false).unwrap();
        let ctx = human::AgentContext {
            signals: vec!["CLAUDECODE is set".into()],
        };
        human::record_agent_context_seen_at(dir.path(), &ctx, now).unwrap();
        let url = fake_daemon(serde_json::json!({
            "agent_delegation": {"active": false, "last_seen": null, "credential_present": false}
        }));
        let daemon = fetch_daemon_delegation(&url).unwrap();
        assert!(delegation_line_with(Some(&daemon), dir.path(), false, now).is_none());
    }

    #[test]
    fn delegation_line_falls_back_to_the_local_file_when_the_daemon_is_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        human::login(dir.path(), false).unwrap();
        let ctx = human::AgentContext {
            signals: vec!["CLAUDECODE is set".into()],
        };
        human::record_agent_context_seen_at(dir.path(), &ctx, now - chrono::Duration::hours(2))
            .unwrap();
        // Nothing listens on this port: bind, note the port, drop the listener.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let daemon = fetch_daemon_delegation(&format!("http://127.0.0.1:{port}"));
        assert!(daemon.is_none());
        let line = delegation_line_with(daemon.as_ref(), dir.path(), false, now).unwrap();
        assert!(line.contains("agent delegation active"), "{line}");
        assert!(line.contains("2 hour(s) ago"), "{line}");
    }

    #[test]
    fn an_older_daemon_without_the_field_falls_back_too() {
        let url = fake_daemon(serde_json::json!({"version": "0.17.0"}));
        assert!(fetch_daemon_delegation(&url).is_none());
    }
}
