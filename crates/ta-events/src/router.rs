// router.rs -- Event routing engine: match events to response strategies.
//
// The EventRouter loads `.ta/event-routing.yaml`, matches incoming events
// to responders, and returns RoutingDecisions. It does NOT execute actions --
// the caller (daemon or CLI) inspects the decision and acts on it.
//
// Config format:
//
// ```yaml
// defaults:
//   max_attempts: 3
//   escalate_after: 2
//   default_strategy: notify
//
// responders:
//   - event: goal_failed
//     strategy: agent
//     agent: claude-code
//     prompt: "Diagnose and fix the failure."
//     require_approval: true
//     max_attempts: 3
//     escalate_after: 2
// ```

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::EventError;
use crate::schema::EventEnvelope;

/// Events that cannot be routed to the `ignore` strategy.
const PROTECTED_EVENTS: &[&str] = &["policy_violation"];

/// Top-level routing configuration parsed from `.ta/event-routing.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub responders: Vec<Responder>,
    #[serde(default)]
    pub defaults: RoutingDefaults,
}

/// Global defaults for the routing engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDefaults {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_escalate_after")]
    pub escalate_after: u32,
    #[serde(default = "default_strategy")]
    pub default_strategy: ResponseStrategy,
}

impl Default for RoutingDefaults {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            escalate_after: default_escalate_after(),
            default_strategy: default_strategy(),
        }
    }
}

fn default_max_attempts() -> u32 {
    3
}
fn default_escalate_after() -> u32 {
    2
}
fn default_strategy() -> ResponseStrategy {
    ResponseStrategy::Notify
}

/// A single event-to-strategy binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Responder {
    /// Event type to match (e.g., "goal_failed", "draft_denied").
    pub event: String,
    /// Response strategy to use when this event fires.
    pub strategy: ResponseStrategy,
    /// Optional filter conditions beyond event type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<EventRoutingFilter>,
    /// Agent name for the `agent` strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Prompt template for the `agent` strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Whether agent output requires human approval (default: false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_approval: Option<bool>,
    /// Escalate to human notification after this many failed attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_after: Option<u32>,
    /// Maximum retry attempts before stopping (prevents infinite loops).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    /// Workflow name for the `workflow` strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Channel names for the `notify` strategy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,
}

/// Response strategy types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStrategy {
    /// Deliver event to configured channels.
    Notify,
    /// Halt the pipeline, require human intervention.
    Block,
    /// Launch an agent goal with event context injected.
    Agent,
    /// Start a named workflow with event data as input.
    Workflow,
    /// Suppress the event entirely.
    Ignore,
}

impl std::fmt::Display for ResponseStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notify => write!(f, "notify"),
            Self::Block => write!(f, "block"),
            Self::Agent => write!(f, "agent"),
            Self::Workflow => write!(f, "workflow"),
            Self::Ignore => write!(f, "ignore"),
        }
    }
}

impl std::str::FromStr for ResponseStrategy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "notify" => Ok(Self::Notify),
            "block" => Ok(Self::Block),
            "agent" => Ok(Self::Agent),
            "workflow" => Ok(Self::Workflow),
            "ignore" => Ok(Self::Ignore),
            _ => Err(format!(
                "unknown strategy '{}'; valid strategies: notify, block, agent, workflow, ignore",
                s
            )),
        }
    }
}

/// Optional filter conditions for event responders.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventRoutingFilter {
    /// Match events with this severity level (exact match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Match events in this phase (supports trailing `*` wildcard).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Match events from this agent (exact match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

impl EventRoutingFilter {
    /// Check whether an event envelope matches this filter.
    pub fn matches(&self, envelope: &EventEnvelope) -> bool {
        if let Some(phase_pattern) = &self.phase {
            let event_phase = envelope.payload.phase();
            match event_phase {
                None => return false,
                Some(p) => {
                    if !matches_glob(phase_pattern, p) {
                        return false;
                    }
                }
            }
        }

        if let Some(agent_filter) = &self.agent_id {
            let event_agent = extract_agent_id(&envelope.payload);
            match event_agent {
                None => return false,
                Some(a) => {
                    if a != agent_filter {
                        return false;
                    }
                }
            }
        }

        // severity is informational -- events don't currently carry a severity
        // field, so we skip this filter for now (always matches if not set).

        true
    }
}

/// Simple glob matching: supports trailing `*` wildcard only.
/// e.g., "v0.9.*" matches "v0.9.1", "v0.9.10", but not "v0.10.0".
fn matches_glob(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        pattern == value
    }
}

/// Extract agent_id from event payloads that carry one.
fn extract_agent_id(event: &crate::schema::SessionEvent) -> Option<&str> {
    match event {
        crate::schema::SessionEvent::GoalStarted { agent_id, .. } => Some(agent_id),
        crate::schema::SessionEvent::PolicyViolation { agent_id, .. } => Some(agent_id),
        _ => None,
    }
}

/// The routing decision returned by `EventRouter::route()`.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    /// The event type that was matched.
    pub event_type: String,
    /// The response strategy to execute.
    pub strategy: ResponseStrategy,
    /// Agent name (for `Agent` strategy).
    pub agent: Option<String>,
    /// Prompt template (for `Agent` strategy).
    pub prompt: Option<String>,
    /// Whether agent output requires human approval.
    pub require_approval: bool,
    /// Workflow name (for `Workflow` strategy).
    pub workflow: Option<String>,
    /// Channels to notify (for `Notify` strategy).
    pub channels: Vec<String>,
    /// Current attempt number (starts at 0).
    pub attempt_number: u32,
    /// Maximum attempts allowed.
    pub max_attempts: u32,
    /// Whether this event has been escalated (attempts > escalate_after).
    pub escalated: bool,
}

/// Key for tracking attempt counts per event type and optional goal.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct AttemptKey {
    event_type: String,
    goal_id: Option<Uuid>,
}

/// The event routing engine.
///
/// Loads configuration from `.ta/event-routing.yaml`, matches incoming events
/// to responders, tracks attempt counts, and returns routing decisions.
pub struct EventRouter {
    config: RoutingConfig,
    #[allow(dead_code)]
    attempts: HashMap<AttemptKey, u32>,
}

impl std::fmt::Debug for EventRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventRouter")
            .field("responders", &self.config.responders.len())
            .field("attempts_tracked", &self.attempts.len())
            .finish()
    }
}

impl EventRouter {
    /// Create a new router from a validated config.
    pub fn new(config: RoutingConfig) -> Result<Self, EventError> {
        Self::validate_config(&config)?;
        Ok(Self {
            config,
            attempts: HashMap::new(),
        })
    }

    /// Load routing config from a YAML file.
    pub fn from_file(path: &Path) -> Result<Self, EventError> {
        let content = std::fs::read_to_string(path).map_err(|e| EventError::RoutingParse {
            path: path.display().to_string(),
            detail: format!("failed to read file: {}", e),
        })?;
        let config: RoutingConfig =
            serde_yaml::from_str(&content).map_err(|e| EventError::RoutingParse {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;
        Self::new(config)
    }

    /// Load routing config from a project's `.ta/event-routing.yaml`.
    /// Falls back to defaults if the file does not exist.
    pub fn from_project(project_root: &Path) -> Result<Self, EventError> {
        let path = project_root.join(".ta").join("event-routing.yaml");
        if path.exists() {
            Self::from_file(&path)
        } else {
            Self::new(RoutingConfig::default())
        }
    }

    /// Validate config: protected events cannot use certain strategies.
    fn validate_config(config: &RoutingConfig) -> Result<(), EventError> {
        for responder in &config.responders {
            if PROTECTED_EVENTS.contains(&responder.event.as_str())
                && responder.strategy == ResponseStrategy::Ignore
            {
                return Err(EventError::ProtectedEvent {
                    event_type: responder.event.clone(),
                    strategy: "ignore".into(),
                    reason:
                        "protected events must always be handled; use 'block' or 'notify' instead"
                            .into(),
                });
            }
        }
        Ok(())
    }

    /// Route an event envelope to a response strategy.
    ///
    /// Finds the first matching responder (by event type + optional filter),
    /// applies attempt tracking and escalation logic, and returns the decision.
    pub fn route(&self, envelope: &EventEnvelope) -> RoutingDecision {
        let event_type = &envelope.event_type;
        let goal_id = envelope.payload.goal_id();

        // Find the first matching responder.
        let matched = self.config.responders.iter().find(|r| {
            if r.event != *event_type {
                return false;
            }
            if let Some(filter) = &r.filter {
                if !filter.matches(envelope) {
                    return false;
                }
            }
            true
        });

        let (
            strategy,
            agent,
            prompt,
            require_approval,
            workflow,
            channels,
            max_attempts,
            escalate_after,
        ) = match matched {
            Some(r) => (
                r.strategy.clone(),
                r.agent.clone(),
                r.prompt.clone(),
                r.require_approval.unwrap_or(false),
                r.workflow.clone(),
                r.channels.clone(),
                r.max_attempts.unwrap_or(self.config.defaults.max_attempts),
                r.escalate_after
                    .unwrap_or(self.config.defaults.escalate_after),
            ),
            None => (
                self.config.defaults.default_strategy.clone(),
                None,
                None,
                false,
                None,
                vec![],
                self.config.defaults.max_attempts,
                self.config.defaults.escalate_after,
            ),
        };

        let attempt_key = AttemptKey {
            event_type: event_type.clone(),
            goal_id,
        };
        let attempt_number = self.attempts.get(&attempt_key).copied().unwrap_or(0);
        let escalated = attempt_number >= escalate_after;

        // Guardrail: if agent strategy has exceeded max_attempts, override to notify.
        let final_strategy =
            if strategy == ResponseStrategy::Agent && attempt_number >= max_attempts {
                tracing::warn!(
                    event_type = %event_type,
                    attempts = attempt_number,
                    max = max_attempts,
                    "agent strategy exceeded max_attempts, escalating to notify"
                );
                ResponseStrategy::Notify
            } else {
                strategy
            };

        RoutingDecision {
            event_type: event_type.clone(),
            strategy: final_strategy,
            agent,
            prompt,
            require_approval,
            workflow,
            channels,
            attempt_number,
            max_attempts,
            escalated,
        }
    }

    /// Record an attempt for the given event type and optional goal.
    pub fn record_attempt(&mut self, event_type: &str, goal_id: Option<Uuid>) {
        let key = AttemptKey {
            event_type: event_type.to_string(),
            goal_id,
        };
        let count = self.attempts.entry(key).or_insert(0);
        *count += 1;
    }

    /// Get the current attempt count for an event type and optional goal.
    pub fn attempt_count(&self, event_type: &str, goal_id: Option<Uuid>) -> u32 {
        let key = AttemptKey {
            event_type: event_type.to_string(),
            goal_id,
        };
        self.attempts.get(&key).copied().unwrap_or(0)
    }

    /// Reset attempt tracking for an event type and optional goal.
    pub fn reset_attempts(&mut self, event_type: &str, goal_id: Option<Uuid>) {
        let key = AttemptKey {
            event_type: event_type.to_string(),
            goal_id,
        };
        self.attempts.remove(&key);
    }

    /// Get the list of configured responders.
    pub fn responders(&self) -> &[Responder] {
        &self.config.responders
    }

    /// Get the routing defaults.
    pub fn defaults(&self) -> &RoutingDefaults {
        &self.config.defaults
    }

    /// Get the full routing config (for serialization back to YAML).
    pub fn config(&self) -> &RoutingConfig {
        &self.config
    }

    /// Dry-run routing for a given event type. Creates a synthetic envelope
    /// and routes it.
    pub fn test_route(&self, event_type: &str) -> RoutingDecision {
        let synthetic = crate::schema::EventEnvelope {
            id: Uuid::nil(),
            timestamp: chrono::Utc::now(),
            version: crate::schema::SCHEMA_VERSION,
            event_type: event_type.to_string(),
            payload: crate::schema::SessionEvent::MemoryStored {
                key: format!("__routing_test_{}", event_type),
                category: Some("routing_test".into()),
                source: "router".into(),
            },
            actions: vec![],
        };
        self.route(&synthetic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SessionEvent;

    fn make_config(responders: Vec<Responder>) -> RoutingConfig {
        RoutingConfig {
            responders,
            defaults: RoutingDefaults::default(),
        }
    }

    #[test]
    fn load_empty_config_uses_defaults() {
        let config = RoutingConfig::default();
        let router = EventRouter::new(config).unwrap();
        assert!(router.responders().is_empty());
        assert_eq!(router.defaults().default_strategy, ResponseStrategy::Notify);
        assert_eq!(router.defaults().max_attempts, 3);
        assert_eq!(router.defaults().escalate_after, 2);
    }

    #[test]
    fn route_exact_match() {
        let config = make_config(vec![Responder {
            event: "goal_failed".into(),
            strategy: ResponseStrategy::Agent,
            filter: None,
            agent: Some("claude-code".into()),
            prompt: Some("Fix it.".into()),
            require_approval: Some(true),
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec![],
        }]);
        let router = EventRouter::new(config).unwrap();

        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "build error".into(),
            exit_code: Some(1),
        });
        let decision = router.route(&envelope);

        assert_eq!(decision.strategy, ResponseStrategy::Agent);
        assert_eq!(decision.agent.as_deref(), Some("claude-code"));
        assert_eq!(decision.prompt.as_deref(), Some("Fix it."));
        assert!(decision.require_approval);
    }

    #[test]
    fn route_no_match_uses_default() {
        let config = make_config(vec![Responder {
            event: "goal_failed".into(),
            strategy: ResponseStrategy::Agent,
            filter: None,
            agent: None,
            prompt: None,
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec![],
        }]);
        let router = EventRouter::new(config).unwrap();

        let envelope = EventEnvelope::new(SessionEvent::MemoryStored {
            key: "k".into(),
            category: None,
            source: "cli".into(),
        });
        let decision = router.route(&envelope);

        assert_eq!(decision.strategy, ResponseStrategy::Notify);
    }

    #[test]
    fn route_with_phase_filter_match() {
        let config = make_config(vec![Responder {
            event: "goal_started".into(),
            strategy: ResponseStrategy::Block,
            filter: Some(EventRoutingFilter {
                phase: Some("v0.9.*".into()),
                ..Default::default()
            }),
            agent: None,
            prompt: None,
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec![],
        }]);
        let router = EventRouter::new(config).unwrap();

        // Matching phase.
        let envelope = EventEnvelope::new(SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "test".into(),
            agent_id: "a".into(),
            phase: Some("v0.9.5".into()),
        });
        assert_eq!(router.route(&envelope).strategy, ResponseStrategy::Block);

        // Non-matching phase.
        let envelope2 = EventEnvelope::new(SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "test".into(),
            agent_id: "a".into(),
            phase: Some("v0.10.0".into()),
        });
        assert_eq!(router.route(&envelope2).strategy, ResponseStrategy::Notify);
    }

    #[test]
    fn route_with_agent_id_filter() {
        let config = make_config(vec![Responder {
            event: "goal_started".into(),
            strategy: ResponseStrategy::Block,
            filter: Some(EventRoutingFilter {
                agent_id: Some("codex".into()),
                ..Default::default()
            }),
            agent: None,
            prompt: None,
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec![],
        }]);
        let router = EventRouter::new(config).unwrap();

        let envelope = EventEnvelope::new(SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "test".into(),
            agent_id: "codex".into(),
            phase: None,
        });
        assert_eq!(router.route(&envelope).strategy, ResponseStrategy::Block);

        let envelope2 = EventEnvelope::new(SessionEvent::GoalStarted {
            goal_id: Uuid::new_v4(),
            title: "test".into(),
            agent_id: "claude-code".into(),
            phase: None,
        });
        assert_eq!(router.route(&envelope2).strategy, ResponseStrategy::Notify);
    }

    #[test]
    fn route_workflow_strategy_fields() {
        let config = make_config(vec![Responder {
            event: "draft_denied".into(),
            strategy: ResponseStrategy::Workflow,
            filter: None,
            agent: None,
            prompt: None,
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: Some("retry-pipeline".into()),
            channels: vec![],
        }]);
        let router = EventRouter::new(config).unwrap();

        let envelope = EventEnvelope::new(SessionEvent::DraftDenied {
            goal_id: Uuid::new_v4(),
            draft_id: Uuid::new_v4(),
            reason: "test fail".into(),
            denied_by: "human".into(),
        });
        let decision = router.route(&envelope);

        assert_eq!(decision.strategy, ResponseStrategy::Workflow);
        assert_eq!(decision.workflow.as_deref(), Some("retry-pipeline"));
    }

    #[test]
    fn attempt_tracking() {
        let config = RoutingConfig::default();
        let mut router = EventRouter::new(config).unwrap();

        assert_eq!(router.attempt_count("goal_failed", None), 0);

        router.record_attempt("goal_failed", None);
        assert_eq!(router.attempt_count("goal_failed", None), 1);

        router.record_attempt("goal_failed", None);
        assert_eq!(router.attempt_count("goal_failed", None), 2);

        router.reset_attempts("goal_failed", None);
        assert_eq!(router.attempt_count("goal_failed", None), 0);
    }

    #[test]
    fn attempt_tracking_per_goal() {
        let config = RoutingConfig::default();
        let mut router = EventRouter::new(config).unwrap();

        let g1 = Uuid::new_v4();
        let g2 = Uuid::new_v4();

        router.record_attempt("goal_failed", Some(g1));
        router.record_attempt("goal_failed", Some(g1));
        router.record_attempt("goal_failed", Some(g2));

        assert_eq!(router.attempt_count("goal_failed", Some(g1)), 2);
        assert_eq!(router.attempt_count("goal_failed", Some(g2)), 1);
    }

    #[test]
    fn escalation_after_threshold() {
        let config = make_config(vec![Responder {
            event: "goal_failed".into(),
            strategy: ResponseStrategy::Agent,
            filter: None,
            agent: Some("claude-code".into()),
            prompt: None,
            require_approval: None,
            escalate_after: Some(1),
            max_attempts: Some(5),
            workflow: None,
            channels: vec![],
        }]);
        let mut router = EventRouter::new(config).unwrap();

        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "err".into(),
            exit_code: Some(1),
        });

        // Before any attempts.
        let d1 = router.route(&envelope);
        assert!(!d1.escalated);

        // After 1 attempt (>= escalate_after=1).
        router.record_attempt("goal_failed", envelope.payload.goal_id());
        let d2 = router.route(&envelope);
        assert!(d2.escalated);
        assert_eq!(d2.strategy, ResponseStrategy::Agent); // still agent, just escalated
    }

    #[test]
    fn max_attempts_override_to_notify() {
        let config = make_config(vec![Responder {
            event: "goal_failed".into(),
            strategy: ResponseStrategy::Agent,
            filter: None,
            agent: Some("claude-code".into()),
            prompt: None,
            require_approval: None,
            escalate_after: Some(1),
            max_attempts: Some(2),
            workflow: None,
            channels: vec![],
        }]);
        let mut router = EventRouter::new(config).unwrap();

        let goal_id = Uuid::new_v4();
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id,
            error: "err".into(),
            exit_code: Some(1),
        });

        router.record_attempt("goal_failed", Some(goal_id));
        router.record_attempt("goal_failed", Some(goal_id));

        let decision = router.route(&envelope);
        // At max_attempts=2, attempt_number=2 >= max_attempts → overridden to Notify.
        assert_eq!(decision.strategy, ResponseStrategy::Notify);
        assert!(decision.escalated);
    }

    #[test]
    fn protected_event_rejects_ignore() {
        let config = make_config(vec![Responder {
            event: "policy_violation".into(),
            strategy: ResponseStrategy::Ignore,
            filter: None,
            agent: None,
            prompt: None,
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec![],
        }]);
        let result = EventRouter::new(config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("policy_violation"));
        assert!(err.contains("ignore"));
    }

    #[test]
    fn config_yaml_round_trip() {
        let config = make_config(vec![
            Responder {
                event: "goal_failed".into(),
                strategy: ResponseStrategy::Agent,
                filter: None,
                agent: Some("claude-code".into()),
                prompt: Some("Fix the build.".into()),
                require_approval: Some(true),
                escalate_after: Some(2),
                max_attempts: Some(3),
                workflow: None,
                channels: vec![],
            },
            Responder {
                event: "policy_violation".into(),
                strategy: ResponseStrategy::Block,
                filter: None,
                agent: None,
                prompt: None,
                require_approval: None,
                escalate_after: None,
                max_attempts: None,
                workflow: None,
                channels: vec![],
            },
        ]);
        let yaml = serde_yaml::to_string(&config).unwrap();
        let restored: RoutingConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(restored.responders.len(), 2);
        assert_eq!(restored.responders[0].event, "goal_failed");
        assert_eq!(restored.responders[0].strategy, ResponseStrategy::Agent);
        assert_eq!(restored.responders[1].strategy, ResponseStrategy::Block);
    }

    #[test]
    fn test_route_dry_run() {
        let config = make_config(vec![Responder {
            event: "goal_failed".into(),
            strategy: ResponseStrategy::Agent,
            filter: None,
            agent: Some("claude-code".into()),
            prompt: Some("Fix it.".into()),
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec![],
        }]);
        let router = EventRouter::new(config).unwrap();

        let decision = router.test_route("goal_failed");
        assert_eq!(decision.strategy, ResponseStrategy::Agent);
        assert_eq!(decision.agent.as_deref(), Some("claude-code"));

        // Unmatched event type.
        let decision2 = router.test_route("memory_stored");
        assert_eq!(decision2.strategy, ResponseStrategy::Notify);
    }

    #[test]
    fn from_file_missing_gracefully() {
        let router = EventRouter::from_project(Path::new("/nonexistent/path"));
        assert!(router.is_ok());
        assert!(router.unwrap().responders().is_empty());
    }

    #[test]
    fn response_strategy_display() {
        assert_eq!(ResponseStrategy::Notify.to_string(), "notify");
        assert_eq!(ResponseStrategy::Block.to_string(), "block");
        assert_eq!(ResponseStrategy::Agent.to_string(), "agent");
        assert_eq!(ResponseStrategy::Workflow.to_string(), "workflow");
        assert_eq!(ResponseStrategy::Ignore.to_string(), "ignore");
    }

    #[test]
    fn response_strategy_from_str() {
        assert_eq!(
            "notify".parse::<ResponseStrategy>().unwrap(),
            ResponseStrategy::Notify
        );
        assert_eq!(
            "agent".parse::<ResponseStrategy>().unwrap(),
            ResponseStrategy::Agent
        );
        assert!("invalid".parse::<ResponseStrategy>().is_err());
    }

    #[test]
    fn glob_matching() {
        assert!(matches_glob("v0.9.*", "v0.9.1"));
        assert!(matches_glob("v0.9.*", "v0.9.10"));
        assert!(!matches_glob("v0.9.*", "v0.10.0"));
        assert!(matches_glob("v0.9.5", "v0.9.5"));
        assert!(!matches_glob("v0.9.5", "v0.9.6"));
    }

    #[test]
    fn filter_no_phase_event_doesnt_match_phase_filter() {
        let filter = EventRoutingFilter {
            phase: Some("v0.9.*".into()),
            ..Default::default()
        };
        let envelope = EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "err".into(),
            exit_code: Some(1),
        });
        assert!(!filter.matches(&envelope));
    }

    #[test]
    fn notify_strategy_channels() {
        let config = make_config(vec![Responder {
            event: "draft_approved".into(),
            strategy: ResponseStrategy::Notify,
            filter: None,
            agent: None,
            prompt: None,
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec!["slack".into(), "email".into()],
        }]);
        let router = EventRouter::new(config).unwrap();

        let envelope = EventEnvelope::new(SessionEvent::DraftApproved {
            goal_id: Uuid::new_v4(),
            draft_id: Uuid::new_v4(),
            approved_by: "human".into(),
        });
        let decision = router.route(&envelope);
        assert_eq!(decision.channels, vec!["slack", "email"]);
    }
}
