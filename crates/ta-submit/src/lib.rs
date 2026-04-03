//! Source adapters for VCS integration
//!
//! This crate provides pluggable adapters for source control operations through
//! different version control systems and workflows. The core abstraction is the
//! `SourceAdapter` trait (unified from the former `SubmitAdapter` in v0.11.1),
//! with built-in implementations for Git and "none" fallback, plus external
//! plugin support for Perforce, SVN, and any community VCS (v0.13.5).
//!
//! ## External VCS plugins (v0.13.5)
//!
//! Perforce and SVN adapters are now external plugins using the JSON-over-stdio
//! protocol (same as channel plugins). Plugins are discovered from:
//! - `.ta/plugins/vcs/<name>/` — project-local
//! - `~/.config/ta/plugins/vcs/<name>/` — user-global
//! - `ta-submit-<name>` on `$PATH` — bare executable fallback
//!
//! Git remains built-in as the zero-configuration default.

pub mod adapter;
pub mod config;
pub mod external_vcs_adapter;
pub mod git;
pub mod none;
pub mod perforce;
pub mod registry;
pub mod svn;
pub mod vcs_plugin_manifest;
pub mod vcs_plugin_protocol;

// Primary exports (v0.11.1+)
pub use adapter::{
    CommitResult, MergeResult, PushResult, ReviewResult, ReviewStatus, SavedVcsState,
    SourceAdapter, SyncResult,
};

// Backward-compatible re-export: SubmitAdapter is a type alias for SourceAdapter.
pub use adapter::SubmitAdapter;

pub use config::{
    check_disk_space_mb, resolve_plan_path, ApplyConfig, AssetDiffConfig, BuildConfig, BuildOnFail,
    CommitConfig, ContextMode, DiffConfig, DraftReviewConfig, GitConfig, PerforceConfig,
    PlanConfig, ShellConfig, StagingConfig, SubmitConfig, SvnConfig, SyncConfig, TaLocalPaths,
    TaPathConfig, TaProjectPaths, VcsAgentConfig, VcsConfig, VerifyCommand, VerifyConfig,
    VerifyOnFailure, WorkflowConfig,
};
pub use external_vcs_adapter::ExternalVcsAdapter;
pub use git::GitAdapter;
pub use none::NoneAdapter;
pub use perforce::PerforceAdapter;
pub use registry::{
    detect_adapter, enforce_section15, enforce_section15_plugin, known_adapters, select_adapter,
    select_adapter_with_sync,
};
pub use svn::SvnAdapter;
pub use vcs_plugin_manifest::{
    discover_vcs_plugins, find_vcs_plugin, DiscoveredVcsPlugin, VcsPluginError, VcsPluginManifest,
    VcsPluginSource,
};
pub use vcs_plugin_protocol::PROTOCOL_VERSION;
