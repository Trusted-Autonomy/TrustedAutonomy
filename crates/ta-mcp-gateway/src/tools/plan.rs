// tools/plan.rs — Plan management MCP tool handler.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use regex::Regex;
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

/// A parsed plan phase (minimal representation for the status tool).
#[derive(Debug, Clone)]
struct PlanPhase {
    id: String,
    title: String,
    status: PlanStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanStatus {
    Done,
    InProgress,
    Deferred,
    Pending,
}

impl PlanStatus {
    fn checkbox(&self) -> &'static str {
        match self {
            PlanStatus::Done => "[x]",
            PlanStatus::InProgress => "[~]",
            PlanStatus::Deferred => "[-]",
            PlanStatus::Pending => "[ ]",
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            PlanStatus::Done => "done",
            PlanStatus::InProgress => "in_progress",
            PlanStatus::Deferred => "deferred",
            PlanStatus::Pending => "pending",
        }
    }
}

/// Parse PLAN.md content into phases using the built-in default schema patterns.
/// Mirrors the logic in `apps/ta-cli/src/commands/plan.rs`.
fn parse_plan_phases(content: &str) -> Vec<PlanPhase> {
    // Phase header patterns (same as PlanSchema::default_schema()).
    let phase_patterns: Vec<Regex> = vec![
        // "## Phase 4b — Title"
        Regex::new(r"(?m)^##\s+Phase[\s\u{a0}]+([0-9a-z.]+)\s+[—\-]\s+(.+)$").unwrap(),
        // "### v0.3.1 — Title"
        Regex::new(r"(?m)^###\s+(v[\d.]+[a-z]?)\s+[—\-]\s+(.+)$").unwrap(),
    ];
    let status_re = Regex::new(r"<!--\s*status:\s*(\w+)\s*-->").unwrap();

    let lines: Vec<&str> = content.lines().collect();
    let mut phases = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();
        for pattern in &phase_patterns {
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
                // Strip trailing markup from title.
                let title = title.trim_end_matches(['*', '(', ')']).trim().to_string();
                // Look at the next line for a status marker.
                let status = if i + 1 < lines.len() {
                    let next = lines[i + 1].trim();
                    if let Some(sc) = status_re.captures(next) {
                        match sc.get(1).map(|m| m.as_str().trim()).unwrap_or("") {
                            "done" => PlanStatus::Done,
                            "in_progress" => PlanStatus::InProgress,
                            "deferred" => PlanStatus::Deferred,
                            _ => PlanStatus::Pending,
                        }
                    } else {
                        PlanStatus::Pending
                    }
                } else {
                    PlanStatus::Pending
                };
                phases.push(PlanPhase { id, title, status });
                break;
            }
        }
        i += 1;
    }
    phases
}

/// Compare phase IDs, normalising the optional `v` prefix.
fn phase_ids_match(parsed_id: &str, phase_id: &str) -> bool {
    if parsed_id == phase_id {
        return true;
    }
    let norm_parsed = parsed_id.strip_prefix('v').unwrap_or(parsed_id);
    let norm_phase = phase_id.strip_prefix('v').unwrap_or(phase_id);
    norm_parsed == norm_phase
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
                .map(|p| format!("- {} Phase {} — {}", p.status.checkbox(), p.id, p.title))
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
                .map(|p| format!("- {} Phase {} — {}", p.status.checkbox(), p.id, p.title))
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
        current.status.checkbox(),
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
            phase.status.checkbox(),
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

    let content = std::fs::read_to_string(&plan_path)
        .map_err(|e| McpError::internal_error(format!("failed to read PLAN.md: {}", e), None))?;

    let phases = parse_plan_phases(&content);

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
                        "status": p.status.as_str(),
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

    fn make_phases() -> Vec<PlanPhase> {
        vec![
            PlanPhase {
                id: "v0.1".to_string(),
                title: "Alpha".to_string(),
                status: PlanStatus::Done,
            },
            PlanPhase {
                id: "v0.2".to_string(),
                title: "Beta".to_string(),
                status: PlanStatus::Done,
            },
            PlanPhase {
                id: "v0.3".to_string(),
                title: "Current".to_string(),
                status: PlanStatus::Pending,
            },
            PlanPhase {
                id: "v0.4".to_string(),
                title: "Next".to_string(),
                status: PlanStatus::Pending,
            },
            PlanPhase {
                id: "v0.5".to_string(),
                title: "Future".to_string(),
                status: PlanStatus::Pending,
            },
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
            .map(|p| serde_json::json!({ "id": p.id, "title": p.title, "status": p.status.as_str() }))
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
        let phases = parse_plan_phases(plan_md);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].id, "v0.1");
        assert_eq!(phases[0].status, PlanStatus::Done);
        assert_eq!(phases[1].id, "v0.2");
        assert_eq!(phases[1].status, PlanStatus::Pending);
    }
}
