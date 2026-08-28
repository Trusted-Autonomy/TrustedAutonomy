//! Shared glob-overlap matching for resource identifiers — both directions,
//! since a declared resource and a query pattern can each be the more
//! specific side (`"src/**"` querying something that declared
//! `"src/auth.rs"`, or something that declared `"src/**"` being queried
//! with a specific file). Used by [`crate::discovery`] (presence-declared
//! `resources`, historically bare file-glob patterns — `task-graph`'s
//! `api_impact` vocabulary, no URI scheme) and [`crate::staged_conflicts`]
//! (staged artifact `resource_uri`s, e.g. `"fs://workspace/src/main.rs"` —
//! always scheme-prefixed) — factored out here rather than duplicated,
//! since both ask the same question of two different data sources
//! (v0.17.11.7).
//!
//! **Scheme-aware by design**, matching the safety invariant
//! `ta-changeset::uri_pattern::matches_uri` already establishes for
//! artifact URIs (checked directly before writing this — not reinvented
//! blind): an explicit-scheme string (`"gmail://inbox/msg"`) must never
//! glob-match against an unrelated scheme (`"fs://workspace/**"` should
//! not swallow it just because both happen to satisfy a naive glob).
//! Two schemeless (bare) strings — today's entire `discovery.rs` usage —
//! are always scheme-compatible, so this changes nothing for existing
//! presence-glob matching; it only guards the new URI-based path.

use glob::Pattern;

/// Does `declared` overlap any of `queries`, glob-matched in both
/// directions, gated by scheme compatibility? Malformed patterns on either
/// side are treated as non-matching rather than erroring — a bad glob
/// shouldn't take down an otherwise-working query.
pub fn glob_overlap(declared: &str, queries: &[String]) -> bool {
    queries.iter().any(|q| pair_overlap(declared, q))
}

fn pair_overlap(a: &str, b: &str) -> bool {
    if !schemes_compatible(a, b) {
        return false;
    }
    glob_matches(a, b) || glob_matches(b, a)
}

/// `pattern` interpreted as a glob, matched against literal string
/// `candidate`. Invalid patterns fail closed (no match, no panic).
fn glob_matches(pattern: &str, candidate: &str) -> bool {
    Pattern::new(pattern).is_ok_and(|p| p.matches(candidate))
}

/// Two resource strings are scheme-compatible if neither has an explicit
/// `scheme://` prefix (both bare — today's only `discovery.rs` case,
/// unaffected by this guard), or if both do and the schemes are equal.
/// A bare string paired with an explicit-scheme string is never
/// compatible — this is the actual safety fix: without it, a bare glob
/// like `"**"` could spuriously match `"gmail://inbox/msg"`.
fn schemes_compatible(a: &str, b: &str) -> bool {
    match (scheme_of(a), scheme_of(b)) {
        (Some(sa), Some(sb)) => sa == sb,
        (None, None) => true,
        _ => false,
    }
}

fn scheme_of(s: &str) -> Option<&str> {
    s.split_once("://").map(|(scheme, _)| scheme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_when_query_is_the_broader_glob() {
        assert!(glob_overlap(
            "src/auth/login.rs",
            &["src/auth/**".to_string()]
        ));
    }

    #[test]
    fn matches_when_declared_is_the_broader_glob() {
        assert!(glob_overlap(
            "src/auth/**",
            &["src/auth/login.rs".to_string()]
        ));
    }

    #[test]
    fn matches_exact_equal_strings() {
        assert!(glob_overlap("src/lib.rs", &["src/lib.rs".to_string()]));
    }

    #[test]
    fn no_match_for_disjoint_paths() {
        assert!(!glob_overlap("docs/**", &["src/**".to_string()]));
    }

    #[test]
    fn malformed_declared_pattern_does_not_match_or_panic() {
        // An unbalanced bracket is not a valid glob pattern on either side —
        // must fail closed (no match), not panic or error out the caller.
        assert!(!glob_overlap(
            "src/[unbalanced",
            &["src/foo.rs".to_string()]
        ));
    }

    #[test]
    fn empty_queries_never_match() {
        assert!(!glob_overlap("src/lib.rs", &[]));
    }

    // ── scheme-awareness (the fix found by cross-checking ta-changeset::uri_pattern) ──

    #[test]
    fn same_scheme_uris_glob_match() {
        assert!(glob_overlap(
            "fs://workspace/src/auth.rs",
            &["fs://workspace/src/**".to_string()]
        ));
    }

    #[test]
    fn different_schemes_never_match_even_if_globs_would_overlap() {
        // A pattern like "**" or "*://*" style broad matches must not leak
        // across schemes — gmail and fs are unrelated resource spaces.
        assert!(!glob_overlap(
            "gmail://inbox/msg-123",
            &["fs://workspace/**".to_string()]
        ));
    }

    #[test]
    fn bare_pattern_never_matches_a_scheme_prefixed_uri() {
        // The actual safety fix: without scheme-gating, a bare "**"-style
        // glob could accidentally swallow a scheme-prefixed resource_uri.
        assert!(!glob_overlap("gmail://inbox/msg-123", &["**".to_string()]));
    }

    #[test]
    fn two_bare_patterns_are_always_scheme_compatible() {
        // Existing discovery.rs behavior, unaffected by the scheme guard —
        // both sides bare means both sides implicitly compatible.
        assert!(glob_overlap("src/**", &["src/lib.rs".to_string()]));
    }
}
