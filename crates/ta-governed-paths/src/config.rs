//! Configuration types for governed paths (workflow.toml `[[governed_paths]]`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Access mode for a governed path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PathMode {
    /// Reads pass through; writes are captured in the SHA store and journal.
    /// Default.
    #[default]
    ReadWrite,
    /// Writes are blocked at the intercept layer; reads pass through.
    ReadOnly,
}

impl std::fmt::Display for PathMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathMode::ReadWrite => write!(f, "read-write"),
            PathMode::ReadOnly => write!(f, "read-only"),
        }
    }
}

/// A single `[[governed_paths]]` entry in workflow.toml.
///
/// ```toml
/// [[governed_paths]]
/// path = "data/outputs"
/// mode = "read-write"
/// purpose = "ComfyUI render outputs"
/// max_sha_store_mb = 1024
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernedPathConfig {
    /// Path to govern, relative to the workspace root.
    pub path: PathBuf,

    /// Access mode: `"read-write"` (default) or `"read-only"`.
    #[serde(default)]
    pub mode: PathMode,

    /// Human-readable description of what this path contains.
    #[serde(default)]
    pub purpose: String,

    /// Maximum total size in MB of SHA blobs for this path (default: unlimited).
    #[serde(default)]
    pub max_sha_store_mb: Option<u64>,
}

impl GovernedPathConfig {
    /// Returns true if `candidate` (relative to workspace root) falls under this governed path.
    pub fn governs(&self, candidate: &std::path::Path) -> bool {
        candidate.starts_with(&self.path)
    }
}
