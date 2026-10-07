// chat_launch.rs: how an agent-facing `ta serve` process learns it was
// launched for a chat-mode session.
//
// `GatewayState` is in-process, so the chat-session manifest has to be
// loaded inside the very MCP server process the agent talks to. `ta run`
// (apps/ta-cli/src/commands/run.rs) does that by:
//
// 1. pointing the agent at a dedicated MCP config
//    (`.ta/mcp-agent-chat.json`) whose `ta` server entry carries
//    `TA_CHAT_MODE=1` in its own `env` block, so chat mode is switched on
//    by the config file itself and does not depend on environment
//    inheritance; and
// 2. putting the per-launch values (`TA_CHAT_SESSION_ID`,
//    `TA_CHAT_AGENT_ID`, `TA_CHAT_WORKSPACE_ROOT`) into the agent process's
//    environment, which the agent harness passes on to its stdio MCP
//    servers (the same convention `TA_PROJECT_ROOT` already relies on).
//
// Fail-closed rules: once `TA_CHAT_MODE` is on, nothing can turn chat mode
// back off. A missing session id gets a freshly generated one (the agent is
// told which id to use by the server's own instructions and by every
// lock-mismatch error); a malformed session id or a reserved agent id is a
// hard startup error, never a silent fallback to an unrestricted server.

use std::path::PathBuf;

use uuid::Uuid;

use crate::error::GatewayError;

/// Turns chat mode on for this MCP server process. Any value other than
/// `""`, `"0"` or `"false"` means on.
pub const ENV_CHAT_MODE: &str = "TA_CHAT_MODE";
/// The chat session id (a UUID) this server is locked to. `ta run`
/// pre-generates it so it can tell the agent the id up front.
pub const ENV_CHAT_SESSION_ID: &str = "TA_CHAT_SESSION_ID";
/// The caller-facing agent id (persona or agent name). Must not contain
/// the reserved `:chat:` marker.
pub const ENV_CHAT_AGENT_ID: &str = "TA_CHAT_AGENT_ID";
/// Workspace root chat reads resolve against (the goal's staging copy).
pub const ENV_CHAT_WORKSPACE_ROOT: &str = "TA_CHAT_WORKSPACE_ROOT";

/// Resource scope the chat manifest is compiled against.
pub const CHAT_RESOURCE_SCOPE: &str = "fs://workspace/**";
/// Validity of a launched chat session's manifest. Matches the
/// `start_goal_with_profile` default so a long agent run is not cut off.
pub const CHAT_VALIDITY_HOURS: i64 = 8;
/// Agent id used when the launcher supplied none.
pub const DEFAULT_CHAT_AGENT_ID: &str = "chat-agent";

/// Parameters for starting an MCP server locked to one chat session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLaunch {
    pub agent_id: String,
    pub session_id: Uuid,
    pub workspace_root: Option<PathBuf>,
}

impl ChatLaunch {
    /// Read chat-launch settings from the process environment.
    pub fn from_env() -> Result<Option<Self>, GatewayError> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// Read chat-launch settings through `get` (injectable for tests).
    /// `Ok(None)` means this is not a chat-mode launch.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, GatewayError> {
        let enabled = match get(ENV_CHAT_MODE) {
            None => false,
            Some(v) => {
                let v = v.trim().to_ascii_lowercase();
                !(v.is_empty() || v == "0" || v == "false")
            }
        };
        if !enabled {
            return Ok(None);
        }

        let session_id = match get(ENV_CHAT_SESSION_ID).filter(|s| !s.trim().is_empty()) {
            Some(raw) => Uuid::parse_str(raw.trim()).map_err(|e| {
                GatewayError::Other(format!(
                    "{}=1 but {}='{}' is not a valid UUID ({}). Refusing to start the TA MCP \
                     server rather than run a chat-mode agent unrestricted. Re-launch through \
                     `ta run --chat-mode` (or a persona with chat_mode = true), which sets this \
                     value for you, or unset {} to start a normal server.",
                    ENV_CHAT_MODE, ENV_CHAT_SESSION_ID, raw, e, ENV_CHAT_MODE
                ))
            })?,
            None => {
                let generated = Uuid::new_v4();
                tracing::warn!(
                    session_id = %generated,
                    "{} is on but {} was not passed to this MCP server; generated a fresh chat \
                     session id. The server is still locked to chat mode; the agent learns the \
                     id from the server instructions.",
                    ENV_CHAT_MODE,
                    ENV_CHAT_SESSION_ID
                );
                generated
            }
        };

        let agent_id = get(ENV_CHAT_AGENT_ID)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_CHAT_AGENT_ID.to_string());
        ta_goal::chat_mode::validate_chat_agent_id(&agent_id).map_err(|msg| {
            GatewayError::Other(format!(
                "refusing to start a chat-mode TA MCP server: {}",
                msg
            ))
        })?;

        let workspace_root = get(ENV_CHAT_WORKSPACE_ROOT)
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);

        Ok(Some(Self {
            agent_id,
            session_id,
            workspace_root,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn not_chat_mode_when_flag_absent_or_off() {
        assert_eq!(ChatLaunch::from_vars(vars(&[])).unwrap(), None);
        assert_eq!(
            ChatLaunch::from_vars(vars(&[(ENV_CHAT_MODE, "0")])).unwrap(),
            None
        );
        assert_eq!(
            ChatLaunch::from_vars(vars(&[(ENV_CHAT_MODE, "false")])).unwrap(),
            None
        );
    }

    #[test]
    fn parses_full_launch() {
        let id = Uuid::new_v4();
        let launch = ChatLaunch::from_vars(vars(&[
            (ENV_CHAT_MODE, "1"),
            (ENV_CHAT_SESSION_ID, &id.to_string()),
            (ENV_CHAT_AGENT_ID, "chief-of-staff"),
            (ENV_CHAT_WORKSPACE_ROOT, "/tmp/staging"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(launch.session_id, id);
        assert_eq!(launch.agent_id, "chief-of-staff");
        assert_eq!(launch.workspace_root, Some(PathBuf::from("/tmp/staging")));
    }

    #[test]
    fn missing_session_id_still_fails_closed_into_chat_mode() {
        let launch = ChatLaunch::from_vars(vars(&[(ENV_CHAT_MODE, "1")]))
            .unwrap()
            .expect("chat mode must stay on");
        assert_eq!(launch.agent_id, DEFAULT_CHAT_AGENT_ID);
    }

    #[test]
    fn malformed_session_id_is_an_actionable_error() {
        let err = ChatLaunch::from_vars(vars(&[
            (ENV_CHAT_MODE, "1"),
            (ENV_CHAT_SESSION_ID, "not-a-uuid"),
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains("not a valid UUID"), "{}", err);
        assert!(err.contains("ta run --chat-mode"), "{}", err);
    }

    #[test]
    fn reserved_chat_marker_in_agent_id_is_rejected() {
        let err = ChatLaunch::from_vars(vars(&[
            (ENV_CHAT_MODE, "1"),
            (ENV_CHAT_AGENT_ID, "cos:chat:deadbeef"),
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains(":chat:"), "{}", err);
    }
}
