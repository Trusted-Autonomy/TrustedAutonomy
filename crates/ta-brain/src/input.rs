//! [`RoutingInput`] — the two shapes a request into `ta-brain` can take:
//! an explicit `ta run` invocation, or a fired `ta_intake::TriggerEvent`.
//! `route()` normalizes both onto the same tier-resolution path so neither
//! entry point needs its own routing logic (§13.1).

use ta_intake::TriggerEvent;

/// A request into `route()`. Either an explicit `ta run` invocation or a
/// triggered event from `ta-intake` — both resolve through identical tiers.
#[derive(Debug, Clone)]
pub enum RoutingInput {
    ExplicitGoal(ExplicitGoalRequest),
    Trigger(TriggerRoutingInput),
}

/// The explicit-path request shape: everything a human or the MCP
/// orchestrator can pass via `ta run` CLI flags.
#[derive(Debug, Clone, Default)]
pub struct ExplicitGoalRequest {
    pub goal_title: String,
    #[allow(dead_code)]
    pub objective: String,
    /// `--agent`
    pub cli_agent: Option<String>,
    /// `--persona`
    pub cli_persona: Option<String>,
    /// `--team` (new, v0.17.0.12.20)
    pub cli_team: Option<String>,
    /// `--security` (new, v0.17.0.12.20)
    pub cli_security: Option<String>,
    /// `--priority` (new, v0.17.0.12.20)
    pub cli_priority: Option<String>,
    /// `--workflow`
    pub workflow_name_or_path: Option<String>,
    /// `--workload` — explicit workload-type override. When unset, `route()`
    /// classifies the workload type from `goal_title` (v0.17.0.12.20).
    pub workload_type_override: Option<String>,
}

impl ExplicitGoalRequest {
    pub fn new(goal_title: impl Into<String>) -> Self {
        Self {
            goal_title: goal_title.into(),
            ..Default::default()
        }
    }

    /// Text used for workload classification: title + objective, since
    /// either may carry the signal (a terse title, a detailed objective).
    pub(crate) fn classification_text(&self) -> String {
        format!("{} {}", self.goal_title, self.objective)
    }
}

/// The triggered-path request shape: a normalized `TriggerEvent` plus
/// whatever routing hints its `.ta/triggers/<type>.toml` config's `settings`
/// table carries (`team`, `persona`, `security`, `priority`, `workload`) —
/// deliberately read the same untyped way `TriggerManifest::get_str` already
/// reads other per-type settings, so a community trigger type can supply
/// routing hints without any `ta-brain` code change.
#[derive(Debug, Clone)]
pub struct TriggerRoutingInput {
    pub event: TriggerEvent,
    pub team_hint: Option<String>,
    pub persona_hint: Option<String>,
    pub security_hint: Option<String>,
    pub priority_hint: Option<String>,
    pub workload_hint: Option<String>,
}

impl TriggerRoutingInput {
    /// Build routing input directly from a fired event, with no manifest
    /// hints (the common case — see `from_event_and_manifest` when the
    /// manifest's `[settings]` table is available).
    pub fn from_event(event: TriggerEvent) -> Self {
        Self {
            event,
            team_hint: None,
            persona_hint: None,
            security_hint: None,
            priority_hint: None,
            workload_hint: None,
        }
    }

    /// Build routing input from a fired event plus its trigger type's
    /// manifest, pulling `team`/`persona`/`security`/`priority`/`workload`
    /// out of `[settings]` when present.
    pub fn from_event_and_manifest(
        event: TriggerEvent,
        manifest: &ta_intake::TriggerManifest,
    ) -> Self {
        Self {
            team_hint: manifest.get_str("team").map(str::to_string),
            persona_hint: manifest.get_str("persona").map(str::to_string),
            security_hint: manifest.get_str("security").map(str::to_string),
            priority_hint: manifest.get_str("priority").map(str::to_string),
            workload_hint: manifest.get_str("workload").map(str::to_string),
            event,
        }
    }

    pub(crate) fn classification_text(&self) -> String {
        format!("{} {}", self.event.suggested_goal_title, self.event.payload)
    }
}
