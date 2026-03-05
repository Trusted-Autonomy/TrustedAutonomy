// tools/context.rs — Persistent memory MCP tool handler.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;

use ta_memory::MemoryStore;

use crate::server::{ContextToolParams, GatewayState};

pub fn handle_context(
    state: &Arc<Mutex<GatewayState>>,
    params: ContextToolParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    match params.action.as_str() {
        "store" => handle_store(&mut state, &params),
        "recall" => handle_recall(&state, &params),
        "list" => handle_list(&state, &params),
        "forget" => handle_forget(&mut state, &params),
        "search" => handle_search(&state, &params),
        "stats" => handle_stats(&state),
        "similar" => handle_similar(&state, &params),
        _ => Err(McpError::invalid_params(
            format!(
                "unknown action '{}'. Expected: store, recall, list, forget, search, stats, similar",
                params.action
            ),
            None,
        )),
    }
}

fn handle_store(
    state: &mut GatewayState,
    params: &ContextToolParams,
) -> Result<CallToolResult, McpError> {
    let key = params
        .key
        .as_deref()
        .ok_or_else(|| McpError::invalid_params("key required for store", None))?;
    let value = params.value.clone().unwrap_or(serde_json::Value::Null);
    let tags = params.tags.clone().unwrap_or_default();
    let source = params.source.as_deref().unwrap_or("agent");

    let goal_id = params
        .goal_id
        .as_deref()
        .and_then(|s| s.parse::<uuid::Uuid>().ok());
    let category = params
        .category
        .as_deref()
        .map(ta_memory::MemoryCategory::from_str_lossy);

    let store_params = ta_memory::StoreParams {
        goal_id,
        category,
        ..Default::default()
    };
    let entry = state
        .memory_store
        .store_with_params(key, value, tags, source, store_params)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({
        "status": "stored",
        "entry_id": entry.entry_id.to_string(),
        "key": entry.key,
        "source": entry.source,
        "category": entry.category.as_ref().map(|c| c.to_string()),
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn handle_recall(
    state: &GatewayState,
    params: &ContextToolParams,
) -> Result<CallToolResult, McpError> {
    let key = params
        .key
        .as_deref()
        .ok_or_else(|| McpError::invalid_params("key required for recall", None))?;

    let entry = state
        .memory_store
        .recall(key)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    match entry {
        Some(e) => {
            let response = serde_json::json!({
                "key": e.key,
                "value": e.value,
                "tags": e.tags,
                "source": e.source,
                "category": e.category.as_ref().map(|c| c.to_string()),
                "goal_id": e.goal_id.map(|id| id.to_string()),
                "created_at": e.created_at.to_rfc3339(),
                "updated_at": e.updated_at.to_rfc3339(),
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        None => {
            let response = serde_json::json!({
                "status": "not_found",
                "key": key,
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
    }
}

fn handle_list(
    state: &GatewayState,
    params: &ContextToolParams,
) -> Result<CallToolResult, McpError> {
    let entries = state
        .memory_store
        .list(params.limit)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let items: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "key": e.key,
                "tags": e.tags,
                "source": e.source,
                "category": e.category.as_ref().map(|c| c.to_string()),
                "updated_at": e.updated_at.to_rfc3339(),
            })
        })
        .collect();

    let response = serde_json::json!({
        "count": items.len(),
        "entries": items,
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn handle_forget(
    state: &mut GatewayState,
    params: &ContextToolParams,
) -> Result<CallToolResult, McpError> {
    let key = params
        .key
        .as_deref()
        .ok_or_else(|| McpError::invalid_params("key required for forget", None))?;

    let existed = state
        .memory_store
        .forget(key)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({
        "status": if existed { "forgotten" } else { "not_found" },
        "key": key,
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn handle_search(
    state: &GatewayState,
    params: &ContextToolParams,
) -> Result<CallToolResult, McpError> {
    let query = params
        .query
        .as_deref()
        .or(params.key.as_deref())
        .ok_or_else(|| McpError::invalid_params("query or key required for search", None))?;
    let k = params.limit.unwrap_or(5);

    let entries = state
        .memory_store
        .semantic_search(query, k)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let items: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "key": e.key,
                "value": e.value,
                "tags": e.tags,
                "source": e.source,
                "category": e.category.as_ref().map(|c| c.to_string()),
            })
        })
        .collect();

    let response = serde_json::json!({
        "count": items.len(),
        "results": items,
        "backend": if items.is_empty() { "no results (semantic search requires ruvector backend)" } else { "ok" },
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn handle_stats(state: &GatewayState) -> Result<CallToolResult, McpError> {
    let stats = state
        .memory_store
        .stats()
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({
        "total_entries": stats.total_entries,
        "by_category": stats.by_category,
        "by_source": stats.by_source,
        "expired_count": stats.expired_count,
        "avg_confidence": stats.avg_confidence,
        "oldest_entry": stats.oldest_entry.map(|t| t.to_rfc3339()),
        "newest_entry": stats.newest_entry.map(|t| t.to_rfc3339()),
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn handle_similar(
    state: &GatewayState,
    params: &ContextToolParams,
) -> Result<CallToolResult, McpError> {
    let entry_id = params
        .key
        .as_deref()
        .ok_or_else(|| McpError::invalid_params("key (entry_id) required for similar", None))?;
    let uuid: uuid::Uuid = entry_id.parse().map_err(|_| {
        McpError::invalid_params(
            format!("invalid UUID '{}' for similar lookup", entry_id),
            None,
        )
    })?;
    let k = params.limit.unwrap_or(5);

    let entry = state
        .memory_store
        .find_by_id(uuid)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(format!("no entry found with ID '{}'", entry_id), None)
        })?;

    let query_text = match &entry.value {
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    };

    let results = state
        .memory_store
        .semantic_search(&query_text, k + 1)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let items: Vec<serde_json::Value> = results
        .iter()
        .filter(|e| e.entry_id != uuid)
        .take(k)
        .map(|e| {
            serde_json::json!({
                "key": e.key,
                "value": e.value,
                "tags": e.tags,
                "source": e.source,
                "category": e.category.as_ref().map(|c| c.to_string()),
            })
        })
        .collect();

    let response = serde_json::json!({
        "reference_key": entry.key,
        "count": items.len(),
        "similar": items,
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}
