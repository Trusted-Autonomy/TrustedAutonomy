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
//
// [[rules.warn]]
// action_type = "db_query"
// condition   = "rows_modified_over_threshold"
// threshold   = 100
// message     = "This draft modifies more than 100 rows — review carefully."
// allow_override = true
//
// [[rules.block]]
// action_type = "db_query"
// condition   = "schema_altering_statement"
// message     = "Schema-altering statements (DROP TABLE, TRUNCATE, ALTER ... DROP COLUMN) are \
//                blocked by default. Set allow_schema_drops = true under [actions.db_query] \
//                in workflow.toml to allow them."
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
    /// - `"rows_modified_over_threshold"` — fires when a DB draft's row-mutation
    ///   count exceeds `threshold` (v0.17.1, evaluated via `check_db_mutation`,
    ///   not `check_action_policy` — this condition ignores `ActionPolicy`)
    /// - `"schema_altering_statement"` — fires when a DB draft contains a
    ///   schema-altering statement (v0.17.1, same caveat)
    pub condition: String,

    /// Human-readable message returned when this rule fires.
    pub message: String,

    /// Whether the caller may override this rule (e.g., with a flag).
    /// Default `false` — block rules are not overridable by default.
    #[serde(default)]
    pub allow_override: bool,

    /// Threshold used by `"rows_modified_over_threshold"`. Ignored by every
    /// other condition. `None` falls back to `DEFAULT_ROWS_MODIFIED_THRESHOLD`.
    #[serde(default)]
    pub threshold: Option<u64>,
}

/// Default row-mutation-count threshold for the `"rows_modified_over_threshold"`
/// condition when a rule doesn't specify its own `threshold` (v0.17.1 item 3).
pub const DEFAULT_ROWS_MODIFIED_THRESHOLD: u64 = 100;

impl ConstitutionRule {
    /// Evaluate whether this rule fires for the given policy.
    fn fires(&self, policy: &ActionPolicy) -> bool {
        match self.condition.as_str() {
            "policy_is_not_review" => !matches!(policy, ActionPolicy::Review),
            "always" => true,
            "rows_modified_over_threshold" | "schema_altering_statement" => {
                // These conditions evaluate DB draft context, not an
                // ActionPolicy — they're only meaningful via
                // `check_db_mutation`, never `check_action_policy`.
                false
            }
            _ => {
                tracing::warn!(
                    condition = %self.condition,
                    "unknown constitution rule condition — treating as 'never'"
                );
                false
            }
        }
    }

    /// Evaluate whether this rule fires for a staged DB draft (v0.17.1).
    fn fires_for_db_mutation(
        &self,
        rows_modified: u64,
        has_schema_altering_statement: bool,
    ) -> bool {
        match self.condition.as_str() {
            "rows_modified_over_threshold" => {
                rows_modified > self.threshold.unwrap_or(DEFAULT_ROWS_MODIFIED_THRESHOLD)
            }
            "schema_altering_statement" => has_schema_altering_statement,
            "always" => true,
            // "policy_is_not_review" has no DB-draft equivalent — never fires here.
            _ => false,
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
            block_rules: vec![
                ConstitutionRule {
                    action_type: "email".into(),
                    condition: "policy_is_not_review".into(),
                    message: "Email actions must use policy = review — TA never sends email \
                          autonomously. Drafts are created in your Drafts folder for you \
                          to review and send."
                        .into(),
                    allow_override: false,
                    threshold: None,
                },
                ConstitutionRule {
                    action_type: "db_query".into(),
                    condition: "schema_altering_statement".into(),
                    message: "Schema-altering statements (DROP TABLE, TRUNCATE, ALTER TABLE \
                          ... DROP COLUMN) are blocked by default. Set allow_schema_drops = \
                          true under [actions.db_query] in workflow.toml to allow them."
                        .into(),
                    allow_override: true,
                    threshold: None,
                },
            ],
            warn_rules: vec![ConstitutionRule {
                action_type: "db_query".into(),
                condition: "rows_modified_over_threshold".into(),
                message: format!(
                    "This draft modifies more than {DEFAULT_ROWS_MODIFIED_THRESHOLD} rows — \
                     review the row-level diff carefully before approving."
                ),
                allow_override: true,
                threshold: Some(DEFAULT_ROWS_MODIFIED_THRESHOLD),
            }],
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

    /// Check a staged database draft against `db_query` constitution rules
    /// (v0.17.1 item 3) — the row-mutation-count warn rule and the
    /// schema-altering-statement block rule.
    ///
    /// `allow_schema_drops` is `[actions.db_query].allow_schema_drops` from
    /// `workflow.toml` (default `false`): when `true`, the
    /// `"schema_altering_statement"` block rule is skipped entirely — a
    /// project opts in explicitly rather than the rule losing its bite
    /// silently. Block rules still take precedence over warn rules, same
    /// ordering as `check_action_policy`.
    pub fn check_db_mutation(
        &self,
        rows_modified: u64,
        has_schema_altering_statement: bool,
        allow_schema_drops: bool,
    ) -> Result<(), ConstitutionViolation> {
        for rule in &self.block_rules {
            if rule.action_type != "db_query" {
                continue;
            }
            if rule.condition == "schema_altering_statement" && allow_schema_drops {
                continue;
            }
            if rule.fires_for_db_mutation(rows_modified, has_schema_altering_statement) {
                return Err(ConstitutionViolation {
                    message: rule.message.clone(),
                    is_warn: false,
                });
            }
        }

        for rule in &self.warn_rules {
            if rule.action_type == "db_query"
                && rule.fires_for_db_mutation(rows_modified, has_schema_altering_statement)
            {
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
            threshold: None,
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
            threshold: None,
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
        // Built-in email block rule + db_query schema-drop block rule.
        assert_eq!(constitution.block_rules.len(), 2);
        assert_eq!(constitution.block_rules[0].action_type, "email");
        assert_eq!(constitution.block_rules[1].action_type, "db_query");
        // Built-in db_query rows-modified warn rule.
        assert_eq!(constitution.warn_rules.len(), 1);
        assert_eq!(constitution.warn_rules[0].action_type, "db_query");
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
        // Built-in block rules (email + db_query schema drop) + custom warn rule
        // on top of the built-in db_query rows-modified warn rule.
        assert_eq!(constitution.block_rules.len(), 2);
        assert_eq!(constitution.warn_rules.len(), 2);
        assert!(constitution
            .warn_rules
            .iter()
            .any(|r| r.action_type == "social_post"));
    }

    // ── DB mutation rules (v0.17.1) ─────────────────────────────────────────

    #[test]
    fn schema_altering_statement_blocks_by_default() {
        let constitution = default_constitution();
        let result = constitution.check_db_mutation(1, true, false);
        assert!(result.is_err());
        let violation = result.unwrap_err();
        assert!(!violation.is_warn, "schema drop should be a hard block");
        assert!(violation.message.contains("allow_schema_drops"));
    }

    #[test]
    fn schema_altering_statement_allowed_when_opted_in() {
        let constitution = default_constitution();
        let result = constitution.check_db_mutation(1, true, true);
        assert!(
            result.is_ok(),
            "allow_schema_drops=true should bypass the block rule"
        );
    }

    #[test]
    fn rows_modified_over_threshold_warns() {
        let constitution = default_constitution();
        let result = constitution.check_db_mutation(101, false, false);
        assert!(result.is_err());
        let violation = result.unwrap_err();
        assert!(
            violation.is_warn,
            "over-threshold row count should warn, not block"
        );
    }

    #[test]
    fn rows_modified_at_or_under_threshold_passes() {
        let constitution = default_constitution();
        assert!(constitution.check_db_mutation(100, false, false).is_ok());
        assert!(constitution.check_db_mutation(0, false, false).is_ok());
    }

    #[test]
    fn schema_drop_block_takes_precedence_over_row_count_warn() {
        let constitution = default_constitution();
        let result = constitution.check_db_mutation(500, true, false);
        assert!(result.is_err());
        assert!(
            !result.unwrap_err().is_warn,
            "block rules win over warn rules"
        );
    }

    #[test]
    fn custom_threshold_overrides_default() {
        let mut constitution = PolicyConstitution::default();
        constitution.warn_rules.push(ConstitutionRule {
            action_type: "db_query".into(),
            condition: "rows_modified_over_threshold".into(),
            message: "Custom threshold exceeded.".into(),
            allow_override: true,
            threshold: Some(10),
        });

        assert!(constitution.check_db_mutation(10, false, false).is_ok());
        assert!(constitution.check_db_mutation(11, false, false).is_err());
    }

    #[test]
    fn non_db_query_action_type_rules_are_ignored_by_check_db_mutation() {
        let mut constitution = PolicyConstitution::default();
        constitution.block_rules.push(ConstitutionRule {
            action_type: "email".into(),
            condition: "schema_altering_statement".into(),
            message: "Should never fire — wrong action_type.".into(),
            allow_override: false,
            threshold: None,
        });
        assert!(constitution.check_db_mutation(0, true, false).is_ok());
    }
}
