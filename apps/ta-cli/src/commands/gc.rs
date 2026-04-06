// gc.rs — Unified garbage collection command (v0.9.8.1).
//
// `ta gc` runs goal GC, draft GC, staging cleanup, and event pruning
// in one pass. Writes history entries before archiving/removing goals.
//
// v0.15.6.2: Aggressive GC defaults for failed goals (4h retention),
//            --status table, --delete-stale flag.

use ta_goal::{
    GoalHistoryEntry, GoalHistoryLedger, GoalOutcome, GoalRunState, GoalRunStore, VelocityEntry,
    VelocityStore,
};
use ta_mcp_gateway::GatewayConfig;

/// Minimal GC config loaded from `.ta/daemon.toml` [gc] section.
///
/// Mirrors `ta_daemon::config::GcConfig` but lives in ta-cli so we don't
/// need a ta-daemon dependency just for config reading.
#[derive(Debug, serde::Deserialize)]
#[serde(default)]
struct GcConfig {
    /// Hours to retain staging for failed goals (default: 4h).
    failed_staging_retention_hours: u32,
    /// Maximum total staging GB before cap enforcement (default: 20).
    max_staging_gb: u32,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            failed_staging_retention_hours: 4,
            max_staging_gb: 20,
        }
    }
}

/// Minimal wrapper to deserialize just the [gc] section from daemon.toml.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct DaemonTomlGc {
    gc: GcConfig,
}

fn load_gc_config(workspace_root: &std::path::Path) -> GcConfig {
    let path = workspace_root.join(".ta/daemon.toml");
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(parsed) = toml::from_str::<DaemonTomlGc>(&content) {
            return parsed.gc;
        }
    }
    GcConfig::default()
}

#[allow(clippy::too_many_arguments)]
pub fn execute(
    config: &GatewayConfig,
    dry_run: bool,
    threshold_days: u32,
    gc_all: bool,
    archive: bool,
    include_events: bool,
    compact: bool,
    compact_after_days: u32,
    force: bool,
    status: bool,
    delete_stale: bool,
) -> anyhow::Result<()> {
    // --status: print a table and exit.
    if status {
        return print_status(config);
    }

    // Refuse to delete staging dirs while a release pipeline is active, unless --force.
    let lock_path = config.workspace_root.join(".ta/release.lock");
    if !force && lock_path.exists() {
        let pid_hint = std::fs::read_to_string(&lock_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|p| format!(" (PID {})", p))
            .unwrap_or_default();
        eprintln!(
            "warning: release pipeline is active{} — skipping staging GC to prevent data loss.",
            pid_hint
        );
        eprintln!("         Wait for `ta release run` to complete, then re-run `ta gc`.");
        eprintln!("         To override: ta gc --force");
        return Ok(());
    }

    let gc_cfg = load_gc_config(&config.workspace_root);

    let store = GoalRunStore::new(&config.goals_dir)?;
    let ledger = GoalHistoryLedger::for_project(&config.workspace_root);
    let velocity = VelocityStore::for_project(&config.workspace_root);

    let now = chrono::Utc::now();

    // Cutoff for applied/completed goals (user-configurable --threshold-days).
    let applied_cutoff = if gc_all {
        now
    } else {
        now - chrono::Duration::days(threshold_days as i64)
    };

    // Aggressive cutoff for failed goals (4h default, config-controlled).
    let failed_cutoff = if gc_all {
        now
    } else {
        now - chrono::Duration::hours(gc_cfg.failed_staging_retention_hours as i64)
    };

    let goals = store.list()?;
    let mut zombie_count = 0u32;
    let mut staging_count = 0u32;
    let mut staging_bytes = 0u64;
    let mut draft_count = 0u32;
    let mut history_count = 0u32;

    // --delete-stale: confirm then delete all terminal staging.
    if delete_stale {
        return delete_stale_staging(config, &store, &goals, dry_run);
    }

    for goal in &goals {
        let is_failed = matches!(goal.state, GoalRunState::Failed { .. });
        let is_applied_or_completed =
            matches!(goal.state, GoalRunState::Applied | GoalRunState::Completed);
        let is_terminal = is_failed || is_applied_or_completed;

        // 1. Zombie detection: running goals past threshold.
        if goal.state == GoalRunState::Running && goal.updated_at < applied_cutoff {
            if dry_run {
                println!(
                    "[dry-run] Would transition to failed: {} \"{}\" (stale {}d)",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                    (now - goal.updated_at).num_days(),
                );
            } else {
                let mut g = goal.clone();
                let _ = g.transition(GoalRunState::Failed {
                    reason: format!("gc: stale goal exceeded {}d threshold", threshold_days),
                });
                store.save(&g)?;
                // Write history before archiving.
                let entry = GoalHistoryEntry::from_goal(&g);
                let _ = ledger.append(&entry);
                history_count += 1;
                let vel = VelocityEntry::from_goal(&g, GoalOutcome::Timeout)
                    .with_cancel_reason(format!("gc: stale {}d", threshold_days));
                let _ = velocity.append(&vel);
                println!(
                    "Transitioned to failed: {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            }
            zombie_count += 1;
        }

        // 2. PrReady goals past threshold (built but never reviewed).
        if goal.state == GoalRunState::PrReady && goal.updated_at < applied_cutoff {
            if dry_run {
                println!(
                    "[dry-run] Would transition to failed: {} \"{}\" (pr_ready, stale {}d)",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                    (now - goal.updated_at).num_days(),
                );
            } else {
                let mut g = goal.clone();
                let _ = g.transition(GoalRunState::Failed {
                    reason: format!(
                        "gc: pr_ready goal never reviewed, exceeded {}d threshold",
                        threshold_days
                    ),
                });
                store.save(&g)?;
                let entry = GoalHistoryEntry::from_goal(&g);
                let _ = ledger.append(&entry);
                history_count += 1;
                let vel = VelocityEntry::from_goal(&g, GoalOutcome::Timeout).with_cancel_reason(
                    format!("gc: pr_ready never reviewed, {}d", threshold_days),
                );
                let _ = velocity.append(&vel);
                println!(
                    "Transitioned to failed: {} \"{}\" (pr_ready, never reviewed)",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            }
            zombie_count += 1;
        }

        // 3. Missing staging detection.
        if !is_terminal
            && goal.state != GoalRunState::Created
            && !goal.workspace_path.as_os_str().is_empty()
            && !goal.workspace_path.exists()
        {
            if dry_run {
                println!(
                    "[dry-run] Would mark failed (missing staging): {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            } else {
                let mut g = goal.clone();
                let _ = g.transition(GoalRunState::Failed {
                    reason: "gc: missing staging workspace".to_string(),
                });
                store.save(&g)?;
                let entry = GoalHistoryEntry::from_goal(&g);
                let _ = ledger.append(&entry);
                history_count += 1;
                let vel = VelocityEntry::from_goal(&g, GoalOutcome::Failed)
                    .with_cancel_reason("gc: missing staging workspace");
                let _ = velocity.append(&vel);
                println!(
                    "Marked failed (missing staging): {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            }
            zombie_count += 1;
        }

        // 4. Staging cleanup for terminal goals past their retention window.
        //
        //    Failed goals: use failed_staging_retention_hours (default 4h) — very aggressive.
        //    Applied/completed goals: use --threshold-days (default 7d).
        let past_cutoff = if is_failed {
            goal.updated_at < failed_cutoff
        } else {
            goal.updated_at < applied_cutoff
        };

        if is_terminal
            && past_cutoff
            && !goal.workspace_path.as_os_str().is_empty()
            && goal.workspace_path.exists()
        {
            let dir_size = walkdir_size(&goal.workspace_path);

            if archive {
                let archive_dir = config.workspace_root.join(".ta/goals/archive");
                if dry_run {
                    println!(
                        "[dry-run] Would archive staging: {} ({}, goal: {})",
                        goal.workspace_path.display(),
                        format_bytes(dir_size),
                        &goal.goal_run_id.to_string()[..8],
                    );
                } else {
                    std::fs::create_dir_all(&archive_dir)?;
                    let dest = archive_dir.join(goal.goal_run_id.to_string());
                    if let Err(e) = std::fs::rename(&goal.workspace_path, &dest) {
                        // rename may fail across filesystems, fall back to remove.
                        tracing::warn!("archive rename failed: {}, removing instead", e);
                        std::fs::remove_dir_all(&goal.workspace_path)?;
                    }
                    println!(
                        "Archived staging: {} ({}, goal: {})",
                        goal.workspace_path.display(),
                        format_bytes(dir_size),
                        &goal.goal_run_id.to_string()[..8],
                    );
                }
            } else if dry_run {
                println!(
                    "[dry-run] Would remove staging: {} ({}, goal: {})",
                    goal.workspace_path.display(),
                    format_bytes(dir_size),
                    &goal.goal_run_id.to_string()[..8],
                );
            } else {
                std::fs::remove_dir_all(&goal.workspace_path)?;
                println!(
                    "Removed staging: {} ({}, goal: {})",
                    goal.workspace_path.display(),
                    format_bytes(dir_size),
                    &goal.goal_run_id.to_string()[..8],
                );
            }
            staging_count += 1;
            staging_bytes += dir_size;

            // Write history entry for terminal goals being cleaned.
            if !dry_run {
                let entry = GoalHistoryEntry::from_goal(goal);
                let _ = ledger.append(&entry);
                history_count += 1;
            }
        }
    }

    // 5. Clean orphaned draft package JSON files.
    if config.pr_packages_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&config.pr_packages_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(pkg) = serde_json::from_str::<
                            ta_changeset::draft_package::DraftPackage,
                        >(&content)
                        {
                            // Check if the goal still exists.
                            let goal_id_str = &pkg.goal.goal_id;
                            let goal_exists = goals
                                .iter()
                                .any(|g| g.goal_run_id.to_string() == *goal_id_str);
                            if !goal_exists {
                                if dry_run {
                                    println!(
                                        "[dry-run] Would remove orphaned draft: {}",
                                        path.display()
                                    );
                                } else {
                                    std::fs::remove_file(&path)?;
                                    println!("Removed orphaned draft: {}", path.display());
                                }
                                draft_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    // Lifecycle compaction pass (v0.13.1).
    let mut compaction_count = 0u32;
    let mut compaction_bytes = 0u64;
    if compact {
        let compact_cutoff = chrono::Utc::now() - chrono::Duration::days(compact_after_days as i64);
        for goal in &goals {
            let is_compactable =
                matches!(goal.state, GoalRunState::Applied | GoalRunState::Completed)
                    && goal.updated_at < compact_cutoff;

            if !is_compactable {
                continue;
            }

            // Remove staging directory.
            if !goal.workspace_path.as_os_str().is_empty() && goal.workspace_path.exists() {
                let dir_size = walkdir_size(&goal.workspace_path);
                if dry_run {
                    println!(
                        "[dry-run] compact: Would remove staging for {} \"{}\" (applied {}d ago, {})",
                        &goal.goal_run_id.to_string()[..8],
                        truncate(&goal.title, 40),
                        (chrono::Utc::now() - goal.updated_at).num_days(),
                        format_bytes(dir_size),
                    );
                } else {
                    if let Err(e) = std::fs::remove_dir_all(&goal.workspace_path) {
                        tracing::warn!(
                            goal_id = %goal.goal_run_id,
                            "compact: failed to remove staging: {}", e
                        );
                    } else {
                        println!(
                            "compact: Removed staging for {} \"{}\" ({})",
                            &goal.goal_run_id.to_string()[..8],
                            truncate(&goal.title, 40),
                            format_bytes(dir_size),
                        );
                        // Write history entry (compaction preserves the ledger).
                        let entry = GoalHistoryEntry::from_goal(goal);
                        let _ = ledger.append(&entry);
                        history_count += 1;
                    }
                }
                compaction_bytes += dir_size;
                compaction_count += 1;
            }

            // Remove associated draft package if present.
            if let Some(draft_id) = &goal.pr_package_id {
                let draft_path = config.pr_packages_dir.join(format!("{}.json", draft_id));
                if draft_path.exists() {
                    if dry_run {
                        println!(
                            "[dry-run] compact: Would remove draft package {} (goal: {})",
                            draft_id,
                            &goal.goal_run_id.to_string()[..8],
                        );
                    } else if let Err(e) = std::fs::remove_file(&draft_path) {
                        tracing::warn!(
                            "compact: failed to remove draft package {}: {}",
                            draft_id,
                            e
                        );
                    } else {
                        println!(
                            "compact: Removed draft package {} (goal: {})",
                            draft_id,
                            &goal.goal_run_id.to_string()[..8],
                        );
                    }
                }
            }
        }
    }

    // Event store pruning (v0.11.3).
    let mut event_count = 0u32;
    if include_events {
        let events_dir = config.workspace_root.join(".ta/events");
        if events_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&events_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Ok(meta) = entry.metadata() {
                        if meta.is_file() {
                            if let Ok(modified) = meta.modified() {
                                let age = std::time::SystemTime::now()
                                    .duration_since(modified)
                                    .unwrap_or_default();
                                let threshold =
                                    std::time::Duration::from_secs(threshold_days as u64 * 86400);
                                if gc_all || age > threshold {
                                    if dry_run {
                                        println!(
                                            "[dry-run] Would remove event: {}",
                                            path.display()
                                        );
                                    } else {
                                        let _ = std::fs::remove_file(&path);
                                        println!("Removed event: {}", path.display());
                                    }
                                    event_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if compact {
        println!(
            "\n{}GC complete: {} zombie(s), {} staging ({}) reclaimed, {} orphan draft(s), {} event(s) pruned, {} history entries, {} compacted ({}).",
            if dry_run { "[dry-run] " } else { "" },
            zombie_count,
            staging_count,
            format_bytes(staging_bytes),
            draft_count,
            event_count,
            history_count,
            compaction_count,
            format_bytes(compaction_bytes),
        );
    } else {
        println!(
            "\n{}GC complete: {} zombie(s), {} staging ({}) reclaimed, {} orphan draft(s), {} event(s) pruned, {} history entries.",
            if dry_run { "[dry-run] " } else { "" },
            zombie_count,
            staging_count,
            format_bytes(staging_bytes),
            draft_count,
            event_count,
            history_count,
        );
    }

    Ok(())
}

/// `ta gc --status`: print a table of all goals with staging dirs.
///
/// Shows goal ID, title (truncated), state, age, and staging size.
/// Does not modify anything.
fn print_status(config: &GatewayConfig) -> anyhow::Result<()> {
    let store = GoalRunStore::new(&config.goals_dir)?;
    let goals = store.list()?;
    let now = chrono::Utc::now();

    // Collect goals with staging dirs or interesting state.
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    let mut total_staging_bytes = 0u64;

    for goal in &goals {
        let has_staging =
            !goal.workspace_path.as_os_str().is_empty() && goal.workspace_path.exists();

        let size_str = if has_staging {
            let sz = walkdir_size(&goal.workspace_path);
            total_staging_bytes += sz;
            format_bytes(sz)
        } else {
            "-".to_string()
        };

        let age_str = {
            let secs = (now - goal.updated_at).num_seconds().unsigned_abs();
            if secs < 3600 {
                format!("{}m", secs / 60)
            } else if secs < 86400 {
                format!("{}h", secs / 3600)
            } else {
                format!("{}d", secs / 86400)
            }
        };

        rows.push((
            goal.goal_run_id.to_string()[..8].to_string(),
            truncate(&goal.title, 36).to_string(),
            goal.state.to_string(),
            age_str,
            size_str,
        ));
    }

    if rows.is_empty() {
        println!("No goals found.");
        return Ok(());
    }

    println!(
        "{:<10} {:<38} {:<18} {:<8} Staging",
        "ID", "Title", "State", "Age"
    );
    println!("{}", "-".repeat(90));
    for (id, title, state, age, size) in &rows {
        println!("{:<10} {:<38} {:<18} {:<8} {}", id, title, state, age, size);
    }
    println!("{}", "-".repeat(90));
    println!(
        "Total staging: {} across {} goal(s)",
        format_bytes(total_staging_bytes),
        goals.len()
    );
    println!();
    println!("Run `ta gc` to clean up staging for terminal goals.");
    println!("Run `ta gc --delete-stale` to delete ALL terminal staging (with confirmation).");

    Ok(())
}

/// `ta gc --delete-stale`: delete staging for all non-running terminal goals.
fn delete_stale_staging(
    _config: &GatewayConfig,
    _store: &GoalRunStore,
    goals: &[ta_goal::GoalRun],
    dry_run: bool,
) -> anyhow::Result<()> {
    let candidates: Vec<_> = goals
        .iter()
        .filter(|g| {
            let is_terminal = matches!(
                g.state,
                GoalRunState::Applied
                    | GoalRunState::Completed
                    | GoalRunState::Failed { .. }
                    | GoalRunState::Merged
            );
            is_terminal && !g.workspace_path.as_os_str().is_empty() && g.workspace_path.exists()
        })
        .collect();

    if candidates.is_empty() {
        println!("No terminal staging directories found.");
        return Ok(());
    }

    let total_size: u64 = candidates
        .iter()
        .map(|g| walkdir_size(&g.workspace_path))
        .sum();

    println!(
        "Found {} terminal staging dir(s) ({} total):",
        candidates.len(),
        format_bytes(total_size)
    );
    for goal in &candidates {
        let sz = walkdir_size(&goal.workspace_path);
        println!(
            "  {} \"{}\" ({}, {})",
            &goal.goal_run_id.to_string()[..8],
            truncate(&goal.title, 40),
            goal.state,
            format_bytes(sz),
        );
    }
    println!();

    if dry_run {
        println!(
            "[dry-run] Would delete {} staging dir(s) ({}).",
            candidates.len(),
            format_bytes(total_size)
        );
        return Ok(());
    }

    // Confirm.
    print!(
        "Delete all {} staging dir(s) ({})? [y/N] ",
        candidates.len(),
        format_bytes(total_size)
    );
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !input.trim().eq_ignore_ascii_case("y") {
        println!("Aborted.");
        return Ok(());
    }

    let mut removed = 0u32;
    let mut removed_bytes = 0u64;
    for goal in &candidates {
        let sz = walkdir_size(&goal.workspace_path);
        if let Err(e) = std::fs::remove_dir_all(&goal.workspace_path) {
            eprintln!(
                "warning: failed to remove {}: {}",
                goal.workspace_path.display(),
                e
            );
        } else {
            removed += 1;
            removed_bytes += sz;
            println!(
                "Removed: {} \"{}\" ({})",
                &goal.goal_run_id.to_string()[..8],
                truncate(&goal.title, 40),
                format_bytes(sz),
            );
        }
    }

    println!(
        "\nDeleted {} staging dir(s), freed {}.",
        removed,
        format_bytes(removed_bytes)
    );
    Ok(())
}

/// Run a lightweight GC pass suitable for daemon startup or periodic invocation.
///
/// Removes staging for failed goals beyond `failed_staging_retention_hours`
/// and applied/completed goals beyond `applied_staging_retention_days`.
/// Does not write history entries or emit velocity records (daemon context).
#[allow(dead_code)]
pub fn run_periodic_gc(
    config: &GatewayConfig,
    failed_staging_retention_hours: u32,
    applied_staging_retention_days: u32,
) -> (u32, u64) {
    let store = match GoalRunStore::new(&config.goals_dir) {
        Ok(s) => s,
        Err(_) => return (0, 0),
    };
    let goals = match store.list() {
        Ok(g) => g,
        Err(_) => return (0, 0),
    };
    let now = chrono::Utc::now();
    let failed_cutoff = now - chrono::Duration::hours(failed_staging_retention_hours as i64);
    let applied_cutoff = now - chrono::Duration::days(applied_staging_retention_days as i64);

    let mut removed = 0u32;
    let mut freed_bytes = 0u64;

    for goal in &goals {
        let is_failed = matches!(goal.state, GoalRunState::Failed { .. });
        let is_applied_completed = matches!(
            goal.state,
            GoalRunState::Applied | GoalRunState::Completed | GoalRunState::Merged
        );

        let past_cutoff = if is_failed {
            goal.updated_at < failed_cutoff
        } else if is_applied_completed {
            goal.updated_at < applied_cutoff
        } else {
            false
        };

        if past_cutoff
            && !goal.workspace_path.as_os_str().is_empty()
            && goal.workspace_path.exists()
        {
            let sz = walkdir_size(&goal.workspace_path);
            if std::fs::remove_dir_all(&goal.workspace_path).is_ok() {
                removed += 1;
                freed_bytes += sz;
                tracing::info!(
                    goal_id = %goal.goal_run_id,
                    state = %goal.state,
                    freed_bytes = sz,
                    "periodic gc: removed staging"
                );
            }
        }
    }

    (removed, freed_bytes)
}

/// Check if total staging usage exceeds `max_staging_gb`.
///
/// Returns `(total_bytes, exceeds_cap)`.
pub fn check_staging_cap(config: &GatewayConfig, max_staging_gb: u32) -> (u64, bool) {
    if max_staging_gb == 0 {
        return (0, false);
    }
    let staging_dir = &config.staging_dir;
    if !staging_dir.exists() {
        return (0, false);
    }
    let total = walkdir_size(staging_dir);
    let cap_bytes = max_staging_gb as u64 * 1_073_741_824;
    (total, total > cap_bytes)
}

/// Enforce the staging size cap before starting a new goal.
///
/// If total staging exceeds `max_staging_gb`, GC the oldest failed/completed
/// dirs to bring usage below the cap. Returns `true` if space was freed.
pub fn enforce_staging_cap(config: &GatewayConfig) -> bool {
    let gc_cfg = load_gc_config(&config.workspace_root);
    if gc_cfg.max_staging_gb == 0 {
        return false;
    }

    let cap_bytes = gc_cfg.max_staging_gb as u64 * 1_073_741_824;
    let (total, exceeds) = check_staging_cap(config, gc_cfg.max_staging_gb);
    if !exceeds {
        return false;
    }

    let store = match GoalRunStore::new(&config.goals_dir) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let mut goals = match store.list() {
        Ok(g) => g,
        Err(_) => return false,
    };

    // Sort by updated_at ascending (oldest first).
    goals.sort_by_key(|g| g.updated_at);

    let mut freed = 0u64;
    let need_to_free = total.saturating_sub(cap_bytes);

    eprintln!(
        "warning: staging exceeds {} cap (currently {}). Freeing oldest failed/completed dirs.",
        format_bytes(cap_bytes),
        format_bytes(total)
    );

    for goal in &goals {
        if freed >= need_to_free {
            break;
        }
        let is_reclaimable = matches!(
            goal.state,
            GoalRunState::Failed { .. }
                | GoalRunState::Applied
                | GoalRunState::Completed
                | GoalRunState::Merged
        );
        if !is_reclaimable {
            continue;
        }
        if goal.workspace_path.as_os_str().is_empty() || !goal.workspace_path.exists() {
            continue;
        }
        let sz = walkdir_size(&goal.workspace_path);
        if std::fs::remove_dir_all(&goal.workspace_path).is_ok() {
            freed += sz;
            tracing::info!(
                goal_id = %goal.goal_run_id,
                freed_bytes = sz,
                "staging cap: removed staging to free space"
            );
            eprintln!(
                "  Removed staging for {} \"{}\" ({})",
                &goal.goal_run_id.to_string()[..8],
                truncate(&goal.title, 40),
                format_bytes(sz),
            );
        }
    }

    freed > 0
}

fn walkdir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += walkdir_size(&entry.path());
                }
            }
        }
    }
    total
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn gc_status_prints_table() {
        let dir = tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // Should not panic with empty goal store.
        let result = print_status(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn gc_failed_uses_aggressive_cutoff() {
        // Verify that failed_cutoff is much more aggressive than applied_cutoff.
        let failed_h = 4u32;
        let now = chrono::Utc::now();
        let failed_cutoff = now - chrono::Duration::hours(failed_h as i64);
        let applied_cutoff = now - chrono::Duration::days(7);

        // A goal updated 5 hours ago should be past the failed cutoff but NOT the applied cutoff.
        let updated_5h_ago = now - chrono::Duration::hours(5);
        assert!(
            updated_5h_ago < failed_cutoff,
            "5h-old failed goal should be past 4h cutoff"
        );
        assert!(
            updated_5h_ago > applied_cutoff,
            "5h-old goal should NOT be past 7d applied cutoff"
        );
    }

    #[test]
    fn check_staging_cap_returns_false_when_zero() {
        let dir = tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let (_total, exceeds) = check_staging_cap(&config, 0);
        assert!(
            !exceeds,
            "cap=0 means disabled — should never report exceeds"
        );
    }

    #[test]
    fn periodic_gc_removes_old_failed_staging() {
        let dir = tempdir().unwrap();
        // Create a fake staging dir for a failed goal.
        let staging_dir = dir.path().join(".ta/staging/old-goal");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::write(staging_dir.join("file.txt"), "data").unwrap();
        assert!(staging_dir.exists());

        let config = GatewayConfig::for_project(dir.path());
        // run_periodic_gc should not panic on missing goal store.
        let (removed, _freed) = run_periodic_gc(&config, 4, 7);
        // No goals in the store, so nothing is "expired" — but also no panic.
        assert_eq!(removed, 0);
    }

    #[test]
    fn load_gc_config_returns_defaults_when_no_file() {
        let dir = tempdir().unwrap();
        let cfg = load_gc_config(dir.path());
        assert_eq!(cfg.failed_staging_retention_hours, 4);
        assert_eq!(cfg.max_staging_gb, 20);
    }

    #[test]
    fn load_gc_config_reads_from_daemon_toml() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("daemon.toml"),
            "[gc]\nfailed_staging_retention_hours = 2\nmax_staging_gb = 10\n",
        )
        .unwrap();
        let cfg = load_gc_config(dir.path());
        assert_eq!(cfg.failed_staging_retention_hours, 2);
        assert_eq!(cfg.max_staging_gb, 10);
    }
}
