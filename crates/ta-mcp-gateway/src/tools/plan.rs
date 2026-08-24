// tools/plan.rs — Plan management MCP tool handler.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;

use ta_changeset::interaction::InteractionRequest;
use ta_goal::TaEvent;

use crate::server::{GatewayState, PlanStatusParams, PlanToolParams};
use crate::validation::{parse_uuid, validate_goal_exists};

pub fn handle_plan(
    state: &Arc<Mutex<GatewayState>>,
    params: PlanToolParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    match params.action.as_str() {
        "read" => {
            // v0.9.6: goal_run_id is optional for read. If provided, reads
            // from that goal's workspace. If omitted, reads from project root.
            let plan_path = if let Some(goal_id_str) = params.goal_run_id.as_deref() {
                let goal_run_id = parse_uuid(goal_id_str)?;
                let goal = validate_goal_exists(&state.goal_store, goal_run_id)?;
                goal.workspace_path.join("PLAN.md")
            } else {
                state.config.workspace_root.join("PLAN.md")
            };

            if plan_path.exists() {
                let content = std::fs::read_to_string(&plan_path)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                Ok(CallToolResult::success(vec![Content::text(content)]))
            } else {
                let response = serde_json::json!({
                    "message": "No PLAN.md found in workspace.",
                });
                Ok(CallToolResult::success(vec![Content::json(response)
                    .map_err(|e| {
                        McpError::internal_error(e.to_string(), None)
                    })?]))
            }
        }
        "update" => {
            let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                McpError::invalid_params("goal_run_id required for update", None)
            })?)?;
            validate_goal_exists(&state.goal_store, goal_run_id)?;
            let phase = params.phase.as_deref().unwrap_or("unknown");
            let status_note = params
                .status_note
                .as_deref()
                .unwrap_or("Agent proposes phase update");

            state
                .event_dispatcher
                .dispatch(&TaEvent::PlanUpdateProposed {
                    goal_run_id,
                    phase: phase.to_string(),
                    status_note: status_note.to_string(),
                    timestamp: Utc::now(),
                });

            let interaction_req =
                InteractionRequest::plan_negotiation(phase, status_note).with_goal_id(goal_run_id);

            let review_result = state.request_review(&interaction_req);

            let (plan_status, plan_decision) = match &review_result {
                Ok(resp) => {
                    let decision_str = format!("{}", resp.decision);
                    (
                        if decision_str == "approved" {
                            "approved"
                        } else {
                            "proposed"
                        },
                        decision_str,
                    )
                }
                Err(_) => ("proposed", "pending".to_string()),
            };

            let response = serde_json::json!({
                "goal_run_id": goal_run_id.to_string(),
                "phase": phase,
                "status": plan_status,
                "decision": plan_decision,
                "message": if plan_decision == "pending" {
                    "Plan update proposed. Human must approve via `ta draft approve` before it takes effect."
                } else {
                    "Plan update reviewed through ReviewChannel."
                },
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        _ => Err(McpError::invalid_params(
            format!("unknown action '{}'. Expected: read, update", params.action),
            None,
        )),
    }
}

// ── ta_plan_status: lazy on-demand plan checklist (v0.14.3.2) ────────────────

use ta_plan::{PlanPhase, PlanStatus};

/// Checklist-box glyph for a phase status. A free function rather than an
/// inherent method since `PlanStatus` is `ta_plan`'s type, not this crate's
/// (v0.17.11.1.2 item 1: this tool used to carry its own local `PlanPhase`/
/// `PlanStatus`/parser, a third independent reimplementation of PLAN.md
/// parsing alongside `ta-cli`'s and `ta-daemon`'s — replaced with `ta_plan`
/// directly, via `PlanStore::list_phases()`, since this tool's read-only
/// windowed-checklist use case has no daemon-response-shape reason to stay
/// separate the way `ta-daemon`'s parser deliberately does).
fn checkbox_for(status: &PlanStatus) -> &'static str {
    match status {
        PlanStatus::Done => "[x]",
        PlanStatus::InProgress => "[~]",
        PlanStatus::Deferred => "[-]",
        PlanStatus::Pending => "[ ]",
    }
}

/// Compare phase IDs, normalising the optional `v` prefix. Re-exported by
/// `ta_plan` too (`phase_ids_match`) — kept as a thin local wrapper so the
/// rest of this file's call sites don't change.
fn phase_ids_match(parsed_id: &str, phase_id: &str) -> bool {
    ta_plan::phase_ids_match(parsed_id, phase_id)
}

/// Format a windowed plan checklist (mirrors `format_plan_checklist_windowed`).
fn format_windowed_checklist(
    phases: &[PlanPhase],
    current_phase: Option<&str>,
    done_window: usize,
    pending_window: usize,
) -> String {
    let current_idx = match current_phase {
        None => {
            // No current phase — show all.
            return phases
                .iter()
                .map(|p| format!("- {} Phase {} — {}", checkbox_for(&p.status), p.id, p.title))
                .collect::<Vec<_>>()
                .join("\n");
        }
        Some(cp) => phases.iter().position(|p| phase_ids_match(&p.id, cp)),
    };

    let current_idx = match current_idx {
        None => {
            // Phase not found — show all.
            return phases
                .iter()
                .map(|p| format!("- {} Phase {} — {}", checkbox_for(&p.status), p.id, p.title))
                .collect::<Vec<_>>()
                .join("\n");
        }
        Some(idx) => idx,
    };

    let before = &phases[..current_idx];
    let current = &phases[current_idx];
    let after = &phases[current_idx + 1..];

    let mut lines: Vec<String> = Vec::new();

    let done_phases: Vec<_> = before
        .iter()
        .filter(|p| matches!(p.status, PlanStatus::Done | PlanStatus::Deferred))
        .collect();
    let non_done_before: Vec<_> = before
        .iter()
        .filter(|p| !matches!(p.status, PlanStatus::Done | PlanStatus::Deferred))
        .collect();

    let shown_done_start = done_phases.len().saturating_sub(done_window);
    let collapsed_count = shown_done_start;

    if collapsed_count > 0 {
        let last_collapsed = &done_phases[collapsed_count - 1];
        lines.push(format!(
            "- [x] Phases 0 – v{} complete ({} phases)",
            last_collapsed.id, collapsed_count
        ));
    }
    for phase in &done_phases[shown_done_start..] {
        let deferred = if phase.status == PlanStatus::Deferred {
            " *(deferred)*"
        } else {
            ""
        };
        lines.push(format!(
            "- [x] Phase {} — {}{}",
            phase.id, phase.title, deferred
        ));
    }
    for phase in non_done_before {
        let cb = if phase.status == PlanStatus::Deferred {
            "[-]"
        } else {
            "[ ]"
        };
        lines.push(format!("- {} Phase {} — {}", cb, phase.id, phase.title));
    }

    // Current phase (bolded + marker).
    lines.push(format!(
        "- {} **Phase {} — {}** <-- current",
        checkbox_for(&current.status),
        current.id,
        current.title
    ));

    // Next pending_window phases after current.
    let mut shown_pending = 0;
    for phase in after {
        if shown_pending >= pending_window {
            break;
        }
        let deferred = if phase.status == PlanStatus::Deferred {
            " *(deferred)*"
        } else {
            ""
        };
        lines.push(format!(
            "- {} Phase {} — {}{}",
            checkbox_for(&phase.status),
            phase.id,
            phase.title,
            deferred
        ));
        shown_pending += 1;
    }

    let remaining = after.len().saturating_sub(shown_pending);
    if remaining > 0 {
        lines.push(format!("- ... ({} more phases)", remaining));
    }

    lines.join("\n")
}

/// Handle `ta_plan_status` — returns the windowed plan checklist on demand (v0.14.3.2).
pub fn handle_plan_status(
    state: &Arc<Mutex<GatewayState>>,
    params: PlanStatusParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    let plan_path = state.config.workspace_root.join("PLAN.md");

    if !plan_path.exists() {
        let response = serde_json::json!({
            "message": "No PLAN.md found in project root.",
        });
        return Ok(CallToolResult::success(vec![
            Content::json(response).map_err(|e| McpError::internal_error(e.to_string(), None))?
        ]));
    }

    let store = ta_plan::FilePlanStore::new(&state.config.workspace_root, &state.config.goals_dir)
        .map_err(|e| McpError::internal_error(format!("failed to open PlanStore: {}", e), None))?;
    let phases = ta_plan::PlanStore::list_phases(&store)
        .map_err(|e| McpError::internal_error(format!("failed to read PLAN.md: {}", e), None))?;

    let done_window = params.done_window.unwrap_or(5) as usize;
    let pending_window = params.pending_window.unwrap_or(5) as usize;

    let format = params.format.as_deref().unwrap_or("text");

    match format {
        "json" => {
            let phase_list: Vec<serde_json::Value> = phases
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "id": p.id,
                        "title": p.title,
                        "status": p.status.to_string(),
                    })
                })
                .collect();
            let response = serde_json::json!({
                "phases": phase_list,
                "total": phases.len(),
                "done": phases.iter().filter(|p| p.status == PlanStatus::Done).count(),
                "pending": phases.iter().filter(|p| p.status == PlanStatus::Pending).count(),
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        _ => {
            // Default: windowed text checklist.
            let checklist = format_windowed_checklist(
                &phases,
                params.phase.as_deref(),
                done_window,
                pending_window,
            );

            let current_line = params.phase.as_deref().and_then(|cp| {
                phases
                    .iter()
                    .find(|p| phase_ids_match(&p.id, cp))
                    .map(|p| format!("\n**You are working on Phase {} — {}.**\n\n", p.id, p.title))
            });

            let output = format!(
                "## Plan Context\n{}Plan progress:\n{}\n",
                current_line.as_deref().unwrap_or(""),
                checklist
            );
            Ok(CallToolResult::success(vec![Content::text(output)]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(id: &str, title: &str, status: PlanStatus) -> PlanPhase {
        PlanPhase {
            id: id.to_string(),
            title: title.to_string(),
            status,
            depends_on: Vec::new(),
            human_review_items: Vec::new(),
            api_impact: Vec::new(),
        }
    }

    fn make_phases() -> Vec<PlanPhase> {
        vec![
            phase("v0.1", "Alpha", PlanStatus::Done),
            phase("v0.2", "Beta", PlanStatus::Done),
            phase("v0.3", "Current", PlanStatus::Pending),
            phase("v0.4", "Next", PlanStatus::Pending),
            phase("v0.5", "Future", PlanStatus::Pending),
        ]
    }

    #[test]
    fn test_ta_plan_status_tool_returns_windowed_checklist() {
        let phases = make_phases();
        let output = format_windowed_checklist(&phases, Some("v0.3"), 5, 5);
        assert!(
            output.contains("**Phase v0.3 — Current** <-- current"),
            "missing current marker"
        );
        assert!(output.contains("Phase v0.4 — Next"), "missing next phase");
        assert!(output.contains("[x]"), "missing done checkbox");
    }

    #[test]
    fn test_windowed_checklist_collapses_old_phases() {
        // With done_window=1, only the last done phase before current is shown individually.
        let phases = make_phases();
        let output = format_windowed_checklist(&phases, Some("v0.3"), 1, 5);
        // v0.1 should be collapsed into the summary line
        assert!(
            output.contains("Phases 0 – vv0.1 complete (1 phases)"),
            "should collapse v0.1: got\n{}",
            output
        );
        // v0.2 should be shown individually (within done_window=1)
        assert!(
            output.contains("Phase v0.2 — Beta"),
            "should show v0.2 individually"
        );
    }

    #[test]
    fn test_windowed_checklist_json_round_trip() {
        let phases = make_phases();
        // JSON format: verify the list structure.
        let phase_list: Vec<serde_json::Value> = phases
            .iter()
            .map(|p| serde_json::json!({ "id": p.id, "title": p.title, "status": p.status.to_string() }))
            .collect();
        assert_eq!(phase_list.len(), 5);
        assert_eq!(phase_list[0]["status"], "done");
        assert_eq!(phase_list[2]["status"], "pending");
    }

    #[test]
    fn test_parse_plan_phases_basic() {
        let plan_md = "\
### v0.1 — Alpha Phase\n\
<!-- status: done -->\n\
\n\
### v0.2 — Beta Phase\n\
<!-- status: pending -->\n\
";
        let phases = ta_plan::parse_plan(plan_md);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].id, "v0.1");
        assert_eq!(phases[0].status, PlanStatus::Done);
        assert_eq!(phases[1].id, "v0.2");
        assert_eq!(phases[1].status, PlanStatus::Pending);
    }
}
