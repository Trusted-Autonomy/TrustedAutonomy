// memory.rs — `ta memory` subcommands for inspecting the memory backend (v0.12.5).
//
// Currently exposes a single `backend` subcommand that shows which store is active,
// how many entries it holds, the on-disk size, and whether migration from the legacy
// FsMemoryStore has occurred.

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand, Debug)]
pub enum MemoryCommands {
    /// Show active memory backend, entry count, and storage size.
    ///
    /// Prints which backend is in use (ruvector or fs), how many entries
    /// are stored, and the disk footprint of the memory directory. Also
    /// reports whether migration from the legacy FsMemoryStore has occurred.
    Backend,

    /// List memory entries (alias for `ta context list`).
    ///
    /// Prints a summary table of stored entries, optionally filtered by
    /// category. Useful for quickly auditing what TA knows about a project.
    List {
        /// Filter by category (e.g., convention, architecture, history).
        #[arg(long, short = 'c')]
        category: Option<String>,
        /// Maximum number of entries to show (default: 50).
        #[arg(long, default_value = "50")]
        limit: usize,
    },
}

pub fn execute(command: &MemoryCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        MemoryCommands::Backend => show_backend(config),
        MemoryCommands::List { category, limit } => {
            // Delegate to context list.
            super::context::execute(
                &super::context::ContextCommands::List {
                    tag: vec![],
                    prefix: None,
                    category: category.clone(),
                    limit: Some(*limit),
                },
                config,
            )
        }
    }
}

fn show_backend(config: &GatewayConfig) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
    let configured_backend = memory_config.backend.as_deref().unwrap_or("ruvector");

    let rvf_path = ta_dir.join("memory.rvf");
    let fs_path = ta_dir.join("memory");

    // Determine active backend (ruvector wins unless explicitly set to "fs").
    let active_backend = if configured_backend == "fs" {
        "fs"
    } else {
        "ruvector"
    };

    println!("Memory Backend");
    println!();
    println!("  Configured: {}", configured_backend);
    println!("  Active:     {}", active_backend);
    println!();

    // RuVector store info.
    if active_backend == "ruvector" {
        #[cfg(feature = "ruvector")]
        {
            if rvf_path.exists() {
                match ta_memory::RuVectorStore::open(&rvf_path) {
                    Ok(store) => {
                        use ta_memory::MemoryStore;
                        let count = store.list(None).map(|e| e.len()).unwrap_or(0);
                        let size_bytes = dir_size(&rvf_path);
                        println!("  RuVector store: {}", rvf_path.display());
                        println!("  Entries:        {}", count);
                        println!("  Index size:     {}", format_bytes(size_bytes));
                    }
                    Err(e) => {
                        println!("  RuVector store: {} (error: {})", rvf_path.display(), e);
                    }
                }
            } else {
                println!("  RuVector store: not yet initialised");
                println!("  Run `ta run` to initialise the store on first goal start.");
            }
        }
        #[cfg(not(feature = "ruvector"))]
        {
            println!("  RuVector store: not compiled in (ruvector feature disabled)");
        }
    }

    // FsMemoryStore info (legacy / fallback).
    {
        use ta_memory::{FsMemoryStore, MemoryStore};
        if fs_path.exists() {
            let store = FsMemoryStore::new(&fs_path);
            let count = store.list(None).map(|e| e.len()).unwrap_or(0);
            let size_bytes = dir_size(&fs_path);
            println!();
            println!("  Legacy FsMemoryStore: {}", fs_path.display());
            println!("  Entries (not yet migrated): {}", count);
            println!("  Size:   {}", format_bytes(size_bytes));

            if count > 0 && active_backend == "ruvector" {
                println!();
                println!(
                    "  Note: {} legacy entries found. They will be auto-migrated",
                    count
                );
                println!("        the next time RuVectorStore is opened (at goal start).");
            }
        } else {
            println!();
            println!("  Legacy FsMemoryStore: not present");
        }
    }

    Ok(())
}

/// Recursively sum the size of all files in a directory (in bytes).
fn dir_size(path: &std::path::Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    if path.is_file() {
        return path.metadata().map(|m| m.len()).unwrap_or(0);
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| dir_size(&e.path()))
        .sum()
}

/// Format a byte count as a human-readable string.
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn dir_size_nonexistent_returns_zero() {
        let dir = TempDir::new().unwrap();
        let ghost = dir.path().join("ghost");
        assert_eq!(dir_size(&ghost), 0);
    }

    #[test]
    fn dir_size_single_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("data.bin");
        std::fs::write(&file, vec![0u8; 512]).unwrap();
        assert_eq!(dir_size(&file), 512);
    }

    #[test]
    fn format_bytes_variants() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert!(format_bytes(2048).contains("KiB"));
        assert!(format_bytes(3 * 1024 * 1024).contains("MiB"));
    }

    #[test]
    fn show_backend_no_store() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // Should not panic with missing store directories.
        let result = show_backend(&config);
        assert!(result.is_ok());
    }
}
