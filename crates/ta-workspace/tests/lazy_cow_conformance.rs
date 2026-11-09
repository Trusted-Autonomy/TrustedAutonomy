// Conformance tests for lazy copy-on-write staging (`OverlayStagingMode::Lazy`).
//
// Every test drives the public `OverlayWorkspace` API the way `ta goal start`,
// `ta draft build` and `ta draft apply` do: create, let the "agent" write in the
// staging directory, diff, apply. The same assertions hold whichever mechanism
// the platform provides (APFS directory clones on macOS, per-file reflinks on
// Btrfs/XFS, the full-copy fallback on ext4/NTFS/other volumes), so the suite
// runs unchanged on macOS, Linux and Windows. Assertions that only make sense
// when cloning is in use are gated on the report's `cloned` flag; symlink and
// permission-bit tests are unix-only.

use std::fs;
use std::path::{Path, PathBuf};

use ta_workspace::lazy_cow::{baseline_path, creating_marker_path};
use ta_workspace::overlay::OverlayChange;
use ta_workspace::{
    ConflictResolution, ExcludePatterns, OverlayStagingMode, OverlayWorkspace, WorkspaceError,
};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    source: PathBuf,
    staging_root: PathBuf,
}

impl Fixture {
    /// A project laid out like a real one: staging lives under `<source>/.ta/staging`.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("project");
        let staging_root = source.join(".ta").join("staging");
        write(&source, "README.md", "# demo\n");
        write(&source, "src/main.rs", "fn main() {}\n");
        write(&source, "src/lib.rs", "pub fn lib() {}\n");
        write(&source, "src/deep/nested/mod.rs", "pub mod nested;\n");
        write(&source, "docs/a.txt", "alpha\n");
        write(&source, "docs/b.txt", "bravo\n");
        write(&source, "scripts/run.sh", "#!/bin/sh\necho hi\n");
        write(&source, "target/debug/junk.bin", "build output");
        write(&source, "node_modules/pkg/index.js", "module.exports = 1;");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                source.join("scripts/run.sh"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        fs::create_dir_all(&staging_root).unwrap();
        Self {
            _tmp: tmp,
            source,
            staging_root,
        }
    }

    fn create(&self, goal: &str) -> OverlayWorkspace {
        OverlayWorkspace::create_with_strategy(
            goal,
            &self.source,
            &self.staging_root,
            ExcludePatterns::defaults(),
            OverlayStagingMode::Lazy,
        )
        .unwrap()
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn summarize(changes: &[OverlayChange]) -> Vec<String> {
    let mut out: Vec<String> = changes
        .iter()
        .map(|c| match c {
            OverlayChange::Modified { path, .. } => format!("M {path}"),
            OverlayChange::Created { path, .. } => format!("C {path}"),
            OverlayChange::Deleted { path } => format!("D {path}"),
        })
        .collect();
    out.sort();
    out
}

fn uris(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .map(|p| format!("fs://workspace/{p}"))
        .collect()
}

// ── Creation and reporting ────────────────────────────────────────

#[test]
fn creation_reports_mechanism_and_skips_excluded_trees() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let stat = overlay.copy_stat().unwrap();
    let lazy = stat.lazy.as_ref().expect("lazy staging records a report");

    // The goal output names the mechanism, and the fallback reason when there is one.
    let report = stat.size_report();
    assert!(report.contains(&lazy.mechanism), "{report}");
    if lazy.cloned {
        assert!(lazy.fallback_reason.is_none());
    } else {
        let reason = lazy
            .fallback_reason
            .as_deref()
            .expect("fallback has a reason");
        assert!(report.contains(reason), "{report}");
        assert!(report.contains("full copy"), "{report}");
    }
    #[cfg(target_os = "macos")]
    assert!(
        lazy.cloned,
        "tempdir on macOS is APFS: expected clonefile, got {}",
        report
    );

    let staging = overlay.staging_dir();
    assert_eq!(
        fs::read_to_string(staging.join("src/deep/nested/mod.rs")).unwrap(),
        "pub mod nested;\n"
    );
    assert!(
        !staging.join("target").exists(),
        "target/ must not be staged"
    );
    assert!(
        !staging.join("node_modules").exists(),
        "node_modules/ must not be staged"
    );
    assert!(!staging.join(".ta").exists(), ".ta/ must not be staged");
    assert!(lazy.baseline_files >= 7, "baseline lists the staged files");
}

#[test]
fn a_goal_that_writes_nothing_has_an_empty_diff() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    assert!(overlay.diff_all().unwrap().is_empty());
    assert!(overlay.apply_to(&fx.source).unwrap().is_empty());
}

#[test]
fn source_edits_made_after_goal_start_are_not_shown_as_agent_changes() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    // Someone else edits and adds files in source while the agent works.
    write(&fx.source, "docs/a.txt", "alpha, edited elsewhere\n");
    write(&fx.source, "docs/new-in-source.txt", "added elsewhere\n");
    assert!(
        overlay.diff_all().unwrap().is_empty(),
        "unchanged staging files are not compared against a moving source"
    );
}

// ── Write-through: modification, creation, deletion, rename ───────

#[test]
fn modification_is_diffed_and_written_through_on_apply() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    write(
        overlay.staging_dir(),
        "src/main.rs",
        "fn main() { run(); }\n",
    );

    let changes = overlay.diff_all().unwrap();
    assert_eq!(summarize(&changes), ["M src/main.rs"]);
    match &changes[0] {
        OverlayChange::Modified { diff, .. } => assert!(diff.contains("run()"), "{diff}"),
        other => panic!("expected Modified, got {other:?}"),
    }

    overlay.apply_to(&fx.source).unwrap();
    assert_eq!(
        fs::read_to_string(fx.source.join("src/main.rs")).unwrap(),
        "fn main() { run(); }\n"
    );
    assert_eq!(
        fs::read_to_string(fx.source.join("src/lib.rs")).unwrap(),
        "pub fn lib() {}\n",
        "files the agent did not write are left alone"
    );
}

#[test]
fn a_same_size_rewrite_is_detected() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    // "alpha\n" -> "ALPHA\n": same length, new content.
    write(overlay.staging_dir(), "docs/a.txt", "ALPHA\n");
    assert_eq!(summarize(&overlay.diff_all().unwrap()), ["M docs/a.txt"]);
}

#[test]
fn rewriting_a_file_with_identical_content_is_not_a_change() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    write(overlay.staging_dir(), "docs/a.txt", "alpha\n");
    assert!(overlay.diff_all().unwrap().is_empty());
}

#[test]
fn created_files_and_directories_are_written_through() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    write(overlay.staging_dir(), "docs/new/c.txt", "charlie\n");
    write(overlay.staging_dir(), "top.txt", "top\n");

    assert_eq!(
        summarize(&overlay.diff_all().unwrap()),
        ["C docs/new/c.txt", "C top.txt"]
    );
    overlay.apply_to(&fx.source).unwrap();
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/new/c.txt")).unwrap(),
        "charlie\n"
    );
    assert_eq!(
        fs::read_to_string(fx.source.join("top.txt")).unwrap(),
        "top\n"
    );
}

#[test]
fn deletion_is_written_through_on_apply() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    fs::remove_file(overlay.staging_dir().join("docs/b.txt")).unwrap();

    assert_eq!(summarize(&overlay.diff_all().unwrap()), ["D docs/b.txt"]);
    assert!(
        fx.source.join("docs/b.txt").exists(),
        "source untouched until apply"
    );

    overlay.apply_to(&fx.source).unwrap();
    assert!(!fx.source.join("docs/b.txt").exists());
    assert!(fx.source.join("docs/a.txt").exists());
}

#[test]
fn deleting_a_whole_directory_deletes_every_file_in_it() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    fs::remove_dir_all(overlay.staging_dir().join("src")).unwrap();

    assert_eq!(
        summarize(&overlay.diff_all().unwrap()),
        ["D src/deep/nested/mod.rs", "D src/lib.rs", "D src/main.rs"]
    );
    overlay.apply_to(&fx.source).unwrap();
    assert!(!fx.source.join("src/lib.rs").exists());
    assert!(!fx.source.join("src/main.rs").exists());
    assert!(fx.source.join("docs/a.txt").exists());
}

#[test]
fn renaming_a_file_is_a_delete_plus_a_create() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    fs::rename(
        overlay.staging_dir().join("docs/a.txt"),
        overlay.staging_dir().join("docs/renamed.txt"),
    )
    .unwrap();

    assert_eq!(
        summarize(&overlay.diff_all().unwrap()),
        ["C docs/renamed.txt", "D docs/a.txt"]
    );
    overlay.apply_to(&fx.source).unwrap();
    assert!(!fx.source.join("docs/a.txt").exists());
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/renamed.txt")).unwrap(),
        "alpha\n"
    );
}

#[test]
fn renaming_a_directory_moves_every_file_in_it() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    fs::rename(
        overlay.staging_dir().join("src"),
        overlay.staging_dir().join("lib"),
    )
    .unwrap();

    assert_eq!(
        summarize(&overlay.diff_all().unwrap()),
        [
            "C lib/deep/nested/mod.rs",
            "C lib/lib.rs",
            "C lib/main.rs",
            "D src/deep/nested/mod.rs",
            "D src/lib.rs",
            "D src/main.rs",
        ]
    );
    overlay.apply_to(&fx.source).unwrap();
    assert!(!fx.source.join("src").join("main.rs").exists());
    assert_eq!(
        fs::read_to_string(fx.source.join("lib/main.rs")).unwrap(),
        "fn main() {}\n"
    );
}

#[test]
fn selective_apply_writes_through_only_the_approved_changes() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    write(overlay.staging_dir(), "docs/a.txt", "alpha v2\n");
    fs::remove_file(overlay.staging_dir().join("docs/b.txt")).unwrap();

    overlay
        .apply_selective(&fx.source, &uris(&["docs/b.txt"]))
        .unwrap();
    assert!(!fx.source.join("docs/b.txt").exists());
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/a.txt")).unwrap(),
        "alpha\n",
        "unapproved modification stays out of source"
    );
}

#[test]
fn staging_writes_never_reach_source_before_apply() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    // An agent killed halfway through a write leaves a truncated staging file.
    fs::write(overlay.staging_dir().join("src/main.rs"), "fn ma").unwrap();
    write(overlay.staging_dir(), "docs/a.txt", "changed\n");
    fs::remove_file(overlay.staging_dir().join("README.md")).unwrap();

    assert_eq!(
        fs::read_to_string(fx.source.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(fx.source.join("README.md").exists());
}

#[cfg(unix)]
#[test]
fn restoring_the_old_size_and_mtime_cannot_hide_a_write() {
    // An agent (or a tool such as `touch -r`) that puts the original mtime and
    // size back must still be caught: the inode change time cannot be set.
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let staged = overlay.staging_dir().join("docs/a.txt");
    let original_mtime = fs::metadata(&staged).unwrap().modified().unwrap();
    fs::write(&staged, "ALPHA\n").unwrap();
    let file = fs::OpenOptions::new().write(true).open(&staged).unwrap();
    file.set_modified(original_mtime).unwrap();
    drop(file);
    assert_eq!(summarize(&overlay.diff_all().unwrap()), ["M docs/a.txt"]);
}

// ── Baseline storage, reopening, cleanup ──────────────────────────

#[test]
fn the_baseline_lives_outside_the_staging_directory() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let baseline = baseline_path(overlay.staging_dir()).unwrap();
    assert!(
        baseline.is_file(),
        "baseline saved at {}",
        baseline.display()
    );
    assert!(
        !baseline.starts_with(overlay.staging_dir()),
        "an agent writing inside staging must not be able to edit the baseline"
    );
    assert!(
        creating_marker_path(overlay.staging_dir()).is_none_or(|m| !m.exists()),
        "the creation marker is removed once the baseline is saved"
    );
}

#[test]
fn a_reopened_workspace_diffs_with_the_saved_baseline() {
    let fx = Fixture::new();
    let staging;
    {
        let overlay = fx.create("g1");
        staging = overlay.staging_dir().to_path_buf();
    }
    write(&staging, "docs/a.txt", "after restart\n");
    fs::remove_file(staging.join("docs/b.txt")).unwrap();

    let reopened = OverlayWorkspace::open("g1", &fx.source, &staging, ExcludePatterns::defaults());
    assert_eq!(
        summarize(&reopened.diff_all().unwrap()),
        ["D docs/b.txt", "M docs/a.txt"]
    );
}

#[test]
fn an_unfinished_creation_is_refused_with_an_actionable_error() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let marker = creating_marker_path(overlay.staging_dir()).unwrap();
    fs::write(&marker, "interrupted").unwrap();

    let err = overlay.diff_all().expect_err("marker present");
    assert!(
        matches!(err, WorkspaceError::IncompleteStaging { .. }),
        "{err}"
    );
    let text = err.to_string();
    assert!(text.contains("start the goal again"), "{text}");
    assert!(text.contains(&marker.display().to_string()), "{text}");
}

#[test]
fn cleanup_removes_the_staging_directory_and_its_baseline() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let staging = overlay.staging_dir().to_path_buf();
    let baseline = baseline_path(&staging).unwrap();
    overlay.cleanup().unwrap();
    assert!(!staging.exists());
    assert!(!baseline.exists());
}

#[test]
fn baselines_of_deleted_staging_directories_are_swept_at_the_next_create() {
    let fx = Fixture::new();
    let first = fx.create("g1");
    let orphan = baseline_path(first.staging_dir()).unwrap();
    // `ta gc` removes staging directories without going through `cleanup`.
    fs::remove_dir_all(first.staging_dir()).unwrap();
    assert!(orphan.exists());

    let _second = fx.create("g2");
    assert!(!orphan.exists(), "orphaned baseline should be swept");
}

#[test]
fn full_mode_is_unchanged_and_writes_no_baseline() {
    let fx = Fixture::new();
    let overlay = OverlayWorkspace::create_with_strategy(
        "full-goal",
        &fx.source,
        &fx.staging_root,
        ExcludePatterns::defaults(),
        OverlayStagingMode::Full,
    )
    .unwrap();
    assert!(overlay.copy_stat().unwrap().lazy.is_none());
    assert!(!baseline_path(overlay.staging_dir()).unwrap().exists());
    write(overlay.staging_dir(), "docs/a.txt", "ALPHA\n");
    assert_eq!(summarize(&overlay.diff_all().unwrap()), ["M docs/a.txt"]);
}

// ── Conflict detection against a source that moved ────────────────

#[test]
fn source_changed_after_goal_start_still_conflicts_with_an_agent_edit() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    write(overlay.staging_dir(), "docs/a.txt", "agent version\n");
    // Different size, so the check does not depend on timestamp granularity.
    write(&fx.source, "docs/a.txt", "someone else edited this file\n");

    let refused = overlay.apply_with_conflict_check(
        &fx.source,
        ConflictResolution::Abort,
        &uris(&["docs/a.txt"]),
    );
    assert!(
        refused.is_err(),
        "Abort must refuse to overwrite: {refused:?}"
    );
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/a.txt")).unwrap(),
        "someone else edited this file\n"
    );

    overlay
        .apply_with_conflict_check(
            &fx.source,
            ConflictResolution::ForceOverwrite,
            &uris(&["docs/a.txt"]),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/a.txt")).unwrap(),
        "agent version\n"
    );
}

#[test]
fn an_agent_deletion_of_a_file_edited_in_source_is_a_conflict() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    fs::remove_file(overlay.staging_dir().join("docs/a.txt")).unwrap();
    write(&fx.source, "docs/a.txt", "someone else edited this file\n");

    let refused = overlay.apply_with_conflict_check(
        &fx.source,
        ConflictResolution::Abort,
        &uris(&["docs/a.txt"]),
    );
    assert!(
        refused.is_err(),
        "deleting a file that changed in source must not be auto-resolved: {refused:?}"
    );
    assert!(fx.source.join("docs/a.txt").exists());
}

#[test]
fn a_source_edit_to_a_file_the_agent_did_not_touch_does_not_block_apply() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    write(overlay.staging_dir(), "docs/b.txt", "bravo v2\n");
    write(
        &fx.source,
        "docs/a.txt",
        "edited elsewhere, different size\n",
    );

    overlay
        .apply_with_conflict_check(
            &fx.source,
            ConflictResolution::Abort,
            &uris(&["docs/b.txt"]),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/b.txt")).unwrap(),
        "bravo v2\n"
    );
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/a.txt")).unwrap(),
        "edited elsewhere, different size\n"
    );
}

// ── Unix: permissions and symlinks ────────────────────────────────

#[cfg(unix)]
#[test]
fn the_executable_bit_survives_staging() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let mode = fs::metadata(overlay.staging_dir().join("scripts/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn symlinks_that_stay_inside_the_project_are_kept_and_others_cannot_leak_writes() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let outside = fx._tmp.path().join("outside");
    fs::create_dir_all(outside.join("dir")).unwrap();
    fs::write(outside.join("secret.txt"), "outside file\n").unwrap();
    fs::write(outside.join("dir/inner.txt"), "outside dir file\n").unwrap();

    symlink("README.md", fx.source.join("readme-link")).unwrap(); // relative, in project
    symlink("../src/lib.rs", fx.source.join("docs/lib-link")).unwrap(); // relative, in project
    symlink(outside.join("secret.txt"), fx.source.join("abs-file-link")).unwrap();
    symlink(outside.join("dir"), fx.source.join("abs-dir-link")).unwrap();
    symlink("../outside/secret.txt", fx.source.join("escape-file-link")).unwrap();

    let overlay = fx.create("g1");
    let lazy = overlay.copy_stat().unwrap().lazy.clone().unwrap();
    let staging = overlay.staging_dir();

    if lazy.cloned {
        // In-project relative links are recreated verbatim.
        assert_eq!(
            fs::read_link(staging.join("readme-link")).unwrap(),
            Path::new("README.md")
        );
        assert_eq!(
            fs::read_link(staging.join("docs/lib-link")).unwrap(),
            Path::new("../src/lib.rs")
        );
        // A link to a file outside the project becomes a copy of that file.
        let abs = fs::symlink_metadata(staging.join("abs-file-link")).unwrap();
        assert!(abs.file_type().is_file(), "absolute file link is copied");
        assert_eq!(
            fs::read_to_string(staging.join("abs-file-link")).unwrap(),
            "outside file\n"
        );
        let esc = fs::symlink_metadata(staging.join("escape-file-link")).unwrap();
        assert!(esc.file_type().is_file(), "escaping file link is copied");
        // A link to a directory outside the project is left out.
        assert!(
            fs::symlink_metadata(staging.join("abs-dir-link")).is_err(),
            "absolute directory link must not be staged"
        );
        assert!(lazy.symlinks_preserved >= 2);
        assert!(lazy.symlinks_dereferenced >= 2);
        assert!(lazy.symlinks_skipped >= 1);

        // The write the staged copy receives never reaches the outside file.
        fs::write(staging.join("abs-file-link"), "agent wrote\n").unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "outside file\n"
        );
    }

    // Nothing the agent did not write shows up in the diff.
    let initial = summarize(&overlay.diff_all().unwrap());
    assert!(
        initial.iter().all(|c| c.ends_with("abs-file-link")),
        "{initial:?}"
    );
}

#[cfg(unix)]
#[test]
fn replacing_a_file_with_a_symlink_out_of_the_project_is_not_applied() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let outside = fx._tmp.path().join("outside.txt");
    fs::write(&outside, "private data\n").unwrap();

    let overlay = fx.create("g1");
    let staged = overlay.staging_dir().join("docs/a.txt");
    fs::remove_file(&staged).unwrap();
    symlink(&outside, &staged).unwrap();

    let changes = summarize(&overlay.diff_all().unwrap());
    assert!(
        !changes.iter().any(|c| c == "M docs/a.txt"),
        "an escaping symlink must not be read into a draft: {changes:?}"
    );
    overlay.apply_to(&fx.source).unwrap();
    assert_eq!(
        fs::read_to_string(fx.source.join("docs/a.txt")).unwrap(),
        "alpha\n",
        "source file must not be replaced by data from outside the project"
    );
}

#[cfg(unix)]
#[test]
fn replacing_a_file_with_an_in_project_symlink_is_a_normal_change() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let staged = overlay.staging_dir().join("docs/a.txt");
    fs::remove_file(&staged).unwrap();
    symlink("b.txt", &staged).unwrap();

    assert_eq!(summarize(&overlay.diff_all().unwrap()), ["M docs/a.txt"]);
}

#[cfg(unix)]
#[test]
fn sockets_and_fifos_in_the_project_do_not_break_staging() {
    let fx = Fixture::new();
    // How special files are handled is a property of the clone population code;
    // the full-copy fallback is the pre-existing code path and is not under test
    // here (and copying a FIFO with a plain file copy could block), so this test
    // applies to the clone mechanisms only.
    if !ta_workspace::lazy_cow::choose_mechanism(&fx.source, &fx.staging_root).is_clone() {
        eprintln!("clone mechanism unavailable on this filesystem: FIFO test not applicable");
        return;
    }
    let fifo = fx.source.join("docs/pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("the mkfifo command is available on unix");
    assert!(status.success(), "mkfifo failed: {status}");

    let overlay = fx.create("g1");
    assert!(!overlay.staging_dir().join("docs/pipe").exists());
    assert!(overlay.diff_all().unwrap().is_empty());
}

// ── Platform expectations ─────────────────────────────────────────

#[cfg(windows)]
#[test]
fn windows_uses_the_full_copy_fallback_and_says_so() {
    let fx = Fixture::new();
    let overlay = fx.create("g1");
    let stat = overlay.copy_stat().unwrap();
    let lazy = stat.lazy.as_ref().unwrap();
    assert!(!lazy.cloned);
    assert!(lazy.fallback_reason.is_some());
    assert!(stat.size_report().contains("full copy"));
}
