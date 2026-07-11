//! [`Priority`] — how urgently a routed request should be worked, relative
//! to other pending requests. New concept for v0.17.0.12.20; no prior
//! `Priority`/urgency type existed anywhere in the codebase before this
//! phase (confirmed by search of the `ta-goal`/`ta-session` crates).

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::str::FromStr;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

impl Priority {
    /// Ordinal used for sorting — higher value sorts first (most urgent).
    fn rank(self) -> u8 {
        match self {
            Priority::Low => 0,
            Priority::Normal => 1,
            Priority::High => 2,
            Priority::Urgent => 3,
        }
    }
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Priority::Low => write!(f, "low"),
            Priority::Normal => write!(f, "normal"),
            Priority::High => write!(f, "high"),
            Priority::Urgent => write!(f, "urgent"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid priority '{0}': expected one of low, normal, high, urgent")]
pub struct InvalidPriority(String);

impl FromStr for Priority {
    type Err = InvalidPriority;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Priority::Low),
            "normal" => Ok(Priority::Normal),
            "high" => Ok(Priority::High),
            "urgent" | "critical" => Ok(Priority::Urgent),
            other => Err(InvalidPriority(other.to_string())),
        }
    }
}

/// Keyword-based urgency detection, used as the lowest-priority tier in
/// `route()`'s priority resolution when no explicit/config-driven priority
/// is set. Purely a heuristic signal, always overridable by an explicit
/// `--priority` flag or config binding.
pub(crate) fn detect_urgency(text: &str) -> Priority {
    let lower = text.to_ascii_lowercase();
    const URGENT_WORDS: &[&str] = &[
        "urgent",
        "critical",
        "prod down",
        "production down",
        "outage",
        "sev1",
        "sev-1",
        "hotfix",
        "security vulnerability",
        "data loss",
    ];
    const HIGH_WORDS: &[&str] = &["bug", "fix", "broken", "regression", "failing", "crash"];
    const LOW_WORDS: &[&str] = &[
        "docs",
        "documentation",
        "chore",
        "cleanup",
        "typo",
        "readme",
    ];

    if URGENT_WORDS.iter().any(|w| lower.contains(w)) {
        Priority::Urgent
    } else if HIGH_WORDS.iter().any(|w| lower.contains(w)) {
        Priority::High
    } else if LOW_WORDS.iter().any(|w| lower.contains(w)) {
        Priority::Low
    } else {
        Priority::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_variants_case_insensitively() {
        assert_eq!("LOW".parse::<Priority>().unwrap(), Priority::Low);
        assert_eq!("Normal".parse::<Priority>().unwrap(), Priority::Normal);
        assert_eq!("high".parse::<Priority>().unwrap(), Priority::High);
        assert_eq!("URGENT".parse::<Priority>().unwrap(), Priority::Urgent);
        assert_eq!("critical".parse::<Priority>().unwrap(), Priority::Urgent);
    }

    #[test]
    fn rejects_invalid_priority() {
        assert!("whenever".parse::<Priority>().is_err());
    }

    #[test]
    fn orders_low_to_urgent() {
        let mut v = vec![
            Priority::High,
            Priority::Low,
            Priority::Urgent,
            Priority::Normal,
        ];
        v.sort();
        assert_eq!(
            v,
            vec![
                Priority::Low,
                Priority::Normal,
                Priority::High,
                Priority::Urgent
            ]
        );
    }

    #[test]
    fn detects_urgent_keywords() {
        assert_eq!(
            detect_urgency("production down, need a hotfix now"),
            Priority::Urgent
        );
    }

    #[test]
    fn detects_high_keywords() {
        assert_eq!(detect_urgency("fix the login bug"), Priority::High);
    }

    #[test]
    fn detects_low_keywords() {
        assert_eq!(detect_urgency("update the README docs"), Priority::Low);
    }

    #[test]
    fn defaults_to_normal() {
        assert_eq!(
            detect_urgency("add a new dashboard widget"),
            Priority::Normal
        );
        assert_eq!(Priority::default(), Priority::Normal);
    }
}
