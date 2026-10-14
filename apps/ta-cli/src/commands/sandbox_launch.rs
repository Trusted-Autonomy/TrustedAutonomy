// sandbox_launch.rs — `ta run` glue for the sandbox launch plumbing
// (v0.17.12.6).
//
// `ta_runtime::sandbox_launch` holds the pure logic (profile, capabilities,
// decision table, env scrub). This module gathers its inputs from the real
// project, starts the out-of-sandbox gateway, prepares the per-goal agent
// home, and reports the decision.
//
// The `[sandbox]` section and `[security] level` are read from the REAL
// project root, never from the staging copy: staging excludes `.ta/`, which
// is why the sandbox used to be silently off.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ta_runtime::egress_proxy::{
    proxy_env, EgressAllowlist, EgressProxy, EgressProxyConfig, EgressRecord, MODEL_API_HOSTS,
};
use ta_runtime::sandbox_launch::{
    decide, prepare_agent_home, scrub_env, EnvScrub, GatewayPlacement, LaunchContext,
    PersonaSandbox, ProjectRequest, RoleClass, SandboxDecision, SandboxPosture, SandboxProfile,
    TrustedPolicy,
};
use ta_runtime::seatbelt_log::{Violation, ViolationTail};
use ta_runtime::{SandboxPolicy, SandboxProvider};

use super::mediation::{self, GatewayGuard};

/// How long the launcher waits for the gateway socket before giving up.
pub const GATEWAY_WAIT_SECS: u64 = 10;

/// Everything `launch_agent_via_runtime` needs from the sandbox decision.
pub struct SandboxPlan {
    pub profile: SandboxProfile,
    pub decision: SandboxDecision,
    pub provider: SandboxProvider,
    /// The spawn is wrapped by a provider.
    pub wraps: bool,
    /// The environment is reduced to the allowlist.
    pub scrubs_env: bool,
    pub agent_home: PathBuf,
    pub claude_config_dir: Option<PathBuf>,
    /// Per-goal bridged MCP config (claude-code with an out-of-sandbox gateway).
    pub mcp_config: Option<PathBuf>,
    pub gateway: Option<GatewayGuard>,
    /// Per-goal egress proxy (hostname allowlist + audit log). Stops on drop.
    pub egress: Option<EgressProxy>,
    /// The allowlist the proxy enforces, as written, for the banner.
    pub egress_hosts: Vec<String>,
    /// Linux bubblewrap: carries the egress proxy across the network namespace.
    /// Stops (and removes its socket) on drop.
    #[cfg(unix)]
    pub egress_relay: Option<ta_runtime::linux_relay::HostRelay>,
    /// Windows AppContainer: carries the egress proxy over a named pipe locked to
    /// the container SID (no loopback exemption). Stops on drop.
    #[cfg(windows)]
    pub egress_pipe: Option<super::egress_pipe::HostPipeRelay>,
    /// TA state directory, where the Windows ACL grant ledger lives.
    pub state_dir: Option<PathBuf>,
    pub warnings: Vec<String>,
}

/// Launch facts the plan is built from.
pub struct PlanInput<'a> {
    pub project_root: &'a Path,
    pub staging: &'a Path,
    pub goal_id: uuid::Uuid,
    pub is_claude_code: bool,
    pub chat: bool,
    pub team_role: bool,
    /// The team role's name (daemon-provided), mapped to a role class.
    pub team_role_name: Option<&'a str>,
    /// The persona this launch uses, for its `[sandbox]` table.
    pub persona: Option<&'a str>,
    /// The agent's own env map, for chat-mode variables the gateway needs too.
    pub agent_env: &'a HashMap<String, String>,
    /// Where denial events are appended (egress proxy denials are reported live).
    pub events_dir: &'a Path,
}

/// Map `[sandbox]` and `[security]` from the project's workflow config.
pub fn project_request(project_root: &Path) -> ProjectRequest {
    let wf = ta_submit::WorkflowConfig::load_or_default(&project_root.join(".ta/workflow.toml"));
    let level = wf.security.level;
    let profile = ta_goal::SecurityProfile::from_level(level, &wf.security.to_overrides());
    let posture = match level {
        ta_goal::SecurityLevel::Low => SandboxPosture::Open,
        ta_goal::SecurityLevel::Mid => SandboxPosture::Balanced,
        ta_goal::SecurityLevel::High => SandboxPosture::Strict,
    };
    ProjectRequest {
        enabled: wf.sandbox.enabled,
        posture: Some(posture),
        security_profile_sandbox: profile.sandbox_enabled,
        allow_read: wf.sandbox.allow_read.clone(),
        allow_write: wf.sandbox.allow_write.clone(),
        allow_network: wf.sandbox.allow_network.clone(),
    }
}

/// Posture from the owner's verified rules file (`posture = "..."`), when one
/// is in effect. The shipped default posture (no owner file) says nothing here,
/// and an invalid file is reported but does not change the box.
pub fn rules_posture(project_root: &Path) -> (Option<SandboxPosture>, Option<String>) {
    use ta_actions::rules::{RulesLoader, Status};
    let loaded = RulesLoader::for_project(project_root).load(None);
    match (&loaded.status, loaded.compiled.as_ref()) {
        (Status::Verified { .. }, Some(c)) => (SandboxPosture::parse(&c.posture), None),
        (Status::Invalid, _) => (
            None,
            Some(
                "the owner rules file is not in effect (invalid or unsigned), so its posture \
                 does not shape the sandbox. Run `ta rules status` for the fix."
                    .to_string(),
            ),
        ),
        _ => (None, None),
    }
}

/// The persona's `[sandbox]` table with its pin status. `Err` is a warning
/// string: the persona could not be read, so no additions apply.
pub fn persona_sandbox(
    project_root: &Path,
    name: &str,
    sandbox_dir: Option<&Path>,
) -> Result<PersonaSandbox, String> {
    let (cfg, hash) = ta_goal::PersonaConfig::load_with_hash(project_root, name)
        .map_err(|e| format!("could not read persona \"{name}\" for its sandbox table: {e}"))?;
    let pins = match sandbox_dir {
        Some(dir) => ta_runtime::sandbox_launch::load_persona_pins(dir)?,
        None => Default::default(),
    };
    let pinned = pins.get(name).is_some_and(|h| *h == hash);
    Ok(PersonaSandbox {
        name: name.to_string(),
        allow_hosts: cfg.sandbox.allow_hosts,
        allow_read: cfg.sandbox.allow_read,
        file_hash: hash,
        pinned,
    })
}

/// The TA state directory for `project_root`, when one exists (where the
/// AppContainer grant ledger lives).
pub fn project_state_dir(project_root: &Path) -> Option<PathBuf> {
    let locator = ta_credentials::state::StateLocator::from_env();
    ta_credentials::state::ProjectState::locate(project_root, &locator)
        .ok()
        .flatten()
        .map(|s| s.dir)
}

/// Pre-population hook for the staging directory (Windows AppContainer only).
///
/// Sets the goal container's inheritable ACE on the EMPTY staging root, so the
/// files copied in afterwards inherit it. Returns `Ok(())` without doing
/// anything when no AppContainer sandbox is going to be used for this goal.
pub fn prepare_staging(project_root: &Path, goal_id: &str, staging: &Path) -> Result<(), String> {
    if SandboxPolicy::detect_provider() != SandboxProvider::WindowsAppContainer {
        return Ok(());
    }
    let state = ta_credentials::state::StateLocator::from_env();
    let state = ta_credentials::state::ProjectState::locate(project_root, &state)
        .ok()
        .flatten();
    let trusted = match &state {
        Some(s) => TrustedPolicy::load(&s.sandbox_dir())?,
        None => None,
    };
    let project = project_request(project_root);
    // Whether this goal is a team role is not known at staging time, so be
    // generous: an unused ACE on a staging dir TA owns costs nothing.
    let wanted = [false, true].into_iter().any(|team_role| {
        let ctx = LaunchContext {
            goal_id: goal_id.to_string(),
            role_class: RoleClass::Worker,
            team_role,
            project_root: project_root.to_path_buf(),
            staging: staging.to_path_buf(),
            state_dir: state.as_ref().map(|s| s.dir.clone()),
            agent_home: PathBuf::new(),
            user_home: dirs::home_dir(),
            mediation_socket: None,
            egress_proxy: false,
            persona: None,
        };
        SandboxProfile::compile(trusted.as_ref(), &project, &ctx).requested
    });
    if !wanted {
        return Ok(());
    }
    let name = ta_runtime::windows_policy::container_name(goal_id);
    ta_runtime::sandbox_windows::prepare_staging_acl(&name, staging)
}

/// Build the plan. `Err` is a launch refusal reason that is already
/// actionable (unreadable trusted policy, refused decision is NOT an error
/// here: the caller inspects `decision`).
pub fn plan(input: &PlanInput<'_>) -> Result<SandboxPlan, String> {
    let locator = ta_credentials::state::StateLocator::from_env();
    let state = ta_credentials::state::ProjectState::locate(input.project_root, &locator)
        .ok()
        .flatten();
    let trusted = match &state {
        Some(s) => TrustedPolicy::load(&s.sandbox_dir())?,
        None => None,
    };
    let mut project = project_request(input.project_root);
    let goal = input.goal_id.to_string();
    let mut warnings = Vec::new();

    // Role class from launch data: chat mode (which includes origins `cos` and
    // `chat`) forces the strictest class; a team role maps through the owner's
    // state-dir table, then the built-in names.
    let owner_roles = trusted
        .as_ref()
        .map(|t| t.roles.clone())
        .unwrap_or_default();
    let role = RoleClass::resolve(input.chat, input.team_role_name, &owner_roles);

    // The owner's verified rules posture tightens chat and team launches.
    if input.chat || input.team_role {
        let (posture, note) = rules_posture(input.project_root);
        if let Some(p) = posture {
            project.posture = Some(project.posture.map_or(p, |q| q.tighten(p)));
        }
        warnings.extend(note);
    }

    let persona = match input.persona {
        Some(name) => match persona_sandbox(
            input.project_root,
            name,
            state.as_ref().map(|s| s.sandbox_dir()).as_deref(),
        ) {
            Ok(p) => Some(p),
            Err(w) => {
                warnings.push(w);
                None
            }
        },
        None => None,
    };
    let mut ctx = LaunchContext {
        goal_id: goal.clone(),
        role_class: role,
        team_role: input.team_role,
        project_root: input.project_root.to_path_buf(),
        staging: input.staging.to_path_buf(),
        state_dir: state.as_ref().map(|s| s.dir.clone()),
        agent_home: PathBuf::new(),
        user_home: dirs::home_dir(),
        mediation_socket: None,
        egress_proxy: false,
        persona,
    };

    // First pass: is a sandbox wanted at all?
    let first = SandboxProfile::compile(trusted.as_ref(), &project, &ctx);
    let provider = SandboxPolicy::detect_provider();
    let wanted = first.requested;

    // Agent home: under the state dir when there is one, else a temp dir.
    ctx.agent_home = match (&state, wanted) {
        (Some(s), _) => s.agent_homes_dir().join(&goal),
        (None, true) => std::env::temp_dir().join("ta-agent-home").join(&goal),
        (None, false) => PathBuf::new(),
    };
    let mut claude_config_dir = None;
    if wanted {
        match prepare_agent_home(&ctx.agent_home) {
            Ok(dir) => claude_config_dir = Some(dir),
            Err(e) => {
                return Err(format!(
                    "could not create the agent home {}: {e}. Check that the TA state \
                     directory is writable.",
                    ctx.agent_home.display()
                ))
            }
        }
    }

    // Gateway outside the sandbox, when there is a box to be outside of.
    let mut gateway = None;
    let can_bridge = wanted && provider != SandboxProvider::None && input.is_claude_code;
    if can_bridge {
        let gateway_env: HashMap<String, String> = input
            .agent_env
            .iter()
            .filter(|(k, _)| k.starts_with("TA_CHAT_"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        match mediation::spawn_gateway(
            &goal,
            input.staging,
            input.project_root,
            &gateway_env,
            std::time::Duration::from_secs(GATEWAY_WAIT_SECS),
        ) {
            Ok(g) => gateway = Some(g),
            Err(e) => warnings.push(format!(
                "could not start the out-of-sandbox gateway: {e}. The agent will run its own \
                 `ta serve` inside the sandbox if the posture allows it."
            )),
        }
    }
    ctx.mediation_socket = gateway.as_ref().map(|g| g.socket.clone());

    // Egress proxy: the Seatbelt profile lets the agent reach only this loopback
    // port, so hostname policy lives here. An explicit "*" grant keeps the open
    // network (still without loopback) and runs no proxy.
    let mut egress = None;
    let mut egress_hosts = Vec::new();
    let sandbox_will_wrap = first.requested
        && matches!(
            provider,
            SandboxProvider::MacosSandboxExec
                | SandboxProvider::LinuxBwrap
                | SandboxProvider::LinuxLandlock
                | SandboxProvider::WindowsAppContainer
        );
    let open_network = first.allow_network.iter().any(|h| h == "*");
    if sandbox_will_wrap && !open_network {
        let mut hosts = first.allow_network.clone();
        if input.is_claude_code {
            hosts.extend(model_api_hosts(input.agent_env));
        }
        hosts.sort();
        hosts.dedup();
        match start_egress_proxy(
            &goal,
            &hosts,
            state.as_ref().map(|s| s.sandbox_dir()),
            input.events_dir,
            input.goal_id,
        ) {
            Ok(p) => {
                egress_hosts = hosts;
                egress = Some(p);
            }
            Err(e) => warnings.push(format!(
                "could not start the egress proxy: {e}. The agent will have no network access."
            )),
        }
    }
    // Linux bubblewrap has no route to host loopback: a Unix socket in a
    // bind-mounted directory carries the proxy across the namespace boundary.
    #[cfg(unix)]
    let mut egress_relay = None;
    #[cfg(unix)]
    if let (Some(p), SandboxProvider::LinuxBwrap) = (&egress, &provider) {
        // Kept short: Unix socket paths are limited to about 100 bytes.
        let short: String = goal.chars().take(8).collect();
        let dir = std::env::temp_dir().join(format!("ta-eg-{short}"));
        match ta_runtime::linux_relay::HostRelay::start(&dir, p.port()) {
            Ok(r) => egress_relay = Some(r),
            Err(e) => warnings.push(format!(
                "could not start the egress relay: {e}. The agent will have no network access."
            )),
        }
    }
    // Windows AppContainer cannot reach host loopback either: a named pipe whose
    // DACL admits only this container carries the proxy across.
    #[cfg(windows)]
    let mut egress_pipe = None;
    #[cfg(windows)]
    if let (Some(p), SandboxProvider::WindowsAppContainer) = (&egress, &provider) {
        match super::egress_pipe::HostPipeRelay::start(&goal, p.port()) {
            Ok(r) => egress_pipe = Some(r),
            Err(e) => warnings.push(format!(
                "could not start the egress pipe relay: {e}. The agent will have no network access."
            )),
        }
    }
    if let Some(note) = ta_runtime::linux_probe::provider_note() {
        if matches!(
            provider,
            SandboxProvider::LinuxBwrap | SandboxProvider::LinuxLandlock | SandboxProvider::None
        ) {
            warnings.push(note);
        }
    }
    ctx.egress_proxy = egress.is_some();
    if wanted && provider == SandboxProvider::WindowsJobObject {
        warnings.push(
            "AppContainer is not available on this machine, so the only Windows layer is a Job \
             Object (process-tree limits, no filesystem or network confinement). The posture \
             decision below says whether this launch may proceed; it is not a silent fallback."
                .to_string(),
        );
    }

    let profile = SandboxProfile::compile(trusted.as_ref(), &project, &ctx);
    let scrubs_env = profile.requested;
    let wraps = profile.requested && provider != SandboxProvider::None;
    let placement = if gateway.is_some() {
        GatewayPlacement::OutOfSandboxSocket
    } else if wraps {
        GatewayPlacement::StdioInSandbox
    } else {
        GatewayPlacement::StdioUnsandboxed
    };
    let decision = decide(&profile, &provider, scrubs_env, placement);
    warnings.extend(profile.warnings.iter().cloned());

    // Per-goal bridged MCP config for claude-code.
    let mut mcp_config = None;
    if let (Some(g), true) = (&gateway, input.is_claude_code) {
        let base_path = super::chat_launch::agent_mcp_config_path(input.project_root, input.chat);
        let base = std::fs::read_to_string(&base_path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let ta_bin = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "ta".into());
        let cfg = mediation::bridged_mcp_config(base.as_ref(), &ta_bin, &g.socket);
        let path = ctx.agent_home.join("mcp-agent.json");
        match std::fs::write(
            &path,
            serde_json::to_string_pretty(&cfg).unwrap_or_default(),
        ) {
            Ok(()) => mcp_config = Some(path),
            Err(e) => {
                return Err(format!(
                    "could not write the bridged MCP config {}: {e}",
                    path.display()
                ))
            }
        }
    }

    Ok(SandboxPlan {
        profile,
        decision,
        provider,
        wraps,
        scrubs_env,
        agent_home: ctx.agent_home,
        claude_config_dir,
        mcp_config,
        gateway,
        egress,
        egress_hosts,
        #[cfg(unix)]
        egress_relay,
        #[cfg(windows)]
        egress_pipe,
        state_dir: state.as_ref().map(|s| s.dir.clone()),
        warnings,
    })
}

/// Hosts a sandboxed Claude Code agent needs to reach its model: the public
/// API plus the host of `ANTHROPIC_BASE_URL` when the owner points it elsewhere.
fn model_api_hosts(agent_env: &HashMap<String, String>) -> Vec<String> {
    let mut hosts: Vec<String> = MODEL_API_HOSTS.iter().map(|h| h.to_string()).collect();
    let base = agent_env
        .get("ANTHROPIC_BASE_URL")
        .cloned()
        .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok());
    if let Some(url) = base {
        if let Some(rest) = url.split("://").nth(1) {
            let authority = rest.split('/').next().unwrap_or("");
            let authority = authority.rsplit('@').next().unwrap_or("");
            if !authority.is_empty() {
                // `host:port` is kept as written; a bare host means 443.
                hosts.push(authority.to_string());
            }
        }
    }
    hosts
}

fn start_egress_proxy(
    goal: &str,
    hosts: &[String],
    state_sandbox_dir: Option<PathBuf>,
    events_dir: &Path,
    goal_id: uuid::Uuid,
) -> Result<EgressProxy, String> {
    let allowlist = EgressAllowlist::parse(hosts)?;
    let audit_dir = match state_sandbox_dir {
        Some(d) => d.join("egress"),
        None => std::env::temp_dir().join("ta-egress"),
    };
    let mut cfg = EgressProxyConfig::new(goal, allowlist);
    cfg.audit_path = Some(audit_dir.join(format!("{goal}.jsonl")));
    let events_dir = events_dir.to_path_buf();
    cfg.on_deny = Some(std::sync::Arc::new(move |r: &EgressRecord| {
        emit_violation(
            &events_dir,
            goal_id,
            "egress_proxy",
            "connect",
            &format!("{}:{}", r.host, r.port),
            None,
            None,
            1,
            &r.reason,
        )
    }));
    EgressProxy::start(cfg).map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn emit_violation(
    events_dir: &Path,
    goal_id: uuid::Uuid,
    source: &str,
    operation: &str,
    target: &str,
    process: Option<String>,
    pid: Option<u32>,
    count: u32,
    detail: &str,
) {
    use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
    let store = FsEventStore::new(events_dir);
    if let Err(e) = store.append(&EventEnvelope::new(SessionEvent::SandboxViolation {
        goal_id,
        source: source.to_string(),
        operation: operation.to_string(),
        target: target.to_string(),
        process,
        pid,
        count,
        detail: detail.to_string(),
    })) {
        tracing::warn!(
            events_dir = %events_dir.display(),
            error = %e,
            "could not record a sandbox violation in the event store"
        );
    }
}

impl SandboxPlan {
    /// The provider-facing policy for the spawn wrapper.
    pub fn policy(&self) -> SandboxPolicy {
        let mut policy = self.profile.to_policy(self.provider.clone());
        policy.egress_proxy_port = self.egress.as_ref().map(|p| p.port());
        #[cfg(unix)]
        {
            policy.egress_unix_socket = self
                .egress_relay
                .as_ref()
                .map(|r| r.socket_path().to_path_buf());
        }
        policy.windows = self.windows_launch();
        policy
    }

    /// Windows-only launch inputs: the egress pipe, Job Object limits for the
    /// role, the ledger directory, and the folder holding the TA helper binaries.
    fn windows_launch(&self) -> ta_runtime::windows_policy::WindowsLaunch {
        let mut extra_read_dirs = Vec::new();
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            extra_read_dirs.push(dir);
        }
        ta_runtime::windows_policy::WindowsLaunch {
            #[cfg(windows)]
            egress_pipe: self.egress_pipe.as_ref().map(|r| r.pipe_name().to_string()),
            #[cfg(not(windows))]
            egress_pipe: None,
            job_limits: Some(ta_runtime::windows_policy::JobLimits::for_role(
                self.profile.role_class,
            )),
            state_dir: self.state_dir.clone(),
            extra_read_dirs,
        }
    }

    /// Watch the unified log for this goal's Seatbelt denials and record each
    /// as a `SandboxViolation` event. macOS only; `None` elsewhere or if the
    /// `log` tool cannot start (the sandbox still enforces). Start it BEFORE the
    /// spawn: `log stream` needs a moment before it sees events. Repeats of the
    /// same denial are reported once.
    pub fn start_violation_tail(
        &self,
        events_dir: &Path,
        goal_id: uuid::Uuid,
    ) -> Option<ViolationTail> {
        if !self.wraps || self.provider != SandboxProvider::MacosSandboxExec {
            return None;
        }
        let tag = ta_runtime::seatbelt::violation_tag(&goal_id.to_string());
        let events_dir = events_dir.to_path_buf();
        let seen = std::sync::Mutex::new(std::collections::HashSet::<String>::new());
        let report = move |v: Violation| {
            let key = format!("{}|{}|{}", v.process, v.operation, v.target);
            if !seen.lock().map(|mut s| s.insert(key)).unwrap_or(true) {
                return;
            }
            eprintln!(
                "[sandbox] denied: {} {} ({}[{}])",
                v.operation, v.target, v.process, v.pid
            );
            emit_violation(
                &events_dir,
                goal_id,
                "seatbelt",
                &v.operation,
                &v.target,
                Some(v.process.clone()),
                Some(v.pid),
                v.count,
                &v.explain(),
            );
        };
        match ViolationTail::start(tag, std::sync::Arc::new(report)) {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(error = %e, "sandbox denial events are unavailable");
                eprintln!("[warn] sandbox: {e}");
                None
            }
        }
    }

    /// Reduce the agent env to the allowlist (no-op unless `scrubs_env`).
    /// `inherited_is_empty` is true when the spawn already starts from a
    /// cleared baseline. Returns the names removed.
    pub fn apply_env(
        &self,
        env: &mut HashMap<String, String>,
        inherited_is_empty: bool,
        goal_id: &str,
    ) -> Vec<String> {
        if !self.scrubs_env {
            return Vec::new();
        }
        let inherited: HashMap<String, String> = if inherited_is_empty {
            HashMap::new()
        } else {
            std::env::vars().collect()
        };
        let scrub = EnvScrub {
            goal_id,
            agent_home: &self.agent_home,
            protected: &self.profile.deny_paths,
            claude_config_dir: self.claude_config_dir.as_deref(),
            mcp_endpoint: self.gateway.as_ref().map(|g| g.socket.as_path()),
            connection_token: self.gateway.as_ref().map(|g| g.token.as_str()),
        };
        let out = scrub_env(&inherited, env, &scrub);
        *env = out.env;
        if let Some(p) = &self.egress {
            env.extend(proxy_env(&p.url()));
        }
        out.removed
    }

    /// Re-run the decision after the provider could not be set up, treating the
    /// launch as having only the Job Object layer. Updates `decision` in place.
    pub fn downgrade_to_job_object(&mut self, reason: &str) {
        let placement = self.decision.gateway;
        self.provider = SandboxProvider::WindowsJobObject;
        self.decision = decide(&self.profile, &self.provider, self.scrubs_env, placement);
        self.decision.detail = format!("{reason}. {}", self.decision.detail);
    }

    /// Print the decision, warnings and the fix to the terminal.
    pub fn print(
        &self,
        removed_env: &[String],
        needs_model_key: bool,
        env: &HashMap<String, String>,
    ) {
        use ta_runtime::sandbox_launch::Outcome;
        if self.decision.outcome != Outcome::NotRequested {
            println!("{}", self.decision.banner());
        }
        for w in &self.warnings {
            eprintln!("[warn] sandbox: {w}");
        }
        if let Some(p) = &self.egress {
            println!(
                "[sandbox] egress proxy on 127.0.0.1:{} allows {} ({} host rule(s)); audit log: {}",
                p.port(),
                if self.egress_hosts.is_empty() {
                    "no hosts".to_string()
                } else {
                    self.egress_hosts.join(", ")
                },
                self.egress_hosts.len(),
                p.audit_path()
                    .map(|a| a.display().to_string())
                    .unwrap_or_else(|| "(none)".into())
            );
        }
        if !removed_env.is_empty() {
            tracing::info!(removed = ?removed_env, "scrubbed variables from the agent environment");
            println!(
                "[sandbox] environment scrubbed: {} variable(s) withheld from the agent ({})",
                removed_env.len(),
                removed_env.join(", ")
            );
        }
        if self.scrubs_env && needs_model_key {
            let has_key = [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
            ]
            .iter()
            .any(|k| env.contains_key(*k));
            if !has_key {
                eprintln!(
                    "[warn] sandbox: no ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN or \
                     CLAUDE_CODE_OAUTH_TOKEN reaches the agent, and its home is a fresh \
                     per-goal directory. Set one in your shell before `ta run`, or the agent \
                     may not be able to authenticate (a login stored in the macOS Keychain is \
                     not available inside the box)."
                );
            }
        }
    }

    /// Append the decision to the event store.
    pub fn emit(&self, events_dir: &Path, goal_id: uuid::Uuid) {
        use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
        let d = &self.decision;
        let store = FsEventStore::new(events_dir);
        if let Err(e) = store.append(&EventEnvelope::new(SessionEvent::SandboxDecision {
            goal_id,
            posture: d.posture.name().to_string(),
            role_class: d.role_class.name().to_string(),
            provider: d.provider.clone(),
            outcome: d.outcome.name().to_string(),
            required: d.required.clone(),
            enforced: d.enforced.clone(),
            missing: d.missing.clone(),
            profile_hash: d.profile_hash.clone(),
            gateway: d.gateway.name().to_string(),
            detail: d.detail.clone(),
        })) {
            tracing::warn!(
                events_dir = %events_dir.display(),
                error = %e,
                "could not record the sandbox decision in the event store"
            );
        }
    }
}

/// Point the `--mcp-config` argument at `path`. No-op if absent.
pub fn override_mcp_config_arg(args: &mut [String], path: &Path) {
    if let Some(i) = args.iter().position(|a| a == "--mcp-config") {
        if let Some(v) = args.get_mut(i + 1) {
            *v = path.display().to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_replaces_only_the_mcp_config_value() {
        let mut args: Vec<String> = [
            "--strict-mcp-config",
            "--mcp-config",
            "/p/.ta/mcp-agent.json",
            "-p",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        override_mcp_config_arg(&mut args, Path::new("/h/mcp-agent.json"));
        assert_eq!(args[2], "/h/mcp-agent.json");
        assert_eq!(args[3], "-p");
    }

    /// Regression: the sandbox settings were read from staging, which has no
    /// `.ta/`. They are read from the project root, and no experimental flag
    /// in `daemon.toml` gates them.
    #[test]
    fn sandbox_setting_comes_from_the_project_root_without_an_experimental_flag() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".ta")).unwrap();
        std::fs::write(
            root.path().join(".ta/workflow.toml"),
            "[sandbox]\nenabled = true\nallow_network = [\"api.example\"]\n",
        )
        .unwrap();
        // No daemon.toml anywhere.
        let req = project_request(root.path());
        assert!(req.enabled);
        assert_eq!(req.allow_network, vec!["api.example".to_string()]);

        let staging = root.path().join(".ta/staging/g1");
        std::fs::create_dir_all(&staging).unwrap();
        let from_staging = project_request(&staging);
        assert!(!from_staging.enabled, "staging has no .ta/workflow.toml");
    }

    #[test]
    fn security_level_maps_to_posture_and_sandbox_request() {
        for (level, posture, sandbox) in [
            ("low", SandboxPosture::Open, false),
            ("mid", SandboxPosture::Balanced, true),
            ("high", SandboxPosture::Strict, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join(".ta")).unwrap();
            std::fs::write(
                root.path().join(".ta/workflow.toml"),
                format!("[security]\nlevel = \"{level}\"\n"),
            )
            .unwrap();
            let req = project_request(root.path());
            assert_eq!(req.posture, Some(posture), "{level}");
            assert_eq!(req.security_profile_sandbox, sandbox, "{level}");
        }
    }
}
