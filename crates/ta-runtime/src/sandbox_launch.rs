// sandbox_launch.rs — Platform-neutral sandbox launch plumbing (v0.17.12.6).
//
// `sandbox.rs` turns a `SandboxPolicy` into a wrapped spawn. This module
// decides *what* that policy is and *whether* a launch may proceed:
//
// 1. A trusted policy source. The owner's `policy.toml` lives in the TA state
//    directory, outside the project tree and every staging workspace, so an
//    agent cannot edit the box it is launched into. Project config
//    (`.ta/workflow.toml`, `security.level`) may only tighten it.
// 2. A platform-neutral `SandboxProfile`, compiled per launch and hashed so
//    "what box was this agent in?" is answerable afterwards.
// 3. Capability reporting. Each provider reports which capabilities it
//    really enforces today; nothing is inferred from a provider existing.
// 4. The degraded-mode table (`decide`): posture x role class x missing
//    capability -> refuse, launch degraded, or launch unsandboxed. Never
//    silent: the result is a `SandboxDecision` the caller audits and prints.
// 5. The environment scrub allowlist.
//
// Design reference: docs/superpowers/specs/2026-10-07-os-sandbox-plan.md
// sections 4, 7, 9.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sandbox::{SandboxPolicy, SandboxProvider};

/// File name of the owner's policy inside `ProjectState::sandbox_dir()`.
pub const TRUSTED_POLICY_FILENAME: &str = "policy.toml";

// ── Posture ────────────────────────────────────────────────────────────────

/// How hard TA is when a required sandbox capability is missing. Ordered
/// `Open < Balanced < Strict`; combining two sources keeps the stricter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxPosture {
    Open,
    Balanced,
    Strict,
}

impl SandboxPosture {
    pub fn name(self) -> &'static str {
        match self {
            SandboxPosture::Open => "open",
            SandboxPosture::Balanced => "balanced",
            SandboxPosture::Strict => "strict",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "open" => Some(SandboxPosture::Open),
            "balanced" => Some(SandboxPosture::Balanced),
            "strict" => Some(SandboxPosture::Strict),
            _ => None,
        }
    }

    /// Interim mapping from `[security] level` until the rules-engine posture
    /// drives it: `low` -> open, `mid` -> balanced, `high` -> strict.
    pub fn from_security_level(level: &str) -> Option<Self> {
        match level.trim().to_ascii_lowercase().as_str() {
            "low" => Some(SandboxPosture::Open),
            "mid" => Some(SandboxPosture::Balanced),
            "high" => Some(SandboxPosture::Strict),
            _ => None,
        }
    }

    /// The stricter of two postures.
    pub fn tighten(self, other: Self) -> Self {
        self.max(other)
    }
}

// ── Role class ─────────────────────────────────────────────────────────────

/// Role class a launch is compiled for. It comes from launch data (chat mode,
/// origin, team role), never from a free-form string the agent can influence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleClass {
    /// Chief-of-staff / chat-mode sessions: the strictest class.
    Chat,
    Reviewer,
    Worker,
    /// Wiki ingestion: a cache directory and the Wayfinder host, nothing else.
    Librarian,
    Plugin,
}

/// What a role class may ever be given on top of its base box, whatever the
/// owner policy, project config or persona asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ceiling {
    pub extra_read: bool,
    pub extra_write: bool,
    pub extra_network: bool,
}

impl RoleClass {
    pub const ALL: [RoleClass; 5] = [
        RoleClass::Chat,
        RoleClass::Reviewer,
        RoleClass::Worker,
        RoleClass::Librarian,
        RoleClass::Plugin,
    ];

    pub fn name(self) -> &'static str {
        match self {
            RoleClass::Chat => "chat",
            RoleClass::Reviewer => "reviewer",
            RoleClass::Worker => "worker",
            RoleClass::Librarian => "librarian",
            RoleClass::Plugin => "plugin",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "chat" | "cos" => Some(RoleClass::Chat),
            "reviewer" => Some(RoleClass::Reviewer),
            "worker" => Some(RoleClass::Worker),
            "librarian" => Some(RoleClass::Librarian),
            "plugin" => Some(RoleClass::Plugin),
            _ => None,
        }
    }

    /// Roles that handle untrusted-origin input: a missing required
    /// capability refuses the launch under `balanced`, not just `strict`.
    pub fn is_untrusted_origin(self) -> bool {
        matches!(self, RoleClass::Chat | RoleClass::Reviewer)
    }

    /// The class for a launch. `chat` is true for chat mode and for origins
    /// `cos` and `chat`; it forces the strictest class whatever the team role,
    /// persona or owner mapping says. Otherwise the owner's `[sandbox.roles]`
    /// mapping (state dir, so the agent cannot edit it) wins, then the built-in
    /// name mapping. A launch with no team role is a worker.
    pub fn resolve(
        chat: bool,
        team_role: Option<&str>,
        owner_map: &std::collections::BTreeMap<String, RoleClass>,
    ) -> RoleClass {
        if chat {
            return RoleClass::Chat;
        }
        let Some(role) = team_role.map(|r| r.trim().to_ascii_lowercase()) else {
            return RoleClass::Worker;
        };
        if role.is_empty() {
            return RoleClass::Worker;
        }
        owner_map
            .iter()
            .find(|(name, _)| name.trim().eq_ignore_ascii_case(&role))
            .map(|(_, class)| *class)
            .unwrap_or_else(|| Self::builtin_for_role_name(&role))
    }

    /// Built-in mapping from a team role name. Names that read or review
    /// become `reviewer`, wiki roles become `librarian`, everything else is a
    /// worker. The owner can pin any name to a class in `policy.toml`.
    pub fn builtin_for_role_name(role: &str) -> RoleClass {
        let r = role.trim().to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| r.contains(w));
        if has(&[
            "review",
            "audit",
            "qa",
            "read_only",
            "readonly",
            "read-only",
        ]) {
            RoleClass::Reviewer
        } else if has(&["librarian", "wiki", "scribe", "archivist"]) {
            RoleClass::Librarian
        } else {
            RoleClass::Worker
        }
    }

    /// What this class may be given beyond its base box. Chat gets nothing
    /// (it reads project content only through MCP tools and reaches only the
    /// model API); a reviewer may read more but never write or reach more hosts.
    pub fn ceiling(self) -> Ceiling {
        match self {
            RoleClass::Chat => Ceiling {
                extra_read: false,
                extra_write: false,
                extra_network: false,
            },
            RoleClass::Reviewer => Ceiling {
                extra_read: true,
                extra_write: false,
                extra_network: false,
            },
            RoleClass::Librarian => Ceiling {
                extra_read: true,
                extra_write: false,
                extra_network: true,
            },
            RoleClass::Worker | RoleClass::Plugin => Ceiling {
                extra_read: true,
                extra_write: true,
                extra_network: true,
            },
        }
    }
}

// ── Capabilities ───────────────────────────────────────────────────────────

/// A property of the box that a provider either enforces or does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// `<project>/.ta/**` (other than this goal's staging) and the TA state dir are unreachable.
    FsDenyTaTree,
    /// Writes are confined to staging, the agent home and scratch.
    FsWriteConfined,
    /// The daemon and NATS loopback ports are unreachable.
    NetLoopbackBlocked,
    /// Outbound traffic is limited to an allowlist through the egress proxy (v0.17.12.7).
    NetEgressAllowlist,
    /// The agent cannot ptrace or open processes outside the box.
    ProcNoPtraceOut,
    /// The agent cannot ask something else to start an unsandboxed process.
    ProcNoUnsandboxedSpawn,
    /// The environment was reduced to the allowlist.
    EnvScrubbed,
    /// The agent cannot reach the daemon's sockets.
    IpcNoDaemonSocket,
    /// The MCP gateway runs outside the sandbox, reached through a per-goal endpoint.
    GatewayOutside,
}

impl Capability {
    pub const ALL: [Capability; 9] = [
        Capability::FsDenyTaTree,
        Capability::FsWriteConfined,
        Capability::NetLoopbackBlocked,
        Capability::NetEgressAllowlist,
        Capability::ProcNoPtraceOut,
        Capability::ProcNoUnsandboxedSpawn,
        Capability::EnvScrubbed,
        Capability::IpcNoDaemonSocket,
        Capability::GatewayOutside,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Capability::FsDenyTaTree => "fs.deny_ta_tree",
            Capability::FsWriteConfined => "fs.write_confined",
            Capability::NetLoopbackBlocked => "net.loopback_blocked",
            Capability::NetEgressAllowlist => "net.egress_allowlist",
            Capability::ProcNoPtraceOut => "proc.no_ptrace_out",
            Capability::ProcNoUnsandboxedSpawn => "proc.no_unsandboxed_spawn",
            Capability::EnvScrubbed => "env.scrubbed",
            Capability::IpcNoDaemonSocket => "ipc.no_daemon_socket",
            Capability::GatewayOutside => "mediation.gateway_outside",
        }
    }

    /// What the user can do when this capability is missing.
    pub fn fix_hint(self) -> &'static str {
        match self {
            Capability::FsDenyTaTree | Capability::FsWriteConfined => {
                "install an OS sandbox provider (macOS: built in; Linux: install bubblewrap or \
                 use a kernel with Landlock (5.13+); Windows: AppContainer)"
            }
            Capability::NetLoopbackBlocked => {
                "remove the \"*\" network grant; on Linux without bubblewrap also needs Landlock \
                 ABI 4 (kernel 6.7+), otherwise install bubblewrap"
            }
            Capability::NetEgressAllowlist => "needs the TA egress proxy with a provider that confines the network (macOS, or Linux \
                 with bubblewrap or Landlock ABI 4+); remove the \"*\" grant and list the hosts instead",
            Capability::ProcNoPtraceOut | Capability::ProcNoUnsandboxedSpawn => {
                "needs the hardened provider profile (Linux: bubblewrap with the ta-sandbox-exec \
                 helper; Windows: AppContainer)"
            }
            Capability::EnvScrubbed => "launch through `ta run` so the environment is scrubbed",
            Capability::IpcNoDaemonSocket => "needs a network namespace (Linux: bubblewrap) or AppContainer",
            Capability::GatewayOutside => {
                "the out-of-sandbox gateway needs a Unix socket (macOS and Linux) or a \
                 named pipe locked to the container SID (Windows); check that the gateway \
                 started (`ta doctor` reports why it did not)"
            }
        }
    }
}

/// Capabilities a role class needs in this release. `NetEgressAllowlist`,
/// `ProcNoPtraceOut`, `ProcNoUnsandboxedSpawn` and `IpcNoDaemonSocket` join
/// these sets as the hardened providers (v0.17.12.7 and later) land; they are
/// already reported per provider.
pub fn required_capabilities(role: RoleClass) -> BTreeSet<Capability> {
    use Capability::*;
    match role {
        RoleClass::Chat | RoleClass::Reviewer => [
            FsDenyTaTree,
            FsWriteConfined,
            NetLoopbackBlocked,
            EnvScrubbed,
            GatewayOutside,
        ]
        .into_iter()
        .collect(),
        RoleClass::Worker => [FsDenyTaTree, FsWriteConfined, EnvScrubbed, GatewayOutside]
            .into_iter()
            .collect(),
        // The librarian reads untrusted wiki text but is not a chat surface:
        // loopback must be closed, a missing capability degrades under balanced.
        RoleClass::Librarian => [
            FsDenyTaTree,
            FsWriteConfined,
            NetLoopbackBlocked,
            EnvScrubbed,
            GatewayOutside,
        ]
        .into_iter()
        .collect(),
        RoleClass::Plugin => [FsDenyTaTree, FsWriteConfined, EnvScrubbed]
            .into_iter()
            .collect(),
    }
}

/// Capabilities `provider` enforces today for `profile`. Reported from what
/// the generated wrapper actually does, never from the provider's name alone.
pub fn provider_capabilities(
    provider: &SandboxProvider,
    profile: &SandboxProfile,
) -> BTreeSet<Capability> {
    use Capability::*;
    let mut caps = BTreeSet::new();
    match provider {
        SandboxProvider::None | SandboxProvider::WindowsJobObject => {}
        // Seatbelt v2: loopback is always denied (even under a "*" grant), and
        // the hostname allowlist holds when the egress proxy is the only way out.
        SandboxProvider::MacosSandboxExec => {
            caps.insert(FsDenyTaTree);
            caps.insert(FsWriteConfined);
            caps.insert(NetLoopbackBlocked);
            let open_network = profile.allow_network.iter().any(|h| h == "*");
            if profile.egress_proxy && !open_network {
                caps.insert(NetEgressAllowlist);
            }
        }
        // bubblewrap + Landlock + seccomp (v0.17.12.8). The network namespace is
        // kept unless the network is fully open ("*"), so loopback (daemon, NATS)
        // is unreachable and the only way out is the bind-mounted egress relay.
        // seccomp denies ptrace and process_vm, the pid namespace hides everything
        // else, and nested user namespaces are denied, so no unsandboxed spawn.
        SandboxProvider::LinuxBwrap => {
            caps.insert(FsDenyTaTree);
            caps.insert(FsWriteConfined);
            caps.insert(ProcNoPtraceOut);
            caps.insert(ProcNoUnsandboxedSpawn);
            let open_network = profile.allow_network.iter().any(|h| h == "*");
            if !open_network {
                caps.insert(NetLoopbackBlocked);
                caps.insert(IpcNoDaemonSocket);
                if profile.egress_proxy {
                    caps.insert(NetEgressAllowlist);
                }
            }
        }
        // Landlock-only: filesystem confinement and the seccomp deny list, but no
        // network namespace. TCP is limited by Landlock only from ABI 4 (Linux
        // 6.7). Pathname Unix sockets are not Landlock-controlled, so the daemon
        // socket stays reachable and there is no pid namespace.
        SandboxProvider::LinuxLandlock => {
            caps.insert(FsDenyTaTree);
            caps.insert(FsWriteConfined);
            caps.insert(ProcNoPtraceOut);
            let open_network = profile.allow_network.iter().any(|h| h == "*");
            if !open_network && host_landlock_abi().is_some_and(|abi| abi >= 4) {
                caps.insert(NetLoopbackBlocked);
                if profile.egress_proxy {
                    caps.insert(NetEgressAllowlist);
                }
            }
        }
        // AppContainer: no ACE outside staging, loopback blocked by default (and no
        // loopback exemption is ever configured), no handles to processes outside
        // the container, no Task Scheduler/WMI. When the egress proxy is in use the
        // container is NOT granted `internetClient`: the only way out is the
        // container-SID-locked named pipe to the host relay, so the hostname
        // allowlist holds even if the relay fails (the agent then has no network).
        SandboxProvider::WindowsAppContainer => {
            caps.insert(FsDenyTaTree);
            caps.insert(FsWriteConfined);
            caps.insert(NetLoopbackBlocked);
            caps.insert(ProcNoPtraceOut);
            caps.insert(ProcNoUnsandboxedSpawn);
            caps.insert(IpcNoDaemonSocket);
            let open_network = profile.allow_network.iter().any(|h| h == "*");
            if profile.egress_proxy && !open_network {
                caps.insert(NetEgressAllowlist);
            }
        }
    }
    caps
}

/// The Landlock ABI of the running kernel (None off Linux or when unavailable).
fn host_landlock_abi() -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        crate::linux_probe::cached().landlock.abi()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub fn provider_name(provider: &SandboxProvider) -> &'static str {
    match provider {
        SandboxProvider::None => "none",
        SandboxProvider::MacosSandboxExec => "macos_sandbox_exec",
        SandboxProvider::LinuxBwrap => "linux_bwrap",
        SandboxProvider::LinuxLandlock => "linux_landlock",
        SandboxProvider::WindowsJobObject => "windows_job_object",
        SandboxProvider::WindowsAppContainer => "windows_appcontainer",
    }
}

// ── Policy sources ─────────────────────────────────────────────────────────

/// The owner's policy from the TA state directory. Every field optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct TrustedPolicy {
    /// Force the sandbox on (or, when `false`, leave the decision to posture and project config).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Floor posture. Project config can only raise it.
    #[serde(default)]
    pub posture: Option<SandboxPosture>,
    #[serde(default)]
    pub allow_read: Vec<String>,
    #[serde(default)]
    pub allow_write: Vec<String>,
    #[serde(default)]
    pub allow_network: Vec<String>,
    /// Team role name -> role class (`reviewer`, `worker`, `librarian`,
    /// `plugin`). Lives in the state dir, so an agent cannot reclassify its own
    /// role. Names not listed use the built-in mapping.
    #[serde(default)]
    pub roles: std::collections::BTreeMap<String, RoleClass>,
}

#[derive(Debug, Deserialize)]
struct TrustedPolicyFile {
    #[serde(default)]
    sandbox: TrustedPolicy,
}

impl TrustedPolicy {
    /// Parse `policy.toml` text (a `[sandbox]` table).
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str::<TrustedPolicyFile>(text)
            .map(|f| f.sandbox)
            .map_err(|e| format!("invalid sandbox policy: {e}"))
    }

    /// Load `<sandbox_dir>/policy.toml`. `Ok(None)` when absent. A malformed
    /// file is an error, never a silent fallback to a weaker policy.
    pub fn load(sandbox_dir: &Path) -> Result<Option<Self>, String> {
        let path = sandbox_dir.join(TRUSTED_POLICY_FILENAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map(Some).map_err(|e| {
                format!(
                    "{e} (file: {}). Fix or delete the file; the sandbox will not launch \
                     with a policy it cannot read.",
                    path.display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!(
                "could not read the trusted sandbox policy {}: {e}. Check its permissions.",
                path.display()
            )),
        }
    }
}

/// File name of the owner's persona pins inside `ProjectState::sandbox_dir()`.
pub const PERSONA_PINS_FILENAME: &str = "persona-pins.toml";

/// Persona name -> sha256 (hex) of the persona file the owner reviewed.
pub fn load_persona_pins(
    sandbox_dir: &Path,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    #[derive(Deserialize, Default)]
    struct Pins {
        #[serde(default)]
        pins: std::collections::BTreeMap<String, String>,
    }
    let path = sandbox_dir.join(PERSONA_PINS_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str::<Pins>(&text).map(|p| p.pins).map_err(|e| {
            format!(
                "invalid persona pins file {}: {e}. Fix or delete it, then re-run \
                 `ta sandbox pin-persona <name>`.",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(format!(
            "could not read the persona pins {}: {e}. Check its permissions.",
            path.display()
        )),
    }
}

/// Record `hash` as the reviewed hash of persona `name`.
pub fn save_persona_pin(sandbox_dir: &Path, name: &str, hash: &str) -> Result<PathBuf, String> {
    let mut pins = load_persona_pins(sandbox_dir)?;
    pins.insert(name.to_string(), hash.to_string());
    let mut text = String::from(
        "# Persona files whose [sandbox] additions the owner has reviewed (ta sandbox pin-persona).\n\
         # Written by TA; lives outside the project so an agent cannot edit it.\n[pins]\n",
    );
    for (k, v) in &pins {
        text.push_str(&format!("{k:?} = {v:?}\n"));
    }
    std::fs::create_dir_all(sandbox_dir).map_err(|e| {
        format!(
            "could not create {}: {e}. Check that the TA state directory is writable.",
            sandbox_dir.display()
        )
    })?;
    let path = sandbox_dir.join(PERSONA_PINS_FILENAME);
    std::fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(path)
}

/// What project config (read from the real project root, never from
/// staging) asks for. It can only add requirements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectRequest {
    /// `[sandbox] enabled`.
    pub enabled: bool,
    /// `[security] level` mapped to a posture, after overrides.
    pub posture: Option<SandboxPosture>,
    /// Whether the resolved security profile turns the sandbox on (mid and high).
    pub security_profile_sandbox: bool,
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    pub allow_network: Vec<String>,
}

/// Per-launch facts the profile is compiled against.
#[derive(Debug, Clone)]
pub struct LaunchContext {
    pub goal_id: String,
    pub role_class: RoleClass,
    /// True for team-session roles: project `allow_*` entries are ignored.
    pub team_role: bool,
    pub project_root: PathBuf,
    pub staging: PathBuf,
    /// The TA state dir for this project, when one exists.
    pub state_dir: Option<PathBuf>,
    /// Per-goal agent home (also the source of `TMPDIR`).
    pub agent_home: PathBuf,
    /// The `$HOME` of the user, for the credential-store deny list.
    pub user_home: Option<PathBuf>,
    /// Per-goal mediation socket, when the gateway runs outside the sandbox.
    pub mediation_socket: Option<PathBuf>,
    /// The launcher runs the per-goal egress proxy for this goal, so hostname
    /// allowlisting is enforceable where the provider can confine the network.
    pub egress_proxy: bool,
    /// The persona's `[sandbox]` table, when the launch has a persona.
    pub persona: Option<PersonaSandbox>,
}

/// A persona's `[sandbox]` additions plus the facts needed to trust them.
/// Persona files live in the project tree and earlier goals can write them, so
/// the additions count only while the file's hash matches a pin the owner made
/// in the state dir (`ta sandbox pin-persona`, stored in `persona-pins.toml`). Unpinned or drifted additions
/// are ignored with a warning; the persona can never loosen the box.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PersonaSandbox {
    pub name: String,
    /// Hosts the persona asks to reach (package registries, docs sites).
    pub allow_hosts: Vec<String>,
    /// Read-only paths the persona asks for (toolchains).
    pub allow_read: Vec<String>,
    /// sha256 (hex) of the persona file as read for this launch.
    pub file_hash: String,
    /// The owner's pin for this persona name matches `file_hash`.
    pub pinned: bool,
}

impl PersonaSandbox {
    pub fn has_additions(&self) -> bool {
        !self.allow_hosts.is_empty() || !self.allow_read.is_empty()
    }
}

// ── Profile ────────────────────────────────────────────────────────────────

/// The compiled, platform-neutral profile. Providers translate it; they never invent policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SandboxProfile {
    pub goal_id: String,
    pub posture: SandboxPosture,
    pub role_class: RoleClass,
    /// Whether any source asked for a sandbox.
    pub requested: bool,
    pub required: BTreeSet<Capability>,
    pub staging: PathBuf,
    pub agent_home: PathBuf,
    pub allow_read: Vec<PathBuf>,
    pub allow_write: Vec<PathBuf>,
    pub allow_network: Vec<String>,
    pub deny_paths: Vec<PathBuf>,
    pub unix_sockets: Vec<PathBuf>,
    /// The per-goal egress proxy is available to this launch.
    pub egress_proxy: bool,
    /// Persona additions that were applied (pinned and within the class ceiling).
    pub persona_applied: Option<String>,
    /// Entries dropped or ignored while compiling, each with the reason.
    pub warnings: Vec<String>,
}

impl SandboxProfile {
    /// Compile the profile. Trusted policy sets the floor; project config only tightens.
    pub fn compile(
        trusted: Option<&TrustedPolicy>,
        project: &ProjectRequest,
        ctx: &LaunchContext,
    ) -> SandboxProfile {
        let mut warnings = Vec::new();

        // Posture: the stricter of the owner's floor and the project's ask.
        // Chat and team-role launches are on by default: with no owner floor
        // they get `balanced`. Everything else keeps `open` (the pre-existing
        // behavior). The owner opts out with `posture = "open"` in the state-dir
        // policy; project config can only move toward `strict`, never away from
        // the owner's floor.
        let default_on = ctx.role_class == RoleClass::Chat || ctx.team_role;
        let default_floor = if default_on {
            SandboxPosture::Balanced
        } else {
            SandboxPosture::Open
        };
        let floor = trusted.and_then(|t| t.posture).unwrap_or(default_floor);
        let posture = match project.posture {
            Some(p) => floor.tighten(p),
            None => floor,
        };

        let requested = trusted.and_then(|t| t.enabled).unwrap_or(false)
            || project.enabled
            || project.security_profile_sandbox
            || posture != SandboxPosture::Open;

        let deny_paths = protected_paths(ctx);

        let mut allow_read = Vec::new();
        let mut allow_write = Vec::new();
        let mut allow_network: Vec<String> = Vec::new();

        if let Some(t) = trusted {
            allow_read.extend(t.allow_read.iter().map(PathBuf::from));
            allow_write.extend(t.allow_write.iter().map(PathBuf::from));
            allow_network.extend(t.allow_network.iter().cloned());
        }

        let has_project_allows = !project.allow_read.is_empty()
            || !project.allow_write.is_empty()
            || !project.allow_network.is_empty();
        if has_project_allows {
            if ctx.team_role {
                // The project file is agent-writable from earlier goals (CR-12).
                warnings.push(
                    "ignored allow_read/allow_write/allow_network from the project's \
                     [sandbox] table: team roles take allowances only from the trusted policy \
                     (the state-dir policy.toml), because the project file is agent-writable"
                        .to_string(),
                );
            } else {
                allow_read.extend(project.allow_read.iter().map(PathBuf::from));
                allow_write.extend(project.allow_write.iter().map(PathBuf::from));
                allow_network.extend(project.allow_network.iter().cloned());
            }
        }

        // Persona additions: only a pinned persona, only up to the class ceiling.
        let mut persona_applied = None;
        if let Some(persona) = ctx.persona.as_ref().filter(|p| p.has_additions()) {
            if !persona.pinned {
                warnings.push(format!(
                    "ignored the [sandbox] additions in persona \"{}\": its file is not pinned \
                     (or changed since it was pinned). Review it, then run \
                     `ta sandbox pin-persona {}`; a persona can never loosen the box without a pin",
                    persona.name, persona.name
                ));
            } else {
                allow_read.extend(persona.allow_read.iter().map(PathBuf::from));
                allow_network.extend(persona.allow_hosts.iter().cloned());
                persona_applied = Some(persona.file_hash.clone());
            }
        }

        // Deny wins: an allowance that reaches into a protected path is dropped.
        let ci = filesystem_is_case_insensitive();
        let keep = |kind: &str, p: PathBuf, warnings: &mut Vec<String>| -> Option<PathBuf> {
            if let Some(hit) = deny_paths.iter().find(|d| path_within(&p, d, ci)) {
                warnings.push(format!(
                    "dropped {kind} allowance {}: it lies inside protected path {}",
                    p.display(),
                    hit.display()
                ));
                None
            } else if deny_paths.iter().any(|d| path_within(d, &p, ci)) {
                warnings.push(format!(
                    "dropped {kind} allowance {}: it contains protected path(s) such as {}",
                    p.display(),
                    deny_paths
                        .iter()
                        .find(|d| path_within(d, &p, ci))
                        .map(|d| d.display().to_string())
                        .unwrap_or_default()
                ));
                None
            } else {
                Some(p)
            }
        };
        let mut allow_read: Vec<PathBuf> = allow_read
            .into_iter()
            .filter_map(|p| keep("read", p, &mut warnings))
            .collect();
        let mut allow_write: Vec<PathBuf> = allow_write
            .into_iter()
            .filter_map(|p| keep("write", p, &mut warnings))
            .collect();

        // Class ceiling: whatever the sources asked for, the class cannot get
        // more than its ceiling. Applied on every platform, before capabilities
        // are checked, so a chat or reviewer goal that was granted network has
        // the grant stripped identically on macOS, Linux and Windows.
        let ceiling = ctx.role_class.ceiling();
        let class = ctx.role_class.name();
        if !ceiling.extra_read && !allow_read.is_empty() {
            warnings.push(format!(
                "dropped {} read allowance(s) for the {class} class: it reads project content only \
                 through the MCP tools",
                allow_read.len()
            ));
            allow_read.clear();
        }
        if !ceiling.extra_write && !allow_write.is_empty() {
            warnings.push(format!(
                "dropped {} write allowance(s) for the {class} class: it writes only to its own \
                 scratch and agent home",
                allow_write.len()
            ));
            allow_write.clear();
        }
        if !ceiling.extra_network && !allow_network.is_empty() {
            warnings.push(format!(
                "dropped network grant(s) [{}] for the {class} class: it reaches the model API \
                 host only",
                allow_network.join(", ")
            ));
            allow_network.clear();
        }
        allow_network.sort();
        allow_network.dedup();

        let unix_sockets = ctx.mediation_socket.iter().cloned().collect();

        SandboxProfile {
            goal_id: ctx.goal_id.clone(),
            posture,
            role_class: ctx.role_class,
            requested,
            required: required_capabilities(ctx.role_class),
            staging: ctx.staging.clone(),
            agent_home: ctx.agent_home.clone(),
            allow_read,
            allow_write,
            allow_network,
            deny_paths,
            unix_sockets,
            egress_proxy: ctx.egress_proxy,
            persona_applied,
            warnings,
        }
    }

    /// Hex SHA-256 of the canonical JSON form. Stable for equal profiles.
    pub fn hash(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let digest = Sha256::digest(&json);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Translate into the provider-facing policy.
    pub fn to_policy(&self, provider: SandboxProvider) -> SandboxPolicy {
        SandboxPolicy {
            enabled: true,
            provider,
            allow_read: self.allow_read.clone(),
            allow_write: self.allow_write.clone(),
            allow_network: self.allow_network.clone(),
            deny_paths: self.deny_paths.clone(),
            goal_paths: vec![self.agent_home.clone()],
            unix_sockets: self.unix_sockets.clone(),
            egress_proxy_port: None,
            egress_unix_socket: None,
            windows: Default::default(),
            violation_tag: Some(crate::seatbelt::violation_tag(&self.goal_id)),
        }
    }
}

/// Credential stores, browser profiles and shell history under `$HOME` that
/// no agent needs. This is still a denylist: anything not named here (another
/// project's source tree, for instance) stays readable on macOS, so reads are
/// not described as confined there.
pub const HOME_PROTECTED: &[&str] = &[
    // Keys, tokens and cloud credentials.
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".config/gh",
    ".config/git/credentials",
    ".git-credentials",
    ".docker",
    ".kube",
    ".netrc",
    ".npmrc",
    ".yarnrc",
    ".pypirc",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".terraform.d/credentials.tfrc.json",
    ".local/share/keyrings",
    ".password-store",
    // macOS stores.
    "Library/Keychains",
    "Library/Cookies",
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Arc",
    "Library/Application Support/1Password",
    "Library/Group Containers/group.com.apple.notes",
    // Linux browser profiles and shell history.
    ".config/google-chrome",
    ".config/chromium",
    ".mozilla",
    ".bash_history",
    ".zsh_history",
    ".python_history",
    ".node_repl_history",
];

/// Paths denied for read and write regardless of any allowance.
pub fn protected_paths(ctx: &LaunchContext) -> Vec<PathBuf> {
    let mut out = vec![ctx.project_root.join(".ta"), ctx.project_root.join(".git")];
    if let Some(state) = &ctx.state_dir {
        out.push(state.clone());
    }
    if let Some(home) = &ctx.user_home {
        for rel in HOME_PROTECTED {
            out.push(home.join(rel));
        }
    }
    out
}

fn filesystem_is_case_insensitive() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// Lexical normalization: drops `.`, resolves `..` without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// True when `path` equals or lies inside `base`, comparing components
/// (never string prefixes, so `.tax` is not inside `.ta`).
pub fn path_within(path: &Path, base: &Path, case_insensitive: bool) -> bool {
    let p = normalize(path);
    let b = normalize(base);
    let mut pi = p.components();
    for bc in b.components() {
        match pi.next() {
            Some(pc) => {
                let same = if case_insensitive {
                    pc.as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&bc.as_os_str().to_string_lossy())
                } else {
                    pc == bc
                };
                if !same {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

// ── Decision ───────────────────────────────────────────────────────────────

/// What `decide` concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// No source asked for a sandbox; the launch proceeds as before.
    NotRequested,
    /// Every required capability is enforced.
    Enforced,
    /// Launching with the best available layers; some required capabilities are missing.
    Degraded,
    /// No provider layer at all; launching without a sandbox.
    Unsandboxed,
    /// The launch must not proceed.
    Refused,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::NotRequested => "not_requested",
            Outcome::Enforced => "enforced",
            Outcome::Degraded => "degraded",
            Outcome::Unsandboxed => "unsandboxed",
            Outcome::Refused => "refused",
        }
    }
}

/// Where the MCP gateway runs for this launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPlacement {
    /// Started by TA outside the sandbox on a per-goal socket; the agent runs `ta mcp-bridge`.
    OutOfSandboxSocket,
    /// The agent launches `ta serve` itself and no sandbox wraps it.
    StdioUnsandboxed,
    /// The agent launches `ta serve` itself inside the sandbox.
    StdioInSandbox,
}

impl GatewayPlacement {
    pub fn name(self) -> &'static str {
        match self {
            GatewayPlacement::OutOfSandboxSocket => "out_of_sandbox_socket",
            GatewayPlacement::StdioUnsandboxed => "stdio_unsandboxed",
            GatewayPlacement::StdioInSandbox => "stdio_in_sandbox",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SandboxDecision {
    pub posture: SandboxPosture,
    pub role_class: RoleClass,
    pub provider: String,
    pub outcome: Outcome,
    pub required: Vec<String>,
    pub enforced: Vec<String>,
    pub missing: Vec<String>,
    pub profile_hash: String,
    pub gateway: GatewayPlacement,
    pub detail: String,
}

impl SandboxDecision {
    pub fn is_refused(&self) -> bool {
        self.outcome == Outcome::Refused
    }

    /// One-line banner for `ta run` output.
    pub fn banner(&self) -> String {
        format!(
            "[sandbox] {} (posture {}, role {}, provider {}): {}",
            self.outcome.name(),
            self.posture.name(),
            self.role_class.name(),
            self.provider,
            self.detail
        )
    }
}

/// Apply the degraded-mode table (plan section 9) to a profile and a provider.
///
/// `env_scrubbed` is true when the launcher scrubs the environment for this
/// launch; `gateway` says where the MCP gateway will run.
pub fn decide(
    profile: &SandboxProfile,
    provider: &SandboxProvider,
    env_scrubbed: bool,
    gateway: GatewayPlacement,
) -> SandboxDecision {
    let mut enforced = provider_capabilities(provider, profile);
    if env_scrubbed {
        enforced.insert(Capability::EnvScrubbed);
    }
    if gateway == GatewayPlacement::OutOfSandboxSocket {
        enforced.insert(Capability::GatewayOutside);
    }
    let missing: BTreeSet<Capability> = profile.required.difference(&enforced).copied().collect();
    let names = |set: &BTreeSet<Capability>| set.iter().map(|c| c.name().to_string()).collect();
    let has_provider = *provider != SandboxProvider::None;

    let (outcome, detail) = if !profile.requested {
        (
            Outcome::NotRequested,
            "no sandbox requested (set [security] level = \"mid\", [sandbox] enabled = true, or \
             a state-dir sandbox policy to turn it on)"
                .to_string(),
        )
    } else if missing.is_empty() {
        (
            Outcome::Enforced,
            format!(
                "all {} required capabilities enforced",
                profile.required.len()
            ),
        )
    } else {
        let list = missing
            .iter()
            .map(|c| format!("{} ({})", c.name(), c.fix_hint()))
            .collect::<Vec<_>>()
            .join("; ");
        let refuse = match profile.posture {
            SandboxPosture::Strict => true,
            SandboxPosture::Balanced => profile.role_class.is_untrusted_origin(),
            SandboxPosture::Open => false,
        };
        if refuse {
            (
                Outcome::Refused,
                format!(
                    "launch refused under posture {} for role {}: missing {}. Fix the missing \
                     capability, or lower the posture in the trusted sandbox policy if you \
                     accept the risk",
                    profile.posture.name(),
                    profile.role_class.name(),
                    list
                ),
            )
        } else if has_provider {
            (
                Outcome::Degraded,
                format!(
                    "launching with the available layers; missing {}. The draft is labelled \
                     \"built without sandbox capability {}\"",
                    list,
                    missing
                        .iter()
                        .map(|c| c.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        } else {
            (
                Outcome::Unsandboxed,
                format!(
                    "no sandbox provider is available on this platform; launching without one. \
                     Missing {}",
                    list
                ),
            )
        }
    };

    SandboxDecision {
        posture: profile.posture,
        role_class: profile.role_class,
        provider: provider_name(provider).to_string(),
        outcome,
        required: names(&profile.required),
        enforced: names(&enforced),
        missing: names(&missing),
        profile_hash: profile.hash(),
        gateway,
        detail,
    }
}

impl SandboxDecision {
    /// Short badge for `ta status` and Studio, e.g. `sandbox enforced (chat, balanced)`.
    pub fn badge(&self) -> String {
        badge_from_parts(
            self.outcome.name(),
            self.posture.name(),
            self.role_class.name(),
            &self.missing,
        )
    }
}

/// The badge text from a decision's recorded fields (the `sandbox_decision`
/// event stores strings), so `ta status`, `ta sandbox explain` and Studio
/// word it identically.
pub fn badge_from_parts(
    outcome: &str,
    posture: &str,
    role_class: &str,
    missing: &[String],
) -> String {
    let missing = missing.join(", ");
    match outcome {
        "not_requested" => "sandbox off".to_string(),
        "enforced" => format!("sandbox enforced ({role_class}, {posture})"),
        "degraded" => format!("sandbox DEGRADED, missing {missing} ({role_class}, {posture})"),
        "unsandboxed" => format!("UNSANDBOXED, missing {missing} ({role_class}, {posture})"),
        "refused" => format!("sandbox REFUSED, missing {missing} ({role_class}, {posture})"),
        other => format!("sandbox {other} ({role_class}, {posture})"),
    }
}

/// How ready this machine is to run one role class under each posture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleReadiness {
    pub role: RoleClass,
    pub required: Vec<Capability>,
    pub missing: Vec<Capability>,
    /// What `decide` concludes for this class under each posture.
    pub by_posture: Vec<(SandboxPosture, Outcome)>,
}

/// A representative profile for `role` (egress proxy running, no network grant,
/// gateway outside, environment scrubbed), for the doctor matrix and
/// `ta sandbox explain <role>` when there is no goal to read a profile from.
pub fn representative_profile(role: RoleClass, posture: SandboxPosture) -> SandboxProfile {
    let ctx = LaunchContext {
        goal_id: "role-preview".to_string(),
        role_class: role,
        team_role: true,
        project_root: PathBuf::from("/project"),
        staging: PathBuf::from("/project/.ta/staging/role-preview"),
        state_dir: None,
        agent_home: PathBuf::from("/agent-home"),
        user_home: None,
        mediation_socket: Some(PathBuf::from("/agent-home/mediation.sock")),
        egress_proxy: true,
        persona: None,
    };
    let mut profile = SandboxProfile::compile(None, &ProjectRequest::default(), &ctx);
    profile.posture = posture;
    profile.requested = true;
    profile
}

/// The role-by-posture readiness table for `provider`, for `ta doctor` and
/// `ta sandbox explain`. Computed from the same capability matrix and decision
/// table a real launch uses, never from the provider's name.
pub fn readiness(provider: &SandboxProvider) -> Vec<RoleReadiness> {
    RoleClass::ALL
        .iter()
        .map(|&role| {
            let probe = representative_profile(role, SandboxPosture::Balanced);
            let mut enforced = provider_capabilities(provider, &probe);
            enforced.insert(Capability::EnvScrubbed);
            enforced.insert(Capability::GatewayOutside);
            let missing: Vec<Capability> = probe.required.difference(&enforced).copied().collect();
            let by_posture = [
                SandboxPosture::Strict,
                SandboxPosture::Balanced,
                SandboxPosture::Open,
            ]
            .into_iter()
            .map(|posture| {
                let profile = representative_profile(role, posture);
                let d = decide(
                    &profile,
                    provider,
                    true,
                    GatewayPlacement::OutOfSandboxSocket,
                );
                (posture, d.outcome)
            })
            .collect();
            RoleReadiness {
                role,
                required: probe.required.iter().copied().collect(),
                missing,
                by_posture,
            }
        })
        .collect()
}

/// The draft label shown in review for a degraded launch, or `None`.
pub fn degraded_label(decision: &SandboxDecision) -> Option<String> {
    match decision.outcome {
        Outcome::Degraded | Outcome::Unsandboxed if !decision.missing.is_empty() => Some(format!(
            "built without sandbox capability {}",
            decision.missing.join(", ")
        )),
        _ => None,
    }
}

// ── Environment scrub ──────────────────────────────────────────────────────

/// Variables never passed to a sandboxed agent, even when role config asks.
fn is_denied_env(key: &str) -> bool {
    const EXACT: &[&str] = &[
        "SSH_AUTH_SOCK",
        "GPG_AGENT_INFO",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "DOCKER_HOST",
        "KUBECONFIG",
        "TA_PROJECT_ROOT",
    ];
    if EXACT.contains(&key) || key.starts_with("AWS_") {
        return true;
    }
    // TA_* is the control plane's own channel: only the launch handoff survives.
    key.starts_with("TA_") && !TA_ALLOWED_ENV.contains(&key)
}

/// `TA_*` variables a sandboxed agent legitimately needs.
pub const TA_ALLOWED_ENV: &[&str] = &[
    "TA_GOAL_ID",
    "TA_IS_STAGING",
    "TA_MCP_ENDPOINT",
    "TA_MCP_CONNECTION_TOKEN",
    "TA_CREDENTIAL_ENDPOINT",
];

/// Inherited variables kept (model credentials and basic terminal/locale/OS needs).
fn is_allowed_inherited_env(key: &str) -> bool {
    const EXACT: &[&str] = &[
        "PATH",
        "LANG",
        "TERM",
        "COLORTERM",
        "NO_COLOR",
        "FORCE_COLOR",
        // Model credentials (broker-scoped injection replaces these in a later phase).
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_ORG_ID",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        // Windows needs these to start processes at all.
        "SystemRoot",
        "SystemDrive",
        "windir",
        "COMSPEC",
        "PATHEXT",
    ];
    EXACT.contains(&key) || key.starts_with("LC_")
}

/// Everything `scrub_env` needs to know about the box.
#[derive(Debug, Clone)]
pub struct EnvScrub<'a> {
    pub goal_id: &'a str,
    pub agent_home: &'a Path,
    pub protected: &'a [PathBuf],
    /// `CLAUDE_CONFIG_DIR` for Claude Code launches.
    pub claude_config_dir: Option<&'a Path>,
    /// Per-goal mediation endpoint and connection token, when the gateway is out of sandbox.
    pub mcp_endpoint: Option<&'a Path>,
    pub connection_token: Option<&'a str>,
}

/// Result of [`scrub_env`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubbedEnv {
    pub env: HashMap<String, String>,
    /// Names removed from the inherited or explicit environment, sorted. Names only, never values.
    pub removed: Vec<String>,
}

/// Reduce the environment to the allowlist (plan section 4.4).
///
/// `inherited` is TA's own environment; `explicit` is what the launcher and
/// role config set on purpose (role-declared extras, proxy settings). The
/// deny list beats both. `HOME`, `TMPDIR` and friends always point at the
/// per-goal agent home.
pub fn scrub_env(
    inherited: &HashMap<String, String>,
    explicit: &HashMap<String, String>,
    scrub: &EnvScrub<'_>,
) -> ScrubbedEnv {
    let ci = filesystem_is_case_insensitive();
    let mut env: HashMap<String, String> = HashMap::new();
    let mut removed: BTreeSet<String> = BTreeSet::new();

    for (k, v) in inherited {
        if is_denied_env(k) || !is_allowed_inherited_env(k) {
            removed.insert(k.clone());
        } else {
            env.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in explicit {
        if is_denied_env(k) {
            removed.insert(k.clone());
        } else {
            env.insert(k.clone(), v.clone());
        }
    }

    // PATH: rebuilt without entries inside protected paths.
    if let Some(path) = env.get("PATH").cloned() {
        let kept: Vec<PathBuf> = std::env::split_paths(&path)
            .filter(|entry| {
                // The agent home sits inside the (protected) state dir but is
                // the one part of it the agent may use, so shims staged there stay.
                path_within(entry, scrub.agent_home, ci)
                    || !scrub.protected.iter().any(|p| path_within(entry, p, ci))
            })
            .collect();
        if let Ok(joined) = std::env::join_paths(kept) {
            env.insert("PATH".into(), joined.to_string_lossy().into_owned());
        }
    }

    let home = scrub.agent_home.to_string_lossy().into_owned();
    let tmp = scrub.agent_home.join("tmp").to_string_lossy().into_owned();
    env.insert("HOME".into(), home.clone());
    env.insert("USERPROFILE".into(), home);
    for k in ["TMPDIR", "TMP", "TEMP"] {
        env.insert(k.into(), tmp.clone());
    }
    env.insert("TA_GOAL_ID".into(), scrub.goal_id.to_string());
    if let Some(dir) = scrub.claude_config_dir {
        env.insert(
            "CLAUDE_CONFIG_DIR".into(),
            dir.to_string_lossy().into_owned(),
        );
    }
    if let Some(ep) = scrub.mcp_endpoint {
        let ep = ep.to_string_lossy().into_owned();
        env.insert("TA_MCP_ENDPOINT".into(), ep.clone());
        env.insert("TA_CREDENTIAL_ENDPOINT".into(), ep);
    }
    if let Some(token) = scrub.connection_token {
        env.insert("TA_MCP_CONNECTION_TOKEN".into(), token.to_string());
    }

    ScrubbedEnv {
        env,
        removed: removed.into_iter().collect(),
    }
}

// ── Agent home ─────────────────────────────────────────────────────────────

/// Create the per-goal agent home (`<home>/`, `<home>/tmp`, and for Claude
/// Code `<home>/.claude` seeded from a TA-owned template). The user's global
/// `~/.claude/settings.json` is never read. Returns the Claude config dir.
pub fn prepare_agent_home(agent_home: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(agent_home.join("tmp"))?;
    let claude = agent_home.join(".claude");
    std::fs::create_dir_all(&claude)?;
    let settings = claude.join("settings.json");
    if !settings.exists() {
        // TA-owned template: no allow rules, so nothing widens the launch.
        std::fs::write(&settings, CLAUDE_SETTINGS_TEMPLATE)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(agent_home, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(claude)
}

const CLAUDE_SETTINGS_TEMPLATE: &str =
    "{\n  \"permissions\": {\n    \"allow\": [],\n    \"deny\": []\n  }\n}\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(role: RoleClass, team: bool) -> LaunchContext {
        LaunchContext {
            goal_id: "g1".into(),
            role_class: role,
            team_role: team,
            project_root: PathBuf::from("/work/proj"),
            staging: PathBuf::from("/work/proj/.ta/staging/g1"),
            state_dir: Some(PathBuf::from("/state/projects/p_1")),
            agent_home: PathBuf::from("/state/projects/p_1/agent-homes/g1"),
            user_home: Some(PathBuf::from("/home/u")),
            mediation_socket: None,
            egress_proxy: false,
            persona: None,
        }
    }

    fn project_open() -> ProjectRequest {
        ProjectRequest::default()
    }

    #[test]
    fn nothing_requested_means_not_requested_and_no_unsandboxed_label() {
        let p = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Worker, false));
        assert!(!p.requested);
        assert_eq!(p.posture, SandboxPosture::Open);
        let d = decide(
            &p,
            &SandboxProvider::None,
            false,
            GatewayPlacement::StdioUnsandboxed,
        );
        assert_eq!(d.outcome, Outcome::NotRequested);
        assert!(degraded_label(&d).is_none());
    }

    #[test]
    fn project_level_can_raise_posture_but_never_lower_the_owners_floor() {
        let trusted = TrustedPolicy {
            posture: Some(SandboxPosture::Strict),
            ..Default::default()
        };
        let low = ProjectRequest {
            posture: Some(SandboxPosture::Open),
            ..Default::default()
        };
        let p = SandboxProfile::compile(Some(&trusted), &low, &ctx(RoleClass::Worker, false));
        assert_eq!(p.posture, SandboxPosture::Strict);

        let high = ProjectRequest {
            posture: SandboxPosture::from_security_level("high"),
            ..Default::default()
        };
        let p = SandboxProfile::compile(None, &high, &ctx(RoleClass::Worker, false));
        assert_eq!(p.posture, SandboxPosture::Strict);
        assert!(p.requested, "a stricter posture turns the sandbox on");
    }

    #[test]
    fn mid_security_level_requests_the_sandbox() {
        let mid = ProjectRequest {
            security_profile_sandbox: true,
            posture: SandboxPosture::from_security_level("mid"),
            ..Default::default()
        };
        let p = SandboxProfile::compile(None, &mid, &ctx(RoleClass::Worker, false));
        assert!(p.requested);
        assert_eq!(p.posture, SandboxPosture::Balanced);
    }

    #[test]
    fn project_allowances_are_ignored_for_team_roles_with_a_warning() {
        let project = ProjectRequest {
            enabled: true,
            allow_read: vec!["/opt/tools".into()],
            allow_network: vec!["evil.example".into()],
            ..Default::default()
        };
        let p = SandboxProfile::compile(None, &project, &ctx(RoleClass::Worker, true));
        assert!(p.allow_read.is_empty());
        assert!(p.allow_network.is_empty());
        assert!(p.warnings.iter().any(|w| w.contains("team roles")));

        let solo = SandboxProfile::compile(None, &project, &ctx(RoleClass::Worker, false));
        assert_eq!(solo.allow_read, vec![PathBuf::from("/opt/tools")]);
        assert_eq!(solo.allow_network, vec!["evil.example".to_string()]);
    }

    #[test]
    fn trusted_allowances_apply_to_team_roles() {
        let trusted = TrustedPolicy {
            allow_network: vec!["api.anthropic.com".into()],
            allow_read: vec!["/opt/toolchain".into()],
            ..Default::default()
        };
        let p = SandboxProfile::compile(
            Some(&trusted),
            &project_open(),
            &ctx(RoleClass::Worker, true),
        );
        assert_eq!(p.allow_network, vec!["api.anthropic.com".to_string()]);
        assert_eq!(p.allow_read, vec![PathBuf::from("/opt/toolchain")]);
    }

    #[test]
    fn allowances_reaching_protected_paths_are_dropped() {
        let project = ProjectRequest {
            enabled: true,
            allow_read: vec![
                "/work/proj/.ta/broker_root.key".into(),
                "/home/u/.ssh".into(),
                "/work/proj".into(), // contains .ta
                "/opt/ok".into(),
            ],
            allow_write: vec!["/state/projects/p_1/rules".into()],
            ..Default::default()
        };
        let p = SandboxProfile::compile(None, &project, &ctx(RoleClass::Worker, false));
        assert_eq!(p.allow_read, vec![PathBuf::from("/opt/ok")]);
        assert!(p.allow_write.is_empty());
        assert_eq!(p.warnings.len(), 4);
    }

    #[test]
    fn path_within_compares_components_not_prefixes() {
        assert!(path_within(
            Path::new("/a/.ta/x"),
            Path::new("/a/.ta"),
            false
        ));
        assert!(!path_within(
            Path::new("/a/.tax"),
            Path::new("/a/.ta"),
            false
        ));
        assert!(path_within(
            Path::new("/a/b/../.ta/x"),
            Path::new("/a/.ta"),
            false
        ));
        assert!(path_within(
            Path::new("/a/.TA/x"),
            Path::new("/a/.ta"),
            true
        ));
        assert!(!path_within(
            Path::new("/a/.TA/x"),
            Path::new("/a/.ta"),
            false
        ));
        assert!(!path_within(Path::new("/a"), Path::new("/a/.ta"), false));
    }

    #[test]
    fn profile_hash_is_stable_and_changes_with_content() {
        let a = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Worker, false));
        let b = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Worker, false));
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.hash().len(), 64);
        let c = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Chat, false));
        assert_ne!(a.hash(), c.hash());
    }

    fn requested(posture: SandboxPosture, role: RoleClass) -> SandboxProfile {
        let trusted = TrustedPolicy {
            enabled: Some(true),
            posture: Some(posture),
            ..Default::default()
        };
        SandboxProfile::compile(Some(&trusted), &project_open(), &ctx(role, true))
    }

    #[test]
    fn degraded_mode_table_for_every_posture_and_role() {
        let none = SandboxProvider::None;
        for (posture, role, want) in [
            (SandboxPosture::Strict, RoleClass::Chat, Outcome::Refused),
            (SandboxPosture::Strict, RoleClass::Worker, Outcome::Refused),
            (SandboxPosture::Strict, RoleClass::Plugin, Outcome::Refused),
            (SandboxPosture::Balanced, RoleClass::Chat, Outcome::Refused),
            (
                SandboxPosture::Balanced,
                RoleClass::Reviewer,
                Outcome::Refused,
            ),
            (
                SandboxPosture::Balanced,
                RoleClass::Worker,
                Outcome::Unsandboxed,
            ),
            (
                SandboxPosture::Balanced,
                RoleClass::Plugin,
                Outcome::Unsandboxed,
            ),
            (SandboxPosture::Open, RoleClass::Chat, Outcome::Unsandboxed),
            (
                SandboxPosture::Open,
                RoleClass::Worker,
                Outcome::Unsandboxed,
            ),
        ] {
            let d = decide(
                &requested(posture, role),
                &none,
                true,
                GatewayPlacement::StdioUnsandboxed,
            );
            assert_eq!(d.outcome, want, "{posture:?} {role:?}");
            if want == Outcome::Refused {
                assert!(d.detail.contains("fs.deny_ta_tree"), "{}", d.detail);
            }
        }
    }

    #[test]
    fn available_provider_with_gaps_is_degraded_not_unsandboxed() {
        let d = decide(
            &requested(SandboxPosture::Balanced, RoleClass::Worker),
            &SandboxProvider::MacosSandboxExec,
            true,
            GatewayPlacement::StdioInSandbox,
        );
        assert_eq!(d.outcome, Outcome::Degraded);
        assert_eq!(d.missing, vec!["mediation.gateway_outside".to_string()]);
        assert_eq!(
            degraded_label(&d).as_deref(),
            Some("built without sandbox capability mediation.gateway_outside")
        );
    }

    #[test]
    fn full_enforcement_with_out_of_sandbox_gateway() {
        let d = decide(
            &requested(SandboxPosture::Strict, RoleClass::Worker),
            &SandboxProvider::LinuxBwrap,
            true,
            GatewayPlacement::OutOfSandboxSocket,
        );
        assert_eq!(d.outcome, Outcome::Enforced);
        assert!(d.missing.is_empty());
        assert!(degraded_label(&d).is_none());
    }

    #[test]
    fn open_network_grant_means_loopback_is_not_counted_as_blocked_on_linux() {
        let make = |hosts: &[&str]| {
            let trusted = TrustedPolicy {
                enabled: Some(true),
                posture: Some(SandboxPosture::Balanced),
                allow_network: hosts.iter().map(|h| h.to_string()).collect(),
                ..Default::default()
            };
            SandboxProfile::compile(
                Some(&trusted),
                &project_open(),
                &ctx(RoleClass::Worker, true),
            )
        };
        // Named hosts: the network namespace stays, the egress relay is the only way out.
        let named = make(&["api.anthropic.com"]);
        let caps = provider_capabilities(&SandboxProvider::LinuxBwrap, &named);
        assert!(caps.contains(&Capability::NetLoopbackBlocked));
        assert!(caps.contains(&Capability::IpcNoDaemonSocket));
        assert!(caps.contains(&Capability::ProcNoPtraceOut));
        assert!(caps.contains(&Capability::ProcNoUnsandboxedSpawn));
        assert!(
            provider_capabilities(&SandboxProvider::MacosSandboxExec, &named)
                .contains(&Capability::NetLoopbackBlocked)
        );
        // "*": the host network namespace is shared, so loopback is open.
        let open = make(&["*"]);
        let caps = provider_capabilities(&SandboxProvider::LinuxBwrap, &open);
        assert!(!caps.contains(&Capability::NetLoopbackBlocked));
        assert!(!caps.contains(&Capability::NetEgressAllowlist));
        // The chat and reviewer classes never keep the "*" grant (class ceiling),
        // so for them loopback stays blocked on Linux too.
        let trusted = TrustedPolicy {
            allow_network: vec!["*".into()],
            ..Default::default()
        };
        let chat =
            SandboxProfile::compile(Some(&trusted), &project_open(), &ctx(RoleClass::Chat, true));
        assert!(chat.allow_network.is_empty());
    }

    #[test]
    fn landlock_only_never_claims_namespace_properties() {
        let trusted = TrustedPolicy {
            enabled: Some(true),
            posture: Some(SandboxPosture::Balanced),
            allow_network: vec!["api.anthropic.com".into()],
            ..Default::default()
        };
        let p =
            SandboxProfile::compile(Some(&trusted), &project_open(), &ctx(RoleClass::Chat, true));
        let caps = provider_capabilities(&SandboxProvider::LinuxLandlock, &p);
        assert!(caps.contains(&Capability::FsDenyTaTree));
        assert!(caps.contains(&Capability::FsWriteConfined));
        assert!(caps.contains(&Capability::ProcNoPtraceOut));
        // No pid namespace, and pathname Unix sockets are outside Landlock.
        assert!(!caps.contains(&Capability::ProcNoUnsandboxedSpawn));
        assert!(!caps.contains(&Capability::IpcNoDaemonSocket));
        assert_eq!(
            provider_name(&SandboxProvider::LinuxLandlock),
            "linux_landlock"
        );
    }

    #[test]
    fn macos_reports_egress_allowlist_only_with_a_proxy_and_no_star() {
        let trusted = |hosts: &[&str]| TrustedPolicy {
            enabled: Some(true),
            posture: Some(SandboxPosture::Balanced),
            allow_network: hosts.iter().map(|h| h.to_string()).collect(),
            ..Default::default()
        };
        let mac = SandboxProvider::MacosSandboxExec;
        let mut with_proxy = ctx(RoleClass::Worker, true);
        with_proxy.egress_proxy = true;

        let listed = SandboxProfile::compile(
            Some(&trusted(&["api.anthropic.com"])),
            &project_open(),
            &with_proxy,
        );
        assert!(provider_capabilities(&mac, &listed).contains(&Capability::NetEgressAllowlist));

        let star = SandboxProfile::compile(Some(&trusted(&["*"])), &project_open(), &with_proxy);
        let caps = provider_capabilities(&mac, &star);
        assert!(!caps.contains(&Capability::NetEgressAllowlist));
        assert!(caps.contains(&Capability::NetLoopbackBlocked));

        let no_proxy = SandboxProfile::compile(
            Some(&trusted(&["api.anthropic.com"])),
            &project_open(),
            &ctx(RoleClass::Worker, true),
        );
        assert!(!provider_capabilities(&mac, &no_proxy).contains(&Capability::NetEgressAllowlist));
    }

    #[test]
    fn job_object_only_enforces_no_filesystem_capability() {
        let p = requested(SandboxPosture::Balanced, RoleClass::Worker);
        assert!(provider_capabilities(&SandboxProvider::WindowsJobObject, &p).is_empty());
        let caps = provider_capabilities(&SandboxProvider::WindowsAppContainer, &p);
        assert!(caps.contains(&Capability::FsDenyTaTree));
        assert!(caps.contains(&Capability::ProcNoPtraceOut));
    }

    #[test]
    fn appcontainer_reports_egress_allowlist_only_with_the_proxy_and_no_star() {
        let trusted = |hosts: &[&str]| TrustedPolicy {
            enabled: Some(true),
            posture: Some(SandboxPosture::Balanced),
            allow_network: hosts.iter().map(|h| h.to_string()).collect(),
            ..Default::default()
        };
        let ac = SandboxProvider::WindowsAppContainer;
        let mut with_proxy = ctx(RoleClass::Worker, true);
        with_proxy.egress_proxy = true;

        let listed = SandboxProfile::compile(
            Some(&trusted(&["api.anthropic.com"])),
            &project_open(),
            &with_proxy,
        );
        assert!(provider_capabilities(&ac, &listed).contains(&Capability::NetEgressAllowlist));

        let star = SandboxProfile::compile(Some(&trusted(&["*"])), &project_open(), &with_proxy);
        let caps = provider_capabilities(&ac, &star);
        assert!(!caps.contains(&Capability::NetEgressAllowlist));
        assert!(caps.contains(&Capability::NetLoopbackBlocked));

        let no_proxy = SandboxProfile::compile(
            Some(&trusted(&["api.anthropic.com"])),
            &project_open(),
            &ctx(RoleClass::Worker, true),
        );
        assert!(!provider_capabilities(&ac, &no_proxy).contains(&Capability::NetEgressAllowlist));
    }

    #[test]
    fn trusted_policy_parses_and_rejects_garbage() {
        let t = TrustedPolicy::parse(
            "[sandbox]\nenabled = true\nposture = \"balanced\"\nallow_network = [\"a.example\"]\n",
        )
        .unwrap();
        assert_eq!(t.enabled, Some(true));
        assert_eq!(t.posture, Some(SandboxPosture::Balanced));
        assert!(TrustedPolicy::parse("[sandbox]\nposture = \"loose\"\n").is_err());
    }

    #[test]
    fn trusted_policy_load_reports_missing_ok_and_malformed_as_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(TrustedPolicy::load(dir.path()).unwrap(), None);
        std::fs::write(dir.path().join(TRUSTED_POLICY_FILENAME), "[sandbox\n").unwrap();
        let err = TrustedPolicy::load(dir.path()).unwrap_err();
        assert!(
            err.contains("policy.toml") && err.contains("will not launch"),
            "{err}"
        );
    }

    fn scrub_ctx<'a>(protected: &'a [PathBuf], home: &'a Path) -> EnvScrub<'a> {
        EnvScrub {
            goal_id: "g1",
            agent_home: home,
            protected,
            claude_config_dir: None,
            mcp_endpoint: None,
            connection_token: None,
        }
    }

    /// A `PATH` value built with the platform separator (`:` on Unix, `;` on
    /// Windows), so fixtures and expectations match what `split_paths` reads.
    fn native_path_list(entries: &[&str]) -> String {
        std::env::join_paths(entries.iter().map(PathBuf::from))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn scrub_drops_secrets_and_control_plane_variables() {
        let inherited: HashMap<String, String> = [
            ("PATH", "/usr/bin"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
            ("AWS_SECRET_ACCESS_KEY", "x"),
            ("GITHUB_TOKEN", "t"),
            ("GH_TOKEN", "t"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/x"),
            ("TA_WHITEBOARD_TOKEN", "secret"),
            ("TA_PROJECT_ROOT", "/work/proj"),
            ("ANTHROPIC_API_KEY", "sk-model"),
            ("LC_ALL", "en_US.UTF-8"),
            ("RANDOM_THING", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let explicit: HashMap<String, String> = [("GITHUB_TOKEN", "again"), ("MY_EXTRA", "ok")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let home = PathBuf::from("/state/agent-homes/g1");
        let out = scrub_env(&inherited, &explicit, &scrub_ctx(&[], &home));
        for gone in [
            "SSH_AUTH_SOCK",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "DBUS_SESSION_BUS_ADDRESS",
            "TA_WHITEBOARD_TOKEN",
            "TA_PROJECT_ROOT",
            "RANDOM_THING",
        ] {
            assert!(!out.env.contains_key(gone), "{gone} must be scrubbed");
            assert!(
                out.removed.contains(&gone.to_string()),
                "{gone} listed as removed"
            );
        }
        assert_eq!(out.env["ANTHROPIC_API_KEY"], "sk-model");
        assert_eq!(out.env["LC_ALL"], "en_US.UTF-8");
        assert_eq!(out.env["MY_EXTRA"], "ok", "role-declared extras survive");
        assert_eq!(out.env["HOME"], "/state/agent-homes/g1");
        assert_eq!(
            out.env["TMPDIR"],
            Path::new("/state/agent-homes/g1")
                .join("tmp")
                .to_string_lossy()
        );
        assert_eq!(out.env["TA_GOAL_ID"], "g1");
        // Removed lists hold names only, never values.
        assert!(!out.removed.iter().any(|n| n.contains("secret")));
    }

    #[test]
    fn scrub_sets_the_mediation_handoff_and_filters_protected_path_entries() {
        let inherited: HashMap<String, String> = [(
            "PATH".to_string(),
            native_path_list(&["/usr/bin", "/work/proj/.ta/bin", "/opt/x"]),
        )]
        .into_iter()
        .collect();
        let protected = vec![PathBuf::from("/work/proj/.ta")];
        let home = PathBuf::from("/h");
        let sock = PathBuf::from("/tmp/ta-1/mcp.sock");
        let scrub = EnvScrub {
            mcp_endpoint: Some(&sock),
            connection_token: Some("tok"),
            claude_config_dir: Some(Path::new("/h/.claude")),
            ..scrub_ctx(&protected, &home)
        };
        let out = scrub_env(&inherited, &HashMap::new(), &scrub);
        assert_eq!(
            out.env["PATH"],
            native_path_list(&["/usr/bin", "/opt/x"]),
            "the protected .ta/bin entry is dropped, the others keep their order"
        );

        // A shim dir inside the agent home survives even when the home lies
        // under a protected path.
        let inherited2: HashMap<String, String> = [(
            "PATH".to_string(),
            native_path_list(&[
                "/usr/bin",
                "/state/p/agent-homes/g1/.ta/bin",
                "/state/p/rules",
            ]),
        )]
        .into_iter()
        .collect();
        let protected2 = vec![PathBuf::from("/state/p")];
        let home2 = PathBuf::from("/state/p/agent-homes/g1");
        let out2 = scrub_env(
            &inherited2,
            &HashMap::new(),
            &scrub_ctx(&protected2, &home2),
        );
        assert_eq!(
            out2.env["PATH"],
            native_path_list(&["/usr/bin", "/state/p/agent-homes/g1/.ta/bin"])
        );
        assert_eq!(out.env["TA_MCP_ENDPOINT"], "/tmp/ta-1/mcp.sock");
        assert_eq!(out.env["TA_CREDENTIAL_ENDPOINT"], "/tmp/ta-1/mcp.sock");
        assert_eq!(out.env["TA_MCP_CONNECTION_TOKEN"], "tok");
        assert_eq!(out.env["CLAUDE_CONFIG_DIR"], "/h/.claude");
    }

    #[test]
    fn agent_home_is_private_and_seeded_without_reading_the_users_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent-homes/g1");
        let claude = prepare_agent_home(&home).unwrap();
        assert_eq!(claude, home.join(".claude"));
        assert!(home.join("tmp").is_dir());
        let settings = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        assert!(settings.contains("\"allow\": []"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn to_policy_carries_denies_goal_paths_and_socket() {
        let mut c = ctx(RoleClass::Worker, false);
        c.mediation_socket = Some(PathBuf::from("/tmp/ta-1/mcp.sock"));
        let p = SandboxProfile::compile(None, &project_open(), &c);
        let policy = p.to_policy(SandboxProvider::MacosSandboxExec);
        assert!(policy.enabled);
        assert!(policy.deny_paths.contains(&PathBuf::from("/work/proj/.ta")));
        assert_eq!(policy.goal_paths, vec![c.agent_home.clone()]);
        assert_eq!(
            policy.unix_sockets,
            vec![PathBuf::from("/tmp/ta-1/mcp.sock")]
        );
    }

    #[test]
    fn banner_names_outcome_posture_and_provider() {
        let d = decide(
            &requested(SandboxPosture::Strict, RoleClass::Chat),
            &SandboxProvider::None,
            true,
            GatewayPlacement::StdioUnsandboxed,
        );
        let b = d.banner();
        assert!(
            b.contains("refused")
                && b.contains("strict")
                && b.contains("chat")
                && b.contains("none")
        );
    }

    /// Real Seatbelt run: the protected `.ta` content is unreadable, the
    /// staging copy inside it stays writable, the agent home is usable, and a
    /// granted mediation socket is reachable. Skips where sandbox-exec is absent.
    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_denies_ta_tree_but_keeps_staging_home_and_socket() {
        use crate::{SpawnRequest, StdinMode, StdoutMode};
        if SandboxPolicy::detect_provider() != SandboxProvider::MacosSandboxExec {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let root_path = std::fs::canonicalize(root.path()).unwrap();
        let project = root_path.join("proj");
        let staging = project.join(".ta/staging/g1");
        let home = root_path.join("state/agent-homes/g1");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(home.join("tmp")).unwrap();
        std::fs::write(project.join(".ta/secret.key"), "TOPSECRET").unwrap();
        let sock = root_path.join("mcp.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

        let ctx = LaunchContext {
            goal_id: "g1".into(),
            role_class: RoleClass::Worker,
            team_role: false,
            project_root: project.clone(),
            staging: staging.clone(),
            state_dir: Some(root_path.join("state")),
            agent_home: home.clone(),
            user_home: None,
            mediation_socket: Some(sock.clone()),
            egress_proxy: false,
            persona: None,
        };
        let project_req = ProjectRequest {
            enabled: true,
            ..Default::default()
        };
        let profile = SandboxProfile::compile(None, &project_req, &ctx);
        let policy = profile.to_policy(SandboxProvider::MacosSandboxExec);
        let script = format!(
            "cat '{secret}' >/dev/null 2>&1 && echo LEAK; \
             echo ok > '{staging}/w' && echo STAGING_OK; \
             echo ok > '{home}/tmp/w' && echo HOME_OK; \
             test -S '{sock}' && echo SOCKET_VISIBLE",
            secret = project.join(".ta/secret.key").display(),
            staging = staging.display(),
            home = home.display(),
            sock = sock.display()
        );
        let req = SpawnRequest {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script],
            env: HashMap::new(),
            working_dir: staging.clone(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Piped,
            clear_env: false,
        };
        let wrapped = policy.apply(req);
        let out = std::process::Command::new(&wrapped.command)
            .args(&wrapped.args)
            .current_dir(&wrapped.working_dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = format!(
            "{} [status {:?}] profile: {}",
            String::from_utf8_lossy(&out.stderr),
            out.status,
            wrapped.args.get(1).cloned().unwrap_or_default()
        );
        assert!(
            !stdout.contains("LEAK"),
            "protected .ta content was readable: {stdout} {stderr}"
        );
        assert!(
            stdout.contains("STAGING_OK"),
            "staging not writable: {stdout} {stderr}"
        );
        assert!(
            stdout.contains("HOME_OK"),
            "agent home not writable: {stdout} {stderr}"
        );
        assert!(
            stdout.contains("SOCKET_VISIBLE"),
            "socket not reachable: {stdout} {stderr}"
        );
    }

    // ── v0.17.12.10: per-role profiles and default-on ──────────────────────

    #[test]
    fn chat_and_team_roles_are_on_by_default_under_balanced() {
        let chat = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Chat, false));
        assert!(chat.requested);
        assert_eq!(chat.posture, SandboxPosture::Balanced);
        let team = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Worker, true));
        assert!(team.requested);
        assert_eq!(team.posture, SandboxPosture::Balanced);
        // A plain solo worker keeps the pre-existing behavior.
        let solo = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Worker, false));
        assert!(!solo.requested);
    }

    #[test]
    fn only_the_owner_can_turn_default_on_off() {
        let owner_open = TrustedPolicy {
            posture: Some(SandboxPosture::Open),
            ..Default::default()
        };
        let p = SandboxProfile::compile(
            Some(&owner_open),
            &project_open(),
            &ctx(RoleClass::Chat, false),
        );
        assert_eq!(p.posture, SandboxPosture::Open);
        assert!(!p.requested);
        // Project config asking for open changes nothing.
        let project_open_ask = ProjectRequest {
            posture: Some(SandboxPosture::Open),
            ..Default::default()
        };
        let p = SandboxProfile::compile(None, &project_open_ask, &ctx(RoleClass::Chat, false));
        assert_eq!(p.posture, SandboxPosture::Balanced);
        assert!(p.requested);
    }

    #[test]
    fn chat_without_capabilities_is_refused_in_every_posture_but_open() {
        for posture in [SandboxPosture::Balanced, SandboxPosture::Strict] {
            let mut p =
                SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Chat, false));
            p.posture = posture;
            let d = decide(
                &p,
                &SandboxProvider::None,
                true,
                GatewayPlacement::StdioUnsandboxed,
            );
            assert!(d.is_refused(), "{posture:?}: {}", d.detail);
        }
        let mut p = SandboxProfile::compile(None, &project_open(), &ctx(RoleClass::Chat, false));
        p.posture = SandboxPosture::Open;
        let d = decide(
            &p,
            &SandboxProvider::None,
            true,
            GatewayPlacement::StdioUnsandboxed,
        );
        assert!(!d.is_refused());
    }

    #[test]
    fn origin_forces_the_chat_class_over_team_role_and_owner_mapping() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("cos".to_string(), RoleClass::Worker);
        assert_eq!(
            RoleClass::resolve(true, Some("cos"), &map),
            RoleClass::Chat,
            "a chat or cos origin wins whatever the role is mapped to"
        );
        assert_eq!(RoleClass::resolve(false, None, &map), RoleClass::Worker);
    }

    #[test]
    fn team_role_names_map_to_classes_and_the_owner_can_override() {
        let none = std::collections::BTreeMap::new();
        assert_eq!(
            RoleClass::resolve(false, Some("Code-Reviewer"), &none),
            RoleClass::Reviewer
        );
        assert_eq!(
            RoleClass::resolve(false, Some("wiki-librarian"), &none),
            RoleClass::Librarian
        );
        assert_eq!(
            RoleClass::resolve(false, Some("implementer"), &none),
            RoleClass::Worker
        );
        let mut map = std::collections::BTreeMap::new();
        map.insert("Implementer".to_string(), RoleClass::Reviewer);
        assert_eq!(
            RoleClass::resolve(false, Some("implementer"), &map),
            RoleClass::Reviewer
        );
    }

    #[test]
    fn owner_policy_parses_role_mapping() {
        let t = TrustedPolicy::parse(
            "[sandbox]\nposture = \"balanced\"\n[sandbox.roles]\nqa-bot = \"reviewer\"\n",
        )
        .unwrap();
        assert_eq!(t.roles.get("qa-bot"), Some(&RoleClass::Reviewer));
        assert!(TrustedPolicy::parse("[sandbox.roles]\nx = \"root\"\n").is_err());
    }

    #[test]
    fn chat_ceiling_strips_every_grant_including_open_network_on_all_platforms() {
        let trusted = TrustedPolicy {
            allow_read: vec!["/opt/tools".into()],
            allow_write: vec!["/opt/out".into()],
            allow_network: vec!["*".into(), "crates.io".into()],
            ..Default::default()
        };
        let p = SandboxProfile::compile(
            Some(&trusted),
            &project_open(),
            &ctx(RoleClass::Chat, false),
        );
        assert!(p.allow_read.is_empty() && p.allow_write.is_empty());
        assert!(p.allow_network.is_empty(), "no \"*\", no extra hosts");
        assert!(p.warnings.iter().any(|w| w.contains("network grant")));
        // With the grant gone, Linux bubblewrap keeps loopback blocked for chat.
        let mut p = p;
        p.egress_proxy = true;
        let caps = provider_capabilities(&SandboxProvider::LinuxBwrap, &p);
        assert!(caps.contains(&Capability::NetLoopbackBlocked));
    }

    #[test]
    fn reviewer_may_read_more_but_never_write_or_reach_more_hosts() {
        let trusted = TrustedPolicy {
            allow_read: vec!["/opt/tools".into()],
            allow_write: vec!["/opt/out".into()],
            allow_network: vec!["docs.rs".into()],
            ..Default::default()
        };
        let p = SandboxProfile::compile(
            Some(&trusted),
            &project_open(),
            &ctx(RoleClass::Reviewer, true),
        );
        assert_eq!(p.allow_read, vec![PathBuf::from("/opt/tools")]);
        assert!(p.allow_write.is_empty());
        assert!(p.allow_network.is_empty());
        let w = SandboxProfile::compile(
            Some(&trusted),
            &project_open(),
            &ctx(RoleClass::Worker, true),
        );
        assert_eq!(w.allow_network, vec!["docs.rs".to_string()]);
        assert_eq!(w.allow_write, vec![PathBuf::from("/opt/out")]);
        let l = SandboxProfile::compile(
            Some(&trusted),
            &project_open(),
            &ctx(RoleClass::Librarian, true),
        );
        assert!(l.allow_write.is_empty());
        assert_eq!(l.allow_network, vec!["docs.rs".to_string()]);
    }

    fn persona(pinned: bool) -> PersonaSandbox {
        PersonaSandbox {
            name: "analyst".into(),
            allow_hosts: vec!["pypi.org".into()],
            allow_read: vec!["/opt/py".into()],
            file_hash: "h1".into(),
            pinned,
        }
    }

    #[test]
    fn unpinned_persona_additions_are_ignored_with_a_warning() {
        let mut c = ctx(RoleClass::Worker, true);
        c.persona = Some(persona(false));
        let p = SandboxProfile::compile(None, &project_open(), &c);
        assert!(p.allow_network.is_empty() && p.allow_read.is_empty());
        assert!(p.persona_applied.is_none());
        assert!(p.warnings.iter().any(|w| w.contains("pin-persona")));
    }

    #[test]
    fn pinned_persona_additions_apply_within_the_class_ceiling() {
        let mut c = ctx(RoleClass::Worker, true);
        c.persona = Some(persona(true));
        let p = SandboxProfile::compile(None, &project_open(), &c);
        assert_eq!(p.allow_network, vec!["pypi.org".to_string()]);
        assert_eq!(p.allow_read, vec![PathBuf::from("/opt/py")]);
        assert_eq!(p.persona_applied.as_deref(), Some("h1"));

        let mut c = ctx(RoleClass::Chat, false);
        c.persona = Some(persona(true));
        let p = SandboxProfile::compile(None, &project_open(), &c);
        assert!(p.allow_network.is_empty() && p.allow_read.is_empty());
    }

    #[test]
    fn persona_read_into_a_protected_path_is_dropped() {
        let mut c = ctx(RoleClass::Worker, true);
        let mut per = persona(true);
        per.allow_read = vec!["/state/projects/p_1/sandbox".into()];
        c.persona = Some(per);
        let p = SandboxProfile::compile(None, &project_open(), &c);
        assert!(p.allow_read.is_empty());
        assert!(p.warnings.iter().any(|w| w.contains("protected path")));
    }

    #[test]
    fn persona_pins_round_trip_and_reject_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_persona_pins(dir.path()).unwrap().is_empty());
        save_persona_pin(dir.path(), "analyst", "abc").unwrap();
        save_persona_pin(dir.path(), "chief-of-staff", "def").unwrap();
        save_persona_pin(dir.path(), "analyst", "abd").unwrap();
        let pins = load_persona_pins(dir.path()).unwrap();
        assert_eq!(pins.get("analyst").map(String::as_str), Some("abd"));
        assert_eq!(pins.len(), 2);
        std::fs::write(dir.path().join(PERSONA_PINS_FILENAME), "[pins\n").unwrap();
        assert!(load_persona_pins(dir.path()).is_err());
    }

    #[test]
    fn librarian_is_not_an_untrusted_origin_class_but_needs_loopback_closed() {
        assert!(!RoleClass::Librarian.is_untrusted_origin());
        assert!(
            required_capabilities(RoleClass::Librarian).contains(&Capability::NetLoopbackBlocked)
        );
    }

    #[test]
    fn readiness_matrix_lists_every_class_and_refuses_chat_without_a_provider() {
        let rows = readiness(&SandboxProvider::None);
        assert_eq!(rows.len(), RoleClass::ALL.len());
        let chat = rows.iter().find(|r| r.role == RoleClass::Chat).unwrap();
        assert!(!chat.missing.is_empty());
        let outcome = |r: &RoleReadiness, p| r.by_posture.iter().find(|(x, _)| *x == p).unwrap().1;
        assert_eq!(outcome(chat, SandboxPosture::Balanced), Outcome::Refused);
        assert_eq!(outcome(chat, SandboxPosture::Strict), Outcome::Refused);
        assert_ne!(outcome(chat, SandboxPosture::Open), Outcome::Refused);
        let worker = rows.iter().find(|r| r.role == RoleClass::Worker).unwrap();
        assert_ne!(outcome(worker, SandboxPosture::Balanced), Outcome::Refused);
    }

    #[test]
    fn badge_names_the_outcome_and_the_missing_capability() {
        let p = representative_profile(RoleClass::Chat, SandboxPosture::Balanced);
        let d = decide(
            &p,
            &SandboxProvider::None,
            true,
            GatewayPlacement::StdioUnsandboxed,
        );
        assert!(d.badge().starts_with("sandbox REFUSED"), "{}", d.badge());
        assert!(d.badge().contains("fs.deny_ta_tree"));
    }
}
