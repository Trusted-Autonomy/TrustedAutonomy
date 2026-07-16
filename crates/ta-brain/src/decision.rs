//! [`RoutingDecision`] — the output of `route()`: which team role, persona,
//! agent, security tier, and priority a request resolved to, plus the
//! workload classification and a human-readable rationale trail (Observable
//! & Actionable — a routing decision, especially an `"auto"` one, must never
//! be a black box).

use serde::{Deserialize, Serialize};
use ta_session::agent_action::TeamRole;
use ta_session::workflow_session::AdvisorSecurity;

use crate::priority::Priority;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoutingDecision {
    /// Which team role handles this request (§3's "Role" node).
    pub team: TeamRole,
    /// Resolved persona name, if any tier bound one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Resolved agent/framework ID.
    pub agent: String,
    /// Resolved security tier — reuses the existing `AdvisorSecurity`
    /// tri-state (read_only/suggest/auto) rather than a new trust-level
    /// concept, per `docs/design/ta-concepts-and-architecture.md` §3.
    pub security_tier: AdvisorSecurity,
    /// Resolved priority relative to other pending requests.
    pub priority: Priority,
    /// The workload type this request was classified as (e.g. "bugfix",
    /// "docs", "security") — either explicit or inferred.
    pub workload_type: String,
    /// Confidence in `workload_type` when it was inferred (1.0 when
    /// explicit). Gates the `security_tier = "auto"` tier — see `route()`.
    pub workload_confidence: f32,
    /// Workflow template `ta-workflow::intent::resolve_intent` matched
    /// against the request text at or above its own confidence threshold,
    /// when no `workflow_name_or_path` was given explicitly (v0.17.0.12.23).
    /// Folds workflow-template matching into `route()` as one signal among
    /// several, rather than a second, parallel intent system a caller would
    /// need to consult separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_workflow_template: Option<String>,
    /// One line per resolution step, most-specific-tier-first, e.g.
    /// `"agent: tier=persona value=claude-opus-4-8"`. Always populated,
    /// always surfaced to a human (Observable & Actionable).
    pub rationale: Vec<String>,
}
