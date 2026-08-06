//! Built-in `ReleaseAdapter` implementations.

pub mod github;
pub mod homebrew;
pub mod plugin;
pub mod remote_file;
pub mod youtube;

pub use github::GitHubReleaseAdapter;
pub use homebrew::{HomebrewTapPr, HomebrewTapUpdater};
pub use plugin::PluginReleaseAdapter;
pub use remote_file::RemoteFileReleaseAdapter;
pub use youtube::YouTubeReleaseAdapter;
