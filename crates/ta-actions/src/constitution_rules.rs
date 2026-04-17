// constitution_rules.rs — PolicyConstitution for the External Action Governance Framework.
//
// Loads constitution rules from `.ta/constitution.toml`. Rules describe which
// actions are blocked or warned. Built-in defaults always block email from
// using `policy = "auto"` — email is always human-reviewed.
//
// Example `.ta/constitution.toml`:
//
// ```toml
// [[rules.block]]
// action_type = "email"
// condition   = "policy_is_not_review"
// message     = "Email actions must use policy = review"
// allow_override = false
//
// [[rules.warn]]
// action_type = "social_post"
// condition   = "always"
// message     = "Social media posts require review."
// allow_override = true
// ```

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ActionPolicy;

// ── Rule ──────────────────────────────────────────────────────────────────────

/// A single constitution rule (block or warn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstitutionRule {
    /// The action type this rule applies to (e.g., `"email"`).
    pub action_type: String,

    /// Condition string evaluated at dispatch time.
    ///
    /// Supported conditions:
    /// - `"policy_is_not_review"` — fires when the action's policy is not `review`
    /// - `"always"` — always fires
    pub condition: String,

    /// Human-readable message returned when this rule fires.
    pub message: String,

    /// Whether the caller may override this rule (e.g., with a flag).
    /// Default `false` — block rules are not overridable by default.
    #[serde(default)]
    pub allow_override: bool,
}

impl ConstitutionRule {
    /// Evaluate whether this rule fires for the given policy.
    fn fires(&self, policy: &ActionPolicy) -> bool {
        match self.condition.as_str() {
            "policy_is_not_review" => !matches!(policy, ActionPolicy::Review),
            "always" => true,
            _ => {
                tracing::warn!(
                    condition = %self.condition,
                    "unknown constitution rule condition — treating as 'never'"
                );
                false
            }
        }
    }
}

// ── Violation ─────────────────────────────────────────────────────────────────

/// Returned when a constitution rule fires.
#[derive(Debug, Clone)]
pub struct ConstitutionViolation {
    /// Human-readable explanation of why the action was blocked or warned.
    pub message: String,
    /// `true` if this is a warn-only violation (action allowed but logged).
    /// `false` if this is a hard block.
    pub is_warn: bool,
}

// ── Constitution ──────────────────────────────────────────────────────────────

/// The full set of constitution rules loaded from `.ta/constitution.toml`.
///
/// Built-in default rules are always active. Custom rules from `constitution.toml`
/// are merged on top — they do not replace the defaults.
#[derive(Debug, Clone, Default)]
pub struct PolicyConstitution {
    /// Rules that block the action.
    pub block_rules: Vec<ConstitutionRule>,
    /// Rules that warn (allow but log).
    pub warn_rules: Vec<ConstitutionRule>,
}

/// TOML-shaped structure for deserialization.
#[derive(Debug, Deserialize, Default)]
struct ConstitutionToml {
    #[serde(default)]
    rules: ConstitutionRuleSets,
}

#[derive(Debug, Deserialize, Default)]
struct ConstitutionRuleSets {
    #[serde(default)]
    block: Vec<ConstitutionRule>,
    #[serde(default)]
    warn: Vec<ConstitutionRule>,
}

impl PolicyConstitution {
    /// Built-in default rules (always active even without a constitution.toml).
    fn default_rules() -> Self {
        Self {
            block_rules: vec![ConstitutionRule {
                action_type: "email".into(),
                condition: "policy_is_not_review".into(),
                message: "Email actions must use policy = review — TA never sends email \
                          autonomously. Drafts are created in your Drafts folder for you \
                          to review and send."
                    .into(),
                allow_override: false,
            }],
            warn_rules: vec![],
        }
    }

    /// Load from `.ta/constitution.toml`. Returns built-in defaults if the file
    /// is absent or unreadable. Custom rules are merged with defaults.
    pub fn load(workspace_root: &Path) -> Self {
        let path = workspace_root.join(".ta").join("constitution.toml");
        let defaults = Self::default_rules();

        if !path.exists() {
            return defaults;
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => Self::parse_and_merge(&content, defaults),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read constitution.toml; using default constitution rules"
                );
                defaults
            }
        }
    }

    fn parse_and_merge(content: &str, mut base: Self) -> Self {
        match toml::from_str::<ConstitutionToml>(content) {
            Ok(parsed) => {
                // Append custom rules after built-in defaults.
                base.block_rules.extend(parsed.rules.block);
                base.warn_rules.extend(parsed.rules.warn);
                base
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to parse constitution.toml; using default constitution rules"
                );
                base
            }
        }
    }

    /// Check whether the given policy is allowed for the given action type.
    ///
    /// Returns:
    /// - `Ok(())` if no rule fires (or only warn rules with `is_warn=true`)
    /// - `Err(ConstitutionViolation { is_warn: false })` if a block rule fires
    /// - `Err(ConstitutionViolation { is_warn: true })` if only warn rules fire
    ///
    /// Block rules take precedence over warn rules. If multiple rules match, the
    /// first block rule wins.
    pub fn check_email_policy(&self, policy: &ActionPolicy) -> Result<(), ConstitutionViolation> {
        self.check_action_policy("email", policy)
    }

    /// Generic policy check for any action type.
    pub fn check_action_policy(
        &self,
        action_type: &str,
        policy: &ActionPolicy,
    ) -> Result<(), ConstitutionViolation> {
        // Check block rules first.
        for rule in &self.block_rules {
            if rule.action_type == action_type && rule.fires(policy) {
                return Err(ConstitutionViolation {
                    message: rule.message.clone(),
                    is_warn: false,
                });
            }
        }

        // Then warn rules.
        for rule in &self.warn_rules {
            if rule.action_type == action_type && rule.fires(policy) {
                return Err(ConstitutionViolation {
                    message: rule.message.clone(),
                    is_warn: true,
                });
            }
        }

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_constitution() -> PolicyConstitution {
        PolicyConstitution::default_rules()
    }

    #[test]
    fn block_rule_fires_when_email_policy_is_auto() {
        let constitution = default_constitution();
        let result = constitution.check_email_policy(&ActionPolicy::Auto);
        assert!(result.is_err());
        let violation = result.unwrap_err();
        assert!(!violation.is_warn, "should be a hard block, not a warn");
        assert!(
            violation.message.contains("policy = review"),
            "message should mention review policy"
        );
    }

    #[test]
    fn block_rule_passes_when_email_policy_is_review() {
        let constitution = default_constitution();
        let result = constitution.check_email_policy(&ActionPolicy::Review);
        assert!(result.is_ok(), "review policy should pass the constitution");
    }

    #[test]
    fn block_rule_fires_when_email_policy_is_block() {
        // Block is also "not review" — the rule fires (block > block is fine
        // since the action won't execute anyway, but the rule still fires).
        let constitution = default_constitution();
        let result = constitution.check_email_policy(&ActionPolicy::Block);
        // Block policy is "not review" so the constitution rule fires
        assert!(result.is_err());
        let violation = result.unwrap_err();
        assert!(!violation.is_warn);
    }

    #[test]
    fn warn_rule_returns_ok_with_is_warn_true() {
        let mut constitution = PolicyConstitution::default();
        constitution.warn_rules.push(ConstitutionRule {
            action_type: "social_post".into(),
            condition: "always".into(),
            message: "Social posts require review.".into(),
            allow_override: true,
        });

        let result = constitution.check_action_policy("social_post", &ActionPolicy::Auto);
        assert!(result.is_err());
        let violation = result.unwrap_err();
        assert!(violation.is_warn, "should be a warn, not a hard block");
    }

    #[test]
    fn allow_override_true_does_not_change_violation_detection() {
        // allow_override is a flag for callers to decide whether to bypass —
        // it does not affect whether the rule fires in check_action_policy.
        let mut constitution = PolicyConstitution::default();
        constitution.block_rules.push(ConstitutionRule {
            action_type: "api_call".into(),
            condition: "always".into(),
            message: "API calls are restricted.".into(),
            allow_override: true,
        });

        let result = constitution.check_action_policy("api_call", &ActionPolicy::Auto);
        assert!(
            result.is_err(),
            "rule still fires even with allow_override=true"
        );
        let violation = result.unwrap_err();
        assert!(!violation.is_warn);
    }

    #[test]
    fn load_from_nonexistent_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let constitution = PolicyConstitution::load(dir.path());
        // Built-in email block rule should be present.
        assert_eq!(constitution.block_rules.len(), 1);
        assert_eq!(constitution.block_rules[0].action_type, "email");
    }

    #[test]
    fn load_merges_custom_rules_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("constitution.toml"),
            r#"
[[rules.warn]]
action_type = "social_post"
condition   = "always"
message     = "Social posts need review."
allow_override = true
"#,
        )
        .unwrap();

        let constitution = PolicyConstitution::load(dir.path());
        // Built-in block rule + custom warn rule.
        assert_eq!(constitution.block_rules.len(), 1);
        assert_eq!(constitution.warn_rules.len(), 1);
        assert_eq!(constitution.warn_rules[0].action_type, "social_post");
    }
}
