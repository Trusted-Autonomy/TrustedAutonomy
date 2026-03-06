// tools/goal.rs — Goal lifecycle MCP tool handlers.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;

use ta_goal::{GoalRunState, TaEvent};

use crate::server::{GatewayState, GoalListParams, GoalStartParams, GoalToolParams};
use crate::validation::{parse_uuid, validate_goal_exists};

pub fn handle_goal_start(
    state: &Arc<Mutex<GatewayState>>,
    params: GoalStartParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    // Re-entrancy guard: reject if MCP server is running inside a staging workspace.
    // This prevents agents spawned by ta_goal_start from creating nested goals,
    // which would fight over the same work or create infinite loops.
    // Uses config.is_staging (set via TA_IS_STAGING env var) rather than path
    // sniffing, so it works with VFS, remote workspaces, and non-standard layouts.
    if state.config.is_staging {
        return Err(McpError::invalid_params(
            format!(
                "ta_goal_start called from inside a staging workspace ({}). \
                 This is a re-entrant call — the implementation agent should not \
                 create new goals from within a goal's workspace. Use ta_goal_inner \
                 for sub-goals within a macro session instead.",
                state.config.workspace_root.display()
            ),
            None,
        ));
    }

    let goal_run = state
        .start_goal(&params.title, &params.objective, &params.agent_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let goal_id = goal_run.goal_run_id;

    // Set source_dir (defaults to workspace root) and plan_phase on the goal.
    if let Ok(Some(mut g)) = state.goal_store.get(goal_id) {
        g.source_dir = Some(
            params
                .source
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| state.config.workspace_root.clone()),
        );
        g.plan_phase = params.phase.clone();
        let _ = state.goal_store.save(&g);
    }

    // Emit GoalStarted event to FsEventStore (v0.9.4.1).
    {
        use ta_events::{EventEnvelope, EventStore, FsEventStore};
        let events_dir = state.config.workspace_root.join(".ta").join("events");
        let event_store = FsEventStore::new(&events_dir);
        let event = ta_events::SessionEvent::GoalStarted {
            goal_id,
            title: goal_run.title.clone(),
            agent_id: goal_run.agent_id.clone(),
            phase: params.phase.clone(),
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Failed to persist GoalStarted event: {}", e);
        }
    }

    // Launch the implementation agent as a background process.
    // Spawns `ta run --headless` for the full lifecycle: overlay copy,
    // CLAUDE.md injection, agent spawn, draft build on exit.
    let source_dir = params
        .source
        .clone()
        .unwrap_or_else(|| state.config.workspace_root.display().to_string());

    let launched = launch_goal_agent(
        &state,
        goal_id,
        &params.title,
        &params.objective,
        &params.agent_id,
        &source_dir,
        params.phase.as_deref(),
    );

    let response = serde_json::json!({
        "goal_run_id": goal_id.to_string(),
        "state": goal_run.state.to_string(),
        "title": goal_run.title,
        "agent_id": goal_run.agent_id,
        "manifest_id": goal_run.manifest_id.to_string(),
        "launched": launched,
        "phase": params.phase,
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_goal_status(
    state: &Arc<Mutex<GatewayState>>,
    goal_run_id_str: &str,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(goal_run_id_str)?;
    let goal = validate_goal_exists(&state.goal_store, goal_run_id)?;

    let response = serde_json::json!({
        "goal_run_id": goal.goal_run_id.to_string(),
        "title": goal.title,
        "objective": goal.objective,
        "state": goal.state.to_string(),
        "agent_id": goal.agent_id,
        "created_at": goal.created_at.to_rfc3339(),
        "updated_at": goal.updated_at.to_rfc3339(),
        "pr_package_id": goal.pr_package_id.map(|id| id.to_string()),
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_goal_list(
    state: &Arc<Mutex<GatewayState>>,
    params: GoalListParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goals = if let Some(ref state_filter) = params.state {
        state
            .goal_store
            .list_by_state(state_filter)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
    } else {
        state
            .goal_store
            .list()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
    };

    let items: Vec<serde_json::Value> = goals
        .iter()
        .map(|g| {
            serde_json::json!({
                "goal_run_id": g.goal_run_id.to_string(),
                "title": g.title,
                "state": g.state.to_string(),
                "agent_id": g.agent_id,
                "created_at": g.created_at.to_rfc3339(),
            })
        })
        .collect();

    let response = serde_json::json!({ "goals": items, "count": items.len() });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_goal_inner(
    state: &Arc<Mutex<GatewayState>>,
    params: GoalToolParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    match params.action.as_str() {
        "start" => {
            let macro_goal_id = parse_uuid(params.macro_goal_id.as_deref().ok_or_else(|| {
                McpError::invalid_params("macro_goal_id required for start", None)
            })?)?;
            let title = params
                .title
                .as_deref()
                .ok_or_else(|| McpError::invalid_params("title required for start", None))?;
            let objective = params.objective.as_deref().unwrap_or(title);

            // Verify macro goal exists and is a macro goal.
            let macro_goal = state
                .goal_store
                .get(macro_goal_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
                .ok_or_else(|| {
                    McpError::invalid_params(
                        format!("macro goal not found: {}", macro_goal_id),
                        None,
                    )
                })?;

            if !macro_goal.is_macro {
                return Err(McpError::invalid_params(
                    "goal is not a macro goal. Use ta run --macro to start a macro session.",
                    None,
                ));
            }

            // Create sub-goal inheriting from macro goal.
            let sub_goal_run = state
                .start_goal(title, objective, &macro_goal.agent_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            let sub_id = sub_goal_run.goal_run_id;

            // Set parent_macro_id on sub-goal and inherit plan phase.
            let mut updated_sub = state
                .goal_store
                .get(sub_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
                .ok_or_else(|| McpError::internal_error("sub-goal vanished", None))?;
            updated_sub.parent_macro_id = Some(macro_goal_id);
            updated_sub.plan_phase = params
                .phase
                .clone()
                .or_else(|| macro_goal.plan_phase.clone());
            updated_sub.source_dir = macro_goal.source_dir.clone();
            let agent_id = params
                .agent
                .clone()
                .unwrap_or_else(|| macro_goal.agent_id.clone());
            updated_sub.agent_id = agent_id.clone();
            state
                .goal_store
                .save(&updated_sub)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            // Add sub-goal ID to macro goal's list.
            let mut updated_macro = macro_goal.clone();
            updated_macro.sub_goal_ids.push(sub_id);
            state
                .goal_store
                .save(&updated_macro)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;

            // Optionally launch the implementation agent in the background (v0.8.2).
            let launched = if params.launch.unwrap_or(false) {
                launch_sub_goal_agent(
                    &state,
                    &macro_goal,
                    sub_id,
                    title,
                    objective,
                    &agent_id,
                    updated_sub.plan_phase.as_deref(),
                )
            } else {
                false
            };

            // Publish GoalStarted event to the file-based event store.
            {
                use ta_events::{EventStore, FsEventStore};
                let events_dir = state.config.workspace_root.join(".ta").join("events");
                let event_store = FsEventStore::new(&events_dir);
                let event = ta_events::SessionEvent::GoalStarted {
                    goal_id: sub_id,
                    title: title.to_string(),
                    agent_id: agent_id.clone(),
                    phase: updated_sub.plan_phase.clone(),
                };
                let envelope = ta_events::EventEnvelope::new(event);
                if let Err(e) = event_store.append(&envelope) {
                    tracing::warn!("Failed to persist GoalStarted event: {}", e);
                }
            }

            let response = serde_json::json!({
                "sub_goal_id": sub_id.to_string(),
                "macro_goal_id": macro_goal_id.to_string(),
                "title": title,
                "state": "running",
                "launched": launched,
                "agent": agent_id,
                "phase": updated_sub.plan_phase,
            });
            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        "status" => {
            let goal_run_id = parse_uuid(params.goal_run_id.as_deref().ok_or_else(|| {
                McpError::invalid_params("goal_run_id required for status", None)
            })?)?;

            let goal = state
                .goal_store
                .get(goal_run_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
                .ok_or_else(|| {
                    McpError::invalid_params(format!("goal not found: {}", goal_run_id), None)
                })?;

            let mut response = serde_json::json!({
                "goal_run_id": goal.goal_run_id.to_string(),
                "title": goal.title,
                "state": goal.state.to_string(),
                "is_macro": goal.is_macro,
            });

            // Include sub-goal tree if this is a macro goal.
            if goal.is_macro && !goal.sub_goal_ids.is_empty() {
                let sub_goals: Vec<serde_json::Value> = goal
                    .sub_goal_ids
                    .iter()
                    .filter_map(|id| state.goal_store.get(*id).ok().flatten())
                    .map(|sg| {
                        serde_json::json!({
                            "sub_goal_id": sg.goal_run_id.to_string(),
                            "title": sg.title,
                            "state": sg.state.to_string(),
                            "draft_id": sg.pr_package_id.map(|id| id.to_string()),
                        })
                    })
                    .collect();
                response["sub_goals"] = serde_json::json!(sub_goals);
            }

            Ok(CallToolResult::success(vec![Content::json(response)
                .map_err(|e| {
                    McpError::internal_error(e.to_string(), None)
                })?]))
        }
        _ => Err(McpError::invalid_params(
            format!(
                "unknown action '{}'. Expected: start, status",
                params.action
            ),
            None,
        )),
    }
}

/// Launch a goal's implementation agent as a background process (v0.9.4.1).
///
/// Used by `ta_goal_start` with `launch:true`. Spawns `ta run --headless`
/// which performs the full lifecycle: overlay copy, CLAUDE.md injection,
/// agent spawn, draft build on exit, and event emission.
///
/// Emits GoalFailed to FsEventStore if the spawn itself fails.
fn launch_goal_agent(
    state: &GatewayState,
    goal_id: uuid::Uuid,
    title: &str,
    objective: &str,
    agent_id: &str,
    source_dir: &str,
    phase: Option<&str>,
) -> bool {
    let mut cmd = std::process::Command::new("ta");
    cmd.arg("run")
        .arg(title)
        .arg("--source")
        .arg(source_dir)
        .arg("--agent")
        .arg(agent_id)
        .arg("--objective")
        .arg(objective)
        .arg("--headless")
        // v0.9.5.1: Pass the existing goal_run_id so `ta run` reuses it
        // instead of creating a duplicate goal record.
        .arg("--goal-id")
        .arg(goal_id.to_string());

    if let Some(phase) = phase {
        cmd.arg("--phase").arg(phase);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    match cmd.spawn() {
        Ok(_child) => {
            tracing::info!("Launched headless agent for goal {} ({})", goal_id, title);
            true
        }
        Err(e) => {
            tracing::warn!("Failed to launch agent for goal {}: {}", goal_id, e);
            state
                .event_dispatcher
                .dispatch(&TaEvent::goal_failed(goal_id, &e.to_string(), None));

            if let Ok(Some(mut g)) = state.goal_store.get(goal_id) {
                let _ = g.transition(GoalRunState::Failed {
                    reason: format!("agent launch failed: {}", e),
                });
                let _ = state.goal_store.save(&g);
            }

            // Persist GoalFailed to file-based event store.
            {
                use ta_events::{EventEnvelope, EventStore, FsEventStore};
                let events_dir = state.config.workspace_root.join(".ta").join("events");
                let event_store = FsEventStore::new(&events_dir);
                let event = ta_events::SessionEvent::GoalFailed {
                    goal_id,
                    error: e.to_string(),
                    exit_code: None,
                };
                let _ = event_store.append(&EventEnvelope::new(event));
            }
            false
        }
    }
}

/// Launch a sub-goal's implementation agent as a background process.
///
/// v0.9.4: Emits GoalFailed event if the launch itself fails. When launch:true
/// is set on ta_goal_inner, this performs the full `ta run --headless` lifecycle:
/// overlay workspace copy, context injection, and agent spawn.
fn launch_sub_goal_agent(
    state: &GatewayState,
    macro_goal: &ta_goal::GoalRun,
    sub_id: uuid::Uuid,
    title: &str,
    objective: &str,
    agent_id: &str,
    phase: Option<&str>,
) -> bool {
    let source_dir = match &macro_goal.source_dir {
        Some(s) => s.clone(),
        None => return false,
    };

    // Spawn `ta run --headless` as a background process.
    let mut cmd = std::process::Command::new("ta");
    cmd.arg("run")
        .arg(title)
        .arg("--source")
        .arg(&source_dir)
        .arg("--agent")
        .arg(agent_id)
        .arg("--objective")
        .arg(objective)
        .arg("--headless")
        // v0.9.5.1: Pass existing sub-goal ID to avoid duplicate creation.
        .arg("--goal-id")
        .arg(sub_id.to_string());

    if let Some(phase) = phase {
        cmd.arg("--phase").arg(phase);
    }

    // Detach: redirect output to null, don't inherit stdin.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    match cmd.spawn() {
        Ok(_child) => {
            tracing::info!(
                "Launched headless agent for sub-goal {} ({})",
                sub_id,
                title
            );
            true
        }
        Err(e) => {
            tracing::warn!("Failed to launch agent for sub-goal {}: {}", sub_id, e);
            // v0.9.4: Emit GoalFailed event on launch failure.
            state
                .event_dispatcher
                .dispatch(&TaEvent::goal_failed(sub_id, &e.to_string(), None));

            // Transition goal to Failed state.
            if let Ok(Some(mut g)) = state.goal_store.get(sub_id) {
                let _ = g.transition(GoalRunState::Failed {
                    reason: format!("agent launch failed: {}", e),
                });
                let _ = state.goal_store.save(&g);
            }

            // Persist GoalFailed to file-based event store.
            {
                use ta_events::{EventStore, FsEventStore};
                let events_dir = state.config.workspace_root.join(".ta").join("events");
                let event_store = FsEventStore::new(&events_dir);
                let event = ta_events::SessionEvent::GoalFailed {
                    goal_id: sub_id,
                    error: e.to_string(),
                    exit_code: None,
                };
                let envelope = ta_events::EventEnvelope::new(event);
                let _ = event_store.append(&envelope);
            }
            false
        }
    }
}
