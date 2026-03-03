// store.rs — Memory store trait and core types.
//
// Agent-agnostic persistent memory that works across agent frameworks.
// TA owns the memory — agents consume it through MCP tools or CLI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::MemoryError;

/// Categories for memory entries (v0.5.6 framework-agnostic state).
///
/// Used to classify what kind of knowledge a memory entry represents,
/// enabling targeted recall (e.g., "give me all conventions for this project").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// Coding style, formatting rules, tool preferences.
    Convention,
    /// Module layout, dependency relationships, design decisions.
    Architecture,
    /// What was tried and why it succeeded or failed.
    History,
    /// Human workflow preferences (small PRs, never auto-commit, etc.).
    Preference,
    /// File/module dependency relationships.
    Relationship,
    /// Uncategorized or user-defined.
    Other,
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Convention => write!(f, "convention"),
            Self::Architecture => write!(f, "architecture"),
            Self::History => write!(f, "history"),
            Self::Preference => write!(f, "preference"),
            Self::Relationship => write!(f, "relationship"),
            Self::Other => write!(f, "other"),
        }
    }
}

impl MemoryCategory {
    /// Parse from a string, falling back to `Other` for unknown values.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "convention" => Self::Convention,
            "architecture" => Self::Architecture,
            "history" => Self::History,
            "preference" => Self::Preference,
            "relationship" => Self::Relationship,
            _ => Self::Other,
        }
    }
}

/// A stored memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub entry_id: Uuid,
    pub key: String,
    pub value: serde_json::Value,
    pub tags: Vec<String>,
    pub source: String,
    pub goal_id: Option<Uuid>,
    /// Knowledge category for targeted recall (v0.5.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MemoryCategory>,
    /// Optional expiration time (v0.5.7 TTL support).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Confidence score 0.0–1.0 (v0.5.7). Approved-draft entries default to 1.0,
    /// auto-captured entries default to 0.5.
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_confidence() -> f64 {
    0.5
}

/// Aggregate statistics about the memory store (v0.5.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Total number of entries.
    pub total_entries: usize,
    /// Entries per category.
    pub by_category: std::collections::HashMap<String, usize>,
    /// Entries per source framework.
    pub by_source: std::collections::HashMap<String, usize>,
    /// Number of expired entries.
    pub expired_count: usize,
    /// Average confidence score.
    pub avg_confidence: f64,
    /// Oldest entry timestamp.
    pub oldest_entry: Option<DateTime<Utc>>,
    /// Newest entry timestamp.
    pub newest_entry: Option<DateTime<Utc>>,
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
    /// Filter by category.
    pub category: Option<MemoryCategory>,
    /// Maximum number of results.
    pub limit: Option<usize>,
}

/// Parameters for storing a memory entry (v0.5.6+).
///
/// Provides a builder-style API so callers can set optional fields
/// without breaking the existing `store()` signature.
#[derive(Debug, Clone, Default)]
pub struct StoreParams {
    /// Associate this entry with a specific goal.
    pub goal_id: Option<Uuid>,
    /// Classify the entry for targeted recall.
    pub category: Option<MemoryCategory>,
    /// Optional expiration time (v0.5.7).
    pub expires_at: Option<DateTime<Utc>>,
    /// Confidence score 0.0–1.0 (v0.5.7). None uses the default (0.5).
    pub confidence: Option<f64>,
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

    /// Store a memory entry with extended parameters (goal_id, category, expires_at, confidence).
    fn store_with_params(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
        params: StoreParams,
    ) -> Result<MemoryEntry, MemoryError> {
        // Default: delegate to basic store, then patch fields.
        // Backends can override for atomic writes.
        let mut entry = self.store(key, value, tags, source)?;
        entry.goal_id = params.goal_id;
        entry.category = params.category;
        entry.expires_at = params.expires_at;
        if let Some(c) = params.confidence {
            entry.confidence = c;
        }
        Ok(entry)
    }

    /// Retrieve a single entry by exact key.
    fn recall(&self, key: &str) -> Result<Option<MemoryEntry>, MemoryError>;

    /// Retrieve a single entry by its UUID (v0.5.7).
    fn find_by_id(&self, id: Uuid) -> Result<Option<MemoryEntry>, MemoryError> {
        // Default: linear scan. Backends can override for efficiency.
        let all = self.list(None)?;
        Ok(all.into_iter().find(|e| e.entry_id == id))
    }

    /// Search entries by query parameters (prefix, tags, goal_id, category).
    fn lookup(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// List all entries, optionally limited.
    fn list(&self, limit: Option<usize>) -> Result<Vec<MemoryEntry>, MemoryError>;

    /// Delete an entry by key. Returns true if it existed.
    fn forget(&mut self, key: &str) -> Result<bool, MemoryError>;

    /// Semantic search: find entries whose value is similar to the query text.
    ///
    /// Returns up to `k` entries ranked by relevance. Only meaningful with
    /// vector-capable backends (e.g., `RuVectorStore`). The default
    /// implementation returns an empty vec for backends without vector support.
    fn semantic_search(&self, _query: &str, _k: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        Ok(vec![])
    }

    /// Compute aggregate statistics about the memory store (v0.5.7).
    fn stats(&self) -> Result<MemoryStats, MemoryError> {
        let all = self.list(None)?;
        let now = chrono::Utc::now();
        let total = all.len();

        let mut by_category = std::collections::HashMap::new();
        let mut by_source = std::collections::HashMap::new();
        let mut expired = 0usize;
        let mut confidence_sum = 0.0f64;
        let mut oldest: Option<DateTime<Utc>> = None;
        let mut newest: Option<DateTime<Utc>> = None;

        for e in &all {
            let cat = e
                .category
                .as_ref()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "other".to_string());
            *by_category.entry(cat).or_insert(0usize) += 1;
            *by_source.entry(e.source.clone()).or_insert(0usize) += 1;

            if let Some(exp) = e.expires_at {
                if exp < now {
                    expired += 1;
                }
            }
            confidence_sum += e.confidence;

            match oldest {
                None => oldest = Some(e.created_at),
                Some(o) if e.created_at < o => oldest = Some(e.created_at),
                _ => {}
            }
            match newest {
                None => newest = Some(e.created_at),
                Some(n) if e.created_at > n => newest = Some(e.created_at),
                _ => {}
            }
        }

        Ok(MemoryStats {
            total_entries: total,
            by_category,
            by_source,
            expired_count: expired,
            avg_confidence: if total > 0 {
                confidence_sum / total as f64
            } else {
                0.0
            },
            oldest_entry: oldest,
            newest_entry: newest,
        })
    }
}
