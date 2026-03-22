// adapter.rs — RuntimeAdapter trait and shared types.
//
// The RuntimeAdapter trait abstracts how TA spawns and manages agent processes.
// Today it is hardcoded as a bare child process; this trait enables container,
// VM, and remote execution backends without changing TA's orchestration logic.
//
// ## Backends
//
// - **BareProcess** (built-in, default): spawns a child process using the OS
//   process API. No isolation. Same machine. Same user. Same network.
//
// - **OCI** (plugin, provided by SecureTA): runs the agent inside a container.
//   Requires a container runtime (Docker, Podman, containerd).
//
// - **VM** (plugin, provided by SecureTA): runs the agent in a lightweight VM
//   (e.g., Firecracker). Maximum isolation; hardware-bounded trust boundary.
//
// ## Transport
//
// Each runtime also knows which MCP transport the agent should use to reach
// the TA gateway. BareProcess uses Stdio (the default .mcp.json stdio config).
// Container/VM runtimes use TCP or Unix socket transports so the agent can
// connect back to the host daemon.
//
// ## Plugin protocol
//
// External runtimes communicate over the same JSON-over-stdio protocol as VCS
// plugins. See `plugin.rs` for the request/response types.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;

use crate::credential::ScopedCredential;

// ── Error ────────────────────────────────────────────────────────────────────

/// Errors from the RuntimeAdapter.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("Runtime not available: {0}")]
    NotAvailable(String),

    #[error("Spawn failed: {0}")]
    SpawnFailed(String),

    #[error("I/O error during process management: {0}")]
    Io(#[from] std::io::Error),

    #[error("Stop failed: {0}")]
    StopFailed(String),

    #[error("Status check failed: {0}")]
    StatusCheckFailed(String),

    #[error("Credential injection failed: {0}")]
    CredentialInjectionFailed(String),

    #[error("Transport attachment failed: {0}")]
    TransportAttachFailed(String),

    #[error("Plugin error: {0}")]
    PluginError(String),
}

pub type Result<T> = std::result::Result<T, RuntimeError>;

// ── Spawn request ─────────────────────────────────────────────────────────────

/// How to connect stdin for a spawned agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinMode {
    /// No stdin. Agent cannot read from terminal.
    Null,
    /// Agent inherits the parent's stdin.
    Inherited,
    /// Stdin connected to a pipe (caller manages writes).
    Piped,
}

/// How to handle the spawned agent's stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdoutMode {
    /// Agent writes directly to terminal.
    Inherited,
    /// Stdout connected to a pipe (caller streams output).
    Piped,
}

/// All the information needed to spawn an agent process.
///
/// The RuntimeAdapter maps this to whatever mechanism the backend uses —
/// a child process, a container, a VM, or a remote API call.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// The executable to run (e.g., "claude", "codex").
    pub command: String,

    /// Arguments already expanded (no template variables).
    pub args: Vec<String>,

    /// Environment variables to set for the agent.
    pub env: HashMap<String, String>,

    /// Working directory for the agent.
    pub working_dir: PathBuf,

    /// How to handle stdin.
    pub stdin_mode: StdinMode,

    /// How to handle stdout.
    pub stdout_mode: StdoutMode,
}

// ── Status ───────────────────────────────────────────────────────────────────

/// Observed state of a running or finished agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// Process is running.
    Running,

    /// Process has exited.
    Exited {
        /// OS exit code; None if the process was killed by a signal.
        exit_code: Option<i32>,
    },

    /// Status is not available (runtime cannot determine state).
    Unknown,
}

impl RuntimeStatus {
    /// Returns true if the agent finished with a successful (zero) exit code.
    pub fn is_success(&self) -> bool {
        matches!(self, RuntimeStatus::Exited { exit_code: Some(0) })
    }

    /// Returns true if the agent is still running.
    pub fn is_running(&self) -> bool {
        matches!(self, RuntimeStatus::Running)
    }
}

// ── Transport info ────────────────────────────────────────────────────────────

/// Which MCP transport the agent should use to reach the TA gateway.
///
/// BareProcess agents use Stdio — the agent's stdin/stdout ARE the MCP
/// transport channel, configured via .mcp.json.  Container/VM runtimes
/// use network transports because stdio is not shared across the isolation
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportInfo {
    /// Agent communicates with the gateway via its stdin/stdout.
    /// This is the default for BareProcess agents.
    Stdio,

    /// Agent should connect to a Unix domain socket.
    UnixSocket {
        /// Absolute path to the socket file, accessible to the agent.
        path: PathBuf,
    },

    /// Agent should connect to a TCP endpoint.
    Tcp {
        /// Host address (e.g., "127.0.0.1" or a hostname visible to the agent).
        host: String,
        /// Port number.
        port: u16,
        /// When true, the connection should use TLS.
        tls: bool,
    },
}

// ── Agent process handle ──────────────────────────────────────────────────────

/// Handle to a running agent — returned by `RuntimeAdapter::spawn()`.
///
/// Callers use this to wait for the agent, stream output, stop it, or query
/// its runtime transport endpoint.  Each backend provides its own concrete
/// implementation; callers hold it as `Box<dyn AgentHandle>`.
pub trait AgentHandle: Send {
    /// OS process ID, if known.  May be None for remote backends.
    fn pid(&self) -> Option<u32>;

    /// Non-blocking status poll.
    fn status(&mut self) -> Result<RuntimeStatus>;

    /// Block until the agent exits and return its exit status.
    fn wait(&mut self) -> Result<ExitStatus>;

    /// Take the stdout pipe, if it was configured as `StdoutMode::Piped`.
    /// May only be called once.
    fn take_stdout(&mut self) -> Option<std::process::ChildStdout>;

    /// Which MCP transport should the agent use to talk to the gateway.
    fn transport_info(&self) -> TransportInfo;

    /// Gracefully stop the agent (SIGTERM on Unix, then SIGKILL after timeout).
    fn stop(&mut self) -> Result<()>;
}

// ── RuntimeAdapter trait ──────────────────────────────────────────────────────

/// Abstraction layer for agent process management.
///
/// A `RuntimeAdapter` knows how to:
/// 1. Spawn an agent in its execution environment (process, container, VM).
/// 2. Inject scoped credentials into the agent's environment without
///    exposing the raw credential vault to the agent.
///
/// Implementations are registered in `RuntimeRegistry` and selected by name
/// from the agent YAML config (`runtime = "process"` is the default).
///
/// ## Thread safety
///
/// `RuntimeAdapter` is `Send + Sync` so it can be shared across threads and
/// stored in a `Box<dyn RuntimeAdapter>`.
pub trait RuntimeAdapter: Send + Sync {
    /// Unique name for this runtime (e.g., "process", "oci", "vm").
    fn name(&self) -> &str;

    /// Spawn an agent and return a handle for monitoring and control.
    ///
    /// This method returns immediately after the process is launched.  The
    /// caller uses the returned `AgentHandle` to wait for completion.
    fn spawn(&self, request: SpawnRequest) -> Result<Box<dyn AgentHandle>>;

    /// Inject scoped credentials into a running agent's environment.
    ///
    /// For `BareProcess`, this is a no-op — credentials are passed as env
    /// vars at spawn time.  For OCI/VM runtimes, this may write credentials
    /// to a mounted secrets path or call a runtime API.
    ///
    /// Credentials injected here are SCOPED: only the listed `scopes` are
    /// accessible to the agent.  The agent never holds the raw vault key.
    fn inject_credentials(
        &self,
        handle: &mut dyn AgentHandle,
        creds: &[ScopedCredential],
    ) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_status_is_success() {
        assert!(RuntimeStatus::Exited { exit_code: Some(0) }.is_success());
        assert!(!RuntimeStatus::Exited { exit_code: Some(1) }.is_success());
        assert!(!RuntimeStatus::Exited { exit_code: None }.is_success());
        assert!(!RuntimeStatus::Running.is_success());
    }

    #[test]
    fn runtime_status_is_running() {
        assert!(RuntimeStatus::Running.is_running());
        assert!(!RuntimeStatus::Exited { exit_code: Some(0) }.is_running());
    }

    #[test]
    fn spawn_request_fields() {
        let req = SpawnRequest {
            command: "claude".into(),
            args: vec!["--prompt".into(), "hello".into()],
            env: [("KEY".into(), "val".into())].into(),
            working_dir: PathBuf::from("/tmp"),
            stdin_mode: StdinMode::Inherited,
            stdout_mode: StdoutMode::Inherited,
        };
        assert_eq!(req.command, "claude");
        assert_eq!(req.args.len(), 2);
        assert_eq!(req.env.get("KEY"), Some(&"val".to_string()));
    }
}
