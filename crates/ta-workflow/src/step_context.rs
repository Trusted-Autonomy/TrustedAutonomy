// step_context.rs — WorkflowContext, StepInput, StepOutput (v0.17.0.4.5).
//
// WorkflowContext: extended state that flows through all transitions.
// StepInput: what a step receives on entry.
// StepOutput: what a step emits on exit.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ── TransitionRecord ──────────────────────────────────────────────────────────

/// A record of a completed transition, stored in the audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionRecord {
    pub from_step: String,
    pub to_step: String,
    pub edge: String,
    pub context_patch: Map<String, Value>,
}

// ── TransitionPayload ─────────────────────────────────────────────────────────

/// Payload carried by a transition into the next step.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransitionPayload {
    /// Step that emitted this payload.
    #[serde(default)]
    pub source_step: String,
    /// Edge name (the transition's `on` field).
    #[serde(default)]
    pub edge: String,
    /// Arbitrary data emitted by the previous step's transition.
    #[serde(default)]
    pub data: Value,
}

// ── WorkflowContext ───────────────────────────────────────────────────────────

/// Extended state object that flows through all transitions.
///
/// `fields` accumulates mutable key-value state across the entire workflow.
/// `history` is a full audit trail of all transitions that have fired.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowContext {
    pub fields: Map<String, Value>,
    pub history: Vec<TransitionRecord>,
}

impl WorkflowContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieve a field value by key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    /// Merge a context_patch into `fields`.
    pub fn apply_patch(&mut self, patch: Map<String, Value>) {
        for (k, v) in patch {
            self.fields.insert(k, v);
        }
    }

    /// Record a transition in the audit trail.
    pub fn record_transition(&mut self, record: TransitionRecord) {
        self.history.push(record);
    }

    /// Return a numeric field value (for guard evaluation).
    pub fn get_number(&self, key: &str) -> Option<f64> {
        self.fields.get(key).and_then(|v| v.as_f64())
    }

    /// Return a string field value (for guard evaluation).
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(|v| v.as_str())
    }

    /// Return a boolean field value (for guard evaluation).
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.fields.get(key).and_then(|v| v.as_bool())
    }
}

// ── StepInput ─────────────────────────────────────────────────────────────────

/// Input to a step on entry.
///
/// Includes the step ID, the trigger payload from the previous transition,
/// and the current workflow context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepInput {
    pub step_id: String,
    /// Payload from the transition that entered this step. `None` for the initial step.
    pub trigger: Option<TransitionPayload>,
    pub context: WorkflowContext,
}

impl StepInput {
    pub fn initial(step_id: impl Into<String>, context: WorkflowContext) -> Self {
        Self {
            step_id: step_id.into(),
            trigger: None,
            context,
        }
    }

    pub fn with_trigger(
        step_id: impl Into<String>,
        trigger: TransitionPayload,
        context: WorkflowContext,
    ) -> Self {
        Self {
            step_id: step_id.into(),
            trigger: Some(trigger),
            context,
        }
    }
}

// ── StepOutput ────────────────────────────────────────────────────────────────

/// Output emitted by a step on exit.
///
/// The executor returns this after finding the matching transition, dispatching
/// actions, and assembling the context patch.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepOutput {
    /// The edge taken (transition's `on` field). Set to `"__terminal__"` when no
    /// transition fires (workflow halts at this step).
    pub edge: String,
    /// Payload to carry to the next step's trigger.
    pub data: Value,
    /// Fields to merge into `WorkflowContext.fields` before advancing.
    pub context_patch: Map<String, Value>,
}

impl StepOutput {
    pub fn new(edge: impl Into<String>) -> Self {
        Self {
            edge: edge.into(),
            data: Value::Null,
            context_patch: Map::new(),
        }
    }

    pub fn terminal() -> Self {
        Self {
            edge: "__terminal__".to_string(),
            data: Value::Null,
            context_patch: Map::new(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.edge == "__terminal__"
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_context_apply_patch_merges_fields() {
        let mut ctx = WorkflowContext::new();
        let mut patch = Map::new();
        patch.insert("rework_count".to_string(), Value::Number(2.into()));
        ctx.apply_patch(patch);
        assert_eq!(ctx.get_number("rework_count"), Some(2.0));
    }

    #[test]
    fn workflow_context_apply_patch_overwrites_existing() {
        let mut ctx = WorkflowContext::new();
        ctx.fields
            .insert("count".to_string(), Value::Number(1.into()));
        let mut patch = Map::new();
        patch.insert("count".to_string(), Value::Number(5.into()));
        ctx.apply_patch(patch);
        assert_eq!(ctx.get_number("count"), Some(5.0));
    }

    #[test]
    fn workflow_context_record_transition_appends_history() {
        let mut ctx = WorkflowContext::new();
        ctx.record_transition(TransitionRecord {
            from_step: "review".to_string(),
            to_step: "build".to_string(),
            edge: "apply".to_string(),
            context_patch: Map::new(),
        });
        assert_eq!(ctx.history.len(), 1);
        assert_eq!(ctx.history[0].edge, "apply");
        assert_eq!(ctx.history[0].from_step, "review");
    }

    #[test]
    fn workflow_context_get_typed_accessors() {
        let mut ctx = WorkflowContext::new();
        ctx.fields.insert("n".to_string(), Value::Number(42.into()));
        ctx.fields
            .insert("s".to_string(), Value::String("hello".to_string()));
        ctx.fields.insert("b".to_string(), Value::Bool(true));
        assert_eq!(ctx.get_number("n"), Some(42.0));
        assert_eq!(ctx.get_str("s"), Some("hello"));
        assert_eq!(ctx.get_bool("b"), Some(true));
        assert!(ctx.get_number("missing").is_none());
    }

    #[test]
    fn step_input_initial_has_no_trigger() {
        let input = StepInput::initial("review", WorkflowContext::new());
        assert_eq!(input.step_id, "review");
        assert!(input.trigger.is_none());
    }

    #[test]
    fn step_input_with_trigger_carries_payload() {
        let trigger = TransitionPayload {
            source_step: "prev".to_string(),
            edge: "apply".to_string(),
            data: serde_json::json!({"draft_id": "d1"}),
        };
        let input = StepInput::with_trigger("build", trigger, WorkflowContext::new());
        assert!(input.trigger.is_some());
        assert_eq!(input.trigger.unwrap().edge, "apply");
    }

    #[test]
    fn step_output_terminal_detection() {
        let out = StepOutput::terminal();
        assert!(out.is_terminal());
        assert_eq!(out.edge, "__terminal__");

        let out2 = StepOutput::new("apply");
        assert!(!out2.is_terminal());
    }

    #[test]
    fn workflow_context_serde_round_trip() {
        let mut ctx = WorkflowContext::new();
        ctx.fields
            .insert("count".to_string(), Value::Number(5.into()));
        ctx.record_transition(TransitionRecord {
            from_step: "a".to_string(),
            to_step: "b".to_string(),
            edge: "ok".to_string(),
            context_patch: Map::new(),
        });
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: WorkflowContext = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.get_number("count"), Some(5.0));
        assert_eq!(restored.history.len(), 1);
    }

    #[test]
    fn step_output_serde_round_trip() {
        let mut out = StepOutput::new("apply");
        out.data = serde_json::json!({"id": "d1"});
        out.context_patch
            .insert("last".to_string(), Value::String("d1".to_string()));
        let json = serde_json::to_string(&out).unwrap();
        let restored: StepOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.edge, "apply");
        assert!(!restored.is_terminal());
    }
}
