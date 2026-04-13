// phase_selector.rs — PhaseSelector for multi-phase workflow orchestration (v0.15.14).
//
// Resolves a `[phases]` config block against a list of plan phases.
// Three selection modes: Count, VersionSet (glob), and Range (inclusive).

use serde::{Deserialize, Serialize};

/// A simplified plan phase record used by the selector.
/// Mirrors the relevant fields from `apps/ta-cli/src/commands/plan.rs:PlanPhase`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedPhase {
    pub id: String,
    pub title: String,
}

impl SelectedPhase {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        SelectedPhase {
            id: id.into(),
            title: title.into(),
        }
    }
}

/// Configuration for how to select a subset of plan phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PhaseSelectorConfig {
    /// Return at most N pending phases.
    Count(u32),
    /// Glob pattern match against phase IDs (only `*` wildcard supported).
    /// E.g. `"v0.15.*"` matches `v0.15.14` but not `v0.14.3`.
    VersionSet(String),
    /// Inclusive range: phases with ID `>= from` and `<= to` (semver ordering).
    Range { from: String, to: String },
}

/// Resolves a `PhaseSelectorConfig` against a list of pending plan phases.
pub struct PhaseSelector;

impl PhaseSelector {
    /// Filter `phases` to the subset matching `config`.
    ///
    /// All inputs are expected to be pending phases (pre-filtered by caller).
    /// The selector applies its own matching logic on top.
    pub fn resolve(phases: &[SelectedPhase], config: &PhaseSelectorConfig) -> Vec<SelectedPhase> {
        match config {
            PhaseSelectorConfig::Count(n) => phases.iter().take(*n as usize).cloned().collect(),
            PhaseSelectorConfig::VersionSet(pattern) => phases
                .iter()
                .filter(|p| glob_match(pattern, &p.id))
                .cloned()
                .collect(),
            PhaseSelectorConfig::Range { from, to } => phases
                .iter()
                .filter(|p| {
                    version_cmp(&p.id, from) != std::cmp::Ordering::Less
                        && version_cmp(&p.id, to) != std::cmp::Ordering::Greater
                })
                .cloned()
                .collect(),
        }
    }
}

/// Simple wildcard glob matching. Only `*` is supported as a wildcard.
///
/// `v0.15.*` matches `v0.15.14`, `v0.15.1`, but not `v0.14.3`.
fn glob_match(pattern: &str, value: &str) -> bool {
    let parts: Vec<&str> = pattern.splitn(2, '*').collect();
    match parts.as_slice() {
        [only] => *only == value,
        [prefix, suffix] => {
            value.starts_with(prefix)
                && value[prefix.len()..].ends_with(suffix)
                && value.len() >= prefix.len() + suffix.len()
        }
        _ => false,
    }
}

/// Compare two version strings (e.g. `v0.15.14` vs `v0.15.5`) by splitting on
/// `.` and comparing each numeric segment. The leading `v` is stripped first.
///
/// Non-numeric segments are compared lexicographically as fallback.
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a_parts = parse_version(a);
    let b_parts = parse_version(b);
    a_parts.cmp(&b_parts)
}

/// Parse a version string like `v0.15.14` into a comparable numeric vector.
/// Strips a leading `v`, then splits on `.`.
/// Numeric parts become `(u64, "")`, non-numeric parts become `(0, part)`.
fn parse_version(s: &str) -> Vec<(u64, String)> {
    let stripped = s.strip_prefix('v').unwrap_or(s);
    stripped
        .split('.')
        .map(|part| {
            if let Ok(n) = part.parse::<u64>() {
                (n, String::new())
            } else {
                (0, part.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phases(ids: &[&str]) -> Vec<SelectedPhase> {
        ids.iter()
            .map(|id| SelectedPhase::new(*id, format!("Phase {}", id)))
            .collect()
    }

    #[test]
    fn phase_selector_count() {
        let input = phases(&["v0.15.1", "v0.15.2", "v0.15.3", "v0.15.4"]);
        let result = PhaseSelector::resolve(&input, &PhaseSelectorConfig::Count(2));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "v0.15.1");
        assert_eq!(result[1].id, "v0.15.2");
    }

    #[test]
    fn phase_selector_count_more_than_available() {
        let input = phases(&["v0.15.1"]);
        let result = PhaseSelector::resolve(&input, &PhaseSelectorConfig::Count(5));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn phase_selector_count_zero() {
        let input = phases(&["v0.15.1", "v0.15.2"]);
        let result = PhaseSelector::resolve(&input, &PhaseSelectorConfig::Count(0));
        assert!(result.is_empty());
    }

    #[test]
    fn phase_selector_version_set_matches() {
        let input = phases(&["v0.15.1", "v0.15.14", "v0.14.3", "v0.16.0"]);
        let result = PhaseSelector::resolve(
            &input,
            &PhaseSelectorConfig::VersionSet("v0.15.*".to_string()),
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "v0.15.1");
        assert_eq!(result[1].id, "v0.15.14");
    }

    #[test]
    fn phase_selector_version_set_no_wildcard() {
        let input = phases(&["v0.15.14", "v0.15.1"]);
        let result = PhaseSelector::resolve(
            &input,
            &PhaseSelectorConfig::VersionSet("v0.15.14".to_string()),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "v0.15.14");
    }

    #[test]
    fn phase_selector_version_set_no_match() {
        let input = phases(&["v0.14.3"]);
        let result = PhaseSelector::resolve(
            &input,
            &PhaseSelectorConfig::VersionSet("v0.15.*".to_string()),
        );
        assert!(result.is_empty());
    }

    #[test]
    fn phase_selector_range_inclusive() {
        let input = phases(&[
            "v0.15.4", "v0.15.5", "v0.15.6", "v0.15.7", "v0.15.8", "v0.15.9",
        ]);
        let result = PhaseSelector::resolve(
            &input,
            &PhaseSelectorConfig::Range {
                from: "v0.15.5".to_string(),
                to: "v0.15.8".to_string(),
            },
        );
        assert_eq!(result.len(), 4);
        let ids: Vec<&str> = result.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["v0.15.5", "v0.15.6", "v0.15.7", "v0.15.8"]);
    }

    #[test]
    fn phase_selector_range_excludes_outside() {
        let input = phases(&["v0.15.4", "v0.15.5", "v0.15.9"]);
        let result = PhaseSelector::resolve(
            &input,
            &PhaseSelectorConfig::Range {
                from: "v0.15.5".to_string(),
                to: "v0.15.8".to_string(),
            },
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "v0.15.5");
    }

    #[test]
    fn glob_match_wildcard_prefix() {
        assert!(glob_match("v0.15.*", "v0.15.14"));
        assert!(glob_match("v0.15.*", "v0.15.1"));
        assert!(!glob_match("v0.15.*", "v0.14.3"));
        assert!(!glob_match("v0.15.*", "v0.15"));
    }

    #[test]
    fn glob_match_no_wildcard() {
        assert!(glob_match("v0.15.14", "v0.15.14"));
        assert!(!glob_match("v0.15.14", "v0.15.1"));
    }

    #[test]
    fn version_ordering() {
        assert_eq!(
            version_cmp("v0.15.14", "v0.15.5"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(version_cmp("v0.15.5", "v0.15.14"), std::cmp::Ordering::Less);
        assert_eq!(version_cmp("v0.15.5", "v0.15.5"), std::cmp::Ordering::Equal);
    }
}
