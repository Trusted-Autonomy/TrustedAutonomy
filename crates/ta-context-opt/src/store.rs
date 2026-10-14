use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::OptError;

/// Content-addressed store for the full original of anything an optimizer
/// reduces. Same content, same reference, written once.
#[derive(Debug, Clone)]
pub struct OriginalStore {
    dir: PathBuf,
}

impl OriginalStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Conventional location under a project root.
    pub fn for_project(project_root: &Path) -> Self {
        Self::new(
            project_root
                .join(".ta")
                .join("compression")
                .join("originals"),
        )
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Save `content` and return its reference (SHA-256, lowercase hex).
    pub fn save(&self, content: &str) -> Result<String, OptError> {
        let reference: String = Sha256::digest(content.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let path = self.dir.join(&reference);
        if path.exists() {
            return Ok(reference);
        }
        let wrap = |source| OptError::Store {
            path: path.display().to_string(),
            source,
        };
        std::fs::create_dir_all(&self.dir).map_err(wrap)?;
        std::fs::write(&path, content).map_err(wrap)?;
        Ok(reference)
    }

    /// Read back the original for `reference`.
    pub fn recover(&self, reference: &str) -> Result<String, OptError> {
        // Reject anything that is not a bare hex digest so a reference can
        // never escape the store directory.
        if reference.len() != 64
            || !reference
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(OptError::BadReference(reference.to_string()));
        }
        std::fs::read_to_string(self.dir.join(reference))
            .map_err(|_| OptError::MissingOriginal(reference.to_string()))
    }
}
