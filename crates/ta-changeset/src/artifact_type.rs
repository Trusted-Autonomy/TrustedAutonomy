// artifact_type.rs — ArtifactType enum for workflow I/O declarations (v0.14.10).
//
// Workflow steps declare typed inputs and outputs using this enum. The
// WorkflowEngine resolves the execution DAG from type compatibility — a step
// that outputs `PlanDocument` is automatically wired to any step that accepts
// `PlanDocument` as input. Memory IS the session artifact store.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Typed artifact that a workflow step declares as input or output.
///
/// Used by the WorkflowEngine to resolve execution order automatically from
/// type compatibility — no explicit `depends_on` required when types match.
///
/// # Custom types
/// Any unrecognized string becomes `Custom(string)`. Prefix with `x-` by
/// convention: `inputs = ["x-my-custom-artifact"]`.
///
/// # Example workflow YAML
/// ```yaml
/// stages:
///   - name: generate-plan
///     outputs: [PlanDocument]
///   - name: implement-plan
///     inputs: [PlanDocument]
///     outputs: [DraftPackage]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArtifactType {
    /// A short goal description string (the starting prompt).
    GoalTitle,
    /// A structured plan document (plan items with acceptance criteria).
    PlanDocument,
    /// A TA draft package ready for review.
    DraftPackage,
    /// Pass/fail/flag verdict from a reviewer agent.
    ReviewVerdict,
    /// A single entry in the audit ledger.
    AuditEntry,
    /// Output from a constitution compliance review.
    ConstitutionReport,
    /// A message emitted by or to an agent.
    AgentMessage,
    /// A file or diff artifact (path + content).
    FileArtifact,
    /// Test run results (pass/fail counts, failures).
    TestResult,
    /// User-defined custom artifact type. Prefix with `x-` by convention.
    Custom(String),
}

impl fmt::Display for ArtifactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GoalTitle => write!(f, "GoalTitle"),
            Self::PlanDocument => write!(f, "PlanDocument"),
            Self::DraftPackage => write!(f, "DraftPackage"),
            Self::ReviewVerdict => write!(f, "ReviewVerdict"),
            Self::AuditEntry => write!(f, "AuditEntry"),
            Self::ConstitutionReport => write!(f, "ConstitutionReport"),
            Self::AgentMessage => write!(f, "AgentMessage"),
            Self::FileArtifact => write!(f, "FileArtifact"),
            Self::TestResult => write!(f, "TestResult"),
            Self::Custom(s) => write!(f, "{}", s),
        }
    }
}

impl FromStr for ArtifactType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "GoalTitle" => Self::GoalTitle,
            "PlanDocument" => Self::PlanDocument,
            "DraftPackage" => Self::DraftPackage,
            "ReviewVerdict" => Self::ReviewVerdict,
            "AuditEntry" => Self::AuditEntry,
            "ConstitutionReport" => Self::ConstitutionReport,
            "AgentMessage" => Self::AgentMessage,
            "FileArtifact" => Self::FileArtifact,
            "TestResult" => Self::TestResult,
            other => Self::Custom(other.to_string()),
        })
    }
}

// Serialize as a plain string.
impl Serialize for ArtifactType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

// Deserialize from a plain string.
impl<'de> Deserialize<'de> for ArtifactType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(s.parse().unwrap()) // Infallible
    }
}

impl ArtifactType {
    /// Returns true if this is a user-defined custom type.
    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom(_))
    }

    /// Returns all built-in (non-custom) artifact type variants.
    pub fn built_ins() -> Vec<ArtifactType> {
        vec![
            ArtifactType::GoalTitle,
            ArtifactType::PlanDocument,
            ArtifactType::DraftPackage,
            ArtifactType::ReviewVerdict,
            ArtifactType::AuditEntry,
            ArtifactType::ConstitutionReport,
            ArtifactType::AgentMessage,
            ArtifactType::FileArtifact,
            ArtifactType::TestResult,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_builtin_types() {
        assert_eq!(ArtifactType::PlanDocument.to_string(), "PlanDocument");
        assert_eq!(ArtifactType::DraftPackage.to_string(), "DraftPackage");
        assert_eq!(ArtifactType::ReviewVerdict.to_string(), "ReviewVerdict");
        assert_eq!(ArtifactType::GoalTitle.to_string(), "GoalTitle");
        assert_eq!(ArtifactType::AuditEntry.to_string(), "AuditEntry");
        assert_eq!(
            ArtifactType::ConstitutionReport.to_string(),
            "ConstitutionReport"
        );
        assert_eq!(ArtifactType::AgentMessage.to_string(), "AgentMessage");
        assert_eq!(ArtifactType::FileArtifact.to_string(), "FileArtifact");
        assert_eq!(ArtifactType::TestResult.to_string(), "TestResult");
    }

    #[test]
    fn display_custom_type() {
        assert_eq!(
            ArtifactType::Custom("x-my-thing".to_string()).to_string(),
            "x-my-thing"
        );
    }

    #[test]
    fn from_str_builtin() {
        assert_eq!(
            "PlanDocument".parse::<ArtifactType>().unwrap(),
            ArtifactType::PlanDocument
        );
        assert_eq!(
            "DraftPackage".parse::<ArtifactType>().unwrap(),
            ArtifactType::DraftPackage
        );
        assert_eq!(
            "TestResult".parse::<ArtifactType>().unwrap(),
            ArtifactType::TestResult
        );
    }

    #[test]
    fn from_str_custom_falls_back() {
        let t: ArtifactType = "x-custom-thing".parse().unwrap();
        assert_eq!(t, ArtifactType::Custom("x-custom-thing".to_string()));
        assert!(t.is_custom());
    }

    #[test]
    fn is_custom_builtin_false() {
        assert!(!ArtifactType::PlanDocument.is_custom());
        assert!(!ArtifactType::DraftPackage.is_custom());
    }

    #[test]
    fn serde_roundtrip_builtin() {
        let t = ArtifactType::ReviewVerdict;
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"ReviewVerdict\"");
        let back: ArtifactType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn serde_roundtrip_custom() {
        let t = ArtifactType::Custom("x-special".to_string());
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"x-special\"");
        let back: ArtifactType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn serde_in_vec() {
        let types = vec![
            ArtifactType::PlanDocument,
            ArtifactType::DraftPackage,
            ArtifactType::Custom("x-custom".to_string()),
        ];
        let json = serde_json::to_string(&types).unwrap();
        assert_eq!(json, r#"["PlanDocument","DraftPackage","x-custom"]"#);
        let back: Vec<ArtifactType> = serde_json::from_str(&json).unwrap();
        assert_eq!(types, back);
    }

    #[test]
    fn built_ins_count() {
        assert_eq!(ArtifactType::built_ins().len(), 9);
    }

    #[test]
    fn built_ins_none_are_custom() {
        for t in ArtifactType::built_ins() {
            assert!(!t.is_custom(), "{} should not be custom", t);
        }
    }
}
