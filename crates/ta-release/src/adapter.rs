//! Core `ReleaseAdapter` trait and supporting types.
//!
//! Modeled directly on `ta-submit::SourceAdapter` (`crates/ta-submit/src/adapter.rs`):
//! object-safe, `Send + Sync`, default-implemented methods for optional capabilities
//! so a minimal adapter (one method, `publish`) is legal and everything else degrades
//! gracefully rather than panicking. Design reference: `docs/release-design.md` §3-4.

use std::path::PathBuf;

use crate::error::{ReleaseError, Result};

/// Channel model and lifecycle: `Draft -> Rc -> Stable -> Lts`, monotonic by default.
/// See `docs/release-design.md` §4.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Not externally visible / private.
    Draft,
    /// Pre-release, externally visible, not "latest".
    Rc,
    /// Externally visible, "latest".
    Stable,
    /// Stable + a long-term-support marker (adapter-specific meaning).
    Lts,
    /// Adapter-native channel name (e.g. "beta" for Steam, "nightly" for GitHub) —
    /// validated against that adapter's `ReleaseCapabilities::custom_channel_names`.
    Custom(String),
}

impl Channel {
    /// Parse a channel name from CLI/config input. Standard names map to the
    /// fixed variants (case-insensitive); anything else becomes `Custom`.
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "draft" => Channel::Draft,
            "rc" => Channel::Rc,
            "stable" => Channel::Stable,
            "lts" => Channel::Lts,
            other => Channel::Custom(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Channel::Draft => "draft",
            Channel::Rc => "rc",
            Channel::Stable => "stable",
            Channel::Lts => "lts",
            Channel::Custom(name) => name,
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Static, adapter-declared capability flags — checked by the CLI *before* invoking
/// any adapter method, so an unsupported operation fails fast with a clear message
/// instead of a runtime `Unsupported` error surfacing mid-pipeline.
#[derive(Debug, Clone, Default)]
pub struct ReleaseCapabilities {
    /// If true, `ta release run` rejects non-semver labels for this adapter.
    pub requires_semver: bool,
    /// If true, `promote` has a real implementation (not the default `Unsupported`).
    pub supports_promote: bool,
    /// If true, `status`/`list` do a live remote query rather than returning `Unknown`/empty.
    pub supports_live_status: bool,
    /// Channel names this adapter recognizes beyond the four standard ones (§4).
    pub custom_channel_names: Vec<String>,
}

/// Input to `ReleaseAdapter::prepare`/`publish`.
#[derive(Debug, Clone)]
pub struct ReleaseContext {
    /// Semver OR arbitrary label — see `docs/release-design.md` §5.
    pub version_or_label: String,
    pub channel: Channel,
    /// Commit log / notes context, for release-notes generation.
    pub commits: String,
    pub workspace_root: PathBuf,
}

/// Result of `ReleaseAdapter::prepare` — staged, not yet published.
#[derive(Debug, Clone)]
pub struct PreparedRelease {
    /// Adapter-chosen idempotency key (e.g. GitHub tag, S3 manifest checksum).
    pub idempotency_key: String,
    pub resolved_label: String,
}

/// One release asset (binary, archive, video, build) to publish.
#[derive(Debug, Clone)]
pub struct ReleaseAsset {
    pub path: PathBuf,
    /// Display name, e.g. "ta-linux-x86_64.tar.gz". Defaults to the file name.
    pub label: Option<String>,
}

/// Reference to a published release, returned by `publish`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRef {
    pub adapter: String,
    /// e.g. GitHub release ID, S3 manifest URL.
    pub external_id: String,
    /// Human-followable link, when the platform has one.
    pub url: Option<String>,
}

/// Live publish state for a version, returned by `status`/`list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseStatus {
    /// Adapter has no live query path — caller falls back to local history.
    Unknown,
    Known {
        channels: Vec<Channel>,
        published_at: Option<String>,
        asset_checksums: Vec<(String, String)>,
    },
}

/// Pluggable adapter for release-publish operations (GitHub, S3/SFTP, YouTube, Steam, ...).
///
/// The pre-publish pipeline (version bump, changelog, constitution check, build) stays
/// VCS/adapter-agnostic — this trait only replaces the *publish* half: staging, publishing,
/// promoting between channels, and querying live status. See `docs/release-design.md` §3.
pub trait ReleaseAdapter: Send + Sync {
    /// Adapter display name (for CLI output, `ta release adapters`, error messages).
    fn name(&self) -> &str;

    /// Static capability flags — drives CLI validation before any adapter method runs.
    fn capabilities(&self) -> ReleaseCapabilities;

    /// Stage a release: resolve the final version/label, run adapter-specific preflight
    /// (e.g. GitHub: verify `gh` auth; S3: verify bucket write access). Does not publish
    /// anything externally yet — a failed `prepare` leaves no visible trace on the target
    /// platform.
    fn prepare(&self, ctx: &ReleaseContext) -> Result<PreparedRelease>;

    /// Publish a prepared release with its assets. Idempotent where the underlying
    /// platform allows it (calling twice with the same `PreparedRelease.idempotency_key`
    /// should not create a duplicate release).
    fn publish(&self, prepared: &PreparedRelease, assets: &[ReleaseAsset]) -> Result<ReleaseRef>;

    /// Move an already-published release to a different channel without rebuilding
    /// or re-uploading.
    ///
    /// Default: not every adapter supports post-hoc promotion (e.g. a one-shot
    /// webhook-based `ServiceReleaseAdapter` might not).
    fn promote(&self, _release_ref: &ReleaseRef, _channel: &Channel) -> Result<()> {
        Err(ReleaseError::Unsupported {
            adapter: self.name().to_string(),
            operation: "promote".to_string(),
        })
    }

    /// Query current publish state for a version. Powers `ta release status`/`list`.
    ///
    /// Default: adapter has no live query path; caller falls back to local
    /// `.ta/release-history.json`.
    fn status(&self, _version: &str) -> Result<ReleaseStatus> {
        Ok(ReleaseStatus::Unknown)
    }

    /// List recent releases this adapter knows about, most recent first.
    /// Default: empty — caller falls back to local history file only.
    fn list(&self, _limit: usize) -> Result<Vec<ReleaseStatus>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockAdapter;
    impl ReleaseAdapter for MockAdapter {
        fn name(&self) -> &str {
            "mock"
        }
        fn capabilities(&self) -> ReleaseCapabilities {
            ReleaseCapabilities::default()
        }
        fn prepare(&self, ctx: &ReleaseContext) -> Result<PreparedRelease> {
            Ok(PreparedRelease {
                idempotency_key: ctx.version_or_label.clone(),
                resolved_label: ctx.version_or_label.clone(),
            })
        }
        fn publish(
            &self,
            prepared: &PreparedRelease,
            _assets: &[ReleaseAsset],
        ) -> Result<ReleaseRef> {
            Ok(ReleaseRef {
                adapter: self.name().to_string(),
                external_id: prepared.idempotency_key.clone(),
                url: None,
            })
        }
    }

    #[test]
    fn default_promote_is_unsupported() {
        let adapter = MockAdapter;
        let release_ref = ReleaseRef {
            adapter: "mock".to_string(),
            external_id: "x".to_string(),
            url: None,
        };
        let err = adapter.promote(&release_ref, &Channel::Stable).unwrap_err();
        assert!(matches!(err, ReleaseError::Unsupported { .. }));
    }

    #[test]
    fn default_status_is_unknown() {
        let adapter = MockAdapter;
        assert_eq!(adapter.status("1.0.0").unwrap(), ReleaseStatus::Unknown);
    }

    #[test]
    fn default_list_is_empty() {
        let adapter = MockAdapter;
        assert!(adapter.list(10).unwrap().is_empty());
    }

    #[test]
    fn channel_parse_standard_names() {
        assert_eq!(Channel::parse("stable"), Channel::Stable);
        assert_eq!(Channel::parse("RC"), Channel::Rc);
        assert_eq!(Channel::parse("Draft"), Channel::Draft);
        assert_eq!(Channel::parse("lts"), Channel::Lts);
    }

    #[test]
    fn channel_parse_custom_name() {
        assert_eq!(Channel::parse("beta"), Channel::Custom("beta".to_string()));
        assert_eq!(Channel::parse("beta").as_str(), "beta");
    }

    #[test]
    fn channel_display_matches_as_str() {
        assert_eq!(Channel::Stable.to_string(), "stable");
        assert_eq!(
            Channel::Custom("nightly".to_string()).to_string(),
            "nightly"
        );
    }

    #[test]
    fn prepare_then_publish_roundtrip() {
        let adapter = MockAdapter;
        let ctx = ReleaseContext {
            version_or_label: "1.2.3".to_string(),
            channel: Channel::Stable,
            commits: String::new(),
            workspace_root: PathBuf::from("."),
        };
        let prepared = adapter.prepare(&ctx).unwrap();
        let release_ref = adapter.publish(&prepared, &[]).unwrap();
        assert_eq!(release_ref.external_id, "1.2.3");
        assert_eq!(release_ref.adapter, "mock");
    }
}
