//! The human credential this plugin presents to the daemon.
//!
//! The daemon refuses approve, deny, apply, command execution and answering an
//! agent question from any caller without it (loopback included). A person
//! creates it once with `ta login --file`; this plugin then reads it from
//! `$TA_HUMAN_CREDENTIAL` or the owner-only file in the per-user state
//! directory (`$TA_USER_STATE_DIR`, else `~/.config/ta/human`). When absent the
//! daemon's refusal says "run `ta login`".

use std::path::{Path, PathBuf};

/// Header the daemon reads the credential from.
pub const CREDENTIAL_HEADER: &str = "x-ta-human-credential";

/// Advisory header telling the daemon this plugin runs under an agent, so
/// approvals it relays are labeled `agent_delegated` in the audit trail. Audit
/// only, not a security control (an agent can unset the variables).
pub const ACTOR_CONTEXT_HEADER: &str = "x-ta-actor-context";

/// Environment variables whose presence marks an agent launch (the same list
/// the `ta` CLI uses).
const AGENT_MARKER_VARS: &[&str] = &[
    "TA_AGENT_CONTEXT",
    "TA_IS_STAGING",
    "TA_AGENT_ID",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
];
const MAX_REASONS: usize = 5;

/// The `X-TA-Actor-Context` value for this environment, or `None` for a person.
/// Format: `agent_delegated; reasons=<percent-encoded>,<...>`.
pub fn actor_context_value(env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut reasons: Vec<String> = AGENT_MARKER_VARS
        .iter()
        .filter(|v| env(v).is_some_and(|x| !x.is_empty()))
        .map(|v| format!("{v} is set"))
        .collect();
    if let Some(mode) = env("TA_CALLER_MODE").filter(|m| !m.is_empty() && m != "normal") {
        reasons.push(format!("TA_CALLER_MODE={mode}"));
    }
    if env("TA_CHAT_MODE").is_some_and(|v| !v.is_empty()) {
        reasons.push("TA_CHAT_MODE is set".to_string());
    }
    if reasons.is_empty() {
        return None;
    }
    let encoded: Vec<String> = reasons
        .iter()
        .take(MAX_REASONS)
        .map(|r| {
            r.chars()
                .take(60)
                .collect::<String>()
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'=') {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect()
        })
        .collect();
    Some(format!("agent_delegated; reasons={}", encoded.join(",")))
}

fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TA_USER_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config").join("ta").join("human")
}

/// The credential, from the environment or the file in `dir`.
pub fn load_from(env_value: Option<String>, dir: &Path) -> Option<String> {
    env_value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::fs::read_to_string(dir.join("credential"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// An HTTP client that presents the credential (when one exists) on every request.
pub fn client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(secret) = load_from(std::env::var("TA_HUMAN_CREDENTIAL").ok(), &state_dir()) {
        if let Ok(mut v) = reqwest::header::HeaderValue::from_str(&secret) {
            v.set_sensitive(true);
            headers.insert(
                reqwest::header::HeaderName::from_static(CREDENTIAL_HEADER),
                v,
            );
        }
    }
    if let Some(value) = actor_context_value(&|k| std::env::var(k).ok()) {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&value) {
            headers.insert(
                reqwest::header::HeaderName::from_static(ACTOR_CONTEXT_HEADER),
                v,
            );
        }
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins_then_file_then_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_from(None, dir.path()), None);
        std::fs::write(dir.path().join("credential"), "tahc_file\n").unwrap();
        assert_eq!(load_from(None, dir.path()).as_deref(), Some("tahc_file"));
        assert_eq!(
            load_from(Some(" tahc_env ".into()), dir.path()).as_deref(),
            Some("tahc_env")
        );
        assert_eq!(
            load_from(Some("  ".into()), dir.path()).as_deref(),
            Some("tahc_file")
        );
    }

    #[test]
    fn actor_context_header_only_for_agent_launches() {
        assert_eq!(actor_context_value(&|_| None), None);
        assert_eq!(
            actor_context_value(&|k| (k == "TA_CALLER_MODE").then(|| "normal".to_string())),
            None
        );
        let v = actor_context_value(&|k| (k == "CLAUDECODE").then(|| "1".to_string())).unwrap();
        assert_eq!(v, "agent_delegated; reasons=CLAUDECODE%20is%20set");
        let v =
            actor_context_value(&|k| (k == "TA_CALLER_MODE").then(|| "orchestrator".to_string()))
                .unwrap();
        assert!(v.contains("TA_CALLER_MODE=orchestrator"), "{v}");
    }
}
