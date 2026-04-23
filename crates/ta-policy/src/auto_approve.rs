// auto_approve.rs — Draft auto-approval evaluation (v0.9.8.1).
//
// Evaluates whether a draft package should be auto-approved based on the
// policy document's conditions. Returns an AutoApproveDecision with either
// approval reasons (for audit trail) or denial blockers (for review request).
//
// Evaluation order (short-circuits on first failure):
//   1. enabled check
//   2. size limits (max_files, max_lines_changed)
//   3. path rules (blocked_paths first, then allowed_paths)
//   4. phase limits
//   5. agent security level

use serde::{Deserialize, Serialize};

use crate::access_filter::AccessFilter;
use crate::approval_rules::{evaluate_approval_rules, ApprovalAction, ApprovalRule};
use crate::document::{AutoApproveDraftConfig, PolicyDocument, SecurityLevel};

/// Minimal draft info needed for auto-approval evaluation.
/// Avoids coupling to the full DraftPackage type.
#[derive(Debug, Clone)]
pub struct DraftInfo {
    /// Resource URIs of all changed files (e.g., "fs://workspace/tests/foo.rs").
    pub changed_paths: Vec<String>,
    /// Total number of lines changed (added + removed).
    pub lines_changed: usize,
    /// Plan phase this draft is associated with (if any).
    pub plan_phase: Option<String>,
    /// Agent ID that produced this draft.
    pub agent_id: String,
}

/// Result of auto-approval evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum AutoApproveDecision {
    /// All conditions met — draft should be auto-approved.
    Approved {
        /// Audit trail of why each condition passed.
        reasons: Vec<String>,
    },
    /// At least one condition failed — route to human review.
    Denied {
        /// Which conditions failed (included in the review request).
        blockers: Vec<String>,
    },
}

impl AutoApproveDecision {
    pub fn is_approved(&self) -> bool {
        matches!(self, AutoApproveDecision::Approved { .. })
    }
}

/// Evaluate a draft using the rule-based constitution (v0.15.25).
///
/// When `constitution_rules` is non-empty the rules take precedence over
/// the binary `auto_approve.enabled` flag.  Rules are evaluated with
/// first-match-wins semantics per path; the most-restrictive action across
/// all changed paths determines the outcome:
///
/// - `approve` → `Approved` (skips the binary-flag path entirely)
/// - `review`  → `Denied` (routes to human review)
/// - `block`   → `Denied` with a "blocked by constitution rule" message
///
/// The agent security-level check still applies — a `Supervised` or `Strict`
/// agent is always denied regardless of the rules.
///
/// Falls back to [`should_auto_approve_draft`] when `constitution_rules` is empty.
pub fn should_auto_approve_with_rules(
    draft: &DraftInfo,
    doc: &PolicyDocument,
    constitution_rules: &[ApprovalRule],
) -> AutoApproveDecision {
    // Agent security level check always applies.
    let agent_level = doc
        .agents
        .get(&draft.agent_id)
        .and_then(|a| a.security_level)
        .unwrap_or(doc.security_level);
    if agent_level >= SecurityLevel::Supervised {
        return AutoApproveDecision::Denied {
            blockers: vec![format!(
                "agent '{}' security level is {} (requires human review)",
                draft.agent_id, agent_level
            )],
        };
    }

    // If no constitution rules, fall back to binary flag evaluation.
    if constitution_rules.is_empty() {
        return should_auto_approve_draft(draft, doc);
    }

    let bare_paths: Vec<&str> = draft
        .changed_paths
        .iter()
        .map(|p| crate::approval_rules::strip_uri_prefix(p))
        .collect();

    match evaluate_approval_rules(constitution_rules, &bare_paths) {
        None => should_auto_approve_draft(draft, doc),
        Some(decision) => match decision.action {
            ApprovalAction::Approve => {
                let reasons: Vec<String> = decision
                    .reasons
                    .iter()
                    .map(|r| {
                        format!(
                            "constitution rule[{}]: {} → approve ({})",
                            r.matched_rule_index.map_or(-1, |i| i as i64),
                            r.path,
                            r.matched_pattern.as_deref().unwrap_or("no pattern")
                        )
                    })
                    .collect();
                AutoApproveDecision::Approved { reasons }
            }
            ApprovalAction::Block => {
                let blockers: Vec<String> = decision
                    .reasons
                    .iter()
                    .filter(|r| r.action == ApprovalAction::Block)
                    .map(|r| {
                        format!(
                            "path '{}' blocked by constitution rule[{}] (pattern: {})",
                            r.path,
                            r.matched_rule_index.map_or(-1, |i| i as i64),
                            r.matched_pattern.as_deref().unwrap_or("?")
                        )
                    })
                    .collect();
                AutoApproveDecision::Denied { blockers }
            }
            ApprovalAction::Review => AutoApproveDecision::Denied {
                blockers: vec![format!(
                    "constitution rules require human review for {} path(s)",
                    draft.changed_paths.len()
                )],
            },
        },
    }
}

/// Evaluate whether a draft should be auto-approved.
///
/// Checks the project-level policy document, optionally tightened by
/// per-agent overrides. Returns `Denied` immediately if the master
/// switch is off or any condition fails.
pub fn should_auto_approve_draft(draft: &DraftInfo, doc: &PolicyDocument) -> AutoApproveDecision {
    let config = resolve_agent_config(doc, &draft.agent_id);

    let mut reasons = Vec::new();

    // 1. Enabled check.
    if !config.enabled {
        return AutoApproveDecision::Denied {
            blockers: vec!["auto-approval is disabled (drafts.enabled: false)".to_string()],
        };
    }
    reasons.push("enabled: true".to_string());

    // 2. Agent security level check — strict/supervised agents always need review.
    let agent_level = doc
        .agents
        .get(&draft.agent_id)
        .and_then(|a| a.security_level)
        .unwrap_or(doc.security_level);
    if agent_level >= SecurityLevel::Supervised {
        return AutoApproveDecision::Denied {
            blockers: vec![format!(
                "agent '{}' security level is {} (requires human review)",
                draft.agent_id, agent_level
            )],
        };
    }

    let conditions = &config.conditions;
    let file_count = draft.changed_paths.len();

    // 3. Size limits.
    if let Some(max_files) = conditions.max_files {
        if file_count > max_files {
            return AutoApproveDecision::Denied {
                blockers: vec![format!(
                    "files changed ({}) exceeds max_files ({})",
                    file_count, max_files
                )],
            };
        }
        reasons.push(format!("max_files: {} <= {}", file_count, max_files));
    }

    if let Some(max_lines) = conditions.max_lines_changed {
        if draft.lines_changed > max_lines {
            return AutoApproveDecision::Denied {
                blockers: vec![format!(
                    "lines changed ({}) exceeds max_lines_changed ({})",
                    draft.lines_changed, max_lines
                )],
            };
        }
        reasons.push(format!(
            "max_lines_changed: {} <= {}",
            draft.lines_changed, max_lines
        ));
    }

    // 4. Path rules — use AccessFilter (deny takes precedence over allow).
    let path_filter = AccessFilter::new(
        conditions.allowed_paths.clone(),
        conditions.blocked_paths.clone(),
    );
    if !path_filter.is_unrestricted() {
        for path in &draft.changed_paths {
            let bare = strip_uri_prefix(path);
            if !path_filter.permits(bare) {
                // Determine whether it was denied or just not allowed.
                let reason = if conditions
                    .blocked_paths
                    .iter()
                    .any(|p| AccessFilter::from_allowed(vec![p.clone()]).permits(bare))
                {
                    format!("path '{}' matches blocked_paths", bare)
                } else {
                    format!("path '{}' does not match any allowed_paths pattern", bare)
                };
                return AutoApproveDecision::Denied {
                    blockers: vec![reason],
                };
            }
        }
        reasons.push(format!("all {} paths pass access filter", file_count));
    }

    // 5. Phase limits.
    if !conditions.allowed_phases.is_empty() {
        match &draft.plan_phase {
            Some(phase) => {
                if !conditions.allowed_phases.contains(phase) {
                    return AutoApproveDecision::Denied {
                        blockers: vec![format!(
                            "phase '{}' not in allowed_phases {:?}",
                            phase, conditions.allowed_phases
                        )],
                    };
                }
                reasons.push(format!("phase '{}' in allowed_phases", phase));
            }
            None => {
                return AutoApproveDecision::Denied {
                    blockers: vec![
                        "no plan phase set, but allowed_phases is configured".to_string()
                    ],
                };
            }
        }
    }

    // Note: require_tests_pass and require_clean_clippy are not evaluated here.
    // They require command execution, which is handled by the caller before
    // calling this function. If the caller ran them and they failed, it should
    // not call this function (or should add blockers manually).
    if conditions.require_tests_pass {
        reasons.push("require_tests_pass: deferred to caller".to_string());
    }
    if conditions.require_clean_clippy {
        reasons.push("require_clean_clippy: deferred to caller".to_string());
    }

    AutoApproveDecision::Approved { reasons }
}

/// Resolve the effective auto-approve config for an agent.
///
/// If the agent has a per-agent override, tighten the project config
/// with it (most restrictive wins).
fn resolve_agent_config<'a>(
    doc: &'a PolicyDocument,
    agent_id: &str,
) -> std::borrow::Cow<'a, AutoApproveDraftConfig> {
    let project = &doc.defaults.auto_approve.drafts;
    match doc
        .agents
        .get(agent_id)
        .and_then(|a| a.auto_approve.as_ref())
    {
        None => std::borrow::Cow::Borrowed(project),
        Some(agent_cfg) => {
            // Agent config can only tighten. If project is disabled, stay disabled.
            if !project.enabled {
                return std::borrow::Cow::Borrowed(project);
            }
            // If agent explicitly disables, use that.
            if !agent_cfg.enabled {
                return std::borrow::Cow::Owned(AutoApproveDraftConfig {
                    enabled: false,
                    ..project.clone()
                });
            }
            // Merge: take the most restrictive of each field.
            let merged = AutoApproveDraftConfig {
                enabled: project.enabled && agent_cfg.enabled,
                auto_apply: project.auto_apply && agent_cfg.auto_apply,
                git_commit: project.git_commit && agent_cfg.git_commit,
                conditions: merge_conditions(&project.conditions, &agent_cfg.conditions),
            };
            std::borrow::Cow::Owned(merged)
        }
    }
}

/// Merge conditions — take the most restrictive of each.
fn merge_conditions(
    base: &crate::document::AutoApproveConditions,
    overlay: &crate::document::AutoApproveConditions,
) -> crate::document::AutoApproveConditions {
    crate::document::AutoApproveConditions {
        // Take the lower limit.
        max_files: match (base.max_files, overlay.max_files) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        },
        max_lines_changed: match (base.max_lines_changed, overlay.max_lines_changed) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        },
        // Use AccessFilter::tighten for path merge (union denied, intersect allowed).
        blocked_paths: {
            let base_filter =
                AccessFilter::new(base.allowed_paths.clone(), base.blocked_paths.clone());
            let overlay_filter =
                AccessFilter::new(overlay.allowed_paths.clone(), overlay.blocked_paths.clone());
            base_filter.tighten(&overlay_filter).denied
        },
        allowed_paths: {
            let base_filter =
                AccessFilter::new(base.allowed_paths.clone(), base.blocked_paths.clone());
            let overlay_filter =
                AccessFilter::new(overlay.allowed_paths.clone(), overlay.blocked_paths.clone());
            base_filter.tighten(&overlay_filter).allowed
        },
        // Tighten: if either requires, require.
        require_tests_pass: base.require_tests_pass || overlay.require_tests_pass,
        require_clean_clippy: base.require_clean_clippy || overlay.require_clean_clippy,
        test_command: overlay.test_command.clone(),
        lint_command: overlay.lint_command.clone(),
        // Intersection of allowed phases (more restrictive).
        allowed_phases: if base.allowed_phases.is_empty() {
            overlay.allowed_phases.clone()
        } else if overlay.allowed_phases.is_empty() {
            base.allowed_phases.clone()
        } else {
            base.allowed_phases
                .iter()
                .filter(|p| overlay.allowed_phases.contains(p))
                .cloned()
                .collect()
        },
        verification_timeout_secs: base
            .verification_timeout_secs
            .min(overlay.verification_timeout_secs),
    }
}

/// Strip the `fs://workspace/` URI prefix to get a bare path.
fn strip_uri_prefix(uri: &str) -> &str {
    uri.strip_prefix("fs://workspace/").unwrap_or(uri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::*;

    fn make_doc(enabled: bool) -> PolicyDocument {
        let mut doc = PolicyDocument::default();
        doc.defaults.auto_approve.drafts.enabled = enabled;
        doc
    }

    fn make_draft(paths: &[&str]) -> DraftInfo {
        DraftInfo {
            changed_paths: paths.iter().map(|s| s.to_string()).collect(),
            lines_changed: 50,
            plan_phase: None,
            agent_id: "test-agent".to_string(),
        }
    }

    #[test]
    fn disabled_denies() {
        let doc = make_doc(false);
        let draft = make_draft(&["tests/foo.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
    }

    #[test]
    fn enabled_with_no_conditions_approves() {
        let doc = make_doc(true);
        let draft = make_draft(&["tests/foo.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(result.is_approved());
    }

    #[test]
    fn max_files_exceeded_denies() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.max_files = Some(2);
        let draft = make_draft(&["a.rs", "b.rs", "c.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
        if let AutoApproveDecision::Denied { blockers } = result {
            assert!(blockers[0].contains("max_files"));
        }
    }

    #[test]
    fn max_files_within_limit_approves() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.max_files = Some(5);
        let draft = make_draft(&["a.rs", "b.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(result.is_approved());
    }

    #[test]
    fn max_lines_exceeded_denies() {
        let mut doc = make_doc(true);
        doc.defaults
            .auto_approve
            .drafts
            .conditions
            .max_lines_changed = Some(100);
        let mut draft = make_draft(&["a.rs"]);
        draft.lines_changed = 150;
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
    }

    #[test]
    fn blocked_path_denies() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.blocked_paths =
            vec![".ta/**".to_string(), "**/main.rs".to_string()];
        let draft = make_draft(&["fs://workspace/src/main.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
        if let AutoApproveDecision::Denied { blockers } = result {
            assert!(blockers[0].contains("blocked_paths"));
        }
    }

    #[test]
    fn allowed_path_mismatch_denies() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.allowed_paths =
            vec!["tests/**".to_string(), "docs/**".to_string()];
        let draft = make_draft(&["fs://workspace/src/lib.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
        if let AutoApproveDecision::Denied { blockers } = result {
            assert!(blockers[0].contains("allowed_paths"));
        }
    }

    #[test]
    fn allowed_path_match_approves() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.allowed_paths = vec!["tests/**".to_string()];
        let draft = make_draft(&["fs://workspace/tests/foo_test.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(result.is_approved());
    }

    #[test]
    fn blocked_overrides_allowed() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.allowed_paths = vec!["**".to_string()];
        doc.defaults.auto_approve.drafts.conditions.blocked_paths = vec!["**/main.rs".to_string()];
        let draft = make_draft(&["fs://workspace/src/main.rs"]);
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
    }

    #[test]
    fn phase_not_allowed_denies() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.allowed_phases =
            vec!["tests".to_string(), "docs".to_string()];
        let mut draft = make_draft(&["a.rs"]);
        draft.plan_phase = Some("feature".to_string());
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
    }

    #[test]
    fn phase_allowed_approves() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.allowed_phases = vec!["tests".to_string()];
        let mut draft = make_draft(&["a.rs"]);
        draft.plan_phase = Some("tests".to_string());
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(result.is_approved());
    }

    #[test]
    fn supervised_agent_always_denied() {
        let mut doc = make_doc(true);
        doc.agents.insert(
            "strict-agent".to_string(),
            AgentPolicyOverride {
                security_level: Some(SecurityLevel::Supervised),
                ..Default::default()
            },
        );
        let mut draft = make_draft(&["a.rs"]);
        draft.agent_id = "strict-agent".to_string();
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
    }

    #[test]
    fn agent_override_tightens() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.max_files = Some(10);
        doc.agents.insert(
            "tight-agent".to_string(),
            AgentPolicyOverride {
                auto_approve: Some(AutoApproveDraftConfig {
                    enabled: true,
                    conditions: AutoApproveConditions {
                        max_files: Some(3),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let mut draft = make_draft(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        draft.agent_id = "tight-agent".to_string();
        let result = should_auto_approve_draft(&draft, &doc);
        // Agent says max 3, we have 4 files.
        assert!(!result.is_approved());
    }

    #[test]
    fn decision_serialization() {
        let decision = AutoApproveDecision::Approved {
            reasons: vec!["enabled: true".to_string(), "max_files: 3 <= 5".to_string()],
        };
        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains("approved"));
        let restored: AutoApproveDecision = serde_json::from_str(&json).unwrap();
        assert!(restored.is_approved());
    }

    #[test]
    fn strip_uri_prefix_works() {
        assert_eq!(
            strip_uri_prefix("fs://workspace/src/main.rs"),
            "src/main.rs"
        );
        assert_eq!(strip_uri_prefix("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn no_phase_with_allowed_phases_denies() {
        let mut doc = make_doc(true);
        doc.defaults.auto_approve.drafts.conditions.allowed_phases = vec!["tests".to_string()];
        let draft = make_draft(&["a.rs"]); // no plan_phase set
        let result = should_auto_approve_draft(&draft, &doc);
        assert!(!result.is_approved());
    }
}
