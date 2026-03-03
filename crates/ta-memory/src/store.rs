// store.rs — Memory store trait and core types.
//
// Agent-agnostic persistent memory that works across agent frameworks.
// TA owns the memory — agents consume it through MCP tools or CLI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::MemoryError;

/// A stored memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub entry_id: Uuid,
    pub key: String,
    pub value: serde_json::Value,
    pub tags: Vec<String>,
    pub source: String,
    pub goal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Query parameters for looking up memory entries.
#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    /// Prefix match on key.
    pub key_prefix: Option<String>,
    /// All of these tags must be present.
    pub tags: Vec<String>,
    /// Restrict to a specific goal's memories.
    pub goal_id: Option<Uuid>,
    /// Maximum number of results.
    pub limit: Option<usize>,
}

/// Pluggable memory storage backend.
pub trait MemoryStore: Send + Sync {
    /// Store a memory entry. Overwrites if key already exists.
    fn store(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
    ) -> Result<MemoryEntry, MemoryError>;

    /// Retrieve a single entry by exact key.
    fn recall(&self, key: &str) -> Result<Option<MemoryEntry>, MemoryError>;

    /// Search entries by query parameters (prefix, tags, goal_id).
    fn lookup(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// List all entries, optionally limited.
    fn list(&self, limit: Option<usize>) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// Delete an entry by key. Returns true if it existed.
    fn forget(&mut self, key: &str) -> Result<bool, MemoryError>;
}
