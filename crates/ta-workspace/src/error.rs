// error.rs — Error types for the workspace subsystem.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during workspace operations.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    /// A file I/O operation failed.
    #[error("I/O error at {path}: {source}")]
    IoError {
        path: PathBuf,
        source: std::io::Error,
    },

    /// A path traversal attempt was detected (security violation).
    #[error("path traversal detected: '{path}' resolves outside staging directory")]
    PathTraversal { path: String },

    /// The requested file was not found in the staging workspace.
    #[error("file not found in staging: '{path}'")]
    FileNotFound { path: String },

    /// Failed to serialize/deserialize changeset data.
    #[error("serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// The change store operation failed.
    #[error("change store error: {0}")]
    StoreError(String),

    /// Conflict detected between source and staging (v0.2.1).
    #[error("Concurrent session conflict detected:\n{}", .conflicts.join("\n"))]
    ConflictDetected { conflicts: Vec<String> },

    /// A Windows Projected File System operation failed (v0.15.8).
    #[error("ProjFS error: {0}")]
    ProjFsError(String),

    /// One or more shared files (PLAN.md, CLAUDE.md, Cargo.toml, memory/*.md)
    /// have unresolved 3-way merge conflicts at apply time (v0.17.0.12.7).
    ///
    /// Unlike `ConflictDetected`, shared files always attempt an automatic
    /// merge first regardless of the caller's `ConflictResolution` — this
    /// variant is only returned when that merge left conflict markers.
    #[error(
        "{} shared file(s) have unresolved merge conflicts: {}",
        .conflicts.len(),
        .conflicts.iter().map(|c| c.path.as_str()).collect::<Vec<_>>().join(", ")
    )]
    SharedFileConflicts { conflicts: Vec<SharedFileConflict> },
}

/// A shared file whose apply-time 3-way merge left conflict markers.
#[derive(Debug, Clone)]
pub struct SharedFileConflict {
    /// Relative path from the workspace root.
    pub path: String,
    /// Merged content with `<<<<<<<`/`=======`/`>>>>>>>` conflict markers.
    pub conflicted_content: Vec<u8>,
}
