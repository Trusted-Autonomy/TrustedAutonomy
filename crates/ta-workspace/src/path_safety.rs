// path_safety.rs — Validation of agent-supplied, workspace-relative paths.
//
// Security hypothesis H5 (docs/superpowers/specs/security-hypotheses.md):
// a path an agent hands to a TA tool (`ta_fs_read`, `ta_fs_write`, ...)
// must never reach a file outside the workspace/staging root it is resolved
// against. `PathBuf::join` silently discards the base when the joined path
// is absolute, so every resolver must validate first.
//
// Two layers live here:
//
// - `validate_relative_path` is purely lexical and host-independent: it
//   rejects POSIX absolute paths (`/x`), Windows drive (`C:\x`, `c:/x`,
//   `C:x`), rooted (`\x`) and UNC/device (`\\server\share`, `\\?\C:\`)
//   forms, and any `..` component under either separator, on every host.
//   A Windows-style absolute path must be rejected on Linux/macOS too (a
//   config or test written on one host must mean the same thing on
//   another), and a POSIX `/x` must be rejected on Windows, where it would
//   resolve to the root of the current drive.
// - `resolve_within_root` additionally follows symlinks: it canonicalizes
//   the deepest existing ancestor of the joined path and requires the
//   result to stay under the canonicalized root, so a symlink inside the
//   root (e.g. a repo committing `config -> ~/.aws`) cannot be used to
//   escape it.

use std::path::{Path, PathBuf};

/// Validate a caller-supplied path that is meant to be relative to some
/// root directory. Returns a human-readable reason on rejection.
///
/// Accepted: `a/b.txt`, `./a/b.txt`, `a//b.txt`, `docs/v1..v2.md` (dots
/// inside a file name are not traversal).
pub fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("empty path".to_string());
    }
    if path.contains('\0') {
        return Err("path contains a NUL byte".to_string());
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(
            "absolute or rooted path (paths must be relative to the workspace root)".to_string(),
        );
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(
            "Windows drive-qualified path (paths must be relative to the workspace root)"
                .to_string(),
        );
    }
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        return Err("home-directory path (paths must be relative to the workspace root)".into());
    }
    if path.split(['/', '\\']).any(|component| component == "..") {
        return Err("parent-directory ('..') component".to_string());
    }
    // Host-specific backstop: anything the host itself considers absolute
    // or prefixed (covers forms the lexical checks above might miss).
    let p = Path::new(path);
    if p.is_absolute() || p.has_root() {
        return Err("absolute path on this host".to_string());
    }
    Ok(())
}

/// Resolve `relative` against `root`, rejecting lexical escapes (see
/// [`validate_relative_path`]) and symlink escapes.
///
/// The returned path is `root.join(relative)` (not canonicalized), so
/// callers keep their existing path shape for error messages. The symlink
/// check canonicalizes the deepest existing ancestor of the joined path
/// (the file itself when it exists, else its nearest existing parent, which
/// covers writes of new files under a symlinked directory) and requires it
/// to stay inside the canonicalized root.
///
/// If `root` itself does not exist yet, there is nothing a symlink could
/// redirect through, so only the lexical check applies.
pub fn resolve_within_root(root: &Path, relative: &str) -> Result<PathBuf, String> {
    validate_relative_path(relative)?;
    let joined = root.join(relative);

    let canonical_root = match root.canonicalize() {
        Ok(r) => r,
        Err(_) => return Ok(joined),
    };

    let mut probe: &Path = &joined;
    loop {
        // symlink_metadata succeeds for dangling symlinks too, which then
        // fail canonicalize below and are rejected (fail closed).
        if probe.symlink_metadata().is_ok() {
            let resolved = probe.canonicalize().map_err(|e| {
                format!(
                    "could not resolve '{}' to check it stays inside the workspace root: {}",
                    probe.display(),
                    e
                )
            })?;
            if !resolved.starts_with(&canonical_root) {
                return Err(format!(
                    "resolves through a symlink to '{}', outside the workspace root '{}'",
                    resolved.display(),
                    canonical_root.display()
                ));
            }
            return Ok(joined);
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return Ok(joined),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn accepts_ordinary_relative_paths() {
        for p in [
            "a.txt",
            "./a.txt",
            "src/lib.rs",
            "src//lib.rs",
            "docs/v1..v2.md",
            ".ta/chat-scratch/notes.md",
            "a~b/c",
        ] {
            assert!(validate_relative_path(p).is_ok(), "should accept {:?}", p);
        }
    }

    #[test]
    fn rejects_posix_and_windows_absolute_forms_on_every_host() {
        for p in [
            "/etc/passwd",
            "//etc/passwd",
            "\\etc\\passwd",
            "C:\\Users\\me\\.aws\\credentials",
            "c:/Users/me",
            "C:relative-to-drive-cwd",
            "\\\\server\\share\\x",
            "\\\\?\\C:\\x",
            "~/.ssh/id_rsa",
            "~",
            "",
            "a\0b",
        ] {
            assert!(validate_relative_path(p).is_err(), "should reject {:?}", p);
        }
    }

    #[test]
    fn rejects_parent_components_under_either_separator() {
        for p in ["..", "../x", "a/../../x", "a\\..\\x", "a/..", "./.."] {
            assert!(validate_relative_path(p).is_err(), "should reject {:?}", p);
        }
    }

    #[test]
    fn resolve_within_root_accepts_new_and_existing_files() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a")).unwrap();
        std::fs::write(root.path().join("a/b.txt"), b"x").unwrap();
        assert!(resolve_within_root(root.path(), "a/b.txt").is_ok());
        assert!(resolve_within_root(root.path(), "a/new/deep.txt").is_ok());
        assert!(resolve_within_root(root.path(), "brand-new.txt").is_ok());
    }

    #[test]
    fn resolve_within_root_with_missing_root_is_lexical_only() {
        let root = tempdir().unwrap();
        let missing = root.path().join("not-created-yet");
        assert!(resolve_within_root(&missing, "x.txt").is_ok());
        assert!(resolve_within_root(&missing, "/x.txt").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_within_root_rejects_symlink_escape_for_existing_and_new_files() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"s").unwrap();
        symlink(outside.path(), root.path().join("link")).unwrap();
        symlink(outside.path().join("secret"), root.path().join("file-link")).unwrap();
        symlink(root.path().join("nowhere"), root.path().join("dangling")).unwrap();

        assert!(resolve_within_root(root.path(), "link/secret").is_err());
        assert!(resolve_within_root(root.path(), "link/new-file").is_err());
        assert!(resolve_within_root(root.path(), "file-link").is_err());
        assert!(resolve_within_root(root.path(), "dangling").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_within_root_accepts_symlink_that_stays_inside() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("real")).unwrap();
        std::fs::write(root.path().join("real/f"), b"x").unwrap();
        symlink(root.path().join("real"), root.path().join("alias")).unwrap();
        assert!(resolve_within_root(root.path(), "alias/f").is_ok());
        assert!(resolve_within_root(root.path(), "alias/new").is_ok());
    }
}
