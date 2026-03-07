//! Submit adapters for VCS integration
//!
//! This crate provides pluggable adapters for submitting changes through different
//! version control systems and workflows. The core abstraction is the `SubmitAdapter`
//! trait, with built-in implementations for Git, SVN (stub), Perforce (stub),
//! and a "none" fallback.

pub mod adapter;
pub mod config;
pub mod git;
pub mod none;
pub mod perforce;
pub mod registry;
pub mod svn;

pub use adapter::{CommitResult, PushResult, ReviewResult, SavedVcsState, SubmitAdapter};
pub use config::{BuildConfig, DiffConfig, GitConfig, SubmitConfig, WorkflowConfig};
pub use git::GitAdapter;
pub use none::NoneAdapter;
pub use perforce::PerforceAdapter;
pub use registry::{detect_adapter, known_adapters, select_adapter};
pub use svn::SvnAdapter;
