// notification.rs — Notification Rules Engine for event-driven delivery.
//
// Rules are loaded from `.ta/notification-rules.toml` and evaluated against
// incoming EventEnvelopes. Each rule carries:
//   - Conditions: event type match, severity threshold, time window
//   - Channels: which delivery adapters to use
//   - Template: title/body string with {placeholder} substitution
//   - Rate limit: max deliveries per time window
//
// The engine is stateful (tracks delivery timestamps for dedup/rate limiting)
// and is designed to be shared behind an Arc in the daemon.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::schema::EventEnvelope;

// ─── Severity ────────────────────────────────────────────────────────────────

/// Notification severity level.
///
/// Used both in rules (`SeverityGte`) and in the notification payload
/// (`ChannelNotification.severity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    #[default]
    Info,
    Warning,
    Error,
    Critical,
}

impl NotificationSeverity {
    /// Derive severity from a known event type string.
    pub fn for_event_type(event_type: &str) -> Self {
        match event_type {
            "policy_violation" => Self::Critical,
            "goal_failed" | "build_failed" | "sync_conflict" | "api_connection_lost" => Self::Error,
            "agent_needs_input"
            | "question_stale"
            | "draft_denied"
            | "graph_decision_escalated" => Self::Warning,
            _ => Self::Info,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }
}

// ─── RuleCondition ───────────────────────────────────────────────────────────

/// A single matching condition within a `NotificationRule`.
///
/// All conditions in a rule must pass for the rule to fire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleCondition {
    /// Match a specific event type string (e.g., `"goal_failed"`).
    EventType { value: String },

    /// Match any event type in the provided list.
    EventTypeIn { values: Vec<String> },

    /// Match if the event's derived severity is >= the threshold.
    SeverityGte { level: NotificationSeverity },

    /// Match only during a local-time hour window (0–23, inclusive).
    ///
    /// Handles overnight ranges: `start_hour: 22, end_hour: 6` means
    /// "between 10 PM and 6 AM".
    TimeWindow { start_hour: u8, end_hour: u8 },

    /// Match if the serialised payload JSON has a field equal to a string value.
    PayloadField { field: String, value: String },
}

impl RuleCondition {
    pub fn matches(&self, event: &EventEnvelope) -> bool {
        match self {
            Self::EventType { value } => &event.event_type == value,

            Self::EventTypeIn { values } => values.iter().any(|v| v == &event.event_type),

            Self::SeverityGte { level } => {
                NotificationSeverity::for_event_type(&event.event_type) >= *level
            }

            Self::TimeWindow {
                start_hour,
                end_hour,
            } => {
                use chrono::Timelike;
                let hour = chrono::Local::now().hour() as u8;
                if start_hour <= end_hour {
                    hour >= *start_hour && hour <= *end_hour
                } else {
                    // Wraps midnight.
                    hour >= *start_hour || hour <= *end_hour
                }
            }

            Self::PayloadField { field, value } => {
                if let Ok(json) = serde_json::to_value(&event.payload) {
                    json.get(field)
                        .and_then(|v| v.as_str())
                        .map(|s| s == value.as_str())
                        .unwrap_or(false)
                } else {
                    false
                }
            }
        }
    }
}

// ─── Template ────────────────────────────────────────────────────────────────

/// Message template for rendering notification title and body.
///
/// Supports `{event_type}`, `{event_id}`, `{timestamp}`, `{goal_id}`,
/// `{title}`, `{agent_id}`, `{phase}`, `{error}` placeholders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationTemplate {
    pub title: String,
    pub body: String,
}

impl Default for NotificationTemplate {
    fn default() -> Self {
        Self {
            title: "[TA] {event_type}".into(),
            body: "Event `{event_type}` fired at {timestamp}.".into(),
        }
    }
}

impl NotificationTemplate {
    pub fn render_title(&self, vars: &HashMap<String, String>) -> String {
        render_template(&self.title, vars)
    }

    pub fn render_body(&self, vars: &HashMap<String, String>) -> String {
        render_template(&self.body, vars)
    }
}

fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("{{{}}}", key), value);
    }
    result
}

// ─── RateLimit ───────────────────────────────────────────────────────────────

/// Rate limiting configuration for a notification rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimit {
    /// Maximum notifications allowed within `period_secs`.
    pub max_per_period: u32,
    /// Length of the rate-limit window in seconds.
    pub period_secs: u64,
}

// ─── NotificationRule ────────────────────────────────────────────────────────

/// A single notification rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRule {
    /// Unique identifier used for dedup tracking.
    pub id: String,
    /// Human-readable name shown in logs.
    pub name: String,

    /// Whether this rule is active.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Evaluation order; lower numbers are evaluated first.
    #[serde(default = "default_priority")]
    pub priority: u32,

    /// All conditions must match for the rule to fire.
    /// An empty list matches every event.
    #[serde(default)]
    pub conditions: Vec<RuleCondition>,

    /// Channel names to deliver to. Empty means use `global_channels`.
    #[serde(default)]
    pub channels: Vec<String>,

    /// Optional message template. Falls back to a default template when absent.
    #[serde(default)]
    pub template: Option<NotificationTemplate>,

    /// Optional per-rule rate limiting.
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
}

fn default_true() -> bool {
    true
}
fn default_priority() -> u32 {
    100
}

impl NotificationRule {
    /// Returns true if the rule is enabled and all conditions match the event.
    pub fn matches(&self, event: &EventEnvelope) -> bool {
        if !self.enabled {
            return false;
        }
        self.conditions.iter().all(|c| c.matches(event))
    }
}

// ─── NotificationRulesConfig ─────────────────────────────────────────────────

/// Top-level structure parsed from `.ta/notification-rules.toml`.
///
/// ```toml
/// global_channels = ["slack", "email"]
/// suppress_duplicates_secs = 300
///
/// [[rules]]
/// id = "goal-failed-alert"
/// name = "Alert on goal failure"
/// channels = ["slack"]
///
/// [[rules.conditions]]
/// type = "event_type"
/// value = "goal_failed"
///
/// [[rules.conditions]]
/// type = "severity_gte"
/// level = "error"
///
/// [rules.template]
/// title = "[TA] Goal failed"
/// body = "Goal {title} failed. Check `ta goal status {goal_id}`."
///
/// [rules.rate_limit]
/// max_per_period = 3
/// period_secs = 3600
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotificationRulesConfig {
    /// Rules to evaluate against each incoming event.
    #[serde(default)]
    pub rules: Vec<NotificationRule>,

    /// Suppress repeated (rule_id, event_type) notifications within this window (seconds).
    /// `None` disables global dedup.
    #[serde(default)]
    pub suppress_duplicates_secs: Option<u64>,

    /// Default delivery channels when a matched rule has no channel list.
    #[serde(default)]
    pub global_channels: Vec<String>,
}

impl NotificationRulesConfig {
    /// Parse config from a TOML file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        toml::from_str(&content).map_err(|e| format!("Cannot parse {}: {}", path.display(), e))
    }
}

// ─── NotificationRulesEngine ─────────────────────────────────────────────────

/// Stateful engine that matches events against notification rules and enforces
/// rate limits / dedup.
///
/// Designed to be wrapped in an `Arc<NotificationRulesEngine>` and shared
/// across the daemon's event-handling tasks.
pub struct NotificationRulesEngine {
    config: NotificationRulesConfig,
    /// Delivery timestamps keyed by `"{rule_id}:{event_type}"`.
    dedup_cache: Mutex<HashMap<String, Vec<Instant>>>,
}

impl NotificationRulesEngine {
    /// Create an engine from a parsed config.
    pub fn new(config: NotificationRulesConfig) -> Self {
        Self {
            config,
            dedup_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Load from a `.ta/notification-rules.toml` file.
    ///
    /// Returns an engine with an empty config (no rules, no channels) when the
    /// file does not exist — this is the common no-configuration case.
    pub fn load(path: &Path) -> Self {
        if !path.exists() {
            tracing::debug!(
                path = %path.display(),
                "No notification-rules.toml found; event notifications use daemon channel defaults"
            );
            return Self::new(NotificationRulesConfig::default());
        }

        match NotificationRulesConfig::load(path) {
            Ok(config) => {
                tracing::info!(
                    path = %path.display(),
                    rules = config.rules.len(),
                    global_channels = ?config.global_channels,
                    "Loaded notification rules"
                );
                Self::new(config)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Failed to parse notification-rules.toml; falling back to empty config. \
                     Fix the file and restart the daemon."
                );
                Self::new(NotificationRulesConfig::default())
            }
        }
    }

    /// Return all enabled rules that match `event`, sorted ascending by priority.
    pub fn matching_rules<'a>(&'a self, event: &EventEnvelope) -> Vec<&'a NotificationRule> {
        let mut matched: Vec<&NotificationRule> = self
            .config
            .rules
            .iter()
            .filter(|r| r.matches(event))
            .collect();
        matched.sort_by_key(|r| r.priority);
        matched
    }

    /// Resolve the delivery channel list for a matched rule.
    ///
    /// Falls back to `global_channels` when the rule has no explicit list.
    pub fn resolve_channels<'a>(&'a self, rule: &'a NotificationRule) -> &'a [String] {
        if rule.channels.is_empty() {
            &self.config.global_channels
        } else {
            &rule.channels
        }
    }

    /// Check dedup and rate limits.  Returns `true` if delivery should proceed;
    /// records the delivery timestamp when returning `true`.
    pub fn check_and_record(&self, rule: &NotificationRule, event: &EventEnvelope) -> bool {
        let key = format!("{}:{}", rule.id, event.event_type);
        let now = Instant::now();

        let mut cache = self.dedup_cache.lock().unwrap();
        let timestamps = cache.entry(key.clone()).or_default();

        // Global dedup window.
        if let Some(secs) = self.config.suppress_duplicates_secs {
            if timestamps
                .last()
                .map(|t| now.duration_since(*t) < Duration::from_secs(secs))
                .unwrap_or(false)
            {
                tracing::debug!(
                    rule_id = %rule.id,
                    event_type = %event.event_type,
                    suppress_secs = secs,
                    "Suppressing duplicate notification (global dedup window)"
                );
                return false;
            }
        }

        // Per-rule rate limit.
        if let Some(ref rl) = rule.rate_limit {
            let window = Duration::from_secs(rl.period_secs);
            let in_window = timestamps
                .iter()
                .filter(|t| now.duration_since(**t) < window)
                .count();
            if in_window >= rl.max_per_period as usize {
                tracing::debug!(
                    rule_id = %rule.id,
                    event_type = %event.event_type,
                    in_window,
                    max = rl.max_per_period,
                    "Suppressing notification (rate limit exceeded)"
                );
                return false;
            }
        }

        // Record this delivery.
        timestamps.push(now);
        // Prune to avoid unbounded growth.
        if timestamps.len() > 200 {
            timestamps.drain(..100);
        }
        true
    }

    /// Build template variable substitution map from an event envelope.
    ///
    /// Extracts common payload fields (`goal_id`, `title`, `agent_id`,
    /// `phase`, `error`) so templates can reference them as `{goal_id}` etc.
    pub fn build_template_vars(event: &EventEnvelope) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert("event_type".into(), event.event_type.clone());
        vars.insert("event_id".into(), event.id.to_string());
        vars.insert("timestamp".into(), event.timestamp.to_rfc3339());

        if let Ok(json) = serde_json::to_value(&event.payload) {
            for field in &[
                "goal_id", "title", "agent_id", "phase", "error", "message", "path",
            ] {
                if let Some(value) = json.get(field).and_then(|v| v.as_str()) {
                    vars.insert((*field).to_string(), value.to_string());
                }
            }
        }

        vars
    }

    /// Number of loaded rules.
    pub fn rule_count(&self) -> usize {
        self.config.rules.len()
    }

    /// Global default channels from the config.
    pub fn global_channels(&self) -> &[String] {
        &self.config.global_channels
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EventEnvelope, SessionEvent};
    use uuid::Uuid;

    fn goal_failed_event() -> EventEnvelope {
        EventEnvelope::new(SessionEvent::GoalFailed {
            goal_id: Uuid::new_v4(),
            error: "exit code 1".into(),
            exit_code: Some(1),
        })
    }

    fn goal_completed_event() -> EventEnvelope {
        EventEnvelope::new(SessionEvent::GoalCompleted {
            goal_id: Uuid::new_v4(),
            title: "Fix bug".into(),
            duration_secs: Some(42),
        })
    }

    fn make_rule(id: &str, condition: RuleCondition) -> NotificationRule {
        NotificationRule {
            id: id.into(),
            name: id.into(),
            enabled: true,
            priority: 100,
            conditions: vec![condition],
            channels: vec!["test".into()],
            template: None,
            rate_limit: None,
        }
    }

    // ── condition matching ──────────────────────────────────────────────────

    #[test]
    fn event_type_condition_matches() {
        let rule = make_rule(
            "r1",
            RuleCondition::EventType {
                value: "goal_failed".into(),
            },
        );
        assert!(rule.matches(&goal_failed_event()));
        assert!(!rule.matches(&goal_completed_event()));
    }

    #[test]
    fn event_type_in_condition() {
        let rule = make_rule(
            "r1",
            RuleCondition::EventTypeIn {
                values: vec!["goal_failed".into(), "build_failed".into()],
            },
        );
        assert!(rule.matches(&goal_failed_event()));
        assert!(!rule.matches(&goal_completed_event()));
    }

    #[test]
    fn severity_gte_error_matches_goal_failed() {
        let rule = make_rule(
            "r1",
            RuleCondition::SeverityGte {
                level: NotificationSeverity::Error,
            },
        );
        assert!(rule.matches(&goal_failed_event()));
        assert!(!rule.matches(&goal_completed_event()));
    }

    #[test]
    fn severity_gte_info_matches_all() {
        let rule = make_rule(
            "r1",
            RuleCondition::SeverityGte {
                level: NotificationSeverity::Info,
            },
        );
        assert!(rule.matches(&goal_failed_event()));
        assert!(rule.matches(&goal_completed_event()));
    }

    #[test]
    fn empty_conditions_matches_everything() {
        let rule = NotificationRule {
            id: "catch-all".into(),
            name: "catch-all".into(),
            enabled: true,
            priority: 200,
            conditions: vec![],
            channels: vec![],
            template: None,
            rate_limit: None,
        };
        assert!(rule.matches(&goal_failed_event()));
        assert!(rule.matches(&goal_completed_event()));
    }

    #[test]
    fn disabled_rule_never_matches() {
        let mut rule = make_rule(
            "r1",
            RuleCondition::EventType {
                value: "goal_failed".into(),
            },
        );
        rule.enabled = false;
        assert!(!rule.matches(&goal_failed_event()));
    }

    #[test]
    fn multiple_conditions_all_must_pass() {
        let rule = NotificationRule {
            id: "r1".into(),
            name: "r1".into(),
            enabled: true,
            priority: 100,
            conditions: vec![
                RuleCondition::EventType {
                    value: "goal_failed".into(),
                },
                RuleCondition::SeverityGte {
                    level: NotificationSeverity::Error,
                },
            ],
            channels: vec![],
            template: None,
            rate_limit: None,
        };
        assert!(rule.matches(&goal_failed_event()));
        assert!(!rule.matches(&goal_completed_event()));
    }

    // ── engine matching ─────────────────────────────────────────────────────

    #[test]
    fn engine_returns_matching_rules_sorted_by_priority() {
        let config = NotificationRulesConfig {
            rules: vec![
                NotificationRule {
                    id: "low".into(),
                    name: "low".into(),
                    enabled: true,
                    priority: 200,
                    conditions: vec![],
                    channels: vec![],
                    template: None,
                    rate_limit: None,
                },
                NotificationRule {
                    id: "high".into(),
                    name: "high".into(),
                    enabled: true,
                    priority: 10,
                    conditions: vec![],
                    channels: vec![],
                    template: None,
                    rate_limit: None,
                },
            ],
            ..Default::default()
        };
        let engine = NotificationRulesEngine::new(config);
        let event = goal_completed_event();
        let matched = engine.matching_rules(&event);
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].id, "high");
        assert_eq!(matched[1].id, "low");
    }

    #[test]
    fn engine_no_rules_returns_empty() {
        let engine = NotificationRulesEngine::new(NotificationRulesConfig::default());
        assert!(engine.matching_rules(&goal_failed_event()).is_empty());
    }

    // ── dedup & rate limiting ───────────────────────────────────────────────

    #[test]
    fn dedup_suppresses_second_delivery() {
        let config = NotificationRulesConfig {
            suppress_duplicates_secs: Some(60),
            ..Default::default()
        };
        let engine = NotificationRulesEngine::new(config);
        let rule = make_rule(
            "r1",
            RuleCondition::EventType {
                value: "goal_failed".into(),
            },
        );
        let event = goal_failed_event();
        assert!(
            engine.check_and_record(&rule, &event),
            "first delivery allowed"
        );
        assert!(!engine.check_and_record(&rule, &event), "second suppressed");
    }

    #[test]
    fn rate_limit_allows_up_to_max() {
        let config = NotificationRulesConfig::default();
        let engine = NotificationRulesEngine::new(config);
        let rule = NotificationRule {
            id: "r1".into(),
            name: "r1".into(),
            enabled: true,
            priority: 100,
            conditions: vec![],
            channels: vec![],
            template: None,
            rate_limit: Some(RateLimit {
                max_per_period: 2,
                period_secs: 60,
            }),
        };
        let event = goal_failed_event();
        assert!(engine.check_and_record(&rule, &event), "first");
        assert!(engine.check_and_record(&rule, &event), "second");
        assert!(!engine.check_and_record(&rule, &event), "third blocked");
    }

    #[test]
    fn no_dedup_allows_repeated_delivery() {
        let config = NotificationRulesConfig {
            suppress_duplicates_secs: None,
            ..Default::default()
        };
        let engine = NotificationRulesEngine::new(config);
        let rule = NotificationRule {
            id: "r1".into(),
            name: "r1".into(),
            enabled: true,
            priority: 100,
            conditions: vec![],
            channels: vec![],
            template: None,
            rate_limit: None,
        };
        let event = goal_completed_event();
        assert!(engine.check_and_record(&rule, &event));
        assert!(engine.check_and_record(&rule, &event));
        assert!(engine.check_and_record(&rule, &event));
    }

    // ── template rendering ──────────────────────────────────────────────────

    #[test]
    fn template_vars_include_event_type() {
        let event = goal_failed_event();
        let vars = NotificationRulesEngine::build_template_vars(&event);
        assert_eq!(vars["event_type"], "goal_failed");
        assert!(vars.contains_key("event_id"));
        assert!(vars.contains_key("timestamp"));
    }

    #[test]
    fn template_renders_placeholders() {
        let tmpl = NotificationTemplate {
            title: "[TA] {event_type} at {timestamp}".into(),
            body: "Event: {event_type}".into(),
        };
        let mut vars = HashMap::new();
        vars.insert("event_type".into(), "goal_failed".into());
        vars.insert("timestamp".into(), "2026-01-01T00:00:00Z".into());

        assert!(tmpl.render_title(&vars).contains("goal_failed"));
        assert_eq!(tmpl.render_body(&vars), "Event: goal_failed");
    }

    // ── channel resolution ──────────────────────────────────────────────────

    #[test]
    fn rule_channels_override_global() {
        let config = NotificationRulesConfig {
            global_channels: vec!["email".into()],
            ..Default::default()
        };
        let engine = NotificationRulesEngine::new(config);
        let rule = make_rule("r1", RuleCondition::EventType { value: "x".into() });
        assert_eq!(engine.resolve_channels(&rule), &["test"]);
    }

    #[test]
    fn empty_rule_channels_fall_back_to_global() {
        let config = NotificationRulesConfig {
            global_channels: vec!["slack".into(), "email".into()],
            ..Default::default()
        };
        let engine = NotificationRulesEngine::new(config);
        let rule = NotificationRule {
            id: "r1".into(),
            name: "r1".into(),
            enabled: true,
            priority: 100,
            conditions: vec![],
            channels: vec![],
            template: None,
            rate_limit: None,
        };
        assert_eq!(engine.resolve_channels(&rule), &["slack", "email"]);
    }

    // ── severity mapping ────────────────────────────────────────────────────

    #[test]
    fn severity_ordering() {
        assert!(NotificationSeverity::Critical > NotificationSeverity::Error);
        assert!(NotificationSeverity::Error > NotificationSeverity::Warning);
        assert!(NotificationSeverity::Warning > NotificationSeverity::Info);
    }

    #[test]
    fn severity_for_event_type() {
        assert_eq!(
            NotificationSeverity::for_event_type("policy_violation"),
            NotificationSeverity::Critical
        );
        assert_eq!(
            NotificationSeverity::for_event_type("goal_failed"),
            NotificationSeverity::Error
        );
        assert_eq!(
            NotificationSeverity::for_event_type("agent_needs_input"),
            NotificationSeverity::Warning
        );
        assert_eq!(
            NotificationSeverity::for_event_type("goal_started"),
            NotificationSeverity::Info
        );
    }

    // ── TOML round-trip ─────────────────────────────────────────────────────

    #[test]
    fn config_toml_round_trip() {
        let toml = r#"
suppress_duplicates_secs = 300
global_channels = ["slack"]

[[rules]]
id = "alert-failure"
name = "Alert on failure"
enabled = true
priority = 10
channels = ["slack", "email"]

[[rules.conditions]]
type = "event_type"
value = "goal_failed"

[rules.template]
title = "[TA] Goal failed"
body = "Goal {title} failed."

[rules.rate_limit]
max_per_period = 3
period_secs = 3600
"#;
        let config: NotificationRulesConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.rules[0].id, "alert-failure");
        assert_eq!(config.suppress_duplicates_secs, Some(300));
        assert_eq!(config.global_channels, &["slack"]);
        assert_eq!(config.rules[0].channels, &["slack", "email"]);
        let rl = config.rules[0].rate_limit.as_ref().unwrap();
        assert_eq!(rl.max_per_period, 3);
    }

    #[test]
    fn engine_load_nonexistent_path() {
        let engine =
            NotificationRulesEngine::load(Path::new("/tmp/definitely-does-not-exist.toml"));
        assert_eq!(engine.rule_count(), 0);
        assert!(engine.global_channels().is_empty());
    }

    #[test]
    fn engine_load_from_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notification-rules.toml");
        std::fs::write(
            &path,
            r#"
global_channels = ["slack"]

[[rules]]
id = "r1"
name = "test rule"
channels = ["slack"]

[[rules.conditions]]
type = "severity_gte"
level = "error"
"#,
        )
        .unwrap();

        let engine = NotificationRulesEngine::load(&path);
        assert_eq!(engine.rule_count(), 1);
        assert_eq!(engine.global_channels(), &["slack"]);

        let event = goal_failed_event();
        let matched = engine.matching_rules(&event);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "r1");
    }
}
