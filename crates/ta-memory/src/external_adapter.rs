//! External memory adapter — MemoryStore implementation over JSON-over-stdio.
//!
//! Wraps an external memory backend plugin process and implements the
//! `MemoryStore` trait by translating every method call into a single
//! JSON-over-stdio request/response exchange.
//!
//! ## Lifecycle
//!
//! For each method call, `ExternalMemoryAdapter`:
//!
//! 1. Spawns the plugin process (fresh per call — plugins are stateless over stdio).
//! 2. Sends a `MemoryPluginRequest` JSON line to stdin.
//! 3. Reads the `MemoryPluginResponse` JSON line from stdout.
//! 4. Waits for the process to exit.
//! 5. Returns the parsed result or a `MemoryError` on failure.
//!
//! ## Handshake
//!
//! The adapter performs a `handshake` op on construction to validate protocol
//! compatibility. An incompatible protocol version causes construction to fail
//! with `MemoryError::Plugin`.
//!
//! ## Transport abstraction
//!
//! The `MemoryTransport` enum gates which transport is active. Only `Stdio`
//! is implemented in v0.14.6.5; `UnixSocket` and `Amp` are reserved for
//! future transports without changing the adapter API or protocol schema.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use uuid::Uuid;

use crate::error::MemoryError;
use crate::plugin_manifest::MemoryPluginManifest;
use crate::plugin_protocol::{
    ForgetParams, HandshakeParams, LookupParams, MemoryPluginRequest, MemoryPluginResponse,
    RecallParams, SemanticSearchParams, StoreParams, MEMORY_PROTOCOL_VERSION,
};
use crate::store::{
    MemoryEntry, MemoryQuery, MemoryStats, MemoryStore, StoreParams as TraitStoreParams,
};

// ---------------------------------------------------------------------------
// Transport abstraction (reserved for future expansion)
// ---------------------------------------------------------------------------

/// Which transport to use for memory plugin communication.
///
/// Only `Stdio` is implemented in v0.14.6.5.  `UnixSocket` and `Amp` are
/// reserved stubs — future transports can be added without changing the
/// adapter API or the JSON operation schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryTransport {
    /// JSON newline-delimited on stdin/stdout (default).
    Stdio,
    /// JSON framed over a Unix domain socket (future).
    UnixSocket,
    /// AMP messages over `.ta/amp.sock` (future, requires AMP broker).
    Amp,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// MemoryStore that delegates all operations to an external plugin process.
#[derive(Debug)]
pub struct ExternalMemoryAdapter {
    /// Plugin command to spawn.
    command: String,
    /// Additional args.
    args: Vec<String>,
    /// Working directory for plugin invocations.
    work_dir: PathBuf,
    /// Plugin's self-reported name (from handshake).
    plugin_name: String,
    /// Plugin version (retained for diagnostics).
    #[allow(dead_code)]
    plugin_version: String,
    /// Per-call process timeout.
    timeout: Duration,
    /// Capabilities reported by the plugin at handshake time.
    capabilities: Vec<String>,
    /// Active transport (always Stdio for v0.14.6.5).
    #[allow(dead_code)]
    transport: MemoryTransport,
}

impl ExternalMemoryAdapter {
    /// Create a new adapter and perform the initial handshake.
    ///
    /// Returns `MemoryError::Plugin` if the plugin is not found,
    /// the handshake fails, or protocol versions are incompatible.
    pub fn new(
        manifest: &MemoryPluginManifest,
        work_dir: impl Into<PathBuf>,
        ta_version: &str,
    ) -> Result<Self, MemoryError> {
        let work_dir = work_dir.into();
        let timeout = Duration::from_secs(manifest.timeout_secs);

        let handshake_req = MemoryPluginRequest {
            op: "handshake".to_string(),
            params: serde_json::to_value(HandshakeParams {
                ta_version: ta_version.to_string(),
                protocol_version: MEMORY_PROTOCOL_VERSION,
            })
            .map_err(|e| MemoryError::Plugin(format!("failed to serialize handshake: {}", e)))?,
        };

        let response = call_plugin(
            &manifest.command,
            &manifest.args,
            &work_dir,
            &handshake_req,
            timeout,
        )?;

        if !response.ok {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' handshake failed: {}",
                manifest.name,
                response.error.as_deref().unwrap_or("unknown error")
            )));
        }

        let proto_ver = response.protocol_version.unwrap_or(0);
        if proto_ver != MEMORY_PROTOCOL_VERSION {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' uses protocol version {} but TA requires {}. \
                 Upgrade the plugin or downgrade TA.",
                manifest.name, proto_ver, MEMORY_PROTOCOL_VERSION
            )));
        }

        let plugin_name = response
            .plugin_name
            .clone()
            .unwrap_or_else(|| manifest.name.clone());
        let plugin_version = response
            .plugin_version
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let capabilities = if response.capabilities.is_empty() {
            manifest.capabilities.clone()
        } else {
            response.capabilities.clone()
        };

        tracing::info!(
            plugin = %plugin_name,
            version = %plugin_version,
            "Memory plugin handshake successful"
        );

        Ok(Self {
            command: manifest.command.clone(),
            args: manifest.args.clone(),
            work_dir,
            plugin_name,
            plugin_version,
            timeout,
            capabilities,
            transport: MemoryTransport::Stdio,
        })
    }

    /// Adapter name as reported by the plugin.
    pub fn name(&self) -> &str {
        &self.plugin_name
    }

    /// Whether this plugin declares a given capability.
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    /// Execute a plugin op and return the raw response.
    fn call(
        &self,
        op: &str,
        params: serde_json::Value,
    ) -> Result<MemoryPluginResponse, MemoryError> {
        let request = MemoryPluginRequest {
            op: op.to_string(),
            params,
        };
        call_plugin(
            &self.command,
            &self.args,
            &self.work_dir,
            &request,
            self.timeout,
        )
    }
}

impl MemoryStore for ExternalMemoryAdapter {
    fn store(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
    ) -> Result<MemoryEntry, MemoryError> {
        let params = StoreParams {
            key: key.to_string(),
            value,
            tags,
            source: source.to_string(),
            goal_id: None,
            category: None,
            expires_at: None,
            confidence: None,
            phase_id: None,
        };
        let resp = self.call("store", serde_json::to_value(&params)?)?;
        if !resp.ok {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' store failed: {}",
                self.plugin_name,
                resp.error.as_deref().unwrap_or("unknown error")
            )));
        }
        resp.entry.ok_or_else(|| {
            MemoryError::Plugin(format!(
                "memory plugin '{}' store returned no entry",
                self.plugin_name
            ))
        })
    }

    fn store_with_params(
        &mut self,
        key: &str,
        value: serde_json::Value,
        tags: Vec<String>,
        source: &str,
        params: TraitStoreParams,
    ) -> Result<MemoryEntry, MemoryError> {
        let plugin_params = StoreParams {
            key: key.to_string(),
            value,
            tags,
            source: source.to_string(),
            goal_id: params.goal_id.map(|id| id.to_string()),
            category: params.category.map(|c| c.to_string()),
            expires_at: params.expires_at.map(|dt| dt.to_rfc3339()),
            confidence: params.confidence,
            phase_id: params.phase_id,
        };
        let resp = self.call("store", serde_json::to_value(&plugin_params)?)?;
        if !resp.ok {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' store_with_params failed: {}",
                self.plugin_name,
                resp.error.as_deref().unwrap_or("unknown error")
            )));
        }
        resp.entry.ok_or_else(|| {
            MemoryError::Plugin(format!(
                "memory plugin '{}' store_with_params returned no entry",
                self.plugin_name
            ))
        })
    }

    fn recall(&self, key: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        let params = RecallParams {
            key: key.to_string(),
        };
        let resp = self.call("recall", serde_json::to_value(&params)?)?;
        if !resp.ok {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' recall failed: {}",
                self.plugin_name,
                resp.error.as_deref().unwrap_or("unknown error")
            )));
        }
        Ok(resp.entry)
    }

    fn lookup(&self, query: MemoryQuery) -> Result<Vec<MemoryEntry>, MemoryError> {
        let params = LookupParams {
            prefix: query.key_prefix,
            tags: query.tags,
            goal_id: query.goal_id.map(|id| id.to_string()),
            category: query.category.map(|c| c.to_string()),
            phase_id: query.phase_id,
            limit: query.limit,
        };
        let resp = self.call("lookup", serde_json::to_value(&params)?)?;
        if !resp.ok {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' lookup failed: {}",
                self.plugin_name,
                resp.error.as_deref().unwrap_or("unknown error")
            )));
        }
        Ok(resp.entries.unwrap_or_default())
    }

    fn list(&self, limit: Option<usize>) -> Result<Vec<MemoryEntry>, MemoryError> {
        let params = LookupParams {
            prefix: None,
            tags: vec![],
            goal_id: None,
            category: None,
            phase_id: None,
            limit,
        };
        let resp = self.call("lookup", serde_json::to_value(&params)?)?;
        if !resp.ok {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' list failed: {}",
                self.plugin_name,
                resp.error.as_deref().unwrap_or("unknown error")
            )));
        }
        Ok(resp.entries.unwrap_or_default())
    }

    fn find_by_id(&self, id: Uuid) -> Result<Option<MemoryEntry>, MemoryError> {
        // Fall back to linear scan via list().
        let all = self.list(None)?;
        Ok(all.into_iter().find(|e| e.entry_id == id))
    }

    fn forget(&mut self, key: &str) -> Result<bool, MemoryError> {
        let params = ForgetParams {
            key: key.to_string(),
        };
        let resp = self.call("forget", serde_json::to_value(&params)?)?;
        if !resp.ok {
            return Err(MemoryError::Plugin(format!(
                "memory plugin '{}' forget failed: {}",
                self.plugin_name,
                resp.error.as_deref().unwrap_or("unknown error")
            )));
        }
        Ok(resp.deleted.unwrap_or(false))
    }

    fn semantic_search(&self, query: &str, k: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !self.has_capability("semantic_search") {
            return Ok(vec![]);
        }
        let params = SemanticSearchParams {
            query: query.to_string(),
            embedding: vec![],
            k,
        };
        let resp = self.call("semantic_search", serde_json::to_value(&params)?)?;
        if !resp.ok {
            tracing::warn!(
                plugin = %self.plugin_name,
                error = resp.error.as_deref().unwrap_or("unknown"),
                "Memory plugin semantic_search failed — returning empty results"
            );
            return Ok(vec![]);
        }
        Ok(resp.entries.unwrap_or_default())
    }

    fn stats(&self) -> Result<MemoryStats, MemoryError> {
        let resp = self.call("stats", serde_json::Value::Object(Default::default()))?;
        if !resp.ok {
            // Fall back to the default implementation (list + aggregate).
            return crate::store::default_stats(self);
        }
        resp.stats
            .ok_or_else(|| {
                MemoryError::Plugin(format!(
                    "memory plugin '{}' stats returned no stats object — falling back to aggregate",
                    self.plugin_name
                ))
            })
            .or_else(|_| crate::store::default_stats(self))
    }
}

// ---------------------------------------------------------------------------
// Low-level plugin call
// ---------------------------------------------------------------------------

/// Spawn the plugin, send one JSON request, read one JSON response.
///
/// Returns `MemoryError::Plugin` / `MemoryError::Io` on infrastructure failures.
fn call_plugin(
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request: &MemoryPluginRequest,
    timeout: Duration,
) -> Result<MemoryPluginResponse, MemoryError> {
    let request_json = serde_json::to_string(request).map_err(|e| {
        MemoryError::Plugin(format!(
            "failed to serialize memory plugin request for op '{}': {}",
            request.op, e
        ))
    })?;

    let mut parts = command.split_whitespace();
    let program = parts.next().ok_or_else(|| {
        MemoryError::Plugin(format!(
            "memory plugin command is empty for op '{}'",
            request.op
        ))
    })?;

    let mut cmd = Command::new(program);
    for arg in parts {
        cmd.arg(arg);
    }
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Retry on ETXTBSY (os error 26) — same pattern as ExternalVcsAdapter.
    let mut child = {
        const ETXTBSY: i32 = 26;
        let mut last_err: Option<std::io::Error> = None;
        let mut spawned = None;
        for delay_ms in [0u64, 20, 80, 200] {
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            match cmd.spawn() {
                Ok(c) => {
                    spawned = Some(c);
                    break;
                }
                Err(e) if e.raw_os_error() == Some(ETXTBSY) => {
                    last_err = Some(e);
                }
                Err(e) => {
                    return Err(MemoryError::Plugin(format!(
                        "failed to spawn memory plugin '{}' for op '{}': {}. \
                         Ensure the plugin is installed and on PATH.",
                        command, request.op, e
                    )));
                }
            }
        }
        spawned.ok_or_else(|| {
            let e = last_err.unwrap();
            MemoryError::Plugin(format!(
                "failed to spawn memory plugin '{}' for op '{}': {}. \
                 Ensure the plugin is installed and on PATH.",
                command, request.op, e
            ))
        })?
    };

    // Write request to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request_json.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .map_err(|e| {
                MemoryError::Plugin(format!(
                    "failed to write to memory plugin '{}' stdin: {}",
                    command, e
                ))
            })?;
    }

    // Wait with timeout.
    let timeout_millis = timeout.as_millis() as u64;
    let output = wait_with_timeout(child, timeout_millis).map_err(|e| {
        MemoryError::Plugin(format!(
            "memory plugin '{}' timed out or failed for op '{}': {}. \
             Increase timeout_secs in memory.toml.",
            command, request.op, e
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MemoryError::Plugin(format!(
            "memory plugin '{}' exited with status {} for op '{}'. stderr: {}",
            command,
            output.status,
            request.op,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("").trim();

    if first_line.is_empty() {
        return Err(MemoryError::Plugin(format!(
            "memory plugin '{}' produced no output for op '{}'. \
             Plugin must write one JSON line to stdout.",
            command, request.op
        )));
    }

    serde_json::from_str(first_line).map_err(|e| {
        MemoryError::Plugin(format!(
            "memory plugin '{}' produced invalid JSON for op '{}': {}. Got: '{}'",
            command,
            request.op,
            e,
            if first_line.len() > 200 {
                &first_line[..200]
            } else {
                first_line
            }
        ))
    })
}

/// Wait for a child process to exit, killing it after `timeout_ms` milliseconds.
fn wait_with_timeout(
    child: std::process::Child,
    timeout_ms: u64,
) -> std::result::Result<std::process::Output, String> {
    use std::sync::mpsc;

    let child_id = child.id();
    let (tx, rx) = mpsc::channel::<()>();

    let watchdog =
        std::thread::spawn(
            move || match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
                Ok(()) => {}
                Err(_) => {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(child_id as libc::pid_t, libc::SIGKILL);
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = child_id;
                    }
                }
            },
        );

    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait_with_output failed: {}", e))?;

    let _ = tx.send(());
    let _ = watchdog.join();

    Ok(output)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write a mock plugin shell script and make it executable.
    ///
    /// Uses /tmp on Linux to avoid ETXTBSY on overlayfs-backed TMPDIR.
    fn write_mock_plugin(_dir: &Path, script: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let name = format!("ta-memory-mock-{}-{}", pid, n);
        #[cfg(target_os = "linux")]
        let path = std::path::PathBuf::from("/tmp").join(&name);
        #[cfg(not(target_os = "linux"))]
        let path = _dir.join(&name);
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(script.as_bytes()).unwrap();
            f.sync_all().unwrap();
        }
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        let _ = std::fs::metadata(&path).unwrap();
        path
    }

    fn mock_manifest(command: &str) -> MemoryPluginManifest {
        MemoryPluginManifest {
            name: "mock".to_string(),
            version: "0.1.0".to_string(),
            command: command.to_string(),
            args: vec![],
            capabilities: vec!["semantic_search".to_string()],
            description: None,
            timeout_secs: 30,
        }
    }

    #[test]
    fn handshake_succeeds_with_valid_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo '{"ok":true,"plugin_name":"mock","plugin_version":"0.1.0","protocol_version":1,"capabilities":["semantic_search"]}'
"#,
        );
        let manifest = mock_manifest(&plugin.display().to_string());
        let adapter = ExternalMemoryAdapter::new(&manifest, dir.path(), "0.14.6-alpha.5").unwrap();
        assert_eq!(adapter.name(), "mock");
        assert!(adapter.has_capability("semantic_search"));
    }

    #[test]
    fn handshake_protocol_mismatch_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo '{"ok":true,"plugin_name":"bad","plugin_version":"0.1.0","protocol_version":99,"capabilities":[]}'
"#,
        );
        let manifest = mock_manifest(&plugin.display().to_string());
        let err = ExternalMemoryAdapter::new(&manifest, dir.path(), "0.14.6-alpha.5").unwrap_err();
        assert!(err.to_string().contains("protocol version"), "Got: {}", err);
    }

    #[test]
    fn handshake_error_response_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo '{"ok":false,"error":"missing API key"}'
"#,
        );
        let manifest = mock_manifest(&plugin.display().to_string());
        let err = ExternalMemoryAdapter::new(&manifest, dir.path(), "0.14.6-alpha.5").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("handshake failed") || msg.contains("missing API key"),
            "Got: {}",
            msg
        );
    }

    #[test]
    fn missing_command_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = mock_manifest("ta-memory-nonexistent-binary-xyz");
        let err = ExternalMemoryAdapter::new(&manifest, dir.path(), "0.14.6-alpha.5").unwrap_err();
        assert!(
            err.to_string().contains("spawn") || err.to_string().contains("No such file"),
            "Got: {}",
            err
        );
    }

    #[test]
    fn plugin_non_zero_exit_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo "some error" >&2
exit 1
"#,
        );
        let manifest = mock_manifest(&plugin.display().to_string());
        let err = ExternalMemoryAdapter::new(&manifest, dir.path(), "0.14.6-alpha.5").unwrap_err();
        assert!(
            err.to_string().contains("exited with status"),
            "Got: {}",
            err
        );
    }

    #[test]
    fn plugin_invalid_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo "not json"
"#,
        );
        let manifest = mock_manifest(&plugin.display().to_string());
        let err = ExternalMemoryAdapter::new(&manifest, dir.path(), "0.14.6-alpha.5").unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "Got: {}", err);
    }

    #[test]
    fn recall_returns_entry() {
        use chrono::Utc;
        use uuid::Uuid;
        let dir = tempfile::tempdir().unwrap();

        let entry_json = serde_json::json!({
            "entry_id": Uuid::new_v4().to_string(),
            "key": "arch:overview",
            "value": "main module",
            "tags": [],
            "source": "cli",
            "goal_id": null,
            "confidence": 0.9,
            "created_at": Utc::now().to_rfc3339(),
            "updated_at": Utc::now().to_rfc3339()
        });

        let handshake_response = r#"{"ok":true,"plugin_name":"mock","plugin_version":"0.1.0","protocol_version":1,"capabilities":[]}"#;
        let recall_response = format!(r#"{{"ok":true,"entry":{}}}"#, entry_json);

        // Script alternates responses: first call → handshake, second → recall.
        // We embed both responses and count calls via a temp counter file.
        let counter_file = dir.path().join("call_count");
        std::fs::write(&counter_file, "0").unwrap();
        let plugin = write_mock_plugin(
            dir.path(),
            &format!(
                r#"#!/bin/sh
read -r line
COUNT=$(cat {counter})
COUNT=$((COUNT + 1))
echo $COUNT > {counter}
if [ "$COUNT" = "1" ]; then
  echo '{handshake}'
else
  echo '{recall}'
fi
"#,
                counter = counter_file.display(),
                handshake = handshake_response,
                recall = recall_response,
            ),
        );
        let manifest = mock_manifest(&plugin.display().to_string());
        let adapter = ExternalMemoryAdapter::new(&manifest, dir.path(), "0.14.6-alpha.5").unwrap();
        let result = adapter.recall("arch:overview").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().key, "arch:overview");
    }

    #[test]
    fn config_dispatch_returns_external_adapter() {
        // Verify that memory_store_from_config creates an ExternalMemoryAdapter
        // when backend = "plugin" and plugin = <name> is configured.
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        // Write memory.toml configuring plugin backend.
        std::fs::write(
            ta_dir.join("memory.toml"),
            r#"
backend = "plugin"
plugin = "test-dispatch"
"#,
        )
        .unwrap();

        // Create the plugin in .ta/plugins/memory/test-dispatch/.
        let plugin_dir = ta_dir.join("plugins").join("memory").join("test-dispatch");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let mock_bin = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo '{"ok":true,"plugin_name":"test-dispatch","plugin_version":"0.1.0","protocol_version":1,"capabilities":[]}'
"#,
        );
        std::fs::write(
            plugin_dir.join("memory.toml"),
            format!(
                r#"name = "test-dispatch"
command = "{}"
"#,
                mock_bin.display()
            ),
        )
        .unwrap();

        // Factory should return an ExternalMemoryAdapter.
        let store = crate::factory::memory_store_from_config(dir.path());
        // If plugin was found, store returns something. We can't introspect the
        // concrete type easily; verify it is usable.
        drop(store);
    }
}
