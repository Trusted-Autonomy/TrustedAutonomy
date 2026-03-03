// context.rs — Context memory subcommands.
//
// Agent-agnostic persistent memory that works across agent frameworks.
// TA owns the memory — agents consume it through MCP tools or CLI.

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_memory::{FsMemoryStore, MemoryQuery, MemoryStore};

#[derive(Subcommand)]
pub enum ContextCommands {
    /// Store a memory entry.
    Store {
        /// Key for the memory entry.
        key: String,
        /// Value to store (JSON string, defaults to the key as a string value).
        #[arg(long)]
        value: Option<String>,
        /// Tags for categorization (repeatable).
        #[arg(long)]
        tag: Vec<String>,
    },
    /// Recall a specific memory entry by key, or search semantically with --semantic.
    Recall {
        /// Key to look up (or query text when using --semantic).
        key: String,
        /// Use semantic search instead of exact key match (requires ruvector backend).
        #[arg(long)]
        semantic: bool,
        /// Maximum results for semantic search.
        #[arg(long, default_value = "5")]
        limit: usize,
    },
    /// List memory entries.
    List {
        /// Filter by tag (repeatable).
        #[arg(long)]
        tag: Vec<String>,
        /// Filter by key prefix.
        #[arg(long)]
        prefix: Option<String>,
        /// Maximum entries to return.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Delete a memory entry by key.
    Forget {
        /// Key to delete.
        key: String,
    },
}

pub fn execute(cmd: &ContextCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    match cmd {
        ContextCommands::Store { key, value, tag } => {
            store_entry(&memory_dir, key, value.as_deref(), tag)
        }
        ContextCommands::Recall {
            key,
            semantic,
            limit,
        } => {
            if *semantic {
                semantic_recall(config, key, *limit)
            } else {
                recall_entry(&memory_dir, key)
            }
        }
        ContextCommands::List { tag, prefix, limit } => {
            list_entries(&memory_dir, tag, prefix.as_deref(), *limit)
        }
        ContextCommands::Forget { key } => forget_entry(&memory_dir, key),
    }
}

fn store_entry(
    memory_dir: &std::path::Path,
    key: &str,
    value: Option<&str>,
    tags: &[String],
) -> anyhow::Result<()> {
    let mut store = FsMemoryStore::new(memory_dir);
    let json_value = match value {
        Some(v) => {
            serde_json::from_str(v).unwrap_or_else(|_| serde_json::Value::String(v.to_string()))
        }
        None => serde_json::Value::String(key.to_string()),
    };

    let entry = store.store(key, json_value, tags.to_vec(), "cli")?;
    println!("Stored memory entry:");
    println!("  Key:  {}", entry.key);
    println!("  ID:   {}", entry.entry_id);
    if !entry.tags.is_empty() {
        println!("  Tags: {}", entry.tags.join(", "));
    }
    Ok(())
}

fn recall_entry(memory_dir: &std::path::Path, key: &str) -> anyhow::Result<()> {
    let store = FsMemoryStore::new(memory_dir);
    match store.recall(key)? {
        Some(entry) => {
            println!("{}", serde_json::to_string_pretty(&entry.value)?);
        }
        None => {
            println!("No memory entry found for key '{}'", key);
        }
    }
    Ok(())
}

fn semantic_recall(config: &GatewayConfig, query: &str, limit: usize) -> anyhow::Result<()> {
    #[cfg(feature = "ruvector")]
    {
        let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
        let store = ta_memory::RuVectorStore::open(&rvf_path)?;

        // Auto-migrate from filesystem if the ruvector store is empty.
        let fs_dir = config.workspace_root.join(".ta").join("memory");
        if fs_dir.exists() {
            let migrated = store.migrate_from_fs(&fs_dir)?;
            if migrated > 0 {
                println!(
                    "Migrated {} entries from filesystem to ruvector.\n",
                    migrated
                );
            }
        }

        let results = store.semantic_search(query, limit)?;
        if results.is_empty() {
            println!("No semantic matches found for '{}'", query);
            return Ok(());
        }

        println!(
            "Semantic search results for '{}' ({}):",
            query,
            results.len()
        );
        println!();
        for e in &results {
            let value_preview = match &e.value {
                serde_json::Value::String(s) if s.len() > 60 => format!("\"{}...\"", &s[..57]),
                v => {
                    let s = v.to_string();
                    if s.len() > 60 {
                        format!("{}...", &s[..57])
                    } else {
                        s
                    }
                }
            };
            println!("  {} = {}", e.key, value_preview);
            if !e.tags.is_empty() {
                println!("    tags: {}", e.tags.join(", "));
            }
        }
        Ok(())
    }

    #[cfg(not(feature = "ruvector"))]
    {
        let _ = (config, query, limit);
        anyhow::bail!(
            "Semantic search requires the ruvector backend.\n\
             Rebuild with: cargo install ta-cli --features ruvector"
        );
    }
}

fn list_entries(
    memory_dir: &std::path::Path,
    tags: &[String],
    prefix: Option<&str>,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    let store = FsMemoryStore::new(memory_dir);

    let entries = if tags.is_empty() && prefix.is_none() {
        store.list(limit)?
    } else {
        store.lookup(MemoryQuery {
            key_prefix: prefix.map(|s| s.to_string()),
            tags: tags.to_vec(),
            goal_id: None,
            limit,
        })?
    };

    if entries.is_empty() {
        println!("No memory entries found.");
        println!();
        println!("Store one with: ta context store <key> --value <json>");
        return Ok(());
    }

    println!("Memory entries ({}):", entries.len());
    println!();
    for e in &entries {
        let value_preview = match &e.value {
            serde_json::Value::String(s) if s.len() > 60 => format!("\"{}...\"", &s[..57]),
            v => {
                let s = v.to_string();
                if s.len() > 60 {
                    format!("{}...", &s[..57])
                } else {
                    s
                }
            }
        };
        println!("  {} = {}", e.key, value_preview);
        if !e.tags.is_empty() {
            println!("    tags: {}", e.tags.join(", "));
        }
    }
    Ok(())
}

fn forget_entry(memory_dir: &std::path::Path, key: &str) -> anyhow::Result<()> {
    let mut store = FsMemoryStore::new(memory_dir);
    if store.forget(key)? {
        println!("Forgot memory entry '{}'", key);
    } else {
        println!("No memory entry found for key '{}'", key);
    }
    Ok(())
}
