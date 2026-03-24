// community.rs — `ta community` CLI commands (v0.13.6 Community Knowledge Hub).
//
// Provides:
//   - `ta community list`        — show configured resources with status
//   - `ta community sync [name]` — refresh local cache(s)
//   - `ta community search <q>`  — search across resources
//   - `ta community get <id>`    — fetch and display a document

use std::io::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use ta_mcp_gateway::GatewayConfig;

/// Access level parsed from TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Access {
    #[default]
    ReadOnly,
    ReadWrite,
    Disabled,
}

impl std::fmt::Display for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Access::ReadOnly => write!(f, "read-only"),
            Access::ReadWrite => write!(f, "read-write"),
            Access::Disabled => write!(f, "disabled"),
        }
    }
}

/// A single configured community resource (mirrors plugin registry).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Resource {
    name: String,
    intent: String,
    description: String,
    source: String,
    #[serde(default = "default_content_path")]
    content_path: String,
    #[serde(default)]
    access: Access,
    #[serde(default)]
    auto_query: bool,
    #[serde(default)]
    pre_inject: bool,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default = "default_update_frequency")]
    update_frequency: String,
}

fn default_content_path() -> String {
    "content/".to_string()
}

fn default_update_frequency() -> String {
    "on-demand".to_string()
}

/// Cache metadata stored in `.ta/community-cache/<name>/_meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheMetadata {
    resource_name: String,
    synced_at: DateTime<Utc>,
    source: String,
    document_count: usize,
}

/// Full community registry loaded from `.ta/community-resources.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    resources: Vec<Resource>,
}

impl Registry {
    fn load(workspace: &Path) -> anyhow::Result<Self> {
        let path = workspace.join(".ta").join("community-resources.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(toml::from_str::<Self>(&content)?)
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum CommunityCommands {
    /// List configured community resources with access level and cache status.
    List {
        /// Output raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Sync the local cache for one or all community resources.
    ///
    /// For `local:` sources, re-indexes the local path.
    /// For `github:` sources, downloads files from the GitHub API.
    /// Requires GITHUB_TOKEN env var for GitHub resources (optional but recommended
    /// to avoid rate limits).
    Sync {
        /// Name of the resource to sync. If omitted, syncs all enabled resources.
        /// Tip: use `ta community list --json | jq -r '.[].name'` in a shell completion
        /// script to enumerate valid resource names dynamically.
        #[arg(value_hint = clap::ValueHint::Other)]
        resource: Option<String>,
        /// Output raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Search across community resources.
    ///
    /// Example: `ta community search "stripe payment intents"`
    Search {
        /// Search query.
        query: String,
        /// Filter to resources with this intent (e.g., "api-integration").
        #[arg(long)]
        intent: Option<String>,
        /// Filter to a specific resource by name.
        /// Tip: use `ta community list --json | jq -r '.[].name'` for valid names.
        #[arg(long, value_hint = clap::ValueHint::Other)]
        resource: Option<String>,
        /// Output raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Fetch and display a document by ID.
    ///
    /// Document IDs take the form `<resource-name>/<path>`, e.g.:
    ///   `ta community get api-docs/stripe`
    Get {
        /// Document ID (resource-name/path).
        /// Tip: use `ta community list --json | jq -r '.[].name'` for valid resource names.
        #[arg(value_hint = clap::ValueHint::Other)]
        id: String,
    },
}

pub fn execute(cmd: &CommunityCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let workspace = &config.workspace_root;
    match cmd {
        CommunityCommands::List { json } => cmd_list(workspace, *json),
        CommunityCommands::Sync { resource, json } => {
            cmd_sync(workspace, resource.as_deref(), *json)
        }
        CommunityCommands::Search {
            query,
            intent,
            resource,
            json,
        } => cmd_search(
            workspace,
            query,
            intent.as_deref(),
            resource.as_deref(),
            *json,
        ),
        CommunityCommands::Get { id } => cmd_get(workspace, id),
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

fn cmd_list(workspace: &Path, json: bool) -> anyhow::Result<()> {
    let registry = Registry::load(workspace)?;

    if registry.resources.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No community resources configured.");
            println!(
                "Create .ta/community-resources.toml to configure resources. See `ta community --help`."
            );
        }
        return Ok(());
    }

    let cache_root = workspace.join(".ta").join("community-cache");

    if json {
        let items: Vec<serde_json::Value> = registry
            .resources
            .iter()
            .map(|r| {
                let meta = load_cache_meta(&cache_root, &r.name);
                serde_json::json!({
                    "name": r.name,
                    "intent": r.intent,
                    "description": r.description,
                    "source": r.source,
                    "access": r.access.to_string(),
                    "auto_query": r.auto_query,
                    "update_frequency": r.update_frequency,
                    "synced_at": meta.as_ref().map(|m| m.synced_at.to_rfc3339()),
                    "cached_docs": meta.as_ref().map(|m| m.document_count).unwrap_or(0),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    println!("Community Resources");
    println!("{}", "─".repeat(70));

    for r in &registry.resources {
        let meta = load_cache_meta(&cache_root, &r.name);
        let status = if r.access == Access::Disabled {
            "disabled".to_string()
        } else {
            match &meta {
                Some(m) => {
                    let age = Utc::now() - m.synced_at;
                    if age.num_days() > 90 {
                        format!(
                            "⚠ stale ({} docs, {}d ago)",
                            m.document_count,
                            age.num_days()
                        )
                    } else {
                        format!(
                            "✓ synced ({} docs, {}d ago)",
                            m.document_count,
                            age.num_days()
                        )
                    }
                }
                None => "not synced".to_string(),
            }
        };

        let auto_label = if r.auto_query { " [auto]" } else { "" };
        println!(
            "  {:<20} {:<20} {:<12}  {}{}",
            r.name,
            r.intent,
            r.access.to_string(),
            status,
            auto_label
        );
        println!("    {} | {}", r.description, r.source);
    }

    println!();
    println!("Use `ta community sync` to refresh cached content.");
    println!("Use `ta community search <query>` to search across resources.");
    Ok(())
}

fn cmd_sync(workspace: &Path, resource_filter: Option<&str>, json: bool) -> anyhow::Result<()> {
    let registry = Registry::load(workspace)?;

    let to_sync: Vec<&Resource> = if let Some(name) = resource_filter {
        match registry.resources.iter().find(|r| r.name == name) {
            Some(r) => vec![r],
            None => {
                anyhow::bail!(
                    "resource '{}' not found in .ta/community-resources.toml. \
                     Run `ta community list` to see configured resources.",
                    name
                );
            }
        }
    } else {
        registry
            .resources
            .iter()
            .filter(|r| r.access != Access::Disabled)
            .collect()
    };

    if to_sync.is_empty() {
        println!("No enabled resources to sync.");
        return Ok(());
    }

    let mut synced = Vec::new();
    let mut errors = Vec::new();

    for resource in &to_sync {
        if !json {
            print!("  Syncing {} ({})... ", resource.name, resource.source);
            let _ = std::io::stdout().flush();
        }
        match sync_resource(workspace, resource) {
            Ok(count) => {
                synced.push(serde_json::json!({
                    "name": resource.name,
                    "documents": count,
                    "synced_at": Utc::now().to_rfc3339(),
                }));
                if !json {
                    println!("{} doc(s)", count);
                }
            }
            Err(e) => {
                errors.push(serde_json::json!({
                    "name": resource.name,
                    "error": e.to_string(),
                }));
                if !json {
                    println!("ERROR: {}", e);
                }
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "synced": synced,
                "errors": errors,
            }))?
        );
        return Ok(());
    }

    println!();
    println!(
        "Synced {} resource(s). {} error(s).",
        synced.len(),
        errors.len()
    );
    if !errors.is_empty() {
        println!("Errors:");
        for e in &errors {
            println!(
                "  {}: {}",
                e["name"].as_str().unwrap_or("?"),
                e["error"].as_str().unwrap_or("?")
            );
        }
    }
    Ok(())
}

fn cmd_search(
    workspace: &Path,
    query: &str,
    intent: Option<&str>,
    resource_filter: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let registry = Registry::load(workspace)?;
    let cache_root = workspace.join(".ta").join("community-cache");

    let candidates: Vec<&Resource> = if let Some(name) = resource_filter {
        registry
            .resources
            .iter()
            .filter(|r| r.name == name && r.access != Access::Disabled)
            .collect()
    } else if let Some(i) = intent {
        registry
            .resources
            .iter()
            .filter(|r| r.intent == i && r.access != Access::Disabled)
            .collect()
    } else {
        registry
            .resources
            .iter()
            .filter(|r| r.access != Access::Disabled)
            .collect()
    };

    let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();

    let mut results: Vec<(String, String, String, usize)> = Vec::new(); // (resource, id, excerpt, score)

    for resource in &candidates {
        let resource_cache = cache_root.join(&resource.name);
        if !resource_cache.exists() {
            continue;
        }
        scan_for_matches(
            &resource_cache,
            &resource_cache,
            &resource.name,
            &words,
            &mut results,
        );
    }

    results.sort_by(|a, b| b.3.cmp(&a.3));
    results.truncate(20);

    if json {
        let items: Vec<serde_json::Value> = results
            .iter()
            .map(|(res, id, excerpt, score)| {
                serde_json::json!({
                    "resource": res,
                    "id": id,
                    "excerpt": excerpt,
                    "score": score,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No results found for '{}'.", query);
        println!("Tip: run `ta community sync` if your cache is empty.");
        return Ok(());
    }

    println!("Search results for '{}'", query);
    println!("{}", "─".repeat(70));
    for (resource, id, excerpt, _) in &results {
        println!("  [{}] {}", resource, id);
        let preview: String = excerpt.chars().take(120).collect();
        println!("    {}", preview.trim());
        println!();
    }
    Ok(())
}

fn cmd_get(workspace: &Path, id: &str) -> anyhow::Result<()> {
    let parts: Vec<&str> = id.splitn(2, '/').collect();
    if parts.len() < 2 {
        anyhow::bail!(
            "Invalid document ID '{}'. Use format: <resource-name>/<path>, e.g. api-docs/stripe",
            id
        );
    }
    let resource_name = parts[0];
    let rel_path = parts[1];

    let cache_root = workspace
        .join(".ta")
        .join("community-cache")
        .join(resource_name);

    // Try exact path and with .md extension.
    let candidates = [
        cache_root.join(rel_path),
        cache_root.join(format!("{}.md", rel_path)),
    ];

    for path in &candidates {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;

            // Load sync metadata for freshness info.
            let meta_path = cache_root.join("_meta.json");
            let meta: Option<CacheMetadata> = std::fs::read_to_string(&meta_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());

            if let Some(m) = &meta {
                let age = Utc::now() - m.synced_at;
                if age.num_days() > 90 {
                    eprintln!(
                        "⚠  This document may be outdated (last synced {} days ago). \
                         Run `ta community sync {}` to refresh.",
                        age.num_days(),
                        resource_name
                    );
                } else {
                    eprintln!("[community: {} v{}d ago]", id, age.num_days());
                }
            }

            println!("{}", content);
            return Ok(());
        }
    }

    anyhow::bail!(
        "Document '{}' not found in local cache. \
         Run `ta community sync {}` to populate the cache, \
         then verify the ID with `ta community search`.",
        id,
        resource_name
    )
}

// ---------------------------------------------------------------------------
// Sync helpers
// ---------------------------------------------------------------------------

fn sync_resource(workspace: &Path, resource: &Resource) -> anyhow::Result<usize> {
    if let Some(local_rel) = resource.source.strip_prefix("local:") {
        let local_base = workspace.join(local_rel);
        if !local_base.exists() {
            anyhow::bail!(
                "local path '{}' does not exist. Create it or update source in community-resources.toml.",
                local_base.display()
            );
        }

        let cache_dir = workspace
            .join(".ta")
            .join("community-cache")
            .join(&resource.name);
        std::fs::create_dir_all(&cache_dir)?;

        let mut count = 0;
        let mut docs = Vec::new();
        collect_local_docs(&local_base, &local_base, &mut docs)?;
        for (rel_path, content) in &docs {
            let dest = cache_dir.join(rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, content)?;
            count += 1;
        }

        // Write metadata.
        let meta = CacheMetadata {
            resource_name: resource.name.clone(),
            synced_at: Utc::now(),
            source: resource.source.clone(),
            document_count: count,
        };
        let meta_path = cache_dir.join("_meta.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
        return Ok(count);
    }

    if resource.source.starts_with("github:") {
        return sync_github_resource(workspace, resource);
    }

    anyhow::bail!(
        "Unknown source format '{}'. Supported: 'github:<owner>/<repo>' or 'local:<path>'.",
        resource.source
    )
}

fn sync_github_resource(workspace: &Path, resource: &Resource) -> anyhow::Result<usize> {
    let rest = resource
        .source
        .strip_prefix("github:")
        .unwrap_or(&resource.source);
    let (owner, repo) = rest.split_once('/').ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid GitHub source '{}'. Expected format: github:<owner>/<repo>",
            resource.source
        )
    })?;

    let content_path = resource.content_path.trim_matches('/');
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/contents/{}",
        owner, repo, content_path
    );

    // Use `curl` as the HTTP client to avoid adding reqwest to the CLI's
    // dependency tree just for this command. The heavy GitHub sync is
    // intentionally thin — production use should configure a local mirror
    // or a dedicated fetcher.
    let token_arg = std::env::var("GITHUB_TOKEN").ok();
    let mut cmd = std::process::Command::new("curl");
    cmd.arg("-fsSL")
        .arg("--user-agent")
        .arg("ta-community-hub/0.1")
        .arg("--header")
        .arg("Accept: application/vnd.github+json");

    if let Some(token) = &token_arg {
        cmd.arg("--header")
            .arg(format!("Authorization: Bearer {}", token));
    }

    cmd.arg(&api_url);

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run curl: {}. Ensure curl is installed.", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "GitHub API request failed for {}/{} (path: {}): {}. \
             Set GITHUB_TOKEN to avoid rate limits.",
            owner,
            repo,
            content_path,
            stderr.trim()
        );
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<serde_json::Value> = serde_json::from_str(&body).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse GitHub API response: {}. Response: {}",
            e,
            body.chars().take(500).collect::<String>()
        )
    })?;

    let cache_dir = workspace
        .join(".ta")
        .join("community-cache")
        .join(&resource.name);
    std::fs::create_dir_all(&cache_dir)?;

    let mut count = 0;
    for entry in &entries {
        if entry.get("type").and_then(|v| v.as_str()) == Some("file") {
            let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !name.ends_with(".md") {
                continue;
            }
            let download_url = match entry.get("download_url").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => continue,
            };

            let file_output = std::process::Command::new("curl")
                .arg("-fsSL")
                .arg("--user-agent")
                .arg("ta-community-hub/0.1")
                .arg(download_url)
                .output();

            match file_output {
                Ok(o) if o.status.success() => {
                    let content = String::from_utf8_lossy(&o.stdout).to_string();
                    let dest = cache_dir.join(name);
                    std::fs::write(&dest, &content)?;
                    count += 1;
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    eprintln!("Warning: failed to download {}: {}", name, stderr.trim());
                }
                Err(e) => {
                    eprintln!("Warning: failed to spawn curl for {}: {}", name, e);
                }
            }
        }
    }

    let meta = CacheMetadata {
        resource_name: resource.name.clone(),
        synced_at: Utc::now(),
        source: resource.source.clone(),
        document_count: count,
    };
    std::fs::write(
        cache_dir.join("_meta.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;

    Ok(count)
}

fn collect_local_docs(
    base: &Path,
    dir: &Path,
    out: &mut Vec<(String, String)>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_local_docs(base, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let rel = path
                .strip_prefix(base)
                .map_err(|_| anyhow::anyhow!("strip_prefix failed"))?;
            let content = std::fs::read_to_string(&path)?;
            out.push((rel.to_string_lossy().to_string(), content));
        }
    }
    Ok(())
}

fn scan_for_matches(
    root: &Path,
    dir: &Path,
    resource_name: &str,
    words: &[String],
    out: &mut Vec<(String, String, String, usize)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_for_matches(root, &path, resource_name, words, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if path.file_name().and_then(|n| n.to_str()) == Some("_meta.json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lower = content.to_lowercase();
            let score: usize = words
                .iter()
                .map(|w| lower.matches(w.as_str()).count())
                .sum();
            if score == 0 {
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().to_string();
            let id_rel = rel_str.trim_end_matches(".md").to_string();
            let id = format!("{}/{}", resource_name, id_rel);

            let start = words
                .iter()
                .filter_map(|w| lower.find(w.as_str()))
                .min()
                .unwrap_or(0);
            let begin = start.saturating_sub(50);
            let chars: Vec<char> = content.chars().collect();
            let end = (begin + 200).min(chars.len());
            let excerpt: String = chars[begin..end].iter().collect();

            out.push((resource_name.to_string(), id, excerpt, score));
        }
    }
}

// ---------------------------------------------------------------------------
// Cache metadata helper
// ---------------------------------------------------------------------------

fn load_cache_meta(cache_root: &Path, name: &str) -> Option<CacheMetadata> {
    let path = cache_root.join(name).join("_meta.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

// ---------------------------------------------------------------------------
// Context injection helper (used by run.rs)
// ---------------------------------------------------------------------------

/// Build the community resources section injected into CLAUDE.md when
/// `auto_query = true` resources are configured.
///
/// Resources with `pre_inject = true` get a full guidance block (opt-in, previous
/// behaviour). Resources with `pre_inject = false` (the new default) produce a
/// compact single-paragraph note that stays well under 200 tokens regardless of
/// how many resources are registered.
///
/// Returns an empty string if no auto_query resources exist or no registry
/// is configured.
pub fn build_community_context_section(workspace: &Path) -> String {
    let registry = match Registry::load(workspace) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };

    // Collect resources that are auto_query and not disabled.
    let auto_resources: Vec<&Resource> = registry
        .resources
        .iter()
        .filter(|r| r.access != Access::Disabled && r.auto_query)
        .collect();

    if auto_resources.is_empty() {
        return String::new();
    }

    // Separate pre_inject resources (full guidance block) from compact-note resources.
    let pre_inject_resources: Vec<&Resource> = auto_resources
        .iter()
        .copied()
        .filter(|r| r.pre_inject)
        .collect();
    let compact_resources: Vec<&Resource> = auto_resources
        .iter()
        .copied()
        .filter(|r| !r.pre_inject)
        .collect();

    let mut output = String::new();

    // Compact community tools note for non-pre_inject resources.
    // Token budget: under 200 tokens regardless of registry size.
    if !compact_resources.is_empty() {
        let resource_list: Vec<String> = compact_resources
            .iter()
            .map(|r| format!("{} ({})", r.name, r.intent))
            .collect();
        output.push_str(&format!(
            "\n# Community Knowledge (MCP)\nAvailable tools: community_search, community_get, community_annotate.\nResources: {}. Use community_search before making API calls or reviewing security-sensitive code.\n",
            resource_list.join(", ")
        ));
    }

    // Full guidance block for pre_inject = true resources (opt-in, legacy behaviour).
    if !pre_inject_resources.is_empty() {
        output.push_str("\n## Community Knowledge Resources (pre-loaded)\n");
        output.push_str(
            "The following community knowledge resources are available via the \
             `ta-community-hub` plugin. Use them before making API calls or \
             security-sensitive decisions:\n",
        );

        for r in &pre_inject_resources {
            output.push_str(&format!(
                "\n- **{}** (`intent: {}`): {}",
                r.name, r.intent, r.description
            ));
            if r.intent.contains("api") {
                output.push_str(&format!(
                    "\n  → Before calling a third-party API, search: \
                     `community_search {{ query: \"<service> <operation>\", intent: \"{}\" }}`",
                    r.intent
                ));
            } else if r.intent.contains("security") {
                output.push_str(&format!(
                    "\n  → Before security-sensitive decisions, check: \
                     `community_search {{ query: \"<topic>\", intent: \"{}\" }}`",
                    r.intent
                ));
            }
        }

        output.push_str(
            "\n\nAlways attribute community sources in your output: \
             `[community: <resource-name>/<doc-id>]`\n",
        );
    }

    output
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;

    fn make_registry(dir: &Path, toml: &str) {
        let ta = dir.join(".ta");
        std::fs::create_dir_all(&ta).unwrap();
        let mut f = std::fs::File::create(ta.join("community-resources.toml")).unwrap();
        f.write_all(toml.as_bytes()).unwrap();
    }

    #[test]
    fn registry_loads_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        make_registry(
            dir.path(),
            r#"
[[resources]]
name = "api-docs"
intent = "api-integration"
description = "Curated API docs"
source = "github:andrewyng/context-hub"
access = "read-write"
auto_query = true
"#,
        );
        let reg = Registry::load(dir.path()).unwrap();
        assert_eq!(reg.resources.len(), 1);
        assert_eq!(reg.resources[0].name, "api-docs");
        assert!(reg.resources[0].auto_query);
    }

    #[test]
    fn registry_empty_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::load(dir.path()).unwrap();
        assert!(reg.resources.is_empty());
    }

    #[test]
    fn community_context_section_empty_without_auto_query() {
        let dir = tempfile::tempdir().unwrap();
        make_registry(
            dir.path(),
            r#"
[[resources]]
name = "docs"
intent = "api-integration"
description = "d"
source = "local:.ta/x/"
auto_query = false
"#,
        );
        let section = build_community_context_section(dir.path());
        assert!(section.is_empty());
    }

    #[test]
    fn community_context_section_includes_auto_query_resources() {
        let dir = tempfile::tempdir().unwrap();
        make_registry(
            dir.path(),
            r#"
[[resources]]
name = "api-docs"
intent = "api-integration"
description = "Curated API documentation"
source = "github:andrewyng/context-hub"
access = "read-only"
auto_query = true

[[resources]]
name = "security-threats"
intent = "security-intelligence"
description = "Known threats and CVEs"
source = "local:.ta/threats/"
access = "read-only"
auto_query = true
"#,
        );
        let section = build_community_context_section(dir.path());
        assert!(
            section.contains("api-docs"),
            "compact note should mention api-docs"
        );
        assert!(
            section.contains("security-threats"),
            "compact note should mention security-threats"
        );
        assert!(
            section.contains("community_search"),
            "compact note should mention community_search tool"
        );
        assert!(
            section.contains("Community Knowledge"),
            "should have community knowledge header"
        );
    }

    #[test]
    fn test_community_section_compact_under_200_tokens() {
        // 5 resources with auto_query=true and pre_inject=false → compact note under 200 tokens
        let dir = tempfile::tempdir().unwrap();
        let mut toml = String::new();
        for i in 1..=5 {
            toml.push_str(&format!(
                r#"
[[resources]]
name = "resource-{i}"
intent = "api-integration"
description = "Resource {i} description"
source = "local:.ta/r{i}/"
access = "read-only"
auto_query = true
pre_inject = false
"#,
                i = i
            ));
        }
        make_registry(dir.path(), &toml);
        let section = build_community_context_section(dir.path());
        assert!(!section.is_empty(), "should produce a compact note");
        // Rough token count: ~4 chars per token.
        let token_estimate = section.len() / 4;
        assert!(
            token_estimate < 200,
            "compact note should be under 200 tokens (estimated {token_estimate}), got: {section}"
        );
    }

    #[test]
    fn test_pre_inject_true_includes_guidance() {
        // resource with pre_inject = true still gets full guidance block
        let dir = tempfile::tempdir().unwrap();
        make_registry(
            dir.path(),
            r#"
[[resources]]
name = "api-docs"
intent = "api-integration"
description = "Curated API documentation"
source = "github:andrewyng/context-hub"
access = "read-only"
auto_query = true
pre_inject = true
"#,
        );
        let section = build_community_context_section(dir.path());
        assert!(section.contains("api-docs"), "should include resource name");
        assert!(
            section.contains("community_search"),
            "should include guidance with community_search"
        );
        assert!(
            section.contains("pre-loaded"),
            "should include pre-loaded header"
        );
    }

    #[test]
    fn test_auto_query_no_longer_injects_bulk() {
        // auto_query = true, pre_inject = false → compact note only, no full guidance block
        let dir = tempfile::tempdir().unwrap();
        make_registry(
            dir.path(),
            r#"
[[resources]]
name = "security-threats"
intent = "security-intelligence"
description = "Known threats and CVEs"
source = "local:.ta/threats/"
access = "read-only"
auto_query = true
pre_inject = false
"#,
        );
        let section = build_community_context_section(dir.path());
        assert!(!section.is_empty(), "compact note should be present");
        assert!(
            !section.contains("pre-loaded"),
            "should not include full guidance block"
        );
        assert!(
            !section.contains("Known threats and CVEs"),
            "should not inject description in compact mode"
        );
        // Should contain compact note with resource name
        assert!(
            section.contains("security-threats"),
            "compact note should list resource name"
        );
    }

    #[test]
    fn community_context_section_excludes_disabled() {
        let dir = tempfile::tempdir().unwrap();
        make_registry(
            dir.path(),
            r#"
[[resources]]
name = "off"
intent = "x"
description = "d"
source = "local:.ta/x/"
access = "disabled"
auto_query = true
"#,
        );
        let section = build_community_context_section(dir.path());
        assert!(section.is_empty());
    }

    #[test]
    fn sync_local_indexes_markdown_files() {
        let dir = tempfile::tempdir().unwrap();
        let local_dir = dir.path().join(".ta").join("knowledge");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::write(local_dir.join("guide.md"), "# Guide\n\nContent.").unwrap();

        let resource = Resource {
            name: "local-kb".into(),
            intent: "project-knowledge".into(),
            description: "d".into(),
            source: "local:.ta/knowledge/".into(),
            content_path: "".into(),
            access: Access::ReadWrite,
            auto_query: true,
            pre_inject: false,
            languages: vec![],
            update_frequency: "on-demand".into(),
        };

        let count = sync_resource(dir.path(), &resource).unwrap();
        assert_eq!(count, 1);

        // Verify cached file exists.
        let cached = dir
            .path()
            .join(".ta")
            .join("community-cache")
            .join("local-kb")
            .join("guide.md");
        assert!(cached.exists());
    }

    #[test]
    fn search_finds_keyword_in_cache() {
        let dir = tempfile::tempdir().unwrap();
        let local_dir = dir.path().join(".ta").join("docs");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::write(
            local_dir.join("stripe.md"),
            "# Stripe\n\nUse PaymentIntents to charge cards.",
        )
        .unwrap();

        let resource = Resource {
            name: "api-docs".into(),
            intent: "api-integration".into(),
            description: "d".into(),
            source: "local:.ta/docs/".into(),
            content_path: "".into(),
            access: Access::ReadOnly,
            auto_query: false,
            pre_inject: false,
            languages: vec![],
            update_frequency: "on-demand".into(),
        };
        sync_resource(dir.path(), &resource).unwrap();

        let cache_root = dir.path().join(".ta").join("community-cache");
        let words = vec!["payment".to_string()];
        let mut results = Vec::new();
        scan_for_matches(
            &cache_root.join("api-docs"),
            &cache_root.join("api-docs"),
            "api-docs",
            &words,
            &mut results,
        );
        assert!(!results.is_empty());
        assert!(results[0].1.contains("stripe"));
    }
}
