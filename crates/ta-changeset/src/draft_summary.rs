// draft_summary.rs — Pre-built draft context injected into advisor CLAUDE.md (v0.17.0.3).
//
// `DraftSummary` is built from a `DraftPackage` at advisor-spawn time and injected
// into the advisor's CLAUDE.md so the advisor can present a useful first message
// without calling `ta_draft_view` first.

use serde::{Deserialize, Serialize};

use crate::draft_package::DecisionLogEntry;

/// A brief summary of a changed file for advisor context injection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// Action: "modified", "created", or "deleted".
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub what: Option<String>,
}

/// A single entry in the constitution signals list surfaced by the supervisor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConstitutionSignal {
    pub signal: String,
    /// Severity: "info", "warn", or "error".
    pub severity: String,
}

/// Pre-built draft summary for advisor context injection (v0.17.0.3).
///
/// Populates the advisor's CLAUDE.md with key draft data up front so the
/// advisor can present a useful first message without calling `ta_draft_view`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DraftSummary {
    pub artifact_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_list: Vec<FileDiff>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_log: Vec<DecisionLogEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constitution_signals: Vec<ConstitutionSignal>,
}

impl DraftSummary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the summary as a markdown section for inclusion in advisor CLAUDE.md.
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("## Draft Summary\n\n");
        out.push_str(&format!("**Artifact count**: {}\n\n", self.artifact_count));

        if let Some(ref verdict) = self.supervisor_verdict {
            out.push_str(&format!("**Supervisor verdict**: {}\n\n", verdict));
        }

        if !self.constitution_signals.is_empty() {
            out.push_str("**Constitution signals**:\n");
            for sig in &self.constitution_signals {
                out.push_str(&format!(
                    "- [{}] {}\n",
                    sig.severity.to_uppercase(),
                    sig.signal
                ));
            }
            out.push('\n');
        }

        if !self.file_list.is_empty() {
            out.push_str("**Changed files**:\n");
            for f in &self.file_list {
                if let Some(ref what) = f.what {
                    out.push_str(&format!("- `{}` ({}): {}\n", f.path, f.action, what));
                } else {
                    out.push_str(&format!("- `{}` ({})\n", f.path, f.action));
                }
            }
            out.push('\n');
        }

        if !self.decision_log.is_empty() {
            out.push_str("**Decision log**:\n");
            for d in &self.decision_log {
                out.push_str(&format!("- **{}**: {}\n", d.decision, d.rationale));
            }
            out.push('\n');
        }

        out.push_str("*(Full diff available via `ta_draft_view` or `ta_fs_read`.)*\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_summary_round_trip() {
        let mut s = DraftSummary::new();
        s.artifact_count = 5;
        s.supervisor_verdict = Some("valid".to_string());
        let json = serde_json::to_string(&s).unwrap();
        let restored: DraftSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.artifact_count, 5);
        assert_eq!(restored.supervisor_verdict, Some("valid".to_string()));
    }

    #[test]
    fn draft_summary_default_empty() {
        let s = DraftSummary::default();
        assert_eq!(s.artifact_count, 0);
        assert!(s.supervisor_verdict.is_none());
        assert!(s.file_list.is_empty());
        assert!(s.decision_log.is_empty());
        assert!(s.constitution_signals.is_empty());
    }

    #[test]
    fn draft_summary_render_markdown_includes_all_sections() {
        let mut s = DraftSummary::new();
        s.artifact_count = 2;
        s.supervisor_verdict = Some("no issues".to_string());
        s.file_list.push(FileDiff {
            path: "a.rs".to_string(),
            action: "modified".to_string(),
            what: None,
        });
        s.constitution_signals.push(ConstitutionSignal {
            signal: "test coverage < 80%".to_string(),
            severity: "warn".to_string(),
        });
        let md = s.render_markdown();
        assert!(md.contains("no issues"));
        assert!(md.contains("a.rs"));
        assert!(md.contains('2'.to_string().as_str()));
        assert!(md.contains("[WARN]"));
        assert!(md.contains("test coverage"));
    }

    #[test]
    fn draft_summary_render_markdown_with_decision_log() {
        let mut s = DraftSummary::new();
        s.artifact_count = 1;
        s.decision_log.push(DecisionLogEntry {
            decision: "Use tokio::fs".to_string(),
            rationale: "Async IO needed".to_string(),
            alternatives: vec![],
            alternatives_considered: vec![],
            confidence: None,
            context: None,
        });
        let md = s.render_markdown();
        assert!(md.contains("Use tokio::fs"));
        assert!(md.contains("Async IO needed"));
    }

    #[test]
    fn file_diff_with_what_renders_correctly() {
        let mut s = DraftSummary::new();
        s.artifact_count = 1;
        s.file_list.push(FileDiff {
            path: "src/lib.rs".to_string(),
            action: "modified".to_string(),
            what: Some("Added DraftSummary struct".to_string()),
        });
        let md = s.render_markdown();
        assert!(md.contains("src/lib.rs"));
        assert!(md.contains("Added DraftSummary struct"));
    }

    #[test]
    fn draft_summary_serializes_without_empty_fields() {
        let s = DraftSummary {
            artifact_count: 3,
            supervisor_verdict: None,
            file_list: vec![],
            decision_log: vec![],
            constitution_signals: vec![],
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("supervisor_verdict"));
        assert!(!json.contains("file_list"));
    }
}
