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

/// Update a phase's status using a provided schema.
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

    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Check if this line is the target phase header.
        // Normalize comparison: "v0.4.0" matches "0.4.0" and vice versa.
        let mut is_target = false;
        for pattern in &compiled_patterns {
            if let Some(caps) = pattern.captures(trimmed) {
                if let Some(id_match) = caps.get(1) {
                    let parsed_id = id_match.as_str().trim();
                    if phase_ids_match(parsed_id, phase_id) {
                        is_target = true;
                        break;
                    }
                }
            }
        }

        result.push(line.to_string());

        // If this is the target phase, find and replace the status marker,
        // skipping over blank lines (up to 3) between the header and the marker.
        if is_target {
            let mut j = i + 1;
            let mut blank_count = 0;
            while j < lines.len() && blank_count <= 3 {
                let next = lines[j].trim();
                if next.is_empty() {
                    blank_count += 1;
                    j += 1;
                    continue;
                }
                if status_re.is_match(next) {
                    // Emit the blank lines we skipped, then the replacement marker.
                    for blank_line in &lines[(i + 1)..j] {
                        result.push(blank_line.to_string());
                    }
                    result.push(format!("<!-- status: {} -->", new_status));
                    i = j + 1;
                    break;
                }
                // Non-blank, non-status line — no marker found; leave as-is.
                break;
            }
            if i == j + 1 {
                continue;
            }
        }

        i += 1;
    }

    let mut out = result.join("\n");
    // Preserve trailing newline: `str::lines()` strips it, join() doesn't restore it.
    if content.ends_with('\n') {
        out.push('\n');
    }
    out
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
}
