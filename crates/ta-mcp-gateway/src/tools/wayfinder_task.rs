//! `ta_propose_task_update`: lets a virtual-team worker propose a
//! title/description revision to the Wayfinder task it's working on,
//! bundled into the SAME draft a human reviews -- not a separate automated
//! outcome report fired after the fact. Per the CoS read-only chat-mode
//! design's corrected item 4 shape (TrustedAutonomy
//! docs/superpowers/specs/2026-10-06-cos-read-only-chat-mode-design.md,
//! 2026-10-06 refinement via trustedautonomy-46): the proposal is captured
//! as a `PendingAction` in the goal's draft package (the same mechanism
//! `ta_external_action`'s `policy = "review"` branch uses -- see
//! `tools/action.rs`), surfaced in `ta draft view` alongside the rest of
//! the draft, and replayed against the real Wayfinder API only when the
//! draft containing it is approved and applied -- see
//! `apps/ta-cli/src/commands/draft.rs`'s `apply_package`.
//!
//! Deliberately simpler than `ta_external_action`: no action-type
//! registry, no policy table, no connector/budget plumbing. A task-update
//! proposal always goes through review -- there is no `auto`/`block`
//! mode -- since the whole point is that a reviewer sees the proposed
//! change alongside the work it describes, not that it ever bypasses that
//! review.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use rmcp::model::{CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use ta_changeset::draft_package::{ActionKind, ArtifactDisposition, PendingAction};

use crate::server::GatewayState;

/// The MCP tool name this module registers -- also the `tool_name` every
/// `PendingAction` it produces carries, which `apply_package`'s replay
/// step matches on exactly to find these (and only these) pending actions
/// to replay.
pub const TOOL_NAME: &str = "ta_propose_task_update";

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposeTaskUpdateParams {
    /// The real Wayfinder task id (not an `external_id`) this update
    /// applies to -- the same task this goal was dispatched to work on.
    pub task_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// The UUID of the goal run this proposal is part of. Required (unlike
    /// `ta_external_action`'s optional `goal_run_id`, which silently drops
    /// the action when absent): a proposal with no goal to attach to has
    /// nowhere to surface for review and would be silently lost.
    pub goal_run_id: String,
}

pub fn handle_propose_task_update(
    state: &Arc<Mutex<GatewayState>>,
    params: ProposeTaskUpdateParams,
) -> Result<CallToolResult, McpError> {
    if params.title.is_none() && params.description.is_none() {
        return Err(McpError::invalid_params(
            "ta_propose_task_update: at least one of title/description must be set -- an \
             empty proposal has nothing to review or apply"
                .to_string(),
            None,
        ));
    }
    let goal_id = Uuid::parse_str(&params.goal_run_id).map_err(|e| {
        McpError::invalid_params(
            format!("ta_propose_task_update: goal_run_id is not a valid UUID: {e}"),
            None,
        )
    })?;

    let description = describe(&params);
    let pending = PendingAction {
        action_id: Uuid::new_v4(),
        tool_name: TOOL_NAME.to_string(),
        parameters: serde_json::json!({
            "task_id": params.task_id,
            "title": params.title,
            "description": params.description,
        }),
        kind: ActionKind::StateChanging,
        intercepted_at: Utc::now(),
        description,
        target_uri: Some(format!("wayfinder://tasks/{}", params.task_id)),
        disposition: ArtifactDisposition::Pending,
    };

    {
        let mut state = state
            .lock()
            .map_err(|e| McpError::internal_error(format!("lock poisoned: {e}"), None))?;
        state
            .pending_actions
            .entry(goal_id)
            .or_default()
            .push(pending);
    }

    Ok(CallToolResult::success(vec![Content::json(
        serde_json::json!({
            "status": "captured_for_review",
            "task_id": params.task_id,
        }),
    )
    .map_err(|e| McpError::internal_error(e.to_string(), None))?]))
}

/// Mirrors `interceptor.rs`'s `generate_description` bar -- a one-line
/// human-readable summary, not a structured card (per the existing
/// precedent in `draft.rs`'s rendering: only `ta_external_action:email`
/// gets a bespoke card, every other action type gets this generic
/// treatment, and the raw `parameters` are still shown at non-Top detail).
fn describe(params: &ProposeTaskUpdateParams) -> String {
    match (&params.title, &params.description) {
        (Some(t), Some(_)) => format!(
            "Propose updating task {}: title -> \"{}\", description updated",
            params.task_id, t
        ),
        (Some(t), None) => format!(
            "Propose updating task {}: title -> \"{}\"",
            params.task_id, t
        ),
        (None, Some(_)) => format!(
            "Propose updating task {}: description updated",
            params.task_id
        ),
        (None, None) => unreachable!("validated by the caller before describe() is reached"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GatewayConfig;

    fn make_state(root: &std::path::Path) -> Arc<Mutex<GatewayState>> {
        let mut config = GatewayConfig::for_project(root);
        config.credential_vault_use_keychain = false;
        let state = GatewayState::new(config).expect("state init failed");
        Arc::new(Mutex::new(state))
    }

    #[test]
    fn captures_a_pending_action_keyed_on_the_goal() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal_id = Uuid::new_v4();

        let result = handle_propose_task_update(
            &state,
            ProposeTaskUpdateParams {
                task_id: "t1".into(),
                title: Some("Revised title".into()),
                description: None,
                goal_run_id: goal_id.to_string(),
            },
        )
        .unwrap();
        assert!(!result.is_error.unwrap_or(false));

        let state_guard = state.lock().unwrap();
        let pending = state_guard
            .pending_actions
            .get(&goal_id)
            .expect("pending action should be stored under this goal");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, TOOL_NAME);
        assert_eq!(pending[0].kind, ActionKind::StateChanging);
        assert_eq!(pending[0].disposition, ArtifactDisposition::Pending);
        assert_eq!(
            pending[0].target_uri.as_deref(),
            Some("wayfinder://tasks/t1")
        );
        assert_eq!(pending[0].parameters["task_id"], "t1");
        assert_eq!(pending[0].parameters["title"], "Revised title");
        assert!(pending[0].description.contains("Revised title"));
    }

    #[test]
    fn rejects_a_proposal_with_neither_title_nor_description() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());

        let err = handle_propose_task_update(
            &state,
            ProposeTaskUpdateParams {
                task_id: "t1".into(),
                title: None,
                description: None,
                goal_run_id: Uuid::new_v4().to_string(),
            },
        )
        .unwrap_err();
        assert!(err.message.contains("at least one of title/description"));
    }

    #[test]
    fn rejects_a_malformed_goal_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());

        let err = handle_propose_task_update(
            &state,
            ProposeTaskUpdateParams {
                task_id: "t1".into(),
                title: Some("New title".into()),
                description: None,
                goal_run_id: "not-a-uuid".into(),
            },
        )
        .unwrap_err();
        assert!(err.message.contains("not a valid UUID"));
    }

    #[test]
    fn two_proposals_for_different_goals_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let state = make_state(dir.path());
        let goal_a = Uuid::new_v4();
        let goal_b = Uuid::new_v4();

        handle_propose_task_update(
            &state,
            ProposeTaskUpdateParams {
                task_id: "t1".into(),
                title: Some("A's title".into()),
                description: None,
                goal_run_id: goal_a.to_string(),
            },
        )
        .unwrap();
        handle_propose_task_update(
            &state,
            ProposeTaskUpdateParams {
                task_id: "t2".into(),
                title: Some("B's title".into()),
                description: None,
                goal_run_id: goal_b.to_string(),
            },
        )
        .unwrap();

        let state_guard = state.lock().unwrap();
        assert_eq!(state_guard.pending_actions.get(&goal_a).unwrap().len(), 1);
        assert_eq!(state_guard.pending_actions.get(&goal_b).unwrap().len(), 1);
    }
}
