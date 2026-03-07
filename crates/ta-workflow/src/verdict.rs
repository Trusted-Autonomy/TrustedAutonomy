// verdict.rs — Verdict schema and feedback types.

use serde::{Deserialize, Serialize};

/// Severity classification for findings and routing decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Minor,
    Major,
    Critical,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Minor => write!(f, "minor"),
            Severity::Major => write!(f, "major"),
            Severity::Critical => write!(f, "critical"),
        }
    }
}

/// A verdict decision from a reviewer role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictDecision {
    Pass,
    Fail,
    Conditional,
}

/// A verdict from a single reviewer role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// Which role produced this verdict.
    pub role: String,
    /// The decision.
    pub decision: VerdictDecision,
    /// Overall severity of findings.
    pub severity: Option<Severity>,
    /// Detailed findings.
    #[serde(default)]
    pub findings: Vec<Finding>,
}

/// A single finding within a verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Short title of the finding.
    pub title: String,
    /// Detailed description.
    pub description: String,
    /// Severity of this finding.
    pub severity: Severity,
    /// Category (e.g., "security", "performance", "style").
    #[serde(default)]
    pub category: Option<String>,
}

impl Verdict {
    /// Check if this verdict passes.
    pub fn is_pass(&self) -> bool {
        self.decision == VerdictDecision::Pass
    }

    /// Check if this verdict has any critical findings.
    pub fn has_critical(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.severity == Severity::Critical)
    }
}

/// Compute an aggregate score from a set of verdicts.
///
/// Pass = 1.0, Conditional = 0.5, Fail = 0.0.
/// Score is the average across all verdicts.
pub fn aggregate_score(verdicts: &[Verdict]) -> f64 {
    if verdicts.is_empty() {
        return 1.0;
    }
    let total: f64 = verdicts
        .iter()
        .map(|v| match v.decision {
            VerdictDecision::Pass => 1.0,
            VerdictDecision::Conditional => 0.5,
            VerdictDecision::Fail => 0.0,
        })
        .sum();
    total / verdicts.len() as f64
}

/// Check if all required roles have passed.
pub fn required_roles_pass(verdicts: &[Verdict], required: &[String]) -> bool {
    for role in required {
        let passed = verdicts
            .iter()
            .any(|v| v.role == *role && v.decision == VerdictDecision::Pass);
        if !passed {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_serialization() {
        let v = Verdict {
            role: "security".to_string(),
            decision: VerdictDecision::Fail,
            severity: Some(Severity::Critical),
            findings: vec![Finding {
                title: "SQL injection".to_string(),
                description: "User input not sanitized".to_string(),
                severity: Severity::Critical,
                category: Some("security".to_string()),
            }],
        };
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("\"decision\":\"fail\""));
        assert!(json.contains("SQL injection"));
        let restored: Verdict = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.role, "security");
        assert!(restored.has_critical());
    }

    #[test]
    fn aggregate_score_all_pass() {
        let verdicts = vec![
            Verdict {
                role: "a".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
            Verdict {
                role: "b".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
        ];
        assert_eq!(aggregate_score(&verdicts), 1.0);
    }

    #[test]
    fn aggregate_score_mixed() {
        let verdicts = vec![
            Verdict {
                role: "a".to_string(),
                decision: VerdictDecision::Pass,
                severity: None,
                findings: vec![],
            },
            Verdict {
                role: "b".to_string(),
                decision: VerdictDecision::Fail,
                severity: None,
                findings: vec![],
            },
        ];
        assert_eq!(aggregate_score(&verdicts), 0.5);
    }

    #[test]
    fn aggregate_score_empty() {
        assert_eq!(aggregate_score(&[]), 1.0);
    }

    #[test]
    fn required_roles_pass_check() {
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
                severity: None,
                findings: vec![],
            },
        ];
        assert!(required_roles_pass(&verdicts, &["security".to_string()]));
        assert!(!required_roles_pass(&verdicts, &["style".to_string()]));
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Minor < Severity::Major);
        assert!(Severity::Major < Severity::Critical);
    }
}
