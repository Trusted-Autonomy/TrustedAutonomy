//! ta-memory-supermemory — Supermemory cloud backend plugin for Trusted Autonomy.
//!
//! Implements the TA memory plugin JSON-over-stdio protocol v1, delegating all
//! operations to the Supermemory REST API.
//!
//! ## Setup
//!
//! 1. Install this binary: `cargo install ta-memory-supermemory` (or build from source).
//! 2. Set `SUPERMEMORY_API_KEY` in your environment.
//! 3. Configure TA to use it:
//!
//! ```toml
//! # .ta/memory.toml
//! backend = "plugin"
//! plugin  = "supermemory"
//! ```
//!
//! ## Protocol
//!
//! TA → plugin: one JSON line on stdin
//! plugin → TA: one JSON line on stdout
//!
//! Operations: `handshake`, `store`, `recall`, `lookup`, `forget`,
//!             `semantic_search`, `stats`
//!
//! ## Supermemory API
//!
//! - POST   /v1/memories            → store
//! - GET    /v1/search?q=<query>    → semantic_search / lookup
//! - DELETE /v1/memories/{id}       → forget
//!
//! Full API docs: https://docs.supermemory.ai

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

const PLUGIN_NAME: &str = "supermemory";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Protocol envelope types (mirrors ta-memory/src/plugin_protocol.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Request {
    op: String,
    #[serde(flatten)]
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entries: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok_entry(entry: serde_json::Value) -> Self {
        Self {
            ok: true,
            entry: Some(entry),
            entries: None,
            deleted: None,
            stats: None,
            plugin_name: None,
            plugin_version: None,
            protocol_version: None,
            capabilities: vec![],
            error: None,
        }
    }

    fn ok_entries(entries: Vec<serde_json::Value>) -> Self {
        Self {
            ok: true,
            entry: None,
            entries: Some(entries),
            deleted: None,
            stats: None,
            plugin_name: None,
            plugin_version: None,
            protocol_version: None,
            capabilities: vec![],
            error: None,
        }
    }

    fn ok_deleted(deleted: bool) -> Self {
        Self {
            ok: true,
            entry: None,
            entries: None,
            deleted: Some(deleted),
            stats: None,
            plugin_name: None,
            plugin_version: None,
            protocol_version: None,
            capabilities: vec![],
            error: None,
        }
    }

    fn ok_handshake() -> Self {
        Self {
            ok: true,
            entry: None,
            entries: None,
            deleted: None,
            stats: None,
            plugin_name: Some(PLUGIN_NAME.to_string()),
            plugin_version: Some(PLUGIN_VERSION.to_string()),
            protocol_version: Some(PROTOCOL_VERSION),
            capabilities: vec!["semantic_search".to_string()],
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            entry: None,
            entries: None,
            deleted: None,
            stats: None,
            plugin_name: None,
            plugin_version: None,
            protocol_version: None,
            capabilities: vec![],
            error: Some(msg.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Supermemory API client
// ---------------------------------------------------------------------------

/// Minimal blocking HTTP client using the system's curl binary.
///
/// This avoids adding a full HTTP client dependency to the plugin binary.
/// Production users can replace this with any HTTP library.
fn api_base() -> String {
    std::env::var("SUPERMEMORY_API_URL")
        .unwrap_or_else(|_| "https://api.supermemory.ai".to_string())
}

fn api_key() -> Result<String, String> {
    std::env::var("SUPERMEMORY_API_KEY").map_err(|_| {
        "SUPERMEMORY_API_KEY environment variable is not set. \
         Get your API key at https://supermemory.ai and run: \
         export SUPERMEMORY_API_KEY=<your-key>"
            .to_string()
    })
}

/// POST /v1/memories — store a memory entry.
fn api_store(key: &str, value: &serde_json::Value, tags: &[String], source: &str) -> Result<serde_json::Value, String> {
    let api_key = api_key()?;
    let body = serde_json::json!({
        "content": serde_json::to_string(value).unwrap_or_default(),
        "metadata": {
            "ta_key": key,
            "ta_source": source,
            "ta_tags": tags,
        }
    });
    let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;

    let output = std::process::Command::new("curl")
        .args([
            "-s", "-X", "POST",
            &format!("{}/v1/memories", api_base()),
            "-H", "Content-Type: application/json",
            "-H", &format!("Authorization: Bearer {}", api_key),
            "-d", &body_str,
        ])
        .output()
        .map_err(|e| format!("curl spawn failed: {}. Install curl to use this plugin.", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Supermemory API POST /v1/memories failed: {}", stderr.trim()));
    }

    let resp: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid JSON from Supermemory API: {}", e))?;

    // Build a TA-compatible MemoryEntry JSON.
    let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or(key);
    Ok(build_entry_json(id, key, value, tags, source))
}

/// GET /v1/search?q=<query> — semantic search.
fn api_search(query: &str, limit: usize) -> Result<Vec<serde_json::Value>, String> {
    let api_key = api_key()?;
    let url = format!(
        "{}/v1/search?q={}&limit={}",
        api_base(),
        urlencoded(query),
        limit
    );

    let output = std::process::Command::new("curl")
        .args([
            "-s",
            &url,
            "-H", &format!("Authorization: Bearer {}", api_key),
        ])
        .output()
        .map_err(|e| format!("curl spawn failed: {}. Install curl to use this plugin.", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Supermemory API GET /v1/search failed: {}", stderr.trim()));
    }

    let resp: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid JSON from Supermemory API: {}", e))?;

    let results = resp
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(results
        .iter()
        .map(|r| {
            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let key = r
                .get("metadata")
                .and_then(|m| m.get("ta_key"))
                .and_then(|v| v.as_str())
                .unwrap_or(id);
            let value: serde_json::Value = serde_json::from_str(content)
                .unwrap_or_else(|_| serde_json::Value::String(content.to_string()));
            build_entry_json(id, key, &value, &[], "supermemory")
        })
        .collect())
}

/// DELETE /v1/memories/{id} — delete by Supermemory ID.
///
/// Since TA uses its own key system, we first search for the entry by key
/// to get the Supermemory ID, then delete it.
fn api_forget(key: &str) -> Result<bool, String> {
    let results = api_search(key, 1)?;
    if results.is_empty() {
        return Ok(false);
    }

    let sm_id = results[0]
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if sm_id.is_empty() {
        return Ok(false);
    }

    let api_key = api_key()?;
    let output = std::process::Command::new("curl")
        .args([
            "-s", "-X", "DELETE",
            &format!("{}/v1/memories/{}", api_base(), sm_id),
            "-H", &format!("Authorization: Bearer {}", api_key),
        ])
        .output()
        .map_err(|e| format!("curl spawn failed: {}", e))?;

    Ok(output.status.success())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_entry_json(
    id: &str,
    key: &str,
    value: &serde_json::Value,
    tags: &[String],
    source: &str,
) -> serde_json::Value {
    let now = chrono::Utc::now_rfc3339_or_fallback();
    serde_json::json!({
        "entry_id": id,
        "key": key,
        "value": value,
        "tags": tags,
        "source": source,
        "goal_id": null,
        "confidence": 0.8,
        "created_at": now,
        "updated_at": now
    })
}

fn urlencoded(s: &str) -> String {
    // Minimal percent-encoding for query parameters.
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                vec![c]
            }
            ' ' => vec!['+'],
            c => format!("%{:02X}", c as u32).chars().collect::<Vec<_>>(),
        })
        .collect()
}

// Provide a fallback for chrono's Utc::now() → RFC3339 string.
trait UtcNowRfc3339 {
    fn now_rfc3339_or_fallback() -> String;
}

impl UtcNowRfc3339 for chrono::Utc {
    fn now_rfc3339_or_fallback() -> String {
        // Use std::time for minimal dependencies in the plugin.
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => format!("{}Z", d.as_secs()),
            Err(_) => "1970-01-01T00:00:00Z".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Main dispatch loop
// ---------------------------------------------------------------------------

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    let line = match stdin.lock().lines().next() {
        Some(Ok(l)) => l,
        Some(Err(e)) => {
            let resp = Response::err(format!("failed to read stdin: {}", e));
            writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).ok();
            return;
        }
        None => {
            let resp = Response::err("no input received on stdin");
            writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).ok();
            return;
        }
    };

    let req: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            let resp = Response::err(format!("invalid JSON request: {}", e));
            writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).ok();
            return;
        }
    };

    let resp = dispatch(&req);
    writeln!(out, "{}", serde_json::to_string(&resp).unwrap()).ok();
}

fn dispatch(req: &Request) -> Response {
    match req.op.as_str() {
        "handshake" => Response::ok_handshake(),

        "store" => {
            let key = req.params.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let value = req.params.get("value").cloned().unwrap_or(serde_json::Value::Null);
            let tags: Vec<String> = req.params
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let source = req.params.get("source").and_then(|v| v.as_str()).unwrap_or("ta");

            match api_store(key, &value, &tags, source) {
                Ok(entry) => Response::ok_entry(entry),
                Err(e) => Response::err(e),
            }
        }

        "recall" => {
            let key = req.params.get("key").and_then(|v| v.as_str()).unwrap_or("");
            match api_search(key, 1) {
                Ok(results) => {
                    let matched = results
                        .into_iter()
                        .find(|e| e.get("key").and_then(|k| k.as_str()) == Some(key));
                    Response {
                        ok: true,
                        entry: matched,
                        entries: None,
                        deleted: None,
                        stats: None,
                        plugin_name: None,
                        plugin_version: None,
                        protocol_version: None,
                        capabilities: vec![],
                        error: None,
                    }
                }
                Err(e) => Response::err(e),
            }
        }

        "lookup" => {
            let prefix = req.params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
            let limit = req.params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let query = if prefix.is_empty() { "*" } else { prefix };
            match api_search(query, limit) {
                Ok(results) => Response::ok_entries(results),
                Err(e) => Response::err(e),
            }
        }

        "forget" => {
            let key = req.params.get("key").and_then(|v| v.as_str()).unwrap_or("");
            match api_forget(key) {
                Ok(deleted) => Response::ok_deleted(deleted),
                Err(e) => Response::err(e),
            }
        }

        "semantic_search" => {
            let query = req.params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let k = req.params.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            match api_search(query, k) {
                Ok(results) => Response::ok_entries(results),
                Err(e) => Response::err(e),
            }
        }

        "stats" => {
            // Return minimal stats — entry count would require a full list call.
            // Return a placeholder; TA will fall back to aggregate from list.
            Response {
                ok: false,
                entry: None,
                entries: None,
                deleted: None,
                stats: None,
                plugin_name: None,
                plugin_version: None,
                protocol_version: None,
                capabilities: vec![],
                error: Some("stats not natively supported — TA will aggregate".to_string()),
            }
        }

        unknown => Response::err(format!(
            "unknown op '{}'. Supported ops: handshake, store, recall, lookup, \
             forget, semantic_search, stats",
            unknown
        )),
    }
}

// ---------------------------------------------------------------------------
// Module-level chrono shim (avoid full chrono dependency in plugin)
// ---------------------------------------------------------------------------

mod chrono {
    pub struct Utc;
}
