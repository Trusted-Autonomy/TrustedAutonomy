//! `ta doctor` build-output audit (v0.17.12.25 item 9).
//!
//! Lists the Cargo `target/` directories under the workspace (the project
//! tree and every goal's staging copy) by size and age, says which are older
//! than [`OLD_TARGET_DAYS`], and removes exactly the directories the user
//! names with `ta doctor --remove-target <path>`. On macOS it also warns when
//! local APFS snapshots may be holding space that deletions did not free.
//!
//! Nothing here deletes on its own: the listing prints the exact command for
//! each old directory, and the removal refuses any path that is not one of the
//! `target/` directories the scan found.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::doctor::CheckResult;

/// A `target/` directory idle at least this long is offered for removal.
pub const OLD_TARGET_DAYS: u64 = 7;

/// How deep below the workspace root a `target/` directory is looked for:
/// `.ta/staging/<goal>/target` is four levels down; one more covers a
/// workspace member such as `apps/<crate>/target`.
const MAX_DEPTH: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDir {
    pub path: PathBuf,
    pub bytes: u64,
    /// Whole days since anything in the directory last changed.
    pub age_days: u64,
}

fn fmt_bytes(bytes: u64) -> String {
    const GB: f64 = 1_073_741_824.0;
    const MB: f64 = 1_048_576.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / MB)
    }
}

/// Cargo marks its output directories with `CACHEDIR.TAG` / `.rustc_info.json`;
/// a bare `debug` or `release` child also qualifies for older layouts.
fn looks_like_cargo_target(dir: &Path) -> bool {
    dir.join("CACHEDIR.TAG").is_file()
        || dir.join(".rustc_info.json").is_file()
        || dir.join("debug").is_dir()
        || dir.join("release").is_dir()
}

/// Bytes under `dir`, not following symlinks.
fn tree_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Newest modification time of the directory and its immediate children
/// (`debug/`, `release/`, `.rustc_info.json`, ...): a build touches those.
fn last_activity(dir: &Path) -> Option<SystemTime> {
    let mut newest = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                newest = Some(newest.map_or(t, |n| n.max(t)));
            }
        }
    }
    newest
}

fn walk(dir: &Path, depth: usize, now: SystemTime, out: &mut Vec<TargetDir>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        // Symlinks are never followed: a link must not lead a scan (or a
        // removal) outside the workspace.
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if name == "target" && looks_like_cargo_target(&path) {
            let age = last_activity(&path)
                .and_then(|t| now.duration_since(t).ok())
                .unwrap_or(Duration::ZERO);
            out.push(TargetDir {
                bytes: tree_bytes(&path),
                age_days: age.as_secs() / 86_400,
                path,
            });
            continue; // never look inside a build directory
        }
        // Not build output; version-control and dependency trees are large and
        // hold no `target/` of ours. The shared build cache is managed by
        // `ta cleanup`, not here.
        if matches!(&*name, ".git" | "node_modules" | "build-cache") || depth + 1 >= MAX_DEPTH {
            continue;
        }
        walk(&path, depth + 1, now, out);
    }
}

/// Every Cargo `target/` directory under `root`, largest first.
pub fn find_target_dirs(root: &Path) -> Vec<TargetDir> {
    let mut out = Vec::new();
    walk(root, 0, SystemTime::now(), &mut out);
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Remove the named directories, and only those. Each name must resolve to one
/// of `found`; anything else is refused and nothing is removed at all (the
/// request is validated in full before the first deletion).
pub fn remove_named(named: &[PathBuf], found: &[TargetDir]) -> anyhow::Result<Vec<(PathBuf, u64)>> {
    let mut chosen = Vec::new();
    for name in named {
        let resolved = std::fs::canonicalize(name).map_err(|e| {
            anyhow::anyhow!(
                "cannot remove {}: {e}. Nothing was removed. Run `ta doctor` to list the \
                 target/ directories it can remove.",
                name.display()
            )
        })?;
        let hit = found
            .iter()
            .find(|t| std::fs::canonicalize(&t.path).ok().as_deref() == Some(&resolved));
        match hit {
            Some(t) => chosen.push(t.clone()),
            None => anyhow::bail!(
                "{} is not a Cargo target/ directory under this workspace, so it was not \
                 removed (and nothing else was). Run `ta doctor` to list the directories it \
                 can remove, then name one of those.",
                name.display()
            ),
        }
    }
    let mut removed = Vec::new();
    for t in chosen {
        std::fs::remove_dir_all(&t.path).map_err(|e| {
            anyhow::anyhow!(
                "removing {} failed after {} director(ies) were removed: {e}. Check its \
                 permissions or whether a build is still running there, then re-run.",
                t.path.display(),
                removed.len()
            )
        })?;
        tracing::info!(path = %t.path.display(), bytes = t.bytes, "doctor removed a target/ directory");
        removed.push((t.path, t.bytes));
    }
    Ok(removed)
}

/// A warning when `tmutil listlocalsnapshots /` output shows local snapshots.
/// Deleted build output stays allocated while a snapshot references it.
pub fn snapshot_warning(tmutil_output: &str) -> Option<String> {
    let n = tmutil_output
        .lines()
        .filter(|l| l.contains("com.apple.TimeMachine."))
        .count();
    (n > 0).then(|| {
        format!(
            "{n} local APFS snapshot(s) exist; space freed by deleting files stays \
             allocated until they are thinned or expire (it does not show as free in `df`). \
             Reclaim it with `tmutil thinlocalsnapshots / 10000000000 4` (asks macOS to \
             free about 10 GB), or list them with `tmutil listlocalsnapshots /`."
        )
    })
}

/// The snapshot warning for this machine, if any (macOS only).
pub fn snapshot_warning_now() -> Option<String> {
    local_snapshot_warning()
}

#[cfg(target_os = "macos")]
fn local_snapshot_warning() -> Option<String> {
    let out = std::process::Command::new("tmutil")
        .args(["listlocalsnapshots", "/"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| snapshot_warning(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

#[cfg(not(target_os = "macos"))]
fn local_snapshot_warning() -> Option<String> {
    None
}

/// The `ta doctor` check: list the directories, offer removal of old ones.
pub fn check(workspace_root: &Path) -> Vec<CheckResult> {
    let dirs = find_target_dirs(workspace_root);
    let mut results = Vec::new();
    if dirs.is_empty() {
        results.push(CheckResult::ok(
            "Build output",
            "no target/ directories found",
        ));
    } else {
        let total: u64 = dirs.iter().map(|d| d.bytes).sum();
        let old: Vec<&TargetDir> = dirs
            .iter()
            .filter(|d| d.age_days >= OLD_TARGET_DAYS)
            .collect();
        let mut listing = String::new();
        for d in &dirs {
            let shown = d.path.strip_prefix(workspace_root).unwrap_or(&d.path);
            listing.push_str(&format!(
                "\n         {:>9}  idle {:>3}d  {}",
                fmt_bytes(d.bytes),
                d.age_days,
                shown.display()
            ));
        }
        let detail = format!(
            "{} target/ director(ies), {} total; {} idle {}+ days ({}){}",
            dirs.len(),
            fmt_bytes(total),
            old.len(),
            OLD_TARGET_DAYS,
            fmt_bytes(old.iter().map(|d| d.bytes).sum()),
            listing
        );
        if old.is_empty() {
            results.push(CheckResult::ok("Build output", detail));
        } else {
            let fix = old
                .iter()
                .map(|d| format!("ta doctor --remove-target {}", d.path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            results.push(CheckResult::warn(
                "Build output",
                detail,
                format!("remove only what you name (a rebuild recreates it):\n{fix}"),
            ));
        }
    }
    if let Some(w) = local_snapshot_warning() {
        results.push(CheckResult::warn(
            "Local snapshots",
            w,
            "after thinning, `df -h /` shows the freed space",
        ));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_target(dir: &Path, bytes: usize) {
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        std::fs::write(
            dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
        std::fs::write(dir.join("debug/artifact"), vec![0u8; bytes]).unwrap();
    }

    fn age(dir: &Path, days: u64) {
        let t = SystemTime::now() - Duration::from_secs(days * 86_400);
        let ft = filetime_set(dir, t);
        assert!(ft, "could not backdate {}", dir.display());
    }

    /// Backdate the directory and its children with `touch -t`-equivalent std calls.
    fn filetime_set(dir: &Path, t: SystemTime) -> bool {
        let set = |p: &Path| {
            std::fs::File::open(p)
                .and_then(|f| f.set_modified(t))
                .is_ok()
        };
        let mut ok = true;
        let entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        for e in entries {
            if e.path().is_dir() {
                ok &= filetime_set(&e.path(), t);
            } else {
                ok &= set(&e.path());
            }
        }
        ok & set(dir)
    }

    #[test]
    fn finds_project_and_staging_targets_with_size_and_age() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_target(&root.join("target"), 4096);
        make_target(&root.join(".ta/staging/goal-a/target"), 8192);
        age(&root.join(".ta/staging/goal-a/target"), 30);
        // Not build output: a plain directory named target, and one under .git.
        std::fs::create_dir_all(root.join("docs/target")).unwrap();
        std::fs::write(root.join("docs/target/notes.txt"), "x").unwrap();
        make_target(&root.join(".git/target"), 1024);

        let found = find_target_dirs(root);
        let paths: Vec<_> = found.iter().map(|d| d.path.clone()).collect();
        assert_eq!(found.len(), 2, "{paths:?}");
        assert_eq!(
            found[0].path,
            root.join(".ta/staging/goal-a/target"),
            "largest first"
        );
        assert!(found[0].bytes >= 8192);
        assert!(found[0].age_days >= 30, "age {}", found[0].age_days);
        assert_eq!(found[1].age_days, 0);
    }

    #[test]
    fn removes_only_the_named_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_target(&root.join("target"), 100);
        make_target(&root.join(".ta/staging/old/target"), 100);
        let found = find_target_dirs(root);

        let named = [root.join(".ta/staging/old/target")];
        let removed = remove_named(&named, &found).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!root.join(".ta/staging/old/target").exists());
        assert!(
            root.join("target").exists(),
            "an unnamed target/ must survive"
        );
        assert!(
            root.join(".ta/staging/old").exists(),
            "only target/ goes, not its parent"
        );
    }

    #[test]
    fn refuses_a_path_that_is_not_a_found_target_and_removes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_target(&root.join("target"), 100);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "x").unwrap();
        let found = find_target_dirs(root);

        let named = [root.join("target"), root.join("src")];
        let err = remove_named(&named, &found).unwrap_err().to_string();
        assert!(err.contains("not a Cargo target/ directory"), "{err}");
        assert!(err.contains("nothing else was"), "{err}");
        assert!(
            root.join("target").exists(),
            "validated in full before deleting"
        );
        assert!(root.join("src/lib.rs").exists());

        let missing = [root.join("nope")];
        assert!(remove_named(&missing, &found)
            .unwrap_err()
            .to_string()
            .contains("Nothing was removed"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_target_outside_the_workspace_is_neither_listed_nor_removable() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        make_target(&outside.path().join("target"), 100);
        let root = tmp.path();
        std::fs::create_dir_all(root.join("ws")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("target"), root.join("ws/target")).unwrap();
        let found = find_target_dirs(root);
        assert!(found.is_empty(), "{found:?}");
        let err = remove_named(&[root.join("ws/target")], &found);
        assert!(err.is_err());
        assert!(outside.path().join("target/debug/artifact").exists());
    }

    #[test]
    fn old_directories_get_an_exact_removal_command_and_young_ones_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_target(&root.join("target"), 100);
        make_target(&root.join(".ta/staging/stale/target"), 100);
        age(&root.join(".ta/staging/stale/target"), 20);
        let results = check(root);
        let r = results.iter().find(|r| r.name == "Build output").unwrap();
        assert_eq!(r.status, super::super::doctor::CheckStatus::Warn);
        let stale = root.join(".ta/staging/stale/target");
        assert!(
            r.fix
                .contains(&format!("ta doctor --remove-target {}", stale.display())),
            "{}",
            r.fix
        );
        assert!(!r.fix.contains(&format!(
            "--remove-target {}\n",
            root.join("target").display()
        )));
        assert!(r.detail.contains("2 target/ director(ies)"), "{}", r.detail);
    }

    #[test]
    fn snapshot_warning_counts_local_snapshots_and_names_the_fix() {
        let out = "Snapshots for volume group containing disk /:\n\
                   com.apple.TimeMachine.2026-10-15-120000.local\n\
                   com.apple.TimeMachine.2026-10-16-020000.local\n";
        let w = snapshot_warning(out).unwrap();
        assert!(w.starts_with("2 local APFS snapshot(s)"), "{w}");
        assert!(w.contains("tmutil thinlocalsnapshots"), "{w}");
        assert_eq!(
            snapshot_warning("Snapshots for volume group containing disk /:\n"),
            None
        );
    }
}
