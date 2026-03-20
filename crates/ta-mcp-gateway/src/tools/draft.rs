// tools/draft.rs — Draft/PR package MCP tool handlers.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::*;
use rmcp::ErrorData as McpError;

use ta_changeset::interaction::InteractionRequest;
use ta_goal::{GoalRunState, TaEvent};
use ta_memory::{AutoCapture, DraftRejectEvent};
use ta_policy::auto_approve::{self, DraftInfo};

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

    let mut pr_package = connector
        .build_pr_package(&goal.title, &goal.objective, &params.summary, &params.title)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    // Populate design alternatives if provided (v0.9.5).
    if let Some(alts) = &params.alternatives {
        pr_package.summary.alternatives_considered = alts
            .iter()
            .map(|a| ta_changeset::DesignAlternative {
                option: a.option.clone(),
                rationale: a.rationale.clone(),
                chosen: a.chosen,
            })
            .collect();
    }

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

    // Emit DraftBuilt event to FsEventStore (v0.9.4.1).
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore};
        let events_dir = state.config.workspace_root.join(".ta").join("events");
        let event_store = FsEventStore::new(&events_dir);
        let artifact_count = state
            .pr_packages
            .get(&package_id)
            .map(|p| p.changes.artifacts.len())
            .unwrap_or(0);
        let event = ta_events::SessionEvent::DraftBuilt {
            goal_id: goal_run_id,
            draft_id: package_id,
            artifact_count,
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Failed to persist DraftBuilt event: {}", e);
        }
    }

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

    // Bug C fix: when a plan phase is linked, use the phase's **Goal**: description
    // as `summary_why` rather than the goal's objective (which is often just a repeat
    // of the title). Fall back to objective when no phase is set or the description
    // cannot be extracted.
    let summary_why = {
        let phase_goal = goal.plan_phase.as_deref().and_then(|phase_id| {
            let plan_path = state.config.workspace_root.join("PLAN.md");
            extract_phase_goal_description(&plan_path, phase_id)
        });
        // Also detect placeholder: if objective equals the goal title exactly,
        // prefer the phase description when available.
        let obj_is_placeholder =
            goal.objective.is_empty() || goal.objective.trim() == goal.title.trim();
        match phase_goal {
            Some(desc)
                if !desc.is_empty() && (obj_is_placeholder || !goal.objective.is_empty()) =>
            {
                desc
            }
            _ if goal.objective.is_empty() => goal.title.clone(),
            _ => goal.objective.clone(),
        }
    };
    let pr_package = connector
        .build_pr_package(&goal.title, &goal.objective, summary, &summary_why)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let package_id = pr_package.package_id;
    state
        .save_pr_package(pr_package)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    // Emit DraftBuilt event to FsEventStore (v0.9.4.1).
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore};
        let events_dir = state.config.workspace_root.join(".ta").join("events");
        let event_store = FsEventStore::new(&events_dir);
        let artifact_count = state
            .pr_packages
            .get(&package_id)
            .map(|p| p.changes.artifacts.len())
            .unwrap_or(0);
        let event = ta_events::SessionEvent::DraftBuilt {
            goal_id: goal_run_id,
            draft_id: package_id,
            artifact_count,
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Failed to persist DraftBuilt event: {}", e);
        }
    }

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

            // v0.10.15: --require-review bypasses auto-approve entirely.
            let require_review = params.require_review.unwrap_or(false);

            // v0.9.8.1: Check auto-approval before routing to ReviewChannel.
            let mut auto_approve_decision = if require_review {
                tracing::info!(
                    draft_id = %pkg_id,
                    goal_id = %goal_run_id,
                    "auto-approve skipped: require_review flag set"
                );
                None
            } else {
                let policy_path = state.config.workspace_root.join(".ta/policy.yaml");
                if policy_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&policy_path) {
                        if let Ok(doc) = serde_yaml::from_str::<ta_policy::PolicyDocument>(&content)
                        {
                            let changed_paths: Vec<String> = state
                                .pr_packages
                                .get(&pkg_id)
                                .map(|p| {
                                    p.changes
                                        .artifacts
                                        .iter()
                                        .map(|a| a.resource_uri.clone())
                                        .collect()
                                })
                                .unwrap_or_default();
                            let draft_info = DraftInfo {
                                changed_paths,
                                lines_changed: 0, // approximate — not available from artifacts
                                plan_phase: goal.plan_phase.clone(),
                                agent_id: goal.agent_id.clone(),
                            };
                            Some(auto_approve::should_auto_approve_draft(&draft_info, &doc))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(auto_approve::AutoApproveDecision::Approved { ref mut reasons }) =
                auto_approve_decision
            {
                // v0.10.15 Item 4: Run verification commands before accepting auto-approve.
                let (verify_passed, auto_apply, git_commit) = {
                    let policy_path = state.config.workspace_root.join(".ta/policy.yaml");
                    if let Ok(content) = std::fs::read_to_string(&policy_path) {
                        if let Ok(doc) = serde_yaml::from_str::<ta_policy::PolicyDocument>(&content)
                        {
                            let cfg = &doc.defaults.auto_approve.drafts;
                            let conditions = &cfg.conditions;
                            let mut passed = true;

                            if conditions.require_tests_pass {
                                let staging_dir = goal.workspace_path.clone();
                                let cmd = &conditions.test_command;
                                let status = std::process::Command::new("sh")
                                    .args(["-c", cmd])
                                    .current_dir(&staging_dir)
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .status();
                                match status {
                                    Ok(s) if s.success() => {
                                        reasons
                                            .push(format!("require_tests_pass: '{}' passed", cmd));
                                    }
                                    _ => {
                                        tracing::warn!(
                                            draft_id = %pkg_id,
                                            command = cmd,
                                            "auto-approve verification failed: tests did not pass"
                                        );
                                        passed = false;
                                    }
                                }
                            }

                            if passed && conditions.require_clean_clippy {
                                let staging_dir = goal.workspace_path.clone();
                                let cmd = &conditions.lint_command;
                                let status = std::process::Command::new("sh")
                                    .args(["-c", cmd])
                                    .current_dir(&staging_dir)
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .status();
                                match status {
                                    Ok(s) if s.success() => {
                                        reasons.push(format!(
                                            "require_clean_clippy: '{}' passed",
                                            cmd
                                        ));
                                    }
                                    _ => {
                                        tracing::warn!(
                                            draft_id = %pkg_id,
                                            command = cmd,
                                            "auto-approve verification failed: clippy not clean"
                                        );
                                        passed = false;
                                    }
                                }
                            }

                            (passed, cfg.auto_apply, cfg.git_commit)
                        } else {
                            (true, false, false)
                        }
                    } else {
                        (true, false, false)
                    }
                };

                if !verify_passed {
                    // Verification failed — fall through to human review.
                    tracing::info!(
                        draft_id = %pkg_id,
                        "auto-approve denied: verification commands failed"
                    );
                    // Don't return — fall through to ReviewChannel below.
                } else {
                    // Auto-approved by policy — skip ReviewChannel.
                    state.event_dispatcher.dispatch(&TaEvent::PrApproved {
                        goal_run_id,
                        pr_package_id: pkg_id,
                        approved_by: "policy:auto".to_string(),
                        timestamp: Utc::now(),
                    });
                    state
                        .event_dispatcher
                        .dispatch(&TaEvent::draft_auto_approved(
                            &pkg_id.to_string(),
                            goal_run_id,
                            reasons.clone(),
                            auto_apply,
                        ));

                    // v0.10.15 Item 8: Write audit trail entry for auto-approved draft.
                    {
                        let agent_id = state.resolve_agent_id();
                        let reasoning = ta_audit::DecisionReasoning {
                            alternatives: vec![ta_audit::Alternative {
                                description: "Route to human review".to_string(),
                                score: None,
                                rejected_reason: "All auto-approve conditions met".to_string(),
                            }],
                            rationale: format!("Auto-approved: {}", reasons.join("; ")),
                            applied_principles: vec!["auto-approve-policy".to_string()],
                        };
                        let mut audit_event = ta_audit::AuditEvent::new(
                            &agent_id,
                            ta_audit::AuditAction::AutoApproval,
                        )
                        .with_target(format!("draft://{}", pkg_id))
                        .with_caller_mode(state.caller_mode.as_str())
                        .with_goal_run_id(goal_run_id)
                        .with_reasoning(reasoning)
                        .with_metadata(serde_json::json!({
                            "draft_id": pkg_id.to_string(),
                            "reasons": reasons,
                            "auto_apply": auto_apply,
                        }));
                        if let Err(e) = state.audit_log.append(&mut audit_event) {
                            tracing::warn!("failed to write auto-approval audit entry: {}", e);
                        }
                    }

                    tracing::info!(
                        draft_id = %pkg_id,
                        goal_id = %goal_run_id,
                        "Draft auto-approved by policy: {:?}",
                        reasons,
                    );

                    if goal.is_macro {
                        if let Ok(Some(mut g)) = state.goal_store.get(goal_run_id) {
                            if g.state == GoalRunState::PrReady {
                                let _ = g.transition(GoalRunState::Running);
                                let _ = state.goal_store.save(&g);
                            }
                        }
                    }

                    // v0.10.15 Item 5: Auto-apply if configured.
                    // Copies staged files from the goal's staging directory to the source.
                    let mut auto_applied = false;
                    if auto_apply {
                        let target_dir = state.config.workspace_root.clone();
                        let staging_dir = goal.workspace_path.clone();
                        if let Some(pkg) = state.pr_packages.get(&pkg_id) {
                            let mut applied_count = 0;
                            for artifact in &pkg.changes.artifacts {
                                let bare_path = artifact
                                    .resource_uri
                                    .strip_prefix("fs://workspace/")
                                    .unwrap_or(&artifact.resource_uri);
                                let staged_path = staging_dir.join(bare_path);
                                let target_path = target_dir.join(bare_path);
                                if staged_path.exists() {
                                    if let Some(parent) = target_path.parent() {
                                        let _ = std::fs::create_dir_all(parent);
                                    }
                                    if std::fs::copy(&staged_path, &target_path).is_ok() {
                                        applied_count += 1;
                                    }
                                }
                            }
                            tracing::info!(
                                draft_id = %pkg_id,
                                files = applied_count,
                                git_commit = git_commit,
                                "auto-apply: applied {} files to {}",
                                applied_count,
                                target_dir.display()
                            );
                            auto_applied = applied_count > 0;
                        }
                    }

                    let mut response = serde_json::json!({
                        "draft_id": pkg_id.to_string(),
                        "goal_run_id": goal_run_id.to_string(),
                        "status": "auto_approved",
                        "decision": "approved",
                        "approved_by": "policy:auto",
                        "reasons": reasons,
                        "message": "Draft auto-approved by policy. All conditions met.",
                    });
                    if auto_applied {
                        response["auto_applied"] = serde_json::json!(true);
                    }
                    return Ok(CallToolResult::success(vec![Content::json(response)
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?]));
                }
            }

            // Route to ReviewChannel for human review.
            let interaction_req =
                InteractionRequest::draft_review(pkg_id, &goal.title, artifact_count)
                    .with_goal_id(goal_run_id);

            // If auto-approve was evaluated but denied, include blockers.
            if let Some(auto_approve::AutoApproveDecision::Denied { ref blockers }) =
                auto_approve_decision
            {
                tracing::debug!(
                    draft_id = %pkg_id,
                    "Draft auto-approval denied, routing to human review: {:?}",
                    blockers,
                );
            }

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
    // v0.9.5.1: Merge in-memory packages with on-disk packages.
    // Drafts built by a different process (e.g., `ta run --headless` subprocess)
    // only exist on disk — the orchestrator's in-memory map won't have them.
    let mut seen_ids = std::collections::HashSet::new();
    let mut packages: Vec<serde_json::Value> = state
        .pr_packages
        .values()
        .map(|pkg| {
            seen_ids.insert(pkg.package_id);
            serde_json::json!({
                "draft_id": pkg.package_id.to_string(),
                "status": format!("{:?}", pkg.status),
                "artifacts": pkg.changes.artifacts.len(),
                "goal_id": &pkg.goal.goal_id,
            })
        })
        .collect();

    // Scan disk for packages not already in memory.
    let disk_dir = state.config.pr_packages_dir.clone();
    if disk_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&disk_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    if let Ok(json) = std::fs::read_to_string(&path) {
                        if let Ok(pkg) =
                            serde_json::from_str::<ta_changeset::draft_package::DraftPackage>(&json)
                        {
                            if !seen_ids.contains(&pkg.package_id) {
                                packages.push(serde_json::json!({
                                    "draft_id": pkg.package_id.to_string(),
                                    "status": format!("{:?}", pkg.status),
                                    "artifacts": pkg.changes.artifacts.len(),
                                    "goal_id": &pkg.goal.goal_id,
                                    "source": "disk",
                                }));
                            }
                        }
                    }
                }
            }
        }
    }

    let response = serde_json::json!({ "drafts": packages, "count": packages.len() });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

// ── Bug C fix helper ──────────────────────────────────────────────────────────

/// Extract the `**Goal**:` description for a given plan phase ID from PLAN.md.
///
/// Searches for the phase section header (e.g. `### v0.13.1.2 —`), then looks
/// for the first `**Goal**:` line within that section. Returns `None` when the
/// file cannot be read, the phase is not found, or no `**Goal**:` line exists.
fn extract_phase_goal_description(plan_path: &std::path::Path, phase_id: &str) -> Option<String> {
    let content = std::fs::read_to_string(plan_path).ok()?;

    // Find the line that introduces the section for this phase.
    // Phase headers look like: `### v0.13.1.2 — Release Completeness...`
    let phase_header_marker = format!("### {}", phase_id);
    let phase_start = content
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(&phase_header_marker))
        .map(|(i, _)| i)?;

    // Scan forward from the phase header until we hit the next `###` section
    // or end of file, looking for a `**Goal**:` line.
    for line in content.lines().skip(phase_start + 1) {
        // Stop at the next phase header.
        if line.starts_with("### ") {
            break;
        }
        // Match `**Goal**: <description>` (with or without leading whitespace).
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("**Goal**:") {
            let desc = rest.trim().to_string();
            if !desc.is_empty() {
                return Some(desc);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_phase_goal_description_finds_goal_line() {
        let plan = "\
### v0.13.1.2 — Release Completeness & Cross-Platform Launch Fix
<!-- status: pending -->
**Goal**: Fix two classes of critical bugs: missing ta-daemon and silent PR failure.

#### Items
1. [ ] Item one
";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PLAN.md");
        std::fs::write(&path, plan).unwrap();
        let result = extract_phase_goal_description(&path, "v0.13.1.2");
        assert_eq!(
            result,
            Some(
                "Fix two classes of critical bugs: missing ta-daemon and silent PR failure."
                    .to_string()
            )
        );
    }

    #[test]
    fn extract_phase_goal_description_stops_at_next_section() {
        let plan = "\
### v0.13.1.2 — Phase A
<!-- status: pending -->

### v0.13.2 — Phase B
**Goal**: This is phase B's goal.
";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PLAN.md");
        std::fs::write(&path, plan).unwrap();
        // Phase A has no **Goal** line — should return None.
        let result = extract_phase_goal_description(&path, "v0.13.1.2");
        assert_eq!(result, None);
    }

    #[test]
    fn extract_phase_goal_description_returns_none_for_missing_phase() {
        let plan = "### v0.13.2 — Some Phase\n**Goal**: Something.\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PLAN.md");
        std::fs::write(&path, plan).unwrap();
        let result = extract_phase_goal_description(&path, "v0.99.0");
        assert_eq!(result, None);
    }
}
