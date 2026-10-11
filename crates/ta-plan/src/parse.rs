// parse.rs — PLAN.md parsing and status-marker update logic (extracted from
// apps/ta-cli/src/commands/plan.rs, v0.17.11.1).

use std::path::Path;

use regex::Regex;
use ta_goal::extract_human_review_items;

use crate::schema::{PlanPhase, PlanSchema, PlanStatus};

/// Parse plan content using a provided schema.
///
/// Each `phase_patterns` regex is tested against each line.
/// The first match wins. The regex must have:
///   - Group 1: phase ID (e.g., "4b", "v0.3.1")
///   - Group 2 (optional): phase title
///
/// The status marker regex is tested against the next non-empty line.
pub fn parse_plan_with_schema(content: &str, schema: &PlanSchema) -> Vec<PlanPhase> {
    // Pre-compile all regexes. Silently skip invalid ones.
    let compiled_patterns: Vec<Regex> = schema
        .phase_patterns
        .iter()
        .filter_map(|p| Regex::new(&p.regex).ok())
        .collect();

    let status_re = match Regex::new(&schema.status_marker) {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut phases = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        for pattern in &compiled_patterns {
            if let Some(caps) = pattern.captures(line) {
                let id = caps
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();
                let title = caps
                    .get(2)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();

                if id.is_empty() {
                    break;
                }

                // Strip trailing markup from title (e.g. "*(release)*").
                let title = title.trim_end_matches(['*', '(', ')']).trim().to_string();

                let status = find_status_in_lookahead(&lines, i + 1, &status_re);
                let depends_on = find_depends_on_in_lookahead(&lines, i + 1);
                let api_impact = find_api_impact_in_lookahead(&lines, i + 1);
                let human_review_items = extract_human_review_items(content, &id, &title);
                phases.push(PlanPhase {
                    id,
                    title,
                    status,
                    depends_on,
                    human_review_items,
                    api_impact,
                });
                break; // First pattern match wins.
            }
        }

        i += 1;
    }

    phases
}

/// Compare phase IDs, normalizing the optional `v` prefix.
/// e.g., "v0.4.0" matches "0.4.0", "4b" matches "4b".
pub fn phase_ids_match(parsed_id: &str, phase_id: &str) -> bool {
    if parsed_id == phase_id {
        return true;
    }
    let norm_parsed = parsed_id.strip_prefix('v').unwrap_or(parsed_id);
    let norm_phase = phase_id.strip_prefix('v').unwrap_or(phase_id);
    norm_parsed == norm_phase
}

/// Look ahead from `start` for a status marker comment.
/// Skips blank lines (up to 3) so that a blank line between a phase heading
/// and its `<!-- status: ... -->` marker does not cause it to read as Pending.
/// Stops immediately on the first non-blank, non-status line.
fn find_status_in_lookahead(lines: &[&str], start: usize, status_re: &Regex) -> PlanStatus {
    let mut skipped = 0;
    let mut i = start;
    while i < lines.len() && skipped <= 3 {
        let line = lines[i].trim();
        if line.is_empty() {
            skipped += 1;
            i += 1;
            continue;
        }
        if let Some(caps) = status_re.captures(line) {
            let status_str = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            return parse_status_str(status_str);
        }
        // First non-blank line that isn't a status marker — stop scanning.
        break;
    }
    PlanStatus::Pending
}

/// Look ahead from `start` for a dependency declaration, in either of two
/// forms actually seen in PLAN.md:
///   - `<!-- depends_on: v0.13.17.3, v0.14.1 -->` (v0.14.3, a plain comma
///     list, rarely used in practice — 1 occurrence across the whole doc).
///   - `**Depends on**: v0.13.17.3 (explanation), v0.14.1 (...)` — a bold
///     prose line (v0.17.0.12.34), the format actually used ~125 times.
///     Parenthetical explanations are stripped; only the leading
///     phase-id-shaped token from each comma-separated entry is kept, and
///     entries that don't start with one (e.g. "Meridian `suggest` command
///     (v0.1.x)") are silently skipped rather than guessed at.
///
/// Scans up to 5 lines ahead, stopping if another phase header is detected.
fn find_depends_on_in_lookahead(lines: &[&str], start: usize) -> Vec<String> {
    let dep_comment_re = match Regex::new(r"<!--\s*depends_on:\s*([^>]+?)\s*-->") {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let dep_prose_re = match Regex::new(r"^\*\*Depends [Oo]n\*\*:\s*(.+)$") {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    // Phase header patterns to detect the next phase boundary.
    let header_re = match Regex::new(r"^(?:##\s+Phase|###\s+v[\d.]+[a-z]?\s+[—\-])") {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let limit = std::cmp::min(start + 5, lines.len());
    for (offset, line) in lines[start..limit].iter().enumerate() {
        let line = line.trim();
        // Stop if we've hit the next phase header (but not on the first lookahead line).
        if offset > 0 && header_re.is_match(line) {
            break;
        }
        if let Some(caps) = dep_comment_re.captures(line) {
            let raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            return raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Some(caps) = dep_prose_re.captures(line) {
            let raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            return extract_leading_id_tokens(raw);
        }
    }
    vec![]
}

/// Look ahead from `start` for a `**API impact**: adds Foo::bar; modifies
/// Baz::qux` prose line (v0.17.0.12.34). Entries are semicolon-separated
/// free-text tokens (not phase IDs) — trimmed verbatim, no id extraction.
/// Scans up to 5 lines ahead, stopping if another phase header is detected.
fn find_api_impact_in_lookahead(lines: &[&str], start: usize) -> Vec<String> {
    let impact_re = match Regex::new(r"^\*\*API [Ii]mpact\*\*:\s*(.+)$") {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let header_re = match Regex::new(r"^(?:##\s+Phase|###\s+v[\d.]+[a-z]?\s+[—\-])") {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let limit = std::cmp::min(start + 5, lines.len());
    for (offset, line) in lines[start..limit].iter().enumerate() {
        let line = line.trim();
        if offset > 0 && header_re.is_match(line) {
            break;
        }
        if let Some(caps) = impact_re.captures(line) {
            let raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            return split_top_level(raw, ';')
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    vec![]
}

/// Split `s` on top-level occurrences of `sep`, treating `(...)` spans as
/// opaque so a comma or semicolon inside a parenthetical explanation doesn't
/// split an entry in half.
fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            c if c == sep && depth <= 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            c => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

/// From a `**Depends on**:` prose line's captured text, extract the leading
/// phase-id-shaped token of each top-level comma-separated entry, dropping
/// any parenthetical explanation. Entries that don't start with an
/// id-shaped token (e.g. "Meridian `suggest` command (v0.1.x)", or "None")
/// are silently skipped — this is a conservative extraction, not a guess.
fn extract_leading_id_tokens(raw: &str) -> Vec<String> {
    let id_re = match Regex::new(r"^(v?\d[\da-z.]*)") {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    split_top_level(raw, ',')
        .iter()
        .filter_map(|entry| {
            id_re
                .captures(entry.trim())
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().trim_end_matches('.').to_string())
        })
        .collect()
}

fn parse_status_str(s: &str) -> PlanStatus {
    match s {
        "done" => PlanStatus::Done,
        "in_progress" => PlanStatus::InProgress,
        "deferred" => PlanStatus::Deferred,
        _ => PlanStatus::Pending,
    }
}

/// Parse PLAN.md content into a list of phases (using the default schema).
///
/// This is the backward-compatible entry point used by existing code.
pub fn parse_plan(content: &str) -> Vec<PlanPhase> {
    parse_plan_with_schema(content, &PlanSchema::default_schema())
}

/// Update a phase's status in PLAN.md content. Returns the new content.
///
/// Finds the phase by ID using the default schema's patterns
/// and replaces its status marker.
pub fn update_phase_status(content: &str, phase_id: &str, new_status: PlanStatus) -> String {
    update_phase_status_with_schema(content, phase_id, new_status, &PlanSchema::default_schema())
}

/// True for lines that open or close a fenced code block (``` or ~~~), per line.
fn fenced_line_flags(lines: &[&str]) -> Vec<bool> {
    let run = |line: &str| -> Option<(char, usize, bool)> {
        let s = line.trim_end_matches(['\n', '\r']).trim_start_matches(' ');
        let ch = s.chars().next()?;
        if ch != '`' && ch != '~' {
            return None;
        }
        let n = s.chars().take_while(|&c| c == ch).count();
        if n < 3 {
            return None;
        }
        Some((ch, n, s[n..].trim().is_empty()))
    };
    let mut out = vec![false; lines.len()];
    let mut open: Option<(char, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        match (open, run(line)) {
            (None, Some((ch, n, _))) => {
                open = Some((ch, n));
                out[i] = true;
            }
            (Some((och, on)), Some((ch, n, bare_rest))) => {
                out[i] = true;
                if ch == och && n >= on && bare_rest {
                    open = None;
                }
            }
            (Some(_), None) => out[i] = true,
            (None, None) => {}
        }
    }
    out
}

/// Update a phase's status using a provided schema.
///
/// Byte-preserving: every line other than the target phase's status marker is
/// returned exactly as it was (line endings, blank lines, trailing spaces, code
/// fences). Only the first heading outside a code fence that matches `phase_id`
/// is considered, and only its own marker line is replaced, so a look-alike or
/// duplicated heading elsewhere in the document is never touched.
pub fn update_phase_status_with_schema(
    content: &str,
    phase_id: &str,
    new_status: PlanStatus,
    schema: &PlanSchema,
) -> String {
    let compiled_patterns: Vec<Regex> = schema
        .phase_patterns
        .iter()
        .filter_map(|p| Regex::new(&p.regex).ok())
        .collect();

    let status_re = match Regex::new(&schema.status_marker) {
        Ok(r) => r,
        Err(_) => return content.to_string(),
    };

    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let fenced = fenced_line_flags(&lines);

    // Locate the first non-fenced heading that names the target phase.
    let heading_idx = lines.iter().enumerate().find_map(|(i, line)| {
        if fenced[i] {
            return None;
        }
        let trimmed = line.trim();
        compiled_patterns.iter().find_map(|pattern| {
            let caps = pattern.captures(trimmed)?;
            let parsed_id = caps.get(1)?.as_str().trim();
            phase_ids_match(parsed_id, phase_id).then_some(i)
        })
    });
    let Some(i) = heading_idx else {
        return content.to_string();
    };

    // Find the marker, skipping up to 3 blank lines between header and marker.
    let mut j = i + 1;
    let mut blank_count = 0;
    while j < lines.len() && blank_count <= 3 {
        let next = lines[j].trim();
        if next.is_empty() {
            blank_count += 1;
            j += 1;
            continue;
        }
        if !fenced[j] && status_re.is_match(next) {
            let eol = if lines[j].ends_with("\r\n") {
                "\r\n"
            } else if lines[j].ends_with('\n') {
                "\n"
            } else {
                ""
            };
            let mut out = String::with_capacity(content.len() + 16);
            out.push_str(&lines[..j].concat());
            out.push_str(&format!("<!-- status: {} -->{}", new_status, eol));
            out.push_str(&lines[j + 1..].concat());
            return out;
        }
        // Non-blank, non-status line: no marker found; leave as-is.
        break;
    }
    content.to_string()
}

/// Read and parse PLAN.md from a project directory.
///
/// Loads `.ta/plan-schema.yaml` if present, otherwise uses the default schema.
pub fn load_plan(project_root: &Path) -> anyhow::Result<Vec<PlanPhase>> {
    let schema = PlanSchema::load_or_default(project_root);
    let plan_path = project_root.join(&schema.source);
    if !plan_path.exists() {
        anyhow::bail!("No {} found in {}", schema.source, project_root.display());
    }
    let content = std::fs::read_to_string(&plan_path)?;
    Ok(parse_plan_with_schema(&content, &schema))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test against this repo's own real PLAN.md — the extraction's
    /// "no behavior change" goal (v0.17.11.1 item 1) is only meaningfully
    /// verified against a real, large, messy document, not just synthetic
    /// fixtures. Loose bounds (not exact counts) so this doesn't need updating
    /// every time a phase is added — it just needs to keep working at all.
    #[test]
    fn parses_this_repos_own_plan_md_without_panicking() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .expect("crates/ta-plan should be two levels below the repo root");
        let plan_path = repo_root.join("PLAN.md");
        assert!(
            plan_path.exists(),
            "expected to find the repo's own PLAN.md at {}",
            plan_path.display()
        );

        let phases = load_plan(repo_root).expect("load_plan should succeed on the real PLAN.md");
        assert!(
            phases.len() > 50,
            "expected a substantial number of real phases, got {}",
            phases.len()
        );
        assert!(
            phases.iter().any(|p| p.status == PlanStatus::Done),
            "expected at least one Done phase in the real plan"
        );
        // A known-stable, long-done phase must parse with the expected status —
        // a canary that would catch a real schema/regex regression, not just a
        // parse-without-panicking check.
        let v17_10_1 = phases
            .iter()
            .find(|p| phase_ids_match(&p.id, "v0.17.10.1"))
            .expect("v0.17.10.1 should exist in the real plan");
        assert_eq!(v17_10_1.status, PlanStatus::Done);
    }

    fn differing_lines(a: &str, b: &str) -> Vec<usize> {
        let al: Vec<&str> = a.split_inclusive('\n').collect();
        let bl: Vec<&str> = b.split_inclusive('\n').collect();
        assert_eq!(al.len(), bl.len(), "line count must not change");
        (0..al.len()).filter(|&i| al[i] != bl[i]).collect()
    }

    #[test]
    fn update_phase_status_changes_only_the_marker_line() {
        let doc = "# T\r\n\r\n### v1.0.0 - A\r\n<!-- status: pending -->\r\n\r\n\r\n1. [ ] x  \r\n\r\n### v1.0.1 - B\r\n<!-- status: pending -->\r\n";
        let out = update_phase_status(doc, "v1.0.0", PlanStatus::Done);
        assert_eq!(differing_lines(doc, &out), vec![3]);
        assert!(out.contains("<!-- status: done -->\r\n"));
        assert!(out.ends_with("<!-- status: pending -->\r\n"));
    }

    #[test]
    fn update_phase_status_ignores_fenced_look_alike_headings_and_duplicates() {
        let doc = "```\n### v1.0.0 - In a fence\n<!-- status: pending -->\n```\n\n### v1.0.0 - Real\n<!-- status: pending -->\n\n### v1.0.0 - Dup\n<!-- status: pending -->\n";
        let out = update_phase_status(doc, "v1.0.0", PlanStatus::Done);
        assert_eq!(differing_lines(doc, &out), vec![6]);
    }

    #[test]
    fn update_phase_status_does_not_match_a_longer_id() {
        let doc = "### v1.0.10 - A\n<!-- status: pending -->\n";
        assert_eq!(update_phase_status(doc, "v1.0.1", PlanStatus::Done), doc);
    }

    /// Golden: on the real PLAN.md, marking any phase changes at most that phase's one
    /// marker line and never the line count, blank lines or any other byte.
    #[test]
    fn update_phase_status_on_real_plan_changes_at_most_one_line() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let plan_path = manifest_dir.join("../../PLAN.md");
        let original = std::fs::read_to_string(&plan_path).expect("read real PLAN.md");
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("PLAN.md");
        std::fs::write(&copy, &original).unwrap();
        let content = std::fs::read_to_string(&copy).unwrap();
        let phases = parse_plan(&content);
        assert!(phases.len() > 50);
        for p in phases.iter().take(400) {
            let out = update_phase_status(&content, &p.id, PlanStatus::InProgress);
            let diff = differing_lines(&content, &out);
            assert!(
                diff.len() <= 1,
                "phase {} changed {} lines: {:?}",
                p.id,
                diff.len(),
                diff
            );
        }
    }
}
