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

use std::path::{Path, PathBuf};

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
/// The REAL project root a read-only chat session reads (never a copy of
/// it). `ta run` sets it for a chat-mode launch; the gateway reads project
/// configuration (`.ta/workflow.toml`, `.ta/wiki-resources.toml`, the plan
/// file) from here and serves `ta_fs_read` from here, subject to the read
/// grant below.
pub const ENV_CHAT_PROJECT_ROOT: &str = "TA_CHAT_PROJECT_ROOT";
/// The biscuit grant ([`mint_chat_read_grant`]) that lets the session read
/// [`ENV_CHAT_PROJECT_ROOT`]. Without it the session gets no project read
/// access at all: every read that needs it is refused with a message
/// saying so.
pub const ENV_CHAT_READ_GRANT: &str = "TA_CHAT_READ_GRANT";

/// The Wayfinder project this chat session is bound to (the CoS wake
/// context). Optional: when absent the pin is `[plan.wayfinder].project_id`
/// from `.ta/workflow.toml`. When present it must AGREE with that config, or
/// the read tools refuse (fail closed). Never taken from a tool argument.
pub const ENV_CHAT_WAYFINDER_PROJECT_ID: &str = "TA_CHAT_WAYFINDER_PROJECT_ID";

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
    /// Wayfinder project this session is pinned to, from the wake context.
    pub wayfinder_project_id: Option<String>,
    /// The real project root this session reads (`TA_CHAT_PROJECT_ROOT`).
    pub project_root: Option<PathBuf>,
    /// Biscuit read grant (`TA_CHAT_READ_GRANT`), verified by the gateway
    /// against the project's broker key.
    pub read_grant: Option<String>,
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

        let wayfinder_project_id = get(ENV_CHAT_WAYFINDER_PROJECT_ID)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let project_root = get(ENV_CHAT_PROJECT_ROOT)
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        let read_grant = get(ENV_CHAT_READ_GRANT)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(Some(Self {
            agent_id,
            session_id,
            workspace_root,
            wayfinder_project_id,
            project_root,
            read_grant,
        }))
    }
}

/// The policy identity of a chat session (`<agent>:chat:<session>`), the id
/// its manifest and its read grant are bound to.
pub fn chat_policy_id(agent_id: &str, session_id: Uuid) -> String {
    format!(
        "{}{}{}",
        agent_id,
        ta_goal::chat_mode::CHAT_POLICY_ID_MARKER,
        session_id
    )
}

/// Credential id carried by chat read grants. They are not tied to any
/// stored credential; the nil id can never name one.
const CHAT_READ_CREDENTIAL_ID: Uuid = Uuid::nil();

/// Mint the biscuit grant for a chat session's read access: signed with the
/// project's broker root key (the same custody connector credential grants
/// use), bound to the session's policy identity, expiring with the
/// session's manifest. The scopes are `ta_policy::ChatReadAccess::to_scopes`.
pub fn mint_chat_read_grant(
    project_root: &Path,
    policy_agent_id: &str,
    access: &ta_policy::ChatReadAccess,
) -> Result<String, GatewayError> {
    let broker =
        ta_credential_broker::CredentialBroker::open_for_project(project_root).map_err(|e| {
            GatewayError::Other(format!(
                "could not open the credential broker for {} to mint the chat read grant: {e}. \
                 Run `ta doctor` to check the project's credential custody.",
                project_root.display()
            ))
        })?;
    let granted = broker
        .grant(
            CHAT_READ_CREDENTIAL_ID,
            policy_agent_id,
            access.to_scopes(),
            (CHAT_VALIDITY_HOURS as u64) * 3600,
        )
        .map_err(|e| GatewayError::Other(format!("could not mint the chat read grant: {e}")))?;
    Ok(granted.token)
}

/// Verify a chat read grant (signature, expiry, revocation, every
/// attenuation hop) and return what it still authorizes. Fails closed: a
/// forged, expired, revoked or differently-bound token is an error, never an
/// empty grant, so a broken launch is visible instead of silently losing
/// access.
pub fn verify_chat_read_grant(
    project_root: &Path,
    token: &str,
    policy_agent_id: &str,
) -> Result<ta_policy::ChatReadAccess, GatewayError> {
    let rejected = |why: String| {
        GatewayError::Other(format!(
            "chat read grant ({ENV_CHAT_READ_GRANT}) rejected for session identity \
             '{policy_agent_id}': {why}. The Chief of Staff gets no project read access without \
             a valid grant; re-launch through `ta run --chat-mode` (or a chat_mode persona) to \
             mint a fresh one."
        ))
    };
    let broker =
        ta_credential_broker::CredentialBroker::open_for_project(project_root).map_err(|e| {
            rejected(format!(
                "could not open the credential broker for {}: {e}",
                project_root.display()
            ))
        })?;
    let scopes = broker
        .effective_scopes(token)
        .map_err(|e| rejected(e.to_string()))?;
    let Some(first) = scopes.first() else {
        return Err(rejected("the token carries no scopes".to_string()));
    };
    let verified = broker
        .authorize_scope(token, first)
        .map_err(|e| rejected(e.to_string()))?;
    if verified.agent_id != policy_agent_id {
        return Err(rejected(format!(
            "the token was minted for '{}', not for this session",
            verified.agent_id
        )));
    }
    ta_policy::ChatReadAccess::from_scopes(&scopes).map_err(rejected)
}

/// The TA state directory of `project_root`, when one exists. Part of the built-in
/// read hard deny: a chat session never reads TA's own state.
pub fn project_state_dir(project_root: &Path) -> Option<PathBuf> {
    let locator = ta_credentials::state::StateLocator::from_env().ok()?;
    project_state_dir_in(project_root, &locator)
}

fn project_state_dir_in(
    project_root: &Path,
    locator: &ta_credentials::state::StateLocator,
) -> Option<PathBuf> {
    ta_credentials::state::ProjectState::locate(project_root, locator)
        .ok()
        .flatten()
        .map(|s| s.dir)
}

/// The environment (home, state dir, project registry) the read policy of
/// `project_root` is compiled against.
pub fn read_env(project_root: &Path) -> ta_policy::read_policy::ReadEnv {
    ta_policy::read_policy::ReadEnv::for_project(project_root)
        .with_state_dir(project_state_dir(project_root))
}

/// The per-machine overlay file of `project_root`: `<state dir>/projects/<project-id>/
/// chat-read.toml`. When the project has no state directory yet (nothing was ever
/// stored for it) the id is not minted by a read; the returned path then carries a
/// placeholder id segment and does not exist.
pub fn machine_lists_path(
    project_root: &Path,
    locator: &ta_credentials::state::StateLocator,
) -> PathBuf {
    match ta_credentials::state::ProjectState::locate(project_root, locator) {
        Ok(Some(state)) => state.chat_read_path(),
        _ => locator
            .projects_dir()
            .join("<project-id, assigned on first write>")
            .join(ta_credentials::state::CHAT_READ_FILENAME),
    }
}

/// Read the overlay at `path`. A missing file is empty lists; a file that does not
/// parse is an error (a broken overlay must not silently drop a `deny`).
pub fn read_machine_lists(
    path: &Path,
) -> Result<ta_policy::read_policy::MachineLists, GatewayError> {
    match std::fs::read_to_string(path) {
        Ok(text) => ta_policy::read_policy::MachineLists::parse(&text, path)
            .map_err(|e| GatewayError::Other(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(GatewayError::Other(format!(
            "could not read the machine read-list file {}: {e}. Check its permissions, or \
             rewrite it with `ta chat-read machine set`.",
            path.display()
        ))),
    }
}

/// Atomically replace the overlay of `project_root` (temp file, fsync, rename; mode
/// 0600 on Unix). Creates the project's state directory on first use. Returns the path
/// written.
pub fn write_machine_lists(
    project_root: &Path,
    locator: &ta_credentials::state::StateLocator,
    lists: &ta_policy::read_policy::MachineLists,
) -> Result<PathBuf, GatewayError> {
    let state = ta_credentials::state::ProjectState::open(project_root, locator).map_err(|e| {
        GatewayError::Other(format!(
            "could not open the TA state directory of {} to store the machine read lists: {e}",
            project_root.display()
        ))
    })?;
    let path = state.chat_read_path();
    ta_credentials::state_dir::write_atomic_private(&path, lists.render().as_bytes()).map_err(
        |e| {
            GatewayError::Other(format!(
                "could not write the machine read lists to {}: {e}. Check that the directory \
                 is writable by your user (the state directory is chosen by TA_STATE_DIR or \
                 `ta state relocate`).",
                path.display()
            ))
        },
    )?;
    Ok(path)
}

/// `[chat.read] allow` / `deny` (plus the `readable_projects` alias) of the project's
/// `.ta/workflow.toml`; empty when the file does not exist.
pub fn project_lists(project_root: &Path) -> Result<(Vec<String>, Vec<String>), GatewayError> {
    let workflow_toml = project_root.join(".ta").join("workflow.toml");
    if !workflow_toml.is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let workflow = ta_submit::WorkflowConfig::load(&workflow_toml).map_err(|e| {
        GatewayError::Other(format!(
            "could not read [chat.read] allow/deny: {} did not parse: {e}. Fix the TOML and \
             re-launch.",
            workflow_toml.display()
        ))
    })?;
    Ok((workflow.chat.read_allow(), workflow.chat.read_deny()))
}

/// Both layers of the read policy with the resolution of every entry.
pub struct ReadListing {
    pub policy: ta_policy::read_policy::ReadPolicy,
    pub entries: Vec<ta_policy::read_policy::ListedEntry>,
    pub machine: ta_policy::read_policy::MachineLists,
    pub machine_file: PathBuf,
    pub env: ta_policy::read_policy::ReadEnv,
}

/// Compile the read policy of `project_root`: `[chat.read]` of its
/// `.ta/workflow.toml` (plus the `readable_projects` alias) as the base, UNION the
/// per-machine overlay. Deny beats allow across both; the built-in hard-deny floor is
/// unchanged. `strict_project` makes an unusable project entry an error (the launch
/// path); `false` reports it per entry (`ta chat-read list`).
pub fn load_read_listing(
    project_root: &Path,
    strict_project: bool,
) -> Result<ReadListing, GatewayError> {
    let locator = ta_credentials::state::StateLocator::from_env().ok();
    load_read_listing_in(project_root, locator.as_ref(), strict_project)
}

/// [`load_read_listing`] with an explicit state locator (`None`: no state directory
/// can be determined, so there is no overlay and no state dir to protect).
pub fn load_read_listing_in(
    project_root: &Path,
    locator: Option<&ta_credentials::state::StateLocator>,
    strict_project: bool,
) -> Result<ReadListing, GatewayError> {
    let (allow, deny) = project_lists(project_root)?;
    let (machine, machine_file) = match locator {
        Some(locator) => {
            let path = machine_lists_path(project_root, locator);
            (read_machine_lists(&path)?, path)
        }
        None => (
            Default::default(),
            PathBuf::from("<no TA state directory could be determined>"),
        ),
    };
    let env = ta_policy::read_policy::ReadEnv::for_project(project_root)
        .with_state_dir(locator.and_then(|l| project_state_dir_in(project_root, l)));
    let (policy, entries) = ta_policy::read_policy::compile_layers(
        &env,
        ta_policy::read_policy::Layers {
            project_allow: &allow,
            project_deny: &deny,
            machine: &machine,
        },
        strict_project,
    )
    .map_err(|e| GatewayError::Other(e.to_string()))?;
    Ok(ReadListing {
        policy,
        entries,
        machine,
        machine_file,
        env,
    })
}

/// Compile the chat read policy of `project_root`: `[chat.read] allow` / `deny` of its
/// `.ta/workflow.toml` (plus the `readable_projects` alias) UNION the per-machine
/// overlay (`chat-read.toml` in the project's TA state directory). This is the
/// ONE place config becomes a policy; the launcher mints the result into the
/// biscuit read grant, the sandbox profile is built from it, and
/// `ta chat-read check` explains it. No config file means project root only.
/// A PROJECT entry that cannot be resolved is an error naming the entry and the key:
/// the lists are the owner's explicit widening of read access, so a typo must be
/// loud, not dropped. A MACHINE entry that cannot be applied (a folder the owner
/// deleted since) is skipped with a warning so the Chief of Staff still launches; it
/// is reported by `ta chat-read list`.
pub fn load_read_policy(
    project_root: &Path,
) -> Result<ta_policy::read_policy::ReadPolicy, GatewayError> {
    let listing = load_read_listing(project_root, true)?;
    for e in &listing.entries {
        if e.source == ta_policy::read_policy::EntrySource::Machine
            && e.state != ta_policy::read_policy::EntryState::Applied
        {
            tracing::warn!(
                list = e.list.as_str(),
                entry = %e.entry,
                state = e.state.as_str(),
                detail = e.detail.as_deref().unwrap_or(""),
                machine_file = %listing.machine_file.display(),
                "a machine chat-read entry is not applied to this launch; \
                 `ta chat-read list` shows why"
            );
        }
    }
    Ok(listing.policy)
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
            (ENV_CHAT_WAYFINDER_PROJECT_ID, " proj-1 "),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(launch.wayfinder_project_id.as_deref(), Some("proj-1"));
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

    #[test]
    fn parses_project_root_and_read_grant() {
        let launch = ChatLaunch::from_vars(vars(&[
            (ENV_CHAT_MODE, "1"),
            (ENV_CHAT_PROJECT_ROOT, "/work/proj"),
            (ENV_CHAT_READ_GRANT, " tok "),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(launch.project_root, Some(PathBuf::from("/work/proj")));
        assert_eq!(launch.read_grant.as_deref(), Some("tok"));
        let none = ChatLaunch::from_vars(vars(&[(ENV_CHAT_MODE, "1")]))
            .unwrap()
            .unwrap();
        assert_eq!((none.project_root, none.read_grant), (None, None));
    }

    fn read_access() -> ta_policy::ChatReadAccess {
        use ta_policy::read_policy::{ReadRule, RuleKind};
        ta_policy::ChatReadAccess {
            project_root: Some(PathBuf::from("/work/proj")),
            allow: vec![ReadRule {
                kind: RuleKind::Path,
                pattern: "/work/other".to_string(),
                source: "~/other".to_string(),
                label: Some("other".to_string()),
                registered: false,
            }],
            deny: vec![ReadRule {
                kind: RuleKind::Glob,
                pattern: "/work/other/**/secret, 1".to_string(),
                source: "~/other/**/secret, 1".to_string(),
                label: None,
                registered: false,
            }],
            tools: vec!["ta_plan_status".to_string()],
        }
    }

    #[test]
    fn read_grant_round_trips_through_the_broker_custody() {
        let project = tempfile::tempdir().unwrap();
        let id = chat_policy_id("chief-of-staff", Uuid::new_v4());
        let token = mint_chat_read_grant(project.path(), &id, &read_access()).unwrap();
        let got = verify_chat_read_grant(project.path(), &token, &id).unwrap();
        assert_eq!(got, read_access());
    }

    #[test]
    fn read_grant_for_another_session_or_a_forged_token_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let id = chat_policy_id("chief-of-staff", Uuid::new_v4());
        let token = mint_chat_read_grant(project.path(), &id, &read_access()).unwrap();

        let other = chat_policy_id("chief-of-staff", Uuid::new_v4());
        let err = verify_chat_read_grant(project.path(), &token, &other)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not for this session"), "{err}");
        assert!(err.contains(ENV_CHAT_READ_GRANT), "{err}");

        let forged = verify_chat_read_grant(project.path(), "not-a-biscuit", &id)
            .unwrap_err()
            .to_string();
        assert!(forged.contains("rejected"), "{forged}");
    }

    #[test]
    fn read_policy_comes_from_chat_read_lists_and_the_alias_and_typos_are_loud() {
        let base = tempfile::tempdir().unwrap();
        let main = base.path().join("main");
        let notes = base.path().join("notes");
        let alias = base.path().join("alias");
        for d in [main.join(".ta"), notes.clone(), alias.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let wf = |body: &str| std::fs::write(main.join(".ta/workflow.toml"), body).unwrap();
        // No config: project root only.
        std::fs::remove_dir_all(main.join(".ta")).unwrap();
        let p = load_read_policy(&main).unwrap();
        assert!(p.allow.is_empty() && p.deny.is_empty());
        std::fs::create_dir_all(main.join(".ta")).unwrap();
        wf(&format!(
            "[chat]\nreadable_projects = [\"{}\"]\n[chat.read]\nallow = [\"{}\"]\ndeny = [\"{}\"]\n",
            alias.display(),
            notes.display(),
            notes.join("private").display()
        ));
        let p = load_read_policy(&main).unwrap();
        assert_eq!(p.allow.len(), 2);
        assert_eq!(p.deny.len(), 1);
        wf("[chat.read]\nallow = [\"nope\"]\n");
        let err = load_read_policy(&main).unwrap_err().to_string();
        assert!(
            err.contains("[chat.read] allow") && err.contains("workflow.toml"),
            "{err}"
        );
        wf("[chat.read\n");
        assert!(load_read_policy(&main)
            .unwrap_err()
            .to_string()
            .contains("did not parse"));
    }
}
