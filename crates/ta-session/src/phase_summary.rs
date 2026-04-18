// phase_summary.rs — Milestone phase summary builder for the advisor agent (v0.15.19).
//
// At a multi-phase milestone boundary, collects completed phases and builds a
// PhaseSummary for the advisor to present before requesting final human approval.

use serde::{Deserialize, Serialize};

/// A single phase record within a multi-phase milestone summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRecord {
    /// Phase ID or title (e.g. "v0.15.14 — Hierarchical Workflows").
    pub title: String,
    /// Key design decisions made during this phase (from decision log).
    pub decisions: Vec<String>,
    /// Number of files changed in this phase.
    pub files_changed: usize,
    /// Embedded unified diff for this phase (may be truncated for display).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_diff: Option<String>,
    /// File paths changed in this phase.
    #[serde(default)]
    pub changed_files: Vec<String>,
}

impl PhaseRecord {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            decisions: Vec::new(),
            files_changed: 0,
            embedded_diff: None,
            changed_files: Vec::new(),
        }
    }

    pub fn with_decision(mut self, decision: impl Into<String>) -> Self {
        self.decisions.push(decision.into());
        self
    }

    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.files_changed = files.len();
        self.changed_files = files;
        self
    }

    pub fn with_diff(mut self, diff: impl Into<String>) -> Self {
        self.embedded_diff = Some(diff.into());
        self
    }
}

/// A structured summary of multiple phases for milestone review.
///
/// Presented by the advisor before requesting final human sign-off at a
/// multi-phase milestone boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSummary {
    pub phases: Vec<PhaseRecord>,
}

impl PhaseSummary {
    pub fn new() -> Self {
        Self { phases: Vec::new() }
    }

    pub fn add_phase(&mut self, phase: PhaseRecord) {
        self.phases.push(phase);
    }

    /// Total files changed across all phases.
    pub fn total_files_changed(&self) -> usize {
        self.phases.iter().map(|p| p.files_changed).sum()
    }

    /// Total decisions recorded across all phases.
    pub fn total_decisions(&self) -> usize {
        self.phases.iter().map(|p| p.decisions.len()).sum()
    }

    /// Render the summary as the terminal display shown in `ta session run --gate agent`.
    pub fn render_terminal(&self) -> String {
        let mut out = String::new();
        out.push_str("--- Phase Run Summary ---\n");
        let titles: Vec<&str> = self.phases.iter().map(|p| p.title.as_str()).collect();
        out.push_str(&format!("Phases completed: {}\n\n", titles.join(" → ")));

        for phase in &self.phases {
            out.push_str(&format!("Phase {}\n", phase.title));
            if !phase.decisions.is_empty() {
                let dec = phase.decisions.join(", ");
                out.push_str(&format!("  Decisions: {}\n", dec));
            }
            out.push_str(&format!("  Files changed: {}", phase.files_changed));
            if !phase.changed_files.is_empty() {
                let preview: Vec<&str> = phase
                    .changed_files
                    .iter()
                    .map(|s| s.as_str())
                    .take(3)
                    .collect();
                let suffix = if phase.changed_files.len() > 3 {
                    format!(", +{} more", phase.changed_files.len() - 3)
                } else {
                    String::new()
                };
                out.push_str(&format!(" ({}{})", preview.join(", "), suffix));
            }
            out.push('\n');
            if phase.embedded_diff.is_some() {
                out.push_str("  [▶ expand diff]\n");
            }
            out.push('\n');
        }

        out.push_str("Apply all? (y/skip/ask about a phase)\n");
        out.push_str("---\n");
        out
    }
}

impl Default for PhaseSummary {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a PhaseSummary from a list of raw phase data.
///
/// `phase_data` is a slice of `(title, decisions, changed_files)` tuples.
pub fn build_phase_summary(phase_data: &[(&str, Vec<String>, Vec<String>)]) -> PhaseSummary {
    let mut summary = PhaseSummary::new();
    for (title, decisions, files) in phase_data {
        let mut record = PhaseRecord::new(*title);
        record.decisions = decisions.clone();
        record.files_changed = files.len();
        record.changed_files = files.clone();
        summary.add_phase(record);
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_record_builder() {
        let record = PhaseRecord::new("v0.15.14 — Hierarchical Workflows")
            .with_decision("fan-out uses tokio::spawn")
            .with_files(vec![
                "workflow_manager.rs".to_string(),
                "session.rs".to_string(),
            ]);
        assert_eq!(record.title, "v0.15.14 — Hierarchical Workflows");
        assert_eq!(record.decisions.len(), 1);
        assert_eq!(record.files_changed, 2);
    }

    #[test]
    fn phase_summary_totals() {
        let mut summary = PhaseSummary::new();
        summary.add_phase(
            PhaseRecord::new("Phase A")
                .with_decision("decision 1")
                .with_files(vec!["a.rs".to_string(), "b.rs".to_string()]),
        );
        summary.add_phase(PhaseRecord::new("Phase B").with_files(vec!["c.rs".to_string()]));
        assert_eq!(summary.total_files_changed(), 3);
        assert_eq!(summary.total_decisions(), 1);
    }

    #[test]
    fn render_terminal_contains_phase_titles() {
        let summary = build_phase_summary(&[
            (
                "v0.15.14",
                vec!["tokio::spawn for fan-out".to_string()],
                vec!["workflow.rs".to_string()],
            ),
            (
                "v0.15.14.1",
                vec![],
                vec!["plan.rs".to_string(), "session.rs".to_string()],
            ),
        ]);
        let rendered = summary.render_terminal();
        assert!(rendered.contains("v0.15.14 → v0.15.14.1"));
        assert!(rendered.contains("Phase v0.15.14"));
        assert!(rendered.contains("tokio::spawn for fan-out"));
        assert!(rendered.contains("Apply all?"));
    }

    #[test]
    fn render_terminal_expand_diff_marker() {
        let mut summary = PhaseSummary::new();
        let record = PhaseRecord::new("Phase X").with_diff("--- a/foo.rs\n+++ b/foo.rs\n@@ ...");
        summary.add_phase(record);
        let rendered = summary.render_terminal();
        assert!(rendered.contains("[▶ expand diff]"));
    }

    #[test]
    fn phase_summary_serialization() {
        let summary = build_phase_summary(&[(
            "Phase A",
            vec!["decision".to_string()],
            vec!["file.rs".to_string()],
        )]);
        let json = serde_json::to_string_pretty(&summary).unwrap();
        let restored: PhaseSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.phases.len(), 1);
        assert_eq!(restored.phases[0].title, "Phase A");
    }
}
