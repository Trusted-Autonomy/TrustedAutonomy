//! Content-addressed SHA-256 blob store at `.ta/sha-fs/<sha256>`.
//!
//! Blobs are immutable: a SHA that already exists on disk is never overwritten.
//! De-duplication is automatic because identical content produces the same hash.

use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::GovernedPathError;

/// Content-addressed blob store rooted at `<workspace>/.ta/sha-fs/`.
pub struct ShaStore {
    root: PathBuf,
}

impl ShaStore {
    /// Open (or create) the store at `<workspace_root>/.ta/sha-fs`.
    pub fn open(workspace_root: &Path) -> Result<Self, GovernedPathError> {
        let root = workspace_root.join(".ta").join("sha-fs");
        std::fs::create_dir_all(&root)
            .map_err(|e| GovernedPathError::io(root.display().to_string(), e))?;
        Ok(Self { root })
    }

    /// Compute the SHA-256 of `data` and store the blob if not already present.
    ///
    /// Returns the hex-encoded SHA-256 digest.
    pub fn write_bytes(&self, data: &[u8]) -> Result<String, GovernedPathError> {
        let sha = Self::sha256_hex(data);
        let blob_path = self.blob_path(&sha);

        // Blob already present — nothing to do (immutable, no overwrite needed).
        if blob_path.exists() {
            return Ok(sha);
        }

        // Write atomically via a temp file in the same directory so a crash
        // during write does not leave a partial blob.
        let tmp_path = blob_path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp_path)
                .map_err(|e| GovernedPathError::io(tmp_path.display().to_string(), e))?;
            f.write_all(data)
                .map_err(|e| GovernedPathError::io(tmp_path.display().to_string(), e))?;
        }
        std::fs::rename(&tmp_path, &blob_path)
            .map_err(|e| GovernedPathError::io(blob_path.display().to_string(), e))?;

        Ok(sha)
    }

    /// Read and hash a file, storing its content as a blob.
    ///
    /// Returns the SHA-256 hex digest.
    pub fn write_file(&self, path: &Path) -> Result<String, GovernedPathError> {
        let data = std::fs::read(path)
            .map_err(|e| GovernedPathError::io(path.display().to_string(), e))?;
        self.write_bytes(&data)
    }

    /// Compute SHA-256 of a file without storing a blob.
    ///
    /// Used for pre-goal snapshots where we only need the hash to later detect
    /// whether the file has changed.
    pub fn sha256_of_file(path: &Path) -> Result<String, GovernedPathError> {
        let data = std::fs::read(path)
            .map_err(|e| GovernedPathError::io(path.display().to_string(), e))?;
        Ok(Self::sha256_hex(&data))
    }

    /// Read a blob by its SHA-256 hex digest.
    pub fn read_blob(&self, sha: &str) -> Result<Vec<u8>, GovernedPathError> {
        let blob_path = self.blob_path(sha);
        std::fs::read(&blob_path).map_err(|_| GovernedPathError::BlobNotFound {
            sha: sha.to_string(),
        })
    }

    /// Write a blob's content to `dest`, replacing the file atomically.
    ///
    /// Creates parent directories if needed.
    pub fn restore_to(&self, sha: &str, dest: &Path) -> Result<(), GovernedPathError> {
        let data = self.read_blob(sha)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| GovernedPathError::io(parent.display().to_string(), e))?;
        }
        let tmp = dest.with_extension("sha-fs.tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| GovernedPathError::io(tmp.display().to_string(), e))?;
            f.write_all(&data)
                .map_err(|e| GovernedPathError::io(tmp.display().to_string(), e))?;
        }
        std::fs::rename(&tmp, dest)
            .map_err(|e| GovernedPathError::io(dest.display().to_string(), e))?;
        Ok(())
    }

    /// Return true if a blob with the given SHA exists in the store.
    pub fn has_blob(&self, sha: &str) -> bool {
        self.blob_path(sha).exists()
    }

    /// Compute total size in bytes of all blobs in the store.
    pub fn total_bytes(&self) -> u64 {
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return 0;
        };
        rd.filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }

    /// List all SHA hex strings currently stored (no particular order).
    pub fn list_shas(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return vec![];
        };
        rd.filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy();
                // Only 64-char hex names are blobs (skip .tmp and other junk).
                if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                    Some(s.into_owned())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Delete a single blob.  Returns true if the blob existed and was removed.
    pub fn remove_blob(&self, sha: &str) -> Result<bool, GovernedPathError> {
        let path = self.blob_path(sha);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(GovernedPathError::io(path.display().to_string(), e)),
        }
    }

    // ── internal helpers ─────────────────────────────────────────────────────

    fn blob_path(&self, sha: &str) -> PathBuf {
        self.root.join(sha)
    }

    pub fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_bytes() {
        let dir = tempdir().unwrap();
        let store = ShaStore::open(dir.path()).unwrap();
        let data = b"hello governed world";
        let sha = store.write_bytes(data).unwrap();
        assert_eq!(sha.len(), 64);
        let got = store.read_blob(&sha).unwrap();
        assert_eq!(got, data);
    }

    #[test]
    fn dedup_same_content() {
        let dir = tempdir().unwrap();
        let store = ShaStore::open(dir.path()).unwrap();
        let sha1 = store.write_bytes(b"same").unwrap();
        let sha2 = store.write_bytes(b"same").unwrap();
        assert_eq!(sha1, sha2);
        // Only one blob on disk.
        assert_eq!(store.list_shas().len(), 1);
    }

    #[test]
    fn restore_to_creates_file() {
        let dir = tempdir().unwrap();
        let store = ShaStore::open(dir.path()).unwrap();
        let sha = store.write_bytes(b"restored content").unwrap();
        let dest = dir.path().join("sub").join("out.bin");
        store.restore_to(&sha, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"restored content");
    }

    #[test]
    fn missing_blob_error() {
        let dir = tempdir().unwrap();
        let store = ShaStore::open(dir.path()).unwrap();
        let result =
            store.read_blob("0000000000000000000000000000000000000000000000000000000000000000");
        assert!(matches!(
            result,
            Err(GovernedPathError::BlobNotFound { .. })
        ));
    }

    #[test]
    fn list_shas_and_remove() {
        let dir = tempdir().unwrap();
        let store = ShaStore::open(dir.path()).unwrap();
        let sha = store.write_bytes(b"to remove").unwrap();
        assert!(store.list_shas().contains(&sha));
        assert!(store.remove_blob(&sha).unwrap());
        assert!(!store.list_shas().contains(&sha));
    }

    #[test]
    fn total_bytes_counts_blobs() {
        let dir = tempdir().unwrap();
        let store = ShaStore::open(dir.path()).unwrap();
        store.write_bytes(b"abc").unwrap();
        store.write_bytes(b"def").unwrap();
        assert_eq!(store.total_bytes(), 6); // 3 + 3
    }
}
