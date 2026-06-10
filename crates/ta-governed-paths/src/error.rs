use thiserror::Error;

#[derive(Debug, Error)]
pub enum GovernedPathError {
    #[error("I/O error on path {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("SHA store is full: {path} exceeds {max_mb} MB limit")]
    StoreFull { path: String, max_mb: u64 },

    #[error("Write blocked: {path} is a read-only governed path")]
    ReadOnly { path: String },

    #[error("SHA blob {sha} not found in store")]
    BlobNotFound { sha: String },

    #[error("Journal serialization error: {0}")]
    Journal(#[from] serde_json::Error),

    #[error("Journal is corrupt at line {line}: {detail}")]
    CorruptJournal { line: usize, detail: String },
}

impl GovernedPathError {
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
