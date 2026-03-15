//! html.rs — HTML output adapter with JavaScript-free progressive disclosure.

use crate::error::ChangeSetError;
use crate::output_adapters::{DetailLevel, OutputAdapter, RenderContext};
use crate::pr_package::{Artifact, ArtifactDisposition, ChangeType};

#[derive(Default)]
pub struct HtmlAdapter {}

impl HtmlAdapter {
    pub fn new() -> Self {
        Self {}
    }

    fn disposition_badge(&self, disposition: &ArtifactDisposition) -> &str {
        match disposition {
            ArtifactDisposition::Pending => r#"<span class="status pending">pending</span>"#,
            ArtifactDisposition::Approved => r#"<span class="status approved">approved</span>"#,
            ArtifactDisposition::Rejected => r#"<span class="status denied">rejected</span>"#,
            ArtifactDisposition::Discuss => r#"<span class="status discuss">discuss</span>"#,
        }
    }

    fn change_badge(&self, change_type: &ChangeType) -> &str {
        match change_type {
            ChangeType::Add => r#"<span class="badge add">+</span>"#,
            ChangeType::Modify => r#"<span class="badge modify">~</span>"#,
            ChangeType::Delete => r#"<span class="badge delete">-</span>"#,
            ChangeType::Rename => r#"<span class="badge rename">&gt;</span>"#,
        }
    }

    fn css(&self) -> &str {
        r#"
        <style>
            body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; max-width: 1200px; margin: 0 auto; padding: 20px; line-height: 1.6; }
            h1, h2, h3 { color: #333; }
            .header { background: #f5f5f5; padding: 20px; border-radius: 8px; margin-bottom: 30px; }
            .status { display: inline-block; padding: 4px 12px; border-radius: 4px; font-weight: 600; text-transform: uppercase; font-size: 12px; }
            .status.pending { background: #fef3c7; color: #92400e; }
            .status.approved { background: #d1fae5; color: #065f46; }
            .status.denied { background: #fee2e2; color: #991b1b; }
            .status.discuss { background: #dbeafe; color: #1e40af; }
            .artifact { background: white; border: 1px solid #e5e7eb; border-radius: 8px; padding: 20px; margin-bottom: 20px; }
            .badge { display: inline-block; width: 24px; height: 24px; text-align: center; border-radius: 4px; font-weight: 700; margin-right: 8px; }
            .badge.add { background: #d1fae5; color: #065f46; }
            .badge.modify { background: #fef3c7; color: #92400e; }
            .badge.delete { background: #fee2e2; color: #991b1b; }
            .badge.rename { background: #dbeafe; color: #1e40af; }
            details { margin-top: 15px; }
            summary { cursor: pointer; font-weight: 600; color: #4b5563; user-select: none; }
            summary:hover { color: #1f2937; }
            pre { background: #f9fafb; padding: 15px; border-radius: 4px; overflow-x: auto; }
            code { font-family: 'Monaco', 'Menlo', monospace; font-size: 13px; }
            .diff-add { color: #065f46; }
            .diff-del { color: #991b1b; }
            .meta { color: #6b7280; font-size: 14px; margin-top: 10px; }
            .tags { display: flex; gap: 8px; margin-top: 10px; }
            .tag { background: #ede9fe; color: #5b21b6; padding: 4px 12px; border-radius: 12px; font-size: 12px; }
        </style>
        "#
    }
}

impl OutputAdapter for HtmlAdapter {
    fn render(&self, ctx: &RenderContext) -> Result<String, ChangeSetError> {
        let pkg = ctx.package;
        let mut html = String::from("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"UTF-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n");
        html.push_str(&format!("<title>Draft: {}</title>\n", pkg.package_id));
        html.push_str(self.css());
        html.push_str("</head>\n<body>\n");

        // Header
        html.push_str("<div class=\"header\">\n");
        html.push_str("<h1>Draft</h1>\n");
        html.push_str(&format!("<p><strong>ID:</strong> {}</p>\n", pkg.package_id));
        html.push_str(&format!(
            "<p><strong>Status:</strong> <span class=\"status {}\">{}</span></p>\n",
            pkg.status, pkg.status
        ));
        html.push_str(&format!(
            "<p><strong>Goal:</strong> {}</p>\n",
            pkg.goal.title
        ));
        html.push_str(&format!(
            "<p><strong>Created:</strong> {}</p>\n",
            pkg.created_at.format("%Y-%m-%d %H:%M:%S")
        ));
        html.push_str("</div>\n");

        // Summary
        html.push_str("<div class=\"summary\">\n<h2>Summary</h2>\n");
        html.push_str(&format!(
            "<p><strong>What changed:</strong> {}</p>\n",
            pkg.summary.what_changed
        ));
        html.push_str(&format!(
            "<p><strong>Why:</strong> {}</p>\n",
            pkg.summary.why
        ));
        html.push_str(&format!(
            "<p><strong>Impact:</strong> {}</p>\n",
            pkg.summary.impact
        ));
        html.push_str("</div>\n");

        // Artifacts
        html.push_str(&format!(
            "<h2>Changes ({} artifacts)</h2>\n",
            pkg.changes.artifacts.len()
        ));

        let artifacts: Vec<&Artifact> = if let Some(filter) = &ctx.file_filter {
            pkg.changes
                .artifacts
                .iter()
                .filter(|a| a.resource_uri.contains(filter))
                .collect()
        } else {
            pkg.changes.artifacts.iter().collect()
        };

        for artifact in artifacts {
            html.push_str("<div class=\"artifact\">\n");
            html.push_str(&format!(
                "{} {} <strong>{}</strong>\n",
                self.change_badge(&artifact.change_type),
                self.disposition_badge(&artifact.disposition),
                artifact.resource_uri
            ));

            if let Some(tiers) = &artifact.explanation_tiers {
                html.push_str(&format!("<p><em>{}</em></p>\n", tiers.summary));

                if ctx.detail_level == DetailLevel::Medium || ctx.detail_level == DetailLevel::Full
                {
                    html.push_str(&format!("<p>{}</p>\n", tiers.explanation));

                    if !tiers.tags.is_empty() {
                        html.push_str("<div class=\"tags\">");
                        for tag in &tiers.tags {
                            html.push_str(&format!("<span class=\"tag\">{}</span>", tag));
                        }
                        html.push_str("</div>\n");
                    }
                }
            } else if let Some(rationale) = &artifact.rationale {
                html.push_str(&format!("<p><em>{}</em></p>\n", rationale));
            }

            if ctx.detail_level == DetailLevel::Full {
                if let Some(provider) = ctx.diff_provider {
                    if let Ok(diff) = provider.get_diff(&artifact.diff_ref) {
                        html.push_str("<details>\n<summary>View diff</summary>\n<pre><code>");
                        for line in diff.lines() {
                            if line.starts_with('+') && !line.starts_with("+++") {
                                html.push_str(&format!(
                                    "<span class=\"diff-add\">{}</span>\n",
                                    line
                                ));
                            } else if line.starts_with('-') && !line.starts_with("---") {
                                html.push_str(&format!(
                                    "<span class=\"diff-del\">{}</span>\n",
                                    line
                                ));
                            } else {
                                html.push_str(&format!("{}\n", line));
                            }
                        }
                        html.push_str("</code></pre>\n</details>\n");
                    }
                }
            }

            html.push_str("</div>\n");
        }

        html.push_str(&format!(
            "<div class=\"meta\">Generated by Trusted Autonomy v{}</div>\n",
            pkg.package_version
        ));
        html.push_str("</body>\n</html>");

        Ok(html)
    }

    fn name(&self) -> &str {
        "html"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_badge_renders_all_variants() {
        let adapter = HtmlAdapter::new();
        assert!(adapter
            .disposition_badge(&ArtifactDisposition::Pending)
            .contains("pending"));
        assert!(adapter
            .disposition_badge(&ArtifactDisposition::Approved)
            .contains("approved"));
        assert!(adapter
            .disposition_badge(&ArtifactDisposition::Rejected)
            .contains("denied"));
        assert!(adapter
            .disposition_badge(&ArtifactDisposition::Discuss)
            .contains("discuss"));
    }

    #[test]
    fn css_includes_discuss_status_class() {
        let adapter = HtmlAdapter::new();
        let css = adapter.css();
        assert!(css.contains(".status.discuss"));
        assert!(css.contains("#dbeafe"));
    }

    #[test]
    fn html_output_includes_disposition_badges() {
        use crate::draft_package::*;
        use crate::output_adapters::RenderContext;
        use chrono::Utc;
        use uuid::Uuid;

        let mut pkg = DraftPackage {
            package_version: "1.0.0".to_string(),
            package_id: Uuid::nil(),
            created_at: Utc::now(),
            goal: Goal {
                goal_id: "g1".to_string(),
                title: "Test".to_string(),
                objective: "Test".to_string(),
                success_criteria: vec![],
                constraints: vec![],
            },
            iteration: Iteration {
                iteration_id: "i1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging_dir".to_string(),
                    ref_name: "staging/g1/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "a1".to_string(),
                agent_type: "test".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "abc".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "test".to_string(),
                why: "test".to_string(),
                impact: "none".to_string(),
                rollback_plan: "revert".to_string(),
                open_questions: vec![],
                alternatives_considered: vec![],
            },
            plan: Plan {
                completed_steps: vec![],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![Artifact {
                    resource_uri: "fs://workspace/src/main.rs".to_string(),
                    change_type: ChangeType::Modify,
                    disposition: ArtifactDisposition::Discuss,
                    diff_ref: String::new(),
                    rationale: Some("test rationale".to_string()),
                    explanation_tiers: None,
                    comments: None,
                    amendment: None,
                    tests_run: vec![],
                    dependencies: vec![],
                }],
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
            status: DraftStatus::Draft,
            verification_warnings: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
        };
        pkg.status = DraftStatus::PendingReview;

        let adapter = HtmlAdapter::new();
        let ctx = RenderContext {
            package: &pkg,
            detail_level: DetailLevel::Top,
            file_filter: None,
            diff_provider: None,
        };
        let html = adapter.render(&ctx).unwrap();
        assert!(html.contains(r#"class="status discuss""#));
    }
}
