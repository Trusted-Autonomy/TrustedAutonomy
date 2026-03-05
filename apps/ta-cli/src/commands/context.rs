// context.rs — Context memory subcommands.
//
// Agent-agnostic persistent memory that works across agent frameworks.
// TA owns the memory — agents consume it through MCP tools or CLI.

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_memory::{FsMemoryStore, KeySchema, MemoryQuery, MemoryStore};

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
        /// Knowledge category (convention, architecture, history, preference, relationship).
        #[arg(long)]
        category: Option<String>,
        /// Entry expires after this duration (e.g., "30d", "12h", "90m").
        #[arg(long)]
        expires_in: Option<String>,
        /// Confidence score 0.0–1.0 (default: 0.5).
        #[arg(long)]
        confidence: Option<f64>,
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
    /// Semantic search across memory entries (v0.5.7).
    Search {
        /// Query text for semantic similarity search.
        query: String,
        /// Maximum results to return.
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// Find entries similar to a given entry by ID (v0.5.7).
    Similar {
        /// Entry ID (UUID) to find similar entries for.
        entry_id: String,
        /// Maximum results to return.
        #[arg(long, default_value = "5")]
        limit: usize,
    },
    /// Show provenance and metadata for a memory entry (v0.5.7).
    Explain {
        /// Entry key or ID (UUID) to explain.
        entry: String,
    },
    /// Show memory store statistics (v0.5.7).
    Stats,
    /// List memory entries.
    List {
        /// Filter by tag (repeatable).
        #[arg(long)]
        tag: Vec<String>,
        /// Filter by key prefix.
        #[arg(long)]
        prefix: Option<String>,
        /// Filter by category (convention, architecture, history, preference, relationship).
        #[arg(long)]
        category: Option<String>,
        /// Maximum entries to return.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Delete a memory entry by key.
    Forget {
        /// Key to delete.
        key: String,
    },
    /// Show the project's key schema and domain mapping (v0.6.3).
    Schema,
    /// Export memory entries to a curated solutions.toml file (v0.8.1).
    Export {
        /// Output path (default: .ta/solutions/solutions.toml).
        #[arg(long)]
        output: Option<String>,
        /// Skip interactive confirmation.
        #[arg(long)]
        non_interactive: bool,
    },
    /// Import solutions from a local file or URL (v0.8.1).
    Import {
        /// Path or URL to a solutions.toml file.
        source: String,
    },
}

pub fn execute(cmd: &ContextCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    match cmd {
        ContextCommands::Store {
            key,
            value,
            tag,
            category,
            expires_in,
            confidence,
        } => store_entry(
            &memory_dir,
            key,
            value.as_deref(),
            tag,
            category.as_deref(),
            expires_in.as_deref(),
            *confidence,
        ),
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
        ContextCommands::Search { query, limit } => semantic_recall(config, query, *limit),
        ContextCommands::Similar { entry_id, limit } => find_similar(config, entry_id, *limit),
        ContextCommands::Explain { entry } => explain_entry(&memory_dir, entry),
        ContextCommands::Stats => show_stats(&memory_dir),
        ContextCommands::List {
            tag,
            prefix,
            category,
            limit,
        } => list_entries(
            &memory_dir,
            tag,
            prefix.as_deref(),
            category.as_deref(),
            *limit,
        ),
        ContextCommands::Forget { key } => forget_entry(&memory_dir, key),
        ContextCommands::Schema => show_schema(config),
        ContextCommands::Export {
            output,
            non_interactive,
        } => export_solutions(config, output.as_deref(), *non_interactive),
        ContextCommands::Import { source } => import_solutions(config, source),
    }
}

fn parse_duration(s: &str) -> anyhow::Result<chrono::Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration");
    }
    let (num_str, unit) = s.split_at(s.len() - 1);
    let n: i64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid number in duration '{}'", s))?;
    match unit {
        "d" => Ok(chrono::Duration::days(n)),
        "h" => Ok(chrono::Duration::hours(n)),
        "m" => Ok(chrono::Duration::minutes(n)),
        _ => anyhow::bail!(
            "unknown duration unit '{}'. Use d (days), h (hours), or m (minutes)",
            unit
        ),
    }
}

fn store_entry(
    memory_dir: &std::path::Path,
    key: &str,
    value: Option<&str>,
    tags: &[String],
    category: Option<&str>,
    expires_in: Option<&str>,
    confidence: Option<f64>,
) -> anyhow::Result<()> {
    let mut store = FsMemoryStore::new(memory_dir);
    let json_value = match value {
        Some(v) => {
            serde_json::from_str(v).unwrap_or_else(|_| serde_json::Value::String(v.to_string()))
        }
        None => serde_json::Value::String(key.to_string()),
    };

    let expires_at = match expires_in {
        Some(d) => Some(chrono::Utc::now() + parse_duration(d)?),
        None => None,
    };

    let params = ta_memory::StoreParams {
        goal_id: None,
        category: category.map(ta_memory::MemoryCategory::from_str_lossy),
        expires_at,
        confidence,
        ..Default::default()
    };

    let entry = store.store_with_params(key, json_value, tags.to_vec(), "cli", params)?;
    println!("Stored memory entry:");
    println!("  Key:        {}", entry.key);
    println!("  ID:         {}", entry.entry_id);
    println!("  Confidence: {:.1}", entry.confidence);
    if let Some(cat) = &entry.category {
        println!("  Category:   {}", cat);
    }
    if let Some(exp) = &entry.expires_at {
        println!("  Expires:    {}", exp.format("%Y-%m-%d %H:%M UTC"));
    }
    if !entry.tags.is_empty() {
        println!("  Tags:       {}", entry.tags.join(", "));
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
            print_entry_summary(&e);
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

fn find_similar(config: &GatewayConfig, entry_id: &str, limit: usize) -> anyhow::Result<()> {
    #[cfg(feature = "ruvector")]
    {
        let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
        let store = ta_memory::RuVectorStore::open(&rvf_path)?;

        let uuid: uuid::Uuid = entry_id
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid entry ID '{}'. Expected a UUID.", entry_id))?;

        let entry = store
            .find_by_id(uuid)?
            .ok_or_else(|| anyhow::anyhow!("no memory entry found with ID '{}'", entry_id))?;

        // Use the entry's value as the semantic query.
        let query_text = match &entry.value {
            serde_json::Value::String(s) => s.clone(),
            v => v.to_string(),
        };

        let results = store.semantic_search(&query_text, limit + 1)?;
        // Filter out the original entry.
        let similar: Vec<_> = results
            .into_iter()
            .filter(|e| e.entry_id != uuid)
            .take(limit)
            .collect();

        if similar.is_empty() {
            println!("No similar entries found for '{}'", entry.key);
            return Ok(());
        }

        println!("Entries similar to '{}' ({}):", entry.key, similar.len());
        println!();
        for e in &similar {
            print_entry_summary(e);
        }
        Ok(())
    }

    #[cfg(not(feature = "ruvector"))]
    {
        let _ = (config, entry_id, limit);
        anyhow::bail!(
            "Similar entry search requires the ruvector backend.\n\
             Rebuild with: cargo install ta-cli --features ruvector"
        );
    }
}

fn explain_entry(memory_dir: &std::path::Path, entry_key_or_id: &str) -> anyhow::Result<()> {
    let store = FsMemoryStore::new(memory_dir);

    // Try exact key first, then UUID lookup.
    let entry = if let Some(e) = store.recall(entry_key_or_id)? {
        e
    } else if let Ok(uuid) = entry_key_or_id.parse::<uuid::Uuid>() {
        store
            .find_by_id(uuid)?
            .ok_or_else(|| anyhow::anyhow!("no entry found for '{}'", entry_key_or_id))?
    } else {
        anyhow::bail!("no entry found for '{}'", entry_key_or_id);
    };

    println!("Memory Entry Provenance");
    println!("{}", "=".repeat(50));
    println!("  Key:        {}", entry.key);
    println!("  ID:         {}", entry.entry_id);
    println!("  Source:     {}", entry.source);
    if let Some(cat) = &entry.category {
        println!("  Category:   {}", cat);
    }
    if let Some(goal_id) = &entry.goal_id {
        println!("  Goal ID:    {}", goal_id);
    }
    println!("  Confidence: {:.2}", entry.confidence);
    println!(
        "  Created:    {}",
        entry.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!(
        "  Updated:    {}",
        entry.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    if let Some(exp) = &entry.expires_at {
        let now = chrono::Utc::now();
        if *exp < now {
            println!(
                "  Expires:    {} (EXPIRED)",
                exp.format("%Y-%m-%d %H:%M UTC")
            );
        } else {
            println!("  Expires:    {}", exp.format("%Y-%m-%d %H:%M UTC"));
        }
    }
    if !entry.tags.is_empty() {
        println!("  Tags:       {}", entry.tags.join(", "));
    }
    println!();
    println!("Value:");
    println!("{}", serde_json::to_string_pretty(&entry.value)?);
    Ok(())
}

fn show_stats(memory_dir: &std::path::Path) -> anyhow::Result<()> {
    let store = FsMemoryStore::new(memory_dir);
    let stats = store.stats()?;

    println!("Memory Store Statistics");
    println!("{}", "=".repeat(50));
    println!("  Total entries:    {}", stats.total_entries);
    println!("  Expired entries:  {}", stats.expired_count);
    println!("  Avg confidence:   {:.2}", stats.avg_confidence);

    if let Some(oldest) = stats.oldest_entry {
        println!(
            "  Oldest entry:     {}",
            oldest.format("%Y-%m-%d %H:%M UTC")
        );
    }
    if let Some(newest) = stats.newest_entry {
        println!(
            "  Newest entry:     {}",
            newest.format("%Y-%m-%d %H:%M UTC")
        );
    }

    if !stats.by_category.is_empty() {
        println!();
        println!("  By category:");
        let mut cats: Vec<_> = stats.by_category.iter().collect();
        cats.sort_by(|a, b| b.1.cmp(a.1));
        for (cat, count) in cats {
            println!("    {:<16} {}", cat, count);
        }
    }

    if !stats.by_source.is_empty() {
        println!();
        println!("  By source:");
        let mut srcs: Vec<_> = stats.by_source.iter().collect();
        srcs.sort_by(|a, b| b.1.cmp(a.1));
        for (src, count) in srcs {
            println!("    {:<16} {}", src, count);
        }
    }
    Ok(())
}

fn list_entries(
    memory_dir: &std::path::Path,
    tags: &[String],
    prefix: Option<&str>,
    category: Option<&str>,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    let store = FsMemoryStore::new(memory_dir);

    let entries = if tags.is_empty() && prefix.is_none() && category.is_none() {
        store.list(limit)?
    } else {
        store.lookup(MemoryQuery {
            key_prefix: prefix.map(|s| s.to_string()),
            tags: tags.to_vec(),
            goal_id: None,
            category: category.map(ta_memory::MemoryCategory::from_str_lossy),
            limit,
            ..Default::default()
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
        print_entry_summary(e);
    }
    Ok(())
}

fn print_entry_summary(e: &ta_memory::MemoryEntry) {
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
    let cat_label = e
        .category
        .as_ref()
        .map(|c| format!("[{}] ", c))
        .unwrap_or_default();
    println!("  {}{} = {}", cat_label, e.key, value_preview);
    if !e.tags.is_empty() {
        println!("    tags: {}", e.tags.join(", "));
    }
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

fn export_solutions(
    config: &GatewayConfig,
    output: Option<&str>,
    non_interactive: bool,
) -> anyhow::Result<()> {
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let store = FsMemoryStore::new(&memory_dir);

    // Gather NegativePath and Convention entries.
    let negative = store.lookup(MemoryQuery {
        category: Some(ta_memory::MemoryCategory::NegativePath),
        ..Default::default()
    })?;
    let convention = store.lookup(MemoryQuery {
        category: Some(ta_memory::MemoryCategory::Convention),
        ..Default::default()
    })?;

    let all_entries: Vec<_> = negative.into_iter().chain(convention).collect();

    if all_entries.is_empty() {
        println!("No NegativePath or Convention memory entries found to export.");
        return Ok(());
    }

    // Convert memory entries to solution entries.
    let solution_store_path = match output {
        Some(p) => std::path::PathBuf::from(p),
        None => config
            .workspace_root
            .join(".ta")
            .join("solutions")
            .join("solutions.toml"),
    };

    let schema = KeySchema::resolve(&config.workspace_root);
    let language = match schema.project_type {
        ta_memory::ProjectType::RustWorkspace => Some("rust".to_string()),
        ta_memory::ProjectType::TypeScript => Some("typescript".to_string()),
        ta_memory::ProjectType::Python => Some("python".to_string()),
        ta_memory::ProjectType::Go => Some("go".to_string()),
        ta_memory::ProjectType::Generic => None,
    };

    let sol_store = ta_memory::SolutionStore::new(&solution_store_path);

    println!("Found {} memory entries to export:", all_entries.len());
    println!();

    let mut solutions = Vec::new();
    let mut idx: u32 = 0;
    for entry in &all_entries {
        idx += 1;
        let value_text = match &entry.value {
            serde_json::Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        let category_str = entry
            .category
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "other".to_string());

        // Strip UUIDs from the value text.
        let cleaned = strip_uuids(&value_text);

        let sol = ta_memory::SolutionEntry {
            id: format!("sol_{:03}", idx),
            problem: entry.key.clone(),
            solution: cleaned,
            context: ta_memory::SolutionContext {
                language: language.clone(),
                framework: None,
            },
            tags: entry.tags.clone(),
            source_category: Some(category_str.clone()),
            created_at: entry.created_at,
        };

        println!(
            "  [{}] {} = {}",
            category_str,
            entry.key,
            truncate(&value_text, 60)
        );
        solutions.push(sol);
    }

    if !non_interactive {
        println!();
        print!(
            "Export {} entries to {}? [y/N] ",
            solutions.len(),
            solution_store_path.display()
        );
        use std::io::Write;
        std::io::stdout().flush()?;

        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Export cancelled.");
            return Ok(());
        }
    }

    let (new_count, dup_count) = sol_store.merge(&solutions)?;
    println!(
        "Exported: {} new, {} duplicate(s) skipped. File: {}",
        new_count,
        dup_count,
        solution_store_path.display()
    );

    Ok(())
}

fn import_solutions(config: &GatewayConfig, source: &str) -> anyhow::Result<()> {
    let content = if source.starts_with("http://") || source.starts_with("https://") {
        anyhow::bail!(
            "URL import is not yet supported. Download the file first and pass a local path."
        );
    } else {
        std::fs::read_to_string(source)
            .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", source, e))?
    };

    // Parse the incoming solutions file.
    let solution_store_path = config
        .workspace_root
        .join(".ta")
        .join("solutions")
        .join("solutions.toml");
    let sol_store = ta_memory::SolutionStore::new(&solution_store_path);

    // Write content to a temp file, parse, then clean up.
    let tmp_path = std::env::temp_dir().join(format!("ta_import_{}.toml", std::process::id()));
    std::fs::write(&tmp_path, &content)?;
    let tmp_store = ta_memory::SolutionStore::new(&tmp_path);
    let incoming = tmp_store.load()?;
    let _ = std::fs::remove_file(&tmp_path);

    if incoming.is_empty() {
        println!("No solution entries found in '{}'.", source);
        return Ok(());
    }

    let (new_count, dup_count) = sol_store.merge(&incoming)?;
    println!(
        "Imported from '{}': {} new, {} duplicate(s) skipped.",
        source, new_count, dup_count
    );
    println!("Solutions file: {}", solution_store_path.display());

    Ok(())
}

fn strip_uuids(s: &str) -> String {
    // Simple UUID pattern stripping: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
    let re = regex::Regex::new(
        r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
    )
    .unwrap();
    re.replace_all(s, "<id>").to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn show_schema(config: &GatewayConfig) -> anyhow::Result<()> {
    let schema = KeySchema::resolve(&config.workspace_root);

    println!("Memory Key Schema (v0.6.3)");
    println!("{}", "=".repeat(50));
    println!("  Project type:  {}", schema.project_type);
    println!("  Backend:       {}", schema.backend);
    println!();
    println!("  Key domains:");
    println!("    Module map:  arch:{}", schema.domains.module_map);
    println!("    Module:      arch:{}:<name>", schema.domains.module);
    println!(
        "    Type system: arch:{}:<name>",
        schema.domains.type_system
    );
    println!("    Build tool:  arch:{}:<name>", schema.domains.build_tool);
    println!();
    println!("  Special keys:");
    println!("    Negative:    neg:<phase>:<slug>");
    println!("    State:       state:<topic>");
    println!();
    println!("  Configure: .ta/memory.toml (optional)");
    Ok(())
}
