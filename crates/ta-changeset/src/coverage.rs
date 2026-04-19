//! Item coverage checker for Draft Pre-Apply Plan Review.
//!
//! For each unchecked `[ ]` item in the phase being applied, extracts 2-4
//! significant tokens and checks whether any appear in the draft artifact diffs.
//! Heuristic only — never blocks apply.

use crate::review_report::CoverageGap;

/// Extract 2-4 significant tokens from a plan item description.
/// Filters out common English stop-words and short tokens.
fn extract_tokens(text: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "the", "and", "or", "in", "on", "at", "to", "for", "of", "with", "by", "from",
        "as", "is", "it", "its", "be", "this", "that", "are", "was", "will", "should", "when",
        "if", "all", "any", "not", "no", "so", "do", "new", "each", "per", "via", "into", "add",
        "use", "run", "get", "set", "has", "can", "may", "must", "after", "before", "up", "out",
        "into", "over", "then", "than",
    ];

    let tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| {
            let lower = t.to_lowercase();
            t.len() >= 4 && !STOP_WORDS.contains(&lower.as_str())
        })
        .map(|t| t.to_lowercase())
        .collect();

    // Deduplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    let mut unique: Vec<String> = Vec::new();
    for token in tokens {
        if seen.insert(token.clone()) {
            unique.push(token);
            if unique.len() >= 4 {
                break;
            }
        }
    }
    unique
}

/// Check item coverage across a combined diff string.
///
/// `phase_id` — the phase ID (e.g., "v0.15.19.3")
/// `items` — list of `(item_number, item_text)` for unchecked items in that phase
/// `diff_content` — concatenated unified diffs for all artifacts in the draft
pub fn check_coverage(
    phase_id: &str,
    items: &[(usize, &str)],
    diff_content: &str,
) -> Vec<CoverageGap> {
    let diff_lower = diff_content.to_lowercase();

    items
        .iter()
        .filter_map(|(item_number, text)| {
            let tokens = extract_tokens(text);
            if tokens.is_empty() {
                // No useful tokens — can't assess coverage.
                return None;
            }

            let found = tokens.iter().any(|tok| diff_lower.contains(tok.as_str()));
            if found {
                None
            } else {
                let excerpt: String = text.chars().take(80).collect();
                Some(CoverageGap {
                    phase_id: phase_id.to_string(),
                    item_number: *item_number,
                    text_excerpt: excerpt,
                })
            }
        })
        .collect()
}

/// Emit a `[plan]` heartbeat line for each item given its coverage status.
///
/// Returns a vec of lines to print (without trailing newline).
/// `phase_id` — phase ID string
/// `items` — list of `(item_number, item_text)` for the items
/// `diff_content` — concatenated content for coverage lookup
/// `gaps` — pre-computed coverage gaps (item numbers that have no token match)
pub fn build_plan_heartbeat_lines(
    phase_id: &str,
    items: &[(usize, &str)],
    gaps: &[CoverageGap],
) -> Vec<String> {
    let gap_numbers: std::collections::HashSet<usize> =
        gaps.iter().map(|g| g.item_number).collect();

    items
        .iter()
        .map(|(item_number, text)| {
            let excerpt: String = text.chars().take(50).collect();
            if gap_numbers.contains(item_number) {
                format!(
                    "[plan] {} item {}: not found (gap) — {}",
                    phase_id, item_number, excerpt
                )
            } else {
                // Extract the first token for the heartbeat annotation.
                let tokens = extract_tokens(text);
                let token_hint = tokens
                    .first()
                    .map(|t| format!(" (token: {})", t))
                    .unwrap_or_default();
                format!(
                    "[plan] {} item {}: verified{} ✓",
                    phase_id, item_number, token_hint
                )
            }
        })
        .collect()
}

/// Check whether any token from `item_text` appears in the given source workspace.
///
/// Walks `.rs`, `.toml`, `.md`, `.json`, `.yaml`, and `.sh` files under `source_root`,
/// returning `true` as soon as the first token match is found. This is a best-effort
/// heuristic — tokens may have false positives on very common words.
pub fn token_found_in_source(item_text: &str, source_root: &std::path::Path) -> bool {
    let tokens = extract_tokens(item_text);
    if tokens.is_empty() {
        return false;
    }

    let extensions: &[&str] = &["rs", "toml", "md", "json", "yaml", "yml", "sh", "ts", "js"];
    search_dir_for_tokens(source_root, &tokens, extensions)
}

fn search_dir_for_tokens(dir: &std::path::Path, tokens: &[String], extensions: &[&str]) -> bool {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        // Skip hidden dirs and common build artifacts.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            if search_dir_for_tokens(&path, tokens, extensions) {
                return true;
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if extensions.contains(&ext) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let lower = content.to_lowercase();
                    if tokens.iter().any(|tok| lower.contains(tok.as_str())) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_found_in_diff_no_gap() {
        let items = vec![(1usize, "Add JsonFileStore persistence layer")];
        let diff = "--- a/src/store.rs\n+++ b/src/store.rs\n+impl JsonFileStore {\n";
        let gaps = check_coverage("v0.1.0", &items, diff);
        assert!(gaps.is_empty(), "expected no gaps, got {:?}", gaps);
    }

    // ── v0.15.19.4.2 tests ────────────────────────────────────────

    #[test]
    fn build_plan_heartbeat_verified_item() {
        let items = vec![(1usize, "Add JsonFileStore persistence layer")];
        let gaps = vec![]; // no gaps → verified
        let lines = build_plan_heartbeat_lines("v0.1.0", &items, &gaps);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("verified"),
            "should say verified: {}",
            lines[0]
        );
        assert!(
            lines[0].contains("✓"),
            "should have checkmark: {}",
            lines[0]
        );
        assert!(
            lines[0].contains("v0.1.0"),
            "should include phase: {}",
            lines[0]
        );
    }

    #[test]
    fn build_plan_heartbeat_gap_item() {
        let items = vec![(2usize, "Implement FancyAlgorithm with caching")];
        let gaps = vec![crate::review_report::CoverageGap {
            phase_id: "v0.1.0".to_string(),
            item_number: 2,
            text_excerpt: "Implement FancyAlgorithm with caching".to_string(),
        }];

        let lines = build_plan_heartbeat_lines("v0.1.0", &items, &gaps);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("not found"),
            "should say not found: {}",
            lines[0]
        );
        assert!(lines[0].contains("gap"), "should say gap: {}", lines[0]);
    }

    #[test]
    fn token_found_in_source_finds_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("foo.rs"),
            "fn validate_cargo_version() {}\n",
        )
        .unwrap();
        let found = token_found_in_source("validate cargo version function", dir.path());
        assert!(found, "should find 'cargo' in source");
    }

    #[test]
    fn token_found_in_source_no_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.rs"), "fn hello_world() {}\n").unwrap();
        let found = token_found_in_source("FancyNonExistentFunction here", dir.path());
        assert!(!found, "should not find random token");
    }

    #[test]
    fn token_absent_produces_gap() {
        let items = vec![(1usize, "Implement FancyAlgorithm with caching")];
        let diff = "--- a/src/other.rs\n+++ b/src/other.rs\n+fn nothing_related() {}\n";
        let gaps = check_coverage("v0.1.0", &items, diff);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].item_number, 1);
    }

    #[test]
    fn multiple_items_mixed_coverage() {
        let items = vec![
            (1usize, "Add AuthMiddleware struct"),
            (2usize, "Implement FancyWidget rendering"),
        ];
        let diff = "--- a/src/auth.rs\n+impl AuthMiddleware {}\n";
        let gaps = check_coverage("v0.1.0", &items, diff);
        // FancyWidget not found → 1 gap
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].item_number, 2);
    }
}
