// config.rs — Runtime configuration and selection.
//
// `RuntimeConfig` is embedded in agent YAML (e.g., `agents/claude.yaml`) and
// workflow TOML files to declare which runtime backend to use.
//
// The `RuntimeRegistry` maps runtime names (e.g., "process", "oci", "vm")
// to `RuntimeAdapter` implementations.  The built-in "process" backend is
// always available.  External runtimes (OCI, VM) are provided by SecureTA
// or other plugins and registered via `RuntimeRegistry::register()`.
//
// ## Config example (agent YAML)
//
//   runtime = "process"   # default; bare child process
//
//   [runtime_options]     # optional, passed to the adapter
//   image = "ghcr.io/secureta/agent:latest"   # for OCI
//
// ## Plugin loading
//
// If a requested runtime is not built-in, the registry looks for a binary
// named `ta-runtime-<name>` in:
//   1. `.ta/plugins/runtimes/`
//   2. `~/.config/ta/plugins/runtimes/`
//   3. Directories on `$PATH`
//
// The binary speaks the JSON-over-stdio runtime plugin protocol (see
// `plugin.rs`).  An `ExternalRuntimeAdapter` wraps it as a `RuntimeAdapter`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::adapter::{RuntimeAdapter, RuntimeError};
use crate::bare_process::BareProcessRuntime;
use crate::plugin::ExternalRuntimeAdapter;

// ── Config ────────────────────────────────────────────────────────────────────

/// Runtime selection embedded in agent/workflow configuration.
///
/// Deserialised from agent YAML or daemon.toml `[agents.<id>.runtime]` blocks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConfig {
    /// Which runtime backend to use.
    ///
    /// Accepted values: "process" (default), "oci", "vm", or any name
    /// for which a `ta-runtime-<name>` plugin binary exists.
    #[serde(default = "default_runtime_name")]
    pub name: String,

    /// Optional backend-specific options forwarded to the adapter.
    ///
    /// For OCI: `image`, `registry`, `pull_policy`, `mounts`…
    /// For VM:  `machine_type`, `vcpus`, `memory_mb`…
    /// For process: currently unused (reserved for future cgroup config).
    #[serde(default)]
    pub options: HashMap<String, serde_json::Value>,
}

fn default_runtime_name() -> String {
    "process".to_string()
}

impl RuntimeConfig {
    /// Convenience constructor for the default bare-process runtime.
    pub fn bare_process() -> Self {
        RuntimeConfig {
            name: "process".to_string(),
            options: HashMap::new(),
        }
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

/// Registry of available `RuntimeAdapter` implementations.
///
/// Always contains the built-in `BareProcessRuntime` under the name
/// `"process"`.  Call `register()` to add more (e.g., from SecureTA
/// plugins at daemon startup).
///
/// `resolve()` first checks registered adapters, then falls back to
/// plugin discovery on `$PATH` and well-known plugin directories.
pub struct RuntimeRegistry {
    adapters: HashMap<String, Arc<dyn RuntimeAdapter>>,
    /// Plugin search paths.  Appended in priority order.
    plugin_dirs: Vec<PathBuf>,
}

impl RuntimeRegistry {
    /// Create a registry pre-populated with the built-in `BareProcessRuntime`.
    pub fn new() -> Self {
        let mut reg = RuntimeRegistry {
            adapters: HashMap::new(),
            plugin_dirs: Vec::new(),
        };
        reg.register(Arc::new(BareProcessRuntime::new()));
        reg
    }

    /// Register a custom `RuntimeAdapter` under its `name()`.
    pub fn register(&mut self, adapter: Arc<dyn RuntimeAdapter>) {
        self.adapters.insert(adapter.name().to_string(), adapter);
    }

    /// Add a directory to search for runtime plugin binaries.
    pub fn add_plugin_dir(&mut self, dir: impl Into<PathBuf>) {
        self.plugin_dirs.push(dir.into());
    }

    /// Populate plugin directories for a given project root.
    ///
    /// Adds `.ta/plugins/runtimes/` (project-local) and
    /// `~/.config/ta/plugins/runtimes/` (user-global).
    pub fn add_default_plugin_dirs(&mut self, project_root: &Path) {
        self.add_plugin_dir(project_root.join(".ta").join("plugins").join("runtimes"));
        if let Some(home) = home_dir() {
            self.add_plugin_dir(
                home.join(".config")
                    .join("ta")
                    .join("plugins")
                    .join("runtimes"),
            );
        }
    }

    /// Resolve a runtime by name.
    ///
    /// Lookup order:
    /// 1. Built-in or previously registered adapters.
    /// 2. Plugin binary `ta-runtime-<name>` in plugin directories.
    /// 3. Plugin binary `ta-runtime-<name>` on `$PATH`.
    ///
    /// Returns `Err(RuntimeError::NotAvailable)` if no matching runtime
    /// can be found.
    pub fn resolve(&self, name: &str) -> Result<Arc<dyn RuntimeAdapter>, RuntimeError> {
        // 1. Built-in / registered.
        if let Some(adapter) = self.adapters.get(name) {
            return Ok(adapter.clone());
        }

        // 2 & 3. Try to discover a plugin binary.
        let binary_name = format!("ta-runtime-{}", name);
        debug!(runtime = name, binary = %binary_name, "Searching for runtime plugin binary");

        // Plugin directories first (in order), then $PATH.
        for dir in &self.plugin_dirs {
            let candidate = dir.join(&binary_name);
            if candidate.exists() {
                debug!(path = %candidate.display(), "Found runtime plugin in plugin dir");
                match ExternalRuntimeAdapter::new(&candidate, name) {
                    Ok(adapter) => return Ok(Arc::new(adapter)),
                    Err(e) => warn!("Runtime plugin {} found but handshake failed: {}", name, e),
                }
            }
        }

        // $PATH search.
        if let Ok(path) = which::which(&binary_name) {
            debug!(path = %path.display(), "Found runtime plugin on $PATH");
            match ExternalRuntimeAdapter::new(&path, name) {
                Ok(adapter) => return Ok(Arc::new(adapter)),
                Err(e) => warn!("Runtime plugin {} on $PATH handshake failed: {}", name, e),
            }
        }

        Err(RuntimeError::NotAvailable(format!(
            "No runtime adapter found for '{}'. \
             Built-in adapters: {}. \
             To add a plugin, install ta-runtime-{} in .ta/plugins/runtimes/ or on $PATH.",
            name,
            self.adapters.keys().cloned().collect::<Vec<_>>().join(", "),
            name,
        )))
    }

    /// List the names of all registered (built-in) adapters.
    pub fn registered_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.adapters.keys().cloned().collect();
        names.sort();
        names
    }
}

impl Default for RuntimeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_process() {
        let reg = RuntimeRegistry::new();
        let adapter = reg
            .resolve("process")
            .expect("process runtime must always exist");
        assert_eq!(adapter.name(), "process");
    }

    #[test]
    fn resolve_unknown_returns_error() {
        let reg = RuntimeRegistry::new();
        match reg.resolve("oci") {
            Ok(_) => panic!("expected error for unknown runtime"),
            Err(RuntimeError::NotAvailable(msg)) => {
                assert!(
                    msg.contains("oci"),
                    "Error should mention the requested runtime"
                );
                assert!(
                    msg.contains("process"),
                    "Error should list available runtimes"
                );
            }
            Err(other) => panic!("unexpected error: {}", other),
        }
    }

    #[test]
    fn register_and_resolve_custom() {
        struct MockRuntime;
        impl RuntimeAdapter for MockRuntime {
            fn name(&self) -> &str {
                "mock"
            }
            fn spawn(
                &self,
                _: crate::adapter::SpawnRequest,
            ) -> crate::adapter::Result<Box<dyn crate::adapter::AgentHandle>> {
                Err(RuntimeError::NotAvailable("mock".into()))
            }
            fn inject_credentials(
                &self,
                _: &mut dyn crate::adapter::AgentHandle,
                _: &[crate::credential::ScopedCredential],
            ) -> crate::adapter::Result<()> {
                Ok(())
            }
        }

        let mut reg = RuntimeRegistry::new();
        reg.register(Arc::new(MockRuntime));
        let adapter = reg.resolve("mock").expect("mock should be registered");
        assert_eq!(adapter.name(), "mock");
    }

    #[test]
    fn registered_names_sorted() {
        let mut reg = RuntimeRegistry::new();

        struct FakeRuntime {
            n: &'static str,
        }
        impl RuntimeAdapter for FakeRuntime {
            fn name(&self) -> &str {
                self.n
            }
            fn spawn(
                &self,
                _: crate::adapter::SpawnRequest,
            ) -> crate::adapter::Result<Box<dyn crate::adapter::AgentHandle>> {
                Err(RuntimeError::NotAvailable("fake".into()))
            }
            fn inject_credentials(
                &self,
                _: &mut dyn crate::adapter::AgentHandle,
                _: &[crate::credential::ScopedCredential],
            ) -> crate::adapter::Result<()> {
                Ok(())
            }
        }

        reg.register(Arc::new(FakeRuntime { n: "zzz" }));
        reg.register(Arc::new(FakeRuntime { n: "aaa" }));
        let names = reg.registered_names();
        assert_eq!(names, vec!["aaa", "process", "zzz"]);
    }

    #[test]
    fn runtime_config_default_is_process() {
        let cfg: RuntimeConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.name, "process");
    }

    #[test]
    fn runtime_config_bare_process_helper() {
        let cfg = RuntimeConfig::bare_process();
        assert_eq!(cfg.name, "process");
        assert!(cfg.options.is_empty());
    }
}
