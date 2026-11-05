// state_dir.rs: the ONE place that decides where TA's per-user state lives, plus
// the one-time migration from the old location.
//
// Every TA process on an account (ta, ta-daemon, the Wayfinder poller, an
// installer's `ta state relocate`) resolves the state directory here, with one
// precedence rule and no dependence on a variable the caller might have
// forgotten to export:
//
//   1. `TA_STATE_DIR` in the environment (absolute path)
//   2. the persisted per-user pointer file `<user dir>/state-dir`
//      (`$TA_USER_STATE_DIR`, else `~/.config/ta/human`): one absolute path,
//      mode 0600, written atomically by `ta state relocate`
//   3. inside a cargo test binary: a per-process temp dir
//   4. the OS default (macOS `~/Library/Application Support/TrustedAutonomy`,
//      Linux `$XDG_STATE_HOME/trusted-autonomy` (default
//      `~/.local/state/trusted-autonomy`), Windows `%LOCALAPPDATA%\TrustedAutonomy`)
//
// With no override and no home directory the resolver returns an error that
// names the fixes. It never falls back to the current directory.
//
// MIGRATION. `migrate` copies a state directory to a new location exactly once:
// copy into a sibling temp dir, verify (file count, kinds, sha256 of every
// file, modes), write `.migrated-from`, then atomically rename into place. The
// old directory is never deleted or altered, except for one `MIGRATED-TO`
// marker file. Project ids are random values stored in `<project>/.ta/project-id`
// (not derived from any path), and keychain items are keyed by project id, so
// both stay valid after the move.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::human;
use crate::state::{POSTURE_FILENAME, STATE_ROOT_ENV};

/// Name of the pointer file inside the per-user state directory.
pub const POINTER_FILENAME: &str = "state-dir";
/// Written into a migrated-to directory: where the data came from.
pub const MIGRATED_FROM_MARKER: &str = ".migrated-from";
/// Written into the old directory after a successful migration.
pub const MIGRATED_TO_MARKER: &str = "MIGRATED-TO";
/// `projects/<id>/project-path` is how a state dir records which project tree
/// owns it; read here only to find that project's daemon pid file.
const PROJECT_PATH_MARKER: &str = "project-path";
/// Test builds and `cargo run` export this (via `.cargo/config.toml`); it also
/// disables automatic migration so a test can never copy the developer's real
/// state. Kept as a literal because ta-credentials does not depend on ta-goal.
const NO_EXTERNAL_AGENTS_ENV: &str = "TA_TEST_NO_EXTERNAL_AGENTS";
const USER_DIR_ENV: &str = human::STATE_DIR_ENV;

// ── Errors ─────────────────────────────────────────────────────────────────

/// Resolution and pointer errors. Every message says what happened and what to do.
#[derive(Debug, thiserror::Error)]
pub enum StateDirError {
    #[error(
        "cannot determine the TA state directory: this process has no home directory \
         (HOME/USERPROFILE is unset or unusable) and no override. Fix one of: set {STATE_ROOT_ENV} \
         to an absolute directory in this process's environment; set HOME so the OS default \
         location can be computed; or, from a session that does have a home, run \
         `ta state relocate --to <absolute dir>` once (it stores the choice in \
         `<user dir>/{POINTER_FILENAME}`, where every ta process on the account finds it). \
         TA will not guess a directory relative to the current working directory."
    )]
    NoHome,
    #[error(
        "{STATE_ROOT_ENV} is set to {value:?}, which is not an absolute path. A relative state \
         directory would resolve differently in every working directory and split the vault, \
         tokens and JetStream data. Set {STATE_ROOT_ENV} to an absolute path, or unset it."
    )]
    EnvNotAbsolute { value: String },
    #[error(
        "the state directory pointer file {path} is unusable: {reason}. Run \
         `ta state relocate --to <absolute dir>` to rewrite it (add --no-migrate to keep the \
         data where it is), or delete the file to fall back to the OS default location."
    )]
    BadPointer { path: PathBuf, reason: String },
    #[error(
        "could not write the state directory pointer {path}: {source}. Check that the directory \
         is writable by your user, or set {USER_DIR_ENV} to a directory you own."
    )]
    PointerWrite {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "cannot store a state directory pointer: this process has no home directory and \
         {USER_DIR_ENV} is not set, so there is no per-user directory to hold it. Set {USER_DIR_ENV} \
         (or HOME) and re-run `ta state relocate`."
    )]
    NoUserDir,
    #[error(
        "refusing to touch the real per-user state directory from a test binary: set \
         {USER_DIR_ENV} to a temp directory first."
    )]
    TestWithoutUserDir,
}

// ── Locations ──────────────────────────────────────────────────────────────

/// Operating system families with distinct state-directory conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// Everything about the running host that the resolver needs, so every OS and
/// every environment is testable on every machine.
#[derive(Debug, Clone)]
pub struct Host {
    pub os: Os,
    pub home: Option<PathBuf>,
    /// True inside a cargo test binary (see `encryption::keychain_guard_active`).
    pub test_guard: bool,
    pub pid: u32,
    pub temp_dir: PathBuf,
}

impl Host {
    pub fn current() -> Host {
        Host {
            os: Os::current(),
            home: dirs::home_dir(),
            test_guard: crate::encryption::keychain_guard_active(),
            pid: std::process::id(),
            temp_dir: std::env::temp_dir(),
        }
    }
}

fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// The OS-default state root; `None` when it cannot be computed without a home
/// directory. This is also the "legacy" location a migration copies from.
pub fn os_default_root(
    os: Os,
    env: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    match os {
        Os::MacOs => home.map(|h| {
            h.join("Library")
                .join("Application Support")
                .join("TrustedAutonomy")
        }),
        Os::Linux => env("XDG_STATE_HOME")
            .filter(|v| !v.is_empty() && Path::new(v).is_absolute())
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local").join("state")))
            .map(|p| p.join("trusted-autonomy")),
        Os::Windows => env("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join("AppData").join("Local")))
            .map(|p| p.join("TrustedAutonomy")),
    }
}

/// Which rule decided the state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateSource {
    /// `TA_STATE_DIR`.
    Env,
    /// The persisted pointer file.
    Pointer { file: PathBuf },
    /// A cargo test binary with no override.
    TestTemp,
    /// The OS default: nothing overrides it.
    OsDefault,
}

impl StateSource {
    /// Short stable word for scripts and JSON.
    pub fn short(&self) -> &'static str {
        match self {
            StateSource::Env => "env",
            StateSource::Pointer { .. } => "pointer",
            StateSource::TestTemp => "test-temp",
            StateSource::OsDefault => "default",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            StateSource::Env => format!("the {STATE_ROOT_ENV} environment variable"),
            StateSource::Pointer { file } => {
                format!("the persisted pointer file {}", file.display())
            }
            StateSource::TestTemp => "a per-process temp dir (cargo test binary)".to_string(),
            StateSource::OsDefault => "the OS default location (no override)".to_string(),
        }
    }
}

/// The resolved state root and which rule chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStateDir {
    pub root: PathBuf,
    pub source: StateSource,
}

/// Resolve the state directory for this process.
pub fn resolve() -> Result<ResolvedStateDir, StateDirError> {
    resolve_with(&process_env, &Host::current())
}

/// The per-user pointer file's path, when a per-user directory exists.
pub fn pointer_file_with(env: &dyn Fn(&str) -> Option<String>, host: &Host) -> Option<PathBuf> {
    // A test binary only honours the pointer when the test chose its own user
    // dir, so a developer's real `~/.config/ta/human/state-dir` can never
    // redirect a test into real state.
    if host.test_guard && env(USER_DIR_ENV).filter(|v| !v.is_empty()).is_none() {
        return None;
    }
    human::user_state_dir_with(env, host.home.as_deref()).map(|d| d.join(POINTER_FILENAME))
}

/// The single resolver (precedence in the module comment). Pure apart from
/// reading the pointer file.
pub fn resolve_with(
    env: &dyn Fn(&str) -> Option<String>,
    host: &Host,
) -> Result<ResolvedStateDir, StateDirError> {
    if let Some(value) = env(STATE_ROOT_ENV).filter(|v| !v.is_empty()) {
        if !Path::new(&value).is_absolute() {
            return Err(StateDirError::EnvNotAbsolute { value });
        }
        return Ok(ResolvedStateDir {
            root: PathBuf::from(value),
            source: StateSource::Env,
        });
    }
    if let Some(file) = pointer_file_with(env, host) {
        if let Some(root) = read_pointer_file(&file)? {
            return Ok(ResolvedStateDir {
                root,
                source: StateSource::Pointer { file },
            });
        }
    }
    if host.test_guard {
        return Ok(ResolvedStateDir {
            root: host
                .temp_dir
                .join(format!("ta-test-state-{}", host.pid))
                .join("TrustedAutonomy"),
            source: StateSource::TestTemp,
        });
    }
    os_default_root(host.os, env, host.home.as_deref())
        .map(|root| ResolvedStateDir {
            root,
            source: StateSource::OsDefault,
        })
        .ok_or(StateDirError::NoHome)
}

// ── Pointer file ───────────────────────────────────────────────────────────

/// Read a pointer file: `Ok(None)` when absent, an error when present but not
/// a single absolute path (a half-written or hand-edited pointer must not
/// silently fall back to another directory).
pub fn read_pointer_file(file: &Path) -> Result<Option<PathBuf>, StateDirError> {
    let bad = |reason: String| StateDirError::BadPointer {
        path: file.to_path_buf(),
        reason,
    };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(bad(format!("could not be read ({e})"))),
    };
    let value = text.trim();
    if value.is_empty() {
        return Err(bad("it is empty".to_string()));
    }
    if value.contains('\n') {
        return Err(bad("it holds more than one line".to_string()));
    }
    if !Path::new(value).is_absolute() {
        return Err(bad(format!("{value:?} is not an absolute path")));
    }
    Ok(Some(PathBuf::from(value)))
}

/// Atomically write `bytes` to `path` with owner-only permissions: write a
/// sibling temp file, fsync, rename over the target, fsync the directory.
pub fn write_atomic_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent directory"))?;
    human::create_private_dirs(parent)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.tmp-{}-{}", std::process::id(), nanos()));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Atomically persist `target` (absolute) as the state directory for every
/// process of this user. Returns the pointer file's path.
pub fn write_pointer_with(
    env: &dyn Fn(&str) -> Option<String>,
    host: &Host,
    target: &Path,
) -> Result<PathBuf, StateDirError> {
    if host.test_guard && env(USER_DIR_ENV).filter(|v| !v.is_empty()).is_none() {
        return Err(StateDirError::TestWithoutUserDir);
    }
    let file = human::user_state_dir_with(env, host.home.as_deref())
        .map(|d| d.join(POINTER_FILENAME))
        .ok_or(StateDirError::NoUserDir)?;
    write_atomic_private(&file, format!("{}\n", target.display()).as_bytes()).map_err(
        |source| StateDirError::PointerWrite {
            path: file.clone(),
            source,
        },
    )?;
    Ok(file)
}

// ── State presence and sizes ───────────────────────────────────────────────

/// Whether `dir` holds TA state: at least one project directory, or a posture
/// file. Migration markers, temp dirs and unrelated files do not count.
pub fn has_state(dir: &Path) -> bool {
    let projects = dir.join("projects");
    let has_projects = std::fs::read_dir(&projects)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false);
    has_projects || dir.join(POSTURE_FILENAME).is_file()
}

/// Size and age facts about a state directory, for reports.
#[derive(Debug, Clone)]
pub struct DirInfo {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub projects: usize,
    pub newest_mtime: Option<DateTime<Utc>>,
}

impl DirInfo {
    pub fn describe(&self) -> String {
        format!(
            "{}: {} project(s), {} file(s), {}, last modified {}",
            self.path.display(),
            self.projects,
            self.files,
            human_size(self.bytes),
            self.newest_mtime
                .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Gather [`DirInfo`] (best effort: unreadable entries are skipped).
pub fn dir_info(path: &Path) -> DirInfo {
    let mut info = DirInfo {
        path: path.to_path_buf(),
        bytes: 0,
        files: 0,
        projects: std::fs::read_dir(path.join("projects"))
            .map(|it| it.flatten().count())
            .unwrap_or(0),
        newest_mtime: None,
    };
    fn walk(dir: &Path, info: &mut DirInfo) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let Ok(md) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if let Ok(m) = md.modified() {
                let t: DateTime<Utc> = m.into();
                if info.newest_mtime.is_none_or(|cur| t > cur) {
                    info.newest_mtime = Some(t);
                }
            }
            if md.is_dir() {
                walk(&e.path(), info);
            } else {
                info.files += 1;
                info.bytes += md.len();
            }
        }
    }
    walk(path, &mut info);
    info
}

/// Single-quote a path for a copy-pasteable shell command.
pub fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

// ── Path normalisation ─────────────────────────────────────────────────────

/// Absolute form of a possibly-nonexistent path: relative paths are resolved
/// against the current directory (an explicit CLI argument), and the deepest
/// existing ancestor is canonicalised so `/var` vs `/private/var` compare equal.
pub fn normalize_target(p: &Path) -> std::io::Result<PathBuf> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    };
    let mut existing = abs.as_path();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if existing.exists() {
            break;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                existing = parent;
            }
            _ => break,
        }
    }
    let mut out = std::fs::canonicalize(existing).unwrap_or_else(|_| existing.to_path_buf());
    for part in tail.iter().rev() {
        if *part == ".." {
            out.pop();
        } else if *part != "." {
            out.push(part);
        }
    }
    Ok(out)
}

// ── Running daemons ────────────────────────────────────────────────────────

/// A live process recorded in a pid file that belongs to a state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub pid: u32,
    pub pid_file: PathBuf,
    pub project: Option<PathBuf>,
}

fn parse_pid(text: &str) -> Option<u32> {
    for line in text.lines() {
        if let Some(v) = line.trim().strip_prefix("pid=") {
            return v.trim().parse().ok();
        }
    }
    text.trim().parse().ok()
}

/// Live daemons (or NATS servers) using the state directory `root`: pid files
/// directly in the root or in a project's state dir, and the
/// `<project>/.ta/daemon.pid` of every project the state dir records.
pub fn running_processes(root: &Path, is_alive: &dyn Fn(u32) -> bool) -> Vec<RunningProcess> {
    fn add_pid_files(
        out: &mut Vec<(PathBuf, Option<PathBuf>)>,
        dir: &Path,
        project: Option<PathBuf>,
    ) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "pid") && p.is_file() {
                    out.push((p, project.clone()));
                }
            }
        }
    }
    let mut candidates: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    add_pid_files(&mut candidates, root, None);
    if let Ok(rd) = std::fs::read_dir(root.join("projects")) {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let project = std::fs::read_to_string(d.join(PROJECT_PATH_MARKER))
                .ok()
                .map(|s| PathBuf::from(s.trim()));
            add_pid_files(&mut candidates, &d, project.clone());
            if let Some(p) = &project {
                let pf = p.join(".ta").join("daemon.pid");
                if pf.is_file() {
                    candidates.push((pf, project.clone()));
                }
            }
        }
    }
    let mut out: Vec<RunningProcess> = Vec::new();
    for (pid_file, project) in candidates {
        let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| parse_pid(&t))
        else {
            continue;
        };
        if is_alive(pid) && !out.iter().any(|r| r.pid_file == pid_file) {
            out.push(RunningProcess {
                pid,
                pid_file,
                project,
            });
        }
    }
    out
}

// ── Migration ──────────────────────────────────────────────────────────────

/// Collaborators of a migration that the caller supplies.
pub struct MigrateCtx<'a> {
    /// Whether a pid is a running process (`ta_lifecycle::process_is_alive`).
    pub is_alive: &'a dyn Fn(u32) -> bool,
    /// The tool version recorded in `.migrated-from`.
    pub tool_version: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error(
        "refusing to migrate the TA state in {from}: {} still running against it: {}. \
         Moving the files under a live daemon would split its vault, tokens and JetStream data. \
         Stop it (`ta daemon stop` in each project listed), then re-run. If a pid file is stale \
         (the process is gone), delete that file and re-run.",
        if procs.len() == 1 { "a process is" } else { "processes are" },
        procs.iter()
            .map(|p| format!(
                "pid {} (recorded in {}{})",
                p.pid,
                p.pid_file.display(),
                p.project.as_ref().map(|x| format!(", project {}", x.display())).unwrap_or_default()
            ))
            .collect::<Vec<_>>()
            .join("; ")
    )]
    DaemonRunning {
        from: PathBuf,
        procs: Vec<RunningProcess>,
    },
    #[error(
        "both state directories already hold TA state, so nothing was copied, merged or \
         overwritten.\n  old: {}\n  new: {}\nDecide which one is right: to keep the NEW directory \
         exactly as it is and ignore the old one, run `ta state relocate --to {} --no-migrate`; \
         to start over from the OLD state instead, move the new directory aside \
         (`mv {} {}.aside`) and re-run `ta state relocate --to {}`.",
        old.describe(),
        new.describe(),
        shell_quote(&new.path),
        shell_quote(&new.path),
        new.path.display(),
        shell_quote(&new.path)
    )]
    Conflict {
        old: Box<DirInfo>,
        new: Box<DirInfo>,
    },
    #[error(
        "the old state directory {} was already migrated to {recorded} (see {} inside it), \
         not to {requested}. Copying it again would fork the state. To use the earlier copy run \
         `ta state relocate --to {recorded}`; to deliberately migrate again, remove that marker \
         file first.",
        from.display(), MIGRATED_TO_MARKER
    )]
    MigratedElsewhere {
        from: PathBuf,
        recorded: String,
        requested: String,
    },
    #[error(
        "the target {} exists and is not an empty directory (and holds no TA state), so TA \
         cannot atomically move the migrated copy into place. Choose a directory that does \
         not exist yet (or is empty), e.g. `ta state relocate --to {}/ta-state`.",
        to.display(), to.display()
    )]
    TargetNotEmpty { to: PathBuf },
    #[error(
        "the state directories overlap ({} and {}): a migration between a directory and one \
         inside it would copy into itself. Choose a target outside the old directory.",
        a.display(), b.display()
    )]
    Nested { a: PathBuf, b: PathBuf },
    #[error(
        "could not {what} {}: {source}. Nothing at the old location was changed and no \
         half-finished copy was left in place. Check permissions and free disk space, then re-run.",
        path.display()
    )]
    Io {
        what: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the copy of {from} did not verify ({reason}). The temporary copy was discarded and the \
         old directory is untouched. Make sure nothing is writing to the old directory, then re-run."
    )]
    Verify { from: PathBuf, reason: String },
}

fn io_err(what: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> MigrationError {
    let path = path.to_path_buf();
    move |source| MigrationError::Io { what, path, source }
}

/// What a migration did.
#[derive(Debug, Clone)]
pub enum MigrationOutcome {
    /// Nothing to copy (and why): no state at the old location, or same directory.
    NothingToMigrate {
        reason: String,
    },
    /// A previous run already completed this exact migration: a no-op.
    AlreadyMigrated {
        from: PathBuf,
        to: PathBuf,
        stale_temps: Vec<StaleTemp>,
    },
    Migrated(MigrationReport),
}

#[derive(Debug, Clone)]
pub struct MigrationReport {
    pub from: PathBuf,
    pub to: PathBuf,
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
    /// Files whose mode was tightened because the old one was looser than owner-only.
    pub tightened: Vec<PathBuf>,
    /// Entries that are not copied: sockets and fifos everywhere, and symlinks off unix
    /// (`copy_into` can only recreate them on unix, so listing them would fail verification).
    pub skipped: Vec<PathBuf>,
    pub old_info: DirInfo,
    /// Set when the `MIGRATED-TO` marker could not be written in the old directory.
    pub old_marker_error: Option<String>,
    pub stale_temps: Vec<StaleTemp>,
}

/// A leftover temp directory from an interrupted copy. Never used as state.
#[derive(Debug, Clone)]
pub struct StaleTemp {
    pub path: PathBuf,
    pub bytes: u64,
    pub pid: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MigratedFrom {
    from: String,
    migrated_at: String,
    ta_version: String,
    files: u64,
    bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct MigratedTo {
    to: String,
    migrated_at: String,
    ta_version: String,
    note: String,
}

fn temp_prefix(to: &Path) -> String {
    format!(
        ".{}.migrating-",
        to.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ta-state".to_string())
    )
}

/// Temp directories beside `to` left by an interrupted migration.
pub fn stale_temp_dirs(to: &Path) -> Vec<StaleTemp> {
    let Some(parent) = to.parent() else {
        return Vec::new();
    };
    let prefix = temp_prefix(to);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(parent) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix(&prefix) {
                let pid = rest.split('-').next().and_then(|p| p.parse().ok());
                out.push(StaleTemp {
                    bytes: dir_info(&e.path()).bytes,
                    path: e.path(),
                    pid,
                });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Dir,
    File { len: u64, sha: String, mode: u32 },
    Link(PathBuf),
}

#[derive(Debug, Default)]
struct Scan {
    manifest: BTreeMap<PathBuf, Node>,
    skipped: Vec<PathBuf>,
}

#[cfg(unix)]
fn mode_of(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o7777
}
#[cfg(not(unix))]
fn mode_of(_md: &std::fs::Metadata) -> u32 {
    0
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Top-level names in an OLD state dir that describe migration itself, not state.
const MARKERS: [&str; 2] = [MIGRATED_FROM_MARKER, MIGRATED_TO_MARKER];

fn scan(root: &Path, skip_markers: bool) -> std::io::Result<Scan> {
    fn walk(root: &Path, dir: &Path, top: bool, skip: bool, out: &mut Scan) -> std::io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if top && skip && MARKERS.iter().any(|m| e.file_name() == *m) {
                continue;
            }
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .expect("child of root")
                .to_path_buf();
            let md = std::fs::symlink_metadata(&path)?;
            let ft = md.file_type();
            if ft.is_dir() {
                out.manifest.insert(rel, Node::Dir);
                walk(root, &path, false, skip, out)?;
            } else if ft.is_symlink() && cfg!(unix) {
                out.manifest
                    .insert(rel, Node::Link(std::fs::read_link(&path)?));
            } else if ft.is_file() {
                out.manifest.insert(
                    rel,
                    Node::File {
                        len: md.len(),
                        sha: sha256_file(&path)?,
                        mode: mode_of(&md),
                    },
                );
            } else {
                out.skipped.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Scan::default();
    walk(root, root, true, skip_markers, &mut out)?;
    Ok(out)
}

/// Whether a file in the state dir is secret material that must end up 0600.
fn is_secret_path(rel: &Path) -> bool {
    let comp_is = |c: &std::path::Component, n: &str| c.as_os_str() == n;
    if rel
        .components()
        .any(|c| comp_is(&c, "secrets") || comp_is(&c, "tokens"))
    {
        return true;
    }
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name == "daemon-tokens.json"
        || name == "vault-key"
        || name == "credential"
        || name.ends_with(".key")
        || name.ends_with(".pem")
}

/// The mode a migrated file must have: owner bits of the source, secrets 0600.
///
/// Permission bits only exist on unix. Everywhere else `mode_of` reports 0 for every
/// file, so the expected mode must be 0 too or the verification below would compare a
/// 0600 expectation against a 0 reading and reject every correct copy. On those
/// platforms the content (length + sha256) and the entry kind are what is verified;
/// access is governed by the ACL the new directory inherits from its parent.
fn target_file_mode(rel: &Path, src_mode: u32) -> u32 {
    if !cfg!(unix) {
        0
    } else if is_secret_path(rel) {
        0o600
    } else {
        src_mode & 0o700
    }
}

fn expected_node(rel: &Path, node: &Node) -> Node {
    match node {
        Node::File { len, sha, mode } => Node::File {
            len: *len,
            sha: sha.clone(),
            mode: target_file_mode(rel, *mode),
        },
        other => other.clone(),
    }
}

/// A copy in a temp directory that has not been moved into place.
struct Staged {
    temp: PathBuf,
    scan: Scan,
    tightened: Vec<PathBuf>,
}

fn stage_copy(from: &Path, to: &Path) -> Result<Staged, MigrationError> {
    let parent = to.parent().ok_or_else(|| MigrationError::Io {
        what: "find the parent directory of",
        path: to.to_path_buf(),
        source: std::io::Error::other("no parent directory"),
    })?;
    human::create_private_dirs(parent).map_err(io_err("create the parent directory", parent))?;
    let temp = parent.join(format!(
        "{}{}-{}",
        temp_prefix(to),
        std::process::id(),
        nanos()
    ));
    let src_scan = scan(from, true).map_err(io_err("read the old state directory", from))?;
    make_dir_private(&temp, false).map_err(io_err("create the temporary copy", &temp))?;
    let result = copy_into(from, &temp, &src_scan);
    match result {
        Ok(tightened) => Ok(Staged {
            temp,
            scan: src_scan,
            tightened,
        }),
        Err(e) => {
            // Our own half-written temp copy; the old data is untouched.
            let _ = std::fs::remove_dir_all(&temp);
            Err(e)
        }
    }
}

fn make_dir_private(path: &Path, recursive: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(recursive)
            .mode(0o700)
            .create(path)?;
        // The umask can only remove bits; set the mode explicitly all the same.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        if recursive {
            std::fs::create_dir_all(path)
        } else {
            std::fs::create_dir(path)
        }
    }
}

fn copy_into(from: &Path, temp: &Path, src: &Scan) -> Result<Vec<PathBuf>, MigrationError> {
    let mut tightened = Vec::new();
    for (rel, node) in &src.manifest {
        let s = from.join(rel);
        let d = temp.join(rel);
        match node {
            Node::Dir => make_dir_private(&d, false).map_err(io_err("create the directory", &d))?,
            Node::Link(target) => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &d)
                    .map_err(io_err("recreate the symlink", &d))?;
                #[cfg(not(unix))]
                {
                    let _ = target;
                }
            }
            Node::File { sha, mode, .. } => {
                let mut input = std::fs::File::open(&s).map_err(io_err("open", &s))?;
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                let mut out = opts.open(&d).map_err(io_err("create", &d))?;
                let mut h = Sha256::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = input.read(&mut buf).map_err(io_err("read", &s))?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                    out.write_all(&buf[..n]).map_err(io_err("write", &d))?;
                }
                out.sync_all().map_err(io_err("flush", &d))?;
                let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
                if &got != sha {
                    return Err(MigrationError::Verify {
                        from: from.to_path_buf(),
                        reason: format!(
                            "{} changed while it was being copied; stop whatever writes to \
                             the old state directory",
                            rel.display()
                        ),
                    });
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    out.set_permissions(std::fs::Permissions::from_mode(target_file_mode(
                        rel, *mode,
                    )))
                    .map_err(io_err("set permissions on", &d))?;
                }
                // `mode` is always 0 off unix (see `mode_of`), so this never fires there.
                if *mode & 0o077 != 0 {
                    tightened.push(rel.clone());
                }
                if let Ok(m) = std::fs::metadata(&s).and_then(|m| m.modified()) {
                    let _ = out.set_modified(m);
                }
            }
        }
    }
    Ok(tightened)
}

/// Verify a staged copy against the source: same entries and kinds, same file
/// count, same sha256 of every file, expected modes, and the source unchanged
/// since it was scanned.
fn verify_stage(from: &Path, staged: &Staged) -> Result<(), MigrationError> {
    let verify = |reason: String| MigrationError::Verify {
        from: from.to_path_buf(),
        reason,
    };
    let dst = scan(&staged.temp, false).map_err(io_err("re-read the copy", &staged.temp))?;
    let expected: BTreeMap<_, _> = staged
        .scan
        .manifest
        .iter()
        .map(|(k, v)| (k.clone(), expected_node(k, v)))
        .collect();
    if dst.manifest != expected {
        let mut diffs = Vec::new();
        for (k, v) in &expected {
            match dst.manifest.get(k) {
                None => diffs.push(format!("{} is missing from the copy", k.display())),
                Some(got) if got != v => diffs.push(format!(
                    "{} differs (size, sha256, kind or mode)",
                    k.display()
                )),
                _ => {}
            }
        }
        for k in dst.manifest.keys() {
            if !expected.contains_key(k) {
                diffs.push(format!("{} is unexpected in the copy", k.display()));
            }
        }
        diffs.truncate(5);
        return Err(verify(diffs.join("; ")));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for (rel, node) in &dst.manifest {
            if matches!(node, Node::Dir) {
                let m = std::fs::metadata(staged.temp.join(rel))
                    .map_err(io_err("stat", &staged.temp.join(rel)))?
                    .permissions()
                    .mode()
                    & 0o7777;
                if m != 0o700 {
                    return Err(verify(format!(
                        "directory {} has mode {m:o}, want 700",
                        rel.display()
                    )));
                }
            }
        }
        let root_mode = std::fs::metadata(&staged.temp)
            .map_err(io_err("stat", &staged.temp))?
            .permissions()
            .mode()
            & 0o7777;
        if root_mode != 0o700 {
            return Err(verify(format!(
                "the new root has mode {root_mode:o}, want 700"
            )));
        }
    }
    let again = scan(from, true).map_err(io_err("re-read the old state directory", from))?;
    if again.manifest != staged.scan.manifest {
        return Err(verify(
            "the old directory changed while it was being copied".to_string(),
        ));
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn same_path(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf())
        == std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf())
}

/// Copy the state in `from` to `to` exactly once (see the module comment).
pub fn migrate(
    from: &Path,
    to: &Path,
    ctx: &MigrateCtx<'_>,
) -> Result<MigrationOutcome, MigrationError> {
    if !from.exists() {
        return Ok(MigrationOutcome::NothingToMigrate {
            reason: format!("there is no state directory at {}", from.display()),
        });
    }
    let from = std::fs::canonicalize(from).map_err(io_err("resolve", from))?;
    let to = normalize_target(to).map_err(io_err("resolve", to))?;
    if from == to {
        return Ok(MigrationOutcome::NothingToMigrate {
            reason: format!(
                "the old and new directories are the same ({})",
                to.display()
            ),
        });
    }
    if to.starts_with(&from) || from.starts_with(&to) {
        return Err(MigrationError::Nested { a: from, b: to });
    }
    let stale_temps = stale_temp_dirs(&to);

    // Idempotence: a previous run completed exactly this migration.
    let done_here = read_json::<MigratedFrom>(&to.join(MIGRATED_FROM_MARKER))
        .is_some_and(|m| same_path(Path::new(&m.from), &from));
    let recorded_to = read_json::<MigratedTo>(&from.join(MIGRATED_TO_MARKER));
    if done_here && has_state(&to) {
        return Ok(MigrationOutcome::AlreadyMigrated {
            from,
            to,
            stale_temps,
        });
    }
    if let Some(rec) = &recorded_to {
        if same_path(Path::new(&rec.to), &to) && has_state(&to) {
            return Ok(MigrationOutcome::AlreadyMigrated {
                from,
                to,
                stale_temps,
            });
        }
    }
    if !has_state(&from) {
        return Ok(MigrationOutcome::NothingToMigrate {
            reason: format!("there is no TA state in {}", from.display()),
        });
    }
    if let Some(rec) = recorded_to {
        return Err(MigrationError::MigratedElsewhere {
            from,
            recorded: rec.to,
            requested: to.display().to_string(),
        });
    }
    if has_state(&to) {
        return Err(MigrationError::Conflict {
            old: Box::new(dir_info(&from)),
            new: Box::new(dir_info(&to)),
        });
    }
    match std::fs::read_dir(&to) {
        Ok(mut it) => {
            if it.next().is_some() {
                return Err(MigrationError::TargetNotEmpty { to });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(MigrationError::TargetNotEmpty { to }),
    }
    let procs = running_processes(&from, ctx.is_alive);
    if !procs.is_empty() {
        return Err(MigrationError::DaemonRunning { from, procs });
    }

    let staged = stage_copy(&from, &to)?;
    if let Err(e) = verify_stage(&from, &staged) {
        let _ = std::fs::remove_dir_all(&staged.temp);
        return Err(e);
    }
    let (mut files, mut dirs, mut symlinks, mut bytes) = (0, 0, 0, 0);
    for node in staged.scan.manifest.values() {
        match node {
            Node::Dir => dirs += 1,
            Node::Link(_) => symlinks += 1,
            Node::File { len, .. } => {
                files += 1;
                bytes += len;
            }
        }
    }
    let marker = MigratedFrom {
        from: from.display().to_string(),
        migrated_at: Utc::now().to_rfc3339(),
        ta_version: ctx.tool_version.to_string(),
        files,
        bytes,
    };
    let marker_json = serde_json::to_string_pretty(&marker).expect("marker serialises") + "\n";
    let commit = write_atomic_private(
        &staged.temp.join(MIGRATED_FROM_MARKER),
        marker_json.as_bytes(),
    )
    .map_err(io_err("write the migration marker in", &staged.temp))
    .and_then(|()| {
        std::fs::rename(&staged.temp, &to).map_err(io_err("move the verified copy to", &to))
    });
    if let Err(e) = commit {
        // A racing migration may have put identical state there first.
        if has_state(&to)
            && read_json::<MigratedFrom>(&to.join(MIGRATED_FROM_MARKER))
                .is_some_and(|m| same_path(Path::new(&m.from), &from))
        {
            let _ = std::fs::remove_dir_all(&staged.temp);
            return Ok(MigrationOutcome::AlreadyMigrated {
                from,
                to,
                stale_temps,
            });
        }
        let _ = std::fs::remove_dir_all(&staged.temp);
        return Err(e);
    }

    let old_info = dir_info(&from);
    let to_marker = MigratedTo {
        to: to.display().to_string(),
        migrated_at: marker.migrated_at.clone(),
        ta_version: ctx.tool_version.to_string(),
        note: "TA state was copied to `to` and verified. This directory is no longer used. \
               It was left untouched; delete it yourself once you have checked the new location."
            .to_string(),
    };
    let old_marker_error = write_atomic_private(
        &from.join(MIGRATED_TO_MARKER),
        (serde_json::to_string_pretty(&to_marker).expect("marker serialises") + "\n").as_bytes(),
    )
    .err()
    .map(|e| e.to_string());
    Ok(MigrationOutcome::Migrated(MigrationReport {
        from,
        to,
        files,
        dirs,
        symlinks,
        bytes,
        tightened: staged.tightened,
        skipped: staged.scan.skipped,
        old_info,
        old_marker_error,
        stale_temps,
    }))
}

// ── Automatic migration ────────────────────────────────────────────────────

/// What `ensure_migrated` found.
#[derive(Debug, Clone)]
pub enum AutoMigration {
    NotNeeded,
    Migrated(Box<MigrationReport>),
}

/// When the resolver selects a directory (env or pointer) that has no TA state
/// while the OS-default (legacy) directory has some that was never migrated,
/// copy it over once. A no-op in every other case, and always a no-op in cargo
/// test binaries and under `TA_TEST_NO_EXTERNAL_AGENTS`, so a test can never
/// copy the developer's real state. Called by `ta` and `ta-daemon` at startup.
pub fn ensure_migrated_with(
    env: &dyn Fn(&str) -> Option<String>,
    host: &Host,
    ctx: &MigrateCtx<'_>,
) -> Result<AutoMigration, MigrationError> {
    if host.test_guard || env(NO_EXTERNAL_AGENTS_ENV).is_some() {
        return Ok(AutoMigration::NotNeeded);
    }
    let Ok(resolved) = resolve_with(env, host) else {
        return Ok(AutoMigration::NotNeeded);
    };
    if !matches!(
        resolved.source,
        StateSource::Env | StateSource::Pointer { .. }
    ) {
        return Ok(AutoMigration::NotNeeded);
    }
    let Some(legacy) = os_default_root(host.os, env, host.home.as_deref()) else {
        return Ok(AutoMigration::NotNeeded);
    };
    if has_state(&resolved.root)
        || same_path(&legacy, &resolved.root)
        || !has_state(&legacy)
        || legacy.join(MIGRATED_TO_MARKER).exists()
    {
        return Ok(AutoMigration::NotNeeded);
    }
    match migrate(&legacy, &resolved.root, ctx)? {
        MigrationOutcome::Migrated(r) => Ok(AutoMigration::Migrated(Box::new(r))),
        _ => Ok(AutoMigration::NotNeeded),
    }
}

/// [`ensure_migrated_with`] for the running process.
pub fn ensure_migrated(ctx: &MigrateCtx<'_>) -> Result<AutoMigration, MigrationError> {
    ensure_migrated_with(&process_env, &Host::current(), ctx)
}

// ── Relocate ───────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum RelocateError {
    #[error(transparent)]
    Dir(#[from] StateDirError),
    #[error(transparent)]
    Migration(#[from] MigrationError),
    #[error("could not resolve the target {}: {source}. Pass an absolute path.", path.display())]
    Target {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct RelocateOutcome {
    pub to: PathBuf,
    pub pointer_file: PathBuf,
    /// False when the pointer already named `to`.
    pub pointer_changed: bool,
    /// `None` with `--no-migrate`, or when this process already used `to`.
    pub migration: Option<MigrationOutcome>,
    /// `TA_STATE_DIR` as set in this process, when it names a different
    /// directory and therefore shadows the pointer for processes that have it.
    pub env_override: Option<String>,
}

/// Point every ta process of this user at `to`, migrating the current state
/// there first (unless `migrate_data` is false). The pointer changes only
/// after the migration succeeded, so a refusal or crash leaves the old
/// directory in use.
pub fn relocate_with(
    env: &dyn Fn(&str) -> Option<String>,
    host: &Host,
    to: &Path,
    migrate_data: bool,
    ctx: &MigrateCtx<'_>,
) -> Result<RelocateOutcome, RelocateError> {
    let to = normalize_target(to).map_err(|source| RelocateError::Target {
        path: to.to_path_buf(),
        source,
    })?;
    let pointer_file = human::user_state_dir_with(env, host.home.as_deref())
        .map(|d| d.join(POINTER_FILENAME))
        .ok_or(StateDirError::NoUserDir)?;
    if host.test_guard && env(USER_DIR_ENV).filter(|v| !v.is_empty()).is_none() {
        return Err(StateDirError::TestWithoutUserDir.into());
    }

    let current = resolve_with(env, host).ok().map(|r| r.root);
    let legacy = os_default_root(host.os, env, host.home.as_deref());
    let mut migration = None;
    if migrate_data {
        let already_here = current.as_ref().is_some_and(|c| same_path(c, &to));
        if !already_here {
            // Prefer what this process uses now; otherwise an unmigrated legacy dir.
            let source = current
                .iter()
                .find(|c| has_state(c))
                .or_else(|| {
                    legacy
                        .iter()
                        .find(|l| has_state(l) || l.join(MIGRATED_TO_MARKER).exists())
                })
                .cloned();
            if let Some(src) = source {
                migration = Some(migrate(&src, &to, ctx)?);
            }
        }
    }

    let pointer_changed = read_pointer_file(&pointer_file)
        .ok()
        .flatten()
        .is_none_or(|p| !same_path(&p, &to));
    if pointer_changed {
        write_pointer_with(env, host, &to)?;
    }
    let env_override = env(STATE_ROOT_ENV)
        .filter(|v| !v.is_empty())
        .filter(|v| !same_path(Path::new(v), &to));
    Ok(RelocateOutcome {
        to,
        pointer_file,
        pointer_changed,
        migration,
        env_override,
    })
}

/// [`relocate_with`] for the running process (`ta state relocate`).
pub fn relocate(
    to: &Path,
    migrate_data: bool,
    ctx: &MigrateCtx<'_>,
) -> Result<RelocateOutcome, RelocateError> {
    relocate_with(&process_env, &Host::current(), to, migrate_data, ctx)
}

// ── Diagnosis (ta doctor / ta state show) ──────────────────────────────────

/// How the legacy (OS-default) directory relates to the resolved one.
#[derive(Debug, Clone)]
pub enum LegacyStatus {
    /// The OS default could not be computed (no home) or IS the resolved directory.
    NotApplicable,
    /// No TA state at the OS-default location.
    Absent,
    /// State there, none in the resolved directory, never migrated.
    Unmigrated(DirInfo),
    /// Both hold state and nothing links them: needs a decision.
    BothHaveState { legacy: DirInfo, current: DirInfo },
    /// Migrated; the old copy still exists and can be deleted after checking.
    MigratedKept { to: PathBuf, info: DirInfo },
}

#[derive(Debug, Clone)]
pub struct Diagnosis {
    pub resolved: Result<ResolvedStateDir, String>,
    pub env_value: Option<String>,
    pub pointer_file: Option<PathBuf>,
    pub pointer_value: Option<PathBuf>,
    pub pointer_error: Option<String>,
    /// Set when `TA_STATE_DIR` and the pointer both name a directory and differ.
    pub diverged: bool,
    pub user_dir: Option<PathBuf>,
    pub os_default: Option<PathBuf>,
    pub legacy: LegacyStatus,
    pub stale_temps: Vec<StaleTemp>,
}

pub fn diagnose_with(env: &dyn Fn(&str) -> Option<String>, host: &Host) -> Diagnosis {
    let resolved = resolve_with(env, host);
    let env_value = env(STATE_ROOT_ENV).filter(|v| !v.is_empty());
    let pointer_file = pointer_file_with(env, host);
    let (pointer_value, pointer_error) = match pointer_file.as_deref().map(read_pointer_file) {
        Some(Ok(v)) => (v, None),
        Some(Err(e)) => (None, Some(e.to_string())),
        None => (None, None),
    };
    let diverged = match (&env_value, &pointer_value) {
        (Some(e), Some(p)) => !same_path(Path::new(e), p),
        _ => false,
    };
    let os_default = os_default_root(host.os, env, host.home.as_deref());
    let legacy = match (&resolved, &os_default) {
        (Ok(r), Some(l)) if !same_path(l, &r.root) && !host.test_guard => {
            if let Ok(m) = std::fs::read_to_string(l.join(MIGRATED_TO_MARKER)) {
                let to = serde_json::from_str::<MigratedTo>(&m)
                    .map(|m| PathBuf::from(m.to))
                    .unwrap_or_default();
                LegacyStatus::MigratedKept {
                    to,
                    info: dir_info(l),
                }
            } else if !has_state(l) {
                LegacyStatus::Absent
            } else if has_state(&r.root) {
                LegacyStatus::BothHaveState {
                    legacy: dir_info(l),
                    current: dir_info(&r.root),
                }
            } else {
                LegacyStatus::Unmigrated(dir_info(l))
            }
        }
        _ => LegacyStatus::NotApplicable,
    };
    let stale_temps = resolved
        .as_ref()
        .map(|r| stale_temp_dirs(&r.root))
        .unwrap_or_default();
    Diagnosis {
        resolved: resolved.map_err(|e| e.to_string()),
        env_value,
        pointer_file,
        pointer_value,
        pointer_error,
        diverged,
        user_dir: human::user_state_dir_with(env, host.home.as_deref()),
        os_default,
        legacy,
        stale_temps,
    }
}

pub fn diagnose() -> Diagnosis {
    diagnose_with(&process_env, &Host::current())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        load_secret, store_secret, ControlSecret, CustodyOpts, ProjectState, StateLocator,
    };
    use std::collections::HashMap;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn env_of(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn host(os: Os, home: Option<&Path>) -> Host {
        Host {
            os,
            home: home.map(Path::to_path_buf),
            test_guard: false,
            pid: 4242,
            temp_dir: PathBuf::from("/tmp"),
        }
    }

    fn s(p: &Path) -> String {
        p.display().to_string()
    }

    /// A path that is absolute on the host OS: a bare `/x` has no drive on Windows and is
    /// (correctly) rejected there as relative.
    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{p}"))
        } else {
            PathBuf::from(p)
        }
    }

    const NOT_ALIVE: &dyn Fn(u32) -> bool = &|_| false;
    fn ctx() -> MigrateCtx<'static> {
        MigrateCtx {
            is_alive: NOT_ALIVE,
            tool_version: "9.9.9-test",
        }
    }

    // ── Precedence ──

    #[test]
    fn os_defaults_are_exact_per_os() {
        let home = Path::new("/home/u");
        let none = env_of(&[]);
        assert_eq!(
            os_default_root(Os::MacOs, &none, Some(home)).unwrap(),
            Path::new("/home/u/Library/Application Support/TrustedAutonomy")
        );
        assert_eq!(
            os_default_root(Os::Linux, &none, Some(home)).unwrap(),
            Path::new("/home/u/.local/state/trusted-autonomy")
        );
        assert_eq!(
            os_default_root(
                Os::Linux,
                &env_of(&[("XDG_STATE_HOME", s(&abs("/x/state")))]),
                None
            )
            .unwrap(),
            abs("/x/state").join("trusted-autonomy"),
            "XDG_STATE_HOME needs no home"
        );
        assert_eq!(
            os_default_root(
                Os::Linux,
                &env_of(&[("XDG_STATE_HOME", "relative".into())]),
                Some(home)
            )
            .unwrap(),
            Path::new("/home/u/.local/state/trusted-autonomy"),
            "a relative XDG_STATE_HOME is invalid per the XDG spec and ignored"
        );
        assert_eq!(
            os_default_root(
                Os::Windows,
                &env_of(&[("LOCALAPPDATA", "C:/Users/u/AppData/Local".into())]),
                Some(home)
            )
            .unwrap(),
            Path::new("C:/Users/u/AppData/Local/TrustedAutonomy")
        );
        assert_eq!(
            os_default_root(Os::Windows, &none, Some(home)).unwrap(),
            Path::new("/home/u/AppData/Local/TrustedAutonomy")
        );
        assert!(os_default_root(Os::MacOs, &none, None).is_none());
    }

    #[test]
    fn precedence_env_then_pointer_then_test_temp_then_default() {
        let home = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let ptr_target = tempfile::tempdir().unwrap();
        let env_target = tempfile::tempdir().unwrap();
        let user_env = ("TA_USER_STATE_DIR", s(user.path()));

        // Nothing set: OS default.
        let h = host(Os::MacOs, Some(home.path()));
        let r = resolve_with(&env_of(std::slice::from_ref(&user_env)), &h).unwrap();
        assert_eq!(r.source, StateSource::OsDefault);
        assert_eq!(
            r.root,
            home.path()
                .join("Library/Application Support/TrustedAutonomy")
        );

        // Pointer beats the default.
        write_atomic_private(
            &user.path().join(POINTER_FILENAME),
            format!("{}\n", ptr_target.path().display()).as_bytes(),
        )
        .unwrap();
        let r = resolve_with(&env_of(std::slice::from_ref(&user_env)), &h).unwrap();
        assert_eq!(r.root, ptr_target.path());
        assert_eq!(
            r.source,
            StateSource::Pointer {
                file: user.path().join(POINTER_FILENAME)
            }
        );

        // Env beats the pointer.
        let with_env = env_of(&[user_env.clone(), ("TA_STATE_DIR", s(env_target.path()))]);
        let r = resolve_with(&with_env, &h).unwrap();
        assert_eq!(
            (r.root.as_path(), r.source),
            (env_target.path(), StateSource::Env)
        );

        // An EMPTY env var is "unset".
        let empty_env = env_of(&[user_env.clone(), ("TA_STATE_DIR", String::new())]);
        assert_eq!(
            resolve_with(&empty_env, &h).unwrap().root,
            ptr_target.path()
        );

        // Test guard: temp dir beats the default, pointer (with an explicit user dir) beats temp.
        let mut th = host(Os::MacOs, Some(home.path()));
        th.test_guard = true;
        th.temp_dir = PathBuf::from("/tmp/x");
        let r = resolve_with(&env_of(std::slice::from_ref(&user_env)), &th).unwrap();
        assert_eq!(
            r.root,
            ptr_target.path(),
            "an explicit user dir's pointer is honoured"
        );
        std::fs::remove_file(user.path().join(POINTER_FILENAME)).unwrap();
        let r = resolve_with(&env_of(&[user_env]), &th).unwrap();
        assert_eq!(r.source, StateSource::TestTemp);
        assert_eq!(
            r.root,
            Path::new("/tmp/x/ta-test-state-4242/TrustedAutonomy")
        );
    }

    #[test]
    fn test_binary_ignores_the_real_user_pointer() {
        // No TA_USER_STATE_DIR: a developer's real ~/.config/ta/human/state-dir
        // must not be consulted by a test binary.
        let home = tempfile::tempdir().unwrap();
        let real_ptr = home.path().join(".config/ta/human").join(POINTER_FILENAME);
        write_atomic_private(&real_ptr, b"/somewhere/real\n").unwrap();
        let mut th = host(Os::MacOs, Some(home.path()));
        th.test_guard = true;
        let r = resolve_with(&env_of(&[]), &th).unwrap();
        assert_eq!(r.source, StateSource::TestTemp);
        assert!(matches!(
            write_pointer_with(&env_of(&[]), &th, Path::new("/x")),
            Err(StateDirError::TestWithoutUserDir)
        ));
    }

    #[test]
    fn no_home_and_no_override_is_an_actionable_error_never_cwd() {
        for os in [Os::MacOs, Os::Windows, Os::Linux] {
            let err = resolve_with(&env_of(&[]), &host(os, None)).unwrap_err();
            assert!(matches!(err, StateDirError::NoHome));
            let msg = err.to_string();
            assert!(msg.contains("TA_STATE_DIR"), "{msg}");
            assert!(msg.contains("ta state relocate"), "{msg}");
            assert!(msg.contains("HOME"), "{msg}");
        }
        // But an override works without a home.
        let srv = abs("/srv/ta");
        let r = resolve_with(
            &env_of(&[("TA_STATE_DIR", s(&srv))]),
            &host(Os::MacOs, None),
        )
        .unwrap();
        assert_eq!(r.root, srv);
        // A pointer works without a home when TA_USER_STATE_DIR names where it is.
        let user = tempfile::tempdir().unwrap();
        let srv2 = abs("/srv/ta2");
        write_atomic_private(
            &user.path().join(POINTER_FILENAME),
            format!("{}\n", srv2.display()).as_bytes(),
        )
        .unwrap();
        let r = resolve_with(
            &env_of(&[("TA_USER_STATE_DIR", s(user.path()))]),
            &host(Os::MacOs, None),
        )
        .unwrap();
        assert_eq!(r.root, srv2);
    }

    #[test]
    fn relative_env_and_bad_pointers_are_errors_not_silent_fallbacks() {
        let home = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let h = host(Os::MacOs, Some(home.path()));
        let e = resolve_with(&env_of(&[("TA_STATE_DIR", "rel/dir".into())]), &h).unwrap_err();
        assert!(matches!(e, StateDirError::EnvNotAbsolute { .. }), "{e}");
        let user_env = ("TA_USER_STATE_DIR", s(user.path()));
        let ptr = user.path().join(POINTER_FILENAME);
        for bad in ["", "   \n", "relative/path\n", "/a\n/b\n"] {
            std::fs::write(&ptr, bad).unwrap();
            let e = resolve_with(&env_of(std::slice::from_ref(&user_env)), &h).unwrap_err();
            assert!(
                matches!(e, StateDirError::BadPointer { .. }),
                "{bad:?}: {e}"
            );
            assert!(e.to_string().contains("ta state relocate"), "{e}");
        }
    }

    #[test]
    fn pointer_write_is_atomic_private_and_replaces() {
        let user = tempfile::tempdir().unwrap();
        let env = env_of(&[("TA_USER_STATE_DIR", s(&user.path().join("nested/human")))]);
        let h = host(Os::MacOs, None);
        let one = abs("/one");
        let two = abs("/two");
        let file = write_pointer_with(&env, &h, &one).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{}\n", one.display())
        );
        write_pointer_with(&env, &h, &two).unwrap();
        assert_eq!(read_pointer_file(&file).unwrap().unwrap(), two);
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dmode = std::fs::metadata(file.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dmode, 0o700, "a user dir TA creates is owner-only");
        }
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            leftovers,
            vec![POINTER_FILENAME.to_string()],
            "no temp file left"
        );
    }

    // ── Migration ──

    /// A realistic old state dir: two projects, secrets, tokens, nested data,
    /// a symlink, a loose-mode file and a loose directory.
    fn fake_old_state(root: &Path) {
        for id in ["p_aaaa", "p_bbbb"] {
            let p = root.join("projects").join(id);
            std::fs::create_dir_all(p.join("secrets")).unwrap();
            std::fs::create_dir_all(p.join("nats-jetstream/streams/x")).unwrap();
            std::fs::create_dir_all(p.join("action-queue")).unwrap();
            std::fs::write(p.join("secrets/vault-key"), format!("AGE-SECRET-KEY-{id}")).unwrap();
            std::fs::write(p.join("daemon-tokens.json"), format!("{{\"id\":\"{id}\"}}")).unwrap();
            std::fs::write(p.join("config-hashes.json"), "{}").unwrap();
            std::fs::write(p.join("project-path"), format!("/not/a/real/{id}")).unwrap();
            std::fs::write(
                p.join("nats-jetstream/streams/x/blk.1.blk"),
                vec![7u8; 5000],
            )
            .unwrap();
            std::fs::write(p.join("action-queue/job1.json"), "{}").unwrap();
            #[cfg(unix)]
            {
                for f in ["secrets/vault-key", "daemon-tokens.json"] {
                    std::fs::set_permissions(p.join(f), std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
                for d in ["secrets", ".", "nats-jetstream"] {
                    std::fs::set_permissions(p.join(d), std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
            }
        }
        std::fs::write(root.join("posture"), "strict").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("posture", root.join("posture-link")).unwrap();
            // Looser than owner-only: must be tightened.
            let loose = root.join("projects/p_aaaa/config-hashes.json");
            std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).unwrap();
            let loose_secret = root.join("projects/p_bbbb/daemon-tokens.json");
            std::fs::set_permissions(&loose_secret, std::fs::Permissions::from_mode(0o644))
                .unwrap();
            std::fs::set_permissions(
                root.join("projects/p_bbbb/action-queue"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Node> {
        scan(root, true).unwrap().manifest
    }

    #[test]
    fn migration_happy_path_copies_verifies_marks_and_never_touches_the_old_data() {
        let base = tempfile::tempdir().unwrap();
        let old = base.path().join("old state");
        let new = base.path().join("data/ta-state");
        std::fs::create_dir_all(&old).unwrap();
        fake_old_state(&old);
        let before = snapshot(&old);

        let out = migrate(&old, &new, &ctx()).unwrap();
        let MigrationOutcome::Migrated(r) = out else {
            panic!("expected Migrated, got {out:?}")
        };
        assert!(
            r.files >= 12
                && r.dirs >= 10
                && r.symlinks == u64::from(cfg!(unix)) // fake_old_state makes the link on unix only
                && r.bytes > 10_000,
            "{r:?}"
        );
        assert!(r.old_marker_error.is_none());

        // Same tree, byte for byte, with modes normalised.
        let after = snapshot(&new);
        assert_eq!(after.len(), before.len());
        for (rel, node) in &before {
            match (node, &after[rel]) {
                (
                    Node::File {
                        sha: a, len: la, ..
                    },
                    Node::File {
                        sha: b, len: lb, ..
                    },
                ) => {
                    assert_eq!((a, la), (b, lb), "{rel:?}")
                }
                (a, b) => assert_eq!(a, b, "{rel:?}"),
            }
        }
        // Permissions: dirs 0700, secrets 0600, loose files tightened.
        #[cfg(unix)]
        {
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode(&new), 0o700);
            assert_eq!(mode(&new.join("projects")), 0o700);
            assert_eq!(mode(&new.join("projects/p_bbbb/action-queue")), 0o700);
            assert_eq!(mode(&new.join("projects/p_aaaa/secrets")), 0o700);
            assert_eq!(mode(&new.join("projects/p_aaaa/secrets/vault-key")), 0o600);
            assert_eq!(mode(&new.join("projects/p_bbbb/daemon-tokens.json")), 0o600);
            assert_eq!(mode(&new.join("projects/p_aaaa/config-hashes.json")), 0o600);
            assert!(r
                .tightened
                .iter()
                .any(|p| p.ends_with("config-hashes.json")));
            assert_eq!(
                std::fs::read_link(new.join("posture-link")).unwrap(),
                Path::new("posture")
            );
        }
        // Markers.
        let from: MigratedFrom = read_json(&new.join(MIGRATED_FROM_MARKER)).unwrap();
        assert_eq!(from.ta_version, "9.9.9-test");
        assert!(Path::new(&from.from).ends_with("old state"));
        assert_eq!(from.files, r.files);
        let to: MigratedTo = read_json(&old.join(MIGRATED_TO_MARKER)).unwrap();
        assert!(Path::new(&to.to).ends_with("data/ta-state"));
        // Old data untouched (markers excluded), nothing temp left behind.
        assert_eq!(snapshot(&old), before);
        assert!(stale_temp_dirs(&new).is_empty());
        assert_eq!(
            std::fs::read_to_string(new.join("projects/p_aaaa/secrets/vault-key")).unwrap(),
            "AGE-SECRET-KEY-p_aaaa"
        );
    }

    #[test]
    fn second_run_is_a_noop() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("old"), base.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        fake_old_state(&old);
        assert!(matches!(
            migrate(&old, &new, &ctx()).unwrap(),
            MigrationOutcome::Migrated(_)
        ));
        let first = snapshot(&new);
        let marker = std::fs::read_to_string(new.join(MIGRATED_FROM_MARKER)).unwrap();
        for _ in 0..2 {
            match migrate(&old, &new, &ctx()).unwrap() {
                MigrationOutcome::AlreadyMigrated { .. } => {}
                other => panic!("expected AlreadyMigrated, got {other:?}"),
            }
        }
        assert_eq!(snapshot(&new), first);
        assert_eq!(
            std::fs::read_to_string(new.join(MIGRATED_FROM_MARKER)).unwrap(),
            marker,
            "the marker is not rewritten"
        );
        // Even if the new marker is gone, the old MIGRATED-TO makes it a no-op.
        std::fs::remove_file(new.join(MIGRATED_FROM_MARKER)).unwrap();
        assert!(matches!(
            migrate(&old, &new, &ctx()).unwrap(),
            MigrationOutcome::AlreadyMigrated { .. }
        ));
    }

    #[test]
    fn running_daemon_blocks_migration_and_nothing_is_created() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("old"), base.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        fake_old_state(&old);
        // The project recorded in projects/p_aaaa/project-path has a live daemon.
        let proj = base.path().join("proj");
        std::fs::create_dir_all(proj.join(".ta")).unwrap();
        std::fs::write(
            proj.join(".ta/daemon.pid"),
            "pid=31337\nbind=127.0.0.1:7700\n",
        )
        .unwrap();
        std::fs::write(
            old.join("projects/p_aaaa/project-path"),
            proj.display().to_string(),
        )
        .unwrap();
        let alive = |pid: u32| pid == 31337;
        let c = MigrateCtx {
            is_alive: &alive,
            tool_version: "t",
        };
        let err = migrate(&old, &new, &c).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, MigrationError::DaemonRunning { .. }));
        assert!(
            msg.contains("31337") && msg.contains("daemon.pid") && msg.contains("ta daemon stop"),
            "{msg}"
        );
        assert!(!new.exists(), "nothing created");
        assert!(stale_temp_dirs(&new).is_empty());
        assert!(!old.join(MIGRATED_TO_MARKER).exists());

        // A dead pid (stale file) does not block.
        let dead = |_pid: u32| false;
        let c2 = MigrateCtx {
            is_alive: &dead,
            tool_version: "t",
        };
        assert!(matches!(
            migrate(&old, &new, &c2).unwrap(),
            MigrationOutcome::Migrated(_)
        ));

        // A pid file inside the state dir (nats) also blocks.
        let new2 = base.path().join("new2");
        let old2 = base.path().join("old2");
        std::fs::create_dir_all(&old2).unwrap();
        fake_old_state(&old2);
        std::fs::write(old2.join("projects/p_bbbb/nats.pid"), "555\n").unwrap();
        let alive555 = |pid: u32| pid == 555;
        let c3 = MigrateCtx {
            is_alive: &alive555,
            tool_version: "t",
        };
        assert!(matches!(
            migrate(&old2, &new2, &c3).unwrap_err(),
            MigrationError::DaemonRunning { .. }
        ));
    }

    #[test]
    fn both_have_state_is_reported_and_nothing_is_changed() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("old"), base.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        fake_old_state(&old);
        std::fs::create_dir_all(new.join("projects/p_cccc")).unwrap();
        std::fs::write(new.join("projects/p_cccc/daemon-tokens.json"), "{}").unwrap();
        let (b_old, b_new) = (snapshot(&old), snapshot(&new));
        let err = migrate(&old, &new, &ctx()).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, MigrationError::Conflict { .. }), "{msg}");
        // The message names the canonical paths (on Windows: long, `\\?\`-prefixed).
        let old_c = std::fs::canonicalize(&old).unwrap();
        let new_c = normalize_target(&new).unwrap();
        assert!(
            msg.contains(&s(&old_c)) && msg.contains(&s(&new_c)),
            "{msg}"
        );
        assert!(
            msg.contains("--no-migrate")
                && msg.contains("project(s)")
                && msg.contains("last modified"),
            "{msg}"
        );
        assert_eq!(snapshot(&old), b_old);
        assert_eq!(snapshot(&new), b_new);
        assert!(!old.join(MIGRATED_TO_MARKER).exists());
        assert!(!new.join(MIGRATED_FROM_MARKER).exists());
    }

    #[test]
    fn interrupted_copy_leaves_no_half_state_and_is_reported() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("old"), base.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        fake_old_state(&old);
        // Simulate a crash after the copy but before the rename: staged, never committed.
        let staged = stage_copy(&old, &new).unwrap();
        assert!(staged.temp.is_dir());
        assert!(!new.exists(), "the final directory does not exist yet");
        assert!(!has_state(&new), "a half copy is never mistaken for state");
        let stale = stale_temp_dirs(&new);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].path, staged.temp);
        assert_eq!(stale[0].pid, Some(std::process::id()));
        assert!(stale[0].bytes > 0);
        // A crash mid-copy: a partial temp with a truncated file.
        let partial = base.path().join(format!("{}1-2", temp_prefix(&new)));
        std::fs::create_dir_all(partial.join("projects/p_aaaa")).unwrap();
        std::fs::write(partial.join("projects/p_aaaa/daemon-tokens.json"), "{\"i").unwrap();
        assert_eq!(stale_temp_dirs(&new).len(), 2);
        // The next run succeeds from the pristine old data and reports both leftovers.
        let MigrationOutcome::Migrated(r) = migrate(&old, &new, &ctx()).unwrap() else {
            panic!()
        };
        assert_eq!(r.stale_temps.len(), 2, "{:?}", r.stale_temps);
        assert!(new.join(MIGRATED_FROM_MARKER).is_file());
        assert_eq!(snapshot(&new).len(), snapshot(&old).len());
    }

    #[test]
    fn verification_catches_a_corrupt_or_incomplete_copy() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("old"), base.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        fake_old_state(&old);
        let staged = stage_copy(&old, &new).unwrap();
        verify_stage(&old, &staged).unwrap();
        // One flipped byte.
        std::fs::write(
            staged.temp.join("projects/p_aaaa/secrets/vault-key"),
            "AGE-SECRET-KEY-p_aaab",
        )
        .unwrap();
        let e = verify_stage(&old, &staged).unwrap_err();
        assert!(
            matches!(e, MigrationError::Verify { .. }) && e.to_string().contains("vault-key"),
            "{e}"
        );
        // A missing file.
        std::fs::remove_file(staged.temp.join("projects/p_bbbb/action-queue/job1.json")).unwrap();
        let e = verify_stage(&old, &staged).unwrap_err().to_string();
        assert!(e.contains("job1.json") || e.contains("vault-key"), "{e}");
        // The source changing during the copy is caught too.
        let staged2 = stage_copy(&old, &base.path().join("new2")).unwrap();
        std::fs::write(
            old.join("projects/p_aaaa/config-hashes.json"),
            "{\"changed\":1}",
        )
        .unwrap();
        assert!(verify_stage(&old, &staged2).is_err());
    }

    #[test]
    fn nested_same_and_nonempty_targets_are_refused_or_noop() {
        let base = tempfile::tempdir().unwrap();
        let old = base.path().join("old");
        std::fs::create_dir_all(&old).unwrap();
        fake_old_state(&old);
        assert!(matches!(
            migrate(&old, &old.join("inside"), &ctx()).unwrap_err(),
            MigrationError::Nested { .. }
        ));
        assert!(matches!(
            migrate(&old, base.path(), &ctx()).unwrap_err(),
            MigrationError::Nested { .. }
        ));
        assert!(matches!(
            migrate(&old, &old, &ctx()).unwrap(),
            MigrationOutcome::NothingToMigrate { .. }
        ));
        let junk = base.path().join("junk");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join("unrelated.txt"), "x").unwrap();
        let e = migrate(&old, &junk, &ctx()).unwrap_err();
        assert!(matches!(e, MigrationError::TargetNotEmpty { .. }), "{e}");
        assert_eq!(
            std::fs::read_to_string(junk.join("unrelated.txt")).unwrap(),
            "x"
        );
        // An empty existing directory is fine (renamed over atomically).
        let empty = base.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            migrate(&old, &empty, &ctx()).unwrap(),
            MigrationOutcome::Migrated(_)
        ));
        // No state at all: nothing to do.
        let blank = base.path().join("blank");
        std::fs::create_dir_all(&blank).unwrap();
        assert!(matches!(
            migrate(&blank, &base.path().join("dst"), &ctx()).unwrap(),
            MigrationOutcome::NothingToMigrate { .. }
        ));
        assert!(!base.path().join("dst").exists());
    }

    #[test]
    fn already_migrated_elsewhere_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let old = base.path().join("old");
        std::fs::create_dir_all(&old).unwrap();
        fake_old_state(&old);
        migrate(&old, &base.path().join("a"), &ctx()).unwrap();
        let e = migrate(&old, &base.path().join("b"), &ctx()).unwrap_err();
        assert!(matches!(e, MigrationError::MigratedElsewhere { .. }), "{e}");
        assert!(!base.path().join("b").exists());
    }

    #[test]
    fn project_ids_and_file_custody_keys_survive_the_move() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("old"), base.path().join("new"));
        let project = tempfile::tempdir().unwrap();
        let opts = CustodyOpts::file_only();

        let ps_old = ProjectState::open(project.path(), &StateLocator::at(&old)).unwrap();
        store_secret(
            &ps_old,
            ControlSecret::VaultKey,
            "AGE-SECRET-KEY-REAL",
            opts,
        )
        .unwrap();
        store_secret(&ps_old, ControlSecret::AuditKey, "audit-hmac", opts).unwrap();
        std::fs::write(ps_old.daemon_tokens_path(), "{\"t\":1}").unwrap();
        let id = ps_old.project_id.clone();

        assert!(matches!(
            migrate(&old, &new, &ctx()).unwrap(),
            MigrationOutcome::Migrated(_)
        ));

        // The id lives in the PROJECT tree; the same project opens the new dir with the same id.
        let ps_new = ProjectState::open(project.path(), &StateLocator::at(&new)).unwrap();
        assert_eq!(
            ps_new.project_id, id,
            "project id is not derived from the state path"
        );
        assert_eq!(ps_new.dir, new.join("projects").join(&id));
        let v = load_secret(&ps_new, ControlSecret::VaultKey, opts)
            .unwrap()
            .unwrap();
        assert_eq!(v.value, "AGE-SECRET-KEY-REAL");
        assert!(
            matches!(&v.origin, crate::state::SecretOrigin::StateFile(p) if p.starts_with(&new))
        );
        let a = load_secret(&ps_new, ControlSecret::AuditKey, opts)
            .unwrap()
            .unwrap();
        assert_eq!(a.value, "audit-hmac");
        assert_eq!(
            std::fs::read_to_string(ps_new.daemon_tokens_path()).unwrap(),
            "{\"t\":1}"
        );
        // The project-path marker (project root, not state root) did not change, so no
        // "copied project" re-mint happened and the old dir still opens too.
        let ps_again = ProjectState::open(project.path(), &StateLocator::at(&old)).unwrap();
        assert_eq!(ps_again.project_id, id);
        // Keychain items are keyed `<project-id>/<secret>` only: independent of the directory.
        // (Asserted through the file-custody round trip above plus the id equality.)
    }

    #[test]
    fn automatic_migration_runs_once_for_env_or_pointer_and_never_for_the_default_or_tests() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let legacy = home.join("Library/Application Support/TrustedAutonomy");
        std::fs::create_dir_all(&legacy).unwrap();
        fake_old_state(&legacy);
        let new = base.path().join("installer-data/ta");
        let user = base.path().join("user");
        let h = host(Os::MacOs, Some(&home));

        // Default source: nothing selected, nothing to do.
        let none = env_of(&[("TA_USER_STATE_DIR", s(&user))]);
        assert!(matches!(
            ensure_migrated_with(&none, &h, &ctx()).unwrap(),
            AutoMigration::NotNeeded
        ));
        // Tests: never, even with an env override.
        let with_env = env_of(&[("TA_STATE_DIR", s(&new)), ("TA_USER_STATE_DIR", s(&user))]);
        let mut th = h.clone();
        th.test_guard = true;
        assert!(matches!(
            ensure_migrated_with(&with_env, &th, &ctx()).unwrap(),
            AutoMigration::NotNeeded
        ));
        let guarded = env_of(&[
            ("TA_STATE_DIR", s(&new)),
            ("TA_USER_STATE_DIR", s(&user)),
            (NO_EXTERNAL_AGENTS_ENV, "1".into()),
        ]);
        assert!(matches!(
            ensure_migrated_with(&guarded, &h, &ctx()).unwrap(),
            AutoMigration::NotNeeded
        ));
        assert!(!new.exists());

        // Env override: migrates once.
        match ensure_migrated_with(&with_env, &h, &ctx()).unwrap() {
            AutoMigration::Migrated(r) => assert!(Path::new(&r.to).ends_with("installer-data/ta")),
            other => panic!("{other:?}"),
        }
        assert!(new.join(MIGRATED_FROM_MARKER).is_file());
        assert!(matches!(
            ensure_migrated_with(&with_env, &h, &ctx()).unwrap(),
            AutoMigration::NotNeeded
        ));

        // Pointer source behaves the same, from a fresh legacy dir.
        let home2 = base.path().join("home2");
        let legacy2 = home2.join("Library/Application Support/TrustedAutonomy");
        std::fs::create_dir_all(&legacy2).unwrap();
        fake_old_state(&legacy2);
        let new2 = base.path().join("ptr-target");
        let h2 = host(Os::MacOs, Some(&home2));
        let user2 = base.path().join("user2");
        write_atomic_private(
            &user2.join(POINTER_FILENAME),
            format!("{}\n", new2.display()).as_bytes(),
        )
        .unwrap();
        let env2 = env_of(&[("TA_USER_STATE_DIR", s(&user2))]);
        assert!(matches!(
            ensure_migrated_with(&env2, &h2, &ctx()).unwrap(),
            AutoMigration::Migrated(_)
        ));
    }

    #[test]
    fn relocate_sets_pointer_after_migration_and_leaves_it_alone_on_refusal() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let legacy = home.join("Library/Application Support/TrustedAutonomy");
        std::fs::create_dir_all(&legacy).unwrap();
        fake_old_state(&legacy);
        let user = base.path().join("user");
        let env = env_of(&[("TA_USER_STATE_DIR", s(&user))]);
        let h = host(Os::MacOs, Some(&home));
        let new = base.path().join("vt-data/ta");

        // Refused while a daemon runs: pointer NOT written.
        let proj = base.path().join("proj");
        std::fs::create_dir_all(proj.join(".ta")).unwrap();
        std::fs::write(proj.join(".ta/daemon.pid"), "pid=77\n").unwrap();
        std::fs::write(legacy.join("projects/p_aaaa/project-path"), s(&proj)).unwrap();
        let alive = |p: u32| p == 77;
        let busy = MigrateCtx {
            is_alive: &alive,
            tool_version: "t",
        };
        assert!(matches!(
            relocate_with(&env, &h, &new, true, &busy),
            Err(RelocateError::Migration(
                MigrationError::DaemonRunning { .. }
            ))
        ));
        assert!(!user.join(POINTER_FILENAME).exists());
        assert!(!new.exists());

        // Success: migrated and pointer persisted; resolver now agrees without any env var.
        std::fs::remove_file(proj.join(".ta/daemon.pid")).unwrap();
        let out = relocate_with(&env, &h, &new, true, &ctx()).unwrap();
        assert!(out.pointer_changed);
        assert!(matches!(out.migration, Some(MigrationOutcome::Migrated(_))));
        let r = resolve_with(&env, &h).unwrap();
        assert_eq!(r.root, normalize_target(&new).unwrap());
        assert!(matches!(r.source, StateSource::Pointer { .. }));

        // Second run: no-op.
        let again = relocate_with(&env, &h, &new, true, &ctx()).unwrap();
        assert!(!again.pointer_changed);
        assert!(
            again.migration.is_none(),
            "this process already uses the target"
        );
        // Pointer deleted, legacy marker remains: still a no-op migration, pointer restored.
        std::fs::remove_file(user.join(POINTER_FILENAME)).unwrap();
        let third = relocate_with(&env, &h, &new, true, &ctx()).unwrap();
        assert!(third.pointer_changed);
        assert!(matches!(
            third.migration,
            Some(MigrationOutcome::AlreadyMigrated { .. })
        ));

        // An env override pointing elsewhere is reported.
        let env_other = env_of(&[
            ("TA_USER_STATE_DIR", s(&user)),
            ("TA_STATE_DIR", s(&base.path().join("elsewhere"))),
        ]);
        let o = relocate_with(&env_other, &h, &new, false, &ctx()).unwrap();
        assert!(o.env_override.is_some());
    }

    #[test]
    fn relocate_no_migrate_and_conflict() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let legacy = home.join("Library/Application Support/TrustedAutonomy");
        std::fs::create_dir_all(&legacy).unwrap();
        fake_old_state(&legacy);
        let user = base.path().join("user");
        let env = env_of(&[("TA_USER_STATE_DIR", s(&user))]);
        let h = host(Os::MacOs, Some(&home));
        let new = base.path().join("fresh");
        // New dir already has its own state: conflict, pointer untouched.
        std::fs::create_dir_all(new.join("projects/p_zzzz")).unwrap();
        let e = relocate_with(&env, &h, &new, true, &ctx()).unwrap_err();
        assert!(
            matches!(e, RelocateError::Migration(MigrationError::Conflict { .. })),
            "{e}"
        );
        assert!(!user.join(POINTER_FILENAME).exists());
        // --no-migrate adopts it as is.
        let out = relocate_with(&env, &h, &new, false, &ctx()).unwrap();
        assert!(out.migration.is_none() && out.pointer_changed);
        assert!(!legacy.join(MIGRATED_TO_MARKER).exists());
    }

    #[test]
    fn diagnosis_reports_divergence_and_legacy_state() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let legacy = home.join("Library/Application Support/TrustedAutonomy");
        std::fs::create_dir_all(&legacy).unwrap();
        fake_old_state(&legacy);
        let user = base.path().join("user");
        let h = host(Os::MacOs, Some(&home));
        let a = base.path().join("a");
        let b = base.path().join("b");
        write_atomic_private(
            &user.join(POINTER_FILENAME),
            format!("{}\n", a.display()).as_bytes(),
        )
        .unwrap();

        let env = env_of(&[("TA_USER_STATE_DIR", s(&user)), ("TA_STATE_DIR", s(&b))]);
        let d = diagnose_with(&env, &h);
        assert!(d.diverged);
        assert_eq!(d.resolved.as_ref().unwrap().source, StateSource::Env);
        assert!(matches!(d.legacy, LegacyStatus::Unmigrated(_)));

        let env_same = env_of(&[("TA_USER_STATE_DIR", s(&user)), ("TA_STATE_DIR", s(&a))]);
        assert!(!diagnose_with(&env_same, &h).diverged);

        // Default source: legacy IS the resolved dir.
        let plain = env_of(&[("TA_USER_STATE_DIR", s(&base.path().join("nouser")))]);
        let d = diagnose_with(&plain, &h);
        assert!(matches!(d.legacy, LegacyStatus::NotApplicable));

        // After migrating, the kept old copy is reported with its size.
        migrate(&legacy, &a, &ctx()).unwrap();
        let d = diagnose_with(&env_same, &h);
        assert!(matches!(d.legacy, LegacyStatus::MigratedKept { .. }));
        // Both have state, unlinked: needs a decision.
        std::fs::remove_file(legacy.join(MIGRATED_TO_MARKER)).unwrap();
        let d = diagnose_with(&env_same, &h);
        assert!(matches!(d.legacy, LegacyStatus::BothHaveState { .. }));
        // A bad pointer is surfaced, not hidden.
        std::fs::write(user.join(POINTER_FILENAME), "nope").unwrap();
        let d = diagnose_with(&env_of(&[("TA_USER_STATE_DIR", s(&user))]), &h);
        assert!(d.pointer_error.is_some() && d.resolved.is_err());
    }

    #[test]
    fn shell_quote_survives_spaces_and_quotes() {
        assert_eq!(
            shell_quote(Path::new(
                "/Users/me/Library/Application Support/TrustedAutonomy"
            )),
            "'/Users/me/Library/Application Support/TrustedAutonomy'"
        );
        assert_eq!(shell_quote(Path::new("/a'b")), r"'/a'\''b'");
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1536), "1.5 KiB");
    }
}
