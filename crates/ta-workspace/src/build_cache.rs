//! Shared cargo build cache for goal agents.
//!
//! Every goal used to compile the whole workspace from scratch into its own
//! `<staging>/target`. Goals of one project now build into a single cache
//! outside staging: `<project>/.ta/build-cache/<key>/target`.
//!
//! The key names everything cargo's artifacts depend on that cargo itself does
//! not separate cleanly: the compiler (`rustc -vV`), the host target triple,
//! `RUSTFLAGS`, and the *dependencies* in `Cargo.lock` (registry and git
//! packages, not the workspace's own crates, whose version changes every phase).
//! A different key is a different directory, so a toolchain or dependency
//! change can never reuse stale artifacts, and an old key is evicted
//! least-recently-used when the cache exceeds its cap.
//!
//! Isolation:
//! * The cache lives under `.ta/`, which staging copies never include, so a
//!   goal's build output cannot enter a staging diff or a draft.
//! * The file connector refuses to read it ([`is_cache_path`]).
//! * It is per project: the directory is inside the project's own `.ta/`.
//!
//! Cargo's own file lock serializes concurrent builds in one target directory.
//! [`register_holder`] records which live goal uses a key so a goal that has to
//! wait can say who it is waiting for.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Cache location relative to the project root.
pub const BUILD_CACHE_REL: &str = ".ta/build-cache";
/// File whose modification time is the least-recently-used clock of a key.
const LAST_USED_FILE: &str = "last_used";
/// Directory of one file per live goal using the key (`<goal id>` -> pid).
const HOLDERS_DIR: &str = "holders";
/// Per-goal measurements, one JSON object per line.
const MEASUREMENTS_FILE: &str = "measurements.jsonl";

/// `<project>/.ta/build-cache`.
pub fn cache_root(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("build-cache")
}

/// True when `relative_path` (relative to the project root) is the build cache
/// or inside it. Used to keep it out of reach of the agent's file tools.
pub fn is_cache_path(relative_path: &str) -> bool {
    let normalized = relative_path.replace('\\', "/");
    let mut parts = normalized.split('/').filter(|p| !p.is_empty() && *p != ".");
    matches!(
        (parts.next(), parts.next()),
        (Some(a), Some(b)) if a.eq_ignore_ascii_case(".ta") && b.eq_ignore_ascii_case("build-cache")
    )
}

/// The facts the key is derived from, captured once when a goal starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainFacts {
    /// Output of `rustc -vV` (release, commit hash, host triple, LLVM version).
    pub rustc_vv: String,
    /// `RUSTFLAGS` (empty when unset).
    pub rustflags: String,
}

impl ToolchainFacts {
    /// Ask the compiler that cargo would use in `dir` (so a `rust-toolchain.toml`
    /// in the project is honoured). `RUSTC` overrides the program like cargo does.
    pub fn probe(dir: &Path) -> Result<Self, String> {
        let program = std::env::var("RUSTC")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "rustc".to_string());
        let output = std::process::Command::new(&program)
            .arg("-vV")
            .current_dir(dir)
            .output()
            .map_err(|e| format!("could not run `{program} -vV` in {}: {e}", dir.display()))?;
        if !output.status.success() {
            return Err(format!(
                "`{program} -vV` failed in {} ({}): {}",
                dir.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(Self {
            rustc_vv: String::from_utf8_lossy(&output.stdout).into_owned(),
            rustflags: std::env::var("RUSTFLAGS").unwrap_or_default(),
        })
    }

    /// The `host:` line of `rustc -vV`.
    pub fn host_triple(&self) -> Option<&str> {
        self.rustc_vv
            .lines()
            .find_map(|l| l.strip_prefix("host:"))
            .map(str::trim)
            .filter(|t| !t.is_empty())
    }
}

/// Digest of the third-party dependencies pinned in a `Cargo.lock`: every
/// `[[package]]` with a `source` (registry or git), by name, version, source and
/// checksum. Workspace crates (no `source`) are ignored so a version bump of the
/// project itself does not invalidate the cache.
pub fn lock_dependency_digest(cargo_lock: &str) -> String {
    let mut deps: Vec<String> = Vec::new();
    let (mut name, mut version, mut source, mut checksum) = (None, None, None, None);
    let mut flush = |name: &mut Option<String>,
                     version: &mut Option<String>,
                     source: &mut Option<String>,
                     checksum: &mut Option<String>| {
        if let (Some(n), Some(v), Some(s)) = (name.take(), version.take(), source.take()) {
            deps.push(format!(
                "{n} {v} {s} {}",
                checksum.take().unwrap_or_default()
            ));
        }
        *name = None;
        *version = None;
        *source = None;
        *checksum = None;
    };
    for line in cargo_lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            flush(&mut name, &mut version, &mut source, &mut checksum);
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "name" => name = Some(v),
            "version" => version = Some(v),
            "source" => source = Some(v),
            "checksum" => checksum = Some(v),
            _ => {}
        }
    }
    flush(&mut name, &mut version, &mut source, &mut checksum);
    deps.sort();
    let mut h = Sha256::new();
    for d in &deps {
        h.update(d.as_bytes());
        h.update(b"\n");
    }
    hex16(&h.finalize())
}

/// `<host triple>-<16 hex>`: readable in `ta doctor`, unique per toolchain,
/// flags and dependency set.
pub fn cache_key(facts: &ToolchainFacts, lock_digest: &str) -> String {
    let mut h = Sha256::new();
    h.update(facts.rustc_vv.as_bytes());
    h.update(b"\0");
    h.update(facts.rustflags.as_bytes());
    h.update(b"\0");
    h.update(lock_digest.as_bytes());
    let digest = hex16(&h.finalize());
    match facts.host_triple() {
        Some(t) => format!("{t}-{digest}"),
        None => digest,
    }
}

fn hex16(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// A resolved cache location for one goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub key: String,
    /// `<root>/<key>`; holds `target/`, `holders/` and the LRU marker.
    pub key_dir: PathBuf,
    /// The directory to hand to cargo as `CARGO_TARGET_DIR`.
    pub target_dir: PathBuf,
}

/// Work out the cache for a goal of `project_root` whose source tree is
/// `source_dir` (where `Cargo.lock` lives). Creates the directories and touches
/// the key's LRU marker. Computed once per goal: a dependency added mid-goal
/// must not move the gates to a different directory than the agent used.
pub fn select(
    project_root: &Path,
    source_dir: &Path,
    facts: &ToolchainFacts,
) -> Result<Selection, String> {
    let lock = std::fs::read_to_string(source_dir.join("Cargo.lock")).unwrap_or_default();
    let key = cache_key(facts, &lock_dependency_digest(&lock));
    let key_dir = cache_root(project_root).join(&key);
    let target_dir = key_dir.join("target");
    std::fs::create_dir_all(&target_dir).map_err(|e| {
        format!(
            "could not create the build cache directory {}: {e}",
            target_dir.display()
        )
    })?;
    touch(&key_dir);
    Ok(Selection {
        key,
        key_dir,
        target_dir,
    })
}

/// Mark a key as used now (the least-recently-used clock).
pub fn touch(key_dir: &Path) {
    let _ = std::fs::write(key_dir.join(LAST_USED_FILE), b"");
    // Rewriting an existing file does not always move its mtime on every
    // filesystem; set it explicitly as well.
    if let Ok(f) = std::fs::OpenOptions::new()
        .write(true)
        .open(key_dir.join(LAST_USED_FILE))
    {
        let _ = f.set_modified(SystemTime::now());
    }
}

// ── Concurrent goals ────────────────────────────────────────────────────────

/// Removes the goal's holder record when dropped.
#[derive(Debug)]
pub struct HolderGuard {
    path: PathBuf,
}

impl Drop for HolderGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Record that `goal_id` (owned by process `pid`) builds in this key.
pub fn register_holder(key_dir: &Path, goal_id: &str, pid: u32) -> std::io::Result<HolderGuard> {
    let dir = key_dir.join(HOLDERS_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(goal_id);
    std::fs::write(&path, pid.to_string())?;
    Ok(HolderGuard { path })
}

/// Other goals whose owning process is alive and which use this key.
pub fn live_holders(key_dir: &Path, except_goal: &str) -> Vec<(String, u32)> {
    let Ok(entries) = std::fs::read_dir(key_dir.join(HOLDERS_DIR)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let goal = e.file_name().to_string_lossy().into_owned();
        if goal == except_goal {
            continue;
        }
        let pid = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        match pid {
            Some(pid) if ta_lifecycle::process_is_alive(pid) => out.push((goal, pid)),
            // Left behind by a process that died: drop it so the key can be evicted.
            _ => {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    out.sort();
    out
}

// ── Size management ─────────────────────────────────────────────────────────

/// One toolchain key in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub key: String,
    pub dir: PathBuf,
    pub bytes: u64,
    pub last_used: SystemTime,
    /// Live goals currently building in it.
    pub holders: Vec<(String, u32)>,
}

/// Every key in the cache, least recently used first.
pub fn list_entries(project_root: &Path) -> Vec<CacheEntry> {
    let Ok(rd) = std::fs::read_dir(cache_root(project_root)) else {
        return Vec::new();
    };
    let mut out: Vec<CacheEntry> = rd
        .flatten()
        .filter(|e| e.path().join("target").is_dir())
        .map(|e| {
            let dir = e.path();
            let last_used = std::fs::metadata(dir.join(LAST_USED_FILE))
                .and_then(|m| m.modified())
                .or_else(|_| std::fs::metadata(&dir).and_then(|m| m.modified()))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            CacheEntry {
                key: e.file_name().to_string_lossy().into_owned(),
                bytes: dir_size(&dir),
                holders: live_holders(&dir, ""),
                last_used,
                dir,
            }
        })
        .collect();
    out.sort_by_key(|e| e.last_used);
    out
}

/// What [`evict_to_cap`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictReport {
    pub cap_bytes: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// `(key, bytes)` removed, in the order they were removed.
    pub evicted: Vec<(String, u64)>,
    /// Keys over the cap that were kept because a live goal builds in them.
    pub kept_in_use: Vec<String>,
    pub errors: Vec<String>,
}

/// Remove least-recently-used keys until the cache fits `cap_bytes`. A key a
/// live goal builds in, and `protect`, are never removed. `cap_bytes == 0`
/// means no cap.
pub fn evict_to_cap(project_root: &Path, cap_bytes: u64, protect: Option<&str>) -> EvictReport {
    let entries = list_entries(project_root);
    let total: u64 = entries.iter().map(|e| e.bytes).sum();
    let mut report = EvictReport {
        cap_bytes,
        bytes_before: total,
        bytes_after: total,
        ..Default::default()
    };
    if cap_bytes == 0 || total <= cap_bytes {
        return report;
    }
    for e in entries {
        if report.bytes_after <= cap_bytes {
            break;
        }
        if !e.holders.is_empty() || protect == Some(e.key.as_str()) {
            report.kept_in_use.push(e.key);
            continue;
        }
        match std::fs::remove_dir_all(&e.dir) {
            Ok(()) => {
                report.bytes_after = report.bytes_after.saturating_sub(e.bytes);
                report.evicted.push((e.key, e.bytes));
            }
            Err(err) => report
                .errors
                .push(format!("could not remove {}: {err}", e.dir.display())),
        }
    }
    report
}

/// Remove one key (`ta doctor --remove`/cleanup of a named entry). Refuses a key
/// a live goal builds in.
pub fn remove_key(project_root: &Path, key: &str) -> Result<u64, String> {
    if key.is_empty() || key.contains(['/', '\\']) || key == ".." || key == "." {
        return Err(format!("'{key}' is not a build cache key"));
    }
    let dir = cache_root(project_root).join(key);
    if !dir.join("target").is_dir() {
        return Err(format!(
            "no build cache key '{key}' under {}",
            cache_root(project_root).display()
        ));
    }
    let holders = live_holders(&dir, "");
    if let Some((goal, pid)) = holders.first() {
        return Err(format!(
            "build cache key '{key}' is in use by goal {goal} (process {pid}); wait for it to finish"
        ));
    }
    let bytes = dir_size(&dir);
    std::fs::remove_dir_all(&dir)
        .map_err(|e| format!("could not remove {}: {e}", dir.display()))?;
    Ok(bytes)
}

/// Total bytes under `dir`, not following symlinks.
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

// ── Measurement ─────────────────────────────────────────────────────────────

/// What one goal's agent session cost to build, for comparing goals that used
/// the shared cache with goals that did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measurement {
    pub goal_id: String,
    pub recorded_at: String,
    /// `Some(key)` when the goal built in the shared cache.
    pub cache_key: Option<String>,
    /// Wall time from agent launch to agent exit.
    pub agent_wall_secs: u64,
    /// Build output on disk when the agent launched (the cache key's size, or 0
    /// for a fresh per-goal `target/`).
    pub build_output_bytes_before: u64,
    /// Build output on disk when the agent exited.
    pub build_output_bytes_after: u64,
}

/// Append a measurement. Best effort: a failure is reported to the caller, which
/// logs it; it never blocks the goal.
pub fn record_measurement(project_root: &Path, m: &Measurement) -> std::io::Result<()> {
    use std::io::Write;
    let root = cache_root(project_root);
    std::fs::create_dir_all(&root)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(MEASUREMENTS_FILE))?;
    let line = serde_json::to_string(m).map_err(std::io::Error::other)?;
    writeln!(f, "{line}")
}

/// All recorded measurements, oldest first. Unreadable lines are skipped.
pub fn read_measurements(project_root: &Path) -> Vec<Measurement> {
    std::fs::read_to_string(cache_root(project_root).join(MEASUREMENTS_FILE))
        .map(|s| {
            s.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Human age such as `3d` or `5h` for a key's last use.
pub fn human_age(t: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(t)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    match secs {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn facts(rustc: &str, flags: &str) -> ToolchainFacts {
        ToolchainFacts {
            rustc_vv: format!(
                "rustc {rustc} (abc 2026-01-01)\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: {rustc}\n"
            ),
            rustflags: flags.to_string(),
        }
    }

    const LOCK_A: &str = r#"
[[package]]
name = "serde"
version = "1.0.1"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaa"

[[package]]
name = "myproj"
version = "0.1.0"
dependencies = ["serde"]
"#;

    fn lock_with_workspace_version(v: &str) -> String {
        LOCK_A.replace("0.1.0", v)
    }

    #[test]
    fn key_changes_with_toolchain_flags_and_dependencies_but_not_workspace_version() {
        let digest = lock_dependency_digest(LOCK_A);
        let base = cache_key(&facts("1.90.0", ""), &digest);
        assert!(base.starts_with("aarch64-apple-darwin-"), "{base}");

        assert_ne!(base, cache_key(&facts("1.91.0", ""), &digest), "toolchain");
        assert_ne!(
            base,
            cache_key(&facts("1.90.0", "-C target-cpu=native"), &digest),
            "RUSTFLAGS"
        );
        let bumped_dep = LOCK_A.replace("1.0.1", "1.0.2");
        assert_ne!(
            base,
            cache_key(&facts("1.90.0", ""), &lock_dependency_digest(&bumped_dep)),
            "dependency version"
        );
        let changed_checksum = LOCK_A.replace("aaa", "bbb");
        assert_ne!(
            base,
            cache_key(
                &facts("1.90.0", ""),
                &lock_dependency_digest(&changed_checksum)
            ),
            "dependency checksum"
        );

        // The project's own version bumps every phase; it must keep the cache.
        let bumped_workspace = lock_with_workspace_version("0.2.0");
        assert_eq!(
            base,
            cache_key(
                &facts("1.90.0", ""),
                &lock_dependency_digest(&bumped_workspace)
            )
        );
        // Same inputs, same key.
        assert_eq!(base, cache_key(&facts("1.90.0", ""), &digest));
    }

    #[test]
    fn dependency_order_in_the_lock_does_not_change_the_key() {
        let a = "[[package]]\nname = \"a\"\nversion = \"1\"\nsource = \"s\"\n\n[[package]]\nname = \"b\"\nversion = \"2\"\nsource = \"s\"\n";
        let b = "[[package]]\nname = \"b\"\nversion = \"2\"\nsource = \"s\"\n\n[[package]]\nname = \"a\"\nversion = \"1\"\nsource = \"s\"\n";
        assert_eq!(lock_dependency_digest(a), lock_dependency_digest(b));
    }

    #[test]
    fn two_goals_of_one_project_share_one_cache_directory() {
        let project = tempfile::tempdir().unwrap();
        let src = project.path();
        fs::write(src.join("Cargo.lock"), LOCK_A).unwrap();
        let f = facts("1.90.0", "");

        let first = select(project.path(), src, &f).unwrap();
        // A file the first goal's build left behind.
        fs::write(first.target_dir.join("dep.rlib"), b"compiled").unwrap();
        let second = select(project.path(), src, &f).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            fs::read(second.target_dir.join("dep.rlib")).unwrap(),
            b"compiled"
        );
        assert_eq!(
            first.target_dir,
            project
                .path()
                .join(".ta/build-cache")
                .join(&first.key)
                .join("target")
        );
        // Never the source tree's own `target/`.
        assert_ne!(first.target_dir, src.join("target"));
    }

    #[test]
    fn different_toolchains_get_different_directories() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("Cargo.lock"), LOCK_A).unwrap();
        let a = select(project.path(), project.path(), &facts("1.90.0", "")).unwrap();
        let b = select(project.path(), project.path(), &facts("1.91.0", "")).unwrap();
        assert_ne!(a.target_dir, b.target_dir);
    }

    fn make_key(project: &Path, key: &str, bytes: usize, age_secs: u64) {
        let dir = cache_root(project).join(key);
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target/blob"), vec![0u8; bytes]).unwrap();
        touch(&dir);
        let f = fs::OpenOptions::new()
            .write(true)
            .open(dir.join(LAST_USED_FILE))
            .unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_secs))
            .unwrap();
    }

    #[test]
    fn eviction_removes_least_recently_used_keys_until_under_the_cap() {
        let project = tempfile::tempdir().unwrap();
        make_key(project.path(), "old", 1000, 3000);
        make_key(project.path(), "mid", 1000, 2000);
        make_key(project.path(), "new", 1000, 1000);

        let report = evict_to_cap(project.path(), 1500, None);

        assert_eq!(report.bytes_before, 3000);
        let evicted: Vec<&str> = report.evicted.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(evicted, ["old", "mid"]);
        assert_eq!(report.bytes_after, 1000);
        assert!(cache_root(project.path()).join("new").exists());
        assert!(!cache_root(project.path()).join("old").exists());
    }

    #[test]
    fn eviction_never_removes_a_key_a_live_goal_builds_in_or_the_protected_key() {
        let project = tempfile::tempdir().unwrap();
        make_key(project.path(), "busy", 1000, 9000);
        make_key(project.path(), "protected", 1000, 8000);
        make_key(project.path(), "idle", 1000, 7000);
        let busy_dir = cache_root(project.path()).join("busy");
        let _guard = register_holder(&busy_dir, "goal-1", std::process::id()).unwrap();

        let report = evict_to_cap(project.path(), 1, Some("protected"));

        assert_eq!(report.evicted.len(), 1);
        assert_eq!(report.evicted[0].0, "idle");
        assert_eq!(report.kept_in_use, ["busy", "protected"]);
        assert!(busy_dir.exists());
    }

    #[test]
    fn cap_zero_means_no_cap_and_under_cap_removes_nothing() {
        let project = tempfile::tempdir().unwrap();
        make_key(project.path(), "k", 1000, 10);
        assert!(evict_to_cap(project.path(), 0, None).evicted.is_empty());
        assert!(evict_to_cap(project.path(), 5000, None).evicted.is_empty());
        assert!(cache_root(project.path()).join("k").exists());
    }

    #[test]
    fn holders_of_dead_processes_are_ignored_and_cleaned() {
        let project = tempfile::tempdir().unwrap();
        make_key(project.path(), "k", 10, 10);
        let dir = cache_root(project.path()).join("k");
        let _live = register_holder(&dir, "live-goal", std::process::id()).unwrap();
        let dead = register_holder(&dir, "dead-goal", u32::MAX - 1).unwrap();
        std::mem::forget(dead); // leave the record behind like a crashed process

        let holders = live_holders(&dir, "me");
        assert_eq!(holders, vec![("live-goal".to_string(), std::process::id())]);
        assert!(!dir.join("holders/dead-goal").exists());
        // A goal does not wait for itself.
        assert!(live_holders(&dir, "live-goal").is_empty());
    }

    #[test]
    fn remove_key_refuses_unknown_keys_path_tricks_and_busy_keys() {
        let project = tempfile::tempdir().unwrap();
        make_key(project.path(), "k", 100, 10);
        assert!(remove_key(project.path(), "../k").is_err());
        assert!(remove_key(project.path(), "missing").is_err());
        let dir = cache_root(project.path()).join("k");
        let guard = register_holder(&dir, "g", std::process::id()).unwrap();
        let err = remove_key(project.path(), "k").unwrap_err();
        assert!(err.contains("in use by goal g"), "{err}");
        drop(guard);
        assert_eq!(remove_key(project.path(), "k").unwrap(), 100);
    }

    #[test]
    fn cache_paths_are_recognised_in_every_spelling() {
        for p in [
            ".ta/build-cache",
            ".ta/build-cache/k/target/debug/x",
            "./.ta/build-cache/k",
            ".ta\\build-cache\\k",
            ".TA/Build-Cache/k",
        ] {
            assert!(is_cache_path(p), "{p}");
        }
        for p in ["src/lib.rs", ".ta/workflow.toml", ".ta", "build-cache/x"] {
            assert!(!is_cache_path(p), "{p}");
        }
    }

    /// The cache sits under `.ta/`; staging copies never include `.ta/`, so a
    /// goal's build output cannot appear in its staging tree or in a draft diff.
    #[test]
    fn cache_is_not_copied_into_staging_and_never_appears_in_a_diff() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("lib.rs"), "fn a() {}").unwrap();
        fs::write(project.path().join("Cargo.lock"), LOCK_A).unwrap();
        let sel = select(project.path(), project.path(), &facts("1.90.0", "")).unwrap();
        fs::create_dir_all(sel.target_dir.join("debug")).unwrap();
        fs::write(sel.target_dir.join("debug/libdep.rlib"), vec![1u8; 4096]).unwrap();

        let staging_root = project.path().join(".ta/staging");
        let ws = crate::OverlayWorkspace::create(
            "goal-x",
            project.path(),
            &staging_root,
            crate::ExcludePatterns::load(project.path()),
        )
        .unwrap();

        assert!(!ws.staging_dir().join(".ta").exists());
        assert!(ws.staging_dir().join("lib.rs").exists());
        // The goal builds: output goes to the cache, never into staging.
        let changes = ws.diff_all().unwrap();
        assert!(changes.is_empty(), "unexpected changes: {changes:?}");
    }

    #[test]
    fn measurements_round_trip_in_recording_order() {
        let project = tempfile::tempdir().unwrap();
        for (i, key) in [Some("k"), None].into_iter().enumerate() {
            record_measurement(
                project.path(),
                &Measurement {
                    goal_id: format!("g{i}"),
                    recorded_at: "2026-10-17T00:00:00Z".into(),
                    cache_key: key.map(str::to_string),
                    agent_wall_secs: 60 + i as u64,
                    build_output_bytes_before: 10,
                    build_output_bytes_after: 20,
                },
            )
            .unwrap();
        }
        let got = read_measurements(project.path());
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].goal_id, "g0");
        assert_eq!(got[0].cache_key.as_deref(), Some("k"));
        assert_eq!(got[1].cache_key, None);
        // A measurements file does not look like a cache key.
        assert!(list_entries(project.path()).is_empty());
    }
}
