// plugin.rs — JSON-over-stdio protocol for external runtime plugins.
//
// External runtimes (OCI, VM, remote) are provided as separate binaries that
// speak this protocol.  TA spawns the plugin binary and communicates with it
// over stdin/stdout using newline-delimited JSON.
//
// ## Protocol overview
//
//   TA → plugin: {"method":"<name>","params":{...}}\n
//   plugin → TA: {"ok":true,"result":{...}}\n
//            or  {"ok":false,"error":"..."}\n
//
// ## Session model
//
// Unlike VCS plugins (which are per-call), runtime plugins are LONG-LIVED:
//
//   1. TA spawns the plugin binary: `ta-runtime-oci --session <id>`
//   2. TA sends `handshake` to negotiate protocol version.
//   3. TA sends `spawn` to start the agent.
//   4. TA may send `status` and `stop` while the agent runs.
//   5. When done (or on error), TA sends `shutdown` and the plugin exits.
//
// The plugin binary manages the actual container/VM lifecycle and keeps a
// mapping from session-scoped agent IDs to their running instances.
//
// ## Methods
//
// | Method     | Description                                            |
// |------------|--------------------------------------------------------|
// | handshake  | Version negotiation; first call                        |
// | spawn      | Start an agent in the runtime environment              |
// | status     | Poll the current state of a running agent              |
// | stop       | Request graceful (then forceful) shutdown of an agent  |
// | shutdown   | Clean up the plugin session; plugin exits after reply  |

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::adapter::{
    AgentHandle, Result, RuntimeAdapter, RuntimeError, RuntimeStatus, SpawnRequest, TransportInfo,
};
use crate::credential::ScopedCredential;

/// Protocol version implemented by this TA build.
pub const RUNTIME_PLUGIN_PROTOCOL_VERSION: u32 = 1;

// ── Request / Response envelopes ─────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct PluginRequest {
    method: String,
    params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct PluginResponse {
    ok: bool,
    #[serde(default)]
    result: serde_json::Value,
    #[serde(default)]
    error: Option<String>,
}

// ── Handshake ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct HandshakeParams {
    protocol_version: u32,
    ta_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct HandshakeResult {
    protocol_version: u32,
    plugin_version: String,
    capabilities: Vec<String>,
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct SpawnParams {
    agent_id: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    working_dir: String,
    stdin_mode: String,  // "null" | "inherited" | "piped"
    stdout_mode: String, // "inherited" | "piped"
    options: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct SpawnResult {
    agent_id: String,
    pid: Option<u32>,
    transport: TransportResult,
}

#[derive(Debug, Serialize, Deserialize)]
struct TransportResult {
    #[serde(rename = "type")]
    transport_type: String, // "stdio" | "unix" | "tcp"
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    tls: bool,
}

// ── Status ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct StatusParams {
    agent_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StatusResult {
    state: String, // "running" | "exited" | "unknown"
    exit_code: Option<i32>,
}

// ── Stop ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct StopParams {
    agent_id: String,
    grace_secs: u64,
}

// ── Shutdown ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[allow(dead_code)]
struct ShutdownParams {}

// ── Plugin process management ─────────────────────────────────────────────────

/// A running plugin process with its stdin/stdout pipes.
struct PluginProcess {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout_reader: BufReader<std::process::ChildStdout>,
}

impl PluginProcess {
    /// Send a request and read back one response line.
    fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let req = PluginRequest {
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&req)
            .map_err(|e| RuntimeError::PluginError(format!("serialize request: {}", e)))?;
        line.push('\n');

        self.stdin
            .write_all(line.as_bytes())
            .map_err(RuntimeError::Io)?;

        let mut response_line = String::new();
        self.stdout_reader
            .read_line(&mut response_line)
            .map_err(RuntimeError::Io)?;

        let resp: PluginResponse = serde_json::from_str(response_line.trim())
            .map_err(|e| RuntimeError::PluginError(format!("parse response: {}", e)))?;

        if resp.ok {
            Ok(resp.result)
        } else {
            Err(RuntimeError::PluginError(
                resp.error.unwrap_or_else(|| "unknown error".to_string()),
            ))
        }
    }

    /// Send `shutdown` and wait for the plugin to exit.
    fn shutdown(mut self) {
        let _ = self.call("shutdown", serde_json::json!({}));
        let _ = self.child.wait();
    }
}

// ── ExternalRuntimeAdapter ────────────────────────────────────────────────────

/// RuntimeAdapter that delegates to an external plugin binary.
///
/// The plugin binary (e.g., `ta-runtime-oci`) is spawned once and kept alive
/// across multiple `spawn()` calls.  The plugin manages its own container/VM
/// lifecycle.
pub struct ExternalRuntimeAdapter {
    runtime_name: String,
    #[allow(dead_code)]
    plugin_path: PathBuf,
    plugin_version: String,
    capabilities: Vec<String>,
    /// Long-lived plugin process, protected by a mutex for thread-safety.
    process: Mutex<PluginProcess>,
}

impl ExternalRuntimeAdapter {
    /// Spawn the plugin binary and perform the handshake.
    pub fn new(plugin_path: &Path, runtime_name: &str) -> Result<Self> {
        let mut child = Command::new(plugin_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                RuntimeError::PluginError(format!(
                    "Failed to spawn runtime plugin {}: {}",
                    plugin_path.display(),
                    e
                ))
            })?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stdout_reader = BufReader::new(stdout);

        let mut proc = PluginProcess {
            child,
            stdin,
            stdout_reader,
        };

        // Perform handshake.
        let handshake_params = serde_json::to_value(HandshakeParams {
            protocol_version: RUNTIME_PLUGIN_PROTOCOL_VERSION,
            ta_version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .unwrap();

        let result = proc.call("handshake", handshake_params)?;
        let handshake: HandshakeResult = serde_json::from_value(result)
            .map_err(|e| RuntimeError::PluginError(format!("handshake parse: {}", e)))?;

        if handshake.protocol_version != RUNTIME_PLUGIN_PROTOCOL_VERSION {
            proc.shutdown();
            return Err(RuntimeError::PluginError(format!(
                "Protocol version mismatch: TA requires v{}, plugin provides v{}",
                RUNTIME_PLUGIN_PROTOCOL_VERSION, handshake.protocol_version,
            )));
        }

        debug!(
            runtime = runtime_name,
            version = %handshake.plugin_version,
            capabilities = ?handshake.capabilities,
            "Runtime plugin handshake succeeded"
        );

        Ok(ExternalRuntimeAdapter {
            runtime_name: runtime_name.to_string(),
            plugin_path: plugin_path.to_path_buf(),
            plugin_version: handshake.plugin_version,
            capabilities: handshake.capabilities,
            process: Mutex::new(proc),
        })
    }

    /// Plugin version string (from handshake).
    pub fn plugin_version(&self) -> &str {
        &self.plugin_version
    }

    /// Capabilities reported by the plugin at handshake time.
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }
}

impl RuntimeAdapter for ExternalRuntimeAdapter {
    fn name(&self) -> &str {
        &self.runtime_name
    }

    fn spawn(&self, request: SpawnRequest) -> Result<Box<dyn AgentHandle>> {
        let agent_id = uuid::Uuid::new_v4().to_string();

        let stdin_mode = match request.stdin_mode {
            crate::adapter::StdinMode::Null => "null",
            crate::adapter::StdinMode::Inherited => "inherited",
            crate::adapter::StdinMode::Piped => "piped",
        };
        let stdout_mode = match request.stdout_mode {
            crate::adapter::StdoutMode::Inherited => "inherited",
            crate::adapter::StdoutMode::Piped => "piped",
        };

        let params = serde_json::to_value(SpawnParams {
            agent_id: agent_id.clone(),
            command: request.command,
            args: request.args,
            env: request.env,
            working_dir: request.working_dir.to_string_lossy().into_owned(),
            stdin_mode: stdin_mode.to_string(),
            stdout_mode: stdout_mode.to_string(),
            options: serde_json::json!({}),
        })
        .map_err(|e| RuntimeError::PluginError(format!("serialize spawn params: {}", e)))?;

        let result = self.process.lock().unwrap().call("spawn", params)?;

        let spawn_result: SpawnResult = serde_json::from_value(result)
            .map_err(|e| RuntimeError::PluginError(format!("parse spawn result: {}", e)))?;

        let transport = match spawn_result.transport.transport_type.as_str() {
            "unix" => TransportInfo::UnixSocket {
                path: PathBuf::from(spawn_result.transport.path.unwrap_or_default()),
            },
            "tcp" => TransportInfo::Tcp {
                host: spawn_result
                    .transport
                    .host
                    .unwrap_or_else(|| "127.0.0.1".into()),
                port: spawn_result.transport.port.unwrap_or(9001),
                tls: spawn_result.transport.tls,
            },
            _ => TransportInfo::Stdio,
        };

        Ok(Box::new(ExternalAgentHandle {
            agent_id,
            pid: spawn_result.pid,
            transport,
            adapter_process: &self.process as *const _,
        }))
    }

    fn inject_credentials(
        &self,
        _handle: &mut dyn AgentHandle,
        creds: &[ScopedCredential],
    ) -> Result<()> {
        // For external runtimes, credentials should ideally be passed at spawn
        // time via the env map.  Post-spawn injection is runtime-specific and
        // not part of the v1 protocol.  We log a warning and proceed.
        if !creds.is_empty() {
            warn!(
                runtime = %self.runtime_name,
                count = creds.len(),
                "inject_credentials called post-spawn on external runtime; \
                 credentials should be in SpawnRequest.env. \
                 Post-spawn injection is not supported in plugin protocol v1."
            );
        }
        Ok(())
    }
}

// ── ExternalAgentHandle ───────────────────────────────────────────────────────

/// Handle to an agent managed by an external runtime plugin.
struct ExternalAgentHandle {
    agent_id: String,
    pid: Option<u32>,
    transport: TransportInfo,
    /// Raw pointer back to the plugin's process mutex.
    /// The plugin outlives all its handles (dropped only when the adapter drops).
    adapter_process: *const Mutex<PluginProcess>,
}

// SAFETY: The pointer points to data owned by ExternalRuntimeAdapter, which
// keeps the mutex alive as long as any handle is alive (handles hold a ref
// to the parent adapter through the caller's Arc<dyn RuntimeAdapter>).
unsafe impl Send for ExternalAgentHandle {}

impl ExternalAgentHandle {
    fn call_plugin(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let proc = unsafe { &*self.adapter_process };
        proc.lock().unwrap().call(method, params)
    }
}

impl AgentHandle for ExternalAgentHandle {
    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn status(&mut self) -> Result<RuntimeStatus> {
        let params = serde_json::to_value(StatusParams {
            agent_id: self.agent_id.clone(),
        })
        .unwrap();
        let result = self.call_plugin("status", params)?;
        let status: StatusResult = serde_json::from_value(result)
            .map_err(|e| RuntimeError::PluginError(format!("parse status: {}", e)))?;

        Ok(match status.state.as_str() {
            "running" => RuntimeStatus::Running,
            "exited" => RuntimeStatus::Exited {
                exit_code: status.exit_code,
            },
            _ => RuntimeStatus::Unknown,
        })
    }

    fn wait(&mut self) -> Result<ExitStatus> {
        // Poll status until the agent exits.
        loop {
            match self.status()? {
                RuntimeStatus::Exited { .. } => {
                    // Return a synthetic ExitStatus representing success (0).
                    // Callers should use status() for the exit code.
                    // We can't construct a real ExitStatus from scratch on all platforms,
                    // so we run a trivial process to get one with the right code.
                    let code = match self.status()? {
                        RuntimeStatus::Exited { exit_code: Some(c) } => c,
                        _ => 0,
                    };
                    let exit = std::process::Command::new("sh")
                        .args(["-c", &format!("exit {}", code)])
                        .status()
                        .map_err(RuntimeError::Io)?;
                    return Ok(exit);
                }
                _ => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        // External agents do not provide direct stdout pipes.
        // Output must be retrieved via the plugin-specific transport.
        None
    }

    fn transport_info(&self) -> TransportInfo {
        self.transport.clone()
    }

    fn stop(&mut self) -> Result<()> {
        let params = serde_json::to_value(StopParams {
            agent_id: self.agent_id.clone(),
            grace_secs: 5,
        })
        .unwrap();
        self.call_plugin("stop", params)?;
        Ok(())
    }
}
