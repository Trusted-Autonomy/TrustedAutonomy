//! `ReleaseAdapterRegistry` — URL-scheme adapter discovery.
//!
//! Modeled on the `ta-db-proxy` registry (`crates/ta-db-proxy/src/registry.rs`) — the
//! same "core has zero awareness of which specific backends exist" pattern, applied to
//! release publish targets. See `docs/release-design.md` §6.
//!
//! Resolution order:
//!   1. `--adapter <name>` CLI override
//!   2. `publish_url` scheme against built-in adapters (`s3://`, `sftp://`, `file://` →
//!      `RemoteFileReleaseAdapter`, `youtube://` → `YouTubeReleaseAdapter`)
//!   3. `publish_url` scheme (or `--adapter <name>`) against a discovered plugin adapter —
//!      `.ta/plugins/release/<scheme-or-name>/plugin.toml` (project-local) or
//!      `~/.config/ta/plugins/release/<scheme-or-name>/plugin.toml` (user-global), by the
//!      same naming convention `ta-db-proxy` uses (v0.17.4 item 3)
//!   4. no `publish_url` at all → `GitHubReleaseAdapter` if a git remote is configured,
//!      else an actionable error

use std::path::Path;

use ta_plugin::discovery::find_plugin;

use crate::adapter::ReleaseAdapter;
use crate::adapters::{
    GitHubReleaseAdapter, PluginReleaseAdapter, RemoteFileReleaseAdapter, YouTubeReleaseAdapter,
};
use crate::error::{ReleaseError, Result};

const RELEASE_PLUGIN_KIND: &str = "release";

/// Resolve a `Box<dyn ReleaseAdapter>` from an explicit override name, a `publish_url`,
/// or (if neither is given) a git-remote fallback to `GitHubReleaseAdapter`.
///
/// `has_git_remote` is passed in by the caller (e.g. `apps/ta-cli` already knows how to
/// check for a git remote via `ta-submit`/`release_git`) rather than this crate shelling
/// out to git itself — keeps `ta-release` free of VCS-specific process calls.
///
/// `project_root` is where `.ta/plugins/release/` is searched for third-party plugin
/// adapters (step 3 above); built-in adapters ignore it.
pub fn resolve(
    adapter_override: Option<&str>,
    publish_url: Option<&str>,
    has_git_remote: bool,
    project_root: &Path,
) -> Result<Box<dyn ReleaseAdapter>> {
    if let Some(name) = adapter_override {
        return resolve_by_name(name, publish_url, project_root);
    }

    if let Some(url) = publish_url {
        return resolve_by_scheme(url, project_root);
    }

    if has_git_remote {
        return Ok(Box::new(GitHubReleaseAdapter::default()));
    }

    Err(ReleaseError::NoAdapterResolved(
        "(none configured, and no git remote found)".to_string(),
    ))
}

fn resolve_by_name(
    name: &str,
    publish_url: Option<&str>,
    project_root: &Path,
) -> Result<Box<dyn ReleaseAdapter>> {
    match name {
        "github" => Ok(Box::new(GitHubReleaseAdapter::default())),
        "remote-file" | "remotefile" => match publish_url {
            Some(url) => Ok(Box::new(RemoteFileReleaseAdapter::with_target(url))),
            None => Ok(Box::new(RemoteFileReleaseAdapter::default())),
        },
        "youtube" => match publish_url {
            Some(url) => Ok(Box::new(YouTubeReleaseAdapter::from_publish_url(url)?)),
            None => Err(ReleaseError::NoAdapterResolved(
                "adapter 'youtube' requires publish_url = \"youtube://channel/<channel-id>\""
                    .to_string(),
            )),
        },
        other => {
            if let Some(plugin) = find_plugin(RELEASE_PLUGIN_KIND, other, project_root) {
                return Ok(Box::new(PluginReleaseAdapter::new(
                    &plugin.manifest,
                    project_root,
                )?));
            }
            Err(ReleaseError::NoAdapterResolved(format!(
                "--adapter '{other}' is not a known built-in adapter (github, remote-file, youtube) \
                 and no plugin named '{other}' was found in .ta/plugins/release/ or \
                 ~/.config/ta/plugins/release/. See docs/community-release-plugin.md to author one."
            )))
        }
    }
}

fn resolve_by_scheme(publish_url: &str, project_root: &Path) -> Result<Box<dyn ReleaseAdapter>> {
    let scheme = publish_url.split("://").next().unwrap_or("");
    match scheme {
        "s3" | "sftp" | "file" => Ok(Box::new(RemoteFileReleaseAdapter::with_target(publish_url))),
        "youtube" => Ok(Box::new(YouTubeReleaseAdapter::from_publish_url(
            publish_url,
        )?)),
        "" => Err(ReleaseError::NoAdapterResolved(publish_url.to_string())),
        other => {
            if let Some(plugin) = find_plugin(RELEASE_PLUGIN_KIND, other, project_root) {
                return Ok(Box::new(PluginReleaseAdapter::new(
                    &plugin.manifest,
                    project_root,
                )?));
            }
            Err(ReleaseError::NoAdapterResolved(format!(
                "'{other}://' has no registered built-in adapter and no plugin named '{other}' was \
                 found in .ta/plugins/release/ or ~/.config/ta/plugins/release/. Known built-in \
                 schemes: s3, sftp, file, youtube. See docs/community-release-plugin.md to author \
                 a plugin (e.g. for steam:// or an App Store target)."
            )))
        }
    }
}

/// List all built-in adapters and the schemes/names they claim — backs `ta release adapters`.
pub fn builtin_adapters() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("github", vec!["(default when no publish_url configured)"]),
        ("remote-file", vec!["s3://", "sftp://", "file://"]),
        ("youtube", vec!["youtube://channel/<channel-id>"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_plugins_root() -> std::path::PathBuf {
        // Leak the TempDir so the directory outlives the test function — these tests
        // only read from it, and cleanup isn't worth an extra guard variable per test.
        tempfile::tempdir().unwrap().keep()
    }

    #[test]
    fn adapter_override_takes_precedence() {
        let root = no_plugins_root();
        let adapter = resolve(Some("github"), Some("s3://bucket/x"), false, &root).unwrap();
        assert_eq!(adapter.name(), "github");
    }

    #[test]
    fn unknown_adapter_override_errors() {
        let root = no_plugins_root();
        let err = resolve(Some("nonexistent"), None, false, &root)
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(err, ReleaseError::NoAdapterResolved(_)));
    }

    #[test]
    fn s3_scheme_resolves_to_remote_file() {
        let root = no_plugins_root();
        let adapter = resolve(None, Some("s3://my-bucket/releases"), false, &root).unwrap();
        assert_eq!(adapter.name(), "remote-file");
    }

    #[test]
    fn sftp_scheme_resolves_to_remote_file() {
        let root = no_plugins_root();
        let adapter = resolve(None, Some("sftp://host/path"), false, &root).unwrap();
        assert_eq!(adapter.name(), "remote-file");
    }

    #[test]
    fn file_scheme_resolves_to_remote_file() {
        let root = no_plugins_root();
        let adapter = resolve(None, Some("file:///tmp/releases"), false, &root).unwrap();
        assert_eq!(adapter.name(), "remote-file");
    }

    #[test]
    fn youtube_scheme_resolves_to_youtube_adapter() {
        let root = no_plugins_root();
        let adapter = resolve(None, Some("youtube://channel/UCxxxx"), false, &root).unwrap();
        assert_eq!(adapter.name(), "youtube");
    }

    #[test]
    fn youtube_adapter_override_requires_publish_url() {
        let root = no_plugins_root();
        let err = resolve(Some("youtube"), None, false, &root)
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(err, ReleaseError::NoAdapterResolved(_)));
    }

    #[test]
    fn unknown_scheme_errors_with_actionable_message() {
        let root = no_plugins_root();
        let err = resolve(None, Some("ftp://host/path"), false, &root)
            .map(|_| ())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ftp"));
    }

    #[test]
    fn no_publish_url_falls_back_to_github_with_git_remote() {
        let root = no_plugins_root();
        let adapter = resolve(None, None, true, &root).unwrap();
        assert_eq!(adapter.name(), "github");
    }

    #[test]
    fn no_publish_url_and_no_git_remote_errors() {
        let root = no_plugins_root();
        let err = resolve(None, None, false, &root).map(|_| ()).unwrap_err();
        assert!(matches!(err, ReleaseError::NoAdapterResolved(_)));
    }

    #[test]
    fn builtin_adapters_lists_github_remote_file_and_youtube() {
        let adapters = builtin_adapters();
        assert!(adapters.iter().any(|(name, _)| *name == "github"));
        assert!(adapters.iter().any(|(name, _)| *name == "remote-file"));
        assert!(adapters.iter().any(|(name, _)| *name == "youtube"));
    }

    #[cfg(unix)]
    #[test]
    fn unknown_scheme_resolves_to_project_local_plugin() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let root = no_plugins_root();
        let plugin_dir = root
            .join(".ta")
            .join("plugins")
            .join("release")
            .join("steam");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let script_path = plugin_dir.join("mock-steam-plugin.sh");
        let mut f = std::fs::File::create(&script_path).unwrap();
        f.write_all(
            b"#!/bin/sh\nread -r line\necho '{\"ok\":true,\"result\":{\"plugin_version\":\"1.0.0\",\"protocol_version\":1,\"adapter_name\":\"steam\",\"capabilities\":[]}}'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!(
                "name = \"steam\"\ntype = \"release\"\ncommand = \"{}\"\n",
                script_path.display()
            ),
        )
        .unwrap();

        let adapter = resolve(None, Some("steam://app/12345"), false, &root).unwrap();
        assert_eq!(adapter.name(), "steam");
    }

    #[test]
    fn unknown_scheme_with_no_matching_plugin_errors_with_actionable_message() {
        let root = no_plugins_root();
        let err = resolve(None, Some("appstore://app/12345"), false, &root)
            .map(|_| ())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("appstore"));
        assert!(msg.contains(".ta/plugins/release/"));
    }
}
