//! path_safety.rs: the single source of truth for "is this path TA or VCS
//! infrastructure?" decisions.
//!
//! Every place that decides whether a relative path is infrastructure (staging
//! copy exclusion, diff exclusion, conflict snapshots, apply-time refusal)
//! must go through this module rather than comparing strings ad hoc.
//!
//! ## Why a normalizer is needed (red-team finding CR-06)
//!
//! The default macOS (APFS) and Windows (NTFS) filesystems are
//! case-insensitive, and Windows additionally:
//!
//! - strips trailing dots and spaces from each path component (`.git.` and
//!   `.git ` both open `.git`),
//! - exposes 8.3 short names (`GIT~1` may alias `.git`),
//! - accepts NTFS alternate-data-stream syntax (`.git::$INDEX_ALLOCATION` names
//!   the `.git` directory itself),
//! - treats both `/` and `\` as separators.
//!
//! A byte-for-byte comparison against `".git"` therefore lets a draft artifact
//! such as `.GIT/hooks/pre-commit` be applied straight into the real `.git/`
//! directory. This module folds all of those spellings onto one canonical form
//! before comparing, and matches infrastructure names as ANY path component,
//! not only the first.
//!
//! ## Fail-closed
//!
//! [`is_infrastructure_path`] returns `true` for a path that cannot be safely
//! normalized at all (parent traversal, absolute paths, stream syntax). Callers
//! that want the precise reason use [`check_relative_path`].
//!
//! ## Apply allowlist
//!
//! TA never writes into `.ta/` (or any other infrastructure directory) through
//! a draft artifact: `.ta/` is excluded from staging copies and from diffs, and
//! TA's own state files (`.ta/plan_history.jsonl`, `.ta/personas/*.toml`, the
//! apply journal, and so on) are written by TA-owned code paths directly. So
//! there is deliberately no allowlist of infrastructure paths that an artifact
//! may target. Shared project files such as `PLAN.md`, `CLAUDE.md` and
//! `Cargo.toml` are not infrastructure and are unaffected.

use std::path::{Component, Path, PathBuf};

/// Agent-runtime infrastructure directories. Always excluded from staging
/// copies and from diffs; never work product.
pub const AGENT_INFRA_DIRS: &[&str] = &[
    ".ta",
    ".claude-flow",
    ".hive-mind",
    ".swarm",
    ".projfs-scratch",
];

/// Version-control metadata directories. A write into one of these can plant
/// hooks or rewrite config that executes on the next VCS operation.
pub const VCS_METADATA_DIRS: &[&str] = &[".git", ".svn", ".hg"];

/// Why a relative path was judged unsafe or protected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSafetyIssue {
    /// The path is empty (or only `.` components).
    Empty,
    /// The path is absolute or carries a drive / UNC prefix.
    Absolute,
    /// A component is `..` (or folds to it on Windows, e.g. `.. `).
    ParentTraversal { component: String },
    /// A component uses NTFS alternate-data-stream type syntax (`::$DATA`,
    /// `:$INDEX_ALLOCATION`), which can name a directory rather than a file.
    AlternateDataStream { component: String },
    /// A component is, or aliases, an infrastructure directory.
    Infrastructure {
        component: String,
        canonical: &'static str,
    },
}

impl PathSafetyIssue {
    /// A one-line human explanation, suitable for error messages.
    pub fn describe(&self) -> String {
        match self {
            PathSafetyIssue::Empty => "path is empty".to_string(),
            PathSafetyIssue::Absolute => {
                "path is absolute or has a drive/UNC prefix; artifacts must be project-relative"
                    .to_string()
            }
            PathSafetyIssue::ParentTraversal { component } => format!(
                "component '{}' is a parent-directory traversal ('..')",
                component
            ),
            PathSafetyIssue::AlternateDataStream { component } => format!(
                "component '{}' uses NTFS alternate-data-stream syntax",
                component
            ),
            PathSafetyIssue::Infrastructure {
                component,
                canonical,
            } => {
                if component == canonical {
                    format!("'{}' is a protected infrastructure directory", canonical)
                } else {
                    format!(
                        "component '{}' aliases the protected infrastructure directory '{}' \
                         on case-insensitive or Windows filesystems",
                        component, canonical
                    )
                }
            }
        }
    }
}

impl std::fmt::Display for PathSafetyIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Unicode code points that HFS+ ignored in file names and that are invisible
/// when rendered. Stripped before comparing so `.g\u{200C}it` cannot sneak past.
fn is_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
    )
}

/// Fold one path component the way a case-insensitive, Windows-tolerant
/// filesystem would: Unicode lowercase, drop ignorable code points, and strip
/// trailing dots and spaces (Win32 path normalization). `.` and `..` are
/// returned unchanged.
pub fn fold_component(raw: &str) -> String {
    if raw == "." || raw == ".." {
        return raw.to_string();
    }
    let lowered: String = raw
        .chars()
        .filter(|c| !is_ignorable(*c))
        .flat_map(char::to_lowercase)
        .collect();
    lowered.trim_end_matches(['.', ' ']).to_string()
}

/// Split a relative path on both `/` and `\`, dropping empty and `.` parts.
fn split_components(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(['/', '\\'])
        .filter(|c| !c.is_empty() && *c != ".")
}

/// The part of a folded component before any `:` (the NTFS stream separator).
fn stream_base(folded: &str) -> &str {
    folded.split(':').next().unwrap_or(folded)
}

/// Does a folded component name `canonical` (an entry from the infra lists)?
fn folded_aliases(folded: &str, canonical: &str) -> bool {
    let base = stream_base(folded);
    if base == canonical {
        return true;
    }
    // Look-alike spelled with non-ASCII marks (e.g. `.gİt` lowercases to
    // `.gi\u{307}t`): compare the ASCII skeleton. Conservative by design.
    if !base.is_ascii() {
        let skeleton: String = base.chars().filter(char::is_ascii).collect();
        if skeleton == canonical {
            return true;
        }
    }
    // Windows 8.3 short name: `GIT~1`, `CLAUDE~1`, `HIVE-M~2`, optionally
    // with a short extension. Flag it when its stem is a prefix of the
    // canonical name with the leading dot removed.
    if let Some((stem, rest)) = base.split_once('~') {
        let digits = rest.split('.').next().unwrap_or("");
        if !stem.is_empty()
            && stem.chars().count() <= 8
            && !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
        {
            let stem = stem.trim_start_matches('.');
            let core = canonical.trim_start_matches('.');
            if !stem.is_empty() && core.starts_with(stem) {
                return true;
            }
        }
    }
    false
}

fn find_alias(raw_component: &str, names: &'static [&'static str]) -> Option<&'static str> {
    let folded = fold_component(raw_component);
    names
        .iter()
        .copied()
        .find(|canonical| folded_aliases(&folded, canonical))
}

/// Return the canonical infrastructure name that `raw_component` aliases,
/// considering both agent-infra and VCS metadata directories.
pub fn infrastructure_alias(raw_component: &str) -> Option<&'static str> {
    find_alias(raw_component, AGENT_INFRA_DIRS)
        .or_else(|| find_alias(raw_component, VCS_METADATA_DIRS))
}

/// Is this single path component (a file or directory name) an
/// agent-runtime infrastructure directory such as `.ta`, in any spelling?
///
/// Used by the staging copy, which must still copy an isolated staging
/// `.git` when the VCS adapter asks for it, so it only hard-excludes the
/// agent-infra set.
pub fn is_agent_infra_component(raw_component: &str) -> bool {
    find_alias(raw_component, AGENT_INFRA_DIRS).is_some()
}

/// Is this single path component any protected infrastructure directory
/// (agent infra or VCS metadata), in any spelling?
pub fn is_infrastructure_component(raw_component: &str) -> bool {
    infrastructure_alias(raw_component).is_some()
}

/// Does `raw_component` name the same directory as `canonical` on a
/// case-insensitive / Windows filesystem? Only meaningful when `canonical` is
/// itself a protected name; used to case-fold adapter-contributed VCS
/// exclude patterns such as `.git/`.
pub fn component_aliases(raw_component: &str, canonical: &str) -> bool {
    folded_aliases(&fold_component(raw_component), &fold_component(canonical))
}

/// Check a project-relative path (either separator) and report the first
/// reason it may not be read from staging or written into the real project.
pub fn check_relative_path(raw: &str) -> Result<(), PathSafetyIssue> {
    if raw.starts_with('/') || raw.starts_with('\\') {
        return Err(PathSafetyIssue::Absolute);
    }
    let mut saw_any = false;
    for (idx, component) in split_components(raw).enumerate() {
        saw_any = true;
        // Drive letter (`C:`) on the first component.
        if idx == 0 {
            let bytes = component.as_bytes();
            if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                return Err(PathSafetyIssue::Absolute);
            }
        }
        let folded = fold_component(component);
        if folded == ".." || (folded.is_empty() && component != ".") {
            // `..`, or something Windows folds to `..` / `.` such as `.. ` or `...`.
            return Err(PathSafetyIssue::ParentTraversal {
                component: component.to_string(),
            });
        }
        if folded.contains(":$") || folded.contains("::") {
            // Report infrastructure first when the stream names an infra dir:
            // it is the more specific (and more alarming) finding.
            if let Some(canonical) = infrastructure_alias(component) {
                return Err(PathSafetyIssue::Infrastructure {
                    component: component.to_string(),
                    canonical,
                });
            }
            return Err(PathSafetyIssue::AlternateDataStream {
                component: component.to_string(),
            });
        }
        if let Some(canonical) = infrastructure_alias(component) {
            return Err(PathSafetyIssue::Infrastructure {
                component: component.to_string(),
                canonical,
            });
        }
    }
    if !saw_any {
        return Err(PathSafetyIssue::Empty);
    }
    Ok(())
}

/// Is this project-relative path inside (or naming) a protected
/// infrastructure directory, in any spelling, at any depth?
///
/// Fail-closed: paths that cannot be safely normalized (parent traversal,
/// absolute paths, stream syntax) also return `true`. An empty path returns
/// `false`.
pub fn is_infrastructure_path(raw: &str) -> bool {
    !matches!(
        check_relative_path(raw),
        Ok(()) | Err(PathSafetyIssue::Empty)
    )
}

/// Resolve `rel` under `root` for writing, defending against both string and
/// filesystem tricks:
///
/// 1. [`check_relative_path`] on the raw string.
/// 2. Canonicalize the deepest existing ancestor of the destination (which
///    resolves symlinks and, on case-insensitive volumes, nothing more is
///    needed because the string check already folded case) and require it to
///    stay under the canonical `root`.
/// 3. Re-check the resolved project-relative path, so a symlink such as
///    `docs -> .git` cannot launder a write into an infrastructure directory.
///
/// Returns the joined (non-canonical) destination path on success.
pub fn resolve_for_write(root: &Path, rel: &str) -> Result<PathBuf, PathSafetyIssue> {
    check_relative_path(rel)?;
    let mut dst = root.to_path_buf();
    for component in split_components(rel) {
        dst.push(component);
    }

    let canon_root = match root.canonicalize() {
        Ok(p) => p,
        // Root does not exist yet: nothing on disk to traverse.
        Err(_) => return Ok(dst),
    };

    // Deepest existing ancestor (the destination itself when it exists, so a
    // destination that is a symlink is followed too).
    let mut existing = dst.as_path();
    let mut tail: Vec<String> = Vec::new();
    while existing.symlink_metadata().is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_string_lossy().into_owned());
                existing = parent;
            }
            _ => break,
        }
    }
    tail.reverse();

    let canon_existing = existing
        .canonicalize()
        .map_err(|_| PathSafetyIssue::Absolute)?;
    let rel_existing =
        canon_existing
            .strip_prefix(&canon_root)
            .map_err(|_| PathSafetyIssue::ParentTraversal {
                component: rel.to_string(),
            })?;

    let mut resolved_rel = String::new();
    for c in rel_existing.components() {
        if let Component::Normal(name) = c {
            if !resolved_rel.is_empty() {
                resolved_rel.push('/');
            }
            resolved_rel.push_str(&name.to_string_lossy());
        }
    }
    for name in &tail {
        if !resolved_rel.is_empty() {
            resolved_rel.push('/');
        }
        resolved_rel.push_str(name);
    }
    match check_relative_path(&resolved_rel) {
        Ok(()) | Err(PathSafetyIssue::Empty) => Ok(dst),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spelling here must be flagged as infrastructure on any host.
    const POSITIVES: &[(&str, &str)] = &[
        (".git/x", ".git"),
        (".GIT/x", ".git"),
        (".Git/hooks", ".git"),
        (".GIT/hooks/pre-commit", ".git"),
        (".TA/personas/x.toml", ".ta"),
        (".TA/personas/chief-of-staff.toml", ".ta"),
        (".git./x", ".git"),
        (".git /x", ".git"),
        (".git. . /x", ".git"),
        (".git::$INDEX_ALLOCATION/x", ".git"),
        (".git:$INDEX_ALLOCATION/x", ".git"),
        ("GIT~1/x", ".git"),
        ("git~2/hooks/pre-commit", ".git"),
        ("TA~1/personas/x.toml", ".ta"),
        ("CLAUDE~1/x", ".claude-flow"),
        ("sub/.git/config", ".git"),
        ("sub\\.GIT\\x", ".git"),
        ("./.git/x", ".git"),
        (".\\.ta\\x", ".ta"),
        (".git", ".git"),
        (".ta", ".ta"),
        (".SVN/entries", ".svn"),
        (".Hg/hgrc", ".hg"),
        (".Hive-Mind/x", ".hive-mind"),
        (".SWARM/x", ".swarm"),
        (".projfs-scratch/x", ".projfs-scratch"),
        (".g\u{200C}it/x", ".git"),
        (".\u{FEFF}git/x", ".git"),
        (".g\u{130}t/x", ".git"),
    ];

    /// Ordinary project paths that merely look similar. Must NOT be flagged.
    const NEGATIVES: &[&str] = &[
        ".github/workflows/x",
        ".gitignore",
        ".gitattributes",
        ".gitmodules",
        "src/.gitkeep",
        ".tablet/x",
        ".taignore",
        ".ta-secret-ignore",
        "docs/git/x.md",
        "ta/x",
        "PLAN.md",
        "CLAUDE.md",
        "Cargo.toml",
        "memory/notes.md",
        "src/progra~1.rs",
        "TABLET~1/x",
        "notes:2024.md",
        "sub/.gitkeep",
        ".swarmrc",
    ];

    #[test]
    fn flags_every_infrastructure_spelling() {
        for (path, canonical) in POSITIVES {
            assert!(
                is_infrastructure_path(path),
                "expected '{}' to be infrastructure",
                path.escape_unicode()
            );
            // Where the spelling is a direct alias (not traversal), the issue
            // must name the right canonical directory.
            match check_relative_path(path) {
                Err(PathSafetyIssue::Infrastructure { canonical: c, .. }) => {
                    assert_eq!(c, *canonical, "wrong canonical for '{}'", path)
                }
                other => panic!("'{}': expected Infrastructure, got {:?}", path, other),
            }
        }
    }

    #[test]
    fn does_not_flag_lookalike_project_paths() {
        for path in NEGATIVES {
            assert!(
                !is_infrastructure_path(path),
                "'{}' must not be infrastructure: {:?}",
                path,
                check_relative_path(path)
            );
        }
    }

    #[test]
    fn rejects_traversal_absolute_and_streams() {
        fn kind(issue: &PathSafetyIssue) -> &'static str {
            match issue {
                PathSafetyIssue::Empty => "empty",
                PathSafetyIssue::Absolute => "absolute",
                PathSafetyIssue::ParentTraversal { .. } => "traversal",
                PathSafetyIssue::AlternateDataStream { .. } => "stream",
                PathSafetyIssue::Infrastructure { .. } => "infra",
            }
        }
        let cases: &[(&str, &str)] = &[
            ("../x", "traversal"),
            ("a/../../x", "traversal"),
            ("a\\..\\x", "traversal"),
            (".. /x", "traversal"),
            ("a/.../x", "traversal"),
            ("/etc/passwd", "absolute"),
            ("\\\\server\\share\\x", "absolute"),
            ("C:\\Windows\\x", "absolute"),
            ("c:/x", "absolute"),
            ("src/file.rs::$DATA", "stream"),
            ("", "empty"),
            ("./", "empty"),
        ];
        for (path, expected) in cases {
            let err = check_relative_path(path).expect_err(path);
            assert_eq!(kind(&err), *expected, "'{}' gave {:?}", path, err);
        }
        // Fail-closed for everything except Empty.
        assert!(is_infrastructure_path("../x"));
        assert!(is_infrastructure_path("/etc/passwd"));
        assert!(!is_infrastructure_path(""));
    }

    #[test]
    fn component_helpers_agree_with_path_check() {
        assert!(is_agent_infra_component(".TA"));
        assert!(is_agent_infra_component(".ta."));
        assert!(!is_agent_infra_component(".git"));
        assert!(is_infrastructure_component(".GIT"));
        assert!(!is_infrastructure_component(".github"));
        assert!(component_aliases(".GIT", ".git"));
        assert!(component_aliases("GIT~1", ".git"));
        assert!(!component_aliases(".gitignore", ".git"));
    }

    /// Simulate a case-insensitive, Windows-tolerant filesystem without
    /// depending on the host: a "volume" is a set of folded first components.
    /// Every positive spelling must collide with the real infra entry on that
    /// volume (that is the attack), and the guard must refuse each of them.
    #[test]
    fn case_insensitive_volume_simulation() {
        fn volume_key(component: &str) -> String {
            // What APFS / NTFS would resolve the directory entry to.
            stream_base(&fold_component(component)).to_string()
        }
        let real_entries = [".git", ".ta", ".svn", ".hg"];
        for spelling in [
            ".GIT",
            ".Git",
            ".git.",
            ".git ",
            ".git::$INDEX_ALLOCATION",
            ".TA",
            ".Ta.",
        ] {
            let key = volume_key(spelling);
            assert!(
                real_entries.contains(&key.as_str()),
                "simulation: '{}' should resolve to a real infra entry, got '{}'",
                spelling,
                key
            );
            let artifact = format!("{}/hooks/pre-commit", spelling);
            assert!(
                is_infrastructure_path(&artifact),
                "guard missed '{}' which the simulated volume maps onto '{}'",
                artifact,
                key
            );
        }
        // And names that resolve to a different entry stay allowed.
        for spelling in [".github", ".gitignore", ".tablet"] {
            assert!(!real_entries.contains(&volume_key(spelling).as_str()));
            assert!(!is_infrastructure_path(&format!("{}/x", spelling)));
        }
    }

    #[test]
    fn resolve_for_write_accepts_normal_paths() {
        let dir = tempfile::tempdir().unwrap();
        let dst = resolve_for_write(dir.path(), "src/new/file.rs").unwrap();
        assert_eq!(dst, dir.path().join("src").join("new").join("file.rs"));
        std::fs::write(dir.path().join("PLAN.md"), "x").unwrap();
        resolve_for_write(dir.path(), "PLAN.md").unwrap();
    }

    #[test]
    fn resolve_for_write_refuses_infra_spellings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        for rel in [".GIT/hooks/pre-commit", "GIT~1/hooks/x", ".git./hooks/x"] {
            assert!(
                matches!(
                    resolve_for_write(dir.path(), rel),
                    Err(PathSafetyIssue::Infrastructure { .. })
                ),
                "{}",
                rel
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_for_write_refuses_symlink_into_infra_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        std::os::unix::fs::symlink(dir.path().join(".git"), dir.path().join("docs")).unwrap();
        let err = resolve_for_write(dir.path(), "docs/hooks/pre-commit").unwrap_err();
        assert!(
            matches!(
                err,
                PathSafetyIssue::Infrastructure {
                    canonical: ".git",
                    ..
                }
            ),
            "{:?}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_for_write_refuses_symlink_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let err = resolve_for_write(dir.path(), "escape/x.txt").unwrap_err();
        assert!(
            matches!(err, PathSafetyIssue::ParentTraversal { .. }),
            "{:?}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_for_write_refuses_existing_file_symlinked_into_infra() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]\n").unwrap();
        std::os::unix::fs::symlink(dir.path().join(".git/config"), dir.path().join("cfg")).unwrap();
        assert!(resolve_for_write(dir.path(), "cfg").is_err());
    }
}
