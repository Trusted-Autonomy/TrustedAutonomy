use serde::{Deserialize, Serialize};
use std::path::Path;

/// A decision made by the work planner agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkPlanDecision {
    pub decision: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_affected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// A single implementation step from the work planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementationStep {
    pub step: u32,
    pub file: String,
    pub action: String,
    #[serde(default)]
    pub detail: String,
}

/// The work plan produced by the planner agent.
///
/// Written to `.ta/work-plan.json` in the staging workspace.
/// Read by the implementor agent via CLAUDE.md injection.
/// Merged into agent_decision_log at draft build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkPlan {
    pub goal: String,
    pub decisions: Vec<WorkPlanDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implementation_plan: Vec<ImplementationStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub out_of_scope: Vec<String>,
}

impl WorkPlan {
    /// Load a work plan from `.ta/work-plan.json` in a staging directory.
    pub fn load(staging_path: &Path) -> anyhow::Result<Self> {
        let path = staging_path.join(".ta").join("work-plan.json");
        let content = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!("Failed to read work-plan.json at {}: {}", path.display(), e)
        })?;
        let plan: Self = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse work-plan.json: {}", e))?;
        Ok(plan)
    }

    /// Load a work plan from an explicit file path.
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("Failed to read work-plan.json at {}: {}", path.display(), e)
        })?;
        let plan: Self = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse work-plan.json: {}", e))?;
        Ok(plan)
    }

    /// Validate the work plan: decisions must be non-empty, each must have a rationale.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.decisions.is_empty() {
            anyhow::bail!(
                "Work plan has no decisions. The planner agent must record at least one \
                 design decision with rationale before the implementor can proceed."
            );
        }
        for (i, d) in self.decisions.iter().enumerate() {
            if d.rationale.trim().is_empty() {
                anyhow::bail!(
                    "Work plan decision {} ('{}') has no rationale. Each decision must \
                     explain why the approach was chosen.",
                    i + 1,
                    d.decision
                );
            }
        }
        Ok(())
    }

    /// Format as a CLAUDE.md section for injection into the implementor's context.
    pub fn to_claude_md_section(&self) -> String {
        let mut out = String::from("\n## Implementation Plan\n\n");
        out.push_str("The work planner has analyzed this goal and produced the following plan. ");
        out.push_str(
            "Execute it faithfully. Do not redesign. If you encounter a blocker, write it to \
             `.ta/work-plan-blockers.json` and exit.\n\n",
        );

        out.push_str("### Design Decisions\n\n");
        for (i, d) in self.decisions.iter().enumerate() {
            out.push_str(&format!("{}. **{}**\n", i + 1, d.decision));
            out.push_str(&format!("   - Rationale: {}\n", d.rationale));
            if !d.alternatives.is_empty() {
                out.push_str(&format!(
                    "   - Alternatives considered: {}\n",
                    d.alternatives.join(", ")
                ));
            }
            if !d.files_affected.is_empty() {
                out.push_str(&format!(
                    "   - Files affected: {}\n",
                    d.files_affected.join(", ")
                ));
            }
            if let Some(c) = d.confidence {
                out.push_str(&format!("   - Confidence: {:.0}%\n", c * 100.0));
            }
        }

        if !self.implementation_plan.is_empty() {
            out.push_str("\n### Implementation Steps\n\n");
            for step in &self.implementation_plan {
                out.push_str(&format!(
                    "{}. `{}` — {}\n",
                    step.step, step.file, step.action
                ));
                if !step.detail.is_empty() {
                    out.push_str(&format!("   {}\n", step.detail));
                }
            }
        }

        if !self.out_of_scope.is_empty() {
            out.push_str("\n### Out of Scope\n\n");
            for item in &self.out_of_scope {
                out.push_str(&format!("- {}\n", item));
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_plan() -> WorkPlan {
        WorkPlan {
            goal: "Add JWT auth".to_string(),
            decisions: vec![WorkPlanDecision {
                decision: "Use RS256 for JWT signing".to_string(),
                rationale: "RS256 allows public key distribution without sharing the secret"
                    .to_string(),
                alternatives: vec!["HS256".to_string()],
                files_affected: vec!["src/auth.rs".to_string()],
                confidence: Some(0.9),
            }],
            implementation_plan: vec![ImplementationStep {
                step: 1,
                file: "src/auth.rs".to_string(),
                action: "add JwtValidator struct".to_string(),
                detail: "RS256, validates exp and iss claims".to_string(),
            }],
            out_of_scope: vec!["OAuth2 provider integration".to_string()],
        }
    }

    #[test]
    fn work_plan_validates_ok() {
        assert!(sample_plan().validate().is_ok());
    }

    #[test]
    fn work_plan_validate_empty_decisions() {
        let plan = WorkPlan {
            goal: "test".to_string(),
            decisions: vec![],
            implementation_plan: vec![],
            out_of_scope: vec![],
        };
        assert!(plan.validate().is_err());
        let msg = plan.validate().unwrap_err().to_string();
        assert!(msg.contains("no decisions"));
    }

    #[test]
    fn work_plan_validate_empty_rationale() {
        let plan = WorkPlan {
            goal: "test".to_string(),
            decisions: vec![WorkPlanDecision {
                decision: "do something".to_string(),
                rationale: String::new(),
                alternatives: vec![],
                files_affected: vec![],
                confidence: None,
            }],
            implementation_plan: vec![],
            out_of_scope: vec![],
        };
        let err = plan.validate().unwrap_err().to_string();
        assert!(err.contains("no rationale"));
    }

    #[test]
    fn work_plan_round_trip_json() {
        let plan = sample_plan();
        let json = serde_json::to_string_pretty(&plan).unwrap();
        let restored: WorkPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.goal, "Add JWT auth");
        assert_eq!(restored.decisions.len(), 1);
        assert_eq!(restored.decisions[0].confidence, Some(0.9));
    }

    #[test]
    fn work_plan_load_from_staging() {
        let dir = tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        let plan = sample_plan();
        let json = serde_json::to_string(&plan).unwrap();
        std::fs::write(ta_dir.join("work-plan.json"), &json).unwrap();
        let loaded = WorkPlan::load(dir.path()).unwrap();
        assert_eq!(loaded.goal, "Add JWT auth");
    }

    #[test]
    fn work_plan_to_claude_md_section() {
        let section = sample_plan().to_claude_md_section();
        assert!(section.contains("## Implementation Plan"));
        assert!(section.contains("RS256"));
        assert!(section.contains("Out of Scope"));
        assert!(section.contains("OAuth2 provider"));
    }
}
