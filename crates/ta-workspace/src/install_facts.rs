// install_facts.rs — Machine-readable facts an installer asks `ta` for (v0.17.12.5.1).
//
// An installer (the Virtual Team installer, a CI bootstrap) must not scrape
// human text to learn what this `ta` can safely do, whether the terms of use
// are accepted, or where the daemon is. Each document here is printed by one
// `--json` command, carries a `schema_version`, and has its JSON Schema
// generated from these types into `schema/` (drift-tested in `ta-data-spec`).
//
// Additive changes (a new optional field) keep the version; renaming,
// removing or re-typing a field bumps it, together with the matching
// `SpecEntry` version in `ta-data-spec`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Shape version of [`CapabilitiesReport`] (`ta capabilities --json`), also
/// its `capabilities_schema`.
pub const CAPABILITIES_SCHEMA_VERSION: u32 = 1;

/// Shape version of [`DaemonProbe`] (`ta-daemon --print-capabilities`).
pub const DAEMON_PROBE_SCHEMA_VERSION: u32 = 1;

/// Shape version of [`TermsStatusReport`] (`ta terms-status --json`).
pub const TERMS_STATUS_SCHEMA_VERSION: u32 = 1;

/// Shape version of [`DaemonReport`] (`ta daemon start|restart|status --json`).
pub const DAEMON_REPORT_SCHEMA_VERSION: u32 = 1;

/// Operating system and CPU architecture this `ta` was built for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Platform {
    /// `std::env::consts::OS`: `macos`, `linux`, `windows`, ...
    pub os: String,
    /// `std::env::consts::ARCH`: `aarch64`, `x86_64`, ...
    pub arch: String,
}

impl Platform {
    /// The platform of the running binary.
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

/// A sandbox provider usable on this machine and the capabilities it enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SandboxProviderFact {
    /// Provider name, e.g. `macos_sandbox_exec`, `linux_bwrap`,
    /// `linux_landlock`, `windows_appcontainer`.
    pub name: String,
    /// Capability names (`fs.deny_ta_tree`, `net.loopback_blocked`, ...) the
    /// provider enforces, computed from the same matrix a real launch uses.
    pub enforces: Vec<String>,
}

/// Outcome of asking the installed `ta-daemon` for its own facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DaemonProbeState {
    /// The daemon answered; its facts are in the capability fields.
    Ok,
    /// No `ta-daemon` binary was found next to `ta` or on PATH.
    BinaryNotFound,
    /// The binary exists but does not understand `--print-capabilities`: a
    /// daemon built before the probe existed. Its capabilities are reported
    /// false because nothing proves them.
    ProbeUnsupported,
    /// The binary ran but its answer could not be used (crash, timeout,
    /// unreadable or unsupported probe version).
    ProbeFailed,
}

/// How the daemon-side capabilities were obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonProbeStatus {
    pub state: DaemonProbeState,
    /// The `ta-daemon` binary that was asked, when one was found.
    pub binary: Option<String>,
    /// What happened and what to do about it; empty when `state` is `ok`.
    pub detail: String,
}

/// What `ta-daemon --print-capabilities` prints: the facts only the daemon
/// binary can vouch for, each computed by running the code the daemon itself
/// uses (never from a flag or version string).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonProbe {
    pub probe_schema: u32,
    /// The daemon's own version.
    pub daemon_version: String,
    /// A CoS launch built by the daemon's role-launch code carries both
    /// `--chat-mode` and `--origin cos`.
    pub passes_restricted_flags: bool,
    /// Only roles in `[whiteboard] outcome_roles` receive the outcome right
    /// when the daemon mints a role token.
    pub outcome_roles: bool,
    /// The daemon's NATS client refuses a non-loopback server without
    /// credentials and accepts one with them.
    pub nats_credentials: bool,
    /// One line per capability saying what was run to decide it.
    pub evidence: BTreeMap<String, String>,
}

/// `ta capabilities --json`: what this installation can safely do.
///
/// Every boolean is true only when the code path behind it was exercised and
/// worked (see `evidence`); a flag appearing in `--help` never counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilitiesReport {
    /// Shape version of this document (same value as `capabilities_schema`).
    pub schema_version: u32,
    /// Version of the capability field set; installers gate on this.
    pub capabilities_schema: u32,
    /// `ta` version. NOT a capability signal: builds before and after the CoS
    /// hardening report the same version.
    pub ta_version: String,
    pub platform: Platform,
    /// The installed daemon launches CoS roles with `--chat-mode` and
    /// `--origin cos`, and this `ta run` accepts and acts on them. False when
    /// no daemon could be asked.
    pub daemon_passes_restricted_flags: bool,
    /// `ta run --chat-mode` starts a read-only chat-mode launch.
    pub supports_chat_mode: bool,
    /// `ta run --origin <name>` validates and stamps the goal origin.
    pub supports_origin: bool,
    /// The compiled rules table decides actions and defaults to deny when unattended.
    pub rules_engine: bool,
    /// The daemon grants the outcome right only to the configured roles.
    pub outcome_roles: bool,
    /// The daemon's NATS client enforces credentials for non-loopback servers.
    pub nats_credentials: bool,
    /// `ta terms-status --json` reports a document version and content hash.
    pub terms_status: bool,
    /// Apply journals prior values and a failed apply restores them.
    pub atomic_apply: bool,
    /// Sandbox providers usable on this machine (empty when none).
    pub sandbox_providers: Vec<SandboxProviderFact>,
    /// How the daemon-side capabilities were obtained.
    pub daemon_probe: DaemonProbeStatus,
    /// Per capability, what was run to decide it (or why it is false).
    pub evidence: BTreeMap<String, String>,
}

/// Where the current terms document stands for this user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TermsState {
    /// Accepted, and the accepted text is the text this `ta` shows.
    Current,
    /// Accepted before, but the terms document has changed since: re-accept.
    Stale,
    /// No acceptance record exists.
    NotAccepted,
    /// The home directory (and so the record location) could not be determined.
    Unavailable,
}

/// `ta terms-status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TermsStatusReport {
    pub schema_version: u32,
    pub status: TermsState,
    /// Version label of the terms document this `ta` embeds (`TERMS_VERSION`).
    /// Unrelated to the `ta` version.
    pub document_version: String,
    /// Lowercase hex SHA-256 of the embedded terms text, byte for byte
    /// (`ta view-terms` prints these bytes plus one trailing newline).
    pub document_sha256: String,
    /// Document version recorded when the terms were accepted; null for a
    /// record written before documents were versioned, or when none exists.
    pub accepted_document_version: Option<String>,
    /// Full SHA-256 recorded at acceptance; null for such a legacy record.
    pub accepted_document_sha256: Option<String>,
    /// When the terms were accepted (RFC 3339).
    pub accepted_at: Option<String>,
    /// The `ta` version that recorded the acceptance.
    pub accepted_by_ta_version: Option<String>,
    /// The record predates document versions and carries only the 16-hex
    /// prefix of the hash. It is still honored while that prefix matches the
    /// current text; `accepted_document_*` are null. A consumer binding
    /// acceptance to the full hash should have the user accept again.
    pub legacy_record: bool,
    /// Where the acceptance record lives.
    pub record_path: Option<String>,
    /// What to run next; null when `status` is `current`.
    pub next_step: Option<String>,
}

/// Which `ta daemon` command produced a [`DaemonReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DaemonAction {
    Start,
    Restart,
    Status,
}

/// What state the project's daemon is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    /// Answering and healthy.
    Running,
    /// Answering (or just spawned and not answering yet) but not finished
    /// starting, usually waiting on a Keychain approval; see `reasons`.
    Starting,
    /// Answering, started with a failed component; see `reasons`.
    Degraded,
    /// The pid in `.ta/daemon.pid` is alive but nothing answers on `url`.
    Unresponsive,
    /// `.ta/daemon.pid` named a process that is gone; the file was removed.
    StalePid,
    /// No daemon for this project.
    NotRunning,
    /// The port answers with another project's daemon. This project has none.
    OtherProject,
    /// A daemon for this project answers but did not write a pid file here.
    External,
}

/// One reason a daemon is `starting` or `degraded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonReason {
    pub component: String,
    pub reason: String,
    pub next_step: Option<String>,
}

/// `ta daemon start|restart|status --json`: the pid/port contract of
/// `.ta/daemon.pid` (pid, bind host, port, log) plus the daemon's health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonReport {
    pub schema_version: u32,
    pub action: DaemonAction,
    pub state: DaemonState,
    /// True for `running`, `starting`, `degraded` and `external`.
    pub running: bool,
    /// Daemon process id; null when none is known.
    pub pid: Option<u32>,
    /// The pid before a `restart`; null for other actions or no earlier daemon.
    pub previous_pid: Option<u32>,
    /// Port the daemon API is (or will be) reachable on.
    pub port: Option<u16>,
    /// Host the daemon binds / is reached on.
    pub host: Option<String>,
    /// `http://<host>:<port>`.
    pub url: Option<String>,
    /// Absolute path of `.ta/daemon.pid`.
    pub pid_file: String,
    /// Absolute path of the daemon log.
    pub log_file: String,
    pub project_root: String,
    /// Daemon version when it answered.
    pub version: Option<String>,
    /// Project root the answering daemon reports; set for `other_project`.
    pub reported_project_root: Option<String>,
    pub active_goals: Option<u64>,
    pub active_agents: Option<u64>,
    pub pending_drafts: Option<u64>,
    /// Why the daemon is `starting` or `degraded`, each with a next step.
    pub reasons: Vec<DaemonReason>,
    /// A stale pid file was removed by this call.
    pub stale_pid_removed: bool,
    /// What happened and what to do next; null when there is nothing to add.
    pub detail: Option<String>,
}
