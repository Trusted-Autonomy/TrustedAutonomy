// kpi_alignment.rs — In-process Meridian KPI alignment scoring (v0.17.0.13).
//
// Meridian's full regression engine runs out-of-process via `meridian suggest`
// (see meridian.rs). This module is a lightweight, dependency-free scorer
// that TA runs in-process — no subprocess, no `meridian` binary required —
// against the KPI definitions in `meridian.toml`. It powers `ta plan status
// --kpi`, `ta meridian suggest --phases`, and the one-line alignment hint
// `ta run` prints when a phase is claimed.
//
// This is deliberately simpler than Meridian's own regression engine: it
// classifies a phase by keyword overlap between the phase's title+description
// and each configured KPI's keywords, picking the highest-scoring KPI as the
// phase's category. It's meant as a fast, always-available approximation —
// not a replacement for `meridian analyze`/`meridian suggest`'s full reports,
// which still require the binary and give Meridian's own regression output.
//
// `meridian.toml` KPI schema TA reads (other Meridian-owned fields in the
// same file are ignored and left untouched):
//
//   [[kpi]]
//   name = "Shipping Velocity"
//   category = "velocity"
//   keywords = ["release", "ship", "deploy", "milestone"]
//   weight = 1.0

use serde::Deserialize;
use std::path::Path;

use super::plan::{extract_phase_description, PlanPhase};

/// One KPI definition read from `meridian.toml`'s `[[kpi]]` array.
#[derive(Debug, Clone, Deserialize)]
pub struct KpiDefinition {
    pub name: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_category() -> String {
    "general".to_string()
}

fn default_weight() -> f64 {
    1.0
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MeridianTomlFile {
    #[serde(default, rename = "kpi")]
    kpis: Vec<KpiDefinition>,
}

/// Alignment result for a single plan phase.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseAlignment {
    pub phase_id: String,
    pub title: String,
    /// Best-matching KPI name, if any keyword overlap was found.
    pub best_kpi: Option<String>,
    pub category: Option<String>,
    /// 0.0 (no match) .. 1.0 (every configured keyword for the winning KPI present).
    pub score: f64,
}

/// Load `meridian.toml`'s `[[kpi]]` entries from the project root.
///
/// Returns `None` when the file doesn't exist, fails to parse, or has no
/// `[[kpi]]` entries — callers treat this as "KPI scoring unavailable", never
/// a hard error, since Meridian configuration is entirely optional.
pub fn load_kpi_definitions(workspace_root: &Path) -> Option<Vec<KpiDefinition>> {
    let path = workspace_root.join("meridian.toml");
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: MeridianTomlFile = toml::from_str(&content).ok()?;
    if parsed.kpis.is_empty() {
        None
    } else {
        Some(parsed.kpis)
    }
}

/// Score `text` (typically a phase's title + description) against `kpis`.
///
/// Matching is a simple case-insensitive keyword-overlap: for each KPI, count
/// how many of its keywords appear in the text, weight by
/// `KpiDefinition::weight`, and normalize by the KPI's own keyword count so
/// KPIs with more keywords aren't unfairly favored. The highest-scoring KPI
/// with at least one match wins; ties keep the earliest KPI in config order.
///
/// Returns `(index of winning KPI, score)`; `(None, 0.0)` when nothing matches.
pub fn score_phase_text(text: &str, kpis: &[KpiDefinition]) -> (Option<usize>, f64) {
    let lower = text.to_lowercase();
    let mut best: Option<(usize, f64)> = None;

    for (idx, kpi) in kpis.iter().enumerate() {
        if kpi.keywords.is_empty() {
            continue;
        }
        let matches = kpi
            .keywords
            .iter()
            .filter(|kw| !kw.is_empty() && lower.contains(&kw.to_lowercase()))
            .count();
        if matches == 0 {
            continue;
        }
        let ratio = matches as f64 / kpi.keywords.len() as f64;
        let weighted = (ratio * kpi.weight).min(1.0);
        if best.is_none_or(|(_, best_score)| weighted > best_score) {
            best = Some((idx, weighted));
        }
    }

    match best {
        Some((idx, score)) => (Some(idx), score),
        None => (None, 0.0),
    }
}

fn alignment_for(
    phase_id: &str,
    title: &str,
    description: &str,
    kpis: &[KpiDefinition],
) -> PhaseAlignment {
    let text = format!("{} {}", title, description);
    let (best_idx, score) = score_phase_text(&text, kpis);
    let (best_kpi, category) = match best_idx {
        Some(idx) => (
            Some(kpis[idx].name.clone()),
            Some(kpis[idx].category.clone()),
        ),
        None => (None, None),
    };
    PhaseAlignment {
        phase_id: phase_id.to_string(),
        title: title.to_string(),
        best_kpi,
        category,
        score,
    }
}

/// Compute per-phase KPI alignment for every phase in `phases`.
///
/// `plan_content` is the raw PLAN.md text (used to pull each phase's
/// description body via `extract_phase_description`).
pub fn compute_phase_alignments(
    phases: &[PlanPhase],
    plan_content: &str,
    kpis: &[KpiDefinition],
) -> Vec<PhaseAlignment> {
    phases
        .iter()
        .map(|phase| {
            let description = extract_phase_description(plan_content, &phase.id, 500);
            alignment_for(&phase.id, &phase.title, &description, kpis)
        })
        .collect()
}

/// Compute alignment for exactly one phase — used by `ta run`'s one-line hint.
///
/// Returns `None` when no KPI definitions are configured; a configured-but-
/// unmatched phase still returns `Some` with `best_kpi: None, score: 0.0`.
pub fn score_one_phase(
    workspace_root: &Path,
    plan_content: &str,
    phase_id: &str,
    phase_title: &str,
) -> Option<PhaseAlignment> {
    let kpis = load_kpi_definitions(workspace_root)?;
    let description = extract_phase_description(plan_content, phase_id, 500);
    Some(alignment_for(phase_id, phase_title, &description, &kpis))
}

/// One-line, human-readable summary for `ta run`'s informational hint.
pub fn format_one_line(alignment: &PhaseAlignment) -> String {
    match (&alignment.best_kpi, &alignment.category) {
        (Some(kpi), Some(category)) => format!(
            "KPI alignment: {} — {} ({:.0}% match, category: {})",
            alignment.phase_id,
            kpi,
            alignment.score * 100.0,
            category
        ),
        _ => format!(
            "KPI alignment: {} — no matching KPI configured (unclassified)",
            alignment.phase_id
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn kpi(name: &str, category: &str, keywords: &[&str], weight: f64) -> KpiDefinition {
        KpiDefinition {
            name: name.to_string(),
            category: category.to_string(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            weight,
        }
    }

    #[test]
    fn load_kpi_definitions_returns_none_when_file_missing() {
        let tmp = tempdir().unwrap();
        assert!(load_kpi_definitions(tmp.path()).is_none());
    }

    #[test]
    fn load_kpi_definitions_returns_none_when_no_kpi_entries() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("meridian.toml"), "[general]\nfoo = 1\n").unwrap();
        assert!(load_kpi_definitions(tmp.path()).is_none());
    }

    #[test]
    fn load_kpi_definitions_parses_kpi_array() {
        let tmp = tempdir().unwrap();
        std::fs::write(
            tmp.path().join("meridian.toml"),
            r#"
[[kpi]]
name = "Shipping Velocity"
category = "velocity"
keywords = ["release", "ship", "deploy"]
weight = 1.0

[[kpi]]
name = "Reliability"
category = "reliability"
keywords = ["test", "bug", "regression"]
"#,
        )
        .unwrap();
        let kpis = load_kpi_definitions(tmp.path()).unwrap();
        assert_eq!(kpis.len(), 2);
        assert_eq!(kpis[0].name, "Shipping Velocity");
        assert_eq!(kpis[1].weight, 1.0); // default_weight
    }

    #[test]
    fn score_phase_text_matches_highest_overlap_kpi() {
        let kpis = vec![
            kpi("Velocity", "velocity", &["release", "ship", "deploy"], 1.0),
            kpi("Reliability", "reliability", &["test", "bug"], 1.0),
        ];
        let (idx, score) = score_phase_text("Fix a regression bug in the test suite", &kpis);
        assert_eq!(idx, Some(1));
        assert!(score > 0.0);
    }

    #[test]
    fn score_phase_text_no_match_returns_none_and_zero() {
        let kpis = vec![kpi("Velocity", "velocity", &["release", "ship"], 1.0)];
        let (idx, score) = score_phase_text("Unrelated phase about documentation", &kpis);
        assert_eq!(idx, None);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn score_phase_text_empty_keywords_are_skipped() {
        let kpis = vec![kpi("Empty", "misc", &[], 1.0)];
        let (idx, score) = score_phase_text("anything at all", &kpis);
        assert_eq!(idx, None);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn score_phase_text_weight_caps_at_one() {
        let kpis = vec![kpi("Overweighted", "velocity", &["ship"], 5.0)];
        let (idx, score) = score_phase_text("ship it", &kpis);
        assert_eq!(idx, Some(0));
        assert_eq!(score, 1.0);
    }

    #[test]
    fn compute_phase_alignments_covers_all_phases() {
        let phases = vec![
            PlanPhase {
                id: "1".to_string(),
                title: "Release Pipeline".to_string(),
                status: super::super::plan::PlanStatus::Done,
                depends_on: vec![],
                human_review_items: vec![],
                api_impact: vec![],
            },
            PlanPhase {
                id: "2".to_string(),
                title: "Docs Cleanup".to_string(),
                status: super::super::plan::PlanStatus::Pending,
                depends_on: vec![],
                human_review_items: vec![],
                api_impact: vec![],
            },
        ];
        let kpis = vec![kpi("Velocity", "velocity", &["release", "ship"], 1.0)];
        let plan_content = "## Phase 1\nRelease Pipeline\n## Phase 2\nDocs Cleanup\n";
        let results = compute_phase_alignments(&phases, plan_content, &kpis);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].best_kpi.as_deref(), Some("Velocity"));
        assert_eq!(results[1].best_kpi, None);
    }

    #[test]
    fn score_one_phase_returns_none_without_config() {
        let tmp = tempdir().unwrap();
        let result = score_one_phase(tmp.path(), "## Phase 1\nTitle\n", "1", "Release it");
        assert!(result.is_none());
    }

    #[test]
    fn score_one_phase_returns_alignment_with_config() {
        let tmp = tempdir().unwrap();
        std::fs::write(
            tmp.path().join("meridian.toml"),
            "[[kpi]]\nname = \"Velocity\"\ncategory = \"velocity\"\nkeywords = [\"release\"]\n",
        )
        .unwrap();
        let result = score_one_phase(tmp.path(), "## Phase 1\nTitle\n", "1", "Release it").unwrap();
        assert_eq!(result.best_kpi.as_deref(), Some("Velocity"));
        assert!(result.score > 0.0);
    }

    #[test]
    fn format_one_line_matched() {
        let alignment = PhaseAlignment {
            phase_id: "v0.17.0.13".to_string(),
            title: "Meridian KPI Regression".to_string(),
            best_kpi: Some("Velocity".to_string()),
            category: Some("velocity".to_string()),
            score: 0.5,
        };
        let line = format_one_line(&alignment);
        assert!(line.contains("v0.17.0.13"));
        assert!(line.contains("Velocity"));
        assert!(line.contains("50%"));
    }

    #[test]
    fn format_one_line_unmatched() {
        let alignment = PhaseAlignment {
            phase_id: "v0.17.0.13".to_string(),
            title: "Meridian KPI Regression".to_string(),
            best_kpi: None,
            category: None,
            score: 0.0,
        };
        let line = format_one_line(&alignment);
        assert!(line.contains("no matching KPI configured"));
    }
}
