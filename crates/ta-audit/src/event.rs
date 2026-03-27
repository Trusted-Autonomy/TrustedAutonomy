// event.rs — Audit event data model.
//
// Every significant action in the system (tool call, policy decision,
// approval, commit) is recorded as an AuditEvent. Events form a chain:
// each event includes a `previous_hash` linking it to the prior event,
// enabling tamper detection.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What kind of action this event records.
///
/// In Rust, an `enum` can carry data in each variant (called a "tagged union"
/// or "algebraic data type"). Here we use simple variants without data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// An MCP tool was called (e.g., fs.read, fs.write_patch).
    ToolCall,
    /// The policy engine made a decision (allow/deny/require_approval).
    PolicyDecision,
    /// A human approved a PR package or action.
    Approval,
    /// Changes were applied to the real target (commit/send/post).
    Apply,
    /// An error occurred during processing.
    Error,
    /// A draft was auto-approved by policy (v0.10.15).
    AutoApproval,
}

// ── Decision Observability (v0.3.3) ──

/// An alternative that was considered during a decision.
///
/// Used in `DecisionReasoning` to document what options were evaluated
/// and why they were accepted or rejected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Alternative {
    /// Description of the alternative considered.
    pub description: String,
    /// Optional score or ranking for this alternative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// Why this alternative was rejected (empty string if it was the chosen option).
    pub rejected_reason: String,
}

/// Structured reasoning captured for a decision point.
///
/// Extends `AuditEvent` to make every decision in the TA pipeline observable —
/// not just *what happened*, but *what was considered and why*.
/// Foundation for drift detection (v0.4.2) and compliance reporting (ISO 42001, IEEE 7001).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionReasoning {
    /// What alternatives were considered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Alternative>,
    /// Why this outcome was selected.
    pub rationale: String,
    /// Values/principles that informed the decision (e.g., "default-deny", "least-privilege").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_principles: Vec<String>,
}

/// A single audit event — one line in the JSONL audit log.
///
/// `#[derive(Serialize, Deserialize)]` lets serde automatically convert
/// this struct to/from JSON. Each field maps to a JSON key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique identifier for this event.
    pub event_id: Uuid,

    /// When this event occurred (UTC).
    pub timestamp: DateTime<Utc>,

    /// Which agent performed the action.
    pub agent_id: String,

    /// What kind of action was performed.
    pub action: AuditAction,

    /// The resource affected (e.g., "fs://workspace/src/main.rs").
    /// `Option<T>` means this field can be `Some(value)` or `None` (null in JSON).
    pub target_uri: Option<String>,

    /// SHA-256 hash of the input to this action.
    pub input_hash: Option<String>,

    /// SHA-256 hash of the output/result of this action.
    pub output_hash: Option<String>,

    /// Links this event to a parent event (for causal chaining).
    pub parent_event_id: Option<Uuid>,

    /// Hash of the previous event in the log (for tamper detection).
    /// The first event in the log has this set to None.
    pub previous_hash: Option<String>,

    /// Arbitrary additional data. `serde_json::Value` can hold any JSON.
    #[serde(default)]
    pub metadata: serde_json::Value,

    /// Structured reasoning for this decision (v0.3.3).
    /// Optional — existing events without reasoning still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<DecisionReasoning>,

    /// Caller mode under which this action was performed (v0.10.15).
    /// Values: "normal", "orchestrator", "unrestricted".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_mode: Option<String>,

    /// MCP tool name for tool-call audit entries (v0.10.15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,

    /// Goal run ID associated with this action (v0.10.15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_run_id: Option<Uuid>,

    /// Cryptographic attestation over this event's canonical form (v0.14.1).
    /// When present, the signature was produced by the backend at log-write time.
    /// Verify with `ta audit verify-attestation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<crate::attestation::AttestationRecord>,

    /// 8-char goal shortref derived from `goal_run_id` (v0.14.7.3).
    /// Allows `grep <shortref> audit.jsonl` to find all entries for a goal.
    /// Populated whenever `goal_run_id` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortref: Option<String>,
}

impl AuditEvent {
    /// Create a new audit event with the current timestamp and a random UUID.
    ///
    /// Most fields start as None — set them before logging.
    pub fn new(agent_id: impl Into<String>, action: AuditAction) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            agent_id: agent_id.into(),
            action,
            target_uri: None,
            input_hash: None,
            output_hash: None,
            parent_event_id: None,
            previous_hash: None,
            metadata: serde_json::Value::Null,
            reasoning: None,
            caller_mode: None,
            tool_name: None,
            goal_run_id: None,
            attestation: None,
            shortref: None,
        }
    }

    /// Set the target URI and return self (builder pattern).
    ///
    /// Builder pattern lets you chain calls:
    ///   `AuditEvent::new("agent-1", ToolCall).with_target("fs://...")`
    pub fn with_target(mut self, uri: impl Into<String>) -> Self {
        self.target_uri = Some(uri.into());
        self
    }

    /// Set the input hash and return self.
    pub fn with_input_hash(mut self, hash: impl Into<String>) -> Self {
        self.input_hash = Some(hash.into());
        self
    }

    /// Set the output hash and return self.
    pub fn with_output_hash(mut self, hash: impl Into<String>) -> Self {
        self.output_hash = Some(hash.into());
        self
    }

    /// Set the parent event ID and return self.
    pub fn with_parent(mut self, parent_id: Uuid) -> Self {
        self.parent_event_id = Some(parent_id);
        self
    }

    /// Set arbitrary metadata and return self.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set structured decision reasoning and return self (v0.3.3).
    pub fn with_reasoning(mut self, reasoning: DecisionReasoning) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    /// Set the caller mode and return self (v0.10.15).
    pub fn with_caller_mode(mut self, mode: impl Into<String>) -> Self {
        self.caller_mode = Some(mode.into());
        self
    }

    /// Set the tool name and return self (v0.10.15).
    pub fn with_tool_name(mut self, name: impl Into<String>) -> Self {
        self.tool_name = Some(name.into());
        self
    }

    /// Set the goal run ID and shortref, and return self (v0.10.15).
    ///
    /// Automatically populates `shortref` from the first 8 chars of the UUID (v0.14.7.3).
    pub fn with_goal_run_id(mut self, id: Uuid) -> Self {
        self.shortref = Some(id.to_string()[..8].to_string());
        self.goal_run_id = Some(id);
        self
    }

    /// Set the goal shortref explicitly and return self (v0.14.7.3).
    ///
    /// Normally populated automatically by `with_goal_run_id`. Use this only when
    /// the shortref needs to be set independently (e.g., replay or migration).
    pub fn with_shortref(mut self, shortref: impl Into<String>) -> Self {
        self.shortref = Some(shortref.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serialization_round_trip() {
        // Create an event, serialize to JSON, deserialize back, compare.
        let event = AuditEvent::new("test-agent", AuditAction::ToolCall)
            .with_target("fs://workspace/test.txt")
            .with_input_hash("abc123")
            .with_output_hash("def456");

        // Serialize to JSON string
        let json = serde_json::to_string(&event).expect("serialize");
        // Deserialize back to struct
        let restored: AuditEvent = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(event.event_id, restored.event_id);
        assert_eq!(event.agent_id, restored.agent_id);
        assert_eq!(event.action, restored.action);
        assert_eq!(event.target_uri, restored.target_uri);
        assert_eq!(event.input_hash, restored.input_hash);
        assert_eq!(event.output_hash, restored.output_hash);
    }

    #[test]
    fn event_ids_are_unique() {
        let e1 = AuditEvent::new("agent", AuditAction::ToolCall);
        let e2 = AuditEvent::new("agent", AuditAction::ToolCall);
        assert_ne!(e1.event_id, e2.event_id);
    }

    #[test]
    fn action_serializes_as_snake_case() {
        // Verify that enum variants serialize as snake_case strings, not PascalCase.
        let json = serde_json::to_string(&AuditAction::PolicyDecision).unwrap();
        assert_eq!(json, "\"policy_decision\"");
    }

    // ── v0.3.3 Decision Reasoning tests ──

    #[test]
    fn decision_reasoning_serialization_round_trip() {
        let reasoning = DecisionReasoning {
            alternatives: vec![
                Alternative {
                    description: "Session-based auth".to_string(),
                    score: Some(0.3),
                    rejected_reason: "Doesn't scale across servers".to_string(),
                },
                Alternative {
                    description: "API key auth".to_string(),
                    score: None,
                    rejected_reason: "Not suitable for user-facing flows".to_string(),
                },
            ],
            rationale: "JWT provides stateless, scalable authentication".to_string(),
            applied_principles: vec![
                "least-privilege".to_string(),
                "defense-in-depth".to_string(),
            ],
        };

        let json = serde_json::to_string(&reasoning).unwrap();
        let restored: DecisionReasoning = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.rationale, reasoning.rationale);
        assert_eq!(restored.alternatives.len(), 2);
        assert_eq!(restored.alternatives[0].score, Some(0.3));
        assert_eq!(restored.alternatives[1].score, None);
        assert_eq!(restored.applied_principles.len(), 2);
    }

    #[test]
    fn event_with_reasoning_round_trip() {
        let reasoning = DecisionReasoning {
            alternatives: vec![Alternative {
                description: "Allow without check".to_string(),
                score: None,
                rejected_reason: "Violates default-deny principle".to_string(),
            }],
            rationale: "Grant matched for fs.read on workspace/**".to_string(),
            applied_principles: vec!["default-deny".to_string()],
        };

        let event = AuditEvent::new("test-agent", AuditAction::PolicyDecision)
            .with_target("fs://workspace/src/main.rs")
            .with_reasoning(reasoning);

        let json = serde_json::to_string(&event).unwrap();
        let restored: AuditEvent = serde_json::from_str(&json).unwrap();

        assert!(restored.reasoning.is_some());
        let r = restored.reasoning.unwrap();
        assert_eq!(r.alternatives.len(), 1);
        assert!(r.rationale.contains("Grant matched"));
    }

    #[test]
    fn event_without_reasoning_backward_compatible() {
        // Old events without reasoning field should deserialize fine.
        let json = r#"{
            "event_id": "550e8400-e29b-41d4-a716-446655440000",
            "timestamp": "2026-02-25T12:00:00Z",
            "agent_id": "agent-1",
            "action": "tool_call",
            "target_uri": "fs://workspace/test.txt",
            "input_hash": null,
            "output_hash": null,
            "parent_event_id": null,
            "previous_hash": null,
            "metadata": {}
        }"#;
        let event: AuditEvent = serde_json::from_str(json).unwrap();
        assert!(event.reasoning.is_none());
    }

    #[test]
    fn auto_approval_action_serializes_as_snake_case() {
        let json = serde_json::to_string(&AuditAction::AutoApproval).unwrap();
        assert_eq!(json, "\"auto_approval\"");
    }

    #[test]
    fn caller_mode_and_tool_name_in_event() {
        let event = AuditEvent::new("agent-1", AuditAction::ToolCall)
            .with_caller_mode("orchestrator")
            .with_tool_name("ta_fs_write")
            .with_goal_run_id(Uuid::new_v4());
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"caller_mode\":\"orchestrator\""));
        assert!(json.contains("\"tool_name\":\"ta_fs_write\""));
        assert!(json.contains("\"goal_run_id\""));
    }

    #[test]
    fn caller_mode_absent_backward_compatible() {
        // Events without caller_mode/tool_name should deserialize fine.
        let json = r#"{
            "event_id": "550e8400-e29b-41d4-a716-446655440000",
            "timestamp": "2026-02-25T12:00:00Z",
            "agent_id": "agent-1",
            "action": "tool_call",
            "target_uri": null,
            "input_hash": null,
            "output_hash": null,
            "parent_event_id": null,
            "previous_hash": null,
            "metadata": {}
        }"#;
        let event: AuditEvent = serde_json::from_str(json).unwrap();
        assert!(event.caller_mode.is_none());
        assert!(event.tool_name.is_none());
        assert!(event.goal_run_id.is_none());
    }

    #[test]
    fn reasoning_skips_empty_fields_in_serialization() {
        let reasoning = DecisionReasoning {
            alternatives: vec![],
            rationale: "Simple allow".to_string(),
            applied_principles: vec![],
        };
        let json = serde_json::to_string(&reasoning).unwrap();
        // Empty vecs should be skipped.
        assert!(!json.contains("alternatives"));
        assert!(!json.contains("applied_principles"));
        assert!(json.contains("rationale"));
    }
}
