// chat_mode.rs: the chat-mode tool profile shared by `ta run`'s launch
// path (apps/ta-cli) and the agent-facing MCP gateway (ta-mcp-gateway).
//
// A chat-mode launch (persona `chat_mode = true`, or `ta run --chat-mode`)
// is a read-only session: the agent's policy identity and capability
// manifest come from `ta_policy::compile_chat_manifest()` (read anywhere in
// the workspace, write only to `.ta/chat-scratch/`, no git/email/external
// grants), and its tool surface is limited to the TA MCP tools listed in
// `CHAT_MODE_MCP_TOOLS` below. There is no native-tool path around the
// manifest: every native Claude Code tool (Bash, Read, Write, Edit, ...) is
// denied outright.
//
// This module is deliberately pure data plus pure functions, so the same
// profile is enforced in two independent places from one definition:
//
// 1. Native launch gating (`ta run` writes `.claude/settings.local.json`):
//    the allow-list is `persona.allowed_tools` INTERSECTED with
//    `chat_mode_allowed_tool_patterns()`, never the union, and every tool
//    in `CHAT_MODE_NATIVE_DENY` is added to the deny list.
// 2. MCP-side gating (`ta serve` started in chat mode): every MCP tool not
//    in `CHAT_MODE_MCP_TOOLS` is removed from the server's tool router, so
//    it is neither listed nor callable, regardless of what the agent
//    harness's own settings would have allowed.
//
// Design: docs/superpowers/specs/2026-10-05-chat-mode-secure-launch-design.md,
// docs/superpowers/specs/2026-10-06-cos-read-only-chat-mode-design.md.

/// Reserved substring marking a chat-session-derived policy identity
/// (`<agent_id>:chat:<session_id>`). A caller-supplied agent id must never
/// contain it (security hypothesis H6): otherwise a caller could pick an id
/// that collides with a live chat session's derived identity and overwrite
/// its narrow manifest.
pub const CHAT_POLICY_ID_MARKER: &str = ":chat:";

/// Claude Code permission-pattern prefix for tools served by the `ta` MCP
/// server (the server name TA registers in the agent's MCP config).
pub const TA_MCP_TOOL_PREFIX: &str = "mcp__ta__";

/// The complete set of TA MCP tools a chat-mode session may hold.
///
/// Everything here is either read-only, writes only through the compiled
/// chat manifest (`ta_fs_write`, which `PolicyEngine` limits to
/// `.ta/chat-scratch/**`), or is team-session coordination delivery that
/// the chat-mode spec explicitly allows (whiteboard presence/handoff).
/// Nothing here creates goals, builds or applies drafts, edits the plan,
/// edits the wiki, mutates tasks, or performs external actions.
pub const CHAT_MODE_MCP_TOOLS: &[&str] = &[
    // Workspace reads, policy-checked per call against the chat manifest.
    "ta_fs_read",
    "ta_fs_list",
    "ta_fs_diff",
    // Scratch-only writes: the chat manifest grants fs_write_patch for
    // `.ta/chat-scratch/**` and nothing else.
    "ta_fs_write",
    // Read-only status queries.
    "ta_goal_status",
    "ta_goal_list",
    "ta_plan_status",
    "ta_pr_status",
    "ta_agent_status",
    // Read-only knowledge lookups.
    "ta_wiki_search",
    "ta_wiki_get",
    "ta_wiki_types",
    "community_search",
    "community_get",
    // Team-session coordination delivery (allowed by the chat-mode spec).
    "ta_whiteboard_presence_register",
    "ta_whiteboard_presence_list",
    "ta_whiteboard_handoff_send",
    "ta_whiteboard_handoff_receive",
    // The Chief-of-Staff's ONLY outbound channel (ratified CoS design): it
    // appends an opaque `reply`/`delegate` outcome to the report-back
    // stream. It cannot touch files, tasks, wiki, goals or drafts itself;
    // a deterministic poller authenticates and validates every outcome
    // (bound to in-flight candidates, capped, tag-checked, launched with
    // `--origin cos` and no auto-approve) before anything happens. Without
    // it the CoS could neither reply nor delegate.
    "ta_whiteboard_outcome_send",
    // A question to a human (blocking, no mutation). NOT `ta_human_verify`,
    // which spawns synthetic headless agents. NOT `ta_context`, whose
    // `store`/`forget` actions write cross-agent persistent memory (a
    // prompt-injection persistence channel into later workers).
    "ta_ask_human",
];

/// Native Claude Code tools denied outright in a chat-mode launch. Bare
/// tool names deny every use of that tool regardless of arguments. These
/// are the unmediated paths around the capability manifest (shell, native
/// file read/write, web fetch, sub-agents), so none may remain reachable.
pub const CHAT_MODE_NATIVE_DENY: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "LS",
    "WebFetch",
    "WebSearch",
    "Task",
    "Agent",
    "Skill",
];

/// True when `tool_name` (a bare MCP tool name such as `"ta_fs_read"`) is
/// part of the chat-mode profile.
pub fn is_chat_mode_mcp_tool(tool_name: &str) -> bool {
    CHAT_MODE_MCP_TOOLS.contains(&tool_name)
}

/// The chat-mode profile as Claude Code permission patterns
/// (`"mcp__ta__ta_fs_read"`, ...), in `CHAT_MODE_MCP_TOOLS` order.
pub fn chat_mode_allowed_tool_patterns() -> Vec<String> {
    CHAT_MODE_MCP_TOOLS
        .iter()
        .map(|t| format!("{}{}", TA_MCP_TOOL_PREFIX, t))
        .collect()
}

/// Result of intersecting a persona's declared `allowed_tools` with the
/// chat-mode profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatToolSurface {
    /// The effective allow-list (Claude Code permission patterns). Always a
    /// subset of `chat_mode_allowed_tool_patterns()`.
    pub allowed: Vec<String>,
    /// Persona entries that matched nothing in the chat-mode profile and
    /// were removed entirely (e.g. `"Bash(*)"`, `"mcp__ta__ta_goal_start"`).
    pub stripped: Vec<String>,
    /// Persona wildcard entries (e.g. `"mcp__ta__*"`) that were narrowed to
    /// only the chat-mode tools they cover.
    pub narrowed: Vec<String>,
}

/// Does permission pattern `entry` cover the specific pattern `target`?
/// Exact match, or a trailing-`*` wildcard with no argument list
/// (`"mcp__ta__*"`, `"mcp__ta__ta_whiteboard_*"`, `"*"`) whose prefix
/// matches.
fn pattern_covers(entry: &str, target: &str) -> bool {
    if entry == target {
        return true;
    }
    match entry.strip_suffix('*') {
        Some(prefix) if !prefix.contains('(') => target.starts_with(prefix),
        _ => false,
    }
}

/// Intersect a persona's declared `allowed_tools` with the chat-mode
/// profile. Never the union: the result can only ever be narrower than
/// either input.
///
/// - Empty `persona_allowed` means the persona declared no restriction of
///   its own, so the effective surface is the full chat-mode profile.
/// - Otherwise each chat-mode tool is kept only if some persona entry
///   covers it. Persona entries that cover no chat-mode tool are reported in
///   `stripped`; wildcard entries that cover some chat-mode tools but are
///   broader than them are reported in `narrowed`. Callers must log both.
pub fn resolve_chat_mode_tool_surface(persona_allowed: &[String]) -> ChatToolSurface {
    let profile = chat_mode_allowed_tool_patterns();
    if persona_allowed.is_empty() {
        return ChatToolSurface {
            allowed: profile,
            stripped: Vec::new(),
            narrowed: Vec::new(),
        };
    }

    let allowed: Vec<String> = profile
        .iter()
        .filter(|p| persona_allowed.iter().any(|e| pattern_covers(e, p)))
        .cloned()
        .collect();

    let mut stripped = Vec::new();
    let mut narrowed = Vec::new();
    for entry in persona_allowed {
        if profile.iter().any(|p| p == entry) {
            continue;
        }
        if profile.iter().any(|p| pattern_covers(entry, p)) {
            narrowed.push(entry.clone());
        } else {
            stripped.push(entry.clone());
        }
    }

    ChatToolSurface {
        allowed,
        stripped,
        narrowed,
    }
}

/// Reject an agent id that contains the reserved chat-session marker (H6).
/// Returns an actionable error message on rejection.
pub fn validate_chat_agent_id(agent_id: &str) -> Result<(), String> {
    if agent_id.trim().is_empty() {
        return Err(
            "chat-mode launch needs a non-empty agent id (the persona or agent \
             name), but got an empty one. Pass --persona <name> or --agent <name>."
                .to_string(),
        );
    }
    if agent_id.contains(CHAT_POLICY_ID_MARKER) {
        return Err(format!(
            "agent id '{}' contains the reserved substring '{}', which is used only for \
             chat-session-derived policy identities; accepting it could let this launch \
             collide with a live chat session's manifest. Rename the persona/agent so its \
             name does not contain '{}'.",
            agent_id, CHAT_POLICY_ID_MARKER, CHAT_POLICY_ID_MARKER
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// The profile itself must never contain a tool that mutates anything
    /// outside chat scratch: goal/draft/plan/wiki-write/task/external
    /// tools are all absent.
    #[test]
    fn chat_profile_contains_no_mutating_tools() {
        for forbidden in [
            "ta_goal_start",
            "ta_goal_inner",
            "ta_pr_build",
            "ta_draft",
            "ta_plan",
            "ta_workflow",
            "ta_external_action",
            "ta_propose_task_update",
            "ta_wiki_create",
            "ta_wiki_update",
            "ta_whiteboard_task_claim",
            "ta_whiteboard_task_complete",
            "ta_context",
            "ta_human_verify",
            "community_annotate",
            "community_feedback",
            "community_suggest",
            "ue5_python_exec",
            "unity_build_trigger",
            "comfyui_workflow_submit",
        ] {
            assert!(
                !is_chat_mode_mcp_tool(forbidden),
                "{} must not be in the chat-mode profile",
                forbidden
            );
        }
        // And no native tool appears in the allow patterns at all.
        for p in chat_mode_allowed_tool_patterns() {
            assert!(p.starts_with(TA_MCP_TOOL_PREFIX), "non-MCP entry {}", p);
        }
    }

    #[test]
    fn empty_persona_list_yields_full_chat_profile() {
        let surface = resolve_chat_mode_tool_surface(&[]);
        assert_eq!(surface.allowed, chat_mode_allowed_tool_patterns());
        assert!(surface.stripped.is_empty());
        assert!(surface.narrowed.is_empty());
    }

    #[test]
    fn mutating_tools_in_persona_list_are_stripped() {
        let persona = s(&[
            "mcp__ta__ta_fs_read",
            "Bash(*)",
            "Write(*)",
            "mcp__ta__ta_goal_start",
            "mcp__ta__ta_wiki_update",
            "mcp__ta__ta_wiki_search",
        ]);
        let surface = resolve_chat_mode_tool_surface(&persona);
        assert_eq!(
            surface.allowed,
            s(&["mcp__ta__ta_fs_read", "mcp__ta__ta_wiki_search"])
        );
        assert_eq!(
            surface.stripped,
            s(&[
                "Bash(*)",
                "Write(*)",
                "mcp__ta__ta_goal_start",
                "mcp__ta__ta_wiki_update"
            ])
        );
        assert!(surface.narrowed.is_empty());
    }

    #[test]
    fn wildcard_is_narrowed_to_chat_profile_never_widened() {
        let persona = s(&["mcp__ta__*"]);
        let surface = resolve_chat_mode_tool_surface(&persona);
        assert_eq!(surface.allowed, chat_mode_allowed_tool_patterns());
        assert_eq!(surface.narrowed, s(&["mcp__ta__*"]));
        assert!(!surface
            .allowed
            .iter()
            .any(|t| t == "mcp__ta__ta_goal_start" || t == "mcp__ta__*"));

        let wb = resolve_chat_mode_tool_surface(&s(&["mcp__ta__ta_whiteboard_*"]));
        assert_eq!(
            wb.allowed,
            s(&[
                "mcp__ta__ta_whiteboard_presence_register",
                "mcp__ta__ta_whiteboard_presence_list",
                "mcp__ta__ta_whiteboard_handoff_send",
                "mcp__ta__ta_whiteboard_handoff_receive",
                "mcp__ta__ta_whiteboard_outcome_send",
            ])
        );
    }

    #[test]
    fn intersection_is_never_a_union() {
        // Every result entry must be in BOTH inputs' coverage.
        let persona = s(&["mcp__ta__ta_fs_read", "mcp__ta__ta_fs_write", "Read(*)"]);
        let surface = resolve_chat_mode_tool_surface(&persona);
        let profile = chat_mode_allowed_tool_patterns();
        for t in &surface.allowed {
            assert!(profile.contains(t));
            assert!(persona.iter().any(|e| pattern_covers(e, t)));
        }
        assert_eq!(
            surface.allowed,
            s(&["mcp__ta__ta_fs_read", "mcp__ta__ta_fs_write"])
        );
        assert_eq!(surface.stripped, s(&["Read(*)"]));
    }

    #[test]
    fn native_deny_list_covers_every_unmediated_path() {
        for t in ["Bash", "Read", "Write", "Edit", "MultiEdit", "NotebookEdit"] {
            assert!(CHAT_MODE_NATIVE_DENY.contains(&t), "{} must be denied", t);
        }
    }

    #[test]
    fn chat_marker_in_agent_id_is_rejected_with_actionable_error() {
        let err = validate_chat_agent_id("cos:chat:deadbeef").unwrap_err();
        assert!(err.contains(":chat:"));
        assert!(err.contains("Rename"));
        assert!(validate_chat_agent_id("").is_err());
        assert!(validate_chat_agent_id("chief-of-staff").is_ok());
    }
}
