//! json.rs — JSON output adapter for machine-readable output.

use crate::error::ChangeSetError;
use crate::output_adapters::{OutputAdapter, RenderContext};

#[derive(Default)]
pub struct JsonAdapter {}

impl JsonAdapter {
    pub fn new() -> Self {
        Self {}
    }
}

impl OutputAdapter for JsonAdapter {
    fn render(&self, ctx: &RenderContext) -> Result<String, ChangeSetError> {
        // For JSON output, we serialize the entire PRPackage
        // The detail_level and file_filter are ignored — the consumer can filter client-side

        let json = serde_json::to_string_pretty(ctx.package).map_err(|e| {
            ChangeSetError::InvalidData(format!("JSON serialization failed: {}", e))
        })?;

        Ok(json)
    }

    fn name(&self) -> &str {
        "json"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_adapters::DetailLevel;
    use crate::pr_package::*;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn renders_valid_json() {
        let package = PRPackage {
            package_version: "1.0.0".to_string(),
            package_id: Uuid::new_v4(),
            created_at: Utc::now(),
            goal: Goal {
                goal_id: "goal-1".to_string(),
                title: "Test".to_string(),
                objective: "Test".to_string(),
                success_criteria: vec![],
                constraints: vec![],
                parent_goal_title: None,
            },
            iteration: Iteration {
                iteration_id: "iter-1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging".to_string(),
                    ref_name: "staging/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "agent-1".to_string(),
                agent_type: "coder".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "hash".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Test".to_string(),
                why: "Test".to_string(),
                impact: "None".to_string(),
                rollback_plan: "Revert".to_string(),
                open_questions: vec![],
                alternatives_considered: vec![],
            },
            plan: Plan {
                completed_steps: vec![],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 0,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "hash".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![],
                reviewers: vec![],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "hash".to_string(),
                agent_signature: "sig".to_string(),
                gateway_attestation: None,
            },
            status: PRStatus::Draft,
            verification_warnings: vec![],
            validation_log: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
            parent_draft_id: None,
            pending_approvals: vec![],
            supervisor_review: None,
            ignored_artifacts: vec![],
            baseline_artifacts: vec![],
            agent_decision_log: vec![],
        };

        let adapter = JsonAdapter::new();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Full,
            file_filter: None,
            diff_provider: None,
            section_filter: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&output).is_ok());
        assert!(output.contains("package_version"));
        assert!(output.contains("package_id"));
    }
}
