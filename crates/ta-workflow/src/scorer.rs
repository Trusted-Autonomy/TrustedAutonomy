// scorer.rs — Feedback scoring agent integration.
//
// The scorer aggregates multiple review verdicts into a single assessment
// with routing recommendations. For now this uses the built-in aggregate_score
// function. In a future version, the scorer can delegate to an LLM agent.

use crate::definition::VerdictConfig;
use crate::verdict::{aggregate_score, required_roles_pass, Finding, Severity, Verdict};

use serde::{Deserialize, Serialize};

/// Result of scoring a set of verdicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringResult {
    /// Aggregate score (0.0-1.0).
    pub score: f64,
    /// Overall severity classification.
    pub severity: Severity,
    /// Whether the stage passes based on threshold and required roles.
    pub passes: bool,
    /// Routing recommendation (stage to route back to, if any).
    pub route_to: Option<String>,
    /// Synthesized feedback for the next iteration.
    pub feedback: String,
    /// All findings collected from verdicts.
    pub findings: Vec<Finding>,
}

/// Score a set of verdicts against a verdict configuration.
///
/// Uses the built-in scoring algorithm:
/// - Aggregate score = average of verdict scores (Pass=1, Conditional=0.5, Fail=0)
/// - Passes if score >= threshold AND all required roles pass
/// - Severity = worst finding severity, or Minor if no findings
pub fn score_verdicts(
    verdicts: &[Verdict],
    config: &VerdictConfig,
    failure_route: Option<&str>,
) -> ScoringResult {
    let score = aggregate_score(verdicts);
    let required_ok = required_roles_pass(verdicts, &config.required_pass);
    let passes = score >= config.pass_threshold && required_ok;

    // Collect all findings.
    let findings: Vec<Finding> = verdicts
        .iter()
        .flat_map(|v| v.findings.iter().cloned())
        .collect();

    // Determine worst severity.
    let severity = findings
        .iter()
        .map(|f| f.severity.clone())
        .max()
        .unwrap_or(Severity::Minor);

    // Build feedback summary.
    let feedback = if passes {
        format!(
            "All checks passed (score: {:.2}, threshold: {:.2}).",
            score, config.pass_threshold
        )
    } else {
        let mut parts = Vec::new();
        if score < config.pass_threshold {
            parts.push(format!(
                "Aggregate score {:.2} below threshold {:.2}",
                score, config.pass_threshold
            ));
        }
        if !required_ok {
            let failed: Vec<&str> = config
                .required_pass
                .iter()
                .filter(|r| !verdicts.iter().any(|v| v.role == **r && v.is_pass()))
                .map(|s| s.as_str())
                .collect();
            parts.push(format!(
                "Required roles did not pass: {}",
                failed.join(", ")
            ));
        }
        let finding_summary: Vec<String> = findings
            .iter()
            .map(|f| format!("[{}] {}: {}", f.severity, f.title, f.description))
            .collect();
        if !finding_summary.is_empty() {
            parts.push(format!("Findings:\n{}", finding_summary.join("\n")));
        }
        parts.join("\n")
    };

    let route_to = if !passes {
        failure_route.map(|s| s.to_string())
    } else {
        None
    };

    ScoringResult {
        score,
        severity,
        passes,
        route_to,
        feedback,
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::VerdictDecision;

    fn make_config() -> VerdictConfig {
        VerdictConfig {
            scorer: None,
            pass_threshold: 0.7,
            required_pass: vec!["security".to_string()],
        }
    }

    #[test]
    fn scoring_all_pass() {
        let verdicts = vec![
            Verdict {
                role: "security".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
            Verdict {
                role: "style".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
        ];
        let result = score_verdicts(&verdicts, &make_config(), Some("build"));
        assert!(result.passes);
        assert_eq!(result.score, 1.0);
        assert!(result.route_to.is_none());
    }

    #[test]
    fn scoring_below_threshold() {
        let verdicts = vec![
            Verdict {
                role: "security".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
            Verdict {
                role: "style".to_string(),
                decision: VerdictDecision::Fail,
                severity: Some(Severity::Major),
                findings: vec![Finding {
                    title: "Bad formatting".to_string(),
                    description: "Inconsistent style".to_string(),
                    severity: Severity::Major,
                    category: Some("style".to_string()),
                }],
            },
        ];
        let result = score_verdicts(&verdicts, &make_config(), Some("build"));
        assert!(!result.passes);
        assert_eq!(result.score, 0.5);
        assert_eq!(result.route_to, Some("build".to_string()));
    }

    #[test]
    fn scoring_required_role_fails() {
        let verdicts = vec![
            Verdict {
                role: "security".to_string(),
                decision: VerdictDecision::Fail,
                severity: Some(Severity::Critical),
                findings: vec![],
            },
            Verdict {
                role: "style".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
        ];
        let result = score_verdicts(&verdicts, &make_config(), Some("build"));
        assert!(!result.passes); // security is required
    }

    #[test]
    fn scoring_feedback_includes_findings() {
        let verdicts = vec![Verdict {
            role: "security".to_string(),
            decision: VerdictDecision::Fail,
            severity: Some(Severity::Critical),
            findings: vec![Finding {
                title: "SQL injection".to_string(),
                description: "Unescaped input".to_string(),
                severity: Severity::Critical,
                category: Some("security".to_string()),
            }],
        }];
        let result = score_verdicts(&verdicts, &make_config(), None);
        assert!(result.feedback.contains("SQL injection"));
    }
}
