//! terminal.rs — Terminal output adapter with configurable color support.
//!
//! Color is off by default. Enable with `TerminalAdapter::with_color()` or `--color` CLI flag.

use crate::error::ChangeSetError;
use crate::output_adapters::{default_summary, DetailLevel, OutputAdapter, RenderContext};
use crate::pr_package::{Artifact, ChangeType};

#[derive(Default)]
pub struct TerminalAdapter {
    color: bool,
}

impl TerminalAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_color(color: bool) -> Self {
        Self { color }
    }

    /// Strip HTML tags from a string to prevent HTML-rendered content
    /// from leaking into terminal output.
    ///
    /// Only strips sequences that look like real HTML tags (contain attributes,
    /// slashes, or known tag names). Preserves angle-bracket text that looks
    /// like code placeholders: `<id>`, `<path>`, `<T>`, etc.
    fn strip_html(s: &str) -> std::borrow::Cow<'_, str> {
        if !s.contains('<') {
            return std::borrow::Cow::Borrowed(s);
        }
        // Only strip if it looks like actual HTML (contains known tags or attributes).
        // Pattern: <tag ...>, </tag>, <tag/>, or tags with class/style attributes.
        let has_html = s.contains("</")
            || s.contains("class=")
            || s.contains("style=")
            || s.contains("<span")
            || s.contains("<div")
            || s.contains("<br")
            || s.contains("<p>")
            || s.contains("<p ")
            || s.contains("<a ")
            || s.contains("<img");
        if !has_html {
            return std::borrow::Cow::Borrowed(s);
        }
        let mut out = String::with_capacity(s.len());
        let mut in_tag = false;
        for c in s.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        std::borrow::Cow::Owned(out)
    }

    // -- ANSI helpers (return empty strings when color is off) --

    fn bold(&self) -> &str {
        if self.color {
            "\x1b[1m"
        } else {
            ""
        }
    }

    fn dim(&self) -> &str {
        if self.color {
            "\x1b[2m"
        } else {
            ""
        }
    }

    fn reset(&self) -> &str {
        if self.color {
            "\x1b[0m"
        } else {
            ""
        }
    }

    fn color_code<'a>(&self, code: &'a str) -> &'a str {
        if self.color {
            code
        } else {
            ""
        }
    }

    fn render_header(&self, ctx: &RenderContext) -> String {
        let pkg = ctx.package;
        let status_color = if self.color {
            match pkg.status {
                crate::pr_package::PRStatus::Draft => "\x1b[33m",
                crate::pr_package::PRStatus::PendingReview => "\x1b[36m",
                crate::pr_package::PRStatus::Approved { .. } => "\x1b[32m",
                crate::pr_package::PRStatus::Denied { .. } => "\x1b[31m",
                crate::pr_package::PRStatus::Applied { .. } => "\x1b[32m",
                crate::pr_package::PRStatus::Superseded { .. } => "\x1b[90m",
                crate::pr_package::PRStatus::Closed { .. } => "\x1b[90m",
            }
        } else {
            ""
        };
        let bold = self.bold();
        let reset = self.reset();

        format!(
            "{bold}Draft: {}{reset}\n\
            Status: {}{}{reset}\n\
            Goal: {}\n\
            Created: {}\n\n\
            {bold}Summary:{reset}\n\
            {}\n\n\
            {bold}Why:{reset}\n\
            {}\n\n\
            {bold}Impact:{reset}\n\
            {}\n\n",
            pkg.package_id,
            status_color,
            pkg.status,
            Self::strip_html(&pkg.goal.title),
            pkg.created_at.format("%Y-%m-%d %H:%M:%S"),
            Self::strip_html(&pkg.summary.what_changed),
            Self::strip_html(&pkg.summary.why),
            Self::strip_html(&pkg.summary.impact),
            bold = bold,
            reset = reset
        )
    }

    fn change_icon(&self, change_type: &ChangeType) -> String {
        if self.color {
            match change_type {
                ChangeType::Add => "\x1b[32m+\x1b[0m".to_string(),
                ChangeType::Modify => "\x1b[33m~\x1b[0m".to_string(),
                ChangeType::Delete => "\x1b[31m-\x1b[0m".to_string(),
                ChangeType::Rename => "\x1b[36m>\x1b[0m".to_string(),
            }
        } else {
            match change_type {
                ChangeType::Add => "+".to_string(),
                ChangeType::Modify => "~".to_string(),
                ChangeType::Delete => "-".to_string(),
                ChangeType::Rename => ">".to_string(),
            }
        }
    }

    fn render_artifact_top(&self, artifact: &Artifact) -> String {
        let icon = self.change_icon(&artifact.change_type);

        let disposition_badge = match artifact.disposition {
            crate::pr_package::ArtifactDisposition::Pending => "[pending]",
            crate::pr_package::ArtifactDisposition::Approved => "[approved]",
            crate::pr_package::ArtifactDisposition::Rejected => "[rejected]",
            crate::pr_package::ArtifactDisposition::Discuss => "[discuss]",
        };

        let summary_raw = artifact
            .explanation_tiers
            .as_ref()
            .map(|t| t.summary.as_str())
            .or(artifact.rationale.as_deref())
            .unwrap_or_else(|| default_summary(&artifact.resource_uri, &artifact.change_type));
        let summary = Self::strip_html(summary_raw);

        // File path on its own line, summary on next line indented to match.
        format!(
            "  {} {} {}\n    {}",
            icon, disposition_badge, artifact.resource_uri, summary
        )
    }

    fn render_artifact_medium(&self, artifact: &Artifact) -> String {
        let mut output = self.render_artifact_top(artifact);
        let dim = self.dim();
        let reset = self.reset();
        output.push('\n');

        if let Some(tiers) = &artifact.explanation_tiers {
            output.push_str(&format!(
                "\n    {dim}Explanation:{reset} {}\n",
                tiers.explanation
            ));

            if !tiers.tags.is_empty() {
                output.push_str(&format!(
                    "    {dim}Tags:{reset} {}\n",
                    tiers.tags.join(", ")
                ));
            }

            if !tiers.related_artifacts.is_empty() {
                output.push_str(&format!("    {dim}Related:{reset}\n"));
                for related in &tiers.related_artifacts {
                    output.push_str(&format!("      - {}\n", related));
                }
            }
        } else if let Some(rationale) = &artifact.rationale {
            output.push_str(&format!("\n    {dim}Rationale:{reset} {}\n", rationale));
        }

        if !artifact.dependencies.is_empty() {
            output.push_str(&format!("    {dim}Dependencies:{reset}\n"));
            for dep in &artifact.dependencies {
                output.push_str(&format!("      {:?}: {}\n", dep.kind, dep.target_uri));
            }
        }

        output
    }

    fn render_artifact_full(&self, artifact: &Artifact, ctx: &RenderContext) -> String {
        let mut output = self.render_artifact_medium(artifact);
        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        // Fetch and display full diff if provider is available
        if let Some(provider) = ctx.diff_provider {
            match provider.get_diff(&artifact.diff_ref) {
                Ok(diff) => {
                    output.push_str(&format!("\n    {bold}Diff:{reset}\n"));
                    let green = self.color_code("\x1b[32m");
                    let red = self.color_code("\x1b[31m");
                    let cyan = self.color_code("\x1b[36m");
                    for line in diff.lines() {
                        if line.starts_with('+') && !line.starts_with("+++") {
                            output.push_str(&format!("    {green}{}{reset}\n", line));
                        } else if line.starts_with('-') && !line.starts_with("---") {
                            output.push_str(&format!("    {red}{}{reset}\n", line));
                        } else if line.starts_with("@@") {
                            output.push_str(&format!("    {cyan}{}{reset}\n", line));
                        } else {
                            output.push_str(&format!("    {}\n", line));
                        }
                    }
                }
                Err(e) => {
                    output.push_str(&format!(
                        "    {red}[Error loading diff: {}]{reset}\n",
                        e,
                        red = self.color_code("\x1b[31m"),
                        reset = reset
                    ));
                }
            }
        } else {
            output.push_str(&format!(
                "    {dim}[Diff available at: {}]{reset}\n",
                artifact.diff_ref
            ));
        }

        output
    }

    /// Group artifacts by module (top-level directory) for the "What Changed" section (v0.9.5).
    fn render_grouped_changes(&self, artifacts: &[&Artifact]) -> String {
        use std::collections::BTreeMap;
        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        let mut output = format!("{bold}What Changed ({} files):{reset}\n", artifacts.len());

        // Group by module (first path segment after fs://workspace/).
        let mut groups: BTreeMap<String, Vec<&Artifact>> = BTreeMap::new();
        for artifact in artifacts {
            let path = artifact
                .resource_uri
                .strip_prefix("fs://workspace/")
                .unwrap_or(&artifact.resource_uri);
            let module = path.split('/').next().unwrap_or("root").to_string();
            groups.entry(module).or_default().push(artifact);
        }

        for (module, arts) in &groups {
            output.push_str(&format!("\n  {bold}{}/{reset}\n", module));
            for artifact in arts {
                let icon = self.change_icon(&artifact.change_type);
                let path = artifact
                    .resource_uri
                    .strip_prefix("fs://workspace/")
                    .unwrap_or(&artifact.resource_uri);
                let short_path = path.strip_prefix(&format!("{}/", module)).unwrap_or(path);

                let summary_raw = artifact
                    .explanation_tiers
                    .as_ref()
                    .map(|t| t.summary.as_str())
                    .or(artifact.rationale.as_deref())
                    .unwrap_or_else(|| {
                        default_summary(&artifact.resource_uri, &artifact.change_type)
                    });
                let summary = Self::strip_html(summary_raw);

                let dep_marker = if !artifact.dependencies.is_empty() {
                    let deps: Vec<&str> = artifact
                        .dependencies
                        .iter()
                        .map(|d| {
                            d.target_uri
                                .strip_prefix("fs://workspace/")
                                .unwrap_or(&d.target_uri)
                        })
                        .collect();
                    format!(" {dim}[deps: {}]{reset}", deps.join(", "))
                } else {
                    String::new()
                };

                output.push_str(&format!(
                    "    {} {} — {}{}\n",
                    icon, short_path, summary, dep_marker
                ));
            }
        }

        output
    }

    /// Render the "Design Decisions" section from alternatives_considered (v0.9.5).
    fn render_design_decisions(&self, ctx: &RenderContext) -> String {
        let alts = &ctx.package.summary.alternatives_considered;
        if alts.is_empty() {
            return String::new();
        }

        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();
        let green = self.color_code("\x1b[32m");

        let mut output = format!("\n{bold}Design Decisions:{reset}\n");
        for alt in alts {
            let marker = if alt.chosen {
                format!("{green}[chosen]{reset}")
            } else {
                format!("{dim}[considered]{reset}")
            };
            output.push_str(&format!("  {} {}\n", marker, alt.option));
            output.push_str(&format!("    {}\n", alt.rationale));
        }

        output
    }
}

impl OutputAdapter for TerminalAdapter {
    fn render(&self, ctx: &RenderContext) -> Result<String, ChangeSetError> {
        let mut output = String::new();
        let bold = self.bold();
        let reset = self.reset();
        let dim = self.dim();

        // ── Summary ──
        output.push_str(&self.render_header(ctx));

        // Filter artifacts
        let artifacts = &ctx.package.changes.artifacts;
        let filtered_artifacts: Vec<&Artifact> = if let Some(filter) = &ctx.file_filter {
            artifacts
                .iter()
                .filter(|a| a.resource_uri.contains(filter))
                .collect()
        } else {
            artifacts.iter().collect()
        };

        if let (true, Some(filter)) = (filtered_artifacts.is_empty(), &ctx.file_filter) {
            return Err(ChangeSetError::InvalidData(format!(
                "No artifacts match filter: {}",
                filter
            )));
        }

        // ── What Changed (module-grouped file list) ──
        output.push_str(&self.render_grouped_changes(&filtered_artifacts));

        // ── Design Decisions ──
        output.push_str(&self.render_design_decisions(ctx));

        // ── Artifacts (detailed per-artifact view) ──
        if ctx.detail_level != DetailLevel::Top {
            output.push_str(&format!(
                "\n{bold}Artifacts ({}):{reset}\n",
                filtered_artifacts.len()
            ));

            for artifact in &filtered_artifacts {
                match ctx.detail_level {
                    DetailLevel::Top => unreachable!(),
                    DetailLevel::Medium => {
                        output.push_str(&self.render_artifact_medium(artifact));
                        output.push('\n');
                    }
                    DetailLevel::Full => {
                        output.push_str(&self.render_artifact_full(artifact, ctx));
                        output.push('\n');
                    }
                }
            }
        }

        // Footer with review guidance
        if ctx.detail_level == DetailLevel::Top || ctx.detail_level == DetailLevel::Medium {
            output.push_str(&format!(
                "\n{dim}Tip: Use --detail full to see complete diffs{reset}\n"
            ));
        }

        Ok(output)
    }

    fn name(&self) -> &str {
        "terminal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_package::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_package() -> PRPackage {
        PRPackage {
            package_version: "1.0.0".to_string(),
            package_id: Uuid::new_v4(),
            created_at: Utc::now(),
            goal: Goal {
                goal_id: "goal-1".to_string(),
                title: "Test Goal".to_string(),
                objective: "Test objective".to_string(),
                success_criteria: vec![],
                constraints: vec![],
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
                capability_manifest_hash: "hash123".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Updated auth system".to_string(),
                why: "To improve security".to_string(),
                impact: "All users must re-login".to_string(),
                rollback_plan: "Revert commit".to_string(),
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
                    resource_uri: "fs://workspace/src/auth.rs".to_string(),
                    change_type: ChangeType::Modify,
                    diff_ref: "changeset:0".to_string(),
                    tests_run: vec![],
                    disposition: ArtifactDisposition::Pending,
                    rationale: Some("JWT migration".to_string()),
                    dependencies: vec![],
                    explanation_tiers: Some(ExplanationTiers {
                        summary: "Migrated to JWT auth".to_string(),
                        explanation: "Full JWT implementation with validation".to_string(),
                        tags: vec!["security".to_string()],
                        related_artifacts: vec![],
                    }),
                    comments: None,
                    amendment: None,
                }],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 10,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "trace123".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![],
                reviewers: vec![],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "hash123".to_string(),
                agent_signature: "sig123".to_string(),
                gateway_attestation: None,
            },
            status: PRStatus::PendingReview,
            verification_warnings: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
        }
    }

    #[test]
    fn render_top_level() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: None,
            diff_provider: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Draft"));
        assert!(output.contains("pending_review"));
        assert!(output.contains("src/"));
        assert!(output.contains("auth.rs"));
        assert!(output.contains("Migrated to JWT auth"));
        // Default (no color) should not contain ANSI escape codes.
        assert!(!output.contains("\x1b["));
    }

    #[test]
    fn render_with_color() {
        let adapter = TerminalAdapter::with_color(true);
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: None,
            diff_provider: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Draft"));
        // Color mode should contain ANSI escape codes.
        assert!(output.contains("\x1b["));
    }

    #[test]
    fn render_medium_level() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Medium,
            file_filter: None,
            diff_provider: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Full JWT implementation"));
        assert!(output.contains("security"));
    }

    #[test]
    fn file_filter_works() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: Some("auth.rs".to_string()),
            diff_provider: None,
        };

        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("auth.rs"));
    }

    #[test]
    fn file_filter_no_match_returns_error() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: Some("nonexistent.rs".to_string()),
            diff_provider: None,
        };

        let result = adapter.render(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn terminal_output_contains_no_html_tags() {
        // Regression test for the garbled HTML bug (ÆpendingÅ in terminal output).
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Medium,
            file_filter: None,
            diff_provider: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            !output.contains("<span"),
            "HTML span tags must not appear in terminal output"
        );
        assert!(
            !output.contains("</span>"),
            "HTML closing tags must not appear in terminal output"
        );
        assert!(
            output.contains("[pending]"),
            "Disposition badge must use bracket notation"
        );
    }

    #[test]
    fn strip_html_removes_tags() {
        assert_eq!(
            TerminalAdapter::strip_html(r#"<span class="status">pending</span>"#).as_ref(),
            "pending"
        );
        assert_eq!(
            TerminalAdapter::strip_html("no tags here").as_ref(),
            "no tags here"
        );
        assert_eq!(TerminalAdapter::strip_html("").as_ref(), "");
    }

    #[test]
    fn strip_html_preserves_code_placeholders() {
        // Angle brackets in code-style text (e.g. <id>, <path>, <T>) should be preserved.
        assert_eq!(
            TerminalAdapter::strip_html("ta session show <id>").as_ref(),
            "ta session show <id>"
        );
        assert_eq!(
            TerminalAdapter::strip_html("Vec<String>").as_ref(),
            "Vec<String>"
        );
        assert_eq!(
            TerminalAdapter::strip_html("list [--all] and show <id>").as_ref(),
            "list [--all] and show <id>"
        );
        // But actual HTML is still stripped.
        assert_eq!(
            TerminalAdapter::strip_html(r#"text <span class="x">inner</span> more"#).as_ref(),
            "text inner more"
        );
    }

    #[test]
    fn strip_html_sanitizes_summary_fields() {
        // Simulate a package where the summary contains HTML (as if data was corrupted).
        let mut package = test_package();
        package.summary.what_changed =
            r#"Updated <span class="bold">auth</span> system"#.to_string();

        let adapter = TerminalAdapter::new();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: None,
            diff_provider: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(
            output.contains("Updated auth system"),
            "HTML should be stripped from summary"
        );
        assert!(!output.contains("<span"), "No HTML tags in terminal output");
    }

    // ── v0.9.5 Structured view tests ──

    #[test]
    fn render_grouped_changes_by_module() {
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.changes.artifacts.push(Artifact {
            resource_uri: "fs://workspace/tests/auth_test.rs".to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:1".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Pending,
            rationale: Some("Added auth tests".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None,
            amendment: None,
        });
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: None,
            diff_provider: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("What Changed (2 files):"));
        assert!(output.contains("src/"));
        assert!(output.contains("tests/"));
    }

    #[test]
    fn render_design_decisions() {
        let adapter = TerminalAdapter::new();
        let mut package = test_package();
        package.summary.alternatives_considered = vec![
            DesignAlternative {
                option: "Use HashMap for lookup".to_string(),
                rationale: "Best performance".to_string(),
                chosen: true,
            },
            DesignAlternative {
                option: "Use BTreeMap".to_string(),
                rationale: "Ordered but slower".to_string(),
                chosen: false,
            },
        ];
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: None,
            diff_provider: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(output.contains("Design Decisions:"));
        assert!(output.contains("[chosen]"));
        assert!(output.contains("[considered]"));
        assert!(output.contains("Use HashMap for lookup"));
        assert!(output.contains("Use BTreeMap"));
    }

    #[test]
    fn render_no_design_decisions_when_empty() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Top,
            file_filter: None,
            diff_provider: None,
        };
        let output = adapter.render(&ctx).unwrap();
        assert!(!output.contains("Design Decisions:"));
    }

    #[test]
    fn render_medium_shows_artifacts_section() {
        let adapter = TerminalAdapter::new();
        let package = test_package();
        let ctx = RenderContext {
            package: &package,
            detail_level: DetailLevel::Medium,
            file_filter: None,
            diff_provider: None,
        };
        let output = adapter.render(&ctx).unwrap();
        // Medium shows both grouped summary and detailed artifacts
        assert!(output.contains("What Changed"));
        assert!(output.contains("Artifacts (1):"));
    }
}
