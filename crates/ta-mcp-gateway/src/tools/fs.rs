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

    // A chat session holding a read grant reads the REAL allowed resources
    // (project root and `[chat.read] allow` entries), read-only; it never reads
    // a copy.
    if let Some(chat) = state.chat_read.clone() {
        return read_chat_file(
            &state,
            &chat,
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

/// Largest file `ta_fs_read` returns whole in a chat session; a larger file is
/// returned truncated, with a notice saying so.
const CHAT_READ_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Most entries one `ta_fs_list` call returns.
const CHAT_LIST_MAX_ENTRIES: usize = 2000;

/// Resolve and authorize one path of a chat session, through the ONE compiled
/// read policy (rebuilt from the biscuit grant) and then the policy engine
/// (manifest grant plus the secrets backstop). Returns the canonical path.
///
/// Order of refusals: the named `project` must be readable; `..`; the built-in
/// hard deny (TA state, `.git`, credential stores, browser profiles); the deny
/// list; the allow list (symlinks are judged by where they lead); TA/VCS
/// infrastructure below the admitting root; the manifest grant and secrets
/// patterns. Every refusal says which config key decides.
fn authorize_chat_path(
    state: &GatewayState,
    chat: &crate::server::ChatReadState,
    tool: &str,
    agent_id: &str,
    project: Option<&str>,
    path: &str,
) -> Result<std::path::PathBuf, McpError> {
    use ta_policy::read_policy::Verdict;
    let base = chat
        .policy
        .root_of_label(project)
        .map_err(|why| McpError::invalid_params(format!("{tool}: {why}"), None))?;
    match chat
        .policy
        .decide_requested(&base, chat.home.as_deref(), path)
    {
        Verdict::Denied(refusal) => {
            tracing::warn!(
                tool,
                path,
                code = ?refusal.code,
                rule = ?refusal.rule,
                resolved = %refusal.resolved.display(),
                "chat read refused by the read policy"
            );
            Err(McpError::invalid_request(
                format!("{tool}: {}", refusal.message),
                None,
            ))
        }
        Verdict::Allowed { resolved, rule, .. } => {
            let decision = state.check_chat_read_policy(agent_id, &resolved);
            enforce_policy(&decision)?;
            tracing::debug!(tool, path, rule = %rule, resolved = %resolved.display(), "chat read allowed");
            Ok(resolved)
        }
    }
}

/// Read one file a chat read grant covers.
fn read_chat_file(
    state: &GatewayState,
    chat: &crate::server::ChatReadState,
    agent_id: &str,
    project: Option<&str>,
    path: &str,
) -> Result<CallToolResult, McpError> {
    use std::io::Read;
    let resolved = authorize_chat_path(state, chat, "ta_fs_read", agent_id, project, path)?;
    let meta = std::fs::metadata(&resolved).map_err(|e| {
        McpError::invalid_params(
            format!(
                "ta_fs_read: cannot read '{}': {e}. Check the path (ta_fs_list shows what a \
                 directory holds).",
                ta_policy::read_policy::display_path(&resolved)
            ),
            None,
        )
    })?;
    if !meta.is_file() {
        return Err(McpError::invalid_params(
            format!(
                "ta_fs_read: '{}' is a directory, not a file. Use ta_fs_list to see what it \
                 holds.",
                ta_policy::read_policy::display_path(&resolved)
            ),
            None,
        ));
    }
    let file = std::fs::File::open(&resolved).map_err(|e| {
        McpError::internal_error(
            format!(
                "ta_fs_read: could not open '{}': {e}",
                ta_policy::read_policy::display_path(&resolved)
            ),
            None,
        )
    })?;
    let mut bytes = Vec::new();
    file.take(CHAT_READ_MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            McpError::internal_error(
                format!(
                    "ta_fs_read: could not read '{}': {e}",
                    ta_policy::read_policy::display_path(&resolved)
                ),
                None,
            )
        })?;
    let mut text = String::from_utf8_lossy(&bytes).to_string();
    if meta.len() > CHAT_READ_MAX_BYTES {
        text.push_str(&format!(
            "\n[ta_fs_read: truncated at {CHAT_READ_MAX_BYTES} of {} bytes]",
            meta.len()
        ));
    }
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

/// List one directory a chat read grant covers: entries the policy refuses are
/// left out (and counted), not named.
fn list_chat_dir(
    state: &GatewayState,
    chat: &crate::server::ChatReadState,
    agent_id: &str,
    project: Option<&str>,
    path: &str,
) -> Result<CallToolResult, McpError> {
    use ta_policy::read_policy::Verdict;
    let dir = authorize_chat_path(state, chat, "ta_fs_list", agent_id, project, path)?;
    let shown = ta_policy::read_policy::display_path(&dir);
    let read = std::fs::read_dir(&dir).map_err(|e| {
        McpError::invalid_params(
            format!("ta_fs_list: cannot list '{shown}': {e}. It must be an existing directory."),
            None,
        )
    })?;
    let mut entries = Vec::new();
    let mut hidden = 0usize;
    let mut truncated = false;
    let mut names: Vec<std::fs::DirEntry> = read.flatten().collect();
    names.sort_by_key(|e| e.file_name());
    for entry in names {
        let child = dir.join(entry.file_name());
        // Judged by where the entry really leads (a symlink out of the allowed
        // area is hidden), exactly as a read of it would be.
        let allowed = matches!(chat.policy.decide(&child), Verdict::Allowed { .. })
            && matches!(
                state.check_chat_read_policy(agent_id, &child),
                ta_policy::PolicyDecision::Allow
            );
        if !allowed {
            hidden += 1;
            continue;
        }
        if entries.len() >= CHAT_LIST_MAX_ENTRIES {
            truncated = true;
            break;
        }
        let meta = std::fs::metadata(&child).ok();
        entries.push(serde_json::json!({
            "name": entry.file_name().to_string_lossy(),
            "kind": match &meta {
                Some(m) if m.is_dir() => "dir",
                Some(m) if m.is_file() => "file",
                _ => "other",
            },
            "size": meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
        }));
    }
    let response = serde_json::json!({
        "path": shown,
        "entries": entries,
        "count": entries.len(),
        "hidden_by_read_policy": hidden,
        "truncated": truncated,
    });
    Ok(CallToolResult::success(vec![Content::json(response)
        .map_err(|e| {
            McpError::internal_error(e.to_string(), None)
        })?]))
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
    // A chat-mode server never writes. The tool is not routed there at all and the
    // chat manifest carries no write grant; this refusal is the third layer, so a
    // router that is ever built wrongly still cannot stage a write.
    if state.chat_lock.is_some() {
        return Err(McpError::invalid_request(
            "ta_fs_write: refused. This TA MCP server runs in chat mode, which is read-only: \
             no tool writes anything. To change files, hand the work to a worker role \
             (ta_whiteboard_outcome_send with outcome \"delegate\")."
                .to_string(),
            None,
        ));
    }

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

    if let Some(chat) = state.chat_read.clone() {
        let agent_id = state
            .agent_for_goal(goal_run_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        return list_chat_dir(
            &state,
            &chat,
            &agent_id,
            params.project.as_deref(),
            params.path.as_deref().unwrap_or("."),
        );
    }
    if params.project.is_some() || params.path.is_some() {
        return Err(McpError::invalid_params(
            "ta_fs_list: `path` and `project` are only available to a chat-mode session that \
             holds a read grant. Omit them to list this goal's staged files."
                .to_string(),
            None,
        ));
    }

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
