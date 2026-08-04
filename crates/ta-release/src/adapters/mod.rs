//! Built-in `ReleaseAdapter` implementations.

pub mod github;
pub mod remote_file;

pub use github::GitHubReleaseAdapter;
pub use remote_file::RemoteFileReleaseAdapter;
