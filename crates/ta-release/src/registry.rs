//! `ReleaseAdapterRegistry` — URL-scheme adapter discovery.
//!
//! Modeled on the `ta-db-proxy` registry (`crates/ta-db-proxy/src/registry.rs`) — the
//! same "core has zero awareness of which specific backends exist" pattern, applied to
//! release publish targets. See `docs/release-design.md` §6.
//!
//! Resolution order:
//!   1. `--adapter <name>` CLI override
//!   2. `publish_url` scheme against built-in adapters (`s3://`, `sftp://`, `file://` →
//!      `RemoteFileReleaseAdapter`)
//!   3. `publish_url` scheme against discovered plugin adapters (deferred to v0.17.4)
//!   4. no `publish_url` at all → `GitHubReleaseAdapter` if a git remote is configured,
//!      else an actionable error

use crate::adapter::ReleaseAdapter;
use crate::adapters::{GitHubReleaseAdapter, RemoteFileReleaseAdapter};
use crate::error::{ReleaseError, Result};

/// Resolve a `Box<dyn ReleaseAdapter>` from an explicit override name, a `publish_url`,
/// or (if neither is given) a git-remote fallback to `GitHubReleaseAdapter`.
///
/// `has_git_remote` is passed in by the caller (e.g. `apps/ta-cli` already knows how to
/// check for a git remote via `ta-submit`/`release_git`) rather than this crate shelling
/// out to git itself — keeps `ta-release` free of VCS-specific process calls.
pub fn resolve(
    adapter_override: Option<&str>,
    publish_url: Option<&str>,
    has_git_remote: bool,
) -> Result<Box<dyn ReleaseAdapter>> {
    if let Some(name) = adapter_override {
        return resolve_by_name(name, publish_url);
    }

    if let Some(url) = publish_url {
        return resolve_by_scheme(url);
    }

    if has_git_remote {
        return Ok(Box::new(GitHubReleaseAdapter::default()));
    }

    Err(ReleaseError::NoAdapterResolved(
        "(none configured, and no git remote found)".to_string(),
    ))
}

fn resolve_by_name(name: &str, publish_url: Option<&str>) -> Result<Box<dyn ReleaseAdapter>> {
    match name {
        "github" => Ok(Box::new(GitHubReleaseAdapter::default())),
        "remote-file" | "remotefile" => match publish_url {
            Some(url) => Ok(Box::new(RemoteFileReleaseAdapter::with_target(url))),
            None => Ok(Box::new(RemoteFileReleaseAdapter::default())),
        },
        other => Err(ReleaseError::NoAdapterResolved(format!(
            "--adapter '{other}' is not a known built-in adapter (github, remote-file). \
             Third-party plugin adapters are not yet supported (planned v0.17.4)."
        ))),
    }
}

fn resolve_by_scheme(publish_url: &str) -> Result<Box<dyn ReleaseAdapter>> {
    let scheme = publish_url.split("://").next().unwrap_or("");
    match scheme {
        "s3" | "sftp" | "file" => Ok(Box::new(RemoteFileReleaseAdapter::with_target(publish_url))),
        "" => Err(ReleaseError::NoAdapterResolved(publish_url.to_string())),
        other => Err(ReleaseError::NoAdapterResolved(format!(
            "'{other}://' has no registered built-in adapter. Known schemes: s3, sftp, file \
             (RemoteFileReleaseAdapter). Third-party plugin adapters are planned for v0.17.4."
        ))),
    }
}

/// List all built-in adapters and the schemes/names they claim — backs `ta release adapters`.
pub fn builtin_adapters() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("github", vec!["(default when no publish_url configured)"]),
        ("remote-file", vec!["s3://", "sftp://", "file://"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_override_takes_precedence() {
        let adapter = resolve(Some("github"), Some("s3://bucket/x"), false).unwrap();
        assert_eq!(adapter.name(), "github");
    }

    #[test]
    fn unknown_adapter_override_errors() {
        let err = resolve(Some("nonexistent"), None, false)
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(err, ReleaseError::NoAdapterResolved(_)));
    }

    #[test]
    fn s3_scheme_resolves_to_remote_file() {
        let adapter = resolve(None, Some("s3://my-bucket/releases"), false).unwrap();
        assert_eq!(adapter.name(), "remote-file");
    }

    #[test]
    fn sftp_scheme_resolves_to_remote_file() {
        let adapter = resolve(None, Some("sftp://host/path"), false).unwrap();
        assert_eq!(adapter.name(), "remote-file");
    }

    #[test]
    fn file_scheme_resolves_to_remote_file() {
        let adapter = resolve(None, Some("file:///tmp/releases"), false).unwrap();
        assert_eq!(adapter.name(), "remote-file");
    }

    #[test]
    fn unknown_scheme_errors_with_actionable_message() {
        let err = resolve(None, Some("youtube://channel/UCxxxx"), false)
            .map(|_| ())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("youtube"));
    }

    #[test]
    fn no_publish_url_falls_back_to_github_with_git_remote() {
        let adapter = resolve(None, None, true).unwrap();
        assert_eq!(adapter.name(), "github");
    }

    #[test]
    fn no_publish_url_and_no_git_remote_errors() {
        let err = resolve(None, None, false).map(|_| ()).unwrap_err();
        assert!(matches!(err, ReleaseError::NoAdapterResolved(_)));
    }

    #[test]
    fn builtin_adapters_lists_github_and_remote_file() {
        let adapters = builtin_adapters();
        assert!(adapters.iter().any(|(name, _)| *name == "github"));
        assert!(adapters.iter().any(|(name, _)| *name == "remote-file"));
    }
}
