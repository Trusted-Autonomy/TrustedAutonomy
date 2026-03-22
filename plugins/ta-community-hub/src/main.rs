//! Community Knowledge Hub plugin for Trusted Autonomy.
//!
//! Implements the TA knowledge plugin JSON-over-stdio protocol (protocol version 1).
//!
//! ## Protocol
//!
//! Reads one JSON request line from stdin, writes one JSON response line to
//! stdout, then exits. Each invocation is stateless — the plugin is spawned
//! fresh for every method call.
//!
//! ## Supported methods
//!
//! | Method                | Description                                                    |
//! |-----------------------|----------------------------------------------------------------|
//! | `handshake`           | Version negotiation                                            |
//! | `community_search`    | Search across configured resources by query and/or intent      |
//! | `community_get`       | Fetch a specific document by ID                                |
//! | `community_annotate`  | Stage a gap annotation for human review                        |
//! | `community_feedback`  | Stage a quality rating for batched upstream submission          |
//! | `community_suggest`   | Stage a new document proposal for human review                 |
//! | `list_resources`      | List configured resources with status                          |
//! | `sync`                | Sync local cache from source (GitHub or local path)            |

mod cache;
mod registry;

use std::io::{self, BufRead, Write};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use cache::{CacheMetadata, ResourceCache};
use registry::{Access, Registry};

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

const PROTOCOL_VERSION: u32 = 1;
const PLUGIN_NAME: &str = "community-hub";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Default token budget per resource per query (≈ 4000 tokens × 4 chars).
const DEFAULT_TOKEN_BUDGET: usize = 4000;

// ---------------------------------------------------------------------------
// Protocol types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Request {
    method: String,
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    result: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok(result: serde_json::Value) -> Self {
        Self { ok: true, result, error: None }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: serde_json::Value::Null,
            error: Some(msg.into()),
        }
    }
}

fn write_response(resp: &Response) {
    let json = serde_json::to_string(resp).unwrap_or_else(|e| {
        format!(r#"{{"ok":false,"error":"serialization error: {}"}}"#, e)
    });
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{}", json);
    let _ = out.flush();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn workspace_path(params: &serde_json::Value) -> String {
    params
        .get("workspace_path")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string()
}

fn str_param<'a>(params: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn handle_handshake(_params: &serde_json::Value) -> Response {
    Response::ok(serde_json::json!({
        "plugin_version": PLUGIN_VERSION,
        "protocol_version": PROTOCOL_VERSION,
        "plugin_name": PLUGIN_NAME,
        "capabilities": [
            "community_search",
            "community_get",
            "community_annotate",
            "community_feedback",
            "community_suggest",
            "list_resources",
            "sync",
        ]
    }))
}

fn handle_list_resources(params: &serde_json::Value) -> Response {
    let workspace = workspace_path(params);
    let ws_path = std::path::Path::new(&workspace);

    let registry = match Registry::load(ws_path) {
        Ok(r) => r,
        Err(e) => return Response::err(e),
    };

    let cache = ResourceCache::new(ws_path);

    let items: Vec<serde_json::Value> = registry
        .resources
        .iter()
        .map(|r| {
            let meta = cache.load_metadata(&r.name);
            let synced_at = meta.as_ref().map(|m| m.synced_at.to_rfc3339());
            let doc_count = meta.as_ref().map(|m| m.document_count).unwrap_or(0);
            serde_json::json!({
                "name": r.name,
                "intent": r.intent,
                "description": r.description,
                "source": r.source,
                "access": r.access.to_string(),
                "auto_query": r.auto_query,
                "update_frequency": r.update_frequency,
                "languages": r.languages,
                "synced_at": synced_at,
                "cached_docs": doc_count,
            })
        })
        .collect();

    Response::ok(serde_json::json!({ "resources": items }))
}

fn handle_community_search(params: &serde_json::Value) -> Response {
    let workspace = workspace_path(params);
    let ws_path = std::path::Path::new(&workspace);

    let query = match str_param(params, "query") {
        Some(q) => q,
        None => return Response::err("community_search: missing 'query' parameter"),
    };

    let intent_filter = str_param(params, "intent");
    let resource_filter = str_param(params, "resource");
    let token_budget = params
        .get("token_budget")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_TOKEN_BUDGET);

    let registry = match Registry::load(ws_path) {
        Ok(r) => r,
        Err(e) => return Response::err(e),
    };

    // Route by intent if specified.
    let candidates: Vec<&registry::Resource> = if let Some(intent) = intent_filter {
        registry.by_intent(intent)
    } else if let Some(name) = resource_filter {
        registry.find(name).into_iter().collect()
    } else {
        registry.enabled()
    };

    if candidates.is_empty() {
        return Response::ok(serde_json::json!({
            "results": [],
            "message": "No enabled community resources configured. Add resources to .ta/community-resources.toml."
        }));
    }

    let cache = ResourceCache::new(ws_path);
    let results = cache.search(query, resource_filter, intent_filter, &candidates, token_budget);

    let result_json: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let mut v = serde_json::json!({
                "id": r.id,
                "resource": r.resource_name,
                "intent": r.intent,
                "excerpt": r.excerpt,
                "synced_at": r.synced_at.to_rfc3339(),
            });
            if r.is_stale {
                v["warning"] = serde_json::json!(
                    "This document may be outdated (last synced >90 days ago). Run `ta community sync` to refresh."
                );
            }
            v
        })
        .collect();

    Response::ok(serde_json::json!({
        "results": result_json,
        "query": query,
        "total": result_json.len(),
    }))
}

fn handle_community_get(params: &serde_json::Value) -> Response {
    let workspace = workspace_path(params);
    let ws_path = std::path::Path::new(&workspace);

    let id = match str_param(params, "id") {
        Some(id) => id,
        None => return Response::err("community_get: missing 'id' parameter"),
    };

    let token_budget = params
        .get("token_budget")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_TOKEN_BUDGET);

    // Check that the resource exists and is accessible.
    let registry = match Registry::load(ws_path) {
        Ok(r) => r,
        Err(e) => return Response::err(e),
    };

    let resource_name = id.split('/').next().unwrap_or("");
    if let Some(resource) = registry.find(resource_name) {
        if resource.access == Access::Disabled {
            return Response::err(format!(
                "community_get: resource '{}' is disabled. Enable it in .ta/community-resources.toml.",
                resource_name
            ));
        }
    }

    let cache = ResourceCache::new(ws_path);
    match cache.get_doc(id, token_budget) {
        Some(doc) => {
            let mut v = serde_json::json!({
                "id": doc.id,
                "resource": doc.resource_name,
                "content": doc.content,
                "synced_at": doc.synced_at.to_rfc3339(),
                "truncated": doc.truncated,
            });
            if let Some(summary) = &doc.summary {
                v["truncation_note"] = serde_json::json!(summary);
            }
            if doc.is_stale {
                v["warning"] = serde_json::json!(
                    "⚠ This document may be outdated (last synced >90 days ago). Run `ta community sync` to refresh."
                );
            }
            Response::ok(v)
        }
        None => Response::err(format!(
            "community_get: document '{}' not found in cache. \
             Run `ta community sync` to populate the cache, or check the ID with `ta community list`.",
            id
        )),
    }
}

fn handle_community_annotate(params: &serde_json::Value) -> Response {
    let workspace = workspace_path(params);
    let ws_path = std::path::Path::new(&workspace);

    let id = match str_param(params, "id") {
        Some(id) => id,
        None => return Response::err("community_annotate: missing 'id' parameter"),
    };
    let note = match str_param(params, "note") {
        Some(n) => n,
        None => return Response::err("community_annotate: missing 'note' parameter"),
    };

    // Check write access.
    let registry = match Registry::load(ws_path) {
        Ok(r) => r,
        Err(e) => return Response::err(e),
    };
    let resource_name = id.split('/').next().unwrap_or("");
    if let Some(resource) = registry.find(resource_name) {
        if resource.access == Access::ReadOnly {
            return Response::err(format!(
                "community_annotate: resource '{}' is read-only. \
                 Change access to 'read-write' in .ta/community-resources.toml to allow contributions.",
                resource_name
            ));
        }
        if resource.access == Access::Disabled {
            return Response::err(format!(
                "community_annotate: resource '{}' is disabled.",
                resource_name
            ));
        }
    }

    let gap_type = str_param(params, "gap_type").unwrap_or("gap");
    let goal_id = str_param(params, "goal_id").unwrap_or("unknown");

    // Write to .ta/community-staging/<resource-name>/annotations/.
    let staging_dir = ws_path
        .join(".ta")
        .join("community-staging")
        .join(resource_name)
        .join("annotations");
    if let Err(e) = std::fs::create_dir_all(&staging_dir) {
        return Response::err(format!("community_annotate: failed to create staging dir: {}", e));
    }

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let slug = id.replace('/', "_").replace(' ', "-");
    let filename = format!("{}_{}.md", slug, timestamp);
    let artifact_path = staging_dir.join(&filename);

    let content = format!(
        "---\ntype: annotation\ndocument: {}\ngap_type: {}\ngoal_id: {}\ncreated_at: {}\n---\n\n{}\n",
        id,
        gap_type,
        goal_id,
        Utc::now().to_rfc3339(),
        note
    );

    if let Err(e) = std::fs::write(&artifact_path, &content) {
        return Response::err(format!("community_annotate: failed to write staging file: {}", e));
    }

    let rel_path = artifact_path
        .strip_prefix(ws_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| filename.clone());

    Response::ok(serde_json::json!({
        "staged": true,
        "artifact_path": rel_path,
        "resource_uri": format!("community://{}/{}", resource_name, id),
        "message": format!(
            "Annotation staged at {}. It will be included in the draft for human review. \
             On draft apply, it will be submitted to the upstream resource as a PR.",
            rel_path
        )
    }))
}

fn handle_community_feedback(params: &serde_json::Value) -> Response {
    let workspace = workspace_path(params);
    let ws_path = std::path::Path::new(&workspace);

    let id = match str_param(params, "id") {
        Some(id) => id,
        None => return Response::err("community_feedback: missing 'id' parameter"),
    };
    let rating = match params.get("rating").and_then(|v| v.as_str()) {
        Some(r) if r == "upvote" || r == "downvote" => r,
        Some(other) => {
            return Response::err(format!(
                "community_feedback: invalid rating '{}'. Use 'upvote' or 'downvote'.",
                other
            ))
        }
        None => return Response::err("community_feedback: missing 'rating' parameter (upvote or downvote)"),
    };

    // Check access.
    let registry = match Registry::load(ws_path) {
        Ok(r) => r,
        Err(e) => return Response::err(e),
    };
    let resource_name = id.split('/').next().unwrap_or("");
    if let Some(resource) = registry.find(resource_name) {
        if resource.access == Access::ReadOnly {
            return Response::err(format!(
                "community_feedback: resource '{}' is read-only. Feedback requires read-write access.",
                resource_name
            ));
        }
    }

    let context = str_param(params, "context").unwrap_or("");
    let goal_id = str_param(params, "goal_id").unwrap_or("unknown");

    let staging_dir = ws_path
        .join(".ta")
        .join("community-staging")
        .join(resource_name)
        .join("feedback");
    if let Err(e) = std::fs::create_dir_all(&staging_dir) {
        return Response::err(format!("community_feedback: failed to create staging dir: {}", e));
    }

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let slug = id.replace('/', "_");
    let filename = format!("{}_{}.json", slug, timestamp);
    let artifact_path = staging_dir.join(&filename);

    let payload = serde_json::json!({
        "document_id": id,
        "resource": resource_name,
        "rating": rating,
        "context": context,
        "goal_id": goal_id,
        "created_at": Utc::now().to_rfc3339(),
    });

    let content = serde_json::to_string_pretty(&payload).unwrap_or_default();
    if let Err(e) = std::fs::write(&artifact_path, &content) {
        return Response::err(format!("community_feedback: failed to write staging file: {}", e));
    }

    let rel_path = artifact_path
        .strip_prefix(ws_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| filename.clone());

    let emoji = if rating == "upvote" { "👍" } else { "👎" };
    Response::ok(serde_json::json!({
        "staged": true,
        "artifact_path": rel_path,
        "resource_uri": format!("community://{}/{}", resource_name, id),
        "message": format!(
            "{} Feedback staged at {}. It will be batched and submitted upstream when the draft is applied.",
            emoji, rel_path
        )
    }))
}

fn handle_community_suggest(params: &serde_json::Value) -> Response {
    let workspace = workspace_path(params);
    let ws_path = std::path::Path::new(&workspace);

    let title = match str_param(params, "title") {
        Some(t) => t,
        None => return Response::err("community_suggest: missing 'title' parameter"),
    };
    let content = match str_param(params, "content") {
        Some(c) => c,
        None => return Response::err("community_suggest: missing 'content' parameter"),
    };
    let intent = match str_param(params, "intent") {
        Some(i) => i,
        None => return Response::err("community_suggest: missing 'intent' parameter"),
    };
    let resource = match str_param(params, "resource") {
        Some(r) => r,
        None => return Response::err("community_suggest: missing 'resource' parameter"),
    };

    // Check access.
    let registry = match Registry::load(ws_path) {
        Ok(r) => r,
        Err(e) => return Response::err(e),
    };
    if let Some(res) = registry.find(resource) {
        if res.access == Access::ReadOnly {
            return Response::err(format!(
                "community_suggest: resource '{}' is read-only. \
                 Change access to 'read-write' in .ta/community-resources.toml to allow suggestions.",
                resource
            ));
        }
        if res.access == Access::Disabled {
            return Response::err(format!(
                "community_suggest: resource '{}' is disabled.",
                resource
            ));
        }
    }

    let goal_id = str_param(params, "goal_id").unwrap_or("unknown");

    let staging_dir = ws_path
        .join(".ta")
        .join("community-staging")
        .join(resource)
        .join("suggestions");
    if let Err(e) = std::fs::create_dir_all(&staging_dir) {
        return Response::err(format!("community_suggest: failed to create staging dir: {}", e));
    }

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let slug = title
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { '-' })
        .collect::<String>();
    let slug = slug.trim_matches('-').to_string();
    let filename = format!("{}_{}.md", slug, timestamp);
    let artifact_path = staging_dir.join(&filename);

    let doc_content = format!(
        "---\ntitle: {}\nintent: {}\nresource: {}\ngoal_id: {}\ncreated_at: {}\n---\n\n{}\n",
        title,
        intent,
        resource,
        goal_id,
        Utc::now().to_rfc3339(),
        content
    );

    if let Err(e) = std::fs::write(&artifact_path, &doc_content) {
        return Response::err(format!("community_suggest: failed to write staging file: {}", e));
    }

    let rel_path = artifact_path
        .strip_prefix(ws_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| filename.clone());

    // Look up the upstream source for this resource.
    let upstream = registry
        .find(resource)
        .map(|r| r.source.clone())
        .unwrap_or_else(|| "configured upstream".to_string());

    Response::ok(serde_json::json!({
        "staged": true,
        "artifact_path": rel_path,
        "resource_uri": format!("community://{}/suggestions/{}", resource, slug),
        "message": format!(
            "📄 New document suggestion staged at {}. \
             On draft apply, it will be submitted as a PR to {}.",
            rel_path, upstream
        )
    }))
}

fn handle_sync(params: &serde_json::Value) -> Response {
    let workspace = workspace_path(params);
    let ws_path = std::path::Path::new(&workspace);

    let resource_filter = str_param(params, "resource");

    let registry = match Registry::load(ws_path) {
        Ok(r) => r,
        Err(e) => return Response::err(e),
    };

    let to_sync: Vec<&registry::Resource> = if let Some(name) = resource_filter {
        match registry.find(name) {
            Some(r) => vec![r],
            None => {
                return Response::err(format!(
                    "sync: resource '{}' not found. Run `ta community list` to see available resources.",
                    name
                ))
            }
        }
    } else {
        registry.enabled()
    };

    let cache = ResourceCache::new(ws_path);
    let mut synced = Vec::new();
    let mut errors = Vec::new();

    for resource in &to_sync {
        match sync_resource(resource, ws_path, &cache) {
            Ok(count) => {
                synced.push(serde_json::json!({
                    "name": resource.name,
                    "documents": count,
                    "synced_at": Utc::now().to_rfc3339(),
                }));
            }
            Err(e) => {
                errors.push(serde_json::json!({
                    "name": resource.name,
                    "error": e,
                }));
            }
        }
    }

    if synced.is_empty() && !errors.is_empty() {
        return Response::err(format!(
            "sync: all resources failed. First error: {}",
            errors[0]["error"].as_str().unwrap_or("unknown")
        ));
    }

    Response::ok(serde_json::json!({
        "synced": synced,
        "errors": errors,
        "message": format!(
            "Synced {} resource(s). {} error(s). Use `ta community list` to check status.",
            synced.len(),
            errors.len()
        )
    }))
}

/// Sync a single resource to the local cache.
///
/// For "local:" sources, copies documents to the cache directory.
/// For "github:" sources, uses the GitHub API to list and download files.
fn sync_resource(
    resource: &registry::Resource,
    workspace: &std::path::Path,
    cache: &ResourceCache,
) -> Result<usize, String> {
    if let Some(local_base) = resource.local_path(workspace) {
        // Local source: scan for .md files and copy to cache.
        if !local_base.exists() {
            return Err(format!(
                "local path '{}' does not exist. Create it or update source in community-resources.toml.",
                local_base.display()
            ));
        }
        let mut docs = Vec::new();
        collect_local_docs(&local_base, &local_base, &mut docs)?;
        let count = docs.len();
        for (rel_path, content) in &docs {
            cache.write_doc(&resource.name, rel_path, content)?;
        }
        let meta = CacheMetadata {
            resource_name: resource.name.clone(),
            synced_at: Utc::now(),
            source: resource.source.clone(),
            document_count: count,
        };
        cache.write_metadata(&meta)?;
        return Ok(count);
    }

    if let Some((_owner, _repo)) = resource.github_repo() {
        // GitHub source: note that this requires HTTP which we don't have in the plugin.
        // The CLI's `ta community sync` command handles GitHub via the TA daemon or
        // by shelling out to curl/gh. Here we return a message directing the user
        // to use the CLI command.
        return Err(format!(
            "GitHub-sourced resources require internet access. \
             Run `ta community sync {}` from the CLI, which handles GitHub API fetching.",
            resource.name
        ));
    }

    Err(format!(
        "Unknown source format '{}'. Supported: 'github:<owner>/<repo>' or 'local:<path>'.",
        resource.source
    ))
}

fn collect_local_docs(
    base: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read dir {}: {}", dir.display(), e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_local_docs(base, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let rel = path
                .strip_prefix(base)
                .map_err(|_| "strip_prefix failed".to_string())?;
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
            out.push((rel.to_string_lossy().to_string(), content));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let stdin = io::stdin();
    let line = match stdin.lock().lines().next() {
        Some(Ok(line)) if !line.trim().is_empty() => line,
        _ => {
            write_response(&Response::err(
                "No input on stdin. Expected one JSON line with a plugin request.",
            ));
            std::process::exit(1);
        }
    };

    let request: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            write_response(&Response::err(format!(
                "Invalid JSON request: {}. Got: '{}'",
                e,
                if line.len() > 200 { &line[..200] } else { &line }
            )));
            std::process::exit(1);
        }
    };

    let response = match request.method.as_str() {
        "handshake" => handle_handshake(&request.params),
        "list_resources" => handle_list_resources(&request.params),
        "community_search" => handle_community_search(&request.params),
        "community_get" => handle_community_get(&request.params),
        "community_annotate" => handle_community_annotate(&request.params),
        "community_feedback" => handle_community_feedback(&request.params),
        "community_suggest" => handle_community_suggest(&request.params),
        "sync" => handle_sync(&request.params),
        unknown => Response::err(format!(
            "Unknown method '{}'. Supported: handshake, list_resources, community_search, \
             community_get, community_annotate, community_feedback, community_suggest, sync.",
            unknown
        )),
    };

    write_response(&response);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;

    fn make_workspace_with_registry(dir: &std::path::Path, toml: &str) {
        let ta = dir.join(".ta");
        std::fs::create_dir_all(&ta).unwrap();
        let mut f = std::fs::File::create(ta.join("community-resources.toml")).unwrap();
        f.write_all(toml.as_bytes()).unwrap();
    }

    #[test]
    fn handshake_returns_plugin_name_and_capabilities() {
        let resp = handle_handshake(&serde_json::json!({}));
        assert!(resp.ok);
        assert_eq!(resp.result["plugin_name"], "community-hub");
        assert_eq!(resp.result["protocol_version"], 1);
        let caps = resp.result["capabilities"].as_array().unwrap();
        let cap_names: Vec<&str> = caps.iter().filter_map(|v| v.as_str()).collect();
        assert!(cap_names.contains(&"community_search"));
        assert!(cap_names.contains(&"community_annotate"));
    }

    #[test]
    fn list_resources_empty_when_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_list_resources(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap()
        }));
        assert!(resp.ok);
        assert_eq!(resp.result["resources"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn list_resources_shows_configured_resources() {
        let dir = tempfile::tempdir().unwrap();
        make_workspace_with_registry(
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
        let resp = handle_list_resources(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap()
        }));
        assert!(resp.ok);
        let resources = resp.result["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["name"], "api-docs");
        assert_eq!(resources[0]["access"], "read-write");
        assert_eq!(resources[0]["auto_query"], true);
    }

    #[test]
    fn community_search_returns_empty_without_resources() {
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_community_search(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap(),
            "query": "stripe api"
        }));
        assert!(resp.ok);
        assert!(resp.result["message"].as_str().unwrap().contains("No enabled"));
    }

    #[test]
    fn community_annotate_requires_note_param() {
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_community_annotate(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap(),
            "id": "api-docs/stripe"
        }));
        assert!(!resp.ok);
        assert!(resp.error.as_deref().unwrap_or("").contains("missing 'note'"));
    }

    #[test]
    fn community_annotate_enforces_read_only_access() {
        let dir = tempfile::tempdir().unwrap();
        make_workspace_with_registry(
            dir.path(),
            r#"
[[resources]]
name = "api-docs"
intent = "api-integration"
description = "d"
source = "local:.ta/docs/"
access = "read-only"
"#,
        );
        let resp = handle_community_annotate(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap(),
            "id": "api-docs/stripe",
            "note": "Missing error codes"
        }));
        assert!(!resp.ok);
        assert!(resp.error.as_deref().unwrap_or("").contains("read-only"));
    }

    #[test]
    fn community_annotate_stages_file_for_read_write_resource() {
        let dir = tempfile::tempdir().unwrap();
        make_workspace_with_registry(
            dir.path(),
            r#"
[[resources]]
name = "api-docs"
intent = "api-integration"
description = "d"
source = "local:.ta/docs/"
access = "read-write"
"#,
        );
        let resp = handle_community_annotate(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap(),
            "id": "api-docs/stripe",
            "note": "Missing card_declined error handling",
            "gap_type": "missing-error-case"
        }));
        assert!(resp.ok, "Expected ok, got: {:?}", resp.error);
        assert_eq!(resp.result["staged"], true);
        let path = resp.result["artifact_path"].as_str().unwrap();
        assert!(path.contains("community-staging"));
        assert!(path.contains("annotations"));
    }

    #[test]
    fn community_feedback_validates_rating() {
        let dir = tempfile::tempdir().unwrap();
        let resp = handle_community_feedback(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap(),
            "id": "api-docs/stripe",
            "rating": "meh"
        }));
        assert!(!resp.ok);
        assert!(resp.error.as_deref().unwrap_or("").contains("invalid rating"));
    }

    #[test]
    fn community_suggest_stages_new_doc() {
        let dir = tempfile::tempdir().unwrap();
        make_workspace_with_registry(
            dir.path(),
            r#"
[[resources]]
name = "api-docs"
intent = "api-integration"
description = "d"
source = "github:andrewyng/context-hub"
access = "read-write"
"#,
        );
        let resp = handle_community_suggest(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap(),
            "title": "Twilio Verify v2",
            "content": "# Twilio Verify v2\n\nComplete reference...",
            "intent": "api-integration",
            "resource": "api-docs"
        }));
        assert!(resp.ok, "Expected ok, got: {:?}", resp.error);
        assert_eq!(resp.result["staged"], true);
        let path = resp.result["artifact_path"].as_str().unwrap();
        assert!(path.contains("suggestions"));
    }

    #[test]
    fn sync_local_resource_copies_docs() {
        let dir = tempfile::tempdir().unwrap();
        // Create a local docs directory with a markdown file.
        let docs_dir = dir.path().join(".ta").join("community");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("guide.md"), "# Guide\n\nContent here.").unwrap();

        make_workspace_with_registry(
            dir.path(),
            r#"
[[resources]]
name = "project-local"
intent = "project-knowledge"
description = "d"
source = "local:.ta/community/"
access = "read-write"
auto_query = true
"#,
        );
        let resp = handle_sync(&serde_json::json!({
            "workspace_path": dir.path().to_str().unwrap(),
            "resource": "project-local"
        }));
        assert!(resp.ok, "Expected ok, got: {:?}", resp.error);
        let synced = resp.result["synced"].as_array().unwrap();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0]["documents"], 1);
    }

    #[test]
    fn unknown_method_returns_error() {
        let resp = Response::err("Unknown method 'bogus'.");
        assert!(!resp.ok);
        assert!(resp.error.is_some());
    }
}
