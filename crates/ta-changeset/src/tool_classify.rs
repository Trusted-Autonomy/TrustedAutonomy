// tool_classify.rs — shared MCP tool-name classification heuristic (v0.17.6.3).
//
// `ta-mcp-gateway::ToolCallInterceptor` and `ta-mediation::ApiMediator` each
// independently maintained their own name-pattern heuristic for guessing
// whether a tool call is read-only, state-changing, irreversible, or an
// external side effect. Same idea, two copies that could silently drift.
// This module is the one implementation both now call.

/// Category a tool name is classified into by suffix/substring pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallCategory {
    /// Read-only: safe to pass through without capture (e.g. `_list`, `_get`).
    ReadOnly,
    /// Cannot be undone once executed (e.g. `_send`, `_delete`).
    Irreversible,
    /// Affects a system outside TA's control but is not classified
    /// irreversible (e.g. `_create`, `_update`).
    ExternalSideEffect,
    /// Some other mutating operation (e.g. `_write`).
    StateChanging,
    /// No pattern matched — caller decides the safe default.
    Unclassified,
}

const READ_PATTERNS: &[&str] = &[
    "_read", "_get", "_list", "_search", "_find", "_query", "_fetch",
];
const IRREVERSIBLE_PATTERNS: &[&str] = &["_send", "_publish", "_tweet", "_delete", "_drop"];
const EXTERNAL_PATTERNS: &[&str] = &["_post", "_create", "_update", "_put", "_patch", "_upload"];
const STATE_CHANGING_PATTERNS: &[&str] = &["_write"];

fn matches_any(tool_name: &str, patterns: &[&str]) -> bool {
    patterns
        .iter()
        .any(|p| tool_name.ends_with(p) || tool_name.contains(p))
}

/// Classify a tool name by suffix/substring pattern.
///
/// Checked in order: read-only, then irreversible, then external-side-effect,
/// then generic state-changing. A name matching none of these is
/// `Unclassified` — callers decide what "no information" means for them
/// (e.g. capture-by-default vs. pass through).
pub fn classify_tool_name(tool_name: &str) -> ToolCallCategory {
    if matches_any(tool_name, READ_PATTERNS) {
        ToolCallCategory::ReadOnly
    } else if matches_any(tool_name, IRREVERSIBLE_PATTERNS) {
        ToolCallCategory::Irreversible
    } else if matches_any(tool_name, EXTERNAL_PATTERNS) {
        ToolCallCategory::ExternalSideEffect
    } else if matches_any(tool_name, STATE_CHANGING_PATTERNS) {
        ToolCallCategory::StateChanging
    } else {
        ToolCallCategory::Unclassified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_patterns_classified_read_only() {
        assert_eq!(
            classify_tool_name("gmail_search"),
            ToolCallCategory::ReadOnly
        );
        assert_eq!(classify_tool_name("drive_list"), ToolCallCategory::ReadOnly);
    }

    #[test]
    fn irreversible_patterns_classified_irreversible() {
        assert_eq!(
            classify_tool_name("gmail_send"),
            ToolCallCategory::Irreversible
        );
        assert_eq!(
            classify_tool_name("db_delete"),
            ToolCallCategory::Irreversible
        );
    }

    #[test]
    fn external_patterns_classified_external_side_effect() {
        assert_eq!(
            classify_tool_name("jira_create"),
            ToolCallCategory::ExternalSideEffect
        );
    }

    #[test]
    fn write_pattern_classified_state_changing() {
        assert_eq!(
            classify_tool_name("cache_write"),
            ToolCallCategory::StateChanging
        );
    }

    #[test]
    fn unknown_tool_is_unclassified() {
        assert_eq!(
            classify_tool_name("custom_tool"),
            ToolCallCategory::Unclassified
        );
    }
}
