// lazy_cow.rs — Lazy copy-on-write staging (PLAN.md v0.17.12.32).
//
// Design: docs/design/lazy-cow-staging.md. Two independent halves:
//
// 1. Population. `choose_mechanism` probes whether the OS can clone the source
//    into the staging directory without copying data (APFS `clonefile(2)` of whole
//    directory trees on macOS, `FICLONE` per file on Linux). `populate` builds the
//    staging tree with those clones. Where no clone is possible the caller falls
//    back to the full copy.
//
// 2. Change tracking. `StagingBaseline` records a metadata fingerprint of every
//    staging file right after population, so the diff only opens files the agent
//    wrote, created or deleted. It works the same for a clone and for the full-copy
//    fallback.
//
// The baseline is stored outside the staging directory so an agent with write
// access to its staging tree cannot edit the record of what it started with.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;
use crate::overlay::ExcludePatterns;

/// Baseline file format version.
pub const BASELINE_VERSION: u32 = 1;

/// Timestamp tick waited after population on filesystems with sub-second
/// timestamps (ext4 jiffies are the coarsest common case, up to 10 ms).
const FINE_TICK: Duration = Duration::from_millis(25);
/// Tick on filesystems whose timestamps carry no sub-second part (HFS+, FAT).
const COARSE_TICK: Duration = Duration::from_secs(2);

/// Name of the directory (next to `staging/`) holding baselines.
pub const BASELINE_DIR_NAME: &str = "staging-baselines";

// ── Mechanism ─────────────────────────────────────────────────────

/// How a lazy staging tree gets populated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LazyMechanism {
    /// macOS APFS: `clonefile(2)` clones whole directory subtrees in one syscall.
    DirectoryClone,
    /// Linux Btrfs/XFS/bcachefs: `FICLONE` per file.
    FileReflink,
    /// No copy-on-write clone is available: use the full copy.
    FullCopy { reason: String },
}

impl LazyMechanism {
    /// True when population clones instead of copying.
    pub fn is_clone(&self) -> bool {
        !matches!(self, Self::FullCopy { .. })
    }

    /// Short human description for goal output and logs.
    pub fn description(&self) -> &'static str {
        match self {
            Self::DirectoryClone => "APFS clonefile (directory trees)",
            Self::FileReflink => "reflink (FICLONE, per file)",
            Self::FullCopy { .. } => "full copy",
        }
    }
}

/// What lazy staging did, for goal output, logs and measurements.
#[derive(Debug, Clone, Default)]
pub struct LazyReport {
    /// Description of the mechanism used (see [`LazyMechanism::description`]).
    pub mechanism: String,
    /// True when population cloned; false when it fell back to the full copy.
    pub cloned: bool,
    /// Why lazy population was not used (fallback only).
    pub fallback_reason: Option<String>,
    /// Whole-directory clones (one syscall each).
    pub dir_clones: usize,
    /// Directory subtrees whose clone failed and were redone entry by entry.
    pub dir_clone_fallbacks: usize,
    /// Files created by an individual clone.
    pub file_clones: usize,
    /// Files whose clone failed and were physically copied.
    pub files_copied: usize,
    /// Bytes physically copied (clone fallbacks and the full-copy fallback).
    pub bytes_copied: u64,
    /// Files covered by directory clones.
    pub files_in_dir_clones: usize,
    /// Relative in-root symlinks recreated verbatim.
    pub symlinks_preserved: usize,
    /// Escaping/absolute symlinks to files, replaced by a copy of the content.
    pub symlinks_dereferenced: usize,
    /// Escaping/absolute symlinks to directories (or broken ones), left out.
    pub symlinks_skipped: usize,
    /// Sockets, FIFOs and devices left out.
    pub specials_skipped: usize,
    /// Files recorded in the baseline.
    pub baseline_files: usize,
    /// Logical size of the baselined files in bytes.
    pub baseline_bytes: u64,
    /// Time spent taking the baseline (excludes the timestamp-tick wait).
    pub baseline_duration: Duration,
    /// Time waited for a timestamp tick before the baseline.
    pub tick_wait: Duration,
}

impl LazyReport {
    pub(crate) fn new(mechanism: &LazyMechanism) -> Self {
        Self {
            mechanism: mechanism.description().to_string(),
            cloned: mechanism.is_clone(),
            fallback_reason: match mechanism {
                LazyMechanism::FullCopy { reason } => Some(reason.clone()),
                _ => None,
            },
            ..Default::default()
        }
    }

    /// One-paragraph report printed by `ta goal start` / `ta run`.
    pub fn summary(&self, total: Duration) -> String {
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        if self.cloned {
            format!(
                "Staging: lazy copy-on-write via {} in {:.2}s: {} directory clone(s) covering {} \
                 files, {} file clone(s), {} file(s) / {:.1} MB physically copied, {} symlink(s) \
                 kept, {} skipped. Baseline of {} files ({:.1} MB logical); the draft will \
                 only read files the agent writes",
                self.mechanism,
                total.as_secs_f64(),
                self.dir_clones,
                self.files_in_dir_clones,
                self.file_clones,
                self.files_copied,
                mb(self.bytes_copied),
                self.symlinks_preserved,
                self.symlinks_skipped,
                self.baseline_files,
                mb(self.baseline_bytes),
            )
        } else {
            format!(
                "Staging: lazy copy-on-write was requested but is unavailable here ({}); used the \
                 full copy: {} files ({:.1} MB) copied in {:.2}s. Change tracking is still lazy \
                 (baseline of {} files; the draft only reads files the agent writes). Set \
                 [staging] strategy = \"full\" in .ta/workflow.toml to silence this notice",
                self.fallback_reason
                    .as_deref()
                    .unwrap_or("no reason recorded"),
                self.files_copied,
                mb(self.bytes_copied),
                total.as_secs_f64(),
                self.baseline_files,
            )
        }
    }
}

/// Probe whether `source` can be cloned into `staging` without copying data.
///
/// Clones a real source file into the staging directory, so a cross-volume
/// layout (source and `.ta/staging` on different volumes) is detected here and
/// not file by file during population.
pub fn choose_mechanism(source: &Path, staging: &Path) -> LazyMechanism {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let Some(sample) = find_sample_file(source) else {
            return LazyMechanism::FullCopy {
                reason: "the source project has no regular file to probe clone support with"
                    .to_string(),
            };
        };
        let probe = staging.join(format!(".ta-lazy-probe-{}", std::process::id()));
        let result = crate::copy_strategy::clone_path(&sample, &probe);
        let _ = fs::remove_file(&probe);
        match result {
            Ok(()) if cfg!(target_os = "macos") => LazyMechanism::DirectoryClone,
            Ok(()) => LazyMechanism::FileReflink,
            Err(e) => LazyMechanism::FullCopy {
                reason: format!(
                    "cloning '{}' into '{}' failed ({e}); the source and .ta/staging must be on \
                     one APFS volume (macOS) or one reflink-capable filesystem such as Btrfs or \
                     XFS (Linux)",
                    sample.display(),
                    staging.display()
                ),
            },
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (source, staging);
        LazyMechanism::FullCopy {
            reason: "this platform has no copy-on-write clone primitive TA can use (on Windows \
                     use strategy = \"projfs\" for a zero-copy workspace)"
                .to_string(),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn find_sample_file(source: &Path) -> Option<PathBuf> {
    let mut queue = std::collections::VecDeque::from([source.to_path_buf()]);
    let mut visited = 0usize;
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 500 {
                return None;
            }
            let name = entry.file_name();
            if crate::path_safety::is_agent_infra_component(&name.to_string_lossy()) {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_file() {
                return Some(entry.path());
            }
            if ft.is_dir() {
                queue.push_back(entry.path());
            }
        }
    }
    None
}

// ── Population ────────────────────────────────────────────────────

/// Populate `staging` from `source` with clones (unix, `mechanism` must be a
/// clone mechanism). Entries dropped by `excludes` are not created.
#[cfg(unix)]
pub(crate) fn populate(
    source: &Path,
    staging: &Path,
    excludes: &ExcludePatterns,
    mechanism: &LazyMechanism,
    report: &mut LazyReport,
) -> Result<(), WorkspaceError> {
    debug_assert!(mechanism.is_clone());
    let plan = plan_dir(source, excludes).map_err(|source_err| WorkspaceError::IoError {
        path: source.to_path_buf(),
        source: source_err,
    })?;
    let mut ctx = PopulateCtx {
        source_root: source.to_path_buf(),
        dir_clone: matches!(mechanism, LazyMechanism::DirectoryClone),
        report,
        warned_clone_failure: false,
    };
    emit_dir(source, staging, &plan, &mut ctx)
}

#[cfg(not(unix))]
pub(crate) fn populate(
    _source: &Path,
    _staging: &Path,
    _excludes: &ExcludePatterns,
    _mechanism: &LazyMechanism,
    _report: &mut LazyReport,
) -> Result<(), WorkspaceError> {
    unreachable!("choose_mechanism never returns a clone mechanism on this platform")
}

#[cfg(unix)]
struct DirPlan {
    entries: Vec<PlanEntry>,
    /// No excluded entry, symlink or special file anywhere below.
    pure: bool,
    /// Regular files in this subtree.
    files: usize,
}

#[cfg(unix)]
enum PlanEntry {
    File(std::ffi::OsString),
    Dir(std::ffi::OsString, DirPlan),
    Symlink(std::ffi::OsString),
    Special,
}

/// Scan `dir`: directory entries only, no per-file `stat`.
#[cfg(unix)]
fn plan_dir(dir: &Path, excludes: &ExcludePatterns) -> io::Result<DirPlan> {
    let mut plan = DirPlan {
        entries: Vec::new(),
        pure: true,
        files: 0,
    };
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if excludes.should_exclude(&name.to_string_lossy()) {
            plan.pure = false;
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_dir() {
            let child = plan_dir(&entry.path(), excludes)?;
            plan.pure &= child.pure;
            plan.files += child.files;
            plan.entries.push(PlanEntry::Dir(name, child));
        } else if ft.is_file() {
            plan.files += 1;
            plan.entries.push(PlanEntry::File(name));
        } else if ft.is_symlink() {
            plan.pure = false;
            plan.entries.push(PlanEntry::Symlink(name));
        } else {
            plan.pure = false;
            plan.entries.push(PlanEntry::Special);
        }
    }
    Ok(plan)
}

#[cfg(unix)]
struct PopulateCtx<'a> {
    source_root: PathBuf,
    dir_clone: bool,
    report: &'a mut LazyReport,
    warned_clone_failure: bool,
}

fn io_err(path: &Path, source: io::Error) -> WorkspaceError {
    WorkspaceError::IoError {
        path: path.to_path_buf(),
        source,
    }
}

/// Fill the existing directory `dst` from `src` according to `plan`.
#[cfg(unix)]
fn emit_dir(
    src: &Path,
    dst: &Path,
    plan: &DirPlan,
    ctx: &mut PopulateCtx<'_>,
) -> Result<(), WorkspaceError> {
    for entry in &plan.entries {
        match entry {
            PlanEntry::File(name) => clone_or_copy_file(&src.join(name), &dst.join(name), ctx)?,
            PlanEntry::Dir(name, child) => {
                let child_src = src.join(name);
                let child_dst = dst.join(name);
                if child.pure && ctx.dir_clone {
                    match crate::copy_strategy::clone_path(&child_src, &child_dst) {
                        Ok(()) => {
                            ctx.report.dir_clones += 1;
                            ctx.report.files_in_dir_clones += child.files;
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!(
                                source = %child_src.display(),
                                error = %e,
                                "directory clone failed; copying this subtree entry by entry"
                            );
                            ctx.report.dir_clone_fallbacks += 1;
                            match fs::remove_dir_all(&child_dst) {
                                Ok(()) => {}
                                Err(r) if r.kind() == io::ErrorKind::NotFound => {}
                                Err(r) => return Err(io_err(&child_dst, r)),
                            }
                        }
                    }
                }
                fs::create_dir(&child_dst).map_err(|e| io_err(&child_dst, e))?;
                emit_dir(&child_src, &child_dst, child, ctx)?;
                // Permissions last: a read-only source directory must not stop
                // us filling it.
                if let Ok(md) = fs::metadata(&child_src) {
                    fs::set_permissions(&child_dst, md.permissions())
                        .map_err(|e| io_err(&child_dst, e))?;
                }
            }
            PlanEntry::Symlink(name) => handle_symlink(&src.join(name), &dst.join(name), ctx)?,
            PlanEntry::Special => ctx.report.specials_skipped += 1,
        }
    }
    Ok(())
}

#[cfg(unix)]
fn clone_or_copy_file(
    src: &Path,
    dst: &Path,
    ctx: &mut PopulateCtx<'_>,
) -> Result<(), WorkspaceError> {
    match crate::copy_strategy::clone_path(src, dst) {
        Ok(()) => {
            ctx.report.file_clones += 1;
            Ok(())
        }
        Err(e) => {
            if !ctx.warned_clone_failure {
                ctx.warned_clone_failure = true;
                tracing::warn!(
                    source = %src.display(),
                    error = %e,
                    "file clone failed; physically copying (further failures not logged)"
                );
            }
            // A failed clone must not leave a stray destination behind.
            let _ = fs::remove_file(dst);
            let n = fs::copy(src, dst).map_err(|e| io_err(dst, e))?;
            ctx.report.files_copied += 1;
            ctx.report.bytes_copied += n;
            Ok(())
        }
    }
}

/// Symlink policy of the design note: keep relative links that stay inside the
/// project, copy the content of files behind other links, skip the rest.
#[cfg(unix)]
fn handle_symlink(src: &Path, dst: &Path, ctx: &mut PopulateCtx<'_>) -> Result<(), WorkspaceError> {
    let target = fs::read_link(src).map_err(|e| io_err(src, e))?;
    let parent = src.parent().unwrap_or(&ctx.source_root);
    if link_stays_inside(&target, parent, &ctx.source_root) {
        std::os::unix::fs::symlink(&target, dst).map_err(|e| io_err(dst, e))?;
        ctx.report.symlinks_preserved += 1;
        return Ok(());
    }
    match fs::metadata(src) {
        Ok(md) if md.is_file() => {
            let n = fs::copy(src, dst).map_err(|e| io_err(dst, e))?;
            ctx.report.symlinks_dereferenced += 1;
            ctx.report.files_copied += 1;
            ctx.report.bytes_copied += n;
        }
        _ => {
            tracing::warn!(
                link = %src.display(),
                target = %target.display(),
                "symlink points outside the project (or is broken) and is not a file; left out \
                 of the staging copy so a write through it cannot reach the real tree. Move the \
                 target inside the project or add the link to .taignore to silence this"
            );
            ctx.report.symlinks_skipped += 1;
        }
    }
    Ok(())
}

/// True when the relative symlink `target`, living in directory `link_dir`
/// (a real directory under `root`), resolves inside `root` without relying on
/// anything the filesystem could change underneath us.
///
/// Accepted shape: leading `..` components (at most the depth of `link_dir`)
/// followed only by normal components. A `..` after a normal component is
/// refused because the normal component might be a symlink whose physical
/// parent differs from its lexical one.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn link_stays_inside(target: &Path, link_dir: &Path, root: &Path) -> bool {
    use std::path::Component;
    if target.is_absolute() {
        return false;
    }
    let Ok(rel) = link_dir.strip_prefix(root) else {
        return false;
    };
    let mut depth = rel.components().count();
    let mut seen_normal = false;
    for c in target.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if seen_normal || depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::Normal(_) => seen_normal = true,
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

// ── Baseline ──────────────────────────────────────────────────────

/// What kind of entry a fingerprint describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    #[serde(rename = "f")]
    File,
    #[serde(rename = "l")]
    Symlink,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Metadata fingerprint of one staging file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    #[serde(rename = "k")]
    pub kind: EntryKind,
    #[serde(rename = "s")]
    pub size: u64,
    /// Modification time, nanoseconds since the epoch.
    #[serde(rename = "m")]
    pub mtime_ns: i64,
    /// Inode change time (unix; 0 elsewhere). Cannot be set from user space.
    #[serde(rename = "c")]
    pub ctime_ns: i64,
    #[serde(rename = "i")]
    pub ino: u64,
    #[serde(rename = "p")]
    pub mode: u32,
    /// Symlink target.
    #[serde(rename = "l", default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Timestamp within one tick of the capture time: never trusted.
    #[serde(rename = "r", default, skip_serializing_if = "is_false")]
    pub racy: bool,
}

impl Fingerprint {
    fn from_metadata(md: &fs::Metadata, link: Option<String>) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                kind: if link.is_some() {
                    EntryKind::Symlink
                } else {
                    EntryKind::File
                },
                size: md.size(),
                mtime_ns: md.mtime() * 1_000_000_000 + md.mtime_nsec(),
                ctime_ns: md.ctime() * 1_000_000_000 + md.ctime_nsec(),
                ino: md.ino(),
                mode: md.mode(),
                link,
                racy: false,
            }
        }
        #[cfg(not(unix))]
        {
            let mtime_ns = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            Self {
                kind: if link.is_some() {
                    EntryKind::Symlink
                } else {
                    EntryKind::File
                },
                size: md.len(),
                mtime_ns,
                ctime_ns: 0,
                ino: 0,
                mode: 0,
                link,
                racy: false,
            }
        }
    }

    /// Equality of the recorded facts (the racy flag is bookkeeping).
    fn same_as(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.size == other.size
            && self.mtime_ns == other.mtime_ns
            && self.ctime_ns == other.ctime_ns
            && self.ino == other.ino
            && self.mode == other.mode
            && self.link == other.link
    }

    fn newest_ns(&self) -> i64 {
        self.mtime_ns.max(self.ctime_ns)
    }
}

/// Metadata baseline of a staging tree taken right after population.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagingBaseline {
    pub version: u32,
    /// Mechanism description, for diagnostics.
    pub mechanism: String,
    /// When the baseline was captured (ns since the epoch).
    pub captured_ns: i64,
    /// Whether fingerprints may be used to skip content comparison.
    /// False where `std` offers no change time (Windows).
    pub trust_stat: bool,
    /// Staging-relative path (forward slashes) to fingerprint.
    pub files: BTreeMap<String, Fingerprint>,
}

/// Directory holding the baselines for the staging root `staging_root`
/// (`.ta/staging` maps to `.ta/staging-baselines`).
fn baseline_dir_for_root(staging_root: &Path) -> PathBuf {
    staging_root
        .parent()
        .unwrap_or(staging_root)
        .join(BASELINE_DIR_NAME)
}

/// Directory holding the baseline of `staging_dir` (`.ta/staging/<id>` maps to
/// `.ta/staging-baselines/`).
pub fn baseline_dir(staging_dir: &Path) -> Option<PathBuf> {
    Some(baseline_dir_for_root(staging_dir.parent()?))
}

fn staging_name(staging_dir: &Path) -> Option<String> {
    staging_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Path of the baseline file of `staging_dir`.
pub fn baseline_path(staging_dir: &Path) -> Option<PathBuf> {
    Some(baseline_dir(staging_dir)?.join(format!("{}.json", staging_name(staging_dir)?)))
}

/// Path of the marker present while a lazy staging is being populated.
pub fn creating_marker_path(staging_dir: &Path) -> Option<PathBuf> {
    Some(baseline_dir(staging_dir)?.join(format!("{}.creating", staging_name(staging_dir)?)))
}

/// Remove the baseline and creation marker of `staging_dir` (best effort).
pub fn remove_baseline_files(staging_dir: &Path) {
    for path in [
        baseline_path(staging_dir),
        creating_marker_path(staging_dir),
    ]
    .into_iter()
    .flatten()
    {
        if let Err(e) = fs::remove_file(&path) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), error = %e, "could not remove staging baseline file");
            }
        }
    }
}

/// Delete baselines (and stale creation markers) whose staging directory no
/// longer exists. Returns the number removed.
pub fn sweep_orphan_baselines(staging_root: &Path) -> usize {
    let dir = baseline_dir_for_root(staging_root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if !staging_root.join(&stem).exists() && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Walk `root` collecting a fingerprint per non-excluded file and symlink.
/// Symlinked directories are leaves (never followed), so cycles cannot hang.
pub fn scan_tree(
    root: &Path,
    excludes: &ExcludePatterns,
) -> io::Result<BTreeMap<String, Fingerprint>> {
    let mut out = BTreeMap::new();
    scan_into(root, root, excludes, &mut out)?;
    Ok(out)
}

fn scan_into(
    root: &Path,
    dir: &Path,
    excludes: &ExcludePatterns,
    out: &mut BTreeMap<String, Fingerprint>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if excludes.should_exclude(&name.to_string_lossy()) {
            continue;
        }
        let ft = entry.file_type()?;
        let path = entry.path();
        let rel = || {
            path.strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/")
        };
        if ft.is_dir() {
            scan_into(root, &path, excludes, out)?;
        } else if ft.is_symlink() {
            let md = fs::symlink_metadata(&path)?;
            let target = fs::read_link(&path)?.to_string_lossy().into_owned();
            out.insert(rel(), Fingerprint::from_metadata(&md, Some(target)));
        } else if ft.is_file() {
            let md = entry.metadata()?;
            out.insert(rel(), Fingerprint::from_metadata(&md, None));
        }
    }
    Ok(())
}

/// Wait one timestamp tick so no file shares a timestamp tick with the capture.
/// Returns how long it slept.
fn wait_for_tick(staging_dir: &Path) -> Duration {
    let tick = timestamp_tick(staging_dir);
    std::thread::sleep(tick);
    tick
}

/// Probe the staging filesystem's timestamp resolution: a few fresh files whose
/// mtimes all have zero sub-second parts mean a one-second-resolution filesystem.
fn timestamp_tick(dir: &Path) -> Duration {
    let mut all_zero = true;
    for i in 0..3 {
        let probe = dir.join(format!(".ta-tick-probe-{}-{i}", std::process::id()));
        if fs::write(&probe, b"t").is_err() {
            // Cannot probe: be conservative.
            return COARSE_TICK;
        }
        let nanos = fs::metadata(&probe)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let _ = fs::remove_file(&probe);
        if nanos != 0 {
            all_zero = false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if all_zero {
        COARSE_TICK
    } else {
        FINE_TICK
    }
}

impl StagingBaseline {
    /// Capture the baseline of the freshly populated `staging_dir`.
    pub fn capture(
        staging_dir: &Path,
        excludes: &ExcludePatterns,
        mechanism: &str,
        report: &mut LazyReport,
    ) -> Result<Self, WorkspaceError> {
        let tick = wait_for_tick(staging_dir);
        report.tick_wait = tick;
        let started = std::time::Instant::now();
        let mut files = scan_tree(staging_dir, excludes).map_err(|e| io_err(staging_dir, e))?;
        let captured_ns = now_ns();
        let window = tick.as_nanos() as i64;
        let mut bytes = 0u64;
        for fp in files.values_mut() {
            bytes += fp.size;
            if captured_ns - fp.newest_ns() < window {
                fp.racy = true;
            }
        }
        report.baseline_files = files.len();
        report.baseline_bytes = bytes;
        report.baseline_duration = started.elapsed();
        Ok(Self {
            version: BASELINE_VERSION,
            mechanism: mechanism.to_string(),
            captured_ns,
            trust_stat: cfg!(unix),
            files,
        })
    }

    /// Write the baseline atomically (temp file in the same directory, rename).
    pub fn save(&self, path: &Path) -> Result<(), WorkspaceError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(self)?;
        fs::write(&tmp, bytes).map_err(|e| io_err(&tmp, e))?;
        fs::rename(&tmp, path).map_err(|e| io_err(path, e))
    }

    /// Load a baseline. `Ok(None)` when no baseline exists.
    pub fn load(path: &Path) -> Result<Option<Self>, WorkspaceError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(path, e)),
        };
        let baseline: Self = serde_json::from_slice(&bytes)?;
        if baseline.version != BASELINE_VERSION {
            tracing::warn!(
                path = %path.display(),
                found = baseline.version,
                expected = BASELINE_VERSION,
                "staging baseline has an unknown version; ignoring it and comparing by content"
            );
            return Ok(None);
        }
        Ok(Some(baseline))
    }

    /// True when `current` proves the file is exactly as it was at baseline time.
    pub fn is_unchanged(&self, rel: &str, current: &Fingerprint) -> bool {
        if !self.trust_stat {
            return false;
        }
        match self.files.get(rel) {
            Some(base) => !base.racy && base.same_as(current),
            None => false,
        }
    }

    /// Fingerprint check against the file on disk: `Some(true)` unchanged,
    /// `Some(false)` written or deleted, `None` when it cannot be decided
    /// (path not in the baseline or fingerprints untrusted).
    pub fn file_unchanged(&self, staging_dir: &Path, rel: &str) -> Option<bool> {
        if !self.trust_stat || !self.files.contains_key(rel) {
            return None;
        }
        let abs = staging_dir.join(rel);
        let Ok(md) = fs::symlink_metadata(&abs) else {
            return Some(false);
        };
        let link = if md.file_type().is_symlink() {
            fs::read_link(&abs)
                .ok()
                .map(|t| t.to_string_lossy().into_owned())
        } else {
            None
        };
        let current = Fingerprint::from_metadata(&md, link);
        Some(self.is_unchanged(rel, &current))
    }
}

/// Marker guard: present from before population until the baseline is saved.
pub(crate) struct CreationMarker {
    path: Option<PathBuf>,
}

impl CreationMarker {
    /// Write the marker for `staging_dir`.
    pub(crate) fn begin(staging_dir: &Path) -> Result<Self, WorkspaceError> {
        let Some(path) = creating_marker_path(staging_dir) else {
            return Ok(Self { path: None });
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        // A stale baseline from a previous use of this directory name must not
        // outlive the staging it described.
        remove_baseline_files(staging_dir);
        fs::write(&path, b"lazy staging population in progress\n").map_err(|e| io_err(&path, e))?;
        Ok(Self { path: Some(path) })
    }

    /// Population and baseline succeeded: remove the marker.
    pub(crate) fn finish(self) {
        if let Some(path) = self.path {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_policy_accepts_only_safe_relative_shapes() {
        let root = Path::new("/p");
        let dir = Path::new("/p/a/b");
        assert!(link_stays_inside(Path::new("sibling.txt"), dir, root));
        assert!(link_stays_inside(Path::new("../x/y"), dir, root));
        assert!(link_stays_inside(Path::new("../../top"), dir, root));
        assert!(
            !link_stays_inside(Path::new("../../../out"), dir, root),
            "one level above the root"
        );
        assert!(!link_stays_inside(Path::new("/etc/passwd"), dir, root));
        assert!(
            !link_stays_inside(Path::new("x/../y"), dir, root),
            "`..` after a normal component may cross a symlinked directory"
        );
        assert!(link_stays_inside(Path::new("./x"), dir, root));
    }

    #[test]
    fn baseline_paths_sit_beside_the_staging_root() {
        let staging = Path::new("/proj/.ta/staging/goal-1");
        assert_eq!(
            baseline_path(staging).unwrap(),
            Path::new("/proj/.ta/staging-baselines/goal-1.json")
        );
        assert_eq!(
            creating_marker_path(staging).unwrap(),
            Path::new("/proj/.ta/staging-baselines/goal-1.creating")
        );
    }

    #[test]
    fn fingerprint_compare_ignores_the_racy_flag() {
        let md = fs::metadata(".").unwrap();
        let a = Fingerprint::from_metadata(&md, None);
        let mut b = a.clone();
        b.racy = true;
        assert!(a.same_as(&b));
    }
}
