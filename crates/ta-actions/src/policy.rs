// policy.rs — ActionPolicy configuration for the External Action Governance Framework.
//
// Loaded from `.ta/workflow.toml` under `[actions.<type>]` sections.
// Each action type can have its own policy (auto, review, block) plus
// rate-limit and type-specific options.
//
// Example `.ta/workflow.toml`:
//
// ```toml
// [actions.email]
// policy = "review"
// rate_limit = 10
//
// [actions.social_post]
// policy = "review"
// rate_limit = 1
//
// [actions.api_call]
// policy = "auto"
// allowed_domains = ["api.stripe.com", "api.github.com"]
//
// [actions.db_query]
// policy = "review"
// auto_approve_reads = true
// ```

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

// ── Policy enum ──────────────────────────────────────────────────────────────

/// Governance policy for an action type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActionPolicy {
    /// Auto-approve and execute immediately (if a plugin executor is registered).
    Auto,
    /// Capture for human review before execution. Surfaces in `ta draft view`.
    #[default]
    Review,
    /// Always block. Returns an error to the agent without capturing.
    Block,
}

impl std::fmt::Display for ActionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionPolicy::Auto => write!(f, "auto"),
            ActionPolicy::Review => write!(f, "review"),
            ActionPolicy::Block => write!(f, "block"),
        }
    }
}

// ── Per-type config ──────────────────────────────────────────────────────────

/// Policy configuration for a single action type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPolicyConfig {
    /// How the action is governed (default: review).
    #[serde(default)]
    pub policy: ActionPolicy,

    /// Maximum number of this action type allowed per goal.
    /// `None` means no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<u32>,

    /// For `api_call`: restrict execution to these domains (policy=auto only).
    /// Wildcards supported: `"*.github.com"` matches `api.github.com`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_domains: Vec<String>,

    /// For `db_query`: automatically approve SELECT-class queries.
    /// INSERT/UPDATE/DELETE still go through the configured policy.
    #[serde(default)]
    pub auto_approve_reads: bool,

    /// For `email`: optional list of allowed recipient addresses.
    /// If set, any email draft to an address not in this list is flagged with
    /// "Recipient not in allowed_recipients" before creating the draft.
    /// Empty list means no restriction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_recipients: Vec<String>,

    /// For `email`: cross-session hourly rate limit. `None` means no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_hour: Option<u32>,

    /// For `email`: cross-session daily rate limit. `None` means no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_day: Option<u32>,
}

impl Default for ActionPolicyConfig {
    fn default() -> Self {
        Self {
            policy: ActionPolicy::Review,
            rate_limit: None,
            allowed_domains: vec![],
            auto_approve_reads: false,
            allowed_recipients: vec![],
            max_per_hour: None,
            max_per_day: None,
        }
    }
}

// ── Full config ──────────────────────────────────────────────────────────────

/// All action policies loaded from `.ta/workflow.toml`.
///
/// Access via `ActionPolicies::load()` — returns defaults for missing keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionPolicies {
    /// Per-action-type configurations. Key is the action type name.
    #[serde(default)]
    pub actions: HashMap<String, ActionPolicyConfig>,
}

impl ActionPolicies {
    /// Load action policies from `.ta/workflow.toml`.
    ///
    /// Returns an `ActionPolicies` with empty `actions` map if the file does not
    /// exist or has no `[actions.*]` sections. Callers use `policy_for()` which
    /// returns a safe default (policy=review) for unknown action types.
    pub fn load(workflow_toml_path: &Path) -> Self {
        if !workflow_toml_path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(workflow_toml_path) {
            Ok(content) => Self::parse(&content),
            Err(e) => {
                tracing::warn!(
                    path = %workflow_toml_path.display(),
                    error = %e,
                    "failed to read workflow.toml; using default action policies"
                );
                Self::default()
            }
        }
    }

    /// Parse action policies from TOML content.
    fn parse(content: &str) -> Self {
        // Use the toml crate to parse the full document, then extract the
        // `[actions]` table.
        #[derive(Deserialize)]
        struct Root {
            #[serde(default)]
            actions: HashMap<String, ActionPolicyConfig>,
        }

        match toml::from_str::<Root>(content) {
            Ok(root) => Self {
                actions: root.actions,
            },
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to parse workflow.toml action policies; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Return the policy config for the given action type.
    ///
    /// Falls back to `ActionPolicyConfig::default()` (policy=review, no rate limit)
    /// when no explicit config exists — safe-by-default behaviour.
    pub fn policy_for(&self, action_type: &str) -> ActionPolicyConfig {
        self.actions.get(action_type).cloned().unwrap_or_default()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_toml(content: &str) -> ActionPolicies {
        ActionPolicies::parse(content)
    }

    #[test]
    fn parse_basic_policy_config() {
        let toml = r#"
[actions.email]
policy = "review"
rate_limit = 10

[actions.social_post]
policy = "review"
rate_limit = 1

[actions.api_call]
policy = "auto"
allowed_domains = ["api.stripe.com", "api.github.com"]

[actions.db_query]
policy = "review"
auto_approve_reads = true
"#;
        let policies = make_toml(toml);

        let email = policies.policy_for("email");
        assert_eq!(email.policy, ActionPolicy::Review);
        assert_eq!(email.rate_limit, Some(10));

        let social = policies.policy_for("social_post");
        assert_eq!(social.rate_limit, Some(1));

        let api = policies.policy_for("api_call");
        assert_eq!(api.policy, ActionPolicy::Auto);
        assert_eq!(
            api.allowed_domains,
            vec!["api.stripe.com", "api.github.com"]
        );

        let db = policies.policy_for("db_query");
        assert!(db.auto_approve_reads);
    }

    #[test]
    fn parse_block_policy() {
        let toml = r#"
[actions.social_post]
policy = "block"
"#;
        let policies = make_toml(toml);
        assert_eq!(
            policies.policy_for("social_post").policy,
            ActionPolicy::Block
        );
    }

    #[test]
    fn unknown_type_returns_review_default() {
        let policies = ActionPolicies::default();
        let config = policies.policy_for("unknown_action_type");
        assert_eq!(config.policy, ActionPolicy::Review);
        assert!(config.rate_limit.is_none());
    }

    #[test]
    fn empty_toml_returns_empty_policies() {
        let policies = make_toml("");
        assert!(policies.actions.is_empty());
    }

    #[test]
    fn load_from_nonexistent_file_returns_default() {
        let path = Path::new("/tmp/does_not_exist_ta_workflow.toml");
        let policies = ActionPolicies::load(path);
        assert!(policies.actions.is_empty());
    }

    #[test]
    fn load_from_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workflow.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "[actions.email]\npolicy = \"block\"\nrate_limit = 5").unwrap();

        let policies = ActionPolicies::load(&path);
        let email = policies.policy_for("email");
        assert_eq!(email.policy, ActionPolicy::Block);
        assert_eq!(email.rate_limit, Some(5));
    }

    #[test]
    fn allowed_recipients_parsed_from_toml() {
        let toml = r#"
[actions.email]
policy = "review"
allowed_recipients = ["alice@example.com", "bob@example.com"]
"#;
        let policies = make_toml(toml);
        let email = policies.policy_for("email");
        assert_eq!(
            email.allowed_recipients,
            vec!["alice@example.com", "bob@example.com"]
        );
    }

    #[test]
    fn max_per_hour_and_max_per_day_parsed_from_toml() {
        let toml = r#"
[actions.email]
policy = "review"
max_per_hour = 5
max_per_day = 20
"#;
        let policies = make_toml(toml);
        let email = policies.policy_for("email");
        assert_eq!(email.max_per_hour, Some(5));
        assert_eq!(email.max_per_day, Some(20));
    }

    #[test]
    fn default_config_has_empty_recipients_and_no_rate_limits() {
        let config = ActionPolicyConfig::default();
        assert!(config.allowed_recipients.is_empty());
        assert!(config.max_per_hour.is_none());
        assert!(config.max_per_day.is_none());
    }
}
