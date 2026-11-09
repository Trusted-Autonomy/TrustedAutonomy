// tools/fs.rs — Filesystem MCP tool handlers.

use std::sync::{Arc, Mutex};

use rmcp::model::*;
use rmcp::ErrorData as McpError;

use crate::server::{FsDiffParams, FsListParams, FsReadParams, FsWriteParams, GatewayState};
use crate::validation::{enforce_policy, parse_uuid};

pub fn handle_fs_read(
    state: &Arc<Mutex<GatewayState>>,
    params: FsReadParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;
    state
        .check_chat_lock(goal_run_id)
        .map_err(|e| McpError::invalid_request(e.to_string(), None))?;
    let agent_id = state
        .agent_for_goal(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    // A chat session holding a read grant reads the REAL project root (or a
    // registry-listed project), read-only; it never reads a copy.
    if let Some(roots) = state.chat_read_roots.clone() {
        return read_chat_project_file(
            &mut state,
            &roots,
            goal_run_id,
            &agent_id,
            params.project.as_deref(),
            &params.path,
        );
    }
    if params.project.is_some() {
        return Err(McpError::invalid_params(
            "ta_fs_read: the `project` argument is only available to a chat-mode session that \
             holds a project read grant. Omit it to read this workspace."
                .to_string(),
            None,
        ));
    }

    let decision = state
        .check_policy(&agent_id, "read", &params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    enforce_policy(&decision)?;

    let workspace_root = state.config.workspace_root.clone();
    let connector = state.connectors.get_mut(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let content = connector
        .read_source(&workspace_root, &params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let text = String::from_utf8_lossy(&content).to_string();
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

/// Read one file of a project a chat read grant covers.
///
/// Order of refusals: the named project must be one the grant covers;
/// policy (the manifest grant for that project, then the hard secrets
/// backstop); path safety (lexical escapes and symlinks that resolve
/// outside the project root); infrastructure paths (`.ta`, `.git`, and
/// every other agent-infra or VCS-metadata directory, in any spelling and
/// also when reached through a symlink). Only then is the file read.
fn read_chat_project_file(
    state: &mut GatewayState,
    roots: &crate::server::ChatReadRoots,
    goal_run_id: uuid::Uuid,
    agent_id: &str,
    project: Option<&str>,
    path: &str,
) -> Result<CallToolResult, McpError> {
    let (label, root) = match project {
        None => (None, roots.project_root.clone()),
        Some(name) => {
            let found = roots
                .readable
                .iter()
                .find(|p| p.label == name)
                .ok_or_else(|| {
                    let listed: Vec<&str> =
                        roots.readable.iter().map(|p| p.label.as_str()).collect();
                    McpError::invalid_params(
                        format!(
                            "ta_fs_read: project '{name}' is not readable from this session. \
                         Readable projects: {}. The owner lists them under [chat] \
                         readable_projects in the project's .ta/workflow.toml; nothing outside \
                         them (and the session's own project) can be read.",
                            if listed.is_empty() {
                                "none".to_string()
                            } else {
                                listed.join(", ")
                            }
                        ),
                        None,
                    )
                })?;
            (Some(found.label.clone()), found.root.clone())
        }
    };

    let decision = state
        .check_project_read_policy(agent_id, label.as_deref(), path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    enforce_policy(&decision)?;

    let resolved = ta_workspace::path_safety::resolve_within_root(&root, path).map_err(|reason| {
        tracing::warn!(path, root = %root.display(), reason = %reason, "chat read refused: outside the project root");
        McpError::invalid_request(
            format!(
                "ta_fs_read: '{path}' is refused: {reason}. A chat session reads only files \
                 that really live inside the project root."
            ),
            None,
        )
    })?;
    // The string check above sees spellings; this one sees where a symlink
    // inside the root actually leads (`docs -> .git`).
    let infra_refusal = |what: &str| {
        tracing::warn!(
            path,
            what,
            "chat read refused: protected infrastructure path"
        );
        McpError::invalid_request(
            format!(
                "ta_fs_read: '{path}' is refused: it is inside TA or VCS infrastructure \
                 ({what}), which a chat session never reads."
            ),
            None,
        )
    };
    if ta_workspace::path_safety::is_infrastructure_path(path) {
        return Err(infra_refusal("by its path"));
    }
    if let (Ok(real), Ok(real_root)) = (resolved.canonicalize(), root.canonicalize()) {
        if let Ok(rel) = real.strip_prefix(&real_root) {
            if ta_workspace::path_safety::is_infrastructure_path(&rel.to_string_lossy()) {
                return Err(infra_refusal("through a symlink"));
            }
        }
    }

    let connector = state.connectors.get_mut(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;
    let content = connector
        .read_source(&root, path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(
        String::from_utf8_lossy(&content).to_string(),
    )]))
}

pub fn handle_fs_write(
    state: &Arc<Mutex<GatewayState>>,
    params: FsWriteParams,
) -> Result<CallToolResult, McpError> {
    let mut state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;

    // v0.9.3: Enforce caller mode — orchestrators cannot use ta_fs_write
    // unless the target path is a whitelisted release artifact (v0.10.6).
    if state.caller_mode.is_tool_forbidden("ta_fs_write")
        && !state.caller_mode.is_write_whitelisted(&params.path)
    {
        return Err(McpError::invalid_request(
            format!(
                "ta_fs_write is forbidden in orchestrator mode for '{}'. \
                 Only release artifacts (.release-draft.md, CHANGELOG.md, version.json, \
                 .press-release-draft.md) are whitelisted. Use ta_goal to launch an \
                 implementation agent for other writes.",
                params.path
            ),
            None,
        ));
    }

    let goal_run_id = parse_uuid(&params.goal_run_id)?;
    state
        .check_chat_lock(goal_run_id)
        .map_err(|e| McpError::invalid_request(e.to_string(), None))?;
    let agent_id = state
        .agent_for_goal(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let decision = state
        .check_policy(&agent_id, "write_patch", &params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    enforce_policy(&decision)?;

    let connector = state.connectors.get_mut(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let changeset = connector
        .write_patch(&params.path, params.content.as_bytes())
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({
        "changeset_id": changeset.changeset_id.to_string(),
        "target_uri": changeset.target_uri,
        "status": "staged",
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_fs_list(
    state: &Arc<Mutex<GatewayState>>,
    params: FsListParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;
    state
        .check_chat_lock(goal_run_id)
        .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

    let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let files = connector
        .list_staged()
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    let response = serde_json::json!({ "files": files, "count": files.len() });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
}

pub fn handle_fs_diff(
    state: &Arc<Mutex<GatewayState>>,
    params: FsDiffParams,
) -> Result<CallToolResult, McpError> {
    let state = state
        .lock()
        .map_err(|e| McpError::internal_error(format!("lock poisoned: {}", e), None))?;
    let goal_run_id = parse_uuid(&params.goal_run_id)?;
    state
        .check_chat_lock(goal_run_id)
        .map_err(|e| McpError::invalid_request(e.to_string(), None))?;

    // §7: policy check — diff exposes source file content, requires read permission.
    let agent_id = state
        .agent_for_goal(goal_run_id)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    let decision = state
        .check_policy(&agent_id, "read", &params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    enforce_policy(&decision)?;

    let connector = state.connectors.get(&goal_run_id).ok_or_else(|| {
        McpError::invalid_params(
            format!("no active connector for goal: {}", goal_run_id),
            None,
        )
    })?;

    let diff = connector
        .diff_file(&params.path)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

    match diff {
        Some(diff_text) => Ok(CallToolResult::success(vec![Content::text(diff_text)])),
        None => Ok(CallToolResult::success(vec![Content::text(
            "No changes (file is identical to source).",
        )])),
    }
}
