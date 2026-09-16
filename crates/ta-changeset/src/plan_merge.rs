//! Three-way PLAN.md merge for the Draft Pre-Apply Plan Review Agent.
//!
//! Compares base (PLAN.md at staging-creation time), staging (agent's version),
//! and source (current main) to detect regressions, agent additions, and conflicts.

use serde::{Deserialize, Serialize};

/// Returns true if `t` (trimmed line) is an unchecked list item: `- [ ]` or `N. [ ]`.
pub fn is_unchecked_item(t: &str) -> bool {
    if t.starts_with("- [ ] ") || t == "- [ ]" {
        return true;
    }
    // Numbered list: `1. [ ] ` or `1. [ ]`
    let digits_end = t.find(|c: char| !c.is_ascii_digit());
    if let Some(pos) = digits_end {
        if pos > 0 {
            let rest = &t[pos..];
            return rest.starts_with(". [ ] ") || rest == ". [ ]";
        }
    }
    false
}

/// Returns true if `t` (trimmed line) is any checkbox list item (checked or unchecked).
pub fn is_any_item(t: &str) -> bool {
    if t.starts_with("- [ ] ") || t == "- [ ]" || t.starts_with("- [x] ") || t.starts_with("- [X] ")
    {
        return true;
    }
    let digits_end = t.find(|c: char| !c.is_ascii_digit());
    if let Some(pos) = digits_end {
        if pos > 0 {
            let rest = &t[pos..];
            return rest.starts_with(". [ ] ")
                || rest == ". [ ]"
                || rest.starts_with(". [x] ")
                || rest.starts_with(". [X] ");
        }
    }
    false
}

/// A parsed section of PLAN.md (one `### v0.x.y` block).
#[derive(Debug, Clone, PartialEq)]
pub struct PlanSection {
    pub id: String,
    pub raw_header: String,
    pub status_marker: Option<String>,
    pub items: Vec<PlanItem>,
    pub raw_body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    pub checked: bool,
    pub text: String,
    pub raw_line: String,
}

/// The type of conflict detected between base, staging, and source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictType {
    StatusConflict,
    ItemTextConflict,
    SectionBodyConflict,
}

/// A conflict that cannot be auto-resolved — both source and staging diverged from base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanConflict {
    pub section_id: String,
    pub conflict_type: ConflictType,
    pub base_text: String,
    pub staging_text: String,
    pub source_text: String,
    pub description: String,
}

/// The output of a three-way PLAN.md merge.
#[derive(Debug, Clone)]
pub struct MergeResult {
    pub merged: String,
    pub silent_fixes: Vec<String>,
    pub agent_additions: Vec<String>,
    pub conflicts: Vec<PlanConflict>,
}

/// Parse PLAN.md into sections.
///
/// Every top-level `### ...` heading starts its own section, whether or not
/// its title is version-shaped (`### v0.x.y`) -- a plain-named phase
/// heading (`### Phase 1 -- Real roster, real work`) gets its own section
/// exactly like a version-style one does; see `extract_version_header`'s
/// own doc comment for why this matters. Only content before the very
/// first `###` heading of any kind is returned as the opaque
/// `"__preamble__"` section (or `"__tail__"` if the document has no
/// heading at all).
pub fn parse_plan_sections(content: &str) -> Vec<PlanSection> {
    let mut sections: Vec<PlanSection> = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_id: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();

    for line in content.lines() {
        if let Some(id) = extract_version_header(line) {
            // Flush previous section.
            if let Some(prev_id) = current_id.take() {
                sections.push(build_section(
                    prev_id,
                    current_header.take().unwrap_or_default(),
                    &current_lines,
                ));
                current_lines.clear();
            } else if !current_lines.is_empty() {
                // Preamble before first versioned section.
                sections.push(build_section(
                    "__preamble__".to_string(),
                    String::new(),
                    &current_lines,
                ));
                current_lines.clear();
            }
            current_id = Some(id);
            current_header = Some(line.to_string());
        } else {
            current_lines.push(line.to_string());
        }
    }

    // Flush last section.
    if let Some(id) = current_id {
        sections.push(build_section(
            id,
            current_header.unwrap_or_default(),
            &current_lines,
        ));
    } else if !current_lines.is_empty() {
        sections.push(build_section(
            "__tail__".to_string(),
            String::new(),
            &current_lines,
        ));
    }

    sections
}

/// True for a token shaped like `v0.x.y[.z...]` -- the same test
/// `extract_version_header` uses to decide whether a heading's first word
/// is a version number rather than an ordinary title word. Exposed
/// separately so `validate_plan_merge` can tell a genuinely version-style
/// section id apart from a plain-heading-text id derived from a non-
/// version heading (see `extract_version_header`'s doc comment).
fn is_version_token(token: &str) -> bool {
    token.starts_with('v')
        && token
            .trim_start_matches('v')
            .split('.')
            .all(|p| p.chars().all(|c| c.is_ascii_digit()))
        && token.trim_start_matches('v').contains('.')
}

/// Extracts a section-boundary id from a `### ...` heading line.
///
/// A version-shaped title (`### v0.x.y ...`) yields its version token
/// (`"0.x.y"`) as the id, matching this function's original, narrower
/// behavior. Any other `### ...` heading (`### Phase 1 -- Real roster,
/// real work`, `### Wiki retrieval + ingestion design`) still yields an
/// id -- the heading's own full title text -- rather than `None`.
///
/// This function used to return `None` for a non-version heading, on the
/// assumption that every real phase in a PLAN.md is version-numbered. That
/// assumption doesn't hold: several real PLAN.md files (this repo's own
/// included, and `ta-virtual-team`'s) mix version-style phases with
/// plain-named ones. Returning `None` for those meant `parse_plan_sections`
/// silently swallowed a plain-named heading's entire body -- including its
/// own `<!-- status: ... -->` marker and item list -- into whichever
/// version-style section happened to precede it, or into `__preamble__` if
/// none had appeared yet. `reconstruct_body` then overwrote every embedded
/// status marker it found inside that swallowed body with the *enclosing*
/// section's single reconciled status, and `auto_correct_done_phase_items`
/// force-checked whatever unchecked items followed -- corrupting phases the
/// merge never should have touched. Found live 2026-09-15 (see this
/// module's `golden_failure_*` tests for the exact reproduction); treating
/// every heading as a real boundary is the fix.
fn extract_version_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("### ") {
        return None;
    }
    let rest = &trimmed[4..];
    // Accept "v0.x.y" or "v0.x.y.z" at the start, optionally followed by " —" or " -" title.
    let token = rest.split_whitespace().next().unwrap_or("");
    if is_version_token(token) {
        Some(token.to_string())
    } else {
        Some(rest.trim().to_string())
    }
}

fn build_section(id: String, raw_header: String, lines: &[String]) -> PlanSection {
    let raw_body = lines.join("\n");

    let status_marker = lines.iter().find_map(|l| {
        let trimmed = l.trim();
        if trimmed.starts_with("<!-- status:") && trimmed.ends_with("-->") {
            Some(trimmed.to_string())
        } else {
            None
        }
    });

    let items = lines
        .iter()
        .filter_map(|l| {
            let trimmed = l.trim();
            if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
                Some(PlanItem {
                    checked: false,
                    text: rest.to_string(),
                    raw_line: l.clone(),
                })
            } else {
                trimmed
                    .strip_prefix("- [x] ")
                    .or_else(|| trimmed.strip_prefix("- [X] "))
                    .map(|rest| PlanItem {
                        checked: true,
                        text: rest.to_string(),
                        raw_line: l.clone(),
                    })
            }
        })
        .collect();

    PlanSection {
        id,
        raw_header,
        status_marker,
        items,
        raw_body,
    }
}

/// Three-way merge of base, staging, and source PLAN.md.
///
/// Rules implemented:
/// - Source updated status, staging didn't (base==staging, source!=base) → take source (silent fix)
/// - Agent completed phase (staging!=base, source==base on status) → take staging (agent addition)
/// - Agent checked off items (`[ ]`→`[x]`) → checkbox union (`[x]` wins)
/// - Agent inserted new sub-phase absent from base+source → insert into merged output
/// - Both agent and source changed same section incompatibly → CONFLICT
/// - Agent changed item text (not just checkbox) → CONFLICT
pub fn merge_plan_md(base: &str, staging: &str, source: &str) -> MergeResult {
    let base_sections = parse_plan_sections(base);
    let staging_sections = parse_plan_sections(staging);
    let source_sections = parse_plan_sections(source);

    let mut merged_output: Vec<String> = Vec::new();
    let mut silent_fixes: Vec<String> = Vec::new();
    let mut agent_additions: Vec<String> = Vec::new();
    let mut conflicts: Vec<PlanConflict> = Vec::new();

    // Build lookup maps by section id.
    let base_map: std::collections::HashMap<&str, &PlanSection> =
        base_sections.iter().map(|s| (s.id.as_str(), s)).collect();
    let staging_map: std::collections::HashMap<&str, &PlanSection> = staging_sections
        .iter()
        .map(|s| (s.id.as_str(), s))
        .collect();
    let source_map: std::collections::HashMap<&str, &PlanSection> =
        source_sections.iter().map(|s| (s.id.as_str(), s)).collect();

    // Collect all known IDs in source order, then append agent-only IDs at the end.
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut ordered_ids: Vec<&str> = Vec::new();

    for s in &source_sections {
        ordered_ids.push(s.id.as_str());
        seen_ids.insert(s.id.as_str());
    }
    // Agent-inserted sections not in source or base.
    for s in &staging_sections {
        if !seen_ids.contains(s.id.as_str()) && !base_map.contains_key(s.id.as_str()) {
            ordered_ids.push(s.id.as_str());
        }
    }

    for section_id in &ordered_ids {
        let base_sec = base_map.get(section_id).copied();
        let staging_sec = staging_map.get(section_id).copied();
        let source_sec = source_map.get(section_id).copied();

        let merged_sec = merge_section(
            section_id,
            base_sec,
            staging_sec,
            source_sec,
            &mut silent_fixes,
            &mut agent_additions,
            &mut conflicts,
        );

        if !merged_sec.raw_header.is_empty() {
            merged_output.push(merged_sec.raw_header.clone());
        }
        merged_output.push(merged_sec.raw_body.clone());
    }

    MergeResult {
        merged: merged_output.join("\n"),
        silent_fixes,
        agent_additions,
        conflicts,
    }
}

fn merge_section<'a>(
    section_id: &str,
    base: Option<&'a PlanSection>,
    staging: Option<&'a PlanSection>,
    source: Option<&'a PlanSection>,
    silent_fixes: &mut Vec<String>,
    agent_additions: &mut Vec<String>,
    conflicts: &mut Vec<PlanConflict>,
) -> PlanSection {
    match (base, staging, source) {
        // Section only in staging (agent-inserted new section).
        (None, Some(stg), None) => {
            agent_additions.push(format!("New sub-phase {} inserted by agent", section_id));
            stg.clone()
        }

        // Normal three-way case: base + staging + source all present.
        (Some(base_sec), Some(stg_sec), Some(src_sec)) => merge_three_way(
            section_id,
            base_sec,
            stg_sec,
            src_sec,
            silent_fixes,
            agent_additions,
            conflicts,
        ),

        // Staging and source exist but no base (pre-v0.15.19.3 goal, two-way fallback).
        (None, Some(stg_sec), Some(src_sec)) => {
            two_way_merge(section_id, stg_sec, src_sec, agent_additions, conflicts)
        }

        // Section only in source (new phase added since goal start) — keep source.
        (None, None, Some(src)) => src.clone(),

        // Section only in staging (agent-inserted, already handled above — guard).
        // Also covers: base+source but no staging (agent deleted) — keep source.
        (Some(_), None, Some(src)) => src.clone(),

        // Base + staging but no source (section removed from source) — take source (omit).
        (Some(_), Some(_), None) => {
            silent_fixes.push(format!(
                "Section {} removed from source — omitted",
                section_id
            ));
            PlanSection {
                id: section_id.to_string(),
                raw_header: String::new(),
                status_marker: None,
                items: vec![],
                raw_body: String::new(),
            }
        }

        // Section only in base (deleted from both) — omit.
        (Some(_), None, None) => PlanSection {
            id: section_id.to_string(),
            raw_header: String::new(),
            status_marker: None,
            items: vec![],
            raw_body: String::new(),
        },

        // No information — empty placeholder.
        (None, None, None) => PlanSection {
            id: section_id.to_string(),
            raw_header: String::new(),
            status_marker: None,
            items: vec![],
            raw_body: String::new(),
        },
    }
}

fn merge_three_way(
    section_id: &str,
    base: &PlanSection,
    staging: &PlanSection,
    source: &PlanSection,
    silent_fixes: &mut Vec<String>,
    agent_additions: &mut Vec<String>,
    conflicts: &mut Vec<PlanConflict>,
) -> PlanSection {
    // --- Status marker reconciliation ---
    let merged_status = reconcile_status(
        section_id,
        base.status_marker.as_deref(),
        staging.status_marker.as_deref(),
        source.status_marker.as_deref(),
        silent_fixes,
        agent_additions,
        conflicts,
    );

    // --- Item-level merge ---
    let merged_items = merge_items(
        section_id,
        &base.items,
        &staging.items,
        &source.items,
        conflicts,
    );

    // --- Non-item body text reconciliation ---
    //
    // `reconstruct_body` only ever substitutes into whichever body string
    // it's handed here -- it has no way to insert a line that exists only
    // in the OTHER body it wasn't given. Always handing it `source.raw_body`
    // (the pre-fix, only behavior) meant prose the agent added during
    // staging (a note, a new paragraph -- anything that isn't a checkbox
    // item or a status marker line) had no path into the merged result at
    // all: silently dropped, every time, regardless of whether source ever
    // touched that text. Found live 2026-09-15 via
    // `golden_failure_untouched_plain_named_phases_keep_their_own_status_and_items`
    // once the section-boundary fix above stopped masking it. Reconcile
    // which body to use as the substitution template the same way status
    // is reconciled: prefer whichever side's non-item, non-marker text
    // actually changed relative to base; a genuine three-way prose
    // conflict is possible (`SectionBodyConflict`, declared for exactly
    // this but never constructed until now) and defaults to source,
    // conservative like every other unresolved conflict in this module.
    let base_prose = non_item_body_signature(&base.raw_body);
    let staging_prose = non_item_body_signature(&staging.raw_body);
    let source_prose = non_item_body_signature(&source.raw_body);
    let staging_prose_changed = staging_prose != base_prose;
    let source_prose_changed = source_prose != base_prose;

    let body_template: &str = match (staging_prose_changed, source_prose_changed) {
        (true, false) => &staging.raw_body,
        (true, true) if staging_prose == source_prose => &staging.raw_body,
        (true, true) => {
            conflicts.push(PlanConflict {
                section_id: section_id.to_string(),
                conflict_type: ConflictType::SectionBodyConflict,
                base_text: base.raw_body.clone(),
                staging_text: staging.raw_body.clone(),
                source_text: source.raw_body.clone(),
                description:
                    "Section body text differs between staging and source (both diverged from \
                     base) -- taking source for the merge, review needed"
                        .to_string(),
            });
            &source.raw_body
        }
        (false, _) => &source.raw_body,
    };

    // Reconstruct raw_body from merged items and non-item lines.
    let merged_body = reconstruct_body(
        &base.raw_body,
        body_template,
        &merged_status,
        &merged_items,
        section_id,
    );

    PlanSection {
        id: section_id.to_string(),
        raw_header: source.raw_header.clone(),
        status_marker: merged_status,
        items: merged_items,
        raw_body: merged_body,
    }
}

fn two_way_merge(
    section_id: &str,
    staging: &PlanSection,
    source: &PlanSection,
    agent_additions: &mut Vec<String>,
    conflicts: &mut Vec<PlanConflict>,
) -> PlanSection {
    // Conservative two-way: apply checkbox union, detect status conflicts.
    let mut merged_items = source.items.clone();
    for (i, src_item) in source.items.iter().enumerate() {
        if let Some(stg_item) = staging.items.get(i) {
            if stg_item.checked && !src_item.checked && stg_item.text == src_item.text {
                merged_items[i].checked = true;
                merged_items[i].raw_line = stg_item.raw_line.clone();
                agent_additions.push(format!(
                    "Section {}: item {} marked complete by agent",
                    section_id,
                    i + 1
                ));
            }
        }
    }

    // Status: if staging advanced status, capture it; if incompatible, conflict.
    let merged_status = if staging.status_marker != source.status_marker {
        // Prefer staging's forward progress.
        if is_status_advancement(
            source.status_marker.as_deref(),
            staging.status_marker.as_deref(),
        ) {
            agent_additions.push(format!(
                "Section {}: status advanced by agent ({:?} → {:?})",
                section_id, source.status_marker, staging.status_marker
            ));
            staging.status_marker.clone()
        } else {
            conflicts.push(PlanConflict {
                section_id: section_id.to_string(),
                conflict_type: ConflictType::StatusConflict,
                base_text: String::new(),
                staging_text: staging.status_marker.clone().unwrap_or_default(),
                source_text: source.status_marker.clone().unwrap_or_default(),
                description:
                    "Status marker differs between staging and source (no base for comparison)"
                        .to_string(),
            });
            source.status_marker.clone()
        }
    } else {
        source.status_marker.clone()
    };

    let merged_body = reconstruct_body(
        &source.raw_body,
        &source.raw_body,
        &merged_status,
        &merged_items,
        section_id,
    );

    PlanSection {
        id: section_id.to_string(),
        raw_header: source.raw_header.clone(),
        status_marker: merged_status,
        items: merged_items,
        raw_body: merged_body,
    }
}

fn reconcile_status(
    section_id: &str,
    base_status: Option<&str>,
    staging_status: Option<&str>,
    source_status: Option<&str>,
    silent_fixes: &mut Vec<String>,
    agent_additions: &mut Vec<String>,
    conflicts: &mut Vec<PlanConflict>,
) -> Option<String> {
    let staging_changed = staging_status != base_status;
    let source_changed = source_status != base_status;

    match (staging_changed, source_changed) {
        // Neither changed → take source (same as base).
        (false, false) => source_status.map(|s| s.to_string()),

        // Only source changed → take source (silent fix — e.g., human marked done).
        (false, true) => {
            silent_fixes.push(format!(
                "Section {}: status updated in source ({:?} → {:?}), staging unchanged — taking source",
                section_id, base_status, source_status
            ));
            source_status.map(|s| s.to_string())
        }

        // Only staging changed (agent advanced status) → take staging.
        (true, false) => {
            agent_additions.push(format!(
                "Section {}: status advanced by agent ({:?} → {:?})",
                section_id, base_status, staging_status
            ));
            staging_status.map(|s| s.to_string())
        }

        // Both changed → check if they agree.
        (true, true) => {
            if staging_status == source_status {
                // Both made the same change — no conflict.
                source_status.map(|s| s.to_string())
            } else {
                // Real conflict: both changed differently.
                conflicts.push(PlanConflict {
                    section_id: section_id.to_string(),
                    conflict_type: ConflictType::StatusConflict,
                    base_text: base_status.unwrap_or("").to_string(),
                    staging_text: staging_status.unwrap_or("").to_string(),
                    source_text: source_status.unwrap_or("").to_string(),
                    description: format!(
                        "Both source and staging changed the status marker for section {}",
                        section_id
                    ),
                });
                // Take source for conflicts (conservative).
                source_status.map(|s| s.to_string())
            }
        }
    }
}

fn merge_items(
    section_id: &str,
    base_items: &[PlanItem],
    staging_items: &[PlanItem],
    source_items: &[PlanItem],
    conflicts: &mut Vec<PlanConflict>,
) -> Vec<PlanItem> {
    // Start with source items as authoritative order.
    let mut merged = source_items.to_vec();

    // For each source item, check base and staging for checkbox advancement or text changes.
    for (i, src_item) in source_items.iter().enumerate() {
        let base_item = base_items.get(i);
        let stg_item = staging_items.get(i);

        let (base_checked, base_text) = base_item
            .map(|b| (b.checked, b.text.as_str()))
            .unwrap_or((false, src_item.text.as_str()));

        if let Some(stg) = stg_item {
            let staging_text_changed = stg.text != base_text;
            let source_text_changed = src_item.text != base_text;

            if staging_text_changed && source_text_changed && stg.text != src_item.text {
                // Both changed item text differently → conflict.
                conflicts.push(PlanConflict {
                    section_id: section_id.to_string(),
                    conflict_type: ConflictType::ItemTextConflict,
                    base_text: base_text.to_string(),
                    staging_text: stg.text.clone(),
                    source_text: src_item.text.clone(),
                    description: format!(
                        "Section {}: item {} text changed by both source and agent",
                        section_id,
                        i + 1
                    ),
                });
                // Take source text for conflicts.
            } else if staging_text_changed && !source_text_changed {
                // Only agent changed text — but this might just be a rename.
                // Item text changes (not checkbox) that don't conflict are still reported as additions.
                if stg.checked && !src_item.checked {
                    // Checkbox union: [x] wins regardless.
                    merged[i].checked = true;
                    merged[i].raw_line = src_item.raw_line.replacen("- [ ] ", "- [x] ", 1);
                }
            } else {
                // Checkbox union: if either staging or source checked it, it's checked.
                let either_checked = stg.checked || src_item.checked;
                let base_was_unchecked = !base_checked;
                if either_checked && base_was_unchecked && !merged[i].checked {
                    merged[i].checked = true;
                    merged[i].raw_line = merged[i].raw_line.replacen("- [ ] ", "- [x] ", 1);
                }
            }
        }
    }

    // Agent-inserted items that don't exist in source — append them.
    for (i, stg_item) in staging_items.iter().enumerate() {
        if i >= source_items.len() {
            // Item index beyond source length — agent added items.
            let base_had_it = base_items.get(i).is_some();
            if !base_had_it {
                merged.push(stg_item.clone());
            }
        }
    }

    merged
}

/// A comparable signature of a section body's non-item, non-marker
/// content -- everything that isn't a `<!-- status: ... -->` line or a
/// checkbox item line. Used to detect whether the *prose* of a section
/// changed, independent of the item/status reconciliation that already
/// handles checkbox and marker lines on their own.
fn non_item_body_signature(body: &str) -> String {
    body.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with("<!-- status:") && t.ends_with("-->")) && !is_any_item(t)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reconstruct a section body from the source body, replacing status marker and items.
fn reconstruct_body(
    _base_body: &str,
    source_body: &str,
    merged_status: &Option<String>,
    merged_items: &[PlanItem],
    _section_id: &str,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut item_idx = 0;

    for line in source_body.lines() {
        let trimmed = line.trim();

        // Replace status marker.
        if trimmed.starts_with("<!-- status:") && trimmed.ends_with("-->") {
            if let Some(ref status) = merged_status {
                lines.push(status.to_string());
            }
            continue;
        }

        // Replace items.
        if trimmed.starts_with("- [ ] ")
            || trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ")
        {
            if let Some(item) = merged_items.get(item_idx) {
                lines.push(item.raw_line.clone());
                item_idx += 1;
            } else {
                lines.push(line.to_string());
            }
            continue;
        }

        lines.push(line.to_string());
    }

    // Append any extra items beyond what was in source.
    while item_idx < merged_items.len() {
        lines.push(merged_items[item_idx].raw_line.clone());
        item_idx += 1;
    }

    lines.join("\n")
}

/// Returns true if `new_status` represents a forward advancement over `old_status`.
fn is_status_advancement(old: Option<&str>, new: Option<&str>) -> bool {
    fn rank(s: Option<&str>) -> u8 {
        match s {
            None => 0,
            Some(s) if s.contains("pending") => 1,
            Some(s) if s.contains("in_progress") => 2,
            Some(s) if s.contains("done") => 3,
            _ => 0,
        }
    }
    rank(new) > rank(old)
}

// ── Post-merge structural validation (v0.15.28.1) ───────────────────────────

/// A missing heading or marker detected during post-merge validation.
#[derive(Debug, Clone)]
pub struct PlanValidationIssue {
    /// The section ID (e.g. "v0.15.28") or a human-readable label for structural issues.
    pub section_id: String,
    /// Human-readable description of what is missing or malformed.
    pub description: String,
}

/// Errors detected during post-merge PLAN.md structural validation.
#[derive(Debug)]
pub struct PlanValidationError {
    pub issues: Vec<PlanValidationIssue>,
}

impl std::fmt::Display for PlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "PLAN.md merge validation failed:")?;
        for issue in &self.issues {
            writeln!(f, "  - [{}] {}", issue.section_id, issue.description)?;
        }
        Ok(())
    }
}

impl std::error::Error for PlanValidationError {}

/// Validate the post-merge PLAN.md against the source PLAN.md.
///
/// Checks:
/// (a) All `### v0.x.y` headings from source are present in the merged result.
/// (b) Each heading in the merged result has a matching `<!-- status: ... -->` marker.
/// (c) No phase section contains only blank lines between its heading and the next
///     `---` or `###` separator (content-less sections indicate dropped content).
///
/// Returns `Ok(())` when valid, `Err(PlanValidationError)` with a structured
/// report of what is missing.
pub fn validate_plan_merge(merged: &str, source: &str) -> Result<(), PlanValidationError> {
    let source_sections = parse_plan_sections(source);
    let merged_sections = parse_plan_sections(merged);

    let merged_ids: std::collections::HashSet<&str> =
        merged_sections.iter().map(|s| s.id.as_str()).collect();

    let mut issues = Vec::new();

    // (a) All versioned source headings must be present in merged.
    for src in &source_sections {
        if src.id.starts_with("__") {
            continue; // Skip preamble / tail pseudo-sections.
        }
        if !merged_ids.contains(src.id.as_str()) {
            issues.push(PlanValidationIssue {
                section_id: src.id.clone(),
                description: format!(
                    "heading '### {}' present in source but missing from merged result",
                    src.id
                ),
            });
        }
    }

    // (b) Each merged heading must have a <!-- status: ... --> marker if
    //     it's version-style (unconditionally, matching this rule's
    //     original behavior exactly), or if it's a plain-named phase that
    //     already had a marker in source (a real dropped-marker
    //     regression). A plain-named heading that never had a status
    //     marker in the first place (a purely narrative section) is not
    //     required to gain one -- see `extract_version_header`'s doc
    //     comment for why plain headings are now their own sections at
    //     all, and `golden_failure_validate_plan_merge_does_not_require_a_marker_on_narrative_headings`.
    let source_had_marker: std::collections::HashSet<&str> = source_sections
        .iter()
        .filter(|s| !s.id.starts_with("__") && s.status_marker.is_some())
        .map(|s| s.id.as_str())
        .collect();
    for sec in &merged_sections {
        if sec.id.starts_with("__") {
            continue;
        }
        let must_have_marker =
            is_version_token(&sec.id) || source_had_marker.contains(sec.id.as_str());
        if must_have_marker && sec.status_marker.is_none() {
            issues.push(PlanValidationIssue {
                section_id: sec.id.clone(),
                description: format!(
                    "section '### {}' has no <!-- status: ... --> marker",
                    sec.id
                ),
            });
        }
    }

    // (c) No versioned section should contain only blank lines / status markers
    //     (dropped content). A section body with only the status marker and blanks
    //     indicates that goal descriptions, items, or sub-headings were lost.
    for sec in &merged_sections {
        if sec.id.starts_with("__") {
            continue;
        }
        let body_has_content = sec.raw_body.lines().any(|l| {
            let t = l.trim();
            !t.is_empty() && t != "---" && !(t.starts_with("<!-- status:") && t.ends_with("-->"))
        });
        if !body_has_content {
            issues.push(PlanValidationIssue {
                section_id: sec.id.clone(),
                description: format!(
                    "section '### {}' body is blank — content may have been dropped by merge",
                    sec.id
                ),
            });
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(PlanValidationError { issues })
    }
}

/// Count `### v0.x.y` headings in a PLAN.md string.
pub fn count_plan_headings(content: &str) -> usize {
    content
        .lines()
        .filter(|l| extract_version_header(l).is_some())
        .count()
}

/// Count `<!-- status: ... -->` markers in a PLAN.md string.
pub fn count_status_markers(content: &str) -> usize {
    content
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("<!-- status:") && t.ends_with("-->")
        })
        .count()
}

// ── Item/status consistency (v0.15.29.2) ─────────────────────────────────────

/// Extract the status value from a `<!-- status: ... -->` line.
fn extract_status_value(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.starts_with("<!-- status:") && t.ends_with("-->") {
        let inner = &t["<!-- status:".len()..t.len() - "-->".len()];
        Some(inner.trim())
    } else {
        None
    }
}

/// Check every `<!-- status: done -->` phase for unchecked `[ ]` items.
///
/// Returns one `PlanValidationIssue` per phase that has unchecked items.
/// These are **warning-level** only — the status marker is authoritative.
pub fn check_done_phase_item_consistency(content: &str) -> Vec<PlanValidationIssue> {
    let mut issues = Vec::new();
    let mut in_done_phase = false;
    let mut current_phase_id = String::new();
    let mut unchecked_count = 0usize;

    let flush = |phase_id: &str, count: usize, out: &mut Vec<PlanValidationIssue>| {
        if !phase_id.is_empty() && count > 0 {
            out.push(PlanValidationIssue {
                section_id: phase_id.to_string(),
                description: format!(
                    "section is 'done' but {} item(s) are unchecked — possible merge corruption",
                    count
                ),
            });
        }
    };

    for line in content.lines() {
        let t = line.trim();
        if let Some(id) = extract_version_header(line) {
            flush(&current_phase_id, unchecked_count, &mut issues);
            current_phase_id = id;
            in_done_phase = false;
            unchecked_count = 0;
        } else if let Some(status) = extract_status_value(t) {
            in_done_phase = status == "done";
        } else if in_done_phase && is_unchecked_item(t) {
            unchecked_count += 1;
        }
    }
    flush(&current_phase_id, unchecked_count, &mut issues);

    issues
}

/// Auto-correct unchecked `[ ]` items in `<!-- status: done -->` phases.
///
/// Returns `(corrected_content, corrections)` where each correction is
/// `(phase_id, item_number_1based)`. Logs nothing itself — callers should
/// print `[plan] auto-checked item N in vX.Y.Z (phase is done; checkmark lost in merge)`.
pub fn auto_correct_done_phase_items(content: &str) -> (String, Vec<(String, usize)>) {
    let mut in_done_phase = false;
    let mut current_phase_id = String::new();
    let mut item_num = 0usize;
    let mut result_lines: Vec<String> = Vec::new();
    let mut corrections: Vec<(String, usize)> = Vec::new();

    for line in content.lines() {
        let t = line.trim();

        if let Some(id) = extract_version_header(line) {
            current_phase_id = id;
            in_done_phase = false;
            item_num = 0;
            result_lines.push(line.to_string());
            continue;
        } else if let Some(status) = extract_status_value(t) {
            in_done_phase = status == "done";
            result_lines.push(line.to_string());
            continue;
        }

        if in_done_phase && is_any_item(t) {
            item_num += 1;
            if is_unchecked_item(t) {
                corrections.push((current_phase_id.clone(), item_num));
                // Replace the first `[ ]` with `[x]` preserving leading whitespace.
                let fixed = if let Some(pos) = line.find("[ ]") {
                    format!("{}[x]{}", &line[..pos], &line[pos + 3..])
                } else {
                    line.to_string()
                };
                result_lines.push(fixed);
                continue;
            }
        }

        result_lines.push(line.to_string());
    }

    let mut out = result_lines.join("\n");
    // Restore the exact trailing newlines of the original content.
    let trailing = content.len() - content.trim_end_matches('\n').len();
    while out.ends_with('\n') {
        out.pop();
    }
    for _ in 0..trailing {
        out.push('\n');
    }
    (out, corrections)
}

/// Returns true if `phase_id`'s own section in `content` has any unchecked
/// `[ ]` item, regardless of that section's own status marker.
///
/// Used to gate the force-`done` status bump in `ta draft apply` (v0.17.10.1
/// item 1): forcing a phase to `done` when its own diff content still shows
/// unchecked items would silently paper over incomplete work. Returns `false`
/// (safe to bump) when the phase is not found in `content` at all — the
/// absence of the phase gives no evidence of incompleteness.
pub fn phase_has_unchecked_items(content: &str, phase_id: &str) -> bool {
    let norm_target = phase_id.strip_prefix('v').unwrap_or(phase_id);
    parse_plan_sections(content)
        .iter()
        .find(|s| s.id.strip_prefix('v').unwrap_or(s.id.as_str()) == norm_target)
        .map(|s| s.raw_body.lines().any(|l| is_unchecked_item(l.trim())))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_plan(sections: &[(&str, &str, &[&str])]) -> String {
        let mut out = String::new();
        for (id, status, items) in sections {
            out.push_str(&format!("### {} — Title\n", id));
            out.push_str(&format!("<!-- status: {} -->\n", status));
            for item in *items {
                out.push_str(item);
                out.push('\n');
            }
            out.push_str("\n---\n\n");
        }
        out
    }

    #[test]
    fn source_updated_status_staging_did_not() {
        let base = make_plan(&[("v0.1.0", "pending", &["- [ ] item a"])]);
        let staging = base.clone();
        let source = make_plan(&[("v0.1.0", "in_progress", &["- [ ] item a"])]);

        let result = merge_plan_md(&base, &staging, &source);

        assert_eq!(result.conflicts.len(), 0);
        assert_eq!(result.silent_fixes.len(), 1);
        assert!(result.silent_fixes[0].contains("taking source"));
        assert!(result.merged.contains("in_progress"));
    }

    #[test]
    fn agent_completed_phase() {
        let base = make_plan(&[("v0.1.0", "pending", &["- [ ] item a"])]);
        let staging = make_plan(&[("v0.1.0", "done", &["- [x] item a"])]);
        let source = base.clone();

        let result = merge_plan_md(&base, &staging, &source);

        assert_eq!(result.conflicts.len(), 0);
        assert!(!result.agent_additions.is_empty());
        assert!(result.merged.contains("done"));
    }

    #[test]
    fn debug_repro_v17_10_3_item2_scenario() {
        // Reproduces the v0.17.10.2 live incident: base == source for the
        // affected phase's section (no concurrent edit to it), staging has
        // items newly checked with status deliberately left in_progress
        // (the documented agent convention), and source additionally has a
        // brand-new phase appended (added directly to main while the goal
        // ran) that neither base nor staging know about.
        let base = make_plan(&[(
            "v0.17.10.2",
            "in_progress",
            &["- [ ] item a", "- [ ] item b"],
        )]);
        let staging = make_plan(&[(
            "v0.17.10.2",
            "in_progress",
            &["- [x] item a", "- [x] item b"],
        )]);
        let source = make_plan(&[
            (
                "v0.17.10.2",
                "in_progress",
                &["- [ ] item a", "- [ ] item b"],
            ),
            ("v0.17.10.3", "pending", &["- [ ] new phase item"]),
        ]);

        let result = merge_plan_md(&base, &staging, &source);

        eprintln!("MERGED:\n{}", result.merged);
        eprintln!("conflicts: {:?}", result.conflicts);
        eprintln!("silent_fixes: {:?}", result.silent_fixes);
        eprintln!("agent_additions: {:?}", result.agent_additions);

        let checked = result.merged.matches("- [x]").count();
        assert_eq!(
            checked, 2,
            "expected both v0.17.10.2 items to remain checked after merge"
        );
        assert!(
            result.merged.contains("v0.17.10.3"),
            "expected the concurrently-added v0.17.10.3 phase to survive the merge"
        );
    }

    #[test]
    fn both_changed_same_status_conflict() {
        let base = make_plan(&[("v0.1.0", "pending", &[])]);
        let staging = make_plan(&[("v0.1.0", "done", &[])]);
        let source = make_plan(&[("v0.1.0", "in_progress", &[])]);

        let result = merge_plan_md(&base, &staging, &source);

        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(
            result.conflicts[0].conflict_type,
            ConflictType::StatusConflict
        );
    }

    #[test]
    fn agent_inserted_sub_phase_not_in_base_or_source() {
        let base = make_plan(&[("v0.1.0", "done", &[])]);
        let staging_content = format!(
            "{}{}",
            make_plan(&[("v0.1.0", "done", &[])]),
            make_plan(&[("v0.1.1", "pending", &["- [ ] new item"])])
        );
        let source = base.clone();

        let result = merge_plan_md(&base, &staging_content, &source);

        assert!(result.agent_additions.iter().any(|a| a.contains("v0.1.1")));
        assert!(result.merged.contains("v0.1.1"));
    }

    #[test]
    fn checkbox_union_either_side_checked_wins() {
        let base = make_plan(&[("v0.1.0", "pending", &["- [ ] item a", "- [ ] item b"])]);
        let staging = make_plan(&[("v0.1.0", "pending", &["- [x] item a", "- [ ] item b"])]);
        let source = make_plan(&[("v0.1.0", "pending", &["- [ ] item a", "- [x] item b"])]);

        let result = merge_plan_md(&base, &staging, &source);

        assert_eq!(result.conflicts.len(), 0);
        // Both items should be checked.
        let checked_count = result.merged.matches("- [x]").count();
        assert_eq!(checked_count, 2);
    }

    #[test]
    fn item_text_conflict_reported() {
        let base = make_plan(&[("v0.1.0", "pending", &["- [ ] original text"])]);
        let staging = make_plan(&[("v0.1.0", "pending", &["- [ ] agent rewrite"])]);
        let source = make_plan(&[("v0.1.0", "pending", &["- [ ] source rewrite"])]);

        let result = merge_plan_md(&base, &staging, &source);

        assert!(!result.conflicts.is_empty());
        assert_eq!(
            result.conflicts[0].conflict_type,
            ConflictType::ItemTextConflict
        );
    }

    // --- v0.15.24.5 tests ---

    #[test]
    fn agent_strips_items_source_items_preserved() {
        // (a) Agent removes all items from a phase in staging.
        // The merged result must still contain the source's items — the agent
        // cannot silently delete plan items that the reviewer relies on.
        let base = make_plan(&[("v0.2.0", "pending", &["- [ ] item one", "- [ ] item two"])]);
        // Agent wrote a stripped version with no items.
        let staging = make_plan(&[("v0.2.0", "pending", &[])]);
        let source = base.clone();

        let result = merge_plan_md(&base, &staging, &source);

        assert_eq!(result.conflicts.len(), 0);
        assert!(result.merged.contains("item one"), "item one must survive");
        assert!(result.merged.contains("item two"), "item two must survive");
    }

    #[test]
    fn agent_adds_new_phase_source_items_intact() {
        // (b) Agent adds a new phase section not in base or source.
        // The new section must appear in merged output AND the existing phase's
        // items must remain intact.
        let base = make_plan(&[("v0.1.0", "done", &["- [x] existing item"])]);
        let new_phase = make_plan(&[("v0.1.1", "pending", &["- [ ] new task"])]);
        let staging_content = format!("{}{}", base, new_phase);
        let source = base.clone();

        let result = merge_plan_md(&base, &staging_content, &source);

        assert_eq!(result.conflicts.len(), 0);
        assert!(
            result.agent_additions.iter().any(|a| a.contains("v0.1.1")),
            "new phase must be reported as agent addition"
        );
        assert!(
            result.merged.contains("v0.1.1"),
            "new phase must be in merged output"
        );
        assert!(
            result.merged.contains("new task"),
            "new phase items must be in merged output"
        );
        assert!(
            result.merged.contains("existing item"),
            "original items must be preserved"
        );
    }

    #[test]
    fn staging_identical_to_source_result_equals_source() {
        // (c) When staging and source are identical, the merged result equals source.
        let base = make_plan(&[("v0.3.0", "pending", &["- [ ] alpha", "- [ ] beta"])]);
        let source = make_plan(&[("v0.3.0", "in_progress", &["- [ ] alpha", "- [ ] beta"])]);
        let staging = source.clone(); // staging == source

        let result = merge_plan_md(&base, &staging, &source);

        assert_eq!(result.conflicts.len(), 0);
        // Result should match source (both sides agree on the same content).
        assert!(result.merged.contains("in_progress"));
        assert!(result.merged.contains("alpha"));
        assert!(result.merged.contains("beta"));
    }

    #[test]
    fn agent_checked_items_preserved_in_merge() {
        // (a) Agent checked off items that source still has unchecked.
        // The checkbox union rule must apply: [x] wins.
        let base = make_plan(&[(
            "v0.4.0",
            "pending",
            &["- [ ] step A", "- [ ] step B", "- [ ] step C"],
        )]);
        let staging = make_plan(&[(
            "v0.4.0",
            "pending",
            &["- [x] step A", "- [x] step B", "- [ ] step C"],
        )]);
        let source = base.clone();

        let result = merge_plan_md(&base, &staging, &source);

        assert_eq!(result.conflicts.len(), 0);
        let checked = result.merged.matches("- [x]").count();
        assert_eq!(checked, 2, "agent's two checked items must be present");
        assert!(
            result.merged.contains("step C"),
            "unchecked item must survive"
        );
    }

    // --- v0.15.28.1 structural validation tests ---

    #[test]
    fn validate_clean_merge_passes() {
        let plan = make_plan(&[("v0.5.0", "pending", &["- [ ] task one"])]);
        assert!(validate_plan_merge(&plan, &plan).is_ok());
    }

    #[test]
    fn validate_missing_heading_fails() {
        let source = make_plan(&[
            ("v0.5.0", "pending", &["- [ ] task one"]),
            ("v0.5.1", "pending", &["- [ ] task two"]),
        ]);
        // merged is missing v0.5.1
        let merged = make_plan(&[("v0.5.0", "pending", &["- [ ] task one"])]);

        let err = validate_plan_merge(&merged, &source).unwrap_err();
        assert!(err.issues.iter().any(|i| i.section_id == "v0.5.1"));
    }

    #[test]
    fn validate_missing_status_marker_fails() {
        let source = make_plan(&[("v0.6.0", "pending", &["- [ ] item"])]);
        // merged lacks the status marker
        let merged = "### v0.6.0 — Title\n\n- [ ] item\n\n---\n\n";

        let err = validate_plan_merge(merged, &source).unwrap_err();
        assert!(err.issues.iter().any(|i| i.section_id == "v0.6.0"));
    }

    #[test]
    fn validate_blank_section_body_fails() {
        let source = make_plan(&[("v0.7.0", "pending", &["- [ ] item"])]);
        // merged section has only blank lines in body
        let merged = "### v0.7.0 — Title\n<!-- status: pending -->\n\n\n---\n\n";

        let err = validate_plan_merge(merged, &source).unwrap_err();
        assert!(err.issues.iter().any(|i| i.section_id == "v0.7.0"));
    }

    #[test]
    fn count_headings_and_markers() {
        let plan = make_plan(&[
            ("v0.1.0", "done", &["- [x] a"]),
            ("v0.2.0", "pending", &["- [ ] b"]),
        ]);
        assert_eq!(count_plan_headings(&plan), 2);
        assert_eq!(count_status_markers(&plan), 2);
    }

    // --- v0.15.29.2 item consistency tests ---

    #[test]
    fn check_consistency_clean_done_phase() {
        let plan = make_plan(&[("v0.1.0", "done", &["- [x] item one", "- [x] item two"])]);
        let issues = check_done_phase_item_consistency(&plan);
        assert!(issues.is_empty(), "all-checked done phase should be clean");
    }

    #[test]
    fn check_consistency_detects_unchecked_in_done() {
        let plan = make_plan(&[("v0.1.0", "done", &["- [x] done", "- [ ] missed"])]);
        let issues = check_done_phase_item_consistency(&plan);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].section_id, "v0.1.0");
        assert!(issues[0].description.contains("1 item(s) are unchecked"));
    }

    #[test]
    fn check_consistency_pending_phase_ignored() {
        let plan = make_plan(&[("v0.1.0", "pending", &["- [ ] not done yet"])]);
        let issues = check_done_phase_item_consistency(&plan);
        assert!(issues.is_empty(), "pending phases should not be flagged");
    }

    #[test]
    fn check_consistency_counts_multiple_unchecked() {
        let plan = make_plan(&[("v0.2.0", "done", &["- [ ] a", "- [ ] b", "- [x] c"])]);
        let issues = check_done_phase_item_consistency(&plan);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].description.contains("2 item(s) are unchecked"));
    }

    #[test]
    fn auto_correct_fixes_unchecked_in_done_phase() {
        let plan = make_plan(&[("v0.1.0", "done", &["- [x] done", "- [ ] missed"])]);
        let (corrected, corrections) = auto_correct_done_phase_items(&plan);
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].0, "v0.1.0");
        assert_eq!(corrections[0].1, 2); // 2nd item
        assert!(
            corrected.contains("- [x] missed"),
            "unchecked item should be corrected"
        );
        assert!(
            !corrected.contains("- [ ] missed"),
            "original unchecked should be gone"
        );
    }

    #[test]
    fn auto_correct_leaves_pending_phase_alone() {
        let plan = make_plan(&[("v0.1.0", "pending", &["- [ ] still pending"])]);
        let (corrected, corrections) = auto_correct_done_phase_items(&plan);
        assert!(
            corrections.is_empty(),
            "pending phase items must not be corrected"
        );
        assert_eq!(corrected, plan);
    }

    #[test]
    fn auto_correct_no_change_when_clean() {
        let plan = make_plan(&[("v0.1.0", "done", &["- [x] all good"])]);
        let (corrected, corrections) = auto_correct_done_phase_items(&plan);
        assert!(corrections.is_empty());
        assert_eq!(corrected, plan);
    }

    // v0.17.10.1 item 1: gate the force-`done` status bump on the phase's own
    // diff content, not just on the fact that `--phase` was passed.
    #[test]
    fn phase_has_unchecked_items_true_for_bullet_style() {
        let plan = make_plan(&[(
            "v0.17.10.1",
            "in_progress",
            &["- [x] done item", "- [ ] not done item"],
        )]);
        assert!(phase_has_unchecked_items(&plan, "v0.17.10.1"));
    }

    #[test]
    fn phase_has_unchecked_items_true_for_numbered_style() {
        // Real PLAN.md phases use "1. [ ]" numbered items, not "- [ ]" bullets.
        let plan = "### v0.17.10.1 — Title\n<!-- status: in_progress -->\n\
                     1. [x] done item\n2. [ ] not done item\n\n---\n\n";
        assert!(phase_has_unchecked_items(plan, "v0.17.10.1"));
    }

    #[test]
    fn phase_has_unchecked_items_false_when_all_checked() {
        let plan = make_plan(&[(
            "v0.17.10.1",
            "in_progress",
            &["- [x] done item", "- [x] also done"],
        )]);
        assert!(!phase_has_unchecked_items(&plan, "v0.17.10.1"));
    }

    #[test]
    fn phase_has_unchecked_items_false_when_phase_not_found() {
        let plan = make_plan(&[("v0.1.0", "in_progress", &["- [ ] item a"])]);
        assert!(!phase_has_unchecked_items(&plan, "v9.9.9"));
    }

    #[test]
    fn phase_has_unchecked_items_matches_id_without_leading_v() {
        let plan = make_plan(&[("v0.17.10.1", "in_progress", &["- [ ] item a"])]);
        assert!(phase_has_unchecked_items(&plan, "0.17.10.1"));
    }

    // ── Golden-failure regressions: mixed version/plain-named headings ─────
    //
    // Found live 2026-09-15 against `ta-virtual-team`'s real PLAN.md (goal
    // `d61f9301`): that file mixes `### v0.x.y.z` version-style phase
    // headings with plain-named ones (`### Phase 1 -- ...`, `### Wiki
    // retrieval + ingestion design`, no version token at all).
    // `extract_version_header` only recognized the version-style form, so
    // every plain-named heading's content was silently swallowed into
    // whichever version-style section preceded it (or `__preamble__` if
    // none had appeared yet). Two independent corruptions followed:
    //   1. `reconstruct_body` blindly overwrites *every* embedded
    //      `<!-- status: ... -->` line it finds inside a swallowed
    //      section's body with that section's own single reconciled
    //      status -- clobbering every unrecognized sub-heading's real
    //      marker with an unrelated one.
    //   2. `auto_correct_done_phase_items`'s own raw-text scanner then
    //      sees those freshly-clobbered "done" markers and force-checks
    //      whatever `[ ]` items follow, using one running item counter
    //      that spans every swallowed phase, not just the one that
    //      "owns" that counter.
    //
    // This fixture reproduces the real document's structure closely
    // enough to trigger both: a `done` phase (Phase 0) immediately
    // followed by an `in_progress` phase with unchecked items (Phase 1,
    // items 2-4) inside `__preamble__`; a `done` version-style phase
    // (v0.0.0.1) swallowing two `in_progress` plain phases with checked
    // items (Phase 5, 6); and the actually-touched phase (v0.0.0.1.1)
    // swallowing a plain `in_progress` phase and two `pending` ones
    // (Phase 7, 8). Confirmed via direct reproduction before writing the
    // fix that these exact corruptions occur on unfixed code: Phase 1
    // items 2-4 force-checked and its own status flipped `done`; Phase 5/6
    // flipped `in_progress` -> `done`; Phase 7/8 flipped `pending` ->
    // `in_progress`. None of these phases were ever touched by the
    // agent's own edit, which only added one line under v0.0.0.1.1.

    fn mixed_heading_incident_fixture() -> &'static str {
        "\
# Plan
Some preamble text.

## Stage 1

### Phase 0 — Repo scaffold
<!-- status: done -->
**Items**:
1. [x] scaffold done

## Stage 2

### Phase 1 — Real roster, real work
<!-- status: in_progress -->

**Items**:
1. [x] roster done
2. [ ] run real goals
3. [ ] exercise whiteboard
4. [ ] draft review apply cycle

---

### v0.0.0.1 — CONTRIBUTING note (original)
<!-- status: done -->
*Inserted goal.*

### Phase 5 — Wire intake end to end
<!-- status: in_progress -->
**Items**:
1. [x] poller publishes candidate

### Phase 6 — Report-back leg
<!-- status: in_progress -->
**Items**:
1. [x] outcome messages drive patch

### v0.0.0.1.1 — CONTRIBUTING note (duplicate)
<!-- status: in_progress -->
*Inserted goal.*

### Wiki retrieval + ingestion design
<!-- status: in_progress -->
**Items**:
1. [x] retrieval spec
6. [ ] background sync

### Phase 7 — Human escalation
<!-- status: pending -->
**Items**:
1. [ ] comment flag glue

### Phase 8 — Cross-links
<!-- status: pending -->
**Items**:
1. [ ] wayfinder task link
"
    }

    /// Builds the incident's exact base/staging/source triple: base and
    /// source are byte-identical (nothing else touched PLAN.md before this
    /// apply); staging is base plus one plain-text line the agent added
    /// under v0.0.0.1.1's own body -- no checkbox, no status change,
    /// exactly what the real incident's draft diff contained.
    fn mixed_heading_incident_triple() -> (&'static str, String, &'static str) {
        let content = mixed_heading_incident_fixture();
        let staging = content.replace(
            "### v0.0.0.1.1 — CONTRIBUTING note (duplicate)\n<!-- status: in_progress -->\n*Inserted goal.*\n",
            "### v0.0.0.1.1 — CONTRIBUTING note (duplicate)\n<!-- status: in_progress -->\n*Inserted goal.*\n\n**Note**: Already satisfied.\n",
        );
        assert_ne!(content, staging, "the fixture edit must actually apply");
        (content, staging, content)
    }

    #[test]
    fn golden_failure_untouched_plain_named_phases_keep_their_own_status_and_items() {
        let (base, staging, source) = mixed_heading_incident_triple();

        let result = merge_plan_md(base, &staging, source);
        let (merged, corrections) = auto_correct_done_phase_items(&result.merged);

        // The only line that should differ anywhere in the whole document
        // is the one the agent actually added, under v0.0.0.1.1.
        assert!(
            merged.contains("**Note**: Already satisfied."),
            "the agent's real edit must still be present"
        );
        assert!(
            merged.contains("### Phase 1 — Real roster, real work\n<!-- status: in_progress -->"),
            "Phase 1's own status marker must stay in_progress -- it was never touched:\n{merged}"
        );
        assert!(
            merged.contains("2. [ ] run real goals")
                && merged.contains("3. [ ] exercise whiteboard")
                && merged.contains("4. [ ] draft review apply cycle"),
            "Phase 1's own unchecked items must stay unchecked -- it was never touched:\n{merged}"
        );
        assert!(
            merged.contains("### Phase 5 — Wire intake end to end\n<!-- status: in_progress -->"),
            "Phase 5's own status marker must stay in_progress -- it was never touched:\n{merged}"
        );
        assert!(
            merged.contains("### Phase 6 — Report-back leg\n<!-- status: in_progress -->"),
            "Phase 6's own status marker must stay in_progress -- it was never touched:\n{merged}"
        );
        assert!(
            merged.contains("### Phase 7 — Human escalation\n<!-- status: pending -->"),
            "Phase 7's own status marker must stay pending -- it was never touched:\n{merged}"
        );
        assert!(
            merged.contains("### Phase 8 — Cross-links\n<!-- status: pending -->"),
            "Phase 8's own status marker must stay pending -- it was never touched:\n{merged}"
        );
        assert!(
            corrections.is_empty(),
            "no phase in this document actually has a done-status/unchecked-item \
             inconsistency -- any correction here is by definition touching a phase \
             that was never part of this apply's own diff: {corrections:?}"
        );
    }

    #[test]
    fn golden_failure_plain_named_heading_is_its_own_parsed_section() {
        // The narrower regression, isolated from the full merge pipeline:
        // a plain `### Phase N -- Title` heading must be recognized as its
        // own section boundary, not swallowed into whichever version-style
        // section happens to precede it.
        let sections = parse_plan_sections(mixed_heading_incident_fixture());
        let ids: Vec<&str> = sections.iter().map(|s| s.id.as_str()).collect();
        for expected in [
            "Phase 0 — Repo scaffold",
            "Phase 1 — Real roster, real work",
            "v0.0.0.1",
            "Phase 5 — Wire intake end to end",
            "Phase 6 — Report-back leg",
            "v0.0.0.1.1",
            "Wiki retrieval + ingestion design",
            "Phase 7 — Human escalation",
            "Phase 8 — Cross-links",
        ] {
            assert!(
                ids.contains(&expected),
                "expected a distinct section for {expected:?}, got: {ids:?}"
            );
        }

        let phase1 = sections
            .iter()
            .find(|s| s.id == "Phase 1 — Real roster, real work")
            .expect("Phase 1 section must exist");
        assert_eq!(
            phase1.status_marker.as_deref(),
            Some("<!-- status: in_progress -->")
        );
    }

    #[test]
    fn golden_failure_validate_plan_merge_does_not_require_a_marker_on_narrative_headings() {
        // A widened parse must not turn every purely-narrative `###`
        // heading (no <!-- status --> line, ever) into a false-positive
        // "missing status marker" validation failure -- that would abort
        // every apply touching a PLAN.md with any non-phase heading in it.
        let plan = "\
# Plan

### Road to Public Alpha

Some narrative prose with no status marker at all.

### v0.1.0 — Real phase
<!-- status: pending -->
- [ ] item
";
        assert!(validate_plan_merge(plan, plan).is_ok());
    }

    #[test]
    fn validate_plan_merge_still_catches_a_dropped_marker_on_a_plain_named_phase() {
        // The relaxation above must not swallow real regressions: a plain-
        // named phase that DID have a status marker in source, but lost it
        // in the merged result, must still fail validation.
        let source = "\
### Phase 1 — Real roster, real work
<!-- status: in_progress -->
- [ ] item
";
        let merged = "\
### Phase 1 — Real roster, real work
- [ ] item
";
        let err = validate_plan_merge(merged, source).unwrap_err();
        assert!(err
            .issues
            .iter()
            .any(|i| i.section_id == "Phase 1 — Real roster, real work"));
    }

    #[test]
    fn golden_failure_agent_added_prose_is_not_silently_dropped() {
        // The second, independent bug this golden test uncovered:
        // `reconstruct_body` only ever iterated `source_body`'s lines, so
        // prose the agent added during staging (not a checkbox item, not a
        // status marker) had no path into the merged output at all and
        // was silently dropped, every time -- not just when section
        // boundaries were also wrong. Minimal repro, isolated from the
        // mixed-heading fixture above.
        let base = make_plan(&[("v0.1.0", "in_progress", &["- [ ] item a"])]);
        let staging = base.replace(
            "<!-- status: in_progress -->\n",
            "<!-- status: in_progress -->\n\n**Note**: added by the agent.\n",
        );
        let source = base.clone();
        assert_ne!(base, staging);

        let result = merge_plan_md(&base, &staging, &source);
        assert!(
            result.merged.contains("**Note**: added by the agent."),
            "the agent's added prose must survive the merge:\n{}",
            result.merged
        );
        assert!(result.conflicts.is_empty());
    }

    #[test]
    fn section_body_conflict_when_staging_and_source_add_different_prose() {
        // Both sides diverged from base in the section's own prose (not
        // its items or status) -- a genuine conflict `SectionBodyConflict`
        // was declared for but never constructed before this fix. Default
        // outcome stays conservative (take source), matching every other
        // unresolved conflict type in this module, but it must be
        // reported, not silently resolved either way.
        let base = make_plan(&[("v0.1.0", "in_progress", &["- [ ] item a"])]);
        let staging = base.replace(
            "<!-- status: in_progress -->\n",
            "<!-- status: in_progress -->\n\n**Note**: agent's own note.\n",
        );
        let source = base.replace(
            "<!-- status: in_progress -->\n",
            "<!-- status: in_progress -->\n\n**Note**: a human's own note.\n",
        );

        let result = merge_plan_md(&base, &staging, &source);
        assert!(result
            .conflicts
            .iter()
            .any(|c| c.conflict_type == ConflictType::SectionBodyConflict
                && c.section_id == "v0.1.0"));
        assert!(result.merged.contains("a human's own note."));
    }
}
