//! Studio boundary check (v0.17.0.12.21 item 4): Studio only ever talks to
//! the daemon's HTTP/SSE API, never to internal `ta-*` Rust types directly
//! (`docs/design/ta-concepts-and-architecture.md` §13.1: "it may never
//! special-case internal Rust types, only the versioned spec").
//!
//! Studio itself is plain HTML/JS (`crates/ta-daemon/assets/*.html`) with no
//! Rust imports, so there's nothing there to violate this rule today. The
//! real enforcement surface is `ta-daemon`'s own API response types
//! (`crates/ta-daemon/src/api/*.rs`, `crates/ta-daemon/src/web.rs`): a
//! response struct that wraps one of the five versioned spec types
//! (`GoalRun`, `DraftPackage`, `Artifact`, `TriggerEvent`, `RoutingDecision`,
//! `PersonaConfig`) directly is fine *only* if it's explicit and versioned
//! (carries a `schema_version` marker so a consumer knows which spec
//! version it's reading) — an un-versioned direct exposure is the "special
//! case" the rule forbids, since a future struct change would silently
//! change the wire shape with no signal.
//!
//! This is a conservative, text-based scan (not full Rust parsing): it
//! finds `struct` bodies and checks whether any spec-type field is paired
//! with a `schema_version` field in the same struct. False negatives are
//! possible (e.g. a type alias hiding a spec type), but it catches the
//! straightforward case new code is most likely to introduce.

use std::path::Path;

const SPEC_TYPES: &[&str] = &[
    "GoalRun",
    "DraftPackage",
    "Artifact",
    "TriggerEvent",
    "RoutingDecision",
    "PersonaConfig",
];

/// Finds top-level `struct Name { ... }` bodies in `text` (brace-depth
/// matched, so nested braces inside the body don't terminate it early).
fn struct_blocks(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::new();
    let mut i = 0;
    while let Some(rel) = text[i..].find("struct ") {
        let start = i + rel + "struct ".len();
        // Read the struct name.
        let name_end = text[start..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map(|o| start + o)
            .unwrap_or(text.len());
        let name = text[start..name_end].to_string();

        // Find the opening brace for this struct (skip tuple-structs/unit-structs
        // that end in `;` before any `{`).
        let Some(brace_rel) = text[name_end..].find(['{', ';']) else {
            break;
        };
        let brace_pos = name_end + brace_rel;
        if bytes[brace_pos] != b'{' {
            i = brace_pos + 1;
            continue;
        }

        // Match braces to find the end of the struct body.
        let mut depth = 0i32;
        let mut end = brace_pos;
        for (offset, ch) in text[brace_pos..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = brace_pos + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        blocks.push((name, text[brace_pos..=end].to_string()));
        i = end + 1;
    }
    blocks
}

/// True if `needle` appears in `haystack` as a whole identifier (not part of
/// a longer identifier like `GoalRunState` containing `GoalRun`).
fn contains_ident(haystack: &str, needle: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut search_from = 0;
    while let Some(rel) = haystack[search_from..].find(needle) {
        let pos = search_from + rel;
        let before_ok = pos == 0 || !is_ident(haystack[..pos].chars().next_back().unwrap());
        let after_pos = pos + needle.len();
        let after_ok =
            after_pos >= haystack.len() || !is_ident(haystack[after_pos..].chars().next().unwrap());
        if before_ok && after_ok {
            return true;
        }
        search_from = pos + needle.len();
    }
    false
}

fn daemon_api_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ta-daemon/src")
        .canonicalize()
        .expect("ta-daemon/src must exist")
}

/// Strips `//...` line comments (including `///` doc comments) so doc-prose
/// mentions of a spec type name (e.g. "set for GoalRun intents") don't count
/// as a real field-type reference. Not string-literal-aware — acceptable
/// for a lint over struct definitions, which rarely embed `//` in a literal.
fn strip_line_comments(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn check_file(path: &Path, violations: &mut Vec<String>) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("failed to read {}: {}", path.display(), e);
    });
    for (name, raw_body) in struct_blocks(&text) {
        let body = strip_line_comments(&raw_body);
        let referenced: Vec<&str> = SPEC_TYPES
            .iter()
            .copied()
            .filter(|t| contains_ident(&body, t))
            .collect();
        if referenced.is_empty() {
            continue;
        }
        if !contains_ident(&body, "schema_version") {
            violations.push(format!(
                "{}: struct `{}` embeds spec type(s) {:?} without a `schema_version` \
                 field — either use a purpose-built DTO instead of the raw spec type, \
                 or add an explicit `schema_version` marker (see docs/design/ta-data-format-spec.md)",
                path.display(),
                name,
                referenced
            ));
        }
    }
}

#[test]
fn daemon_api_responses_dont_leak_unversioned_spec_types() {
    let mut violations = Vec::new();

    let web_rs = daemon_api_dir().join("web.rs");
    check_file(&web_rs, &mut violations);

    let api_dir = daemon_api_dir().join("api");
    let mut entries: Vec<_> = std::fs::read_dir(&api_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", api_dir.display(), e))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect();
    entries.sort();
    for path in entries {
        check_file(&path, &mut violations);
    }

    assert!(
        violations.is_empty(),
        "Studio boundary violation(s) found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn contains_ident_does_not_match_substrings() {
    assert!(contains_ident("draft: DraftPackage,", "DraftPackage"));
    assert!(!contains_ident("state: GoalRunState,", "GoalRun"));
    assert!(contains_ident("state: GoalRun,", "GoalRun"));
}
