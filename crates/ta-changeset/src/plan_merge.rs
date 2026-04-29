//! Three-way PLAN.md merge for the Draft Pre-Apply Plan Review Agent.
//!
//! Compares base (PLAN.md at staging-creation time), staging (agent's version),
//! and source (current main) to detect regressions, agent additions, and conflicts.

use serde::{Deserialize, Serialize};

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
/// Sections without a `### v0.x.y` version header (preamble, appendices) are
/// returned as opaque sections with `id = "__preamble__"` or `"__tail_N__"`.
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

fn extract_version_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with("### ") {
        return None;
    }
    let rest = &trimmed[4..];
    // Accept "v0.x.y" or "v0.x.y.z" at the start, optionally followed by " —" or " -" title.
    let token = rest.split_whitespace().next().unwrap_or("");
    if token.starts_with('v')
        && token
            .trim_start_matches('v')
            .split('.')
            .all(|p| p.chars().all(|c| c.is_ascii_digit()))
        && token.trim_start_matches('v').contains('.')
    {
        Some(token.to_string())
    } else {
        None
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

    // Reconstruct raw_body from merged items and non-item lines.
    let merged_body = reconstruct_body(
        &base.raw_body,
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

    // (b) Each versioned merged heading must have a <!-- status: ... --> marker.
    for sec in &merged_sections {
        if sec.id.starts_with("__") {
            continue;
        }
        if sec.status_marker.is_none() {
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
        } else if in_done_phase && (t.starts_with("- [ ] ") || t == "- [ ]") {
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

        if in_done_phase
            && (t.starts_with("- [ ] ")
                || t == "- [ ]"
                || t.starts_with("- [x] ")
                || t.starts_with("- [X] "))
        {
            item_num += 1;
            if t.starts_with("- [ ] ") || t == "- [ ]" {
                corrections.push((current_phase_id.clone(), item_num));
                // Replace the first `[ ]` with `[x]` preserving leading whitespace.
                let fixed = if let Some(pos) = line.find("- [ ]") {
                    format!("{}{}[x]{}", &line[..pos], "- ", &line[pos + 5..])
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
}
