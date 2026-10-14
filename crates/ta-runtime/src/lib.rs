//! # ta-runtime
//!
//! Runtime Adapter Trait for Trusted Autonomy agent process management.
//!
//! Today TA spawns agent processes directly as bare OS child processes.
//! The `RuntimeAdapter` trait abstracts this so future backends (OCI containers,
//! VMs, remote execution) can be plugged in without changing the orchestration
//! logic.
//!
//! ## Architecture
//!
//! ```text
//! ta run / daemon orchestrator
//!         │
//!         ▼
//!  RuntimeRegistry.resolve("process")
//!         │
//!         ▼
//!  BareProcessRuntime.spawn(SpawnRequest)
//!         │
//!         ▼
//!  Box<dyn AgentHandle>   ──────► wait() / status() / stop()
//! ```
//!
//! ## Plugin runtimes
//!
//! SecureTA (and user plugins) register additional runtimes by shipping a
//! binary named `ta-runtime-<name>` (e.g., `ta-runtime-oci`).  The binary
//! speaks the JSON-over-stdio protocol defined in `plugin.rs`.  TA discovers
//! it via `RuntimeRegistry::resolve()`.
//!
//! ## Usage
//!
//! ```ignore
//! use ta_runtime::{RuntimeRegistry, SpawnRequest, StdinMode, StdoutMode};
//!
//! let registry = RuntimeRegistry::new();
//! let runtime = registry.resolve("process")?;
//!
//! let request = SpawnRequest {
//!     command: "claude".into(),
//!     args: vec!["--prompt".into(), "Fix the bug".into()],
//!     env: std::collections::HashMap::new(),
//!     working_dir: "/tmp/staging".into(),
//!     stdin_mode: StdinMode::Inherited,
//!     stdout_mode: StdoutMode::Inherited,
//! };
//!
//! let mut handle = runtime.spawn(request)?;
//! let exit = handle.wait()?;
//! ```

pub mod adapter;
pub mod auth_spec;
pub mod bare_process;
pub mod channels;
pub mod config;
pub mod credential;
pub mod egress_proxy;
pub mod framework;
pub mod linux_bwrap;
pub mod linux_confine;
pub mod linux_probe;
#[cfg(unix)]
pub mod linux_relay;
pub mod plugin;
pub mod sandbox;
pub mod sandbox_launch;
pub mod sandbox_windows;
pub mod seatbelt;
pub mod seatbelt_log;

// Re-export the most commonly used types.
pub use adapter::{
    AgentHandle, Result, RuntimeAdapter, RuntimeError, RuntimeStatus, SpawnRequest, StdinMode,
    StdoutMode, TransportInfo,
};
pub use auth_spec::{detect_auth_mode, AgentAuthSpec, AuthCheckResult, AuthMethodSpec};
pub use bare_process::{apply_credentials_to_env, BareProcessRuntime};
pub use channels::{
    build_channel, AgentContext, AgentContextChannel, ChannelCapabilities, ChannelType,
    ClaudeCodeChannel, CodexChannel, GenericFileChannel, HumanNote, NoteDelivery, OllamaChannel,
    DEFAULT_CONTEXT_FILE,
};
pub use config::{RuntimeConfig, RuntimeRegistry};
pub use credential::ScopedCredential;
pub use framework::{
    build_workflow_agent_index, inject_context_arg, inject_context_env, inject_memory_out_env,
    inject_memory_snapshot_env, AgentContextConfig, AgentFramework, AgentFrameworkManifest,
    ContextInjectMode, ContextInjectionResult, FrameworkMemoryConfig, ManifestBackedFramework,
    MemoryInjectMode,
};
pub use sandbox::{SandboxPolicy, SandboxProvider};
pub use sandbox_launch::{
    decide as decide_sandbox, degraded_label, prepare_agent_home, provider_capabilities,
    required_capabilities, scrub_env, Capability, EnvScrub, GatewayPlacement, LaunchContext,
    Outcome as SandboxOutcome, ProjectRequest, RoleClass, SandboxDecision, SandboxPosture,
    SandboxProfile, ScrubbedEnv, TrustedPolicy,
};
pub use sandbox_windows::{sandboxed_spawn, WindowsAppContainerGuard, WindowsJobObjectGuard};
