// access_filter.rs — Unified allow/deny list pattern (v0.9.8.1.1).
//
// Reusable access filter with consistent semantics across all TA subsystems:
//   - Deny always takes precedence over allow.
//   - Empty `allowed` = allow all.
//   - Empty `denied` = deny nothing.
//
// Used by: daemon command routing, auto-approval paths, sandbox commands,
// network policy, and channel access control.

use glob::Pattern;
use serde::{Deserialize, Serialize};

/// Reusable allow/deny filter. Deny always takes precedence.
///
/// Supports glob patterns (via the `glob` crate) for flexible matching.
/// The evaluation logic:
///   1. If any `denied` pattern matches → **false** (deny always wins)
///   2. If `allowed` is empty → **true** (empty allow = allow all)
///   3. If any `allowed` pattern matches → **true**
///   4. Otherwise → **false**
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccessFilter {
    /// Glob patterns for permitted inputs. Empty = allow all.
    #[serde(default)]
    pub allowed: Vec<String>,
    /// Glob patterns for denied inputs. Empty = deny nothing.
    /// Deny always takes precedence over allow.
    #[serde(default)]
    pub denied: Vec<String>,
}

impl AccessFilter {
    /// Create a filter that allows everything (default).
    pub fn allow_all() -> Self {
        Self {
            allowed: Vec::new(),
            denied: Vec::new(),
        }
    }

    /// Create a filter from only an allow list (backward compat).
    pub fn from_allowed(allowed: Vec<String>) -> Self {
        Self {
            allowed,
            denied: Vec::new(),
        }
    }

    /// Create a filter from separate allow and deny lists.
    pub fn new(allowed: Vec<String>, denied: Vec<String>) -> Self {
        Self { allowed, denied }
    }

    /// Returns true if the input is permitted.
    ///
    /// Logic:
    ///   1. If any `denied` pattern matches → false (deny always wins)
    ///   2. If `allowed` is empty → true (allow all)
    ///   3. If any `allowed` pattern matches → true
    ///   4. Otherwise → false
    pub fn permits(&self, input: &str) -> bool {
        // Deny takes precedence.
        for pattern in &self.denied {
            if matches_pattern(pattern, input) {
                return false;
            }
        }

        // Empty allowed = allow all.
        if self.allowed.is_empty() {
            return true;
        }

        // Check allow list.
        self.allowed
            .iter()
            .any(|pattern| matches_pattern(pattern, input))
    }

    /// Returns true if both allowed and denied are empty (permits everything).
    pub fn is_unrestricted(&self) -> bool {
        self.allowed.is_empty() && self.denied.is_empty()
    }

    /// Merge two filters with tighten-only semantics:
    /// - denied lists are unioned (more restrictions)
    /// - allowed lists are intersected (if both non-empty), or the non-empty one wins
    pub fn tighten(&self, other: &AccessFilter) -> AccessFilter {
        // Union of denied (more restrictions).
        let mut denied = self.denied.clone();
        for p in &other.denied {
            if !denied.contains(p) {
                denied.push(p.clone());
            }
        }

        // Intersection of allowed (more restrictive).
        let allowed = if self.allowed.is_empty() {
            other.allowed.clone()
        } else if other.allowed.is_empty() {
            self.allowed.clone()
        } else {
            self.allowed
                .iter()
                .filter(|p| other.allowed.contains(p))
                .cloned()
                .collect()
        };

        AccessFilter { allowed, denied }
    }
}

/// Match a pattern against input. Supports glob patterns via the `glob` crate,
/// with a fast path for simple cases (exact match, `*`, prefix `*`, suffix `*`).
fn matches_pattern(pattern: &str, input: &str) -> bool {
    // Fast paths for common simple patterns.
    if pattern == "*" {
        return true;
    }
    if pattern == input {
        return true;
    }

    // Use glob::Pattern for full glob support.
    Pattern::new(pattern)
        .map(|p| p.matches(input))
        .unwrap_or(false)
}

impl std::fmt::Display for AccessFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_unrestricted() {
            write!(f, "allow all")
        } else if self.allowed.is_empty() {
            write!(f, "deny: [{}]", self.denied.join(", "))
        } else if self.denied.is_empty() {
            write!(f, "allow: [{}]", self.allowed.join(", "))
        } else {
            write!(
                f,
                "allow: [{}], deny: [{}]",
                self.allowed.join(", "),
                self.denied.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_allows_all() {
        let filter = AccessFilter::allow_all();
        assert!(filter.permits("anything"));
        assert!(filter.permits(""));
        assert!(filter.is_unrestricted());
    }

    #[test]
    fn deny_takes_precedence_over_allow() {
        let filter = AccessFilter::new(
            vec!["**".to_string()],    // allow everything
            vec!["*.exe".to_string()], // deny .exe
        );
        assert!(filter.permits("script.sh"));
        assert!(!filter.permits("malware.exe"));
    }

    #[test]
    fn empty_allowed_means_allow_all() {
        let filter = AccessFilter::new(
            vec![],                         // empty = allow all
            vec!["secret.key".to_string()], // deny specific file
        );
        assert!(filter.permits("normal.txt"));
        assert!(!filter.permits("secret.key"));
    }

    #[test]
    fn non_empty_allowed_restricts() {
        let filter =
            AccessFilter::from_allowed(vec!["tests/**".to_string(), "docs/**".to_string()]);
        assert!(filter.permits("tests/foo.rs"));
        assert!(filter.permits("docs/readme.md"));
        assert!(!filter.permits("src/main.rs"));
    }

    #[test]
    fn glob_patterns_work() {
        let filter = AccessFilter::new(
            vec!["src/**/*.rs".to_string()],
            vec!["**/secret*".to_string()],
        );
        assert!(filter.permits("src/lib.rs"));
        assert!(filter.permits("src/commands/run.rs"));
        assert!(!filter.permits("src/secret_key.rs"));
        assert!(!filter.permits("build/output.js"));
    }

    #[test]
    fn exact_match() {
        let filter = AccessFilter::from_allowed(vec!["ta status".to_string()]);
        assert!(filter.permits("ta status"));
        assert!(!filter.permits("ta goal list"));
    }

    #[test]
    fn wildcard_star_matches_all() {
        let filter = AccessFilter::from_allowed(vec!["*".to_string()]);
        assert!(filter.permits("anything"));
    }

    #[test]
    fn command_style_glob() {
        // Matches the pattern used by daemon command routing.
        let filter = AccessFilter::new(
            vec!["ta draft *".to_string(), "ta status".to_string()],
            vec!["ta draft apply *".to_string()],
        );
        assert!(filter.permits("ta draft list"));
        assert!(filter.permits("ta status"));
        assert!(!filter.permits("ta draft apply abc123")); // denied
        assert!(!filter.permits("ta goal start foo")); // not in allowed
    }

    #[test]
    fn backward_compat_from_allowed() {
        let filter = AccessFilter::from_allowed(vec!["*".to_string()]);
        assert!(filter.permits("anything"));
        assert!(filter.denied.is_empty());
    }

    #[test]
    fn tighten_unions_denied() {
        let a = AccessFilter::new(vec![], vec!["a".to_string()]);
        let b = AccessFilter::new(vec![], vec!["b".to_string()]);
        let merged = a.tighten(&b);
        assert_eq!(merged.denied, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn tighten_intersects_allowed() {
        let a = AccessFilter::from_allowed(vec!["x".to_string(), "y".to_string()]);
        let b = AccessFilter::from_allowed(vec!["y".to_string(), "z".to_string()]);
        let merged = a.tighten(&b);
        assert_eq!(merged.allowed, vec!["y".to_string()]);
    }

    #[test]
    fn tighten_empty_allowed_uses_other() {
        let a = AccessFilter::allow_all();
        let b = AccessFilter::from_allowed(vec!["only-this".to_string()]);
        let merged = a.tighten(&b);
        assert_eq!(merged.allowed, vec!["only-this".to_string()]);
    }

    #[test]
    fn tighten_deduplicates_denied() {
        let a = AccessFilter::new(vec![], vec!["x".to_string()]);
        let b = AccessFilter::new(vec![], vec!["x".to_string()]);
        let merged = a.tighten(&b);
        assert_eq!(merged.denied, vec!["x".to_string()]);
    }

    #[test]
    fn display_unrestricted() {
        assert_eq!(format!("{}", AccessFilter::allow_all()), "allow all");
    }

    #[test]
    fn display_with_lists() {
        let filter = AccessFilter::new(vec!["a".to_string()], vec!["b".to_string()]);
        assert_eq!(format!("{}", filter), "allow: [a], deny: [b]");
    }

    #[test]
    fn serde_round_trip() {
        let filter =
            AccessFilter::new(vec!["tests/**".to_string()], vec!["**/secret*".to_string()]);
        let json = serde_json::to_string(&filter).unwrap();
        let restored: AccessFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, restored);
    }

    #[test]
    fn serde_yaml_round_trip() {
        let filter = AccessFilter::new(vec!["src/**".to_string()], vec!["*.key".to_string()]);
        let yaml = serde_yaml::to_string(&filter).unwrap();
        let restored: AccessFilter = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(filter, restored);
    }

    #[test]
    fn empty_denied_denies_nothing() {
        let filter = AccessFilter::from_allowed(vec!["*".to_string()]);
        assert!(filter.permits("anything"));
        // No denied patterns means nothing is denied.
    }

    #[test]
    fn permits_with_path_globs() {
        let filter = AccessFilter::new(
            vec!["**".to_string()],
            vec![".ta/**".to_string(), "**/main.rs".to_string()],
        );
        assert!(filter.permits("tests/foo.rs"));
        assert!(!filter.permits(".ta/config.yaml"));
        assert!(!filter.permits("src/main.rs"));
    }
}
