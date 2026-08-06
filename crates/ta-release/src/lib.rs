//! ta-release — `ReleaseAdapter` trait and built-in adapters for `ta release`.
//!
//! Replaces the implicit "tag + push + GitHub Actions dispatch" ending of the release
//! pipeline with a pluggable, URL-scheme-discovered adapter abstraction covering
//! GitHub, S3/SFTP/local-file targets, and (v0.17.4+) content/game platforms.
//! Design reference: `docs/release-design.md`.

pub mod adapter;
pub mod adapters;
pub mod config;
pub mod error;
pub mod registry;

pub use adapter::{
    Channel, PreparedRelease, ReleaseAdapter, ReleaseAsset, ReleaseCapabilities, ReleaseContext,
    ReleaseRef, ReleaseStatus,
};
pub use adapters::{
    GitHubReleaseAdapter, HomebrewTapPr, HomebrewTapUpdater, PluginReleaseAdapter,
    RemoteFileReleaseAdapter, YouTubeReleaseAdapter,
};
pub use config::{HomebrewTapConfig, ReleaseAdapterConfig};
pub use error::{ReleaseError, Result};
pub use registry::resolve as resolve_adapter;
