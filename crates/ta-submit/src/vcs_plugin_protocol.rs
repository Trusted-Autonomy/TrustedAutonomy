//! JSON-over-stdio protocol types for external VCS adapter plugins.
//!
//! VCS adapter plugins communicate with TA using a request/response protocol
//! over stdin/stdout. TA spawns the plugin process, writes one JSON request
//! line to stdin, reads one JSON response line from stdout.
//!
//! ## Protocol overview
//!
//! Every exchange is a single JSON line in each direction:
//!
//! ```text
//! TA → plugin: {"method":"<name>","params":{...}}
//! plugin → TA: {"ok":true,"result":{...}}   or   {"ok":false,"error":"..."}
//! ```
//!
//! ## Message methods
//!
//! | Method              | Direction | Description                                      |
//! |---------------------|-----------|--------------------------------------------------|
//! | `handshake`         | TA→plugin | Version negotiation; first call on every spawn   |
//! | `detect`            | TA→plugin | Auto-detect from project root                    |
//! | `exclude_patterns`  | TA→plugin | Patterns to exclude from staging copy             |
//! | `save_state`        | TA→plugin | Save VCS working state before apply              |
//! | `restore_state`     | TA→plugin | Restore saved state after apply                  |
//! | `commit`            | TA→plugin | Commit staged changes                            |
//! | `push`              | TA→plugin | Push committed changes                           |
//! | `open_review`       | TA→plugin | Open a review request (PR, Swarm review, etc.)   |
//! | `revision_id`       | TA→plugin | Get current revision identifier                  |
//! | `protected_targets` | TA→plugin | Get §15 protected submit targets                 |
//! | `verify_target`     | TA→plugin | §15 invariant check post-prepare()               |
//! | `sync_upstream`     | TA→plugin | Sync local workspace with upstream               |
//! | `prepare`           | TA→plugin | Create feature branch / changelist               |
//! | `check_review`      | TA→plugin | Check status of an open review                   |
//! | `merge_review`      | TA→plugin | Merge / submit a review                          |

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Protocol version implemented by this TA build.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Request envelope
// ---------------------------------------------------------------------------

/// Request sent from TA to a VCS plugin over stdin.
///
/// One JSON line per request. The plugin processes it and writes one
/// `VcsPluginResponse` line to stdout, then the process exits.
#[derive(Debug, Serialize, Deserialize)]
pub struct VcsPluginRequest {
    /// Method name (e.g., "handshake", "commit", "detect").
    pub method: String,

    /// Method parameters (structure depends on method).
    pub params: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Response envelope
// ---------------------------------------------------------------------------

/// Response sent from a VCS plugin to TA over stdout.
///
/// One JSON line per response.
#[derive(Debug, Serialize, Deserialize)]
pub struct VcsPluginResponse {
    /// Whether the operation succeeded.
    pub ok: bool,

    /// Result payload (structure depends on method; null on error).
    #[serde(default)]
    pub result: serde_json::Value,

    /// Human-readable error message (only set when ok=false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl VcsPluginResponse {
    /// Construct a success response.
    pub fn success(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    /// Construct an error response.
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: serde_json::Value::Null,
            error: Some(msg.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// Parameters for the `handshake` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct HandshakeParams {
    /// TA binary version string (semver).
    pub ta_version: String,
    /// Protocol version TA is using (currently 1).
    pub protocol_version: u32,
}

/// Result from the `handshake` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct HandshakeResult {
    /// Plugin's self-reported version string.
    pub plugin_version: String,
    /// Protocol version the plugin supports.
    pub protocol_version: u32,
    /// Adapter name (e.g., "perforce", "svn").
    pub adapter_name: String,
    /// List of capabilities this plugin supports (maps to `plugin.toml`).
    #[serde(default)]
    pub capabilities: Vec<String>,
}

// ---------------------------------------------------------------------------
// detect
// ---------------------------------------------------------------------------

/// Parameters for the `detect` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct DetectParams {
    /// Absolute path to the project root directory.
    pub project_root: String,
}

/// Result from the `detect` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct DetectResult {
    /// Whether this adapter applies to the given project root.
    pub detected: bool,
}

// ---------------------------------------------------------------------------
// exclude_patterns
// ---------------------------------------------------------------------------

// No params needed — plugin returns patterns for its VCS metadata dirs.

/// Result from the `exclude_patterns` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExcludePatternsResult {
    /// Patterns in .taignore format (e.g., ".p4config", ".svn/").
    pub patterns: Vec<String>,
}

// ---------------------------------------------------------------------------
// save_state / restore_state
// ---------------------------------------------------------------------------

/// Result from the `save_state` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct SaveStateResult {
    /// Opaque adapter state. TA passes this back to `restore_state`.
    /// `null` means no state was saved (adapter returned None).
    pub state: serde_json::Value,
}

/// Parameters for the `restore_state` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreStateParams {
    /// Opaque state returned by a previous `save_state` call.
    pub state: serde_json::Value,
}

// ---------------------------------------------------------------------------
// prepare
// ---------------------------------------------------------------------------

/// Parameters for the `prepare` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct PrepareParams {
    /// Goal ID.
    pub goal_id: String,
    /// Goal title.
    pub goal_title: String,
    /// Absolute path to the workspace.
    pub workspace_path: String,
    /// Branch prefix from config (e.g., "feature/").
    pub branch_prefix: String,
    /// Co-author string for commit messages, if any.
    #[serde(default)]
    pub co_author: Option<String>,
}

// ---------------------------------------------------------------------------
// commit
// ---------------------------------------------------------------------------

/// Parameters for the `commit` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitParams {
    /// Goal ID.
    pub goal_id: String,
    /// Goal title.
    pub goal_title: String,
    /// Commit message text.
    pub message: String,
    /// Files changed (paths relative to workspace root).
    pub changed_files: Vec<String>,
}

/// Result from the `commit` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitResult {
    /// Commit identifier (hash, changelist number, etc.).
    pub commit_id: String,
    /// Human-readable message.
    pub message: String,
    /// Adapter-specific metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// push
// ---------------------------------------------------------------------------

/// Parameters for the `push` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct PushParams {
    /// Goal ID.
    pub goal_id: String,
}

/// Result from the `push` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct PushResult {
    /// Remote reference (branch name, changelist URL, etc.).
    pub remote_ref: String,
    /// Human-readable message.
    pub message: String,
    /// Adapter-specific metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// open_review
// ---------------------------------------------------------------------------

/// Parameters for the `open_review` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenReviewParams {
    /// Goal ID.
    pub goal_id: String,
    /// Goal title.
    pub goal_title: String,
    /// Draft package summary (human-readable description of changes).
    pub draft_summary: String,
    /// List of changed files.
    pub changed_files: Vec<String>,
}

/// Result from the `open_review` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenReviewResult {
    /// Review URL (GitHub PR, Perforce Swarm review, etc.).
    pub review_url: String,
    /// Review identifier (PR number, CL number, etc.).
    pub review_id: String,
    /// Human-readable message.
    pub message: String,
    /// Adapter-specific metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// revision_id
// ---------------------------------------------------------------------------

/// Result from the `revision_id` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct RevisionIdResult {
    /// Current revision identifier (e.g., "abc1234", "r1234", "@5678").
    pub revision_id: String,
}

// ---------------------------------------------------------------------------
// protected_targets / verify_target  (§15 compliance)
// ---------------------------------------------------------------------------

/// Result from the `protected_targets` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProtectedTargetsResult {
    /// List of protected refs/branches/paths.
    pub targets: Vec<String>,
}

// verify_target: no additional result struct needed — the envelope ok/error is sufficient.
// On success the plugin returns `ok: true` with an empty result.
// On violation it returns `ok: false` with a descriptive error.

// ---------------------------------------------------------------------------
// sync_upstream
// ---------------------------------------------------------------------------

/// Result from the `sync_upstream` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncUpstreamResult {
    /// Whether upstream had new changes.
    pub updated: bool,
    /// Files with merge conflicts.
    pub conflicts: Vec<String>,
    /// Number of new upstream commits.
    pub new_commits: u32,
    /// Human-readable summary.
    pub message: String,
    /// Adapter-specific metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// check_review
// ---------------------------------------------------------------------------

/// Parameters for the `check_review` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckReviewParams {
    /// Review identifier (PR number, CL number, etc.).
    pub review_id: String,
}

/// Result from the `check_review` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckReviewResult {
    /// Whether review info was found (false = review not found by adapter).
    pub found: bool,
    /// Current state (e.g., "open", "merged", "closed").
    #[serde(default)]
    pub state: String,
    /// Whether CI checks are passing.
    #[serde(default)]
    pub checks_passing: Option<bool>,
}

// ---------------------------------------------------------------------------
// merge_review
// ---------------------------------------------------------------------------

/// Parameters for the `merge_review` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct MergeReviewParams {
    /// Review identifier to merge.
    pub review_id: String,
}

/// Result from the `merge_review` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct MergeReviewResult {
    /// Whether the merge was completed immediately.
    pub merged: bool,
    /// Merge commit SHA or changelist number.
    #[serde(default)]
    pub merge_commit: Option<String>,
    /// Human-readable message.
    pub message: String,
    /// Adapter-specific metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = VcsPluginRequest {
            method: "handshake".to_string(),
            params: serde_json::json!({
                "ta_version": "0.13.5-alpha",
                "protocol_version": 1
            }),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: VcsPluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "handshake");
    }

    #[test]
    fn response_success_roundtrip() {
        let resp = VcsPluginResponse::success(serde_json::json!({"adapter_name": "perforce"}));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: VcsPluginResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
        assert!(parsed.error.is_none());
    }

    #[test]
    fn response_error_roundtrip() {
        let resp = VcsPluginResponse::error("p4 not found");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: VcsPluginResponse = serde_json::from_str(&json).unwrap();
        assert!(!parsed.ok);
        assert_eq!(parsed.error.as_deref(), Some("p4 not found"));
    }

    #[test]
    fn handshake_params_roundtrip() {
        let params = HandshakeParams {
            ta_version: "0.13.5-alpha".to_string(),
            protocol_version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: HandshakeParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.protocol_version, 1);
    }

    #[test]
    fn handshake_result_roundtrip() {
        let result = HandshakeResult {
            plugin_version: "0.1.0".to_string(),
            protocol_version: 1,
            adapter_name: "perforce".to_string(),
            capabilities: vec![
                "commit".to_string(),
                "push".to_string(),
                "protected_targets".to_string(),
            ],
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: HandshakeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.adapter_name, "perforce");
        assert!(parsed
            .capabilities
            .contains(&"protected_targets".to_string()));
    }

    #[test]
    fn commit_params_roundtrip() {
        let params = CommitParams {
            goal_id: "abc123".to_string(),
            goal_title: "Fix bug".to_string(),
            message: "Fix critical bug\n\nCo-authored-by: test".to_string(),
            changed_files: vec!["src/main.rs".to_string()],
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: CommitParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.goal_id, "abc123");
        assert_eq!(parsed.changed_files.len(), 1);
    }

    #[test]
    fn detect_params_roundtrip() {
        let params = DetectParams {
            project_root: "/home/user/project".to_string(),
        };
        let json = serde_json::to_string(&params).unwrap();
        let parsed: DetectParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.project_root, "/home/user/project");
    }

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
