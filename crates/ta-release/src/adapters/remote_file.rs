//! `RemoteFileReleaseAdapter` — `sftp://`, `s3://`, `file://` publish targets.
//!
//! Copies release assets to `publish_url` and generates a `manifest.json` alongside
//! them (version, checksums, channel, timestamp). PLAN.md v0.17.3 item 6,
//! `docs/release-design.md` §3-4.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adapter::{
    Channel, PreparedRelease, ReleaseAdapter, ReleaseAsset, ReleaseCapabilities, ReleaseContext,
    ReleaseRef, ReleaseStatus,
};
use crate::error::{ReleaseError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: String,
    channel: String,
    timestamp: String,
    checksums: Vec<(String, String)>,
}

/// Abstraction over "put a file/text at a remote path, read text back" so `s3://`/`sftp://`
/// subprocess invocations are mockable in tests, mirroring `GhRunner` in `github.rs`.
pub trait Transport: Send + Sync {
    fn put_file(&self, local: &Path, dest: &str) -> std::result::Result<(), String>;
    fn put_text(&self, text: &str, dest: &str) -> std::result::Result<(), String>;
    fn get_text(&self, dest: &str) -> std::result::Result<Option<String>, String>;
}

/// `file://` — direct filesystem copy. Used as-is (not injected) since it has no
/// subprocess dependency and is fully deterministic; RemoteFileReleaseAdapter routes
/// to it directly when the target scheme is `file`.
struct LocalTransport;

impl Transport for LocalTransport {
    fn put_file(&self, local: &Path, dest: &str) -> std::result::Result<(), String> {
        let dest_path = PathBuf::from(dest);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(local, &dest_path).map_err(|e| e.to_string())?;
        Ok(())
    }
    fn put_text(&self, text: &str, dest: &str) -> std::result::Result<(), String> {
        let dest_path = PathBuf::from(dest);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&dest_path, text).map_err(|e| e.to_string())
    }
    fn get_text(&self, dest: &str) -> std::result::Result<Option<String>, String> {
        match std::fs::read_to_string(dest) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// `s3://` — shells out to `aws s3 cp`.
struct S3Transport;

impl Transport for S3Transport {
    fn put_file(&self, local: &Path, dest: &str) -> std::result::Result<(), String> {
        run_shell("aws", &["s3", "cp", &local.to_string_lossy(), dest])
    }
    fn put_text(&self, text: &str, dest: &str) -> std::result::Result<(), String> {
        let tmp =
            std::env::temp_dir().join(format!("ta-release-manifest-{}.json", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        let result = run_shell("aws", &["s3", "cp", &tmp.to_string_lossy(), dest]);
        let _ = std::fs::remove_file(&tmp);
        result
    }
    fn get_text(&self, dest: &str) -> std::result::Result<Option<String>, String> {
        let tmp =
            std::env::temp_dir().join(format!("ta-release-fetch-{}.json", std::process::id()));
        match run_shell("aws", &["s3", "cp", dest, &tmp.to_string_lossy()]) {
            Ok(()) => {
                let text = std::fs::read_to_string(&tmp).map_err(|e| e.to_string())?;
                let _ = std::fs::remove_file(&tmp);
                Ok(Some(text))
            }
            Err(_) => Ok(None),
        }
    }
}

/// `sftp://` — shells out to `scp`.
struct SftpTransport;

impl Transport for SftpTransport {
    fn put_file(&self, local: &Path, dest: &str) -> std::result::Result<(), String> {
        // dest is "sftp://host/path" — convert to scp's "host:path" form.
        let scp_dest = dest
            .strip_prefix("sftp://")
            .unwrap_or(dest)
            .replacen('/', ":/", 1);
        run_shell("scp", &[&local.to_string_lossy(), &scp_dest])
    }
    fn put_text(&self, text: &str, dest: &str) -> std::result::Result<(), String> {
        let tmp =
            std::env::temp_dir().join(format!("ta-release-manifest-{}.json", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        let result = self.put_file(&tmp, dest);
        let _ = std::fs::remove_file(&tmp);
        result
    }
    fn get_text(&self, dest: &str) -> std::result::Result<Option<String>, String> {
        let scp_src = dest
            .strip_prefix("sftp://")
            .unwrap_or(dest)
            .replacen('/', ":/", 1);
        let tmp =
            std::env::temp_dir().join(format!("ta-release-fetch-{}.json", std::process::id()));
        match run_shell("scp", &[&scp_src, &tmp.to_string_lossy()]) {
            Ok(()) => {
                let text = std::fs::read_to_string(&tmp).map_err(|e| e.to_string())?;
                let _ = std::fs::remove_file(&tmp);
                Ok(Some(text))
            }
            Err(_) => Ok(None),
        }
    }
}

fn run_shell(cmd: &str, args: &[&str]) -> std::result::Result<(), String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn `{cmd} {}`: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "`{cmd} {}` failed (exit {:?}): {}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[derive(Default)]
pub struct RemoteFileReleaseAdapter {
    /// e.g. "s3://my-bucket/releases", "file:///tmp/releases", "sftp://host/path".
    /// `None` when constructed via `Default` (e.g. `ta release adapters` listing) —
    /// `prepare`/`publish` fail with an actionable error in that case.
    target: Option<String>,
}

impl RemoteFileReleaseAdapter {
    pub fn with_target(target: impl Into<String>) -> Self {
        Self {
            target: Some(target.into()),
        }
    }

    fn scheme(&self) -> Option<&str> {
        self.target.as_deref().and_then(|t| t.split("://").next())
    }

    fn transport(&self) -> Result<Box<dyn Transport>> {
        match self.scheme() {
            Some("file") => Ok(Box::new(LocalTransport)),
            Some("s3") => Ok(Box::new(S3Transport)),
            Some("sftp") => Ok(Box::new(SftpTransport)),
            Some(other) => Err(ReleaseError::Config(format!(
                "RemoteFileReleaseAdapter does not support scheme '{other}://'"
            ))),
            None => Err(ReleaseError::PrepareFailed {
                adapter: self.name().to_string(),
                reason: "no publish_url configured — set [release] publish_url in .release.toml \
                         or pass --adapter with a target"
                    .to_string(),
            }),
        }
    }

    /// Resolve the channel-specific directory under the target, e.g.
    /// "file:///tmp/releases" + Stable -> "/tmp/releases/stable" for `file://`, or the
    /// raw string form for `s3://`/`sftp://` (their transports don't need filesystem
    /// path joining semantics).
    fn channel_dir(&self, channel: &Channel) -> Result<String> {
        let target = self
            .target
            .as_deref()
            .ok_or_else(|| ReleaseError::PrepareFailed {
                adapter: self.name().to_string(),
                reason: "no publish_url configured".to_string(),
            })?;
        let trimmed = target.trim_end_matches('/');
        if let Some(rest) = trimmed.strip_prefix("file://") {
            Ok(format!(
                "file://{}/{}",
                rest.trim_end_matches('/'),
                channel.as_str()
            ))
        } else {
            Ok(format!("{trimmed}/{}", channel.as_str()))
        }
    }

    fn dest_path(dir: &str, filename: &str) -> String {
        if let Some(local) = dir.strip_prefix("file://") {
            format!("{local}/{filename}")
        } else {
            format!("{dir}/{filename}")
        }
    }
}

impl ReleaseAdapter for RemoteFileReleaseAdapter {
    fn name(&self) -> &str {
        "remote-file"
    }

    fn capabilities(&self) -> ReleaseCapabilities {
        ReleaseCapabilities {
            requires_semver: false,
            supports_promote: true,
            supports_live_status: true,
            custom_channel_names: vec![],
        }
    }

    fn prepare(&self, ctx: &ReleaseContext) -> Result<PreparedRelease> {
        // Preflight: verify the target is reachable/writable by resolving its transport.
        self.transport()?;
        Ok(PreparedRelease {
            idempotency_key: format!("{}-{}", ctx.version_or_label, ctx.channel),
            resolved_label: ctx.version_or_label.clone(),
        })
    }

    fn publish(&self, prepared: &PreparedRelease, assets: &[ReleaseAsset]) -> Result<ReleaseRef> {
        let transport = self.transport()?;
        // idempotency_key is "<label>-<channel>"; recover the channel for directory routing.
        let channel_str = prepared
            .idempotency_key
            .rsplit('-')
            .next()
            .unwrap_or("stable");
        let dir = self.channel_dir(&Channel::parse(channel_str))?;

        let mut checksums = Vec::new();
        for asset in assets {
            let filename = asset
                .label
                .clone()
                .or_else(|| {
                    asset
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| "asset".to_string());
            let dest = Self::dest_path(&dir, &filename);
            transport.put_file(&asset.path, &dest).map_err(|reason| {
                ReleaseError::PublishFailed {
                    adapter: self.name().to_string(),
                    reason: format!("copying '{}' to '{dest}': {reason}", asset.path.display()),
                }
            })?;
            let checksum = sha256_hex_file(&asset.path).unwrap_or_default();
            checksums.push((filename, checksum));
        }

        let timestamp = unix_timestamp_string();
        let manifest = Manifest {
            version: prepared.resolved_label.clone(),
            channel: channel_str.to_string(),
            timestamp,
            checksums,
        };
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| ReleaseError::Config(format!("manifest serialization failed: {e}")))?;
        let manifest_dest = Self::dest_path(&dir, "manifest.json");
        transport
            .put_text(&manifest_json, &manifest_dest)
            .map_err(|reason| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!("writing manifest.json to '{manifest_dest}': {reason}"),
            })?;

        Ok(ReleaseRef {
            adapter: self.name().to_string(),
            external_id: manifest_dest,
            url: None,
        })
    }

    fn promote(&self, release_ref: &ReleaseRef, channel: &Channel) -> Result<()> {
        let transport = self.transport()?;
        let existing = transport
            .get_text(&release_ref.external_id)
            .map_err(|reason| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!(
                    "reading existing manifest '{}': {reason}",
                    release_ref.external_id
                ),
            })?
            .ok_or_else(|| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!("no manifest found at '{}'", release_ref.external_id),
            })?;
        let mut manifest: Manifest = serde_json::from_str(&existing)
            .map_err(|e| ReleaseError::Config(format!("invalid manifest.json: {e}")))?;
        manifest.channel = channel.as_str().to_string();

        let dir = self.channel_dir(channel)?;
        let manifest_dest = Self::dest_path(&dir, "manifest.json");
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| ReleaseError::Config(format!("manifest serialization failed: {e}")))?;
        transport
            .put_text(&manifest_json, &manifest_dest)
            .map_err(|reason| ReleaseError::PublishFailed {
                adapter: self.name().to_string(),
                reason: format!("writing promoted manifest to '{manifest_dest}': {reason}"),
            })?;
        Ok(())
    }

    fn status(&self, version: &str) -> Result<ReleaseStatus> {
        let transport = self.transport()?;
        for channel in [Channel::Stable, Channel::Rc, Channel::Draft, Channel::Lts] {
            let dir = self.channel_dir(&channel)?;
            let manifest_dest = Self::dest_path(&dir, "manifest.json");
            if let Ok(Some(text)) = transport.get_text(&manifest_dest) {
                if let Ok(manifest) = serde_json::from_str::<Manifest>(&text) {
                    if manifest.version == version {
                        return Ok(ReleaseStatus::Known {
                            channels: vec![channel],
                            published_at: Some(manifest.timestamp),
                            asset_checksums: manifest.checksums,
                        });
                    }
                }
            }
        }
        Ok(ReleaseStatus::Unknown)
    }
}

fn sha256_hex_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(format!("{:x}", hasher.finalize()))
}

fn unix_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_remote_file() {
        assert_eq!(RemoteFileReleaseAdapter::default().name(), "remote-file");
    }

    #[test]
    fn capabilities_do_not_require_semver() {
        let caps = RemoteFileReleaseAdapter::default().capabilities();
        assert!(!caps.requires_semver);
        assert!(caps.supports_promote);
    }

    #[test]
    fn prepare_fails_without_target() {
        let adapter = RemoteFileReleaseAdapter::default();
        let ctx = ReleaseContext {
            version_or_label: "episode-3".to_string(),
            channel: Channel::Stable,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        let err = adapter.prepare(&ctx).unwrap_err();
        assert!(matches!(err, ReleaseError::PrepareFailed { .. }));
    }

    #[test]
    fn prepare_fails_for_unknown_scheme() {
        let adapter = RemoteFileReleaseAdapter::with_target("youtube://channel/UCxxxx");
        let ctx = ReleaseContext {
            version_or_label: "1.0.0".to_string(),
            channel: Channel::Stable,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        assert!(adapter.prepare(&ctx).is_err());
    }

    #[test]
    fn publish_copies_asset_and_writes_manifest_to_local_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = format!("file://{}", dir.path().display());
        let adapter = RemoteFileReleaseAdapter::with_target(target);

        let asset_src = dir.path().join("source-asset.txt");
        std::fs::write(&asset_src, b"hello world").unwrap();

        let ctx = ReleaseContext {
            version_or_label: "episode-3".to_string(),
            channel: Channel::Stable,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        let prepared = adapter.prepare(&ctx).unwrap();
        let asset = ReleaseAsset {
            path: asset_src,
            label: Some("asset.txt".to_string()),
        };
        let release_ref = adapter.publish(&prepared, &[asset]).unwrap();

        let copied = dir.path().join("stable").join("asset.txt");
        assert!(copied.exists());
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "hello world");

        let manifest_path = dir.path().join("stable").join("manifest.json");
        assert!(manifest_path.exists());
        let manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.version, "episode-3");
        assert_eq!(manifest.channel, "stable");
        assert_eq!(manifest.checksums.len(), 1);
        assert_eq!(manifest.checksums[0].0, "asset.txt");
        assert!(!manifest.checksums[0].1.is_empty());

        assert_eq!(
            release_ref.external_id,
            manifest_path.to_string_lossy().to_string()
        );
    }

    #[test]
    fn promote_rewrites_manifest_channel_at_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = format!("file://{}", dir.path().display());
        let adapter = RemoteFileReleaseAdapter::with_target(target);

        let asset_src = dir.path().join("build.bin");
        std::fs::write(&asset_src, b"binary-data").unwrap();
        let ctx = ReleaseContext {
            version_or_label: "1.2.3".to_string(),
            channel: Channel::Rc,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        let prepared = adapter.prepare(&ctx).unwrap();
        let release_ref = adapter
            .publish(
                &prepared,
                &[ReleaseAsset {
                    path: asset_src,
                    label: None,
                }],
            )
            .unwrap();

        adapter.promote(&release_ref, &Channel::Stable).unwrap();

        let promoted_manifest = dir.path().join("stable").join("manifest.json");
        assert!(promoted_manifest.exists());
        let manifest: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&promoted_manifest).unwrap()).unwrap();
        assert_eq!(manifest.channel, "stable");
        assert_eq!(manifest.version, "1.2.3");
    }

    #[test]
    fn status_finds_version_across_channels() {
        let dir = tempfile::tempdir().unwrap();
        let target = format!("file://{}", dir.path().display());
        let adapter = RemoteFileReleaseAdapter::with_target(target);

        let asset_src = dir.path().join("build.bin");
        std::fs::write(&asset_src, b"data").unwrap();
        let ctx = ReleaseContext {
            version_or_label: "9.9.9".to_string(),
            channel: Channel::Draft,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        let prepared = adapter.prepare(&ctx).unwrap();
        adapter
            .publish(
                &prepared,
                &[ReleaseAsset {
                    path: asset_src,
                    label: None,
                }],
            )
            .unwrap();

        let status = adapter.status("9.9.9").unwrap();
        match status {
            ReleaseStatus::Known { channels, .. } => assert!(channels.contains(&Channel::Draft)),
            ReleaseStatus::Unknown => panic!("expected Known status"),
        }
    }

    #[test]
    fn status_unknown_for_unpublished_version() {
        let dir = tempfile::tempdir().unwrap();
        let target = format!("file://{}", dir.path().display());
        let adapter = RemoteFileReleaseAdapter::with_target(target);
        assert_eq!(adapter.status("0.0.1").unwrap(), ReleaseStatus::Unknown);
    }
}
