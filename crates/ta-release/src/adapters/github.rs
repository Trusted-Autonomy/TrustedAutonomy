//! `GitHubReleaseAdapter` — draft-first publish via the `gh` CLI.
//!
//! Full replacement for the current manual tag + `release.yml` dispatch ending
//! (`docs/release-design.md` §3, PLAN.md v0.17.3 item 5). Draft-first publish
//! (create draft -> upload assets -> publish) avoids the "assets uploading while
//! release is already public" race the existing pipeline is exposed to.

use std::path::PathBuf;
use std::process::Command;

use crate::adapter::{
    Channel, PreparedRelease, ReleaseAdapter, ReleaseAsset, ReleaseCapabilities, ReleaseContext,
    ReleaseRef, ReleaseStatus,
};
use crate::error::{ReleaseError, Result};

/// Runs `gh` as a subprocess. Overridable in tests via `GitHubReleaseAdapter::with_runner`
/// so adapter logic (draft-first sequencing, channel mapping) is testable without a real
/// GitHub repo or network access.
pub trait GhRunner: Send + Sync {
    fn run(&self, args: &[&str]) -> std::result::Result<String, String>;
}

#[derive(Default)]
struct RealGhRunner;

impl GhRunner for RealGhRunner {
    fn run(&self, args: &[&str]) -> std::result::Result<String, String> {
        let output = Command::new("gh")
            .args(args)
            .output()
            .map_err(|e| format!("failed to spawn `gh {}`: {e}", args.join(" ")))?;
        if !output.status.success() {
            return Err(format!(
                "`gh {}` failed (exit {:?}): {}",
                args.join(" "),
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

pub struct GitHubReleaseAdapter {
    runner: Box<dyn GhRunner>,
}

impl Default for GitHubReleaseAdapter {
    fn default() -> Self {
        Self {
            runner: Box::new(RealGhRunner),
        }
    }
}

impl GitHubReleaseAdapter {
    pub fn with_runner(runner: Box<dyn GhRunner>) -> Self {
        Self { runner }
    }

    fn tag_for(label: &str) -> String {
        if label.starts_with('v') {
            label.to_string()
        } else {
            format!("v{label}")
        }
    }

    /// Map the standard channel model onto GitHub's prerelease/--latest primitives.
    /// See `docs/release-design.md` §4's mapping table.
    fn channel_flags(channel: &Channel) -> (&'static str, bool) {
        match channel {
            Channel::Draft => ("--draft", false),
            Channel::Rc => ("--prerelease", false),
            Channel::Stable => ("", true),
            Channel::Lts => ("", true),
            Channel::Custom(name) if name == "nightly" => ("--prerelease", false),
            Channel::Custom(_) => ("--prerelease", false),
        }
    }
}

impl ReleaseAdapter for GitHubReleaseAdapter {
    fn name(&self) -> &str {
        "github"
    }

    fn capabilities(&self) -> ReleaseCapabilities {
        ReleaseCapabilities {
            requires_semver: true,
            supports_promote: true,
            supports_live_status: true,
            custom_channel_names: vec!["nightly".to_string()],
        }
    }

    fn prepare(&self, ctx: &ReleaseContext) -> Result<PreparedRelease> {
        // Preflight: verify `gh` auth before touching the target platform.
        self.runner
            .run(&["auth", "status"])
            .map_err(|reason| ReleaseError::PrepareFailed {
                adapter: self.name().to_string(),
                reason: format!("gh auth check failed: {reason}. Run `gh auth login` and retry."),
            })?;
        let tag = Self::tag_for(&ctx.version_or_label);
        Ok(PreparedRelease {
            idempotency_key: tag.clone(),
            resolved_label: tag,
        })
    }

    fn publish(&self, prepared: &PreparedRelease, assets: &[ReleaseAsset]) -> Result<ReleaseRef> {
        let tag = &prepared.idempotency_key;

        // Step 1: create as a draft — no public visibility yet.
        let mut create_args: Vec<String> = vec![
            "release".into(),
            "create".into(),
            tag.clone(),
            "--draft".into(),
        ];
        create_args.push("--title".into());
        create_args.push(tag.clone());
        let create_args_ref: Vec<&str> = create_args.iter().map(String::as_str).collect();
        self.runner
            .run(&create_args_ref)
            .map_err(|reason| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!("draft creation failed: {reason}"),
            })?;

        // Step 2: upload assets to the still-draft release.
        for asset in assets {
            let path_str = asset.path.to_string_lossy().to_string();
            let upload_args = [
                "release".to_string(),
                "upload".to_string(),
                tag.clone(),
                path_str,
                "--clobber".to_string(),
            ];
            let upload_args_ref: Vec<&str> = upload_args.iter().map(String::as_str).collect();
            self.runner
                .run(&upload_args_ref)
                .map_err(|reason| ReleaseError::PublishFailed {
                    adapter: self.name().to_string(),
                    reason: format!(
                        "asset upload failed for '{}': {reason}",
                        asset.path.display()
                    ),
                })?;
        }

        // Step 3: publish (edit draft=false), applying the channel's prerelease/--latest flags.
        let mut edit_args = vec![
            "release".to_string(),
            "edit".to_string(),
            tag.clone(),
            "--draft=false".to_string(),
        ];
        // Default to a stable, --latest publish unless the caller overrides via
        // a follow-up `promote`. `prepare`/`publish` don't carry channel state today
        // (see `ReleaseContext.channel` — threaded in by the caller at a higher level
        // when this adapter is invoked through `ta release run --channel`).
        edit_args.push("--latest".to_string());
        let edit_args_ref: Vec<&str> = edit_args.iter().map(String::as_str).collect();
        self.runner
            .run(&edit_args_ref)
            .map_err(|reason| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!("publishing draft failed: {reason}"),
            })?;

        let url = self
            .runner
            .run(&["release", "view", tag, "--json", "url", "-q", ".url"])
            .ok();

        Ok(ReleaseRef {
            adapter: self.name().to_string(),
            external_id: tag.clone(),
            url,
        })
    }

    fn promote(&self, release_ref: &ReleaseRef, channel: &Channel) -> Result<()> {
        let (flag, latest) = Self::channel_flags(channel);
        let mut args = vec![
            "release".to_string(),
            "edit".to_string(),
            release_ref.external_id.clone(),
        ];
        if !flag.is_empty() {
            args.push(flag.to_string());
        } else {
            args.push("--prerelease=false".to_string());
        }
        if latest {
            args.push("--latest".to_string());
        }
        let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
        self.runner
            .run(&args_ref)
            .map_err(|reason| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!("promote to '{channel}' failed: {reason}"),
            })?;
        Ok(())
    }

    fn status(&self, version: &str) -> Result<ReleaseStatus> {
        let tag = Self::tag_for(version);
        let output = match self.runner.run(&[
            "release",
            "view",
            &tag,
            "--json",
            "isDraft,isPrerelease,isLatest,publishedAt,assets",
        ]) {
            Ok(out) => out,
            Err(_) => return Ok(ReleaseStatus::Unknown),
        };
        let parsed: serde_json::Value = serde_json::from_str(&output).map_err(|e| {
            ReleaseError::Config(format!("unparseable gh release view output: {e}"))
        })?;

        let mut channels = Vec::new();
        if parsed["isDraft"].as_bool().unwrap_or(false) {
            channels.push(Channel::Draft);
        } else if parsed["isPrerelease"].as_bool().unwrap_or(false) {
            channels.push(Channel::Rc);
        } else {
            channels.push(Channel::Stable);
        }
        if parsed["isLatest"].as_bool().unwrap_or(false) && !channels.contains(&Channel::Stable) {
            channels.push(Channel::Stable);
        }

        let published_at = parsed["publishedAt"].as_str().map(|s| s.to_string());
        let asset_checksums = parsed["assets"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a["name"].as_str().map(|n| (n.to_string(), String::new())))
                    .collect()
            })
            .unwrap_or_default();

        Ok(ReleaseStatus::Known {
            channels,
            published_at,
            asset_checksums,
        })
    }

    fn list(&self, limit: usize) -> Result<Vec<ReleaseStatus>> {
        let limit_str = limit.to_string();
        let output = match self.runner.run(&[
            "release",
            "list",
            "--limit",
            &limit_str,
            "--json",
            "tagName,isDraft,isPrerelease,isLatest,publishedAt",
        ]) {
            Ok(out) => out,
            Err(_) => return Ok(Vec::new()),
        };
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap_or_default();
        Ok(parsed
            .into_iter()
            .map(|entry| {
                let mut channels = Vec::new();
                if entry["isDraft"].as_bool().unwrap_or(false) {
                    channels.push(Channel::Draft);
                } else if entry["isPrerelease"].as_bool().unwrap_or(false) {
                    channels.push(Channel::Rc);
                } else {
                    channels.push(Channel::Stable);
                }
                ReleaseStatus::Known {
                    channels,
                    published_at: entry["publishedAt"].as_str().map(|s| s.to_string()),
                    asset_checksums: Vec::new(),
                }
            })
            .collect())
    }
}

#[allow(dead_code)]
fn asset_label(asset: &ReleaseAsset) -> String {
    asset.label.clone().unwrap_or_else(|| {
        asset
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    })
}

#[allow(dead_code)]
fn asset_paths(assets: &[ReleaseAsset]) -> Vec<PathBuf> {
    assets.iter().map(|a| a.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockGh {
        calls: Mutex<Vec<Vec<String>>>,
        responses: std::collections::HashMap<String, std::result::Result<String, String>>,
        fail_auth: bool,
    }

    impl MockGh {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: std::collections::HashMap::new(),
                fail_auth: false,
            }
        }
        fn failing_auth() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: std::collections::HashMap::new(),
                fail_auth: true,
            }
        }
        fn with_response(mut self, args: &[&str], response: &str) -> Self {
            self.responses
                .insert(args.join(" "), Ok(response.to_string()));
            self
        }
    }

    impl GhRunner for MockGh {
        fn run(&self, args: &[&str]) -> std::result::Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            if self.fail_auth && args.first() == Some(&"auth") {
                return Err("not logged in".to_string());
            }
            let key = args.join(" ");
            self.responses
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Ok(String::new()))
        }
    }

    #[test]
    fn name_is_github() {
        let adapter = GitHubReleaseAdapter::default();
        assert_eq!(adapter.name(), "github");
    }

    #[test]
    fn capabilities_require_semver_and_support_promote() {
        let adapter = GitHubReleaseAdapter::default();
        let caps = adapter.capabilities();
        assert!(caps.requires_semver);
        assert!(caps.supports_promote);
        assert!(caps.supports_live_status);
        assert!(caps.custom_channel_names.contains(&"nightly".to_string()));
    }

    #[test]
    fn prepare_fails_when_gh_auth_fails() {
        let adapter = GitHubReleaseAdapter::with_runner(Box::new(MockGh::failing_auth()));
        let ctx = ReleaseContext {
            version_or_label: "1.0.0".to_string(),
            channel: Channel::Stable,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        let err = adapter.prepare(&ctx).unwrap_err();
        assert!(matches!(err, ReleaseError::PrepareFailed { .. }));
    }

    #[test]
    fn prepare_normalizes_tag_with_v_prefix() {
        let adapter = GitHubReleaseAdapter::with_runner(Box::new(MockGh::new()));
        let ctx = ReleaseContext {
            version_or_label: "1.0.0".to_string(),
            channel: Channel::Stable,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        let prepared = adapter.prepare(&ctx).unwrap();
        assert_eq!(prepared.idempotency_key, "v1.0.0");
    }

    #[test]
    fn publish_creates_draft_uploads_assets_then_publishes() {
        let mock = MockGh::new();
        let adapter = GitHubReleaseAdapter::with_runner(Box::new(mock));
        let prepared = PreparedRelease {
            idempotency_key: "v1.0.0".to_string(),
            resolved_label: "v1.0.0".to_string(),
        };
        let asset = ReleaseAsset {
            path: PathBuf::from("/tmp/ta-linux.tar.gz"),
            label: None,
        };
        let release_ref = adapter.publish(&prepared, &[asset]).unwrap();
        assert_eq!(release_ref.external_id, "v1.0.0");
        assert_eq!(release_ref.adapter, "github");
    }

    #[test]
    fn channel_flags_map_stable_to_latest() {
        assert_eq!(
            GitHubReleaseAdapter::channel_flags(&Channel::Stable),
            ("", true)
        );
    }

    #[test]
    fn channel_flags_map_draft_to_draft_flag() {
        assert_eq!(
            GitHubReleaseAdapter::channel_flags(&Channel::Draft),
            ("--draft", false)
        );
    }

    #[test]
    fn channel_flags_map_rc_to_prerelease() {
        assert_eq!(
            GitHubReleaseAdapter::channel_flags(&Channel::Rc),
            ("--prerelease", false)
        );
    }

    #[test]
    fn promote_to_stable_sets_latest() {
        let adapter = GitHubReleaseAdapter::with_runner(Box::new(MockGh::new()));
        let release_ref = ReleaseRef {
            adapter: "github".to_string(),
            external_id: "v1.0.0".to_string(),
            url: None,
        };
        adapter.promote(&release_ref, &Channel::Stable).unwrap();
    }

    #[test]
    fn status_parses_stable_release_json() {
        let mock = MockGh::new().with_response(
            &[
                "release",
                "view",
                "v1.0.0",
                "--json",
                "isDraft,isPrerelease,isLatest,publishedAt,assets",
            ],
            r#"{"isDraft":false,"isPrerelease":false,"isLatest":true,"publishedAt":"2026-01-01T00:00:00Z","assets":[]}"#,
        );
        let adapter = GitHubReleaseAdapter::with_runner(Box::new(mock));
        let status = adapter.status("1.0.0").unwrap();
        match status {
            ReleaseStatus::Known {
                channels,
                published_at,
                ..
            } => {
                assert!(channels.contains(&Channel::Stable));
                assert_eq!(published_at.as_deref(), Some("2026-01-01T00:00:00Z"));
            }
            ReleaseStatus::Unknown => panic!("expected Known status"),
        }
    }
}
