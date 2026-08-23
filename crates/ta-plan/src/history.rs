// history.rs — PLAN.md phase-transition history log and in-source status
// mutation helpers (extracted from apps/ta-cli/src/commands/plan.rs,
// v0.17.11.1).

use std::path::Path;

use ta_submit::WorkflowConfig as TaWorkflowConfig;

use crate::parse::{parse_plan, phase_ids_match, update_phase_status};
use crate::schema::PlanStatus;

/// Record a plan phase status change to the history log.
pub fn record_history(
    project_root: &Path,
    phase_id: &str,
    old_status: &PlanStatus,
    new_status: &PlanStatus,
) -> anyhow::Result<()> {
    // Validate state-machine transition. Log a warning for illegal moves;
    // return an error when strict_transitions is enabled in [plan] config.
    if !old_status.is_valid_transition_to(new_status) {
        tracing::warn!(
            phase = %phase_id,
            from = %old_status,
            to = %new_status,
            "invalid plan phase transition — expected pending→in_progress, \
             in_progress→done, or in_progress→pending"
        );
        // Check strict mode from workflow config.
        let wf_path = project_root.join(".ta/workflow.toml");
        let wf_config = TaWorkflowConfig::load_or_default(&wf_path);
        if wf_config.plan.strict_transitions {
            anyhow::bail!(
                "Phase {}: invalid state transition {} → {} (strict_transitions enabled). \
                 Legal: pending → in_progress → done, or in_progress → pending on reset.",
                phase_id,
                old_status,
                new_status
            );
        }
    }

    let ta_dir = project_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;
    let history_path = ta_dir.join("plan_history.jsonl");

    let entry = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "phase_id": phase_id,
        "old_status": old_status.to_string(),
        "new_status": new_status.to_string(),
    });

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)?;
    writeln!(file, "{}", entry)?;
    Ok(())
}

/// Read the full plan-phase transition history log.
pub fn load_history(project_root: &Path) -> anyhow::Result<Vec<serde_json::Value>> {
    let history_path = project_root.join(".ta/plan_history.jsonl");
    if !history_path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&history_path)?;
    let entries: Vec<serde_json::Value> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(entries)
}

/// Mark a phase as `in_progress` in the source PLAN.md.
///
/// Called by `ta run --phase <id>` immediately after staging is created,
/// before the agent launches. Writes to the **source** PLAN.md so that
/// `ta plan status` reflects active work immediately.
///
/// Logs the transition to `.ta/plan_history.jsonl`. No-ops if PLAN.md
/// doesn't exist or the phase is not found.
pub fn mark_phase_in_source(project_root: &Path, phase_id: &str) -> anyhow::Result<()> {
    let plan_path = project_root.join("PLAN.md");
    if !plan_path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&plan_path)?;
    let phases = parse_plan(&content);
    let old_status = phases
        .iter()
        .find(|p| phase_ids_match(&p.id, phase_id))
        .map(|p| p.status.clone())
        .unwrap_or(PlanStatus::Pending);

    // Only update if the phase is currently pending (don't downgrade done→in_progress).
    if !matches!(old_status, PlanStatus::Pending) {
        return Ok(());
    }

    let updated = update_phase_status(&content, phase_id, PlanStatus::InProgress);
    if updated == content {
        // Phase not found or content unchanged — silently no-op.
        return Ok(());
    }
    std::fs::write(&plan_path, &updated)?;
    let _ = record_history(project_root, phase_id, &old_status, &PlanStatus::InProgress);
    Ok(())
}

/// Reset a phase from `in_progress` back to `pending` in the source PLAN.md.
///
/// Called on `ta draft deny`, `ta draft close`, and `ta goal delete` when the
/// associated goal had a linked plan phase. Logs the transition to
/// `.ta/plan_history.jsonl` with the provided `note`.
///
/// No-ops if the phase is not currently `in_progress`. In particular, a `done`
/// phase is an explicit, defended invariant here (not just an accident of the
/// match falling through): a stale/duplicate goal referencing already-completed,
/// already-merged work must never revert it to pending (v0.17.0.12.11).
///
/// Returns `true` if the phase was actually reset, `false` if this was a no-op —
/// callers use this to avoid printing a misleading "reset to pending" message
/// when nothing changed.
///
/// Note: unlike the original `apps/ta-cli` version, this does **not** release
/// the daemon's in-memory phase claim over HTTP — that's a CLI-process
/// concern (talking to a locally running daemon), not core plan-storage
/// logic, so `ta-cli`'s thin wrapper does it after calling this function.
pub fn reset_phase_if_in_progress(
    project_root: &Path,
    phase_id: &str,
    note: &str,
) -> anyhow::Result<bool> {
    let plan_path = project_root.join("PLAN.md");
    if !plan_path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(&plan_path)?;
    let phases = parse_plan(&content);
    let current_status = phases
        .iter()
        .find(|p| phase_ids_match(&p.id, phase_id))
        .map(|p| p.status.clone());

    match current_status {
        Some(PlanStatus::InProgress) => {}
        Some(PlanStatus::Done) => return Ok(false), // never revert a completed phase
        _ => return Ok(false),                      // pending/deferred/missing — nothing to reset
    }

    let updated = update_phase_status(&content, phase_id, PlanStatus::Pending);
    if updated == content {
        return Ok(false);
    }
    std::fs::write(&plan_path, &updated)?;

    // Log with a note field appended to the standard history entry.
    let ta_dir = project_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;
    let history_path = ta_dir.join("plan_history.jsonl");
    let entry = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "phase_id": phase_id,
        "old_status": "in_progress",
        "new_status": "pending",
        "note": note,
    });
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)?;
    writeln!(file, "{}", entry)?;

    Ok(true)
}

/// Insert an ad-hoc phase stub into PLAN.md immediately after the last Done phase.
///
/// The stub has `<!-- status: in_progress -->` since it starts immediately.
/// If the phase ID already exists, this is a no-op.
pub fn insert_adhoc_phase(project_root: &Path, phase_id: &str, title: &str) -> anyhow::Result<()> {
    let plan_path = project_root.join("PLAN.md");
    if !plan_path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&plan_path)?;

    // No-op if phase already exists.
    if content.contains(phase_id) {
        return Ok(());
    }

    // Find the last Done phase and insert after its block.
    // Simple heuristic: find the last `<!-- status: done -->` line, then insert
    // after the paragraph following it (next blank line after the line).
    let stub = format!(
        "\n### {} — {}\n<!-- status: in_progress -->\n*Inserted goal — not in original plan.*\n",
        phase_id, title
    );

    // Find insertion point: after the last `<!-- status: done -->` section.
    // We walk backward looking for the last occurrence of "status: done", then
    // find the next blank line after it (end of that phase's intro paragraph).
    let insert_pos = find_insert_pos_after_last_done(&content);
    let updated = format!(
        "{}{}{}",
        &content[..insert_pos],
        stub,
        &content[insert_pos..]
    );
    std::fs::write(&plan_path, &updated)?;
    Ok(())
}

/// Find the character position immediately after the last Done phase block.
fn find_insert_pos_after_last_done(content: &str) -> usize {
    // Find the last "<!-- status: done -->" occurrence.
    let done_marker = "<!-- status: done -->";
    let last_done_pos = content.rfind(done_marker);
    let Some(done_start) = last_done_pos else {
        // No done phases — insert at the end.
        return content.len();
    };

    // From done_start, scan forward to find the end of this phase's content block.
    // End of block = the next `### ` or `## ` header, or end of file.
    let after_done = done_start + done_marker.len();
    let rest = &content[after_done..];

    // Look for the next section header.
    for (i, line) in rest.lines().enumerate() {
        let trimmed = line.trim();
        if i > 0 && (trimmed.starts_with("### ") || trimmed.starts_with("## ")) {
            // Insert before this header.
            let byte_offset: usize = rest.lines().take(i).map(|l| l.len() + 1).sum();
            return after_done + byte_offset;
        }
    }

    // No next header found — insert at end.
    content.len()
}
