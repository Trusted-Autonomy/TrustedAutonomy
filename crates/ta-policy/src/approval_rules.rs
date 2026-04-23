// approval_rules.rs — Rule-based auto-approve constitution (v0.15.25).
//
// Replaces the binary `auto_approve.enabled = true/false` with an ordered
// list of glob-pattern rules. First-match-wins evaluation over all changed
// paths; most-restrictive action wins across files.
//
// Configuration lives in `.ta/constitution.toml` under `[[approval_rules]]`:
//
// ```toml
// [[approval_rules]]
// patterns = ["docs/**", "*.md"]
// action   = "approve"
//
// [[approval_rules]]
// patterns = ["src/auth/**", "*_token*", "*.pem", "*.key"]
// action   = "block"
//
// [[approval_rules]]
// patterns = ["**"]
// action   = "review"
// ```
//
// Amendment flow: `ta constitution amend` stages the constitution file as a
// draft. Changes take effect only after `ta draft apply` — no silent policy
// updates.

use serde::{Deserialize, Serialize};

/// Action assigned to a set of path patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAction {
    /// Auto-approve immediately — no human review needed.
    Approve,
    /// Route to human review (default for unmatched paths).
    Review,
    /// Block unconditionally — cannot be approved without policy change.
    Block,
}

impl std::fmt::Display for ApprovalAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApprovalAction::Approve => write!(f, "approve"),
            ApprovalAction::Review => write!(f, "review"),
            ApprovalAction::Block => write!(f, "block"),
        }
    }
}

/// A single rule in the auto-approve constitution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRule {
    /// Glob patterns. A path matches this rule when it matches ANY pattern.
    pub patterns: Vec<String>,

    /// What happens when a path matches this rule.
    pub action: ApprovalAction,

    /// Optional human-readable label for display and audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Outcome of evaluating approval rules against a set of changed paths.
#[derive(Debug, Clone)]
pub struct ApprovalRuleDecision {
    /// The most-restrictive action across all changed paths.
    pub action: ApprovalAction,
    /// Per-path reasons for the audit trail.
    pub reasons: Vec<ApprovalReason>,
}

/// Explains why a single path received its action.
#[derive(Debug, Clone)]
pub struct ApprovalReason {
    pub path: String,
    pub action: ApprovalAction,
    /// Which rule matched (index into the rules slice, 0-based).
    pub matched_rule_index: Option<usize>,
    /// The pattern that matched.
    pub matched_pattern: Option<String>,
}

/// Warning about overlapping patterns in the rule set.
#[derive(Debug, Clone)]
pub struct OverlapWarning {
    /// Index of the earlier (higher-priority) rule that will always shadow the later one.
    pub shadowing_rule: usize,
    /// Index of the later rule that can never be reached for the given pattern.
    pub shadowed_rule: usize,
    /// The pattern that is shadowed.
    pub pattern: String,
}

impl std::fmt::Display for OverlapWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rule[{}] pattern {:?} is always shadowed by rule[{}] (first-match-wins)",
            self.shadowed_rule, self.pattern, self.shadowing_rule
        )
    }
}

/// Evaluate approval rules against a set of changed paths.
///
/// Algorithm:
///  - For each path, iterate rules in order; the first rule whose patterns
///    match the path determines that path's action (first-match-wins).
///  - Unmatched paths default to `Review`.
///  - The overall decision is the most-restrictive action across all paths
///    (Block > Review > Approve).
///
/// Returns [`None`] when `rules` is empty (caller should fall back to the
/// binary `auto_approve` config).
pub fn evaluate_approval_rules(
    rules: &[ApprovalRule],
    changed_paths: &[&str],
) -> Option<ApprovalRuleDecision> {
    if rules.is_empty() {
        return None;
    }

    let mut reasons = Vec::with_capacity(changed_paths.len());
    let mut worst = ApprovalAction::Approve;

    for &path in changed_paths {
        let bare = strip_uri_prefix(path);
        let reason = match_path_against_rules(rules, bare);
        if reason.action > worst {
            worst = reason.action;
        }
        reasons.push(reason);
    }

    // If no files changed, default to review (safe).
    if changed_paths.is_empty() {
        worst = ApprovalAction::Review;
    }

    Some(ApprovalRuleDecision {
        action: worst,
        reasons,
    })
}

/// Check a single bare path against the ordered rule list.
fn match_path_against_rules(rules: &[ApprovalRule], bare_path: &str) -> ApprovalReason {
    for (idx, rule) in rules.iter().enumerate() {
        if let Some(matched_pattern) = first_matching_pattern(&rule.patterns, bare_path) {
            return ApprovalReason {
                path: bare_path.to_string(),
                action: rule.action,
                matched_rule_index: Some(idx),
                matched_pattern: Some(matched_pattern),
            };
        }
    }
    // No rule matched — safe default is review.
    ApprovalReason {
        path: bare_path.to_string(),
        action: ApprovalAction::Review,
        matched_rule_index: None,
        matched_pattern: None,
    }
}

/// Return the first pattern in `patterns` that matches `path`, or None.
fn first_matching_pattern(patterns: &[String], path: &str) -> Option<String> {
    for pattern in patterns {
        let opts = glob::MatchOptions {
            require_literal_separator: true,
            case_sensitive: true,
            require_literal_leading_dot: false,
        };
        if let Ok(p) = glob::Pattern::new(pattern) {
            if p.matches_with(path, opts) {
                return Some(pattern.clone());
            }
        }
        // Also try without require_literal_separator for single-component globs
        // like "*.md" so they match "README.md" at the top level.  Both the
        // pattern AND the path must be separator-free: if the path contains a
        // '/' (e.g. "src/auth/notes.md") a flat pattern must NOT match it,
        // otherwise rule ordering can be defeated (*.md approve fires before
        // src/auth/** block).
        let opts_flat = glob::MatchOptions {
            require_literal_separator: false,
            case_sensitive: true,
            require_literal_leading_dot: false,
        };
        if let Ok(p) = glob::Pattern::new(pattern) {
            if p.matches_with(path, opts_flat) && !pattern.contains('/') && !path.contains('/') {
                return Some(pattern.clone());
            }
        }
    }
    None
}

/// Validate the rule set and return warnings about shadowed patterns.
///
/// A pattern in rule N is shadowed when a pattern in rule M (M < N) is a
/// superset of it — meaning rule N can never be reached for that pattern.
///
/// This is a best-effort heuristic: exact-match shadowing (identical patterns)
/// is always detected. Glob-superset detection uses test paths derived from
/// the shadowed pattern.
pub fn validate_approval_rules(rules: &[ApprovalRule]) -> Vec<OverlapWarning> {
    let mut warnings = Vec::new();

    for (later_idx, later_rule) in rules.iter().enumerate() {
        for later_pattern in &later_rule.patterns {
            // Check if any earlier rule's patterns subsume this pattern.
            for (earlier_idx, earlier_rule) in rules.iter().enumerate().take(later_idx) {
                if earlier_rule
                    .patterns
                    .iter()
                    .any(|ep| pattern_subsumes(ep, later_pattern))
                {
                    warnings.push(OverlapWarning {
                        shadowing_rule: earlier_idx,
                        shadowed_rule: later_idx,
                        pattern: later_pattern.clone(),
                    });
                    break;
                }
            }
        }
    }

    warnings
}

/// Heuristic: does `wider` subsume `narrower`?
///
/// Exact match is always true. Glob-level subsumption is checked by testing
/// whether `narrower` itself (used as a path) would match `wider`.
fn pattern_subsumes(wider: &str, narrower: &str) -> bool {
    if wider == narrower {
        return true;
    }
    // "**" matches everything.
    if wider == "**" {
        return true;
    }
    // If the narrower pattern has no glob characters, treat it as a literal
    // path and check whether the wider glob matches it.
    if !narrower.contains('*') && !narrower.contains('?') && !narrower.contains('[') {
        return glob_match(wider, narrower);
    }
    // For overlapping globs (e.g. "src/**" vs "src/auth/**"), generate a
    // representative sample path from the narrower pattern and check whether
    // the wider pattern matches it.
    let sample = derive_sample_path(narrower);
    glob_match(wider, &sample)
}

/// Derive a concrete sample path from a glob pattern for subsumption testing.
fn derive_sample_path(pattern: &str) -> String {
    pattern
        .replace("**", "a/b")
        .replace('*', "x")
        .replace('?', "c")
        .replace(['[', ']'], "")
}

fn glob_match(pattern: &str, target: &str) -> bool {
    let opts = glob::MatchOptions {
        require_literal_separator: true,
        case_sensitive: true,
        require_literal_leading_dot: false,
    };
    glob::Pattern::new(pattern)
        .map(|p| p.matches_with(target, opts))
        .unwrap_or(false)
}

/// Strip the `fs://workspace/` URI prefix to get a bare path.
pub fn strip_uri_prefix(uri: &str) -> &str {
    uri.strip_prefix("fs://workspace/").unwrap_or(uri)
}

/// Default approval rules shipped with new projects.
///
/// - Doc-only files: auto-approve
/// - Security-sensitive files: block
/// - Everything else: require review
pub fn default_approval_rules() -> Vec<ApprovalRule> {
    vec![
        ApprovalRule {
            patterns: vec!["docs/**".to_string(), "*.md".to_string()],
            action: ApprovalAction::Approve,
            label: Some("Documentation files".to_string()),
        },
        ApprovalRule {
            patterns: vec![
                "src/auth/**".to_string(),
                "*_token*".to_string(),
                "**/*_token*".to_string(),
                "*.pem".to_string(),
                "**/*.pem".to_string(),
                "*.key".to_string(),
                "**/*.key".to_string(),
                "*.pfx".to_string(),
                "**/*.pfx".to_string(),
                "*.p12".to_string(),
                "**/*.p12".to_string(),
                ".env".to_string(),
                ".env.*".to_string(),
                "**/.env".to_string(),
                "**/.env.*".to_string(),
            ],
            action: ApprovalAction::Block,
            label: Some("Security-sensitive files".to_string()),
        },
        ApprovalRule {
            patterns: vec!["**".to_string()],
            action: ApprovalAction::Review,
            label: Some("All other files (default)".to_string()),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rules() -> Vec<ApprovalRule> {
        default_approval_rules()
    }

    // ── evaluate_approval_rules ──

    #[test]
    fn empty_rules_returns_none() {
        let result = evaluate_approval_rules(&[], &["src/main.rs"]);
        assert!(result.is_none());
    }

    #[test]
    fn doc_file_approves() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["docs/guide.md"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Approve);
        assert_eq!(result.reasons[0].action, ApprovalAction::Approve);
        assert_eq!(result.reasons[0].matched_rule_index, Some(0));
    }

    #[test]
    fn md_file_at_root_approves() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["README.md"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Approve);
    }

    #[test]
    fn auth_file_blocks() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["src/auth/login.rs"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Block);
    }

    #[test]
    fn md_file_in_blocked_dir_is_blocked_not_approved() {
        // *.md (approve) must NOT beat src/auth/** (block) when the file lives
        // inside the blocked directory.  Without the path.contains('/') guard
        // the flat-mode pass would match src/auth/notes.md against *.md and
        // return Approve before reaching the Block rule.
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["src/auth/notes.md"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Block);
    }

    #[test]
    fn pem_file_blocks() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["certs/server.pem"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Block);
    }

    #[test]
    fn token_file_blocks() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["config/access_token.json"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Block);
    }

    #[test]
    fn src_file_reviews() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["src/main.rs"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Review);
    }

    #[test]
    fn unmatched_path_reviews() {
        let rules = vec![ApprovalRule {
            patterns: vec!["docs/**".to_string()],
            action: ApprovalAction::Approve,
            label: None,
        }];
        let result = evaluate_approval_rules(&rules, &["src/main.rs"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Review);
        assert!(result.reasons[0].matched_rule_index.is_none());
    }

    #[test]
    fn most_restrictive_wins_across_files() {
        let rules = make_rules();
        // Mix of doc (approve) + auth (block) → block wins.
        let paths = &["docs/guide.md", "src/auth/secret.rs"];
        let result = evaluate_approval_rules(&rules, paths).unwrap();
        assert_eq!(result.action, ApprovalAction::Block);
    }

    #[test]
    fn most_restrictive_review_beats_approve() {
        let rules = make_rules();
        let paths = &["docs/guide.md", "src/lib.rs"];
        let result = evaluate_approval_rules(&rules, paths).unwrap();
        assert_eq!(result.action, ApprovalAction::Review);
    }

    #[test]
    fn uri_prefix_stripped_before_matching() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &["fs://workspace/docs/guide.md"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Approve);
    }

    #[test]
    fn empty_changed_paths_defaults_to_review() {
        let rules = make_rules();
        let result = evaluate_approval_rules(&rules, &[]).unwrap();
        assert_eq!(result.action, ApprovalAction::Review);
    }

    #[test]
    fn first_match_wins_order() {
        // Rule 0: approve docs/**; Rule 1: block ** — docs should still approve.
        let rules = vec![
            ApprovalRule {
                patterns: vec!["docs/**".to_string()],
                action: ApprovalAction::Approve,
                label: None,
            },
            ApprovalRule {
                patterns: vec!["**".to_string()],
                action: ApprovalAction::Block,
                label: None,
            },
        ];
        let result = evaluate_approval_rules(&rules, &["docs/guide.md"]).unwrap();
        assert_eq!(result.action, ApprovalAction::Approve);
        assert_eq!(result.reasons[0].matched_rule_index, Some(0));
    }

    // ── validate_approval_rules ──

    #[test]
    fn no_warnings_for_disjoint_rules() {
        let rules = vec![
            ApprovalRule {
                patterns: vec!["docs/**".to_string()],
                action: ApprovalAction::Approve,
                label: None,
            },
            ApprovalRule {
                patterns: vec!["src/**".to_string()],
                action: ApprovalAction::Review,
                label: None,
            },
        ];
        let warnings = validate_approval_rules(&rules);
        assert!(warnings.is_empty());
    }

    #[test]
    fn catchall_shadows_later_rules() {
        let rules = vec![
            ApprovalRule {
                patterns: vec!["**".to_string()],
                action: ApprovalAction::Review,
                label: None,
            },
            ApprovalRule {
                patterns: vec!["docs/**".to_string()],
                action: ApprovalAction::Approve,
                label: None,
            },
        ];
        let warnings = validate_approval_rules(&rules);
        assert!(!warnings.is_empty());
        assert_eq!(warnings[0].shadowing_rule, 0);
        assert_eq!(warnings[0].shadowed_rule, 1);
        assert_eq!(warnings[0].pattern, "docs/**");
    }

    #[test]
    fn identical_pattern_is_shadowed() {
        let rules = vec![
            ApprovalRule {
                patterns: vec!["src/auth/**".to_string()],
                action: ApprovalAction::Block,
                label: None,
            },
            ApprovalRule {
                patterns: vec!["src/auth/**".to_string()],
                action: ApprovalAction::Review,
                label: None,
            },
        ];
        let warnings = validate_approval_rules(&rules);
        assert!(!warnings.is_empty());
        assert_eq!(warnings[0].shadowed_rule, 1);
    }

    #[test]
    fn no_warnings_for_single_rule() {
        let rules = vec![ApprovalRule {
            patterns: vec!["**".to_string()],
            action: ApprovalAction::Review,
            label: None,
        }];
        let warnings = validate_approval_rules(&rules);
        assert!(warnings.is_empty());
    }

    // ── ApprovalAction ordering ──

    #[test]
    fn action_ordering() {
        assert!(ApprovalAction::Approve < ApprovalAction::Review);
        assert!(ApprovalAction::Review < ApprovalAction::Block);
    }

    // ── Serialization ──

    #[test]
    fn rule_yaml_round_trip() {
        let rule = ApprovalRule {
            patterns: vec!["docs/**".to_string(), "*.md".to_string()],
            action: ApprovalAction::Approve,
            label: Some("Documentation".to_string()),
        };
        let yaml = serde_yaml::to_string(&rule).unwrap();
        let restored: ApprovalRule = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(restored.patterns, rule.patterns);
        assert_eq!(restored.action, ApprovalAction::Approve);
        assert_eq!(restored.label, rule.label);
    }

    #[test]
    fn action_serializes_as_snake_case() {
        let yaml = serde_yaml::to_string(&ApprovalAction::Approve).unwrap();
        assert!(yaml.contains("approve"));
        let yaml = serde_yaml::to_string(&ApprovalAction::Block).unwrap();
        assert!(yaml.contains("block"));
    }

    #[test]
    fn default_rules_compile_and_cover_examples() {
        let rules = default_approval_rules();
        assert!(!rules.is_empty());
        // Doc → approve
        let r = evaluate_approval_rules(&rules, &["docs/README.md"]).unwrap();
        assert_eq!(r.action, ApprovalAction::Approve);
        // Auth → block
        let r = evaluate_approval_rules(&rules, &["src/auth/token.rs"]).unwrap();
        assert_eq!(r.action, ApprovalAction::Block);
        // Generic src → review
        let r = evaluate_approval_rules(&rules, &["src/lib.rs"]).unwrap();
        assert_eq!(r.action, ApprovalAction::Review);
    }

    #[test]
    fn strip_uri_prefix_works() {
        assert_eq!(
            strip_uri_prefix("fs://workspace/src/main.rs"),
            "src/main.rs"
        );
        assert_eq!(strip_uri_prefix("src/main.rs"), "src/main.rs");
    }
}
