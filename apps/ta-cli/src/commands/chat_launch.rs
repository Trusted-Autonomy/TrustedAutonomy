//! Chat-mode launch planning for `ta run` (persona `chat_mode = true`, or
//! `ta run --chat-mode`).
//!
//! A chat-mode launch runs the agent as a read-only session:
//!
//! - Policy identity and capability manifest: the compiled chat manifest
//!   (`ta_policy::compile_chat_manifest`), loaded by the agent's own `ta serve`
//!   MCP process via `GatewayState::start_chat_session_with_id`. `ta run`
//!   pre-generates the session id and passes it, with the agent id and the
//!   staging root, through the agent's environment; the dedicated
//!   `.ta/mcp-agent-chat.json` MCP config switches the server into chat mode
//!   from its own `env` block.
//! - Tool surface: `persona.allowed_tools` intersected with the chat-mode
//!   profile (never the union), further capped by any security-posture
//!   `max_allowed_tools` ceiling. Mutating entries are stripped with a warning.
//! - Native-tool bypass: every native tool in
//!   `ta_goal::chat_mode::CHAT_MODE_NATIVE_DENY` is denied in the injected
//!   `.claude/settings.local.json`, on top of the allow-list restriction
//!   (which already denies every broad default the list does not declare).
//!
//! Everything here is pure planning plus small file writers, so each security
//! property is unit-testable against the exact values `run::execute` uses.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use ta_goal::chat_mode::{
    resolve_chat_mode_tool_surface, validate_chat_agent_id, CHAT_MODE_NATIVE_DENY,
};
use ta_mcp_gateway::chat_launch::{
    ENV_CHAT_AGENT_ID, ENV_CHAT_MODE, ENV_CHAT_SESSION_ID, ENV_CHAT_WORKSPACE_ROOT,
};

/// File name (under the project's `.ta/`) of the MCP config used for
/// chat-mode launches. Stable content, like `mcp-agent.json`, so the agent
/// harness approves it once.
pub(crate) const CHAT_MCP_CONFIG_FILE: &str = "mcp-agent-chat.json";
/// File name (under the project's `.ta/`) of the normal agent MCP config.
pub(crate) const AGENT_MCP_CONFIG_FILE: &str = "mcp-agent.json";

thread_local! {
    static CLI_CHAT_MODE: Cell<bool> = const { Cell::new(false) };
}

/// Scoped carrier for the `ta run --chat-mode` flag. `main.rs` holds one of
/// these for the duration of its `run::execute(...)` call, which keeps the
/// flag out of `execute`'s long, widely-shared parameter list. Thread-local,
/// so parallel tests cannot observe each other's value, and restored on drop.
pub struct CliChatModeGuard {
    previous: bool,
}

impl CliChatModeGuard {
    pub fn set(enabled: bool) -> Self {
        let previous = CLI_CHAT_MODE.with(|c| c.replace(enabled));
        Self { previous }
    }
}

impl Drop for CliChatModeGuard {
    fn drop(&mut self) {
        CLI_CHAT_MODE.with(|c| c.set(self.previous));
    }
}

/// Whether `--chat-mode` was passed for the `execute` call on this thread.
pub(crate) fn cli_chat_mode_requested() -> bool {
    CLI_CHAT_MODE.with(|c| c.get())
}

/// Everything `plan_chat_mode_launch` needs to decide whether, and how, a
/// launch runs in chat mode.
#[derive(Debug, Clone)]
pub(crate) struct ChatModeInputs<'a> {
    pub cli_flag: bool,
    pub persona_name: Option<&'a str>,
    pub persona_chat_mode: bool,
    pub persona_allowed_tools: &'a [String],
    /// Resolved agent id (`--agent` / persona binding / default).
    pub agent: &'a str,
    /// The agent framework's `name` (only `claude-code` supports both the
    /// settings-based native tool restriction and the MCP-config handoff).
    pub agent_framework_name: Option<&'a str>,
    pub injects_settings: bool,
    pub macro_goal: bool,
    /// True when the agent would be launched through the PTY interactive
    /// path, which neither carries the chat MCP config nor survives
    /// `ta run --resume` with chat mode intact.
    pub uses_pty: bool,
}

/// A resolved chat-mode launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatModeLaunchPlan {
    /// Caller-facing agent id the chat session is derived from.
    pub agent_id: String,
    /// Pre-generated chat session id (the agent's `goal_run_id` for `ta_fs_*`).
    pub session_id: uuid::Uuid,
    /// Effective allow-list before any posture ceiling.
    pub allowed_tools: Vec<String>,
    /// Persona entries removed because they are not chat-safe.
    pub stripped: Vec<String>,
    /// Persona wildcards narrowed to the chat-safe subset.
    pub narrowed: Vec<String>,
    /// What switched chat mode on: `"--chat-mode"` or `"persona"`.
    pub source: &'static str,
}

/// Decide whether this launch runs in chat mode and resolve its tool
/// surface. `Ok(None)` means a normal launch: nothing about it changes.
/// Invalid combinations are hard errors that say what happened, what was
/// being attempted, and what to do.
pub(crate) fn plan_chat_mode_launch(
    inputs: &ChatModeInputs,
) -> anyhow::Result<Option<ChatModeLaunchPlan>> {
    if !inputs.cli_flag && !inputs.persona_chat_mode {
        return Ok(None);
    }
    let source = if inputs.cli_flag {
        "--chat-mode"
    } else {
        "persona"
    };
    let what = match inputs.persona_name {
        Some(p) if inputs.persona_chat_mode => format!("persona '{}' (chat_mode = true)", p),
        _ => "ta run --chat-mode".to_string(),
    };

    if inputs.macro_goal {
        anyhow::bail!(
            "Cannot launch {} as a macro goal: chat mode is read-only and removes the \
             ta_draft/ta_goal_inner tools a macro goal depends on. Drop --macro, or run \
             without chat mode.",
            what
        );
    }
    if inputs.uses_pty {
        anyhow::bail!(
            "Cannot launch {} in interactive (PTY) mode: that path does not carry the \
             chat-mode MCP server configuration, so the session could not be locked to its \
             read-only manifest. Use --headless (how the daemon launches chat personas) or \
             omit --interactive.",
            what
        );
    }
    if !inputs.injects_settings || inputs.agent_framework_name != Some("claude-code") {
        anyhow::bail!(
            "Cannot launch {} with agent '{}': chat mode must close the native-tool bypass \
             (Bash, native Read/Write/Edit) by writing the agent harness's own permission \
             settings and must hand it a chat-locked TA MCP server, and only the \
             claude-code framework supports both today (this agent's framework is '{}'). \
             Use --agent claude-code (or a claude-code based agent), or launch without \
             chat mode.",
            what,
            inputs.agent,
            inputs.agent_framework_name.unwrap_or("<unnamed>")
        );
    }

    let agent_id = inputs.persona_name.unwrap_or(inputs.agent).to_string();
    validate_chat_agent_id(&agent_id)
        .map_err(|msg| anyhow::anyhow!("Cannot launch {}: {}", what, msg))?;

    let surface = resolve_chat_mode_tool_surface(inputs.persona_allowed_tools);
    if surface.allowed.is_empty() {
        anyhow::bail!(
            "Cannot launch {}: none of its allowed_tools are chat-safe, so the agent would \
             start with no tools at all. Stripped as not chat-safe: [{}]. Add chat-safe \
             entries (for example \"mcp__ta__ta_fs_read\", \"mcp__ta__ta_wiki_search\") to \
             .ta/personas/{}.toml, or leave allowed_tools empty to get the full chat-mode \
             profile.",
            what,
            surface.stripped.join(", "),
            inputs.persona_name.unwrap_or("<persona>")
        );
    }

    Ok(Some(ChatModeLaunchPlan {
        agent_id,
        session_id: uuid::Uuid::new_v4(),
        allowed_tools: surface.allowed,
        stripped: surface.stripped,
        narrowed: surface.narrowed,
        source,
    }))
}

/// Log (and print, unless quiet) every persona entry chat mode removed or
/// narrowed, so an operator can see exactly why a declared tool is absent.
pub(crate) fn report_chat_mode_plan(plan: &ChatModeLaunchPlan, quiet: bool) {
    if !plan.stripped.is_empty() {
        tracing::warn!(
            agent_id = %plan.agent_id,
            session_id = %plan.session_id,
            stripped = ?plan.stripped,
            "chat mode stripped persona allowed_tools entries that are not chat-safe \
             (mutating or native tools); they are not available to this launch"
        );
        eprintln!(
            "[warn] chat mode: stripped non-chat-safe tools from the allow-list: {}",
            plan.stripped.join(", ")
        );
    }
    if !plan.narrowed.is_empty() {
        tracing::warn!(
            agent_id = %plan.agent_id,
            narrowed = ?plan.narrowed,
            "chat mode narrowed wildcard allowed_tools entries to the chat-safe subset"
        );
    }
    tracing::info!(
        agent_id = %plan.agent_id,
        session_id = %plan.session_id,
        source = plan.source,
        allowed = ?plan.allowed_tools,
        "launching in chat mode (read-only manifest, scratch-only writes, native tools denied)"
    );
    if !quiet {
        println!(
            "Chat mode: read-only session {} for '{}' ({} tools allowed, native tools denied)",
            plan.session_id,
            plan.agent_id,
            plan.allowed_tools.len()
        );
    }
}

/// Resolve the final `(allow, deny)` lists written to the agent's
/// `.claude/settings.local.json` for a chat-mode launch. The allow-list is
/// the plan's surface capped by the posture's `max_allowed_tools` ceiling
/// (intersection again, never widened); the deny list is the posture's own
/// deny patterns plus every chat-mode native deny.
pub(crate) fn chat_mode_settings_lists(
    plan: &ChatModeLaunchPlan,
    ceiling: Option<&[String]>,
    posture_deny: &[String],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let allow: Vec<String> = match ceiling {
        Some(c) => plan
            .allowed_tools
            .iter()
            .filter(|t| c.contains(t))
            .cloned()
            .collect(),
        None => plan.allowed_tools.clone(),
    };
    if allow.is_empty() {
        anyhow::bail!(
            "Cannot launch chat-mode session for '{}': the security posture's \
             max_allowed_tools ceiling {:?} shares no tool with the chat-mode allow-list {:?}, \
             so the agent would have no tools. Add the needed mcp__ta__* entries to the \
             ceiling, or remove the ceiling.",
            plan.agent_id,
            ceiling.unwrap_or(&[]),
            plan.allowed_tools
        );
    }
    let mut deny: Vec<String> = posture_deny.to_vec();
    for t in CHAT_MODE_NATIVE_DENY {
        if !deny.iter().any(|d| d == t) {
            deny.push((*t).to_string());
        }
    }
    Ok((allow, deny))
}

/// Environment entries that hand the chat session to the agent's `ta serve`
/// MCP process.
pub(crate) fn chat_mode_agent_env(
    plan: &ChatModeLaunchPlan,
    staging_path: &Path,
) -> Vec<(String, String)> {
    vec![
        (ENV_CHAT_MODE.to_string(), "1".to_string()),
        (ENV_CHAT_SESSION_ID.to_string(), plan.session_id.to_string()),
        (ENV_CHAT_AGENT_ID.to_string(), plan.agent_id.clone()),
        (
            ENV_CHAT_WORKSPACE_ROOT.to_string(),
            staging_path.display().to_string(),
        ),
    ]
}

/// True when an agent spawn environment carries the chat-mode switch.
pub(crate) fn env_requests_chat_mode(env: &std::collections::HashMap<String, String>) -> bool {
    env.get(ENV_CHAT_MODE)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false")
        })
        .unwrap_or(false)
}

/// The MCP config file an agent launch should point at.
pub(crate) fn agent_mcp_config_path(project_root: &Path, chat_mode: bool) -> PathBuf {
    project_root.join(".ta").join(if chat_mode {
        CHAT_MCP_CONFIG_FILE
    } else {
        AGENT_MCP_CONFIG_FILE
    })
}

/// Write `project_root/.ta/mcp-agent-chat.json`: only the `ta` server, with
/// `TA_CHAT_MODE=1` set in its own `env` block so chat mode does not depend
/// on environment inheritance. No other MCP server (meridian or otherwise)
/// is configured, and the launch uses `--strict-mcp-config`, so the agent
/// has no other MCP tool source.
pub(crate) fn write_chat_agent_mcp_config(project_root: &Path) -> anyhow::Result<PathBuf> {
    let ta_binary = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "ta".to_string());
    let config = serde_json::json!({
        "mcpServers": {
            "ta": {
                "command": ta_binary,
                "args": ["serve"],
                "env": { "TA_IS_STAGING": "1", ENV_CHAT_MODE: "1" }
            }
        }
    });
    let path = agent_mcp_config_path(project_root, true);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&config)?)?;
    Ok(path)
}

/// Context-file section telling the agent it is in chat mode and which
/// `goal_run_id` to pass. Informational only: enforcement is the manifest,
/// the MCP router filter, the chat lock and the settings deny list.
pub(crate) fn chat_mode_context_section(plan: &ChatModeLaunchPlan) -> String {
    format!(
        "\n## Chat Mode (read-only)\n\n\
         This session runs in TA chat mode. Use only the TA MCP tools available to you; \
         native shell and file tools are disabled.\n\n\
         - Pass `goal_run_id = \"{}\"` to `ta_fs_read`, `ta_fs_list`, `ta_fs_diff` and `ta_fs_write`.\n\
         - You can read the workspace. Writes are allowed only under `{}/` and are discarded \
         when the session ends.\n\
         - You cannot start goals, build or apply drafts, change the plan, edit the wiki, \
         update tasks, or take external actions. Answer in your reply instead.\n",
        plan.session_id,
        ta_policy::CHAT_SCRATCH_DIR
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn inputs<'a>(persona_allowed: &'a [String]) -> ChatModeInputs<'a> {
        ChatModeInputs {
            cli_flag: false,
            persona_name: Some("chief-of-staff"),
            persona_chat_mode: true,
            persona_allowed_tools: persona_allowed,
            agent: "claude-code",
            agent_framework_name: Some("claude-code"),
            injects_settings: true,
            macro_goal: false,
            uses_pty: false,
        }
    }

    #[test]
    fn non_chat_launch_is_untouched() {
        let allowed = s(&["Bash(*)", "mcp__ta__ta_goal_start"]);
        let mut i = inputs(&allowed);
        i.persona_chat_mode = false;
        assert_eq!(plan_chat_mode_launch(&i).unwrap(), None);
        // And the MCP config selection is today's file.
        let root = Path::new("/proj");
        assert_eq!(
            agent_mcp_config_path(root, false),
            root.join(".ta").join("mcp-agent.json")
        );
        assert!(!env_requests_chat_mode(&Default::default()));
    }

    #[test]
    fn persona_chat_mode_strips_mutating_tools_from_allow_list() {
        let allowed = s(&[
            "mcp__ta__ta_fs_read",
            "mcp__ta__ta_wiki_search",
            "mcp__ta__ta_goal_start",
            "mcp__ta__ta_wiki_update",
            "Bash(*)",
            "Edit(*)",
        ]);
        let plan = plan_chat_mode_launch(&inputs(&allowed)).unwrap().unwrap();
        assert_eq!(plan.source, "persona");
        assert_eq!(plan.agent_id, "chief-of-staff");
        assert_eq!(
            plan.allowed_tools,
            s(&["mcp__ta__ta_fs_read", "mcp__ta__ta_wiki_search"])
        );
        assert_eq!(
            plan.stripped,
            s(&[
                "mcp__ta__ta_goal_start",
                "mcp__ta__ta_wiki_update",
                "Bash(*)",
                "Edit(*)"
            ])
        );
    }

    #[test]
    fn cli_flag_enables_chat_mode_without_persona_opt_in() {
        let allowed: Vec<String> = Vec::new();
        let mut i = inputs(&allowed);
        i.cli_flag = true;
        i.persona_chat_mode = false;
        i.persona_name = None;
        let plan = plan_chat_mode_launch(&i).unwrap().unwrap();
        assert_eq!(plan.source, "--chat-mode");
        assert_eq!(plan.agent_id, "claude-code");
        assert_eq!(
            plan.allowed_tools,
            ta_goal::chat_mode::chat_mode_allowed_tool_patterns()
        );
    }

    #[test]
    fn invalid_combinations_fail_with_actionable_errors() {
        let allowed: Vec<String> = Vec::new();

        let mut i = inputs(&allowed);
        i.macro_goal = true;
        let e = plan_chat_mode_launch(&i).unwrap_err().to_string();
        assert!(
            e.contains("macro goal") && e.contains("Drop --macro"),
            "{}",
            e
        );

        let mut i = inputs(&allowed);
        i.uses_pty = true;
        let e = plan_chat_mode_launch(&i).unwrap_err().to_string();
        assert!(
            e.contains("interactive") && e.contains("--headless"),
            "{}",
            e
        );

        let mut i = inputs(&allowed);
        i.agent_framework_name = Some("codex");
        i.injects_settings = false;
        i.agent = "codex";
        let e = plan_chat_mode_launch(&i).unwrap_err().to_string();
        assert!(
            e.contains("codex") && e.contains("--agent claude-code"),
            "{}",
            e
        );

        let mut i = inputs(&allowed);
        i.persona_name = Some("cos:chat:deadbeef");
        let e = plan_chat_mode_launch(&i).unwrap_err().to_string();
        assert!(e.contains(":chat:") && e.contains("Rename"), "{}", e);

        let only_mutating = s(&["Bash(*)", "mcp__ta__ta_goal_start"]);
        let e = plan_chat_mode_launch(&inputs(&only_mutating))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("none of its allowed_tools are chat-safe")
                && e.contains("Bash(*)")
                && e.contains(".ta/personas/chief-of-staff.toml"),
            "{}",
            e
        );
    }

    #[test]
    fn ceiling_can_only_narrow_and_empty_result_is_an_error() {
        let allowed: Vec<String> = Vec::new();
        let plan = plan_chat_mode_launch(&inputs(&allowed)).unwrap().unwrap();
        let ceiling = s(&["mcp__ta__ta_fs_read", "Bash(*)"]);
        let (allow, _) = chat_mode_settings_lists(&plan, Some(&ceiling), &[]).unwrap();
        assert_eq!(allow, s(&["mcp__ta__ta_fs_read"]));

        let disjoint = s(&["Bash(*)"]);
        let e = chat_mode_settings_lists(&plan, Some(&disjoint), &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("max_allowed_tools"), "{}", e);
    }

    /// H13: the settings file a chat-mode launch actually writes (via the
    /// same `inject_claude_settings_with_security` the launch path calls)
    /// allows only chat-mode MCP tools and denies every native tool, so
    /// there is no Bash / native file read-write path around the manifest.
    #[test]
    fn chat_mode_settings_close_the_native_tool_bypass() {
        let allowed = s(&["mcp__ta__*", "Bash(*)", "Write(*)"]);
        let plan = plan_chat_mode_launch(&inputs(&allowed)).unwrap().unwrap();
        let (allow, deny) = chat_mode_settings_lists(&plan, None, &[]).unwrap();

        let staging = tempfile::tempdir().unwrap();
        super::super::run::inject_claude_settings_with_security(
            staging.path(),
            None,
            &deny,
            true,
            Some(&allow),
        )
        .unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(staging.path().join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        let allow_written: Vec<String> =
            serde_json::from_value(written["permissions"]["allow"].clone()).unwrap();
        let deny_written: Vec<String> =
            serde_json::from_value(written["permissions"]["deny"].clone()).unwrap();

        assert_eq!(
            allow_written,
            ta_goal::chat_mode::chat_mode_allowed_tool_patterns()
        );
        for t in &allow_written {
            assert!(t.starts_with("mcp__ta__"), "non-MCP tool allowed: {}", t);
            assert_ne!(t, "mcp__ta__*", "wildcard must be narrowed");
        }
        for native in [
            "Bash",
            "Read",
            "Write",
            "Edit",
            "MultiEdit",
            "NotebookEdit",
            "Bash(*)",
            "Write(*)",
            "Edit(*)",
        ] {
            assert!(
                deny_written.iter().any(|d| d == native),
                "{} must be denied in chat mode, deny = {:?}",
                native,
                deny_written
            );
        }
    }

    #[test]
    fn chat_env_and_mcp_config_hand_the_session_to_ta_serve() {
        let allowed: Vec<String> = Vec::new();
        let plan = plan_chat_mode_launch(&inputs(&allowed)).unwrap().unwrap();
        let staging = Path::new("/proj/.ta/staging/abc");
        let env: std::collections::HashMap<String, String> =
            chat_mode_agent_env(&plan, staging).into_iter().collect();
        assert!(env_requests_chat_mode(&env));
        assert_eq!(env[ENV_CHAT_SESSION_ID], plan.session_id.to_string());
        assert_eq!(env[ENV_CHAT_AGENT_ID], "chief-of-staff");
        assert_eq!(env[ENV_CHAT_WORKSPACE_ROOT], staging.display().to_string());

        // The env round-trips through the gateway's own parser into a
        // launch for exactly this session.
        let launch = ta_mcp_gateway::chat_launch::ChatLaunch::from_vars(|k| env.get(k).cloned())
            .unwrap()
            .unwrap();
        assert_eq!(launch.session_id, plan.session_id);
        assert_eq!(launch.agent_id, plan.agent_id);

        let project = tempfile::tempdir().unwrap();
        let path = write_chat_agent_mcp_config(project.path()).unwrap();
        assert_eq!(path, agent_mcp_config_path(project.path(), true));
        let cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let servers = cfg["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1, "only the ta server may be configured");
        assert_eq!(cfg["mcpServers"]["ta"]["env"][ENV_CHAT_MODE], "1");

        let section = chat_mode_context_section(&plan);
        assert!(section.contains(&plan.session_id.to_string()));
    }

    /// Drive the real `run::execute` (with `--no-launch`, so no agent
    /// process is spawned) for a project whose persona declares
    /// `chat_mode`, returning the result and the project dir.
    fn execute_with_persona(
        persona_toml: &str,
        macro_goal: bool,
    ) -> (anyhow::Result<()>, tempfile::TempDir) {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        let personas = project.path().join(".ta").join("personas");
        std::fs::create_dir_all(&personas).unwrap();
        std::fs::write(personas.join("chief-of-staff.toml"), persona_toml).unwrap();
        let config = ta_mcp_gateway::GatewayConfig::for_project(project.path());
        let result = super::super::run::execute(
            &config,
            Some("Chat goal"),
            "claude-code",
            Some(project.path()),
            "Answer a question",
            None,
            None,
            None,
            None,
            None,
            true, // no_launch
            false,
            macro_goal,
            None,
            false,
            true, // skip_verify
            true, // quiet
            None,
            None,
            Some("chief-of-staff"),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        (result, project)
    }

    #[test]
    fn execute_routes_chat_mode_persona_through_the_chat_launch_path() {
        let _home = super::super::test_support::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // chat_mode = true: the chat-locked MCP config is written.
        let (result, project) = execute_with_persona(
            "[persona]\nname = \"chief-of-staff\"\n\n[capabilities]\nchat_mode = true\n\
             allowed_tools = [\"mcp__ta__ta_fs_read\", \"Bash(*)\"]\n",
            false,
        );
        result.unwrap();
        let chat_cfg = agent_mcp_config_path(project.path(), true);
        assert!(chat_cfg.exists(), "chat launch must write {:?}", chat_cfg);

        // Regression: chat_mode absent behaves as before (no chat config).
        let (result, project) = execute_with_persona(
            "[persona]\nname = \"chief-of-staff\"\n\n[capabilities]\n\
             allowed_tools = [\"mcp__ta__ta_fs_read\", \"Bash(*)\"]\n",
            false,
        );
        result.unwrap();
        assert!(!agent_mcp_config_path(project.path(), true).exists());
        assert!(agent_mcp_config_path(project.path(), false).exists());
    }

    #[test]
    fn execute_rejects_invalid_chat_mode_combinations() {
        let _home = super::super::test_support::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (result, project) = execute_with_persona(
            "[persona]\nname = \"chief-of-staff\"\n\n[capabilities]\nchat_mode = true\n",
            true,
        );
        let e = result.unwrap_err().to_string();
        assert!(e.contains("macro goal"), "{}", e);
        assert!(!agent_mcp_config_path(project.path(), true).exists());

        let (result, _p) = execute_with_persona(
            "[persona]\nname = \"chief-of-staff\"\n\n[capabilities]\nchat_mode = true\n\
             allowed_tools = [\"Bash(*)\", \"mcp__ta__ta_goal_start\"]\n",
            false,
        );
        let e = result.unwrap_err().to_string();
        assert!(
            e.contains("none of its allowed_tools are chat-safe"),
            "{}",
            e
        );
    }

    #[test]
    fn cli_guard_is_scoped_and_restored() {
        assert!(!cli_chat_mode_requested());
        {
            let _g = CliChatModeGuard::set(true);
            assert!(cli_chat_mode_requested());
        }
        assert!(!cli_chat_mode_requested());
    }
}
