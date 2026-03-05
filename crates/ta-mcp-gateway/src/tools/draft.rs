// tools/draft.rs — Draft/PR package MCP tool handlers.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;

use ta_changeset::interaction::InteractionRequest;
use ta_goal::{GoalRunState, TaEvent};
use ta_memory::{AutoCapture, DraftRejectEvent};

use crate::server::{DraftToolParams, GatewayState, GoalIdParams, PrBuildParams};
use crate::validation::parse_uuid;

pub fn handle_pr_build(
    state: &Arc<Mutex<GatewayState>>,
    params: PrBuildParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;

    let goal = state
        .goal_store
        .get(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
        })?;

    let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let pr_package = connector
        .build_pr_package(&goal.title, &goal.objective, &params.summary, &params.title)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let package_id = pr_package.package_id;
    state
        .save_pr_package(pr_package)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    // Transition goal to PrReady.
    let mut updated_goal = goal;
    updated_goal.pr_package_id = Some(package_id);
    updated_goal
        .transition(GoalRunState::PrReady)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    state
        .goal_store
        .save(&updated_goal)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    state.event_dispatcher.dispatch(&TaEvent::PrReady {
        goal_run_id,
        pr_package_id: package_id,
        summary: params.summary,
        timestamp: Utc::now(),
    });

    let response = serde_json::json!({
        "pr_package_id": package_id.to_string(),
        "goal_run_id": goal_run_id.to_string(),
        "state": "pr_ready",
        "message": "PR package built. Awaiting human review via `ta pr view` / `ta pr approve`.",
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_pr_status(
    state: &Arc<Mutex<GatewayState>>,
    params: GoalIdParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;

    let goal = state
        .goal_store
        .get(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
        })?;

    let pr_status = if let Some(pkg_id) = goal.pr_package_id {
        if let Some(pkg) = state.pr_packages.get(&pkg_id) {
            serde_json::json!({
                "pr_package_id": pkg_id.to_string(),
                "status": format!("{:?}", pkg.status),
                "artifacts": pkg.changes.artifacts.len(),
            })
        } else {
            serde_json::json!({
                "pr_package_id": pkg_id.to_string(),
                "status": "unknown",
            })
        }
    } else {
        serde_json::json!({
            "status": "no_pr_package",
            "message": "No PR package has been built yet. Use ta_pr_build first.",
        })
    };

    let response = serde_json::json!({
        "goal_run_id": goal_run_id.to_string(),
        "goal_state": goal.state.to_string(),
        "pr": pr_status,
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_draft(
    state: &Arc<Mutex<GatewayState>>,
    params: DraftToolParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    match params.action.as_str() {
        "build" => handle_draft_build(&mut state, &params),
        "submit" => handle_draft_submit(&mut state, &params),
        "status" => handle_draft_status(&state, &params),
        "list" => handle_draft_list(&state),
        _ => Err(McpError::invalid_params(
            format!(
                "unknown action '{}'. Expected: build, submit, status, list",
                params.action
            ),
            None,
        )),
    }
}

fn handle_draft_build(
    state: &mut GatewayState,
    params: &DraftToolParams,
) -> Result<CallToolResult, McpError> {
    let goal_run_id = parse_uuid(
        params
            .goal_run_id
            .as_deref()
            .ok_or_else(|| McpError::invalid_params("goal_run_id required for build", None))?,
    )?;
    let summary = params
        .summary
        .as_deref()
        .unwrap_or("Changes from macro goal sub-task");

    let goal = state
        .goal_store
        .get(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
        })?;

    let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let summary_why = if goal.objective.is_empty() {
        goal.title.clone()
    } else {
        goal.objective.clone()
    };
    let pr_package = connector
        .build_pr_package(&goal.title, &goal.objective, summary, &summary_why)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let package_id = pr_package.package_id;
    state
        .save_pr_package(pr_package)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({
        "draft_id": package_id.to_string(),
        "goal_run_id": goal_run_id.to_string(),
        "status": "built",
        "message": "Draft built. Call ta_draft with action: 'submit' to send for review.",
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

fn handle_draft_submit(
    state: &mut GatewayState,
    params: &DraftToolParams,
) -> Result<CallToolResult, McpError> {
    let goal_run_id = parse_uuid(
        params
            .goal_run_id
            .as_deref()
            .ok_or_else(|| McpError::invalid_params("goal_run_id required for submit", None))?,
    )?;

    let mut goal = state
        .goal_store
        .get(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .ok_or_else(|| {
            McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
        })?;

    let goal_id_str = goal_run_id.to_string();
    let package_id = goal.pr_package_id.or_else(|| {
        state
            .pr_packages
            .values()
            .filter(|p| p.goal.goal_id == goal_id_str)
            .max_by_key(|p| p.created_at)
            .map(|p| p.package_id)
    });

    match package_id {
        Some(pkg_id) => {
            if goal.state == GoalRunState::Running {
                goal.pr_package_id = Some(pkg_id);
                goal.transition(GoalRunState::PrReady)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
                state
                    .goal_store
                    .save(&goal)
                    .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            }

            state.event_dispatcher.dispatch(&TaEvent::PrReady {
                goal_run_id,
                pr_package_id: pkg_id,
                summary: "Macro goal draft submitted for review".to_string(),
                timestamp: Utc::now(),
            });

            let artifact_count = state
                .pr_packages
                .get(&pkg_id)
                .map(|p| p.changes.artifacts.len())
                .unwrap_or(0);
            let interaction_req =
                InteractionRequest::draft_review(pkg_id, &goal.title, artifact_count)
                    .with_goal_id(goal_run_id);

            let review_result = state.request_review(&interaction_req);

            let (review_status, review_decision) = match &review_result {
                Ok(resp) => {
                    let decision_str = format!("{}", resp.decision);
                    if decision_str == "approved" {
                        state.event_dispatcher.dispatch(&TaEvent::PrApproved {
                            goal_run_id,
                            pr_package_id: pkg_id,
                            approved_by: "human".to_string(),
                            timestamp: Utc::now(),
                        });

                        if goal.is_macro {
                            if let Ok(Some(mut g)) = state.goal_store.get(goal_run_id) {
                                if g.state == GoalRunState::PrReady {
                                    let _ = g.transition(GoalRunState::Running);
                                    let _ = state.goal_store.save(&g);
                                }
                            }
                        }
                    } else {
                        state.event_dispatcher.dispatch(&TaEvent::PrDenied {
                            goal_run_id,
                            pr_package_id: pkg_id,
                            reason: resp
                                .reasoning
                                .clone()
                                .unwrap_or_else(|| "denied".to_string()),
                            denied_by: "human".to_string(),
                            timestamp: Utc::now(),
                        });

                        let reject_event = DraftRejectEvent {
                            goal_id: goal_run_id,
                            draft_id: pkg_id,
                            agent_framework: goal.agent_id.clone(),
                            attempted: goal.title.clone(),
                            rejection_reason: resp
                                .reasoning
                                .clone()
                                .unwrap_or_else(|| "denied".to_string()),
                            phase_id: goal.plan_phase.clone(),
                        };
                        let capture = AutoCapture::new(state.auto_capture_config.clone());
                        let _ = capture.on_draft_reject(&mut state.memory_store, &reject_event);
                    }
                    ("reviewed".to_string(), decision_str)
                }
                Err(_) => ("submitted".to_string(), "pending".to_string()),
            };

            let response = serde_json::json!({
                "draft_id": pkg_id.to_string(),
                "goal_run_id": goal_run_id.to_string(),
                "status": review_status,
                "decision": review_decision,
                "message": if review_decision == "pending" {
                    "Draft submitted for human review. Use ta_draft with action: 'status' to check review progress."
                } else {
                    "Draft reviewed through ReviewChannel."
                },
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        None => {
            let response = serde_json::json!({
                "error": "no_draft",
                "message": "No draft package found. Call ta_draft with action: 'build' first.",
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
    }
}

fn handle_draft_status(
    state: &GatewayState,
    params: &DraftToolParams,
) -> Result<CallToolResult, McpError> {
    let draft_id = parse_uuid(
        params
            .draft_id
            .as_deref()
            .or(params.goal_run_id.as_deref())
            .ok_or_else(|| {
                McpError::invalid_params("draft_id or goal_run_id required for status", None)
            })?,
    )?;

    if let Some(pkg) = state.pr_packages.get(&draft_id) {
        let response = serde_json::json!({
            "draft_id": draft_id.to_string(),
            "status": format!("{:?}", pkg.status),
            "artifacts": pkg.changes.artifacts.len(),
        });
        Ok(CallToolResult::success(vec![Content::json(response)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    } else if let Ok(Some(goal)) = state.goal_store.get(draft_id) {
        let pr_status = if let Some(pkg_id) = goal.pr_package_id {
            if let Some(pkg) = state.pr_packages.get(&pkg_id) {
                serde_json::json!({
                    "draft_id": pkg_id.to_string(),
                    "status": format!("{:?}", pkg.status),
                    "artifacts": pkg.changes.artifacts.len(),
                })
            } else {
                serde_json::json!({ "status": "unknown" })
            }
        } else {
            serde_json::json!({
                "status": "no_draft",
                "message": "No draft built yet.",
            })
        };
        Ok(CallToolResult::success(vec![Content::json(pr_status)
            .map_err(|e| {
                McpError::internal_error(e.to_string(), None)
            })?]))
    } else {
        Err(McpError::invalid_params(
            format!("not found: {}", draft_id),
            None,
        ))
    }
}

fn handle_draft_list(state: &GatewayState) -> Result<CallToolResult, McpError> {
    let packages: Vec<serde_json::Value> = state
        .pr_packages
        .values()
        .map(|pkg| {
            serde_json::json!({
                "draft_id": pkg.package_id.to_string(),
                "status": format!("{:?}", pkg.status),
                "artifacts": pkg.changes.artifacts.len(),
                "goal_id": &pkg.goal.goal_id,
            })
        })
        .collect();

    let response = serde_json::json!({ "drafts": packages, "count": packages.len() });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}
