// memory.rs — `ta memory` subcommands for inspecting the memory backend (v0.12.5+).
//
// Subcommands:
//   backend  — show the active backend, entry count, and storage size
//   list     — list stored entries (alias for `ta context list`)
//   plugin   — list/probe discovered memory plugins (v0.14.6.5)
//   sync     — push local FsMemoryStore entries to the configured backend (v0.14.6.5)

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand, Debug)]
pub enum MemoryCommands {
    /// Show active memory backend, entry count, and storage size.
    ///
    /// Prints which backend is in use (ruvector, fs, or plugin), how many entries
    /// are stored, and the disk footprint of the memory directory.
    Backend,

    /// List memory entries (alias for `ta context list`).
    List {
        /// Filter by category (e.g., convention, architecture, history).
        #[arg(long, short = 'c')]
        category: Option<String>,
        /// Maximum number of entries to show (default: 50).
        #[arg(long, default_value = "50")]
        limit: usize,
    },

    /// List discovered memory backend plugins and optionally probe them.
    ///
    /// Shows all plugins found in:
    ///   .ta/plugins/memory/
    ///   ~/.config/ta/plugins/memory/
    ///   ta-memory-* on $PATH
    Plugin {
        /// Probe each plugin by sending a `{"op":"stats"}` request and printing the response.
        #[arg(long)]
        probe: bool,
    },

    /// Push all local FsMemoryStore entries to the configured backend.
    ///
    /// Use this when migrating from the default file backend to an external plugin
    /// (e.g., Supermemory, Redis). Reads all entries from `.ta/memory/` and writes
    /// them to the backend configured in `.ta/memory.toml`.
    ///
    /// Use `--dry-run` to see what would be pushed without making changes.
    Sync {
        /// Print what would be pushed without actually writing to the backend.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn execute(command: &MemoryCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        MemoryCommands::Backend => show_backend(config),
        MemoryCommands::List { category, limit } => super::context::execute(
            &super::context::ContextCommands::List {
                tag: vec![],
                prefix: None,
                category: category.clone(),
                limit: Some(*limit),
            },
            config,
        ),
        MemoryCommands::Plugin { probe } => list_plugins(config, *probe),
        MemoryCommands::Sync { dry_run } => sync_to_backend(config, *dry_run),
    }
}

/// `ta memory plugin [--probe]` — list and optionally health-check discovered plugins.
fn list_plugins(config: &GatewayConfig, probe: bool) -> anyhow::Result<()> {
    let plugins = ta_memory::discover_all_memory_plugins(&config.workspace_root);

    if plugins.is_empty() {
        println!("No memory plugins found.");
        println!();
        println!("Search paths:");
        println!("  .ta/plugins/memory/<name>/memory.toml     (project-local)");
        println!("  ~/.config/ta/plugins/memory/<name>/       (user-global)");
        println!("  ta-memory-<name> on $PATH                  (bare binary)");
        println!();
        println!("Reference plugin: plugins/ta-memory-supermemory/ (ships with TA)");
        return Ok(());
    }

    println!("Discovered memory plugins ({}):", plugins.len());
    println!();
    for p in &plugins {
        let m = &p.manifest;
        println!("  {} v{} ({})", m.name, m.version, p.source);
        if let Some(ref desc) = m.description {
            println!("    {}", desc);
        }
        println!("    command:      {}", m.command);
        println!(
            "    capabilities: {}",
            if m.capabilities.is_empty() {
                "none".to_string()
            } else {
                m.capabilities.join(", ")
            }
        );
        if let Some(ref dir) = p.plugin_dir {
            println!("    directory:    {}", dir.display());
        }

        if probe {
            println!("    probe:        ",);
            match probe_plugin(m, &config.workspace_root) {
                Ok(msg) => println!("OK — {}", msg),
                Err(e) => println!("FAILED — {}", e),
            }
        }
        println!();
    }
    Ok(())
}

/// Send `{"op":"stats"}` to a plugin and return a human-readable summary.
fn probe_plugin(
    manifest: &ta_memory::MemoryPluginManifest,
    work_dir: &std::path::Path,
) -> anyhow::Result<String> {
    let ta_version = env!("CARGO_PKG_VERSION");
    let adapter = ta_memory::ExternalMemoryAdapter::new(manifest, work_dir, ta_version)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    use ta_memory::MemoryStore;
    match adapter.stats() {
        Ok(stats) => Ok(format!(
            "{} entries, avg confidence {:.2}",
            stats.total_entries, stats.avg_confidence
        )),
        Err(e) => Err(anyhow::anyhow!("stats error: {}", e)),
    }
}

/// `ta memory sync [--dry-run]` — copy FsMemoryStore entries to the configured backend.
fn sync_to_backend(config: &GatewayConfig, dry_run: bool) -> anyhow::Result<()> {
    use ta_memory::MemoryStore;

    let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
    let backend = memory_config.backend.as_deref().unwrap_or("file");

    if backend == "file" {
        println!(
            "Nothing to sync: backend is already 'file' (FsMemoryStore).\n\
             Set backend = \"plugin\" or backend = \"ruvector\" in .ta/memory.toml first."
        );
        return Ok(());
    }

    // Read all entries from FsMemoryStore.
    let fs_dir = config.workspace_root.join(".ta").join("memory");
    if !fs_dir.exists() {
        println!("Local memory store (.ta/memory/) is empty — nothing to sync.");
        return Ok(());
    }

    let fs_store = ta_memory::FsMemoryStore::new(&fs_dir);
    let entries = fs_store
        .list(None)
        .map_err(|e| anyhow::anyhow!("failed to read local memory store: {}", e))?;

    if entries.is_empty() {
        println!("Local memory store is empty — nothing to sync.");
        return Ok(());
    }

    println!(
        "{} {} {} entries to '{}' backend...",
        if dry_run { "Would push" } else { "Pushing" },
        entries.len(),
        if entries.len() == 1 {
            "entry"
        } else {
            "entries"
        },
        backend
    );

    if dry_run {
        println!();
        for e in &entries {
            let cat = e
                .category
                .as_ref()
                .map(|c| format!("[{}] ", c))
                .unwrap_or_default();
            println!("  {}{}", cat, e.key);
        }
        println!();
        println!("(dry-run: no changes written)");
        return Ok(());
    }

    let mut dest = ta_memory::memory_store_from_config(&config.workspace_root);

    let mut pushed = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        let params = ta_memory::StoreParams {
            goal_id: entry.goal_id,
            category: entry.category.clone(),
            expires_at: entry.expires_at,
            confidence: Some(entry.confidence),
            phase_id: entry.phase_id.clone(),
        };
        match dest.store_with_params(
            &entry.key,
            entry.value.clone(),
            entry.tags.clone(),
            &entry.source,
            params,
        ) {
            Ok(_) => pushed += 1,
            Err(e) => {
                eprintln!("  FAILED {}: {}", entry.key, e);
                failed += 1;
            }
        }
    }

    println!("Sync complete: {} pushed, {} failed.", pushed, failed);
    if failed > 0 {
        anyhow::bail!(
            "{} entries failed to push. Check the error messages above.",
            failed
        );
    }
    Ok(())
}

fn show_backend(config: &GatewayConfig) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
    let configured_backend = memory_config.backend.as_deref().unwrap_or("ruvector");

    let rvf_path = ta_dir.join("memory.rvf");
    let fs_path = ta_dir.join("memory");

    // Determine active backend.
    let active_backend = match configured_backend {
        "file" | "fs" => "file",
        "plugin" => "plugin",
        _ => "ruvector",
    };

    println!("Memory Backend");
    println!();
    println!("  Configured: {}", configured_backend);
    if configured_backend == "plugin" {
        if let Some(ref plugin_name) = memory_config.plugin {
            println!("  Plugin:     {}", plugin_name);
        }
    }
    println!("  Active:     {}", active_backend);

    // Plugin backend info.
    if active_backend == "plugin" {
        if let Some(ref plugin_name) = memory_config.plugin {
            match ta_memory::find_memory_plugin(plugin_name, &config.workspace_root) {
                Some(p) => {
                    println!();
                    println!("  Plugin binary:    {} ({})", p.manifest.command, p.source);
                    println!("  Plugin version:   {}", p.manifest.version);
                    println!(
                        "  Capabilities:     {}",
                        if p.manifest.capabilities.is_empty() {
                            "none".to_string()
                        } else {
                            p.manifest.capabilities.join(", ")
                        }
                    );
                    println!("  Run `ta memory plugin --probe` to health-check.");
                }
                None => {
                    println!();
                    println!(
                        "  Plugin '{}' not found. Install it or check .ta/memory.toml.",
                        plugin_name
                    );
                    println!(
                        "  Search: .ta/plugins/memory/{name}/, ta-memory-{name} on $PATH",
                        name = plugin_name
                    );
                }
            }
        } else {
            println!();
            println!("  No plugin name configured. Set `plugin = \"<name>\"` in .ta/memory.toml.");
        }
        return Ok(());
    }
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
