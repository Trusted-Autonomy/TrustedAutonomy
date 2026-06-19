// step_action.rs — StepAction and ActionRouter (v0.17.0.4.5).
//
// Actions are typed operations a step can invoke on entry, on transition, or on exit.
// External actions are brokered by TA — step YAML never carries credentials.
// TA resolves the adapter, checks constitution guard, injects auth, returns result.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ── Known TA action primitives ────────────────────────────────────────────────

/// TA-primitive action names (the part after "ta.").
///
/// These are known statically and dispatched by the TA daemon without external
/// credentials. The validator uses this list to detect typos at parse time.
pub const KNOWN_TA_ACTIONS: &[&str] = &[
    "apply_draft",
    "deny_draft",
    "start_goal",
    "plan_mod",
    "escalate",
    "merge_pr",
    "run_build",
    "notify",
];

// ── StepAction ────────────────────────────────────────────────────────────────

/// An action to dispatch during a step transition.
///
/// The `action_type` field is a dotted identifier:
///   - `ta.apply_draft`   — TA primitive: apply a draft
///   - `ta.notify`        — TA primitive: send a notification
///   - `external.send_email` — brokered external action
///   - `set_context`      — modify workflow context fields
///
/// `params` may contain template strings (e.g., `"{{trigger.data.draft_id}}"`)
/// that are resolved before dispatch.
///
/// YAML example:
/// ```yaml
/// - action_type: ta.apply_draft
///   params:
///     draft_id: "{{trigger.data.draft_id}}"
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepAction {
    pub action_type: String,
    #[serde(default)]
    pub params: Map<String, Value>,
}

impl StepAction {
    pub fn new(action_type: impl Into<String>) -> Self {
        Self {
            action_type: action_type.into(),
            params: Map::new(),
        }
    }

    pub fn with_params(action_type: impl Into<String>, params: Map<String, Value>) -> Self {
        Self {
            action_type: action_type.into(),
            params,
        }
    }

    /// True when this is a TA-primitive action (`ta.*`).
    pub fn is_ta(&self) -> bool {
        self.action_type.starts_with("ta.")
    }

    /// True when this is an externally-brokered action (`external.*`).
    pub fn is_external(&self) -> bool {
        self.action_type.starts_with("external.")
    }

    /// True when this action modifies the workflow context.
    pub fn is_set_context(&self) -> bool {
        self.action_type == "set_context"
    }

    /// Returns the TA primitive name (the part after `ta.`), or `None`.
    pub fn ta_primitive(&self) -> Option<&str> {
        self.action_type.strip_prefix("ta.")
    }

    /// Returns the external adapter name (the part after `external.`), or `None`.
    pub fn external_adapter(&self) -> Option<&str> {
        self.action_type.strip_prefix("external.")
    }
}

// ── ActionRouter ──────────────────────────────────────────────────────────────

/// Routes dispatched actions to their handlers.
///
/// In production this is backed by the TA daemon (for `ta.*` primitives) or the
/// adapter registry (for `external.*` actions). In tests, use
/// `CollectingActionRouter` or `NoopActionRouter`.
pub trait ActionRouter {
    /// Dispatch a single action. Returns the result value (may be `Null` for
    /// fire-and-forget actions) or an error string.
    fn dispatch(&mut self, action: &StepAction) -> Result<Value, String>;
}

// ── NoopActionRouter ─────────────────────────────────────────────────────────

/// No-op router that silently accepts all actions and returns `Null`.
///
/// Use in tests or stubs where action effects are irrelevant.
pub struct NoopActionRouter;

impl ActionRouter for NoopActionRouter {
    fn dispatch(&mut self, _action: &StepAction) -> Result<Value, String> {
        Ok(Value::Null)
    }
}

// ── CollectingActionRouter ────────────────────────────────────────────────────

/// Records all dispatched actions for inspection in tests.
pub struct CollectingActionRouter {
    pub dispatched: Vec<StepAction>,
}

impl CollectingActionRouter {
    pub fn new() -> Self {
        Self {
            dispatched: Vec::new(),
        }
    }
}

impl Default for CollectingActionRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionRouter for CollectingActionRouter {
    fn dispatch(&mut self, action: &StepAction) -> Result<Value, String> {
        self.dispatched.push(action.clone());
        Ok(Value::Null)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_action_is_ta() {
        let a = StepAction::new("ta.apply_draft");
        assert!(a.is_ta());
        assert!(!a.is_external());
        assert!(!a.is_set_context());
    }

    #[test]
    fn step_action_is_external() {
        let a = StepAction::new("external.send_email");
        assert!(a.is_external());
        assert!(!a.is_ta());
        assert!(!a.is_set_context());
    }

    #[test]
    fn step_action_is_set_context() {
        let a = StepAction::new("set_context");
        assert!(a.is_set_context());
        assert!(!a.is_ta());
        assert!(!a.is_external());
    }

    #[test]
    fn ta_primitive_extraction() {
        let a = StepAction::new("ta.apply_draft");
        assert_eq!(a.ta_primitive(), Some("apply_draft"));

        let b = StepAction::new("external.send_email");
        assert_eq!(b.ta_primitive(), None);
        assert_eq!(b.external_adapter(), Some("send_email"));
    }

    #[test]
    fn step_action_serde_round_trip() {
        let mut params = Map::new();
        params.insert(
            "draft_id".to_string(),
            Value::String("{{trigger.data.draft_id}}".to_string()),
        );
        let action = StepAction::with_params("ta.apply_draft", params);
        let json = serde_json::to_string(&action).unwrap();
        let restored: StepAction = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.action_type, "ta.apply_draft");
        assert_eq!(
            restored.params["draft_id"],
            Value::String("{{trigger.data.draft_id}}".to_string())
        );
    }

    #[test]
    fn collecting_router_captures_dispatched_actions() {
        let mut router = CollectingActionRouter::new();
        let a1 = StepAction::new("ta.notify");
        let a2 = StepAction::new("external.send_email");
        router.dispatch(&a1).unwrap();
        router.dispatch(&a2).unwrap();
        assert_eq!(router.dispatched.len(), 2);
        assert_eq!(router.dispatched[0].action_type, "ta.notify");
        assert_eq!(router.dispatched[1].action_type, "external.send_email");
    }

    #[test]
    fn noop_router_accepts_all_actions() {
        let mut router = NoopActionRouter;
        let action = StepAction::new("ta.escalate");
        assert!(router.dispatch(&action).is_ok());
    }

    #[test]
    fn known_ta_actions_contains_expected() {
        assert!(KNOWN_TA_ACTIONS.contains(&"apply_draft"));
        assert!(KNOWN_TA_ACTIONS.contains(&"notify"));
        assert!(KNOWN_TA_ACTIONS.contains(&"escalate"));
        assert!(!KNOWN_TA_ACTIONS.contains(&"unknown_action"));
    }
}
