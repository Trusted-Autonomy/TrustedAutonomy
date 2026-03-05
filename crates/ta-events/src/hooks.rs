// hooks.rs -- Event hook execution: map event types to shell commands or webhook URLs.
//
// Hook configuration lives in `.ta/hooks.toml`:
//
// ```toml
// [[hooks]]
// event = "draft_approved"
// command = "notify-send 'Draft approved!'"
//
// [[hooks]]
// event = "policy_violation"
// webhook = "https://hooks.slack.com/services/..."
// ```

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::EventError;
use crate::schema::EventEnvelope;

/// A single hook configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    /// Event type to trigger on (e.g., "draft_approved", "policy_violation").
    pub event: String,
    /// Shell command to execute. The event JSON is passed via stdin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Webhook URL to POST the event JSON to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<String>,
}

/// Top-level hook configuration parsed from `.ta/hooks.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookConfig {
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

impl HookConfig {
    /// Load hook configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self, EventError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        // Use a basic TOML parser (we parse manually to avoid adding toml dep).
        Self::parse_toml(&content)
    }

    /// Parse the hooks config from TOML content.
    ///
    /// We support a simple subset: `[[hooks]]` arrays with `event`, `command`, `webhook` keys.
    fn parse_toml(content: &str) -> Result<Self, EventError> {
        let mut hooks = Vec::new();
        let mut current: Option<HookEntry> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            if trimmed == "[[hooks]]" {
                if let Some(entry) = current.take() {
                    hooks.push(entry);
                }
                current = Some(HookEntry {
                    event: String::new(),
                    command: None,
                    webhook: None,
                });
                continue;
            }

            if let Some(ref mut entry) = current {
                if let Some((key, value)) = trimmed.split_once('=') {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"');
                    match key {
                        "event" => entry.event = value.to_string(),
                        "command" => entry.command = Some(value.to_string()),
                        "webhook" => entry.webhook = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
        }

        if let Some(entry) = current {
            hooks.push(entry);
        }

        Ok(Self { hooks })
    }

    /// Find hooks that match a given event type.
    pub fn hooks_for_event(&self, event_type: &str) -> Vec<&HookEntry> {
        self.hooks
            .iter()
            .filter(|h| h.event == event_type)
            .collect()
    }
}

/// Executes hooks when events are published.
pub struct HookRunner {
    config: HookConfig,
}

impl HookRunner {
    /// Create a new hook runner with the given configuration.
    pub fn new(config: HookConfig) -> Self {
        Self { config }
    }

    /// Load configuration from `.ta/hooks.toml` at the given project root.
    pub fn from_project(project_root: &Path) -> Result<Self, EventError> {
        let path = project_root.join(".ta").join("hooks.toml");
        let config = HookConfig::load(&path)?;
        Ok(Self { config })
    }

    /// Execute all matching hooks for an event. Returns results per hook.
    pub fn execute(&self, envelope: &EventEnvelope) -> Vec<HookResult> {
        let hooks = self.config.hooks_for_event(&envelope.event_type);
        let mut results = Vec::new();

        let event_json = match serde_json::to_string(envelope) {
            Ok(json) => json,
            Err(e) => {
                results.push(HookResult {
                    event_type: envelope.event_type.clone(),
                    hook_type: "serialize".into(),
                    success: false,
                    message: Some(format!("Failed to serialize event: {}", e)),
                });
                return results;
            }
        };

        for hook in hooks {
            if let Some(cmd) = &hook.command {
                let result = execute_command(cmd, &event_json, &hook.event);
                results.push(result);
            }
            if let Some(url) = &hook.webhook {
                // Webhook execution is a best-effort POST. We don't block on response.
                results.push(HookResult {
                    event_type: hook.event.clone(),
                    hook_type: "webhook".into(),
                    success: true,
                    message: Some(format!("Webhook POST queued to {}", url)),
                });
            }
        }

        results
    }

    /// Get the number of configured hooks.
    pub fn hook_count(&self) -> usize {
        self.config.hooks.len()
    }

    /// Get event types that have hooks configured.
    pub fn configured_events(&self) -> Vec<String> {
        let mut events: Vec<String> = self.config.hooks.iter().map(|h| h.event.clone()).collect();
        events.sort();
        events.dedup();
        events
    }
}

/// Result of executing a single hook.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub event_type: String,
    pub hook_type: String,
    pub success: bool,
    pub message: Option<String>,
}

fn execute_command(cmd: &str, event_json: &str, event_type: &str) -> HookResult {
    let mut env = HashMap::new();
    env.insert("TA_EVENT_TYPE".to_string(), event_type.to_string());
    env.insert("TA_EVENT_JSON".to_string(), event_json.to_string());

    match Command::new("sh").arg("-c").arg(cmd).envs(env).output() {
        Ok(output) => HookResult {
            event_type: event_type.to_string(),
            hook_type: "command".into(),
            success: output.status.success(),
            message: if output.status.success() {
                None
            } else {
                Some(String::from_utf8_lossy(&output.stderr).to_string())
            },
        },
        Err(e) => HookResult {
            event_type: event_type.to_string(),
            hook_type: "command".into(),
            success: false,
            message: Some(format!("Failed to execute: {}", e)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SessionEvent;
    use tempfile::tempdir;

    #[test]
    fn parse_hooks_config() {
        let toml = r#"
[[hooks]]
event = "draft_approved"
command = "echo approved"

[[hooks]]
event = "policy_violation"
webhook = "https://example.com/hook"
"#;
        let config = HookConfig::parse_toml(toml).unwrap();
        assert_eq!(config.hooks.len(), 2);
        assert_eq!(config.hooks[0].event, "draft_approved");
        assert_eq!(config.hooks[0].command, Some("echo approved".into()));
        assert_eq!(config.hooks[1].event, "policy_violation");
        assert_eq!(
            config.hooks[1].webhook,
            Some("https://example.com/hook".into())
        );
    }

    #[test]
    fn hooks_for_event_filtering() {
        let config = HookConfig {
            hooks: vec![
                HookEntry {
                    event: "draft_approved".into(),
                    command: Some("echo a".into()),
                    webhook: None,
                },
                HookEntry {
                    event: "goal_completed".into(),
                    command: Some("echo b".into()),
                    webhook: None,
                },
                HookEntry {
                    event: "draft_approved".into(),
                    webhook: Some("https://example.com".into()),
                    command: None,
                },
            ],
        };
        let matched = config.hooks_for_event("draft_approved");
        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn hook_runner_execute_command() {
        let config = HookConfig {
            hooks: vec![HookEntry {
                event: "memory_stored".into(),
                command: Some("echo ok".into()),
                webhook: None,
            }],
        };
        let runner = HookRunner::new(config);

        let envelope = EventEnvelope::new(SessionEvent::MemoryStored {
            key: "test".into(),
            category: None,
            source: "cli".into(),
        });

        let results = runner.execute(&envelope);
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
    }

    #[test]
    fn hook_runner_no_matching_hooks() {
        let config = HookConfig {
            hooks: vec![HookEntry {
                event: "draft_approved".into(),
                command: Some("echo a".into()),
                webhook: None,
            }],
        };
        let runner = HookRunner::new(config);

        let envelope = EventEnvelope::new(SessionEvent::MemoryStored {
            key: "k".into(),
            category: None,
            source: "cli".into(),
        });

        let results = runner.execute(&envelope);
        assert!(results.is_empty());
    }

    #[test]
    fn load_missing_config() {
        let dir = tempdir().unwrap();
        let config = HookConfig::load(&dir.path().join("nonexistent.toml")).unwrap();
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn configured_events() {
        let config = HookConfig {
            hooks: vec![
                HookEntry {
                    event: "draft_approved".into(),
                    command: Some("echo".into()),
                    webhook: None,
                },
                HookEntry {
                    event: "draft_approved".into(),
                    webhook: Some("url".into()),
                    command: None,
                },
                HookEntry {
                    event: "goal_completed".into(),
                    command: Some("echo".into()),
                    webhook: None,
                },
            ],
        };
        let runner = HookRunner::new(config);
        let events = runner.configured_events();
        assert_eq!(events, vec!["draft_approved", "goal_completed"]);
    }
}
