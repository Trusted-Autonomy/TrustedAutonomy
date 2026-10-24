// build_cache.rs — CLI side of the shared goal build cache.
//
// Goal agents build Rust projects into one cache per project
// (`.ta/build-cache/<key>/target`, see `ta_workspace::build_cache`) instead of
// compiling the whole workspace into every goal's staging directory. This module
// decides, per goal, whether the cache is used and why not when it is not; hands
// the agent and the verification gates the same `CARGO_TARGET_DIR`; says who a
// goal waits for when another goal is building; records what each goal's build
// cost; and reports the cache in `ta doctor`.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use ta_mcp_gateway::GatewayConfig;
use ta_submit::config::StagingConfig;
use ta_workspace::build_cache as cache;

thread_local! {
    static NO_BUILD_CACHE: Cell<bool> = const { Cell::new(false) };
}

/// Scoped carrier for `ta run --no-build-cache`; see `KeepBuildOutputGuard`.
pub struct NoBuildCacheGuard {
    previous: bool,
}

impl NoBuildCacheGuard {
    pub fn set(disabled: bool) -> Self {
        let previous = NO_BUILD_CACHE.with(|c| c.replace(disabled));
        Self { previous }
    }
}

impl Drop for NoBuildCacheGuard {
    fn drop(&mut self) {
        NO_BUILD_CACHE.with(|c| c.set(self.previous));
    }
}

fn no_build_cache_requested() -> bool {
    NO_BUILD_CACHE.with(|c| c.get())
}

/// A goal that builds in the shared cache.
#[derive(Debug)]
pub struct CacheUse {
    pub selection: cache::Selection,
    /// Other live goals building in the same key: cargo serializes on its build
    /// directory lock, so this goal's builds wait for them.
    pub waiting_on: Vec<(String, u32)>,
    pub eviction: cache::EvictReport,
    pub bytes_before: u64,
    _holder: Option<cache::HolderGuard>,
}

/// Decide whether a goal uses the shared cache and set it up. `Err(reason)` says
/// why it does not (the goal then builds in its own staging `target/`).
pub fn plan_cache_use(
    project_root: &Path,
    source_dir: &Path,
    goal_id: &str,
    staging: &StagingConfig,
    disabled_by_flag: bool,
    facts: Result<cache::ToolchainFacts, String>,
) -> Result<CacheUse, String> {
    if disabled_by_flag {
        return Err("--no-build-cache was passed".to_string());
    }
    if !staging.build_cache {
        return Err("`[staging] build_cache = false` in .ta/workflow.toml".to_string());
    }
    let facts = facts.map_err(|e| format!("the Rust toolchain could not be identified: {e}"))?;
    let selection = cache::select(project_root, source_dir, &facts)?;

    let cap_bytes = if staging.build_cache_max_gb > 0.0 {
        (staging.build_cache_max_gb * 1_073_741_824.0) as u64
    } else {
        0
    };
    let eviction = cache::evict_to_cap(project_root, cap_bytes, Some(&selection.key));
    let holder = match cache::register_holder(&selection.key_dir, goal_id, std::process::id()) {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(
                goal_id,
                key = %selection.key,
                error = %e,
                "could not record this goal as a user of the build cache; other goals will not see it waiting"
            );
            None
        }
    };
    let waiting_on = cache::live_holders(&selection.key_dir, goal_id);
    let bytes_before = cache::dir_size(&selection.key_dir);
    Ok(CacheUse {
        selection,
        waiting_on,
        eviction,
        bytes_before,
        _holder: holder,
    })
}

/// What `ta run` remembers about one goal's build directory.
struct GoalBuild {
    /// The shared cache, when the goal uses it.
    cache: Option<CacheUse>,
    /// Where the agent builds when it does not (`<staging>/target`).
    own_target: PathBuf,
    /// False once a sandboxed agent was sent back to `own_target`.
    agent_in_cache: bool,
}

type Registry = Mutex<HashMap<PathBuf, GoalBuild>>;

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn registry() -> &'static Registry {
    &REGISTRY
}

/// Choose and announce the cargo target directory for a goal agent. Called once
/// per goal, before the agent starts. Returns the directory for
/// `CARGO_TARGET_DIR`.
pub fn start_goal_build(
    config: &GatewayConfig,
    goal_id: uuid::Uuid,
    staging_path: &Path,
    source_dir: &Path,
) -> PathBuf {
    let wf = ta_submit::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let own_target = super::verify::goal_target_dir(staging_path);
    let id = goal_id.to_string();
    let facts = cache::ToolchainFacts::probe(source_dir);
    match plan_cache_use(
        &config.workspace_root,
        source_dir,
        &id,
        &wf.staging,
        no_build_cache_requested(),
        facts,
    ) {
        Ok(used) => {
            let target = used.selection.target_dir.clone();
            println!(
                "Build cache: {} ({} cached; shared by this project's goals, cap {}).",
                used.selection.key,
                super::staging_reclaim::format_bytes(used.bytes_before),
                if wf.staging.build_cache_max_gb > 0.0 {
                    format!("{} GB", wf.staging.build_cache_max_gb)
                } else {
                    "none".to_string()
                },
            );
            for (key, bytes) in &used.eviction.evicted {
                println!(
                    "Build cache: evicted unused toolchain key {key} ({}) to stay under the cap.",
                    super::staging_reclaim::format_bytes(*bytes)
                );
            }
            if !used.eviction.kept_in_use.is_empty()
                && used.eviction.bytes_after > used.eviction.cap_bytes
            {
                tracing::warn!(
                    cap_bytes = used.eviction.cap_bytes,
                    bytes = used.eviction.bytes_after,
                    kept = ?used.eviction.kept_in_use,
                    "build cache is over its cap but the remaining keys are in use; raise `[staging] build_cache_max_gb` or run `ta cleanup`"
                );
            }
            for e in &used.eviction.errors {
                tracing::warn!(error = %e, "build cache eviction problem; run `ta doctor` to see the cache");
            }
            for (other, pid) in &used.waiting_on {
                println!(
                    "Build cache: waiting for build lock held by goal {other} (process {pid}). \
                     Cargo builds in this goal wait until that goal's build finishes \
                     (`--no-build-cache` builds in a private directory instead)."
                );
                tracing::info!(
                    goal_id = %goal_id,
                    key = %used.selection.key,
                    held_by = %other,
                    holder_pid = pid,
                    "goal shares a build cache key with a running goal; cargo serializes their builds"
                );
            }
            tracing::info!(
                goal_id = %goal_id,
                key = %used.selection.key,
                target_dir = %target.display(),
                cached_bytes = used.bytes_before,
                "Using the shared build cache"
            );
            registry().lock().unwrap_or_else(|p| p.into_inner()).insert(
                staging_path.to_path_buf(),
                GoalBuild {
                    cache: Some(used),
                    own_target,
                    agent_in_cache: true,
                },
            );
            target
        }
        Err(reason) => {
            tracing::info!(
                goal_id = %goal_id,
                reason = %reason,
                target_dir = %own_target.display(),
                decision = "per_goal_target",
                "Build cache not used for this goal"
            );
            println!(
                "Build cache not used for this goal: {reason}. Building in {} (removed when the agent exits).",
                own_target.display()
            );
            registry().lock().unwrap_or_else(|p| p.into_inner()).insert(
                staging_path.to_path_buf(),
                GoalBuild {
                    cache: None,
                    own_target: own_target.clone(),
                    agent_in_cache: false,
                },
            );
            own_target
        }
    }
}

/// The shared cache directory the verification gates for this goal should use,
/// when `start_goal_build` chose it. `None` leaves them on `<staging>/target`.
pub fn gate_target_dir(staging_path: &Path) -> Option<PathBuf> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(staging_path)
        .and_then(|g| g.cache.as_ref())
        .map(|c| c.selection.target_dir.clone())
}

/// A sandboxed agent cannot write to `.ta/` (a protected path), so it cannot use
/// the cache. Point it back at its own staging `target/`. Returns `true` when it
/// changed `CARGO_TARGET_DIR`.
pub fn use_own_target_in_sandbox(
    env: &mut HashMap<String, String>,
    project_root: &Path,
    staging_path: &Path,
    goal_id: uuid::Uuid,
) -> bool {
    let Some(current) = env.get("CARGO_TARGET_DIR").map(PathBuf::from) else {
        return false;
    };
    if !current.starts_with(cache::cache_root(project_root)) {
        return false;
    }
    let own = super::verify::goal_target_dir(staging_path);
    env.insert("CARGO_TARGET_DIR".to_string(), own.display().to_string());
    if let Some(g) = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_mut(staging_path)
    {
        // The gates still use the cache; only the agent's own builds moved.
        g.agent_in_cache = false;
    }
    tracing::info!(
        goal_id = %goal_id,
        target_dir = %own.display(),
        decision = "per_goal_target",
        "sandboxed agent cannot write to .ta/build-cache; building in its own staging target"
    );
    println!(
        "Build cache not used by the sandboxed agent (it may not write under .ta/). \
         Building in {} (removed when the agent exits); verification gates still use the cache.",
        own.display()
    );
    true
}

/// Record what the agent's builds cost. Call once when the agent exits.
pub fn record_agent_exit(
    config: &GatewayConfig,
    goal_id: uuid::Uuid,
    staging_path: &Path,
    agent_wall: Duration,
) {
    let (key, before, after) = {
        let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        let Some(g) = reg.get(staging_path) else {
            return;
        };
        match g.cache.as_ref().filter(|_| g.agent_in_cache) {
            Some(c) => {
                cache::touch(&c.selection.key_dir);
                (
                    Some(c.selection.key.clone()),
                    c.bytes_before,
                    cache::dir_size(&c.selection.key_dir),
                )
            }
            None => (None, 0, cache::dir_size(&g.own_target)),
        }
    };
    let m = cache::Measurement {
        goal_id: goal_id.to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
        cache_key: key,
        agent_wall_secs: agent_wall.as_secs(),
        build_output_bytes_before: before,
        build_output_bytes_after: after,
    };
    tracing::info!(
        goal_id = %goal_id,
        cache_key = ?m.cache_key,
        agent_wall_secs = m.agent_wall_secs,
        build_output_bytes_before = before,
        build_output_bytes_after = after,
        "Recorded goal build measurement"
    );
    if let Err(e) = cache::record_measurement(&config.workspace_root, &m) {
        tracing::warn!(
            goal_id = %goal_id,
            error = %e,
            "could not record the build measurement in .ta/build-cache/measurements.jsonl; `ta doctor` will not list this goal"
        );
    }
}

/// The goal's builds are over: stop being counted as a user of the cache.
pub fn release_goal_build(staging_path: &Path) {
    let removed = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(staging_path);
    drop(removed);
}

/// Lines for `ta doctor`: cache size per key against the cap, and the first ten
/// measured goals.
pub fn doctor_lines(project_root: &Path, staging: &StagingConfig) -> Vec<String> {
    let mut lines = Vec::new();
    let entries = cache::list_entries(project_root);
    let total: u64 = entries.iter().map(|e| e.bytes).sum();
    let cap = if staging.build_cache_max_gb > 0.0 {
        format!("{} GB cap", staging.build_cache_max_gb)
    } else {
        "no cap".to_string()
    };
    lines.push(format!(
        "Build cache: {} in {} toolchain key(s), {} ({}).",
        super::staging_reclaim::format_bytes(total),
        entries.len(),
        cap,
        if staging.build_cache {
            "enabled"
        } else {
            "disabled by `[staging] build_cache = false`"
        }
    ));
    for e in entries.iter().rev() {
        let busy = match e.holders.first() {
            Some((goal, _)) => format!(", in use by goal {}", &goal[..goal.len().min(8)]),
            None => String::new(),
        };
        lines.push(format!(
            "  {}  {}  last used {} ago{}",
            e.key,
            super::staging_reclaim::format_bytes(e.bytes),
            cache::human_age(e.last_used),
            busy
        ));
    }
    if !entries.is_empty() {
        lines.push(
            "  Free space with `ta cleanup --only staging`, or remove one key with `rm -r .ta/build-cache/<key>`."
                .to_string(),
        );
    }
    let measured = cache::read_measurements(project_root);
    if !measured.is_empty() {
        lines.push(format!(
            "Build cost of the first {} goal(s) (agent wall time, build output before -> after):",
            measured.len().min(10)
        ));
        for m in measured.iter().take(10) {
            lines.push(format!(
                "  {}  {:>6}s  {} -> {}  {}",
                &m.goal_id[..m.goal_id.len().min(8)],
                m.agent_wall_secs,
                super::staging_reclaim::format_bytes(m.build_output_bytes_before),
                super::staging_reclaim::format_bytes(m.build_output_bytes_after),
                if m.cache_key.is_some() {
                    "shared cache"
                } else {
                    "own target/"
                }
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Result<cache::ToolchainFacts, String> {
        Ok(cache::ToolchainFacts {
            rustc_vv: "rustc 1.90.0\nhost: aarch64-apple-darwin\n".to_string(),
            rustflags: String::new(),
        })
    }

    fn project() -> tempfile::TempDir {
        let p = tempfile::tempdir().unwrap();
        std::fs::write(p.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(p.path().join("Cargo.lock"), "").unwrap();
        p
    }

    #[test]
    fn disabled_by_flag_or_config_or_missing_toolchain_says_why() {
        let p = project();
        let on = StagingConfig::default();
        let reason = |staging: &StagingConfig, flag: bool, f| {
            plan_cache_use(p.path(), p.path(), "g", staging, flag, f)
                .err()
                .unwrap()
        };
        assert!(reason(&on, true, facts()).contains("--no-build-cache"));
        let off = StagingConfig {
            build_cache: false,
            ..StagingConfig::default()
        };
        assert!(reason(&off, false, facts()).contains("build_cache = false"));
        let r = reason(&on, false, Err("no rustc on PATH".into()));
        assert!(r.contains("toolchain could not be identified"), "{r}");
        assert!(r.contains("no rustc on PATH"), "{r}");
        // Nothing was created for a goal that does not use the cache.
        assert!(!cache::cache_root(p.path()).exists());
    }

    #[test]
    fn two_running_goals_share_a_key_and_the_second_names_the_first() {
        let p = project();
        let cfg = StagingConfig::default();
        let first = plan_cache_use(p.path(), p.path(), "goal-a", &cfg, false, facts()).unwrap();
        assert!(first.waiting_on.is_empty());
        std::fs::write(first.selection.target_dir.join("libdep.rlib"), b"x").unwrap();

        let second = plan_cache_use(p.path(), p.path(), "goal-b", &cfg, false, facts()).unwrap();
        assert_eq!(first.selection.target_dir, second.selection.target_dir);
        assert_eq!(
            second.waiting_on,
            vec![("goal-a".to_string(), std::process::id())]
        );
        // The second goal sees what the first goal built.
        assert_eq!(second.bytes_before, 1);

        // When the first goal finishes, a third goal waits for nobody but the second.
        drop(first);
        let third = plan_cache_use(p.path(), p.path(), "goal-c", &cfg, false, facts()).unwrap();
        assert_eq!(third.waiting_on.len(), 1);
        assert_eq!(third.waiting_on[0].0, "goal-b");
    }

    #[test]
    fn starting_a_goal_evicts_old_keys_but_not_its_own_or_busy_ones() {
        let p = project();
        let old = cache::cache_root(p.path()).join("old-key");
        std::fs::create_dir_all(old.join("target")).unwrap();
        std::fs::write(old.join("target/blob"), vec![0u8; 4096]).unwrap();
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(old.join("last_used"))
            .unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(86_400))
            .unwrap();
        // 1 byte cap in GB would be 0 (= no cap), so use a tiny positive fraction.
        let cfg = StagingConfig {
            build_cache_max_gb: 1e-9,
            ..StagingConfig::default()
        };

        let used = plan_cache_use(p.path(), p.path(), "g", &cfg, false, facts()).unwrap();

        assert_eq!(used.eviction.evicted.len(), 1);
        assert_eq!(used.eviction.evicted[0].0, "old-key");
        assert!(!old.exists());
        assert!(used.selection.target_dir.is_dir(), "own key must survive");
    }

    #[test]
    fn sandboxed_agent_falls_back_to_its_own_target_only_when_it_pointed_at_the_cache() {
        let p = project();
        let staging = p.path().join(".ta/staging/abc");
        let id = uuid::Uuid::new_v4();
        let mut env = HashMap::new();
        assert!(!use_own_target_in_sandbox(&mut env, p.path(), &staging, id));

        env.insert("CARGO_TARGET_DIR".into(), "/somewhere/else".into());
        assert!(!use_own_target_in_sandbox(&mut env, p.path(), &staging, id));
        assert_eq!(env["CARGO_TARGET_DIR"], "/somewhere/else");

        let in_cache = cache::cache_root(p.path()).join("k/target");
        env.insert("CARGO_TARGET_DIR".into(), in_cache.display().to_string());
        assert!(use_own_target_in_sandbox(&mut env, p.path(), &staging, id));
        assert_eq!(
            PathBuf::from(&env["CARGO_TARGET_DIR"]),
            staging.join("target")
        );
    }

    #[test]
    fn doctor_reports_keys_against_the_cap_and_the_first_ten_goals() {
        let p = project();
        let cfg = StagingConfig::default();
        let used = plan_cache_use(p.path(), p.path(), "goal-a", &cfg, false, facts()).unwrap();
        for i in 0..12 {
            cache::record_measurement(
                p.path(),
                &cache::Measurement {
                    goal_id: format!("goal-{i:02}-xxxxxxxx"),
                    recorded_at: "t".into(),
                    cache_key: (i % 2 == 0).then(|| used.selection.key.clone()),
                    agent_wall_secs: 100 + i,
                    build_output_bytes_before: 0,
                    build_output_bytes_after: 2048,
                },
            )
            .unwrap();
        }
        let text = doctor_lines(p.path(), &cfg).join("\n");
        assert!(text.contains("1 toolchain key(s)"), "{text}");
        assert!(text.contains("20 GB cap"), "{text}");
        assert!(text.contains("in use by goal goal-a"), "{text}");
        assert!(text.contains("first 10 goal(s)"), "{text}");
        assert!(text.contains("goal-09-"), "{text}");
        assert!(
            !text.contains("goal-10-"),
            "only the first ten are listed:\n{text}"
        );
        assert!(text.contains("shared cache") && text.contains("own target/"));
    }
}
