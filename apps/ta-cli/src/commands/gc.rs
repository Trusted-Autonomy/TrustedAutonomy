// gc.rs — Unified garbage collection command (v0.9.8.1).
//
// `ta gc` runs goal GC, draft GC, staging cleanup, and event pruning
// in one pass. Writes history entries before archiving/removing goals.

use ta_goal::{GoalHistoryEntry, GoalHistoryLedger, GoalRunState, GoalRunStore};
use ta_mcp_gateway::GatewayConfig;

pub fn execute(
    config: &GatewayConfig,
    dry_run: bool,
    threshold_days: u32,
    gc_all: bool,
    archive: bool,
    include_events: bool,
) -> anyhow::Result<()> {
    let store = GoalRunStore::new(&config.goals_dir)?;
    let ledger = GoalHistoryLedger::for_project(&config.workspace_root);

    let cutoff = if gc_all {
        chrono::Utc::now() // everything is "past" the cutoff
    } else {
        chrono::Utc::now() - chrono::Duration::days(threshold_days as i64)
    };

    let goals = store.list()?;
    let mut zombie_count = 0u32;
    let mut staging_count = 0u32;
    let mut staging_bytes = 0u64;
    let mut draft_count = 0u32;
    let mut history_count = 0u32;

    for goal in &goals {
        let is_terminal = matches!(
            goal.state,
            GoalRunState::Applied | GoalRunState::Completed | GoalRunState::Failed { .. }
        );

        // 1. Zombie detection: running goals past threshold.
        if goal.state == GoalRunState::Running && goal.updated_at < cutoff {
            if dry_run {
                println!(
                    "[dry-run] Would transition to failed: {} \"{}\" (stale {}d)",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                    (chrono::Utc::now() - goal.updated_at).num_days(),
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
                println!(
                    "Transitioned to failed: {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            }
            zombie_count += 1;
        }

        // 2. PrReady goals past threshold (built but never reviewed).
        if goal.state == GoalRunState::PrReady && goal.updated_at < cutoff {
            if dry_run {
                println!(
                    "[dry-run] Would transition to failed: {} \"{}\" (pr_ready, stale {}d)",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                    (chrono::Utc::now() - goal.updated_at).num_days(),
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
                println!(
                    "Marked failed (missing staging): {} \"{}\"",
                    &goal.goal_run_id.to_string()[..8],
                    truncate(&goal.title, 40),
                );
            }
            zombie_count += 1;
        }

        // 4. Staging cleanup for terminal goals past threshold.
        if is_terminal
            && goal.updated_at < cutoff
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

    Ok(())
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max - 3])
    } else {
        s.to_string()
    }
}
