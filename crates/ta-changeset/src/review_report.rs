//! ReviewReport: the output of the draft pre-apply plan review.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::plan_merge::PlanConflict;

/// A plan item that appears unchecked in the phase being applied, with no matching
/// token found in the draft diffs. Informational only — does not block apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageGap {
    pub phase_id: String,
    pub item_number: usize,
    pub text_excerpt: String,
}

/// The result of the automatic PLAN.md audit run during `ta draft build`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    pub draft_id: Uuid,
    pub generated_at: DateTime<Utc>,
    /// Regressions silently fixed: source had a newer status and staging reverted it.
    pub silent_fixes: Vec<String>,
    /// Agent-driven additions: newly checked items, phase completions, new sub-phases.
    pub agent_additions: Vec<String>,
    /// Unresolvable conflicts requiring human decision.
    pub conflicts: Vec<PlanConflict>,
    /// Plan items with no token coverage in the diff (informational).
    pub coverage_gaps: Vec<CoverageGap>,
    /// Unified diff against source PLAN.md incorporating all non-conflict resolutions.
    /// `None` when there is nothing to patch (already clean or no PLAN.md in draft).
    pub plan_patch: Option<String>,
    /// When true: this draft touched only PLAN.md and all newly-checked items were
    /// found in the source workspace, confirming it is a catch-up record (not fabrication).
    /// When false (the default): either non-PLAN.md artifacts exist or verification
    /// was not performed.
    #[serde(default)]
    pub source_verified: bool,
}

impl ReviewReport {
    pub fn is_clean(&self) -> bool {
        self.silent_fixes.is_empty()
            && self.agent_additions.is_empty()
            && self.conflicts.is_empty()
            && self.coverage_gaps.is_empty()
    }

    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    pub fn summary_line(&self) -> String {
        format!(
            "{} fixes, {} additions, {} conflicts, {} gaps",
            self.silent_fixes.len(),
            self.agent_additions.len(),
            self.conflicts.len(),
            self.coverage_gaps.len()
        )
    }

    /// Load a ReviewReport from `.ta/review/<draft_id>/report.json`.
    pub fn load(workspace_root: &std::path::Path, draft_id: Uuid) -> Option<Self> {
        let path = workspace_root
            .join(".ta")
            .join("review")
            .join(draft_id.to_string())
            .join("report.json");
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save the ReviewReport to `.ta/review/<draft_id>/report.json`.
    pub fn save(&self, workspace_root: &std::path::Path) -> anyhow::Result<()> {
        let dir = workspace_root
            .join(".ta")
            .join("review")
            .join(self.draft_id.to_string());
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("report.json");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }
}

/// Returns true when a draft's artifact list contains only PLAN.md.
/// Used to trigger source-verification mode in the reviewer.
pub fn is_planmd_only_draft(artifact_uris: &[&str]) -> bool {
    if artifact_uris.is_empty() {
        return false;
    }
    artifact_uris.iter().all(|uri| {
        uri.ends_with("/PLAN.md") || *uri == "fs://workspace/PLAN.md" || *uri == "PLAN.md"
    })
}

/// Returns true if the gap text refers to tests or documentation (non-functional).
///
/// Used to classify unchecked plan items when determining the recommended action.
/// Keywords (case-insensitive): "test", "usage", "docs", "readme", "comment", "doc".
pub fn is_tests_docs_item(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["test", "usage", "docs", "readme", "comment", "doc"]
        .iter()
        .any(|kw| lower.contains(kw))
}

/// Render the plan-review verdict and recommended action for display in `ta draft view`.
///
/// Produces a verdict header (with phase_id when provided) and a "Recommended action"
/// block when there are unchecked coverage gaps. Items are classified as tests/docs vs
/// functional to choose between the apply-then-follow-up path and the deny-then-re-run path.
///
/// Verdict rules:
/// - No gaps, no conflicts → [PASS] → "Ready to apply: ta draft apply <id>"
/// - Gaps, all tests/docs only → [WARN] → apply now, then ta run --follow-up
/// - Gaps with functional items → [WARN] → ta draft deny, then ta run --follow-up
/// - Only conflicts, no gaps → [WARN] → resolve conflicts first
pub fn render_review_verdict_and_action(
    report: &ReviewReport,
    phase_id: Option<&str>,
    draft_id_short: Option<&str>,
    goal_title: Option<&str>,
    color: bool,
) -> String {
    let mut out = String::new();

    let verdict_header = match phase_id {
        Some(pid) => format!("Verdict for {}", pid),
        None => "Verdict".to_string(),
    };

    let apply_cmd = draft_id_short
        .map(|id| format!("ta draft apply {}", id))
        .unwrap_or_else(|| "ta draft apply <id>".to_string());

    if report.source_verified {
        // PLAN.md-only catch-up draft: all checked items verified in source.
        if color {
            out.push_str(&format!(
                "\x1b[32m[review] {}: [PASS]\x1b[0m\n",
                verdict_header
            ));
            out.push_str(
                "  \x1b[32mItems verified present in source — catch-up PLAN.md update.\x1b[0m\n",
            );
            out.push_str(&format!("  \x1b[32mReady to apply: {}\x1b[0m\n", apply_cmd));
        } else {
            out.push_str(&format!("[review] {}: [PASS]\n", verdict_header));
            out.push_str("  Items verified present in source — catch-up PLAN.md update.\n");
            out.push_str(&format!("  Ready to apply: {}\n", apply_cmd));
        }
    } else if report.coverage_gaps.is_empty() && report.conflicts.is_empty() {
        if color {
            out.push_str(&format!(
                "\x1b[32m[review] {}: [PASS]\x1b[0m\n",
                verdict_header
            ));
            out.push_str(&format!("  \x1b[32mReady to apply: {}\x1b[0m\n", apply_cmd));
        } else {
            out.push_str(&format!("[review] {}: [PASS]\n", verdict_header));
            out.push_str(&format!("  Ready to apply: {}\n", apply_cmd));
        }
    } else if !report.coverage_gaps.is_empty() {
        let all_tests_docs = report
            .coverage_gaps
            .iter()
            .all(|g| is_tests_docs_item(&g.text_excerpt));

        let followup_cmd = match (goal_title, phase_id) {
            (Some(title), Some(pid)) => {
                format!("ta run \"{}\" --follow-up --phase {}", title, pid)
            }
            (Some(title), None) => format!("ta run \"{}\" --follow-up", title),
            _ => "ta run \"<goal>\" --follow-up".to_string(),
        };

        if color {
            out.push_str(&format!(
                "\x1b[33m[review] {}: [WARN]\x1b[0m\n",
                verdict_header
            ));
        } else {
            out.push_str(&format!("[review] {}: [WARN]\n", verdict_header));
        }

        if all_tests_docs {
            out.push_str("  Unchecked items are tests/docs only — safe to apply now.\n");
            out.push_str("  Recommended action:\n");
            out.push_str(&format!("    {}\n", apply_cmd));
            out.push_str(&format!("    {}\n", followup_cmd));
        } else {
            let first_excerpt = &report.coverage_gaps[0].text_excerpt;
            let summary: String = first_excerpt.chars().take(60).collect();
            let summary = if first_excerpt.chars().count() > 60 {
                format!("{}…", summary)
            } else {
                summary
            };
            let deny_cmd = draft_id_short
                .map(|id| format!("ta draft deny {} --reason \"incomplete: {}\"", id, summary))
                .unwrap_or_else(|| {
                    format!("ta draft deny <id> --reason \"incomplete: {}\"", summary)
                });
            out.push_str(
                "  Unchecked items include functional code — apply would be incomplete.\n",
            );
            out.push_str("  Recommended action:\n");
            out.push_str(&format!("    {}\n", deny_cmd));
            out.push_str(&format!("    {}\n", followup_cmd));
        }
    } else {
        // Conflicts only (no coverage gaps).
        if color {
            out.push_str(&format!(
                "\x1b[33m[review] {}: [WARN]\x1b[0m\n",
                verdict_header
            ));
        } else {
            out.push_str(&format!("[review] {}: [WARN]\n", verdict_header));
        }
        out.push_str("  Plan conflicts require resolution before applying.\n");
    }

    out
}

/// Render a ReviewReport for display in `ta draft view`.
pub fn render_review_report(report: &ReviewReport, color: bool) -> String {
    if report.is_clean() {
        if color {
            return "\x1b[32m[review] Plan audit clean.\x1b[0m\n".to_string();
        } else {
            return "[review] Plan audit clean.\n".to_string();
        }
    }

    let mut out = String::new();
    out.push_str("[review] Plan audit:\n");

    for fix in &report.silent_fixes {
        if color {
            out.push_str(&format!("  \x1b[90m[fix] {}\x1b[0m\n", fix));
        } else {
            out.push_str(&format!("  [fix] {}\n", fix));
        }
    }

    for addition in &report.agent_additions {
        if color {
            out.push_str(&format!("  \x1b[32m[+] {}\x1b[0m\n", addition));
        } else {
            out.push_str(&format!("  [+] {}\n", addition));
        }
    }

    for gap in &report.coverage_gaps {
        if color {
            out.push_str(&format!(
                "  \x1b[33m[gap] {} item {}: {}\x1b[0m\n",
                gap.phase_id, gap.item_number, gap.text_excerpt
            ));
        } else {
            out.push_str(&format!(
                "  [gap] {} item {}: {}\n",
                gap.phase_id, gap.item_number, gap.text_excerpt
            ));
        }
    }

    for conflict in &report.conflicts {
        if color {
            out.push_str(&format!(
                "  \x1b[31m[CONFLICT] {}\x1b[0m\n",
                conflict.description
            ));
            if !conflict.base_text.is_empty() {
                out.push_str(&format!("    base:    {}\n", conflict.base_text));
            }
            out.push_str(&format!("    staging: {}\n", conflict.staging_text));
            out.push_str(&format!("    source:  {}\n", conflict.source_text));
            out.push_str("    \x1b[31m→ Resolve before applying.\x1b[0m\n");
        } else {
            out.push_str(&format!("  [CONFLICT] {}\n", conflict.description));
            if !conflict.base_text.is_empty() {
                out.push_str(&format!("    base:    {}\n", conflict.base_text));
            }
            out.push_str(&format!("    staging: {}\n", conflict.staging_text));
            out.push_str(&format!("    source:  {}\n", conflict.source_text));
            out.push_str("    → Resolve before applying.\n");
        }
    }

    out
}

// ── v0.15.19.4.1: Supervisor verdict actionable guidance tests ─────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn make_report_with_gaps(gaps: Vec<(&str, &str)>) -> ReviewReport {
        ReviewReport {
            draft_id: Uuid::new_v4(),
            generated_at: Utc::now(),
            silent_fixes: vec![],
            agent_additions: vec![],
            conflicts: vec![],
            coverage_gaps: gaps
                .into_iter()
                .enumerate()
                .map(|(i, (phase_id, text))| CoverageGap {
                    phase_id: phase_id.to_string(),
                    item_number: i + 1,
                    text_excerpt: text.to_string(),
                })
                .collect(),
            plan_patch: None,
            source_verified: false,
        }
    }

    fn make_clean_report() -> ReviewReport {
        ReviewReport {
            draft_id: Uuid::new_v4(),
            generated_at: Utc::now(),
            silent_fixes: vec![],
            agent_additions: vec![],
            conflicts: vec![],
            coverage_gaps: vec![],
            plan_patch: None,
            source_verified: false,
        }
    }

    #[test]
    fn verdict_warn_tests_docs_only_recommends_apply_then_followup() {
        let report = make_report_with_gaps(vec![
            ("v0.15.19.4", "Integration test: version_check_suppressed"),
            (
                "v0.15.19.4",
                "USAGE.md — apply section: Add a note explaining",
            ),
        ]);
        let output = render_review_verdict_and_action(
            &report,
            Some("v0.15.19.4"),
            Some("abc12345"),
            Some("Version-Check Fix"),
            false,
        );
        assert!(
            output.contains("[WARN]"),
            "should be warn verdict: {}",
            output
        );
        assert!(
            output.contains("v0.15.19.4"),
            "should include phase_id: {}",
            output
        );
        assert!(
            output.contains("tests/docs only"),
            "should note tests/docs: {}",
            output
        );
        assert!(
            output.contains("ta draft apply abc12345"),
            "should recommend apply: {}",
            output
        );
        assert!(
            output.contains("--follow-up"),
            "should recommend follow-up run: {}",
            output
        );
        assert!(
            output.contains("--phase v0.15.19.4"),
            "should include --phase flag: {}",
            output
        );
    }

    #[test]
    fn verdict_warn_functional_recommends_deny() {
        let report = make_report_with_gaps(vec![(
            "v0.15.19.4",
            "implement X feature in governed_workflow.rs",
        )]);
        let output = render_review_verdict_and_action(
            &report,
            Some("v0.15.19.4"),
            Some("abc12345"),
            Some("My Goal"),
            false,
        );
        assert!(
            output.contains("[WARN]"),
            "should be warn verdict: {}",
            output
        );
        assert!(
            output.contains("ta draft deny"),
            "should recommend deny: {}",
            output
        );
        assert!(
            output.contains("incomplete:"),
            "deny reason should include 'incomplete:': {}",
            output
        );
        assert!(
            output.contains("--follow-up"),
            "should recommend follow-up run: {}",
            output
        );
    }

    #[test]
    fn verdict_pass_recommends_apply() {
        let report = make_clean_report();
        let output = render_review_verdict_and_action(
            &report,
            Some("v0.15.19.4"),
            Some("abc12345"),
            Some("My Goal"),
            false,
        );
        assert!(
            output.contains("[PASS]"),
            "should be pass verdict: {}",
            output
        );
        assert!(
            output.contains("Ready to apply"),
            "should say ready to apply: {}",
            output
        );
        assert!(
            output.contains("ta draft apply abc12345"),
            "should include apply command: {}",
            output
        );
    }

    #[test]
    fn is_tests_docs_item_matches_keywords() {
        assert!(is_tests_docs_item("Integration test: version_check"));
        assert!(is_tests_docs_item("USAGE.md — apply section"));
        assert!(is_tests_docs_item("update docs for the feature"));
        assert!(is_tests_docs_item("Add README section"));
        assert!(is_tests_docs_item("Add a comment explaining"));
        assert!(is_tests_docs_item("Update docstring for function"));
    }

    #[test]
    fn is_tests_docs_item_rejects_functional() {
        assert!(!is_tests_docs_item(
            "implement X feature in governed_workflow.rs"
        ));
        assert!(!is_tests_docs_item("Add StageKind::PlanWork variant"));
        assert!(!is_tests_docs_item("Wire event bus in orchestrator"));
    }

    #[test]
    fn verdict_header_includes_phase_id() {
        let report = make_clean_report();
        let with_phase =
            render_review_verdict_and_action(&report, Some("v0.15.19.4"), None, None, false);
        assert!(with_phase.contains("Verdict for v0.15.19.4:"));

        let without_phase = render_review_verdict_and_action(&report, None, None, None, false);
        assert!(without_phase.contains("Verdict:"));
        assert!(!without_phase.contains("Verdict for"));
    }

    // ── v0.15.19.4.2: Source-verification tests ──────────────────────────────

    #[test]
    fn reviewer_passes_planmd_only_when_source_verified() {
        let mut report = make_clean_report();
        report.source_verified = true;
        let output = render_review_verdict_and_action(
            &report,
            Some("v0.15.19.4.1"),
            Some("abc12345"),
            Some("Catch-up goal"),
            false,
        );
        assert!(output.contains("[PASS]"), "should be pass: {}", output);
        assert!(
            output.contains("verified present in source"),
            "should note source verification: {}",
            output
        );
        assert!(
            output.contains("catch-up PLAN.md update"),
            "should note catch-up: {}",
            output
        );
    }

    #[test]
    fn is_planmd_only_draft_detects_single_planmd() {
        assert!(is_planmd_only_draft(&["fs://workspace/PLAN.md"]));
        assert!(is_planmd_only_draft(&["PLAN.md"]));
        assert!(is_planmd_only_draft(&["workspace/PLAN.md"]));
    }

    #[test]
    fn is_planmd_only_draft_rejects_mixed() {
        assert!(!is_planmd_only_draft(&[
            "fs://workspace/PLAN.md",
            "fs://workspace/src/foo.rs"
        ]));
        assert!(!is_planmd_only_draft(&[]));
    }
}
