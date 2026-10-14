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
/// team role), never from a free-form string the agent can influence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoleClass {
    /// Chief-of-staff / chat-mode sessions: the strictest class.
    Chat,
    Reviewer,
    Worker,
    Plugin,
}

impl RoleClass {
    pub fn name(self) -> &'static str {
        match self {
            RoleClass::Chat => "chat",
            RoleClass::Reviewer => "reviewer",
            RoleClass::Worker => "worker",
            RoleClass::Plugin => "plugin",
        }
    }

    /// Roles that handle untrusted-origin input: a missing required
    /// capability refuses the launch under `balanced`, not just `strict`.
    pub fn is_untrusted_origin(self) -> bool {
        matches!(self, RoleClass::Chat | RoleClass::Reviewer)
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
                "install an OS sandbox provider (macOS: built in; Linux: install bubblewrap; \
                 Windows: AppContainer)"
            }
            Capability::NetLoopbackBlocked => {
                "remove allow_network entries (a granted network opens loopback to the daemon \
                 until the egress proxy ships)"
            }
            Capability::NetEgressAllowlist => "needs the TA egress proxy (not yet shipped)",
            Capability::ProcNoPtraceOut | Capability::ProcNoUnsandboxedSpawn => {
                "needs the hardened provider profile (not yet shipped for this platform)"
            }
            Capability::EnvScrubbed => "launch through `ta run` so the environment is scrubbed",
            Capability::IpcNoDaemonSocket => "needs a network namespace or AppContainer",
            Capability::GatewayOutside => {
                "the out-of-sandbox gateway needs a Unix socket (macOS and Linux); \
                 Windows named-pipe support is not yet shipped"
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
    let no_network_granted = profile.allow_network.is_empty();
    let mut caps = BTreeSet::new();
    match provider {
        SandboxProvider::None | SandboxProvider::WindowsJobObject => {}
        // Seatbelt v1 and bwrap v1: filesystem confinement, and loopback is
        // closed only while no network is granted (a granted network opens
        // all outbound including loopback until the egress proxy ships).
        SandboxProvider::MacosSandboxExec | SandboxProvider::LinuxBwrap => {
            caps.insert(FsDenyTaTree);
            caps.insert(FsWriteConfined);
            if no_network_granted {
                caps.insert(NetLoopbackBlocked);
            }
        }
        // AppContainer: no ACE outside staging, loopback blocked by default,
        // no handles to processes outside the container, no Task Scheduler/WMI.
        SandboxProvider::WindowsAppContainer => {
            caps.insert(FsDenyTaTree);
            caps.insert(FsWriteConfined);
            caps.insert(NetLoopbackBlocked);
            caps.insert(ProcNoPtraceOut);
            caps.insert(ProcNoUnsandboxedSpawn);
            caps.insert(IpcNoDaemonSocket);
        }
    }
    caps
}

pub fn provider_name(provider: &SandboxProvider) -> &'static str {
    match provider {
        SandboxProvider::None => "none",
        SandboxProvider::MacosSandboxExec => "macos_sandbox_exec",
        SandboxProvider::LinuxBwrap => "linux_bwrap",
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
        // With neither, `open` (the pre-existing behavior); project config can
        // only move toward `strict`, never away from the owner's floor.
        let floor = trusted
            .and_then(|t| t.posture)
            .unwrap_or(SandboxPosture::Open);
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
        let allow_read: Vec<PathBuf> = allow_read
            .into_iter()
            .filter_map(|p| keep("read", p, &mut warnings))
            .collect();
        let allow_write: Vec<PathBuf> = allow_write
            .into_iter()
            .filter_map(|p| keep("write", p, &mut warnings))
            .collect();
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
        }
    }
}

/// Paths denied for read and write regardless of any allowance.
pub fn protected_paths(ctx: &LaunchContext) -> Vec<PathBuf> {
    let mut out = vec![ctx.project_root.join(".ta"), ctx.project_root.join(".git")];
    if let Some(state) = &ctx.state_dir {
        out.push(state.clone());
    }
    if let Some(home) = &ctx.user_home {
        for rel in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".config/gh",
            ".docker",
            ".kube",
            ".netrc",
            "Library/Keychains",
            ".local/share/keyrings",
        ] {
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
    fn granted_network_means_loopback_is_not_counted_as_blocked() {
        let trusted = TrustedPolicy {
            enabled: Some(true),
            posture: Some(SandboxPosture::Balanced),
            allow_network: vec!["api.anthropic.com".into()],
            ..Default::default()
        };
        let p =
            SandboxProfile::compile(Some(&trusted), &project_open(), &ctx(RoleClass::Chat, true));
        let caps = provider_capabilities(&SandboxProvider::MacosSandboxExec, &p);
        assert!(!caps.contains(&Capability::NetLoopbackBlocked));
        let d = decide(
            &p,
            &SandboxProvider::MacosSandboxExec,
            true,
            GatewayPlacement::OutOfSandboxSocket,
        );
        assert_eq!(d.outcome, Outcome::Refused);
        assert!(d.missing.contains(&"net.loopback_blocked".to_string()));
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
}
