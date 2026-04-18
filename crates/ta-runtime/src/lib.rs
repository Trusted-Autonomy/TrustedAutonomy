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
pub mod config;
pub mod credential;
pub mod framework;
pub mod plugin;
pub mod sandbox;

// Re-export the most commonly used types.
pub use adapter::{
    AgentHandle, Result, RuntimeAdapter, RuntimeError, RuntimeStatus, SpawnRequest, StdinMode,
    StdoutMode, TransportInfo,
};
pub use auth_spec::{detect_auth_mode, AgentAuthSpec, AuthCheckResult, AuthMethodSpec};
pub use bare_process::{apply_credentials_to_env, BareProcessRuntime};
pub use config::{RuntimeConfig, RuntimeRegistry};
pub use credential::ScopedCredential;
pub use framework::{
    inject_context_arg, inject_context_env, inject_memory_out_env, inject_memory_snapshot_env,
    AgentFramework, AgentFrameworkManifest, ContextInjectMode, ContextInjectionResult,
    FrameworkMemoryConfig, ManifestBackedFramework, MemoryInjectMode,
};
pub use sandbox::{SandboxPolicy, SandboxProvider};
