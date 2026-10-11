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
        assert_eq!(load_from(Some("  ".into()), dir.path()).as_deref(), Some("tahc_file"));
    }
}
