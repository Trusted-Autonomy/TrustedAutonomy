//! JSON-over-stdio protocol types for external memory backend plugins.
//!
//! Memory backend plugins communicate with TA using a request/response protocol
//! over stdin/stdout. TA spawns the plugin process, writes one JSON operation
//! line to stdin, reads one JSON response line from stdout.
//!
//! ## Protocol overview
//!
//! ```text
//! TA → plugin: {"op":"<name>",...params}
//! plugin → TA: {"ok":true,"entry":{...}}   or   {"ok":false,"error":"..."}
//! ```
//!
//! ## Operations
//!
//! | Op                | Description                                           |
//! |-------------------|-------------------------------------------------------|
//! | `handshake`       | Version negotiation; first call on every spawn        |
//! | `store`           | Store a memory entry (overwrites if key exists)       |
//! | `recall`          | Retrieve entry by exact key                           |
//! | `lookup`          | Search entries by prefix, tags, category, phase       |
//! | `forget`          | Delete entry by key                                   |
//! | `semantic_search` | Semantic similarity search (optional capability)      |
//! | `stats`           | Aggregate statistics about the store                  |

use serde::{Deserialize, Serialize};

use crate::store::{MemoryEntry, MemoryStats};

/// Protocol version implemented by this TA build.
pub const MEMORY_PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Request envelope
// ---------------------------------------------------------------------------

/// Request sent from TA to a memory plugin over stdin.
///
/// One JSON line per request.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryPluginRequest {
    /// Operation name (e.g., "handshake", "store", "recall").
    pub op: String,

    /// Operation parameters (flat fields; structure depends on op).
    #[serde(flatten)]
    pub params: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Response envelope
// ---------------------------------------------------------------------------

/// Response sent from a memory plugin to TA over stdout.
///
/// One JSON line per response.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryPluginResponse {
    /// Whether the operation succeeded.
    pub ok: bool,

    /// Single entry result (for `store`, `recall`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<MemoryEntry>,

    /// Multiple entry results (for `lookup`, `semantic_search`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<MemoryEntry>>,

    /// Boolean result (for `forget` — true if the key existed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,

    /// Stats result (for `stats`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<MemoryStats>,

    /// Handshake result fields (for `handshake`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,

    /// Human-readable error message (only set when ok=false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl MemoryPluginResponse {
    /// Construct a success response with a single entry.
    pub fn entry(entry: MemoryEntry) -> Self {
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

    /// Construct a success response with multiple entries.
    pub fn entries(entries: Vec<MemoryEntry>) -> Self {
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

    /// Construct a success response for forget (with deleted flag).
    pub fn deleted(deleted: bool) -> Self {
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

    /// Construct a stats response.
    pub fn stats(stats: MemoryStats) -> Self {
        Self {
            ok: true,
            entry: None,
            entries: None,
            deleted: None,
            stats: Some(stats),
            plugin_name: None,
            plugin_version: None,
            protocol_version: None,
            capabilities: vec![],
            error: None,
        }
    }

    /// Construct an error response.
    pub fn error(msg: impl Into<String>) -> Self {
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
// Handshake
// ---------------------------------------------------------------------------

/// Parameters for the `handshake` op.
#[derive(Debug, Serialize, Deserialize)]
pub struct HandshakeParams {
    /// TA binary version string (semver).
    pub ta_version: String,
    /// Protocol version TA is using.
    pub protocol_version: u32,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Parameters for the `store` op.
#[derive(Debug, Serialize, Deserialize)]
pub struct StoreParams {
    /// Memory key.
    pub key: String,
    /// Value to store (arbitrary JSON).
    pub value: serde_json::Value,
    /// Tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Source framework or CLI identifier.
    pub source: String,
    /// Optional goal ID (UUID string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    /// Knowledge category (convention, architecture, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Optional expiration timestamp (RFC3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Confidence score 0.0–1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Plan phase ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Recall
// ---------------------------------------------------------------------------

/// Parameters for the `recall` op.
#[derive(Debug, Serialize, Deserialize)]
pub struct RecallParams {
    /// Exact key to retrieve.
    pub key: String,
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// Parameters for the `lookup` op.
#[derive(Debug, Serialize, Deserialize)]
pub struct LookupParams {
    /// Optional key prefix filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// All of these tags must be present.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Filter by goal ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    /// Filter by category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Filter by plan phase (returns matching phase and global entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    /// Maximum results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Forget
// ---------------------------------------------------------------------------

/// Parameters for the `forget` op.
#[derive(Debug, Serialize, Deserialize)]
pub struct ForgetParams {
    /// Key to delete.
    pub key: String,
}

// ---------------------------------------------------------------------------
// Semantic search
// ---------------------------------------------------------------------------

/// Parameters for the `semantic_search` op.
#[derive(Debug, Serialize, Deserialize)]
pub struct SemanticSearchParams {
    /// Query text for semantic similarity.
    pub query: String,
    /// Optional pre-computed embedding vector (saves re-embedding in the plugin).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding: Vec<f32>,
    /// Maximum results to return.
    pub k: usize,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(MEMORY_PROTOCOL_VERSION, 1);
    }

    #[test]
    fn request_roundtrip_store() {
        let params = StoreParams {
            key: "arch:overview".to_string(),
            value: serde_json::json!({"desc": "main module"}),
            tags: vec!["arch".to_string()],
            source: "cli".to_string(),
            goal_id: None,
            category: Some("architecture".to_string()),
            expires_at: None,
            confidence: Some(0.9),
            phase_id: None,
        };
        let req = MemoryPluginRequest {
            op: "store".to_string(),
            params: serde_json::to_value(&params).unwrap(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: MemoryPluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.op, "store");
    }

    #[test]
    fn request_roundtrip_recall() {
        let params = RecallParams {
            key: "arch:overview".to_string(),
        };
        let req = MemoryPluginRequest {
            op: "recall".to_string(),
            params: serde_json::to_value(&params).unwrap(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: MemoryPluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.op, "recall");
    }

    #[test]
    fn response_error_roundtrip() {
        let resp = MemoryPluginResponse::error("connection refused: check API key");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: MemoryPluginResponse = serde_json::from_str(&json).unwrap();
        assert!(!parsed.ok);
        assert!(parsed.error.unwrap().contains("connection refused"));
    }

    #[test]
    fn response_deleted_roundtrip() {
        let resp = MemoryPluginResponse::deleted(true);
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: MemoryPluginResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.deleted, Some(true));
    }

    #[test]
    fn handshake_params_roundtrip() {
        let params = HandshakeParams {
            ta_version: "0.14.6-alpha.5".to_string(),
            protocol_version: MEMORY_PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: HandshakeParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.protocol_version, 1);
    }

    #[test]
    fn lookup_params_defaults() {
        let params = LookupParams {
            prefix: Some("arch:".to_string()),
            tags: vec![],
            goal_id: None,
            category: None,
            phase_id: None,
            limit: Some(10),
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: LookupParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.prefix.as_deref(), Some("arch:"));
        assert_eq!(parsed.limit, Some(10));
    }

    #[test]
    fn semantic_search_params_with_embedding() {
        let params = SemanticSearchParams {
            query: "module architecture".to_string(),
            embedding: vec![0.021, -0.134, 0.567],
            k: 5,
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: SemanticSearchParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.k, 5);
        assert!((parsed.embedding[0] - 0.021_f32).abs() < 1e-5);
    }
}
