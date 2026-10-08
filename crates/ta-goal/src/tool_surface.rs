// tool_surface.rs: Read-only vs mutating classification of every TA MCP
// tool, and the read-only (chat/Chief-of-Staff) tool surface (H7).
//
// Security hypothesis H7 (docs/superpowers/specs/security-hypotheses.md):
// the Chief-of-Staff (CoS) runs in read-only chat mode and must hold no tool
// that mutates anything. Several TA tools (wiki, whiteboard, `ta_propose_*`)
// are not gated by `ta_policy::PolicyEngine` at all, so the only real
// safeguard is tool availability: what the launched agent is allowed to call.
//
// This module is the single source of truth for that:
//
// - `MCP_TOOL_EFFECTS` classifies every tool the TA MCP gateway registers.
//   A test in `ta-mcp-gateway` enumerates the live tool registry and fails
//   if any registered tool is missing here, so a new tool cannot silently
//   enter a read-only surface unclassified.
// - "Mutating" is deliberately broad: anything with an effect beyond reading
//   (writes, staging, drafts, goal/workflow starts, outbound messages to a
//   human or external system, queue consumption, presence publication,
//   memory writes, remote jobs). A multi-action tool with any mutating
//   action is mutating as a whole.
// - `READ_ONLY_PERSONA_ALLOWED_TOOLS` is the default harness allow-list for a
//   persona that declares `read_only = true` (e.g. `chief-of-staff`), and
//   `validate_read_only_tool_surface` rejects any declared list that would
//   give such a persona a native tool, a wildcard, another MCP server, or a
//   mutating TA tool.

/// What calling a tool can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// Only reads state; no effect anywhere.
    ReadOnly,
    /// Has an effect beyond reading (see module docs for the definition).
    Mutating,
}

/// Every tool registered by the TA MCP gateway (`ta-mcp-gateway`'s
/// `TaGatewayServer` tool router), classified. Keep in sync with the
/// registry: `ta-mcp-gateway`'s `h7_every_registered_mcp_tool_is_classified`
/// test fails otherwise.
pub const MCP_TOOL_EFFECTS: &[(&str, ToolEffect)] = &[
    // Goals and orchestration.
    ("ta_goal_start", ToolEffect::Mutating),
    ("ta_goal_status", ToolEffect::ReadOnly),
    ("ta_goal_list", ToolEffect::ReadOnly),
    ("ta_goal_inner", ToolEffect::Mutating),
    ("ta_workflow", ToolEffect::Mutating),
    ("ta_agent_status", ToolEffect::ReadOnly),
    ("ta_event_subscribe", ToolEffect::ReadOnly),
    // Filesystem.
    ("ta_fs_read", ToolEffect::ReadOnly),
    ("ta_fs_write", ToolEffect::Mutating),
    ("ta_fs_list", ToolEffect::ReadOnly),
    ("ta_fs_diff", ToolEffect::ReadOnly),
    // Drafts / review.
    ("ta_pr_build", ToolEffect::Mutating),
    ("ta_pr_status", ToolEffect::ReadOnly),
    ("ta_draft", ToolEffect::Mutating),
    // Plan (ta_plan has an `update` action).
    ("ta_plan", ToolEffect::Mutating),
    ("ta_plan_status", ToolEffect::ReadOnly),
    // Memory store (store/recall).
    ("ta_context", ToolEffect::Mutating),
    // Human interaction: outbound message to a human channel.
    ("ta_ask_human", ToolEffect::ReadOnly),
    ("ta_human_verify", ToolEffect::Mutating),
    // Whiteboard. presence_register publishes, handoff_receive consumes a
    // pending handoff, the rest write.
    ("ta_whiteboard_presence_register", ToolEffect::Mutating),
    ("ta_whiteboard_presence_list", ToolEffect::ReadOnly),
    ("ta_whiteboard_handoff_send", ToolEffect::Mutating),
    ("ta_whiteboard_handoff_receive", ToolEffect::Mutating),
    ("ta_whiteboard_task_claim", ToolEffect::Mutating),
    ("ta_whiteboard_task_complete", ToolEffect::Mutating),
    ("ta_whiteboard_outcome_send", ToolEffect::ReadOnly),
    // Wiki (Wayfinder; not policy-gated on the TA side).
    ("ta_wiki_search", ToolEffect::ReadOnly),
    ("ta_wiki_get", ToolEffect::ReadOnly),
    ("ta_wiki_types", ToolEffect::ReadOnly),
    ("ta_wiki_create", ToolEffect::Mutating),
    ("ta_wiki_update", ToolEffect::Mutating),
    // External actions and outcome proposals.
    ("ta_external_action", ToolEffect::Mutating),
    ("ta_propose_task_update", ToolEffect::Mutating),
    // Unreal Engine.
    ("ue5_python_exec", ToolEffect::Mutating),
    ("ue5_scene_query", ToolEffect::ReadOnly),
    ("ue5_asset_list", ToolEffect::ReadOnly),
    ("ue5_mrq_submit", ToolEffect::Mutating),
    ("ue5_mrq_status", ToolEffect::ReadOnly),
    ("ue5_sequencer_query", ToolEffect::ReadOnly),
    ("ue5_lighting_preset_list", ToolEffect::ReadOnly),
    // ComfyUI.
    ("comfyui_workflow_submit", ToolEffect::Mutating),
    ("comfyui_job_status", ToolEffect::ReadOnly),
    ("comfyui_job_cancel", ToolEffect::Mutating),
    ("comfyui_model_list", ToolEffect::ReadOnly),
    // Community knowledge hub.
    ("community_search", ToolEffect::ReadOnly),
    ("community_get", ToolEffect::ReadOnly),
    ("community_annotate", ToolEffect::Mutating),
    ("community_feedback", ToolEffect::Mutating),
    ("community_suggest", ToolEffect::Mutating),
    // Unity.
    ("unity_build_trigger", ToolEffect::Mutating),
    ("unity_scene_query", ToolEffect::ReadOnly),
    ("unity_test_run", ToolEffect::Mutating),
    ("unity_addressables_build", ToolEffect::Mutating),
    ("unity_render_capture", ToolEffect::Mutating),
];

/// Tools that are read-only (they change no project state) but whose OUTPUT is
/// a message that another component acts on. Whatever such a tool sends is
/// untrusted-origin data: an agent that ingests untrusted input (the
/// Chief-of-Staff) can be prompt-injected into sending anything. Consumers
/// (for the Chief-of-Staff, the `ta-virtual-team` poller) must therefore
/// validate every message and must never treat it as trusted instructions.
/// Any tool served by a third-party extension is likewise untrusted by
/// default (it is not in `MCP_TOOL_EFFECTS`, so `classify_mcp_tool` returns
/// `None` and a read-only surface refuses it).
pub const UNTRUSTED_SINK_TOOLS: &[&str] = &["ta_whiteboard_outcome_send"];

/// True when the named tool's output must be treated as untrusted by whoever
/// consumes it.
pub fn is_untrusted_sink(name: &str) -> bool {
    UNTRUSTED_SINK_TOOLS.contains(&name)
}

/// Name prefixes that are mutating by construction, so a future tool in
/// the family is fail-closed even before it is added to the table.
pub const MUTATING_TOOL_PREFIXES: &[&str] = &["ta_propose_"];

/// Claude Code permission prefix for TA's own MCP server.
pub const TA_MCP_PREFIX: &str = "mcp__ta__";

/// Classify a bare MCP tool name (e.g. `ta_fs_read`). `None` means the tool
/// is unknown: callers must treat that as not read-only.
pub fn classify_mcp_tool(name: &str) -> Option<ToolEffect> {
    if MUTATING_TOOL_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Some(ToolEffect::Mutating);
    }
    MCP_TOOL_EFFECTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, effect)| *effect)
}

/// Default harness allow-list (Claude Code permission patterns) for a
/// persona declaring `read_only = true` with no explicit `allowed_tools`:
/// the CoS toolset from the CoS read-only chat-mode design (read files and
/// diffs, read wiki, see who is on the whiteboard, read goal/plan/draft
/// status). No native tool at all: native `Read` would bypass the policy
/// layer's secrets backstop, and every other native tool can mutate.
pub const READ_ONLY_PERSONA_ALLOWED_TOOLS: &[&str] = &[
    "mcp__ta__ta_fs_read",
    "mcp__ta__ta_fs_diff",
    "mcp__ta__ta_fs_list",
    "mcp__ta__ta_wiki_search",
    "mcp__ta__ta_wiki_get",
    "mcp__ta__ta_wiki_types",
    "mcp__ta__ta_whiteboard_presence_list",
    "mcp__ta__ta_goal_status",
    "mcp__ta__ta_goal_list",
    "mcp__ta__ta_plan_status",
    "mcp__ta__ta_pr_status",
    "mcp__ta__ta_agent_status",
];

/// Why one entry of a read-only persona's allow-list is not acceptable.
pub fn read_only_violation(entry: &str) -> Option<String> {
    let Some(tool) = entry.strip_prefix(TA_MCP_PREFIX) else {
        return Some(format!(
            "'{}' is not a TA MCP tool (native tools and other MCP servers are not allowed \
             for a read-only persona)",
            entry
        ));
    };
    if tool.contains(['*', '?', '[', '(', ' ']) {
        return Some(format!(
            "'{}' is a pattern, not a single tool (wildcards could include mutating tools)",
            entry
        ));
    }
    match classify_mcp_tool(tool) {
        Some(ToolEffect::ReadOnly) => None,
        Some(ToolEffect::Mutating) => Some(format!("'{}' is a mutating tool", entry)),
        None => Some(format!("'{}' is not a known TA MCP tool", entry)),
    }
}

/// Validate a read-only persona's allow-list. Returns every violation so an
/// operator fixing a persona file sees them all at once.
pub fn validate_read_only_tool_surface(allowed: &[String]) -> Result<(), Vec<String>> {
    let violations: Vec<String> = allowed
        .iter()
        .filter_map(|e| read_only_violation(e))
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// Explicit deny patterns for every mutating TA MCP tool, added to a
/// read-only persona's harness `deny` list as defense in depth: an allow
/// list only pre-approves, deny wins at every settings layer.
pub fn mutating_mcp_deny_patterns() -> Vec<String> {
    MCP_TOOL_EFFECTS
        .iter()
        .filter(|(_, e)| *e == ToolEffect::Mutating)
        .map(|(n, _)| format!("{}{}", TA_MCP_PREFIX, n))
        .chain(
            MUTATING_TOOL_PREFIXES
                .iter()
                .map(|p| format!("{}{}*", TA_MCP_PREFIX, p)),
        )
        .collect()
}

/// Whether a harness allow-list entry covers the TA MCP tool `name`: an
/// exact `mcp__ta__<name>` entry, the server-wide `mcp__ta__*` / `mcp__ta`,
/// or a trailing-`*` prefix such as `mcp__ta__ta_wiki_*`.
fn allow_entry_covers_ta_tool(entry: &str, name: &str) -> bool {
    let entry = entry.trim();
    if entry == "mcp__ta" || entry == "mcp__ta__" {
        return true;
    }
    let Some(rest) = entry.strip_prefix(TA_MCP_PREFIX) else {
        return false;
    };
    match rest.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => rest == name,
    }
}

/// Explicit deny patterns for every TA MCP tool a restricted launch did NOT
/// allow (every tool in [`MCP_TOOL_EFFECTS`], plus the
/// [`MUTATING_TOOL_PREFIXES`] families).
///
/// A restricted launch (persona `allowed_tools`, `read_only`, chat mode)
/// writes these so a broader allow from any other settings source (for
/// example an operator's global `mcp__ta__*`) cannot hand the agent a TA
/// tool its persona never listed: deny wins over allow in Claude Code.
/// Nothing the allow-list covers is ever denied.
pub fn unlisted_ta_mcp_deny_patterns(allowed: &[String]) -> Vec<String> {
    let covered = |name: &str| allowed.iter().any(|a| allow_entry_covers_ta_tool(a, name));
    let mut deny: Vec<String> = MCP_TOOL_EFFECTS
        .iter()
        .filter(|(n, _)| !covered(n))
        .map(|(n, _)| format!("{}{}", TA_MCP_PREFIX, n))
        .collect();
    for prefix in MUTATING_TOOL_PREFIXES {
        // Deny the whole family only when no allow entry reaches into it,
        // otherwise the family deny would override that allow.
        let family_touched = allowed.iter().any(|a| {
            let a = a.trim();
            a == "mcp__ta"
                || a == "mcp__ta__"
                || a.strip_prefix(TA_MCP_PREFIX).is_some_and(|rest| {
                    rest.starts_with(prefix)
                        || rest
                            .strip_suffix('*')
                            .is_some_and(|wild| prefix.starts_with(wild))
                })
        });
        if !family_touched {
            deny.push(format!("{}{}*", TA_MCP_PREFIX, prefix));
        }
    }
    deny
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unlisted_ta_tools_are_all_denied_and_listed_ones_never_are() {
        let allowed = strings(&["mcp__ta__ta_fs_read", "mcp__ta__ta_wiki_search", "Bash(*)"]);
        let deny = unlisted_ta_mcp_deny_patterns(&allowed);
        assert!(!deny.contains(&"mcp__ta__ta_fs_read".to_string()));
        assert!(!deny.contains(&"mcp__ta__ta_wiki_search".to_string()));
        for (name, _) in MCP_TOOL_EFFECTS {
            if *name != "ta_fs_read" && *name != "ta_wiki_search" {
                assert!(
                    deny.contains(&format!("mcp__ta__{}", name)),
                    "{} not denied",
                    name
                );
            }
        }
        assert!(deny.contains(&"mcp__ta__ta_propose_*".to_string()));
    }

    #[test]
    fn server_wide_allow_denies_nothing() {
        for wide in ["mcp__ta__*", "mcp__ta"] {
            assert!(unlisted_ta_mcp_deny_patterns(&strings(&[wide])).is_empty());
        }
    }

    #[test]
    fn prefix_allow_covers_its_family_only() {
        let deny = unlisted_ta_mcp_deny_patterns(&strings(&["mcp__ta__ta_wiki_*"]));
        assert!(!deny.iter().any(|d| d.starts_with("mcp__ta__ta_wiki_")));
        assert!(deny.contains(&"mcp__ta__ta_fs_write".to_string()));
    }

    #[test]
    fn allowed_propose_tool_is_not_shadowed_by_the_family_deny() {
        let deny = unlisted_ta_mcp_deny_patterns(&strings(&["mcp__ta__ta_propose_task_update"]));
        assert!(!deny.contains(&"mcp__ta__ta_propose_*".to_string()));
    }

    #[test]
    fn empty_allow_list_denies_every_ta_tool() {
        let deny = unlisted_ta_mcp_deny_patterns(&[]);
        assert_eq!(
            deny.len(),
            MCP_TOOL_EFFECTS.len() + MUTATING_TOOL_PREFIXES.len()
        );
    }

    #[test]
    fn table_has_no_duplicates() {
        let mut names: Vec<&str> = MCP_TOOL_EFFECTS.iter().map(|(n, _)| *n).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }

    #[test]
    fn default_read_only_surface_is_valid() {
        let list: Vec<String> = READ_ONLY_PERSONA_ALLOWED_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        validate_read_only_tool_surface(&list).unwrap();
    }

    #[test]
    fn rejects_native_wildcard_foreign_unknown_and_mutating() {
        let list: Vec<String> = [
            "Bash(*)",
            "Read(*)",
            "mcp__ta__*",
            "mcp__ta__ta_whiteboard_*",
            "mcp__github__get_issue",
            "mcp__ta__ta_brand_new_tool",
            "mcp__ta__ta_fs_write",
            "mcp__ta__ta_propose_anything",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let errs = validate_read_only_tool_surface(&list).unwrap_err();
        assert_eq!(errs.len(), list.len(), "{:?}", errs);
    }

    #[test]
    fn ask_human_and_outcome_send_are_read_only_and_outcome_send_is_an_untrusted_sink() {
        assert_eq!(
            classify_mcp_tool("ta_ask_human"),
            Some(ToolEffect::ReadOnly)
        );
        assert_eq!(
            classify_mcp_tool("ta_whiteboard_outcome_send"),
            Some(ToolEffect::ReadOnly)
        );
        assert!(is_untrusted_sink("ta_whiteboard_outcome_send"));
        assert!(!is_untrusted_sink("ta_fs_read"));
        // Every untrusted sink must itself be a classified tool.
        for t in UNTRUSTED_SINK_TOOLS {
            assert!(classify_mcp_tool(t).is_some(), "{t} is unclassified");
        }
    }

    #[test]
    fn propose_prefix_is_mutating_even_when_not_in_table() {
        assert_eq!(
            classify_mcp_tool("ta_propose_task_create"),
            Some(ToolEffect::Mutating)
        );
        assert!(mutating_mcp_deny_patterns().contains(&"mcp__ta__ta_propose_*".to_string()));
    }
}
