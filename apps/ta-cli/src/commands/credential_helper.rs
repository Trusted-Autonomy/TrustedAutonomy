// credential_helper.rs — `ta credential-helper`: a git credential.helper
// backend for broker-mediated connectors (v0.17.6.7, PLAN item 1).
//
// git already supports pluggable credential helpers (see
// `gitcredentials(7)`): `credential.helper` is invoked as
// `<helper> <get|store|erase>` with a `key=value\n`-per-line request on
// stdin, and (for `get`) responds with `key=value\n` lines on stdout. No
// change to git's own behavior is needed — this just points
// `credential.helper` at this subcommand for repos where a connector
// declares a matching `hosts` entry in `.ta/connectors.toml`.
//
// The raw secret only ever exists in this short-lived helper process's own
// memory and in git's stdin/stdout pipe to it — the agent's own persistent
// shell environment and the LLM's context never see it, unlike the
// reduced-security `bare_process.rs` env-injection fallback this replaces
// for broker-mediated connectors.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

use ta_credential_broker::{resolve_for_host, ShimError};

/// Parse a git credential-protocol request body (`key=value\n` lines,
/// terminated by a blank line or EOF — `gitcredentials(7)`).
///
/// Pure function: no I/O, so the protocol parsing is directly testable
/// without spawning a process or faking stdin.
fn parse_request(body: &str) -> HashMap<String, String> {
    body.lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// git's `host=` field may carry a port (`github.com:443`); connectors
/// declare bare hostnames, so strip it before matching.
fn host_without_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

/// Format a successful `get` response. Git expects `username`/`password`
/// (or just `password`, but a username makes the resulting log/URL more
/// legible) followed by a blank-line-equivalent EOF — git reads until the
/// pipe closes, so no explicit trailing blank line is required.
fn format_response(connector_id: &str, secret: &str) -> String {
    format!("username={connector_id}\npassword={secret}\n")
}

/// Run `ta credential-helper <operation>` for git.
///
/// `operation` is `get`, `store`, or `erase` (whatever git passes as
/// `argv[1]`). Only `get` produces output; `store`/`erase` read and discard
/// their input and exit cleanly — the broker is the sole source of truth
/// for credential lifecycle, so there is nothing for this helper to persist
/// or delete on disk (unlike git's built-in `store`/`cache` helpers).
///
/// A host that no broker-mediated connector declares (or a missing/invalid
/// session token) is **not** an error: `get` silently produces no output,
/// exactly as git's own protocol expects when a helper "doesn't know" a
/// credential — git then falls through to its next configured helper or its
/// interactive prompt. Only a genuine broker/vault failure (unreadable
/// vault, mismatched credential) is surfaced as an error, since that's a
/// misconfiguration worth the operator seeing rather than a silent stall.
/// `use_keychain` should be `true` for every real invocation (the default
/// call site in `main.rs` always passes `true`); `false` only in tests, to
/// match the vault fixture's own `CredentialsConfig::use_keychain = false`
/// (see that field's doc comment for why tests must not touch the real OS
/// keychain).
pub fn execute(
    operation: &str,
    project_root: &Path,
    reader: &mut impl Read,
    writer: &mut impl Write,
    use_keychain: bool,
) -> anyhow::Result<()> {
    let mut body = String::new();
    reader.read_to_string(&mut body)?;

    if operation != "get" {
        // `store` / `erase`: consciously a no-op (see doc comment above).
        return Ok(());
    }

    let fields = parse_request(&body);
    let Some(host) = fields.get("host") else {
        return Ok(());
    };
    let host = host_without_port(host);

    match resolve_for_host(project_root, host, use_keychain) {
        Ok(resolution) => {
            write!(
                writer,
                "{}",
                format_response(&resolution.connector_id, &resolution.secret)
            )?;
            Ok(())
        }
        Err(ShimError::NoConnector(_)) | Err(ShimError::NoSessionToken { .. }) => {
            // Silent fallback — see doc comment above.
            Ok(())
        }
        Err(e) => {
            tracing::warn!(host, error = %e, "ta credential-helper: broker-mediated lookup failed");
            Err(e.into())
        }
    }
}

/// Resolve the project root a credential-helper invocation should use:
/// `TA_PROJECT_ROOT` (set at agent spawn — see `run.rs::install_credential_shims`)
/// if present, otherwise `fallback` (the parsed `--project-root`, itself
/// defaulted to the current working directory). Mirrors
/// `commands::serve::execute`'s existing `TA_PROJECT_ROOT` handling — git
/// invokes this helper with its own CWD set to wherever the agent ran `git`
/// from, which is very often a subdirectory of the staging workspace, not
/// the project root `.ta/` actually lives under.
pub fn resolve_project_root(fallback: &Path) -> std::path::PathBuf {
    std::env::var("TA_PROJECT_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| fallback.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use ta_credentials::CredentialVault;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parse_request_reads_key_value_lines_until_blank() {
        let body = "protocol=https\nhost=github.com\n\nusername=ignored\n";
        let fields = parse_request(body);
        assert_eq!(fields.get("protocol").map(String::as_str), Some("https"));
        assert_eq!(fields.get("host").map(String::as_str), Some("github.com"));
        // Everything after the blank line is a new (unrelated) request and
        // must not leak into this one.
        assert!(!fields.contains_key("username"));
    }

    #[test]
    fn host_without_port_strips_trailing_port() {
        assert_eq!(host_without_port("github.com:443"), "github.com");
        assert_eq!(host_without_port("github.com"), "github.com");
    }

    #[test]
    fn get_for_broker_mediated_host_prints_credential_protocol_response() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta/connectors.toml"),
            "[connectors.github]\ncredential_name = \"GITHUB_TOKEN\"\n\
             broker_mediated = true\nhosts = [\"github.com\"]\n",
        )
        .unwrap();

        let mut cred_config = ta_credentials::CredentialsConfig::for_project(dir.path());
        cred_config.use_keychain = false;
        let mut vault = ta_credentials::FileVault::open(&cred_config).unwrap();
        let cred = vault
            .add("GITHUB_TOKEN", "github", "ghp_real_secret", vec![])
            .unwrap();
        let broker = ta_credential_broker::CredentialBroker::open(&dir.path().join(".ta")).unwrap();
        let granted = broker.grant(cred.id, "agent-1", vec![], 3600).unwrap();

        std::env::set_var("TA_SESSION_TOKEN_GITHUB_TOKEN", &granted.token);
        let mut input = std::io::Cursor::new("protocol=https\nhost=github.com\n\n");
        let mut output = Vec::new();
        let result = execute("get", dir.path(), &mut input, &mut output, false);
        std::env::remove_var("TA_SESSION_TOKEN_GITHUB_TOKEN");

        result.unwrap();
        let response = String::from_utf8(output).unwrap();
        assert!(response.contains("password=ghp_real_secret"));
        assert!(!response.is_empty());
    }

    #[test]
    fn get_for_unlisted_host_produces_no_output_and_no_error() {
        let dir = TempDir::new().unwrap();
        let mut input = std::io::Cursor::new("protocol=https\nhost=gitlab.com\n\n");
        let mut output = Vec::new();
        execute("get", dir.path(), &mut input, &mut output, false).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn store_and_erase_are_no_ops() {
        let dir = TempDir::new().unwrap();
        for op in ["store", "erase"] {
            let mut input = std::io::Cursor::new("protocol=https\nhost=github.com\n\n");
            let mut output = Vec::new();
            execute(op, dir.path(), &mut input, &mut output, false).unwrap();
            assert!(output.is_empty());
        }
    }
}
