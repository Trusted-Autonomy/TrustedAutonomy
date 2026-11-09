// staging.rs — `ta staging` subcommands (v0.14.3.4).
//
// Provides inspection and management tools for the TA staging workspace:
//   ta staging inspect [goal-id]   — report strategy, size, file counts

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand, Debug)]
pub enum StagingCommands {
    /// Inspect the current staging workspace: strategy, size, file counts.
    ///
    /// Without a goal-id, reports on the most recent active staging directory.
    /// With a goal-id (or prefix), reports on that goal's staging workspace.
    ///
    /// Examples:
    ///   ta staging inspect
    ///   ta staging inspect abc123
    Inspect {
        /// Goal ID or prefix to inspect. Defaults to the most recently active goal.
        goal_id: Option<String>,
    },
}

pub fn execute(command: &StagingCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        StagingCommands::Inspect { goal_id } => inspect(config, goal_id.as_deref()),
    }
}

/// Report on the staging workspace for a goal.
fn inspect(config: &GatewayConfig, goal_id_hint: Option<&str>) -> anyhow::Result<()> {
    use ta_goal::GoalRunStore;
    use ta_workspace::overlay::ExcludePatterns;
    use ta_workspace::OverlayWorkspace;

    // Find the staging directory.
    let store = GoalRunStore::new(&config.goals_dir)?;
    let goals = store.list().unwrap_or_default();

    let goal = if let Some(hint) = goal_id_hint {
        goals
            .iter()
            .find(|g| g.goal_run_id.to_string().starts_with(hint))
            .cloned()
    } else {
        // Most recent active goal with an existing staging workspace.
        goals
            .iter()
            .filter(|g| g.workspace_path.exists())
            .max_by_key(|g| g.created_at)
            .cloned()
    };

    let goal = match goal {
        Some(g) => g,
        None => {
            if let Some(hint) = goal_id_hint {
                anyhow::bail!(
                    "No goal found matching '{}'. Run `ta goal list` to see available goals.",
                    hint
                );
            } else {
                println!("No active staging workspaces found.");
                println!("  Start a goal with `ta run <title>` to create one.");
                return Ok(());
            }
        }
    };

    let staging_dir = if goal.workspace_path.exists() {
        goal.workspace_path.clone()
    } else {
        anyhow::bail!(
            "Staging directory no longer exists: {}\n  The goal may have been cleaned up. Run `ta goal list` to see current state.",
            goal.workspace_path.display()
        );
    };

    let source_dir = match &goal.source_dir {
        Some(p) => p.clone(),
        None => config.workspace_root.clone(),
    };

    let id_str = goal.goal_run_id.to_string();
    println!("Staging Inspect — Goal {}", &id_str[..8.min(id_str.len())]);
    println!("{}", "=".repeat(50));
    println!("  Title:       {}", goal.title);
    println!("  Goal ID:     {}", goal.goal_run_id);
    println!("  State:       {:?}", goal.state);
    println!("  Source:      {}", source_dir.display());
    println!("  Staging dir: {}", staging_dir.display());
    println!();

    // Load workflow config for strategy.
    let workflow = ta_submit::config::WorkflowConfig::load_or_default(
        &config.workspace_root.join(".ta/workflow.toml"),
    );
    let configured_strategy = workflow.staging.strategy.as_str();
    println!("  Configured strategy: {}", configured_strategy);

    // Count files and symlinks in staging.
    let mut total_files = 0u64;
    let mut total_symlinks = 0u64;
    let mut total_bytes: u64 = 0;

    walk_staging(
        &staging_dir,
        &mut total_files,
        &mut total_symlinks,
        &mut total_bytes,
    )?;

    let staged_mb = total_bytes as f64 / (1024.0 * 1024.0);
    println!();
    println!("  Staging workspace:");
    println!("    Files copied:    {}", total_files);
    println!(
        "    Symlinks:        {} (smart-mode excluded directories)",
        total_symlinks
    );
    println!(
        "    Disk used:       {:.1} MB (physical, excluding symlink targets)",
        staged_mb
    );

    // Estimate source size (what the agent can see, including symlink targets).
    let source_bytes = dir_size_bytes_no_follow(&source_dir);
    let source_mb = source_bytes as f64 / (1024.0 * 1024.0);
    println!("    Source size:     {:.1} MB", source_mb);
    if source_bytes > 0 && total_bytes < source_bytes {
        let overhead_pct = (total_bytes as f64 / source_bytes as f64) * 100.0;
        println!(
            "    TA overhead:     {:.1}% of source size staged physically",
            overhead_pct
        );
    }

    // Load excludes and report.
    let excludes = ExcludePatterns::load(&source_dir);
    let patterns = excludes.patterns();
    println!();
    println!("  Exclude patterns ({} total):", patterns.len());
    for p in patterns {
        println!("    {}", p);
    }

    // Diff summary: how many files changed.
    let overlay = OverlayWorkspace::open(
        goal.goal_run_id.to_string(),
        &source_dir,
        &staging_dir,
        excludes,
    );
    match overlay.list_changes() {
        Ok(changes) => {
            let modified = changes.iter().filter(|(_, k)| *k == "modified").count();
            let created = changes.iter().filter(|(_, k)| *k == "created").count();
            let deleted = changes.iter().filter(|(_, k)| *k == "deleted").count();
            println!();
            println!("  Changes vs source:");
            println!(
                "    {} modified, {} created, {} deleted",
                modified, created, deleted
            );
        }
        Err(e) => {
            println!();
            println!("  Changes: (could not compute diff: {})", e);
        }
    }

    // Warn if staged workspace is large.
    let warn_gb = workflow.staging.warn_above_gb;
    if warn_gb > 0.0 && staged_mb > warn_gb * 1024.0 {
        println!();
        println!(
            "  ⚠ Staging workspace is {:.1} GB (warn_above_gb = {})",
            staged_mb / 1024.0,
            warn_gb
        );
        println!("    Consider adding a .taignore or switching to strategy = \"smart\".");
        println!("    To silence this: set [staging] warn_above_gb = 0 in .ta/workflow.toml");
    }

    println!();
    println!("Tip: `ta doctor` shows strategy selection and workspace health across all goals.");
    Ok(())
}

/// Walk a directory counting regular files and symlinks separately.
fn walk_staging(
    dir: &std::path::Path,
    files: &mut u64,
    symlinks: &mut u64,
    bytes: &mut u64,
) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            *symlinks += 1;
        } else if file_type.is_dir() {
            walk_staging(&entry.path(), files, symlinks, bytes)?;
        } else {
            *files += 1;
            *bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(())
}

/// Compute the total size of a directory without following symlinks.
fn dir_size_bytes_no_follow(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    total += dir_size_bytes_no_follow(&entry.path());
                } else if !ft.is_symlink() {
                    total += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn walk_staging_counts_files_and_symlinks() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"world").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/c.txt"), b"nested").unwrap();

        let mut files = 0;
        let mut symlinks = 0;
        let mut bytes = 0;
        walk_staging(dir.path(), &mut files, &mut symlinks, &mut bytes).unwrap();

        assert_eq!(files, 3);
        assert_eq!(symlinks, 0);
        assert!(bytes > 0);
    }

    #[test]
    fn walk_staging_empty_dir() {
        let dir = TempDir::new().unwrap();
        let mut files = 0;
        let mut symlinks = 0;
        let mut bytes = 0;
        walk_staging(dir.path(), &mut files, &mut symlinks, &mut bytes).unwrap();
        assert_eq!(files, 0);
        assert_eq!(symlinks, 0);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn dir_size_bytes_no_follow_counts_only_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"12345").unwrap();
        let size = dir_size_bytes_no_follow(dir.path());
        assert_eq!(size, 5);
    }

    #[test]
    fn staging_commands_have_inspect_variant() {
        // Verify the enum compiles and has the Inspect variant.
        let _cmd = StagingCommands::Inspect { goal_id: None };
        let _cmd2 = StagingCommands::Inspect {
            goal_id: Some("abc123".to_string()),
        };
    }
}
