// action_router.rs — WorkflowPrimitive trait + ActionRouter (v0.17.0.2).
//
// Routes an `ActionEnvelope` to the first matching `WorkflowPrimitive`.
// Built-in primitives cover Apply, Deny, StartGoal, and Escalate.

use std::path::PathBuf;

use crate::agent_action::{ActionEnvelope, AgentAction};
use crate::workflow_session::AdvisorSecurity;

// ── PrimitiveError ────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum PrimitiveError {
    #[error("no primitive matched action: {0}")]
    Rejected(String),
    #[error("primitive execution failed: {0}")]
    Execution(String),
    #[error("constitution guard blocked the action: {0}")]
    ConstitutionBlocked(String),
}

// ── WorkflowContext ───────────────────────────────────────────────────────────

/// Execution context threaded through every `WorkflowPrimitive::execute` call.
#[derive(Debug, Clone)]
pub struct WorkflowContext {
    pub workspace_root: PathBuf,
    pub security: AdvisorSecurity,
    /// If true (and security = Auto), structural plan edits are permitted.
    pub allow_plan_structural_edits: bool,
}

impl WorkflowContext {
    pub fn new(workspace_root: impl Into<PathBuf>, security: AdvisorSecurity) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            security,
            allow_plan_structural_edits: false,
        }
    }
}

// ── WorkflowPrimitive ─────────────────────────────────────────────────────────

/// A primitive that can match and handle a typed `AgentAction`.
///
/// Primitives are stateless — they receive the full `WorkflowContext` on
/// each call and return the (possibly transformed) envelope on success.
pub trait WorkflowPrimitive: Send + Sync {
    /// Return true if this primitive handles the given action kind.
    fn matches(&self, action: &AgentAction) -> bool;

    /// Execute the action.  Returns the resulting envelope or a `PrimitiveError`.
    fn execute(
        &self,
        envelope: &ActionEnvelope,
        ctx: &WorkflowContext,
    ) -> Result<ActionEnvelope, PrimitiveError>;
}

// ── ActionRouter ──────────────────────────────────────────────────────────────

/// Routes an `ActionEnvelope` to the first matching `WorkflowPrimitive`.
///
/// Primitives are tried in the order they were added (index 0 = highest priority).
pub struct ActionRouter {
    primitives: Vec<Box<dyn WorkflowPrimitive>>,
}

impl ActionRouter {
    pub fn new() -> Self {
        Self {
            primitives: Vec::new(),
        }
    }

    /// Build a router pre-loaded with the default built-in primitives.
    ///
    /// Priority order (highest first):
    /// 1. `ConstitutionGuardPrimitive` — intercepts PlanMod before anything else
    /// 2. `ApplyPrimitive`
    /// 3. `DenyPrimitive`
    /// 4. `StartGoalPrimitive`
    /// 5. `EscalatePrimitive`
    pub fn with_defaults() -> Self {
        let mut router = Self::new();
        router.add(Box::new(ConstitutionGuardPrimitive));
        router.add(Box::new(ApplyPrimitive));
        router.add(Box::new(DenyPrimitive));
        router.add(Box::new(StartGoalPrimitive));
        router.add(Box::new(EscalatePrimitive));
        router
    }

    /// Add a primitive at the end of the priority list.
    pub fn add(&mut self, primitive: Box<dyn WorkflowPrimitive>) {
        self.primitives.push(primitive);
    }

    /// Route the envelope to the first matching primitive and execute it.
    pub fn route(
        &self,
        envelope: &ActionEnvelope,
        ctx: &WorkflowContext,
    ) -> Result<ActionEnvelope, PrimitiveError> {
        for primitive in &self.primitives {
            if primitive.matches(&envelope.action) {
                return primitive.execute(envelope, ctx);
            }
        }
        Err(PrimitiveError::Rejected(format!(
            "No primitive matched action kind: {}",
            envelope.action
        )))
    }
}

impl Default for ActionRouter {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ── Constitution guard ────────────────────────────────────────────────────────

/// Check whether a PlanMod edit text is constitutionally permitted.
///
/// Blocks any edit that contains `constitution_check` or `[[rules.block]]`
/// unless BOTH conditions hold:
///   - `ctx.allow_plan_structural_edits == true`
///   - `ctx.security == AdvisorSecurity::Auto`
pub fn check_plan_mod_constitution(
    edit_text: &str,
    ctx: &WorkflowContext,
) -> Result<(), PrimitiveError> {
    let targets = ["constitution_check", "[[rules.block]]", "rules.block"];
    let is_structural = targets.iter().any(|t| edit_text.contains(t));

    if is_structural {
        if ctx.allow_plan_structural_edits && ctx.security == AdvisorSecurity::Auto {
            tracing::warn!(
                "Constitution guard: allowing structural plan edit (allow_plan_structural_edits=true, security=auto)"
            );
            return Ok(());
        }
        return Err(PrimitiveError::ConstitutionBlocked(
            "PlanMod removes or weakens a constitution_check step or [[rules.block]] entry. \
             Set allow_plan_structural_edits = true in session config AND \
             advisor_security = \"auto\" to permit structural plan edits."
                .to_string(),
        ));
    }

    Ok(())
}

// ── Built-in primitives ───────────────────────────────────────────────────────

/// Intercepts `PlanMod` actions and checks constitutional limits before passing through.
struct ConstitutionGuardPrimitive;

impl WorkflowPrimitive for ConstitutionGuardPrimitive {
    fn matches(&self, action: &AgentAction) -> bool {
        matches!(action, AgentAction::PlanMod { .. })
    }

    fn execute(
        &self,
        envelope: &ActionEnvelope,
        ctx: &WorkflowContext,
    ) -> Result<ActionEnvelope, PrimitiveError> {
        if let AgentAction::PlanMod {
            edit,
            justification,
            ..
        } = &envelope.action
        {
            let edit_text = serde_json::to_string(edit).unwrap_or_default();
            check_plan_mod_constitution(&edit_text, ctx)?;
            check_plan_mod_constitution(justification, ctx)?;
        }
        Ok(envelope.clone())
    }
}

/// Acknowledges an `Apply` action (pass-through; actual apply runs in ta-changeset).
pub struct ApplyPrimitive;

impl WorkflowPrimitive for ApplyPrimitive {
    fn matches(&self, action: &AgentAction) -> bool {
        matches!(action, AgentAction::Apply { .. })
    }

    fn execute(
        &self,
        envelope: &ActionEnvelope,
        _ctx: &WorkflowContext,
    ) -> Result<ActionEnvelope, PrimitiveError> {
        tracing::info!(
            action_id = %envelope.action_id,
            agent_id = %envelope.agent_id,
            action = %envelope.action,
            "ApplyPrimitive: acknowledged"
        );
        Ok(envelope.clone())
    }
}

/// Acknowledges a `Deny` action (pass-through; actual denial runs in ta-changeset).
pub struct DenyPrimitive;

impl WorkflowPrimitive for DenyPrimitive {
    fn matches(&self, action: &AgentAction) -> bool {
        matches!(action, AgentAction::Deny { .. })
    }

    fn execute(
        &self,
        envelope: &ActionEnvelope,
        _ctx: &WorkflowContext,
    ) -> Result<ActionEnvelope, PrimitiveError> {
        tracing::info!(
            action_id = %envelope.action_id,
            agent_id = %envelope.agent_id,
            action = %envelope.action,
            "DenyPrimitive: acknowledged"
        );
        Ok(envelope.clone())
    }
}

/// Acknowledges a `StartGoal` action. Requires `AdvisorSecurity::Auto`.
pub struct StartGoalPrimitive;

impl WorkflowPrimitive for StartGoalPrimitive {
    fn matches(&self, action: &AgentAction) -> bool {
        matches!(action, AgentAction::StartGoal { .. })
    }

    fn execute(
        &self,
        envelope: &ActionEnvelope,
        ctx: &WorkflowContext,
    ) -> Result<ActionEnvelope, PrimitiveError> {
        if ctx.security != AdvisorSecurity::Auto {
            return Err(PrimitiveError::Rejected(
                "StartGoalPrimitive: launching a goal autonomously requires \
                 advisor_security = \"auto\". The human must approve this action."
                    .to_string(),
            ));
        }
        tracing::info!(
            action_id = %envelope.action_id,
            action = %envelope.action,
            "StartGoalPrimitive: acknowledged (caller must spawn the goal)"
        );
        Ok(envelope.clone())
    }
}

/// Acknowledges an `Escalate` action (pass-through; caller handles notification).
pub struct EscalatePrimitive;

impl WorkflowPrimitive for EscalatePrimitive {
    fn matches(&self, action: &AgentAction) -> bool {
        matches!(action, AgentAction::Escalate { .. })
    }

    fn execute(
        &self,
        envelope: &ActionEnvelope,
        _ctx: &WorkflowContext,
    ) -> Result<ActionEnvelope, PrimitiveError> {
        tracing::info!(
            action_id = %envelope.action_id,
            agent_id = %envelope.agent_id,
            action = %envelope.action,
            "EscalatePrimitive: acknowledged"
        );
        Ok(envelope.clone())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_action::{ActionEnvelope, AgentAction, PlanEdit, RoleRef, TeamRole};
    use crate::workflow_session::AdvisorSecurity;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn make_ctx(dir: &TempDir, security: AdvisorSecurity) -> WorkflowContext {
        WorkflowContext::new(dir.path(), security)
    }

    fn envelope(action: AgentAction) -> ActionEnvelope {
        ActionEnvelope::new("test-agent", TeamRole::reviewer(), action)
    }

    // ── Router matching ───────────────────────────────────────────────────────

    #[test]
    fn router_matches_apply() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::Apply {
            draft_id: Uuid::new_v4(),
            confidence: None,
            notes: None,
        });
        assert!(router.route(&env, &ctx).is_ok());
    }

    #[test]
    fn router_matches_deny() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::Deny {
            draft_id: Uuid::new_v4(),
            reason: "not ready".to_string(),
            rework_hint: None,
        });
        assert!(router.route(&env, &ctx).is_ok());
    }

    #[test]
    fn router_matches_escalate() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::Escalate {
            question: "Is this safe?".to_string(),
            escalate_to: RoleRef::Role(TeamRole::architect()),
        });
        assert!(router.route(&env, &ctx).is_ok());
    }

    #[test]
    fn router_no_match_returns_rejected() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::new(); // empty — no primitives
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::Continue);
        let result = router.route(&env, &ctx);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PrimitiveError::Rejected(_)));
    }

    // ── StartGoal security gate ───────────────────────────────────────────────

    #[test]
    fn start_goal_blocked_for_read_only() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::StartGoal {
            title: "Fix tests".to_string(),
            phase_id: None,
            context: None,
        });
        let result = router.route(&env, &ctx);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PrimitiveError::Rejected(_)));
    }

    #[test]
    fn start_goal_blocked_for_suggest() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::Suggest);
        let env = envelope(AgentAction::StartGoal {
            title: "Fix tests".to_string(),
            phase_id: None,
            context: None,
        });
        assert!(router.route(&env, &ctx).is_err());
    }

    #[test]
    fn start_goal_allowed_for_auto() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::Auto);
        let env = envelope(AgentAction::StartGoal {
            title: "Fix tests".to_string(),
            phase_id: None,
            context: None,
        });
        assert!(router.route(&env, &ctx).is_ok());
    }

    // ── Constitution guard ────────────────────────────────────────────────────

    #[test]
    fn constitution_guard_blocks_constitution_check_in_item() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let result =
            check_plan_mod_constitution("remove the constitution_check step from phase", &ctx);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PrimitiveError::ConstitutionBlocked(_)
        ));
    }

    #[test]
    fn constitution_guard_blocks_rules_block_removal() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let result = check_plan_mod_constitution("delete [[rules.block]] entry", &ctx);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PrimitiveError::ConstitutionBlocked(_)
        ));
    }

    #[test]
    fn constitution_guard_blocks_with_auto_but_no_flag() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_ctx(&tmp, AdvisorSecurity::Auto);
        // allow_plan_structural_edits defaults to false
        let result = check_plan_mod_constitution("remove constitution_check", &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn constitution_guard_allows_with_auto_and_flag() {
        let tmp = TempDir::new().unwrap();
        let mut ctx = make_ctx(&tmp, AdvisorSecurity::Auto);
        ctx.allow_plan_structural_edits = true;
        let result = check_plan_mod_constitution("remove constitution_check", &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn constitution_guard_allows_non_structural_edit() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let result = check_plan_mod_constitution("add new test item to phase", &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn plan_mod_with_constitution_item_blocked_via_router() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::PlanMod {
            phase_id: "v0.17.0.3".to_string(),
            edit: PlanEdit::RemoveItem {
                item: "constitution_check step".to_string(),
            },
            justification: "unnecessary overhead".to_string(),
        });
        let result = router.route(&env, &ctx);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PrimitiveError::ConstitutionBlocked(_)
        ));
    }

    #[test]
    fn plan_mod_non_structural_passes() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::PlanMod {
            phase_id: "v0.17.0.3".to_string(),
            edit: PlanEdit::AddItem {
                item: "extra test coverage".to_string(),
            },
            justification: "need more tests".to_string(),
        });
        assert!(router.route(&env, &ctx).is_ok());
    }

    // ── Router preserves envelope ─────────────────────────────────────────────

    #[test]
    fn router_returns_same_action_id_for_apply() {
        let tmp = TempDir::new().unwrap();
        let router = ActionRouter::with_defaults();
        let ctx = make_ctx(&tmp, AdvisorSecurity::ReadOnly);
        let env = envelope(AgentAction::Apply {
            draft_id: Uuid::new_v4(),
            confidence: Some(95),
            notes: None,
        });
        let original_id = env.action_id;
        let result = router.route(&env, &ctx).unwrap();
        assert_eq!(result.action_id, original_id);
    }
}
