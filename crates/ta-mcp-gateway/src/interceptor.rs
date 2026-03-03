// interceptor.rs — MCP tool call interceptor (v0.5.1).
//
// Classifies outbound MCP tool calls as read-only (passthrough) or
// state-changing (capture for human review). State-changing calls are
// recorded as PendingAction items in the draft package.

use std::collections::HashSet;

use chrono::Utc;
use uuid::Uuid;

use ta_changeset::draft_package::{ActionKind, PendingAction};

/// Classifies MCP tool calls and produces PendingAction records for
/// state-changing calls.
pub struct ToolCallInterceptor {
    /// Tool names that are always read-only (passed through unintercepted).
    passthrough_tools: HashSet<String>,
}

impl ToolCallInterceptor {
    /// Create an interceptor with the default passthrough rules.
    ///
    /// Built-in TA tools that are read-only are automatically whitelisted.
    /// All external/unknown tools default to state-changing.
    pub fn new() -> Self {
        let passthrough = [
            "ta_fs_read",
            "ta_fs_list",
            "ta_fs_diff",
            "ta_goal_status",
            "ta_goal_list",
            "ta_pr_status",
            "ta_draft",   // draft management is TA-internal, not external
            "ta_plan",    // plan reading is read-only
            "ta_context", // memory operations are TA-internal
        ];
        Self {
            passthrough_tools: passthrough.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Classify a tool call.
    ///
    /// Returns `None` for passthrough (read-only) calls.
    /// Returns `Some(PendingAction)` for state-changing calls that should be captured.
    pub fn classify(&self, tool_name: &str, params: &serde_json::Value) -> Option<PendingAction> {
        if self.passthrough_tools.contains(tool_name) {
            return None;
        }

        // ta_fs_write is TA-internal staging — not an external action.
        if tool_name == "ta_fs_write" {
            return None;
        }

        // Everything else is an external tool call — capture it.
        let kind = self.classify_kind(tool_name);
        let description = self.generate_description(tool_name, params);
        let target_uri = format!("mcp://{}", tool_name.replace('_', "/"));

        Some(PendingAction {
            action_id: Uuid::new_v4(),
            tool_name: tool_name.to_string(),
            parameters: params.clone(),
            kind,
            intercepted_at: Utc::now(),
            description,
            target_uri: Some(target_uri),
            disposition: Default::default(),
        })
    }

    /// Determine the action kind for a tool call.
    fn classify_kind(&self, tool_name: &str) -> ActionKind {
        // Tools with common read-only patterns.
        let read_patterns = [
            "_read", "_get", "_list", "_search", "_find", "_query", "_fetch",
        ];
        for pattern in &read_patterns {
            if tool_name.ends_with(pattern) || tool_name.contains(pattern) {
                return ActionKind::ReadOnly;
            }
        }

        // Tools with common write patterns.
        let write_patterns = [
            "_send", "_post", "_create", "_update", "_delete", "_write", "_put", "_patch",
            "_publish", "_tweet", "_upload",
        ];
        for pattern in &write_patterns {
            if tool_name.ends_with(pattern) || tool_name.contains(pattern) {
                return ActionKind::StateChanging;
            }
        }

        ActionKind::Unclassified
    }

    /// Generate a human-readable description of the tool call.
    fn generate_description(&self, tool_name: &str, params: &serde_json::Value) -> String {
        // Try to extract common descriptive fields from params.
        let subject = params
            .get("subject")
            .or_else(|| params.get("title"))
            .or_else(|| params.get("message"))
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.len() > 60 {
                    format!("{}...", &s[..57])
                } else {
                    s.to_string()
                }
            });

        let to = params
            .get("to")
            .or_else(|| params.get("recipient"))
            .or_else(|| params.get("channel"))
            .and_then(|v| v.as_str());

        match (to, subject) {
            (Some(to), Some(subj)) => format!("{}: to {}, \"{}\"", tool_name, to, subj),
            (Some(to), None) => format!("{}: to {}", tool_name, to),
            (None, Some(subj)) => format!("{}: \"{}\"", tool_name, subj),
            (None, None) => format!("{} (see parameters for details)", tool_name),
        }
    }

    /// Check if a tool name is in the passthrough list (for testing).
    pub fn is_passthrough(&self, tool_name: &str) -> bool {
        self.passthrough_tools.contains(tool_name)
    }
}

impl Default for ToolCallInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_ta_internal_tools() {
        let interceptor = ToolCallInterceptor::new();
        assert!(interceptor.is_passthrough("ta_fs_read"));
        assert!(interceptor.is_passthrough("ta_goal_status"));
        assert!(interceptor.is_passthrough("ta_pr_status"));
        assert!(interceptor.is_passthrough("ta_context"));
    }

    #[test]
    fn ta_fs_write_is_not_intercepted() {
        let interceptor = ToolCallInterceptor::new();
        let result = interceptor.classify("ta_fs_write", &serde_json::json!({}));
        assert!(result.is_none());
    }

    #[test]
    fn external_send_tool_is_captured() {
        let interceptor = ToolCallInterceptor::new();
        let params = serde_json::json!({
            "to": "alice@example.com",
            "subject": "Q3 Report"
        });
        let action = interceptor.classify("gmail_send", &params).unwrap();
        assert_eq!(action.tool_name, "gmail_send");
        assert_eq!(action.kind, ActionKind::StateChanging);
        assert!(action.description.contains("alice@example.com"));
        assert!(action.description.contains("Q3 Report"));
    }

    #[test]
    fn external_read_tool_classified_as_readonly() {
        let interceptor = ToolCallInterceptor::new();
        let action = interceptor
            .classify("gmail_search", &serde_json::json!({"query": "from:bob"}))
            .unwrap();
        assert_eq!(action.kind, ActionKind::ReadOnly);
    }

    #[test]
    fn unknown_tool_classified_as_unclassified() {
        let interceptor = ToolCallInterceptor::new();
        let action = interceptor
            .classify("custom_tool", &serde_json::json!({}))
            .unwrap();
        assert_eq!(action.kind, ActionKind::Unclassified);
    }

    #[test]
    fn target_uri_format() {
        let interceptor = ToolCallInterceptor::new();
        let action = interceptor
            .classify("slack_post_message", &serde_json::json!({}))
            .unwrap();
        assert_eq!(action.target_uri.unwrap(), "mcp://slack/post/message");
    }

    #[test]
    fn changes_backward_compat() {
        // Old JSON without pending_actions must deserialize cleanly.
        use ta_changeset::draft_package::Changes;
        let json = r#"{"artifacts": [], "patch_sets": []}"#;
        let changes: Changes = serde_json::from_str(json).unwrap();
        assert!(changes.pending_actions.is_empty());
    }

    #[test]
    fn pending_action_round_trip() {
        let action = PendingAction {
            action_id: Uuid::new_v4(),
            tool_name: "test_tool".into(),
            parameters: serde_json::json!({"key": "value"}),
            kind: ActionKind::StateChanging,
            intercepted_at: Utc::now(),
            description: "Test action".into(),
            target_uri: Some("mcp://test/tool".into()),
            disposition: Default::default(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let roundtrip: PendingAction = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.tool_name, "test_tool");
        assert_eq!(roundtrip.kind, ActionKind::StateChanging);
    }
}
