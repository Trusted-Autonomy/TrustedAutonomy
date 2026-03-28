// trigger.rs — Workflow trigger conditions for event-driven execution (v0.14.8.3).
//
// Defines `TriggerCondition` for matching TA events against workflow definitions.
// A workflow with `trigger_on` conditions parks (waits) until a matching event arrives.
//
// # workflow.toml example
//
// ```toml
// [[trigger]]
// event = "vcs.pr_merged"
// workflow = "governed-goal"
//
// [trigger.filter]
// branch = "main"
//
// [[trigger]]
// event = "vcs.changelist_submitted"
// workflow = "governed-goal"
//
// [trigger.filter]
// depot_path = "//depot/main/..."
// ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A single trigger condition in `workflow.toml`.
///
/// When TA receives an event matching `event` (and all `filter` fields match),
/// it starts or resumes the named workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerCondition {
    /// TA event type to trigger on (e.g., "vcs.pr_merged", "vcs.branch_pushed").
    pub event: String,
    /// Workflow name to start when the event fires.
    pub workflow: String,
    /// Optional filter — key/value pairs that must match fields in the event payload.
    /// All filters must match (AND semantics).
    ///
    /// Example: `{ branch = "main" }` — only trigger when the PR targets main.
    #[serde(default)]
    pub filter: HashMap<String, String>,
    /// How long (in hours) to wait for this event before the trigger expires.
    /// Default: 72 hours.
    #[serde(default = "default_timeout_hours")]
    pub timeout_hours: u64,
}

fn default_timeout_hours() -> u64 {
    72
}

impl TriggerCondition {
    /// Check whether an incoming event (by event_type string) and its JSON payload
    /// satisfy this trigger condition.
    ///
    /// Returns `true` if `event_type` matches AND all filter fields match in `payload`.
    pub fn matches(&self, event_type: &str, payload: &serde_json::Value) -> bool {
        if self.event != event_type {
            return false;
        }
        // All filter key/value pairs must match a field in the payload.
        for (key, expected) in &self.filter {
            let actual = payload.get(key).and_then(|v| v.as_str()).unwrap_or("");
            if actual != expected {
                return false;
            }
        }
        true
    }
}

/// A trigger configuration block from `workflow.toml`.
///
/// Typically placed at the top level of the workflow config file.
///
/// ```toml
/// [[trigger]]
/// event = "vcs.pr_merged"
/// workflow = "governed-goal"
///
/// [trigger.filter]
/// branch = "main"
/// ```
pub type TriggerConfig = Vec<TriggerCondition>;

/// A parked workflow run waiting for a trigger event.
///
/// When a workflow step of type `trigger_on` is reached, the run is serialized
/// into `TriggerWaitRecord` and stored. When the matching event arrives, the
/// record is picked up and the workflow is resumed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerWaitRecord {
    /// Unique ID for this parked run (matches the workflow run ID).
    pub run_id: String,
    /// The event type being waited on.
    pub event: String,
    /// The workflow to resume.
    pub workflow: String,
    /// When this wait was created.
    pub parked_at: chrono::DateTime<chrono::Utc>,
    /// When this wait expires (from `timeout_hours`).
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Optional filter fields.
    #[serde(default)]
    pub filter: HashMap<String, String>,
}

impl TriggerWaitRecord {
    /// Create a new parked trigger wait record.
    pub fn new(run_id: String, condition: &TriggerCondition) -> Self {
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::hours(condition.timeout_hours as i64);
        Self {
            run_id,
            event: condition.event.clone(),
            workflow: condition.workflow.clone(),
            parked_at: now,
            expires_at,
            filter: condition.filter.clone(),
        }
    }

    /// Check if this wait record has expired.
    pub fn is_expired(&self) -> bool {
        chrono::Utc::now() > self.expires_at
    }

    /// Check if an incoming event matches this wait record.
    pub fn matches(&self, event_type: &str, payload: &serde_json::Value) -> bool {
        if self.event != event_type || self.is_expired() {
            return false;
        }
        for (key, expected) in &self.filter {
            let actual = payload.get(key).and_then(|v| v.as_str()).unwrap_or("");
            if actual != expected {
                return false;
            }
        }
        true
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_condition_matches_event_type() {
        let trigger = TriggerCondition {
            event: "vcs.pr_merged".to_string(),
            workflow: "governed-goal".to_string(),
            filter: HashMap::new(),
            timeout_hours: 72,
        };
        let payload = serde_json::json!({});
        assert!(trigger.matches("vcs.pr_merged", &payload));
        assert!(!trigger.matches("vcs.branch_pushed", &payload));
    }

    #[test]
    fn trigger_condition_filter_branch() {
        let mut filter = HashMap::new();
        filter.insert("branch".to_string(), "main".to_string());
        let trigger = TriggerCondition {
            event: "vcs.pr_merged".to_string(),
            workflow: "governed-goal".to_string(),
            filter,
            timeout_hours: 72,
        };
        let payload_main = serde_json::json!({ "branch": "main" });
        let payload_dev = serde_json::json!({ "branch": "develop" });
        assert!(trigger.matches("vcs.pr_merged", &payload_main));
        assert!(!trigger.matches("vcs.pr_merged", &payload_dev));
    }

    #[test]
    fn trigger_condition_multi_filter() {
        let mut filter = HashMap::new();
        filter.insert("branch".to_string(), "main".to_string());
        filter.insert("provider".to_string(), "github".to_string());
        let trigger = TriggerCondition {
            event: "vcs.pr_merged".to_string(),
            workflow: "governed-goal".to_string(),
            filter,
            timeout_hours: 72,
        };
        let match_payload = serde_json::json!({ "branch": "main", "provider": "github" });
        let no_match_payload = serde_json::json!({ "branch": "main", "provider": "gitlab" });
        assert!(trigger.matches("vcs.pr_merged", &match_payload));
        assert!(!trigger.matches("vcs.pr_merged", &no_match_payload));
    }

    #[test]
    fn trigger_wait_record_expires() {
        let condition = TriggerCondition {
            event: "vcs.pr_merged".to_string(),
            workflow: "governed-goal".to_string(),
            filter: HashMap::new(),
            timeout_hours: 0, // Immediately expired.
        };
        // timeout_hours = 0 means expires_at = now (already past).
        let record = TriggerWaitRecord::new("run-1".to_string(), &condition);
        // Sleep 1ms to ensure expiry.
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(record.is_expired());
    }

    #[test]
    fn trigger_wait_record_matches() {
        let condition = TriggerCondition {
            event: "vcs.pr_merged".to_string(),
            workflow: "governed-goal".to_string(),
            filter: HashMap::new(),
            timeout_hours: 72,
        };
        let record = TriggerWaitRecord::new("run-1".to_string(), &condition);
        assert!(record.matches("vcs.pr_merged", &serde_json::json!({})));
        assert!(!record.matches("vcs.branch_pushed", &serde_json::json!({})));
    }

    #[test]
    fn trigger_config_toml_parse() {
        let toml_str = r#"
[[trigger]]
event = "vcs.pr_merged"
workflow = "governed-goal"
timeout_hours = 48

[trigger.filter]
branch = "main"
"#;
        // Parse as array of TriggerCondition.
        let parsed: toml::Value = toml::from_str(toml_str).unwrap();
        let triggers = parsed["trigger"].as_array().unwrap();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0]["event"].as_str().unwrap(), "vcs.pr_merged");
        assert_eq!(triggers[0]["filter"]["branch"].as_str().unwrap(), "main");
        assert_eq!(triggers[0]["timeout_hours"].as_integer().unwrap(), 48);
    }
}
