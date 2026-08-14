// api_mediator.rs — ResourceMediator for MCP tool calls (v0.7.1).
//
// Stages intercepted MCP tool calls as StagedMutations. After human approval,
// replays the original tool call. Best-effort rollback (many API calls are
// not reversible). Uses the existing ToolCallInterceptor classification to
// determine risk level.
//
// URI scheme: `mcp://<tool_name>` (e.g., "mcp://gmail_send").

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::MediationError;
use crate::mediator::{
    ActionClassification, ApplyResult, MutationPreview, ProposedAction, ResourceMediator,
    StagedMutation,
};

/// Serializable representation of a staged API call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedApiCall {
    /// MCP tool name.
    pub tool_name: String,
    /// Tool call parameters.
    pub parameters: serde_json::Value,
    /// Classification from ToolCallInterceptor.
    pub action_kind: String,
    /// Human-readable description.
    pub description: String,
    /// Timestamp when intercepted.
    pub intercepted_at: chrono::DateTime<Utc>,
}

/// ApiMediator implements ResourceMediator for the `mcp://` scheme.
///
/// Stages MCP tool calls as JSON files in a staging directory. On apply,
/// the caller is responsible for replaying the tool call through the MCP
/// transport (this mediator provides the staged call data for replay).
pub struct ApiMediator {
    /// Directory where staged API calls are serialized.
    staging_dir: PathBuf,
    /// In-memory cache of staged calls (mutation_id → staged call data).
    staged_calls: Mutex<HashMap<Uuid, StagedApiCall>>,
}

impl ApiMediator {
    /// Create a new ApiMediator with the given staging directory.
    pub fn new(staging_dir: &Path) -> Self {
        Self {
            staging_dir: staging_dir.to_path_buf(),
            staged_calls: Mutex::new(HashMap::new()),
        }
    }

    /// Extract tool name from an `mcp://` URI.
    ///
    /// `mcp://gmail_send` → `gmail_send`
    /// `mcp://slack/post/message` → `slack_post_message`
    fn extract_tool_name(uri: &str) -> Option<String> {
        let stripped = uri.strip_prefix("mcp://")?;
        // Normalize: replace / with _ for tool name resolution.
        Some(stripped.replace('/', "_"))
    }

    /// Classify a tool call based on name patterns.
    ///
    /// Delegates to `ta_changeset::tool_classify` (v0.17.6.3) — the same
    /// heuristic `ToolCallInterceptor` uses, previously duplicated here with
    /// its own independently-maintained pattern lists.
    fn classify_tool(tool_name: &str) -> ActionClassification {
        match ta_changeset::tool_classify::classify_tool_name(tool_name) {
            ta_changeset::ToolCallCategory::ReadOnly => ActionClassification::ReadOnly,
            ta_changeset::ToolCallCategory::Irreversible => ActionClassification::Irreversible,
            ta_changeset::ToolCallCategory::ExternalSideEffect => {
                ActionClassification::ExternalSideEffect
            }
            ta_changeset::ToolCallCategory::StateChanging
            | ta_changeset::ToolCallCategory::Unclassified => ActionClassification::StateChanging,
        }
    }

    /// Generate a human-readable description of what a tool call would do.
    fn describe_tool_call(tool_name: &str, params: &serde_json::Value) -> String {
        let subject = params
            .get("subject")
            .or_else(|| params.get("title"))
            .or_else(|| params.get("message"))
            .or_else(|| params.get("content"))
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.len() > 60 {
                    format!("\"{}...\"", &s[..57])
                } else {
                    format!("\"{}\"", s)
                }
            });

        let target = params
            .get("to")
            .or_else(|| params.get("recipient"))
            .or_else(|| params.get("channel"))
            .or_else(|| params.get("path"))
            .or_else(|| params.get("url"))
            .and_then(|v| v.as_str());

        match (target, subject) {
            (Some(t), Some(s)) => format!("Call {} → {} with {}", tool_name, t, s),
            (Some(t), None) => format!("Call {} → {}", tool_name, t),
            (None, Some(s)) => format!("Call {} with {}", tool_name, s),
            (None, None) => format!("Call {} ({})", tool_name, summarize_params(params)),
        }
    }

    /// Load a staged call from disk.
    fn load_staged_call(&self, mutation_id: Uuid) -> Result<StagedApiCall, MediationError> {
        // Check in-memory cache first.
        if let Ok(cache) = self.staged_calls.lock() {
            if let Some(call) = cache.get(&mutation_id) {
                return Ok(call.clone());
            }
        }

        // Load from disk.
        let path = self.staging_dir.join(format!("{}.json", mutation_id));
        let content = std::fs::read_to_string(&path).map_err(|e| MediationError::Io {
            path: path.clone(),
            source: e,
        })?;
        serde_json::from_str(&content).map_err(|e| MediationError::StagingFailed {
            uri: format!("mcp://{}", mutation_id),
            reason: format!("failed to parse staged call: {}", e),
        })
    }
}

/// Summarize JSON parameters for display.
fn summarize_params(params: &serde_json::Value) -> String {
    match params {
        serde_json::Value::Object(map) => {
            let keys: Vec<&str> = map.keys().map(|k| k.as_str()).take(3).collect();
            if keys.is_empty() {
                "no parameters".into()
            } else if map.len() > 3 {
                format!("{} + {} more", keys.join(", "), map.len() - 3)
            } else {
                keys.join(", ")
            }
        }
        serde_json::Value::Null => "no parameters".into(),
        _ => "complex parameters".into(),
    }
}

impl ResourceMediator for ApiMediator {
    fn scheme(&self) -> &str {
        "mcp"
    }

    fn stage(&self, action: ProposedAction) -> Result<StagedMutation, MediationError> {
        let tool_name = Self::extract_tool_name(&action.target_uri).unwrap_or_else(|| {
            // Fallback: use verb as tool name.
            action.verb.clone()
        });

        let staged_call = StagedApiCall {
            tool_name: tool_name.clone(),
            parameters: action.parameters.clone(),
            action_kind: format!("{}", self.classify(&action)),
            description: Self::describe_tool_call(&tool_name, &action.parameters),
            intercepted_at: Utc::now(),
        };

        let mutation_id = Uuid::new_v4();

        // Ensure staging dir exists.
        std::fs::create_dir_all(&self.staging_dir).map_err(|e| MediationError::Io {
            path: self.staging_dir.clone(),
            source: e,
        })?;

        // Write to disk.
        let path = self.staging_dir.join(format!("{}.json", mutation_id));
        let content = serde_json::to_string_pretty(&staged_call).map_err(|e| {
            MediationError::StagingFailed {
                uri: action.target_uri.clone(),
                reason: format!("serialization failed: {}", e),
            }
        })?;
        std::fs::write(&path, &content).map_err(|e| MediationError::Io {
            path: path.clone(),
            source: e,
        })?;

        // Cache in memory.
        if let Ok(mut cache) = self.staged_calls.lock() {
            cache.insert(mutation_id, staged_call);
        }

        Ok(StagedMutation {
            mutation_id,
            action,
            staged_at: Utc::now(),
            preview: None,
            staging_ref: path.to_string_lossy().to_string(),
        })
    }

    fn preview(&self, staged: &StagedMutation) -> Result<MutationPreview, MediationError> {
        let call = self.load_staged_call(staged.mutation_id)?;
        let classification = self.classify(&staged.action);

        let mut risk_flags = Vec::new();
        match classification {
            ActionClassification::Irreversible => {
                risk_flags.push("IRREVERSIBLE: cannot be undone after execution".into());
            }
            ActionClassification::ExternalSideEffect => {
                risk_flags.push("EXTERNAL: affects systems outside TA control".into());
            }
            _ => {}
        }

        // Build a readable diff-like summary of parameters.
        let param_summary = if call.parameters.is_null() {
            None
        } else {
            Some(serde_json::to_string_pretty(&call.parameters).unwrap_or_default())
        };

        Ok(MutationPreview {
            summary: call.description,
            diff: param_summary,
            risk_flags,
            classification,
        })
    }

    fn apply(&self, staged: &StagedMutation) -> Result<ApplyResult, MediationError> {
        // In a real implementation, this would replay the MCP tool call
        // through the transport. For now, we mark it as "ready for replay"
        // and the MCP gateway handles the actual execution.
        let _call = self.load_staged_call(staged.mutation_id)?;

        // Clean up the staged file.
        let path = self
            .staging_dir
            .join(format!("{}.json", staged.mutation_id));
        let _ = std::fs::remove_file(&path);

        // Remove from cache.
        if let Ok(mut cache) = self.staged_calls.lock() {
            cache.remove(&staged.mutation_id);
        }

        Ok(ApplyResult {
            mutation_id: staged.mutation_id,
            success: true,
            message: format!(
                "API call approved for execution: {}",
                staged.action.target_uri
            ),
            applied_at: Utc::now(),
        })
    }

    fn rollback(&self, staged: &StagedMutation) -> Result<(), MediationError> {
        // Remove the staged file.
        let path = self
            .staging_dir
            .join(format!("{}.json", staged.mutation_id));
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| MediationError::Io {
                path: path.clone(),
                source: e,
            })?;
        }

        // Remove from cache.
        if let Ok(mut cache) = self.staged_calls.lock() {
            cache.remove(&staged.mutation_id);
        }

        Ok(())
    }

    fn classify(&self, action: &ProposedAction) -> ActionClassification {
        let tool_name =
            Self::extract_tool_name(&action.target_uri).unwrap_or_else(|| action.verb.clone());
        Self::classify_tool(&tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_mediator() -> (TempDir, ApiMediator) {
        let dir = TempDir::new().unwrap();
        let staging = dir.path().join("mcp-staging");
        let mediator = ApiMediator::new(&staging);
        (dir, mediator)
    }

    #[test]
    fn scheme_is_mcp() {
        let (_dir, mediator) = test_mediator();
        assert_eq!(mediator.scheme(), "mcp");
    }

    #[test]
    fn stage_and_preview() {
        let (_dir, mediator) = test_mediator();
        let action = ProposedAction::new("mcp", "execute", "mcp://gmail_send").with_parameters(
            serde_json::json!({
                "to": "alice@example.com",
                "subject": "Q3 Report"
            }),
        );

        let staged = mediator.stage(action).unwrap();
        assert!(!staged.staging_ref.is_empty());

        let preview = mediator.preview(&staged).unwrap();
        assert!(preview.summary.contains("gmail_send"));
        assert!(preview.summary.contains("alice@example.com"));
        assert_eq!(preview.classification, ActionClassification::Irreversible);
        assert!(!preview.risk_flags.is_empty());
    }

    #[test]
    fn stage_and_apply() {
        let (_dir, mediator) = test_mediator();
        let action = ProposedAction::new("mcp", "execute", "mcp://slack_post_message")
            .with_parameters(serde_json::json!({"channel": "#general", "text": "hello"}));

        let staged = mediator.stage(action).unwrap();
        let result = mediator.apply(&staged).unwrap();
        assert!(result.success);
        assert!(result.message.contains("approved"));

        // Staged file should be cleaned up.
        let path = Path::new(&staged.staging_ref);
        assert!(!path.exists());
    }

    #[test]
    fn stage_and_rollback() {
        let (_dir, mediator) = test_mediator();
        let action = ProposedAction::new("mcp", "execute", "mcp://custom_tool");
        let staged = mediator.stage(action).unwrap();

        let path = PathBuf::from(&staged.staging_ref);
        assert!(path.exists());

        mediator.rollback(&staged).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn classify_read_tools() {
        let (_dir, mediator) = test_mediator();
        let action = ProposedAction::new("mcp", "execute", "mcp://gmail_search");
        assert_eq!(mediator.classify(&action), ActionClassification::ReadOnly);

        let action = ProposedAction::new("mcp", "execute", "mcp://drive_list");
        assert_eq!(mediator.classify(&action), ActionClassification::ReadOnly);
    }

    #[test]
    fn classify_irreversible_tools() {
        let (_dir, mediator) = test_mediator();
        let action = ProposedAction::new("mcp", "execute", "mcp://gmail_send");
        assert_eq!(
            mediator.classify(&action),
            ActionClassification::Irreversible
        );

        let action = ProposedAction::new("mcp", "execute", "mcp://db_delete");
        assert_eq!(
            mediator.classify(&action),
            ActionClassification::Irreversible
        );
    }

    #[test]
    fn classify_external_tools() {
        let (_dir, mediator) = test_mediator();
        let action = ProposedAction::new("mcp", "execute", "mcp://jira_create");
        assert_eq!(
            mediator.classify(&action),
            ActionClassification::ExternalSideEffect
        );
    }

    #[test]
    fn extract_tool_name_variants() {
        assert_eq!(
            ApiMediator::extract_tool_name("mcp://gmail_send"),
            Some("gmail_send".into())
        );
        assert_eq!(
            ApiMediator::extract_tool_name("mcp://slack/post/message"),
            Some("slack_post_message".into())
        );
        assert_eq!(ApiMediator::extract_tool_name("fs://file"), None);
    }

    #[test]
    fn describe_with_subject_and_target() {
        let desc = ApiMediator::describe_tool_call(
            "gmail_send",
            &serde_json::json!({"to": "alice@co.com", "subject": "Report"}),
        );
        assert!(desc.contains("gmail_send"));
        assert!(desc.contains("alice@co.com"));
        assert!(desc.contains("Report"));
    }

    #[test]
    fn describe_no_params() {
        let desc = ApiMediator::describe_tool_call("custom_tool", &serde_json::json!(null));
        assert!(desc.contains("custom_tool"));
        assert!(desc.contains("no parameters"));
    }

    #[test]
    fn summarize_params_formats_keys() {
        let params = serde_json::json!({"a": 1, "b": 2});
        let summary = summarize_params(&params);
        assert!(summary.contains("a"));
        assert!(summary.contains("b"));
    }

    #[test]
    fn staged_api_call_serialization() {
        let call = StagedApiCall {
            tool_name: "test_tool".into(),
            parameters: serde_json::json!({"key": "value"}),
            action_kind: "state_changing".into(),
            description: "Test call".into(),
            intercepted_at: Utc::now(),
        };
        let json = serde_json::to_string(&call).unwrap();
        let restored: StagedApiCall = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.tool_name, "test_tool");
    }
}
