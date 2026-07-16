//! Workload classification: infer a workload type (`"bugfix"`, `"docs"`,
//! `"feature"`, `"refactor"`, `"test"`, `"release"`, `"security"`,
//! `"chore"`) from free text (a goal title/objective, or a triggered
//! event's payload) when no explicit `--workload` override is given.
//!
//! Keyword-heuristic, matching the style of `ta_advisor::classify`'s intent
//! classifier rather than inventing a new classification approach. Not a
//! machine-learning model — deliberately simple and auditable, since its
//! output gates the `security_tier = "auto"` resolution in `route()`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkloadClassification {
    pub workload_type: String,
    /// Confidence in `[0.0, 1.0]`. `1.0` when the workload type was given
    /// explicitly rather than inferred.
    pub confidence: f32,
}

const CATEGORIES: &[(&str, &[&str])] = &[
    (
        "security",
        &[
            "security",
            "vulnerability",
            "cve",
            "exploit",
            "auth bypass",
            "credential leak",
        ],
    ),
    (
        "bugfix",
        &[
            "bug",
            "fix",
            "broken",
            "regression",
            "crash",
            "failing test",
        ],
    ),
    (
        "release",
        &["release", "publish", "version bump", "changelog", "tag v"],
    ),
    (
        "docs",
        &["docs", "documentation", "readme", "guide", "usage.md"],
    ),
    (
        "test",
        &["add test", "test coverage", "unit test", "integration test"],
    ),
    (
        "refactor",
        &["refactor", "cleanup", "clean up", "simplify", "extract"],
    ),
    (
        "chore",
        &["chore", "bump dependency", "housekeeping", "rename"],
    ),
    (
        "feature",
        &["add", "implement", "build", "new feature", "support for"],
    ),
];

/// Classify free text into a workload type. Falls back to `"other"` at low
/// confidence when nothing matches.
pub fn classify_workload(text: &str) -> WorkloadClassification {
    let lower = text.to_ascii_lowercase();

    for (category, keywords) in CATEGORIES {
        for kw in *keywords {
            if lower.contains(kw) {
                // Longer/more-specific keyword matches are more confident;
                // clamp to a sane [0.6, 0.95] band so classification is
                // never treated as certain enough to skip logging.
                let confidence = (0.6 + (kw.len() as f32 / 40.0)).min(0.95);
                return WorkloadClassification {
                    workload_type: category.to_string(),
                    confidence,
                };
            }
        }
    }

    WorkloadClassification {
        workload_type: "other".to_string(),
        confidence: 0.3,
    }
}

/// An explicit workload type (from `--workload` or a config binding) is
/// always maximally confident — it's a declaration, not an inference.
pub(crate) fn explicit(workload_type: impl Into<String>) -> WorkloadClassification {
    WorkloadClassification {
        workload_type: workload_type.into(),
        confidence: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_bugfix() {
        let c = classify_workload("Fix the flaky login test");
        assert_eq!(c.workload_type, "bugfix");
    }

    #[test]
    fn classifies_docs() {
        let c = classify_workload("Update the README documentation");
        assert_eq!(c.workload_type, "docs");
    }

    #[test]
    fn classifies_security_before_bugfix() {
        // "vulnerability" and "fix" both appear; security must win since
        // it's checked first — security misclassification has higher cost.
        let c = classify_workload("Fix the auth bypass vulnerability");
        assert_eq!(c.workload_type, "security");
    }

    #[test]
    fn classifies_feature() {
        let c = classify_workload("Implement OAuth2 support for the API");
        assert_eq!(c.workload_type, "feature");
    }

    #[test]
    fn unmatched_text_falls_back_to_other_with_low_confidence() {
        let c = classify_workload("xyzzy plugh");
        assert_eq!(c.workload_type, "other");
        assert!(c.confidence < 0.5);
    }

    #[test]
    fn explicit_workload_is_fully_confident() {
        let c = explicit("bugfix");
        assert_eq!(c.confidence, 1.0);
    }
}
