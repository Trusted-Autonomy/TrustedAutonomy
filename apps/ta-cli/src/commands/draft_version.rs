//! Version-file recovery and consistency checks for `ta draft apply`.
//!
//! An interrupted apply can leave a half-finished version bump in the working
//! tree. The next apply sees most manifests already current, so
//! `bump_workspace_version` reports only the files it changed this time. These
//! helpers make sure the whole bump is committed, and refuse to commit a tree
//! whose manifests disagree about the workspace version.

use std::path::{Path, PathBuf};
use std::process::Command;

/// File names that carry the workspace version.
const VERSION_FILE_NAMES: [&str; 4] = ["Cargo.toml", "Cargo.lock", "CLAUDE.md", ".release.toml"];

/// Category of a dirty working-tree path, used to report what apply found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyCategory {
    Version,
    Plan,
    Other,
}

/// A manifest whose version disagrees with the workspace version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMismatch {
    /// Manifest path relative to the workspace root, with `/` separators.
    pub manifest: String,
    /// What the manifest says, e.g. `requires ta-changeset ^0.17.12-alpha.5`.
    pub detail: String,
}

/// True when `rel_path` is a version file by pattern (`Cargo.toml`, `Cargo.lock`,
/// `CLAUDE.md`, `.release.toml`, at any depth).
pub fn is_version_file(rel_path: &str) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    VERSION_FILE_NAMES.contains(&name)
}

pub fn classify_dirty_path(rel_path: &str) -> DirtyCategory {
    if is_version_file(rel_path) {
        DirtyCategory::Version
    } else if rel_path == "PLAN.md" {
        DirtyCategory::Plan
    } else {
        DirtyCategory::Other
    }
}

/// Parse `git status --porcelain` into `(xy, path)` pairs. Renames keep the
/// destination path. Untracked `.ta/` paths are skipped (TA-internal).
pub fn parse_porcelain(porcelain: &str) -> Vec<(String, String)> {
    porcelain
        .lines()
        .filter(|l| l.len() >= 4)
        .filter_map(|l| {
            let xy = l[..2].to_string();
            let mut path = l[3..].trim();
            if let Some((_, dest)) = path.split_once(" -> ") {
                path = dest;
            }
            let path = path.trim_matches('"').to_string();
            if xy.contains('?') && (path.starts_with(".ta/") || path == ".ta") {
                None
            } else {
                Some((xy, path))
            }
        })
        .collect()
}

/// Describe a dirty tree by category. Returns `None` when there is nothing to
/// report. The text states what apply will include and what it will leave.
pub fn summarize_dirty(porcelain: &str) -> Option<String> {
    let entries = parse_porcelain(porcelain);
    if entries.is_empty() {
        return None;
    }
    let mut version = Vec::new();
    let mut plan = Vec::new();
    let mut other = Vec::new();
    for (xy, path) in &entries {
        // Untracked files are never staged by apply, whatever they are called.
        let cat = if xy.contains('?') {
            DirtyCategory::Other
        } else {
            classify_dirty_path(path)
        };
        match cat {
            DirtyCategory::Version => version.push(path.as_str()),
            DirtyCategory::Plan => plan.push(path.as_str()),
            DirtyCategory::Other => other.push(path.as_str()),
        }
    }
    let list = |v: &[&str]| -> String {
        const SHOW: usize = 5;
        let mut s = v.iter().take(SHOW).copied().collect::<Vec<_>>().join(", ");
        if v.len() > SHOW {
            s.push_str(&format!(", and {} more", v.len() - SHOW));
        }
        s
    };
    let mut out = format!(
        "[apply] Working tree is not clean ({} path(s)). This usually means an earlier apply was interrupted.\n",
        entries.len()
    );
    if !version.is_empty() {
        out.push_str(&format!(
            "[apply]   version files ({}): will be INCLUDED in the commit if they match the version being applied: {}\n",
            version.len(),
            list(&version)
        ));
    }
    if !plan.is_empty() {
        out.push_str(&format!(
            "[apply]   plan files ({}): will be rewritten by this apply: {}\n",
            plan.len(),
            list(&plan)
        ));
    }
    if !other.is_empty() {
        out.push_str(&format!(
            "[apply]   other files ({}): will be LEFT as they are, not committed: {}\n",
            other.len(),
            list(&other)
        ));
    }
    Some(out)
}

fn git(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_CEILING_DIRECTORIES");
    cmd
}

/// Stage every tracked version file that differs from `HEAD`, including a bump
/// left in the working tree by an interrupted run. Returns the staged paths.
pub fn stage_version_files(dir: &Path) -> anyhow::Result<Vec<String>> {
    let out = git(dir, &["status", "--porcelain"]).output().map_err(|e| {
        anyhow::anyhow!(
            "could not run `git status` in {} to find version files to stage: {e}",
            dir.display()
        )
    })?;
    if !out.status.success() {
        anyhow::bail!(
            "`git status --porcelain` failed in {}: {}. Fix the repository state, then re-run `ta draft apply`.",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut staged = Vec::new();
    for (xy, path) in parse_porcelain(&text) {
        if xy.contains('?') || !is_version_file(&path) {
            continue;
        }
        // Skip entries already fully staged (second column clean).
        if xy.as_bytes()[1] == b' ' {
            continue;
        }
        let add = git(dir, &["add", "--", &path]).output()?;
        if !add.status.success() {
            anyhow::bail!(
                "`git add {path}` failed: {}. Stage it manually or restore it with `git restore {path}`.",
                String::from_utf8_lossy(&add.stderr).trim()
            );
        }
        staged.push(path);
    }
    Ok(staged)
}

fn manifests(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for pattern in ["apps/**/Cargo.toml", "crates/**/Cargo.toml"] {
        if let Ok(entries) = glob::glob(&dir.join(pattern).to_string_lossy()) {
            found.extend(entries.flatten());
        }
    }
    found.sort();
    found
}

/// Find manifests that disagree with `workspace_version`: a literal
/// `[package] version`, or a `ta-*` path dependency with a different `version`
/// requirement. This is the condition that makes cargo fail with
/// `failed to select a version for the requirement`.
pub fn find_version_mismatches(dir: &Path, workspace_version: &str) -> Vec<VersionMismatch> {
    let dep_re = regex::Regex::new(r#"(ta-[a-z0-9-]+)\s*=\s*\{[^}]*version\s*=\s*"([^"]+)""#)
        .expect("static regex");
    let pkg_re = regex::Regex::new(r#"(?m)^version\s*=\s*"([^"]+)""#).expect("static regex");
    let mut out = Vec::new();
    for manifest in manifests(dir) {
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let rel = manifest
            .strip_prefix(dir)
            .unwrap_or(&manifest)
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(c) = pkg_re.captures(&text) {
            if &c[1] != workspace_version {
                out.push(VersionMismatch {
                    manifest: rel.clone(),
                    detail: format!("declares version {}", &c[1]),
                });
            }
        }
        for c in dep_re.captures_iter(&text) {
            let req = c[2].trim_start_matches(['^', '=', '~']);
            if req != workspace_version {
                out.push(VersionMismatch {
                    manifest: rel.clone(),
                    detail: format!("requires {} {}", &c[1], &c[2]),
                });
            }
        }
    }
    out
}

/// Fail with the list of disagreeing crates, before a commit that could not build.
pub fn verify_version_consistency(dir: &Path) -> anyhow::Result<()> {
    let Some(ws) = super::draft::read_cargo_version(dir) else {
        return Ok(());
    };
    let mismatches = find_version_mismatches(dir, &ws);
    if mismatches.is_empty() {
        return Ok(());
    }
    let mut msg = format!(
        "Refusing to commit: {} manifest entr{} disagree with the workspace version {ws}, \
         so cargo could not resolve the workspace:\n",
        mismatches.len(),
        if mismatches.len() == 1 { "y" } else { "ies" }
    );
    for m in &mismatches {
        msg.push_str(&format!("  {}: {}\n", m.manifest, m.detail));
    }
    msg.push_str(&format!(
        "Fix: run ./scripts/bump-version.sh {ws} to align every manifest, then re-run `ta draft apply`."
    ));
    anyhow::bail!(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = git(dir, args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn workspace(ver: &str, dep_ver: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(
            d,
            "Cargo.toml",
            &format!("[workspace.package]\nversion = \"{ver}\"\n"),
        );
        for name in ["a", "b"] {
            write(
                d,
                &format!("crates/ta-{name}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"ta-{name}\"\nversion.workspace = true\n[dependencies]\nta-x = {{ path = \"../ta-x\", version = \"^{dep_ver}\" }}\n"
                ),
            );
        }
        write(d, "src.txt", "x\n");
        run(d, &["init", "-q"]);
        run(d, &["config", "user.email", "t@t"]);
        run(d, &["config", "user.name", "t"]);
        run(d, &["add", "-A"]);
        run(d, &["commit", "-q", "-m", "base"]);
        dir
    }

    #[test]
    fn classifies_paths() {
        assert!(is_version_file("crates/ta-a/Cargo.toml"));
        assert!(is_version_file("Cargo.lock"));
        assert!(!is_version_file("src/main.rs"));
        assert_eq!(classify_dirty_path("PLAN.md"), DirtyCategory::Plan);
        assert_eq!(classify_dirty_path("x/y.rs"), DirtyCategory::Other);
    }

    #[test]
    fn summary_groups_by_category_and_skips_ta_dir() {
        let s = summarize_dirty(" M Cargo.toml\n M crates/a/Cargo.toml\n M PLAN.md\n M src/x.rs\n?? .ta/foo\n?? notes.txt\n")
            .unwrap();
        assert!(s.contains("version files (2)"), "{s}");
        assert!(s.contains("plan files (1)"), "{s}");
        assert!(s.contains("other files (2)"), "{s}");
        assert!(s.contains("LEFT"), "{s}");
        assert!(!s.contains(".ta/foo"), "{s}");
        assert!(summarize_dirty("?? .ta/x\n").is_none());
    }

    #[test]
    fn half_bumped_tree_is_fully_staged() {
        let dir = workspace("0.1.0-alpha.4", "0.1.0-alpha.4");
        let d = dir.path();
        // Simulate an interrupted apply: every manifest already bumped, nothing staged.
        write(
            d,
            "Cargo.toml",
            "[workspace.package]\nversion = \"0.1.0-alpha.5\"\n",
        );
        for name in ["a", "b"] {
            write(
                d,
                &format!("crates/ta-{name}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"ta-{name}\"\nversion.workspace = true\n[dependencies]\nta-x = {{ path = \"../ta-x\", version = \"^0.1.0-alpha.5\" }}\n"
                ),
            );
        }
        write(d, "src.txt", "unrelated change\n");
        write(d, "untracked.txt", "u\n");

        let mut staged = stage_version_files(d).unwrap();
        staged.sort();
        assert_eq!(
            staged,
            vec![
                "Cargo.toml",
                "crates/ta-a/Cargo.toml",
                "crates/ta-b/Cargo.toml"
            ]
        );
        let out = git(d, &["diff", "--name-only"]).output().unwrap();
        let unstaged = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            unstaged.trim(),
            "src.txt",
            "non-version file must stay unstaged"
        );
        assert!(verify_version_consistency(d).is_ok());
    }

    #[test]
    fn inconsistent_tree_is_refused_with_crate_list() {
        let dir = workspace("0.1.0-alpha.5", "0.1.0-alpha.4");
        let err = verify_version_consistency(dir.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("crates/ta-a/Cargo.toml"), "{err}");
        assert!(err.contains("crates/ta-b/Cargo.toml"), "{err}");
        assert!(err.contains("ta-x ^0.1.0-alpha.4"), "{err}");
        assert!(err.contains("bump-version.sh 0.1.0-alpha.5"), "{err}");
    }

    #[test]
    fn clean_consistent_tree_is_unchanged() {
        let dir = workspace("0.1.0-alpha.4", "0.1.0-alpha.4");
        assert!(stage_version_files(dir.path()).unwrap().is_empty());
        assert!(verify_version_consistency(dir.path()).is_ok());
    }
}
