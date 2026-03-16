//! Source adapters for VCS integration
//!
//! This crate provides pluggable adapters for source control operations through
//! different version control systems and workflows. The core abstraction is the
//! `SourceAdapter` trait (unified from the former `SubmitAdapter` in v0.11.1),
//! with built-in implementations for Git, SVN (stub), Perforce (stub),
//! and a "none" fallback.

pub mod adapter;
pub mod config;
pub mod git;
pub mod none;
pub mod perforce;
pub mod registry;
pub mod svn;

// Primary exports (v0.11.1+)
pub use adapter::{
    CommitResult, PushResult, ReviewResult, ReviewStatus, SavedVcsState, SourceAdapter, SyncResult,
};

// Backward-compatible re-export: SubmitAdapter is a type alias for SourceAdapter.
pub use adapter::SubmitAdapter;

pub use config::{
    check_disk_space_mb, BuildConfig, BuildOnFail, DiffConfig, GitConfig, PerforceConfig,
    ShellConfig, StagingConfig, SubmitConfig, SvnConfig, SyncConfig, VerifyCommand, VerifyConfig,
    VerifyOnFailure, WorkflowConfig,
};
pub use git::GitAdapter;
pub use none::NoneAdapter;
pub use perforce::PerforceAdapter;
pub use registry::{detect_adapter, known_adapters, select_adapter, select_adapter_with_sync};
pub use svn::SvnAdapter;
