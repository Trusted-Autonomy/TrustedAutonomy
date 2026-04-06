//! # ta-workspace
//!
//! Staging workspace manager for Trusted Autonomy.
//!
//! Manages ephemeral temp directories where agents stage filesystem changes.
//! Changes are tracked via a [`ChangeStore`] trait — the MVP implementation
//! ([`JsonFileStore`]) persists to JSONL on disk so work is never lost.
//! The trait can be swapped for SQLite or other backends later.
//!
//! ## Key components
//!
//! - [`StagingWorkspace`] — ephemeral temp directory where files are staged
//!   before review. Tracks original snapshots for diff generation.
//! - [`ChangeStore`] — trait abstracting changeset persistence. Lets us swap
//!   backends (JSONL → SQLite → S3) without changing callers.
//! - [`JsonFileStore`] — MVP implementation: one JSONL file per goal,
//!   append-optimized, survives process restarts.

pub mod conflict;
pub mod copy_strategy;
pub mod error;
pub mod overlay;
pub mod partitioning;
pub mod projfs_strategy;
pub mod staging;
pub mod store;
pub mod windows_features;

pub use conflict::{Conflict, ConflictResolution, FileSnapshot, SourceSnapshot};
pub use copy_strategy::{CopyStat, CopyStrategy};
pub use error::WorkspaceError;
pub use overlay::{ExcludePatterns, OverlayStagingMode, OverlayWorkspace};
pub use partitioning::{
    gitignore_block, p4ignore_block, update_gitignore, update_p4ignore, VcsBackend,
    GITIGNORE_MARKER, LOCAL_TA_PATHS, P4IGNORE_MARKER, SHARED_TA_PATHS,
};
pub use staging::StagingWorkspace;
pub use store::{ChangeStore, JsonFileStore};
