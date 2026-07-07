# Plugin/Adapter/Connector Unification (v0.17.0.12.14) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the 4-category extensibility model from `docs/design/ta-concepts-and-architecture.md` §2.2 (Plugin / Channel-Listener / Backend / Resource-list) by extracting one shared JSON-over-stdio plugin transport+manifest+discovery crate, migrating VCS/messaging/social/agent-runtime plugins onto it, and bringing `EXTERNAL_TOOLS`, `DbProxyPlugin`, and `ReleaseAdapter` into the Plugin category as PLAN.md v0.17.0.12.14 requires.

**Architecture:** New crate `crates/ta-plugin` provides format-agnostic transport (spawn + ETXTBSY retry + newline-JSON framing + watchdog-thread timeout+SIGKILL, one copy instead of three), a canonical `PluginManifest` (superset of the VCS/messaging/social fields) and `.ta/plugins/<kind>/<name>/plugin.toml` discovery (project-local → user-global, matching VCS's existing order — the "most mature" of the three prior orders per the phase goal). Each existing domain (VCS/messaging/social/runtime) keeps its own request/response wire types unchanged (no breaking changes to already-published plugin binaries) but delegates spawn/framing/manifest/discovery to the shared crate. New Plugin-category integrations (tool, db, release) are built directly on the shared crate's generic `PluginRequest{method,params}` / `PluginResponse{ok,result,error}` envelope from the start.

**Tech Stack:** Rust, serde/serde_json, toml, thiserror, std::process (no async), libc (unix SIGKILL), tempfile (tests).

## Global Constraints

- Do not change the wire-level JSON shape of `VcsPluginRequest/Response`, `MessagingPluginRequest/Response`, or `SocialPluginRequest/Response` — real published plugin binaries under `plugins/` depend on today's bytes.
- Every new/changed manifest-driven discovery path lives under `.ta/plugins/<kind>/<name>/plugin.toml` (project-local) or `~/.config/ta/plugins/<kind>/<name>/plugin.toml` (user-global), per `docs/design/ta-concepts-and-architecture.md` §2.2.
- `crates/ta-plugin` has version `0.17.0-alpha.12.13` (workspace version) and no new external (non-workspace) dependencies beyond what's already in `[workspace.dependencies]` (serde, serde_json, thiserror, toml, libc, tempfile).
- Never disable or skip tests. Run `./dev cargo test --workspace` after every task.
- Mark PLAN.md items `[x]` immediately as each is implemented (see Task Completion Enforcement in the injected CLAUDE.md).

---

### Task 1: `ta-plugin` crate — envelope, error, transport

**Files:**
- Create: `crates/ta-plugin/Cargo.toml`
- Create: `crates/ta-plugin/src/lib.rs`
- Create: `crates/ta-plugin/src/envelope.rs`
- Create: `crates/ta-plugin/src/error.rs`
- Create: `crates/ta-plugin/src/transport.rs`
- Modify: `Cargo.toml:14-51` (add `"crates/ta-plugin",` to `[workspace] members`)

**Interfaces:**
- Produces (used by every later task):
  - `ta_plugin::envelope::{PluginRequest, PluginResponse, HandshakeParams, HandshakeResult, PROTOCOL_VERSION}`
  - `ta_plugin::error::PluginError` (thiserror enum: `NotFound{name}`, `CallFailed{name,method,reason}`, `InvalidResponse{name,method,reason}`, `SpawnFailed{command,reason}`, `Timeout{name,method,timeout_secs}`, `ManifestNotFound{path}`, `InvalidManifest{path,reason}`, `MissingCommand{path}`, `Io(#[from] std::io::Error)`, `Json(#[from] serde_json::Error)`)
  - `ta_plugin::transport::call_raw(name: &str, method: &str, command: &str, extra_args: &[String], work_dir: &Path, request_line: &str, timeout: Duration) -> Result<String, PluginError>` — spawns `command` (first whitespace token = program, rest + `extra_args` = args) in `work_dir`, retries spawn on Unix `ETXTBSY` (os error 26) up to 4 times with backoff `[0, 20, 80, 200]` ms, writes `request_line` + `"\n"` to stdin, waits for exit with a watchdog thread that `SIGKILL`s the child on timeout (unix; no-op elsewhere), returns the first line of stdout. Non-zero exit → `CallFailed` with trimmed stderr. Empty/missing first stdout line → `InvalidResponse`.
  - `ta_plugin::transport::call_json<Req: Serialize, Resp: DeserializeOwned>(name: &str, method: &str, command: &str, extra_args: &[String], work_dir: &Path, request: &Req, timeout: Duration) -> Result<Resp, PluginError>` — `serde_json::to_string(request)` then `call_raw`, then `serde_json::from_str` the returned line, mapping JSON errors to `InvalidResponse`.

**Cargo.toml contents:**
```toml
[package]
name = "ta-plugin"
version.workspace = true
edition = "2021"
description = "Shared JSON-over-stdio plugin transport, manifest, and discovery for Trusted Autonomy Plugin-category integrations"
license = "Apache-2.0"
repository = "https://github.com/trustedautonomy/ta"
homepage = "https://github.com/trustedautonomy/ta"
keywords = ["ai", "agent", "autonomy", "plugin"]
categories = ["development-tools"]

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
toml = { workspace = true }

[target.'cfg(unix)'.dependencies]
libc = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 1: Write `error.rs`**

```rust
//! Shared error type for all Plugin-category (§2.2) integrations.

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin '{name}' not found")]
    NotFound { name: String },
    #[error("plugin '{name}' method '{method}' failed: {reason}")]
    CallFailed {
        name: String,
        method: String,
        reason: String,
    },
    #[error("plugin '{name}' method '{method}' returned an invalid response: {reason}")]
    InvalidResponse {
        name: String,
        method: String,
        reason: String,
    },
    #[error("failed to spawn plugin command '{command}': {reason}")]
    SpawnFailed { command: String, reason: String },
    #[error("plugin '{name}' method '{method}' timed out after {timeout_secs}s")]
    Timeout {
        name: String,
        method: String,
        timeout_secs: u64,
    },
    #[error("plugin manifest not found at {path}")]
    ManifestNotFound { path: String },
    #[error("invalid plugin manifest at {path}: {reason}")]
    InvalidManifest { path: String, reason: String },
    #[error("plugin manifest at {path} is missing the required 'command' field")]
    MissingCommand { path: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
```

- [ ] **Step 2: Write `envelope.rs`**

```rust
//! The canonical `method:String` / `{ok,result,error}` call/response envelope
//! (§2.2 Plugin category) — the reference shape for new Plugin-category
//! integrations. Existing VCS/messaging/social wire types keep their own
//! shapes for backward compatibility; they use `transport::call_json` with
//! their own Req/Resp types instead of this envelope directly.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequest {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl PluginRequest {
    pub fn new(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginResponse {
    pub ok: bool,
    #[serde(default)]
    pub result: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PluginResponse {
    pub fn success(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: serde_json::Value::Null,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeParams {
    pub ta_version: String,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResult {
    pub plugin_version: String,
    pub protocol_version: u32,
    #[serde(default)]
    pub adapter_name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
```

- [ ] **Step 3: Write `transport.rs`**

```rust
//! Shared spawn + newline-delimited-JSON framing + timeout transport, used by
//! every Plugin-category integration (VCS, messaging, social, agent-runtime,
//! tool, db, release). Format-agnostic: `call_json` is generic over the
//! caller's own request/response types, so it does not change any existing
//! plugin's wire bytes.

use crate::error::PluginError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const ETXTBSY_BACKOFF_MS: [u64; 4] = [0, 20, 80, 200];

fn spawn_with_retry(command: &str, extra_args: &[String], work_dir: &Path) -> Result<Child, PluginError> {
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| PluginError::SpawnFailed {
            command: command.to_string(),
            reason: "empty command".to_string(),
        })?;
    let baked_args: Vec<&str> = parts.collect();

    let mut last_err = None;
    for delay_ms in ETXTBSY_BACKOFF_MS {
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        let mut cmd = Command::new(program);
        cmd.args(&baked_args)
            .args(extra_args)
            .current_dir(work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(26) => {
                last_err = Some(e);
                continue;
            }
            Err(e) => {
                return Err(PluginError::SpawnFailed {
                    command: command.to_string(),
                    reason: e.to_string(),
                })
            }
        }
    }
    Err(PluginError::SpawnFailed {
        command: command.to_string(),
        reason: last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "ETXTBSY retry exhausted".to_string()),
    })
}

/// Wait for `child` to exit, killing it after `timeout` via a watchdog thread.
fn wait_with_timeout(mut child: Child, timeout: Duration) -> Result<std::process::Output, PluginError> {
    let pid = child.id();
    let (tx, rx) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    });
    let started = Instant::now();
    let output = child.wait_with_output();
    let _ = tx.send(());
    let _ = watchdog.join();
    let output = output?;
    if started.elapsed() >= timeout && !output.status.success() {
        return Err(PluginError::Timeout {
            name: String::new(),
            method: String::new(),
            timeout_secs: timeout.as_secs(),
        });
    }
    Ok(output)
}

/// Format-agnostic call: write `request_line` + "\n" to the plugin's stdin,
/// return the first line of stdout. Non-zero exit or empty stdout is an error.
#[allow(clippy::too_many_arguments)]
pub fn call_raw(
    name: &str,
    method: &str,
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request_line: &str,
    timeout: Duration,
) -> Result<String, PluginError> {
    let mut child = spawn_with_retry(command, extra_args, work_dir)?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| PluginError::CallFailed {
            name: name.to_string(),
            method: method.to_string(),
            reason: "plugin process has no stdin".to_string(),
        })?;
        stdin.write_all(request_line.as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    let output = wait_with_timeout(child, timeout).map_err(|e| match e {
        PluginError::Timeout { .. } => PluginError::Timeout {
            name: name.to_string(),
            method: method.to_string(),
            timeout_secs: timeout.as_secs(),
        },
        other => other,
    })?;

    if !output.status.success() {
        let mut stderr = String::new();
        let _ = std::io::Cursor::new(&output.stderr).read_to_string(&mut stderr);
        return Err(PluginError::CallFailed {
            name: name.to_string(),
            method: method.to_string(),
            reason: stderr.trim().to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("");
    if first_line.is_empty() {
        return Err(PluginError::InvalidResponse {
            name: name.to_string(),
            method: method.to_string(),
            reason: "plugin wrote no output line to stdout".to_string(),
        });
    }
    Ok(first_line.to_string())
}

/// JSON convenience wrapper over `call_raw`, generic over the caller's own
/// request/response wire types (VCS/messaging/social keep their existing
/// types; new integrations use `envelope::{PluginRequest,PluginResponse}`).
#[allow(clippy::too_many_arguments)]
pub fn call_json<Req: Serialize, Resp: DeserializeOwned>(
    name: &str,
    method: &str,
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request: &Req,
    timeout: Duration,
) -> Result<Resp, PluginError> {
    let line = serde_json::to_string(request)?;
    let response_line = call_raw(name, method, command, extra_args, work_dir, &line, timeout)?;
    serde_json::from_str(&response_line).map_err(|e| PluginError::InvalidResponse {
        name: name.to_string(),
        method: method.to_string(),
        reason: format!("{e} (raw: {})", truncate(&response_line, 200)),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn write_mock_script(dir: &Path, body: &str) -> String {
        let path = dir.join("mock-plugin.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn call_raw_round_trips_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(dir.path(), "read -r line\necho '{\"ok\":true,\"result\":{}}'");
        let result = call_raw("mock", "ping", &script, &[], dir.path(), "{\"method\":\"ping\"}", Duration::from_secs(5)).unwrap();
        assert_eq!(result, "{\"ok\":true,\"result\":{}}");
    }

    #[cfg(unix)]
    #[test]
    fn call_json_deserializes_response() {
        use crate::envelope::{PluginRequest, PluginResponse};
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(dir.path(), "read -r line\necho '{\"ok\":true,\"result\":{\"pong\":true}}'");
        let req = PluginRequest::new("ping", serde_json::json!({}));
        let resp: PluginResponse = call_json("mock", "ping", &script, &[], dir.path(), &req, Duration::from_secs(5)).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result["pong"], serde_json::json!(true));
    }

    #[cfg(unix)]
    #[test]
    fn call_raw_reports_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(dir.path(), "echo 'boom' >&2\nexit 1");
        let err = call_raw("mock", "ping", &script, &[], dir.path(), "{}", Duration::from_secs(5)).unwrap_err();
        match err {
            PluginError::CallFailed { reason, .. } => assert!(reason.contains("boom")),
            other => panic!("expected CallFailed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn call_raw_kills_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_mock_script(dir.path(), "sleep 5\necho '{\"ok\":true,\"result\":{}}'");
        let err = call_raw("mock", "ping", &script, &[], dir.path(), "{}", Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, PluginError::Timeout { .. }));
    }
}
```

- [ ] **Step 4: Write `lib.rs`**

```rust
//! Shared JSON-over-stdio plugin transport, manifest schema, and discovery
//! convention for every Trusted Autonomy Plugin-category integration
//! (docs/design/ta-concepts-and-architecture.md §2.2): VCS, messaging,
//! social, agent-runtime, tool, db, and release plugins all use this crate.

pub mod discovery;
pub mod envelope;
pub mod error;
pub mod manifest;
pub mod transport;

pub use envelope::{HandshakeParams, HandshakeResult, PluginRequest, PluginResponse, PROTOCOL_VERSION};
pub use error::PluginError;
pub use manifest::PluginManifest;
pub use discovery::{discover_plugins, find_plugin, user_config_dir, DiscoveredPlugin, PluginSource};
```

- [ ] **Step 5: Add `crates/ta-plugin` to workspace members**

In `Cargo.toml`, add `"crates/ta-plugin",` to the `members` array (alphabetically near `"crates/ta-policy",` — after `"crates/ta-output-schema",`).

- [ ] **Step 6: Build and test (manifest/discovery come in Task 2, so this compiles once Task 2's files exist too — do Steps 1-5 then proceed directly to Task 2 before building)**

---

### Task 2: `ta-plugin` crate — manifest + discovery

**Files:**
- Create: `crates/ta-plugin/src/manifest.rs`
- Create: `crates/ta-plugin/src/discovery.rs`

**Interfaces:**
- Consumes: `ta_plugin::error::PluginError` (Task 1)
- Produces:
  - `ta_plugin::manifest::PluginManifest { name: String, version: String, kind: String, command: String, args: Vec<String>, capabilities: Vec<String>, description: Option<String>, timeout_secs: Option<u64>, protocol_version: Option<u32>, min_daemon_version: Option<String>, source_url: Option<String>, staging_env: HashMap<String,String> }` with `PluginManifest::load(path: &Path) -> Result<Self, PluginError>` and `PluginManifest::validate(&self, expected_kind: &str) -> Result<(), PluginError>`. `timeout_secs` is `Option` (not defaulted in this shared struct) so each domain can keep applying its own historical default (VCS 30s, messaging/social 60s) without a behavior change.
  - `ta_plugin::discovery::{PluginSource::{ProjectLocal,UserGlobal}, DiscoveredPlugin{manifest,plugin_dir,source}, discover_plugins(kind: &str, project_root: &Path) -> Vec<DiscoveredPlugin>, find_plugin(kind: &str, name: &str, project_root: &Path) -> Option<DiscoveredPlugin>, user_config_dir() -> Option<PathBuf>}`. Search order is project-local (`<project_root>/.ta/plugins/<kind>/*/plugin.toml`) then user-global (`~/.config/ta/plugins/<kind>/*/plugin.toml`, via `XDG_CONFIG_HOME` then `HOME/.config`) — this is VCS's existing order, chosen as canonical per the phase goal ("matching VCS's existing...shape (most mature)"). No PATH-fallback synthesis here — each domain keeps constructing its own PATH-fallback manifest (their assumed-capabilities lists differ intentionally; unifying that silently would hide real per-domain capability assumptions).

- [ ] **Step 1: Write `manifest.rs`**

```rust
//! The canonical `plugin.toml` manifest schema (§2.2), a superset of the
//! fields historically split across VcsPluginManifest/MessagingPluginManifest/
//! SocialPluginManifest.

use crate::error::PluginError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

fn default_version() -> String {
    "0.1.0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub protocol_version: Option<u32>,
    #[serde(default)]
    pub min_daemon_version: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub staging_env: HashMap<String, String>,
}

impl PluginManifest {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        if !path.exists() {
            return Err(PluginError::ManifestNotFound {
                path: path.display().to_string(),
            });
        }
        let text = std::fs::read_to_string(path)?;
        let manifest: PluginManifest = toml::from_str(&text).map_err(|e| PluginError::InvalidManifest {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        if manifest.command.trim().is_empty() {
            return Err(PluginError::MissingCommand {
                path: path.display().to_string(),
            });
        }
        Ok(manifest)
    }

    pub fn validate(&self, expected_kind: &str) -> Result<(), PluginError> {
        if self.kind != expected_kind {
            return Err(PluginError::InvalidManifest {
                path: self.name.clone(),
                reason: format!("expected type = \"{expected_kind}\", found \"{}\"", self.kind),
            });
        }
        Ok(())
    }

    pub fn timeout(&self, default_secs: u64) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.unwrap_or(default_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_minimal_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugin.toml");
        std::fs::write(&path, "name = \"demo\"\ntype = \"vcs\"\ncommand = \"demo-plugin\"\n").unwrap();
        let manifest = PluginManifest::load(&path).unwrap();
        assert_eq!(manifest.name, "demo");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.timeout_secs, None);
        assert!(manifest.validate("vcs").is_ok());
        assert!(manifest.validate("messaging").is_err());
    }

    #[test]
    fn missing_command_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugin.toml");
        std::fs::write(&path, "name = \"demo\"\ntype = \"vcs\"\ncommand = \"\"\n").unwrap();
        assert!(matches!(
            PluginManifest::load(&path),
            Err(PluginError::MissingCommand { .. })
        ));
    }

    #[test]
    fn missing_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope").join("plugin.toml");
        assert!(matches!(
            PluginManifest::load(&path),
            Err(PluginError::ManifestNotFound { .. })
        ));
    }
}
```

- [ ] **Step 2: Write `discovery.rs`**

```rust
//! `.ta/plugins/<kind>/<name>/plugin.toml` discovery convention (§2.2),
//! shared by every Plugin-category integration.

use crate::manifest::PluginManifest;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    ProjectLocal,
    UserGlobal,
}

impl fmt::Display for PluginSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginSource::ProjectLocal => write!(f, "project"),
            PluginSource::UserGlobal => write!(f, "global"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub plugin_dir: PathBuf,
    pub source: PluginSource,
}

/// `$XDG_CONFIG_HOME` if set, else `$HOME/.config`. Intentionally not
/// macOS-special-cased, matching the VCS/messaging/social behavior being
/// unified (changing this would alter where existing plugins are found).
pub fn user_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    std::env::var("HOME").ok().map(|home| PathBuf::from(home).join(".config"))
}

fn scan_kind_dir(base: &Path, kind: &str, source: PluginSource) -> Vec<DiscoveredPlugin> {
    let kind_dir = base.join("plugins").join(kind);
    let Ok(entries) = std::fs::read_dir(&kind_dir) else {
        return vec![];
    };
    let mut found = vec![];
    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let manifest_path = plugin_dir.join("plugin.toml");
        if let Ok(manifest) = PluginManifest::load(&manifest_path) {
            found.push(DiscoveredPlugin {
                manifest,
                plugin_dir,
                source,
            });
        }
    }
    found.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    found
}

/// Discover every `<kind>` plugin, project-local first then user-global.
/// If a name exists in both, both entries are returned (callers that want a
/// single result per name should use `find_plugin`, which takes the first
/// match in this same project-then-global order).
pub fn discover_plugins(kind: &str, project_root: &Path) -> Vec<DiscoveredPlugin> {
    let mut found = scan_kind_dir(&project_root.join(".ta"), kind, PluginSource::ProjectLocal);
    if let Some(config_dir) = user_config_dir() {
        found.extend(scan_kind_dir(&config_dir.join("ta"), kind, PluginSource::UserGlobal));
    }
    found
}

pub fn find_plugin(kind: &str, name: &str, project_root: &Path) -> Option<DiscoveredPlugin> {
    discover_plugins(kind, project_root)
        .into_iter()
        .find(|p| p.manifest.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, kind: &str, name: &str) {
        let plugin_dir = dir.join(".ta").join("plugins").join(kind).join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            format!("name = \"{name}\"\ntype = \"{kind}\"\ncommand = \"{name}-bin\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn discovers_project_local_plugin() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "vcs", "perforce");
        let found = discover_plugins("vcs", dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.name, "perforce");
        assert_eq!(found[0].source, PluginSource::ProjectLocal);
    }

    #[test]
    fn find_plugin_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_plugin("vcs", "nope", dir.path()).is_none());
    }

    #[test]
    fn ignores_kind_dir_with_no_plugins() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_plugins("vcs", dir.path()).is_empty());
    }
}
```

- [ ] **Step 3: Build**

Run: `./dev cargo build -p ta-plugin`
Expected: builds clean.

- [ ] **Step 4: Test**

Run: `./dev cargo test -p ta-plugin`
Expected: all tests pass (transport: 4, manifest: 3, discovery: 3).

- [ ] **Step 5: Clippy + fmt**

Run: `./dev cargo clippy -p ta-plugin --all-targets -- -D warnings && ./dev cargo fmt -p ta-plugin -- --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/ta-plugin
git commit -m "feat(ta-plugin): add shared plugin transport, manifest, and discovery crate"
```

---

### Task 3: Migrate VCS plugin transport onto `ta_plugin::transport`

**Files:**
- Modify: `crates/ta-submit/Cargo.toml` (add `ta-plugin` dependency)
- Modify: `crates/ta-submit/src/external_vcs_adapter.rs` (replace private `call_plugin`/`wait_with_timeout`/spawn-retry with `ta_plugin::transport::call_json`)

**Interfaces:**
- Consumes: `ta_plugin::transport::call_json::<VcsPluginRequest, VcsPluginResponse>` (Task 1)
- Produces: `ExternalVcsAdapter` keeps its exact existing public API (`SourceAdapter` impl) — no signature changes for any caller outside this file.

- [ ] **Step 1: Add dependency**

In `crates/ta-submit/Cargo.toml`, add under `[dependencies]`:
```toml
ta-plugin = { path = "../ta-plugin", version = "0.17.0-alpha.12.13" }
```

- [ ] **Step 2: Locate and replace the transport internals**

In `crates/ta-submit/src/external_vcs_adapter.rs`, find the private `call_plugin` function (spawn + ETXTBSY retry + `wait_with_timeout`) and its `wait_with_timeout` helper. Replace the body of `call_plugin` with a call into the shared crate, preserving the existing function signature (`fn call_plugin(command: &str, extra_args: &[String], work_dir: &Path, request: &VcsPluginRequest, timeout: Duration) -> Result<VcsPluginResponse>`) so every call site in the file is unaffected:

```rust
fn call_plugin(
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request: &VcsPluginRequest,
    timeout: Duration,
) -> Result<VcsPluginResponse> {
    ta_plugin::transport::call_json(
        "vcs-plugin",
        &request.method,
        command,
        extra_args,
        work_dir,
        request,
        timeout,
    )
    .map_err(|e| SubmitError::VcsError(e.to_string()))
}
```

Delete the now-unused private `wait_with_timeout` function and its `ETXTBSY`/backoff constants from this file (they moved to `ta_plugin::transport`). Leave everything else in the file (handshake logic, capability checks, `SourceAdapter` impl) untouched.

- [ ] **Step 3: Build**

Run: `./dev cargo build -p ta-submit`
Expected: builds clean (watch for now-unused imports like `libc`, `mpsc` — remove them if `cargo build` warns).

- [ ] **Step 4: Test**

Run: `./dev cargo test -p ta-submit vcs`
Expected: `vcs_plugin_lifecycle` and `vcs_perforce_plugin` integration tests still pass unchanged (they exercise `ExternalVcsAdapter` end-to-end, so this proves the transport swap didn't change behavior).

- [ ] **Step 5: Full workspace test**

Run: `./dev cargo test --workspace`
Expected: all pass.

- [ ] **Step 6: Clippy + fmt**

Run: `./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`

- [ ] **Step 7: Commit**

```bash
git add crates/ta-submit
git commit -m "refactor(ta-submit): delegate VCS plugin transport to shared ta-plugin crate"
```

---

### Task 4: Migrate messaging + social plugin transport onto `ta_plugin::transport`

**Files:**
- Modify: `crates/ta-submit/src/messaging_adapter.rs` (replace private `call_plugin`/`wait_with_timeout`/spawn-retry)
- Modify: `crates/ta-submit/src/social_adapter.rs` (same)

**Interfaces:**
- Consumes: `ta_plugin::transport::call_json::<MessagingPluginRequest, MessagingPluginResponse>` and `::<SocialPluginRequest, SocialPluginResponse>`
- Produces: `ExternalMessagingAdapter`/`ExternalSocialAdapter` keep their exact existing public methods (`fetch`, `create_draft`, `draft_status`, `health`, `create_scheduled`, etc.) — no signature changes.

- [ ] **Step 1: `messaging_adapter.rs`**

Replace the private `call_plugin` method on `ExternalMessagingAdapter` with:

```rust
fn call_plugin(&self, req: &MessagingPluginRequest, op: &str) -> Result<MessagingPluginResponse, MessagingPluginError> {
    let resp: MessagingPluginResponse = ta_plugin::transport::call_json(
        &self.provider,
        op,
        &self.command,
        &self.args,
        Path::new("."),
        req,
        self.timeout,
    )
    .map_err(|e| match e {
        ta_plugin::PluginError::Timeout { timeout_secs, .. } => MessagingPluginError::Timeout {
            name: self.provider.clone(),
            op: op.to_string(),
            timeout_secs,
        },
        ta_plugin::PluginError::SpawnFailed { command, reason } => {
            MessagingPluginError::SpawnFailed { command, reason }
        }
        other => MessagingPluginError::InvalidResponse {
            name: self.provider.clone(),
            op: op.to_string(),
            reason: other.to_string(),
        },
    })?;
    if !resp.ok {
        return Err(MessagingPluginError::OpFailed {
            name: self.provider.clone(),
            op: op.to_string(),
            reason: resp.error.clone().unwrap_or_default(),
        });
    }
    Ok(resp)
}
```

Note the existing call sites pass `work_dir: &Path` implicitly via `Path::new(".")` — check the current `call_plugin` signature first: if it already threads a real `work_dir` (e.g. `self.work_dir` or a param), keep using that value instead of `Path::new(".")` — read the surrounding code before editing to preserve identical spawn `current_dir` behavior. Delete the file's private `wait_with_timeout` and its ETXTBSY loop.

- [ ] **Step 2: `social_adapter.rs`**

Same transformation for `ExternalSocialAdapter::call_plugin`, mapping errors to `SocialPluginError` variants instead of `MessagingPluginError`. Remember social's current `call_plugin` has **no** ETXTBSY retry loop (per the earlier research) — using `ta_plugin::transport::call_json` *adds* ETXTBSY retry that didn't exist before. This is a deliberate, safe improvement (retrying a transient spawn error can only help, never break passing tests) — note it in `.ta-decisions.json` (Task 11) rather than trying to suppress it.

- [ ] **Step 3: Add dependency (already added in Task 3 to `ta-submit/Cargo.toml` — confirm it's present, no new edit needed)**

- [ ] **Step 4: Build + test**

Run: `./dev cargo build -p ta-submit && ./dev cargo test -p ta-submit`
Expected: messaging/social unit tests (embedded `#[cfg(test)]` modules in both files) pass unchanged.

- [ ] **Step 5: Full workspace test, clippy, fmt**

Run: `./dev cargo test --workspace && ./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`

- [ ] **Step 6: Commit**

```bash
git add crates/ta-submit
git commit -m "refactor(ta-submit): delegate messaging/social plugin transport to shared ta-plugin crate"
```

---

### Task 5: Migrate agent-runtime plugin transport + add manifest/discovery

**Files:**
- Modify: `crates/ta-runtime/Cargo.toml` (add `ta-plugin` dependency)
- Modify: `crates/ta-runtime/src/plugin.rs` (delegate `PluginProcess::call` framing to `ta_plugin::transport`; add manifest-based discovery)

**Interfaces:**
- Consumes: `ta_plugin::transport::call_json`, `ta_plugin::{PluginManifest, discover_plugins, find_plugin}`
- Produces: `ExternalRuntimeAdapter::new(plugin_path: &Path, runtime_name: &str) -> Result<Self>` (unchanged, kept for explicit-path callers) plus a new `ExternalRuntimeAdapter::discover(name: &str, project_root: &Path) -> Result<Self>` that resolves `command` via `ta_plugin::find_plugin("agent", name, project_root)` before falling back to the bare binary name on `PATH`.

- [ ] **Step 1: Add dependency**

`crates/ta-runtime/Cargo.toml`, under `[dependencies]`:
```toml
ta-plugin = { path = "../ta-plugin", version = "0.17.0-alpha.12.13" }
```

- [ ] **Step 2: Replace `PluginProcess::call`'s framing**

In `crates/ta-runtime/src/plugin.rs`, `PluginProcess` currently owns its own `Child`/`stdin`/`stdout_reader` because it is long-lived (spawn once, many calls) — unlike VCS/messaging/social's per-call spawn. `ta_plugin::transport::call_raw` spawns a fresh process per call, so it is **not** a drop-in replacement for `PluginProcess::call`'s per-line read/write over an already-running child. Instead, extract just the line-framing (not the spawn) into a small shared-shape helper: keep `PluginProcess`'s existing `Child`/`stdin`/`stdout_reader` fields and its long-lived spawn in `ExternalRuntimeAdapter::new`, but replace the manual `serde_json::to_string` + `write_all` + `read_line` + `serde_json::from_str` sequence inside `call()` with calls to two new small `pub` functions added to `ta_plugin::transport` in this task:

```rust
// in crates/ta-plugin/src/transport.rs, added alongside call_raw/call_json:

/// Write one JSON request line to an already-open stdin handle (long-lived
/// process framing, e.g. agent-runtime plugins that stay alive across calls).
pub fn write_line<Req: Serialize>(stdin: &mut impl Write, request: &Req) -> Result<(), PluginError> {
    let line = serde_json::to_string(request)?;
    stdin.write_all(line.as_bytes())?;
    stdin.write_all(b"\n")?;
    Ok(())
}

/// Read one JSON response line from an already-open stdout reader.
pub fn read_line<Resp: DeserializeOwned>(reader: &mut impl std::io::BufRead) -> Result<Resp, PluginError> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(PluginError::InvalidResponse {
            name: String::new(),
            method: String::new(),
            reason: "plugin wrote no output line to stdout".to_string(),
        });
    }
    serde_json::from_str(&line).map_err(PluginError::from)
}
```

Then in `plugin.rs`, `PluginProcess::call` becomes:
```rust
fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let request = PluginRequest { method: method.to_string(), params };
    ta_plugin::transport::write_line(&mut self.stdin, &request)
        .map_err(|e| RuntimeError::PluginError(e.to_string()))?;
    let response: PluginResponse = ta_plugin::transport::read_line(&mut self.stdout_reader)
        .map_err(|e| RuntimeError::PluginError(e.to_string()))?;
    if response.ok {
        Ok(response.result)
    } else {
        Err(RuntimeError::PluginError(response.error.unwrap_or_default()))
    }
}
```
(Keep this file's own `PluginRequest`/`PluginResponse` structs as-is — they are structurally identical to `ta_plugin::envelope`'s but renaming/removing them is out of scope for this task; only the framing internals move to the shared crate.)

- [ ] **Step 3: Add manifest-based discovery**

Add a new function in `plugin.rs`:
```rust
impl ExternalRuntimeAdapter {
    /// Resolve `name` via `.ta/plugins/agent/<name>/plugin.toml` discovery
    /// (project-local then user-global), falling back to treating `name`
    /// as a bare command on PATH if no manifest is found.
    pub fn discover(name: &str, project_root: &Path) -> Result<Self> {
        let command = match ta_plugin::find_plugin("agent", name, project_root) {
            Some(found) => found.plugin_dir.join(&found.manifest.command),
            None => PathBuf::from(name),
        };
        Self::new(&command, name)
    }
}
```

- [ ] **Step 4: Build + test**

Run: `./dev cargo build -p ta-runtime && ./dev cargo test -p ta-runtime`
Expected: existing runtime plugin tests pass; add one new test in `plugin.rs`'s `#[cfg(test)]` module:

```rust
#[test]
fn discover_falls_back_to_bare_name_when_no_manifest() {
    let dir = tempfile::tempdir().unwrap();
    // No .ta/plugins/agent/<name>/plugin.toml exists — discover() must not
    // panic and must fall back to a bare-name command (which then fails to
    // spawn, since "definitely-not-a-real-binary" doesn't exist on PATH).
    let result = ExternalRuntimeAdapter::discover("definitely-not-a-real-binary", dir.path());
    assert!(result.is_err());
}
```

- [ ] **Step 5: Full workspace test, clippy, fmt**

Run: `./dev cargo test --workspace && ./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`

- [ ] **Step 6: Commit**

```bash
git add crates/ta-runtime
git commit -m "feat(ta-runtime): delegate plugin line framing to ta-plugin, add manifest-based discovery"
```

---

### Task 6: Migrate `EXTERNAL_TOOLS` onto the Plugin category

**Files:**
- Modify: `apps/ta-cli/Cargo.toml` (add `ta-plugin` dependency)
- Modify: `apps/ta-cli/src/commands/tools.rs` (change `ExternalTool` fields from `&'static str` to `String`; add manifest-backed discovery merged with 3 built-in defaults)
- Create: `plugins/tool/superpowers/plugin.toml`
- Create: `plugins/tool/bmad/plugin.toml`
- Create: `plugins/tool/meridian/plugin.toml`
- Modify: `apps/ta-cli/src/commands/onboard.rs` (adjust to `String`-based `ExternalTool` if it pattern-matches on `&'static str`)
- Modify: `apps/ta-cli/src/commands/release.rs` (same adjustment for `validate_third_party_plugins`)

**Interfaces:**
- Consumes: `ta_plugin::{PluginManifest, discover_plugins}`
- Produces: `pub fn external_tools(project_root: &Path) -> Vec<ExternalTool>` (replaces the `EXTERNAL_TOOLS` const array as the thing callers iterate; `ExternalTool`'s field types change from `&'static str` to `String` and `install: ExternalToolInstall` gains owned-`String` variants). `check_tool_installed(tool: &ExternalTool) -> bool` keeps its existing signature (works the same on `&ExternalTool` regardless of field ownership).

- [ ] **Step 1: Design the tool manifest extension**

Tool manifests need 3 fields beyond the shared `PluginManifest` (`label`, `detect_command`, `install_hint`) plus an install spec. Rather than inventing a second manifest type, encode these as an optional `[tool]` TOML table alongside the standard `PluginManifest` fields, parsed as a second pass over the same file:

```rust
// in tools.rs
#[derive(Debug, Clone, serde::Deserialize)]
struct ToolExtra {
    tool: ToolExtraFields,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ToolExtraFields {
    label: String,
    detect_command: String,
    install_hint: String,
    install: ToolInstallSpec,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ToolInstallSpec {
    Cargo { package: String },
    Npm { package: String },
    Git { url: String, dest: String },
    ClaudePlugin { spec: String },
}
```

- [ ] **Step 2: Rewrite `ExternalTool`/`ExternalToolInstall` with owned `String` fields**

```rust
#[derive(Debug, Clone)]
pub struct ExternalTool {
    pub name: String,
    pub label: String,
    pub description: String,
    pub detect_command: String,
    pub install_hint: String,
    pub install: ExternalToolInstall,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ExternalToolInstall {
    Cargo(String),
    Npm(String),
    Git { url: String, dest: String },
    ClaudePlugin(String),
}
```

- [ ] **Step 3: Write the 3 bundled default manifests**

`plugins/tool/superpowers/plugin.toml`:
```toml
name = "superpowers"
type = "tool"
command = ""
description = "Agent skills and orchestration plugin for Claude Code"

[tool]
label = "Superpowers Claude Code plugin"
detect_command = "claude-plugin:superpowers"
install_hint = "claude plugin install superpowers@superpowers-dev"

[tool.install]
kind = "claude_plugin"
spec = "superpowers@superpowers-dev"
```

`plugins/tool/bmad/plugin.toml`:
```toml
name = "bmad"
type = "tool"
command = ""
description = "Structured multi-role planning: Analyst, Architect, PM roles"

[tool]
label = "BMAD planning library"
detect_command = "test -d ~/.bmad/agents"
install_hint = "git clone --depth=1 https://github.com/bmadcode/bmad-method ~/.bmad"

[tool.install]
kind = "git"
url = "https://github.com/bmadcode/bmad-method"
dest = "~/.bmad"
```

`plugins/tool/meridian/plugin.toml`:
```toml
name = "meridian"
type = "tool"
command = ""
description = "Velocity reports, cost-per-phase, and goal-alignment scoring (cargo)"

[tool]
label = "Meridian KPI analytics"
detect_command = "meridian --version"
install_hint = "cargo install meridian"

[tool.install]
kind = "cargo"
package = "meridian"
```
(`command = ""` is deliberate: these are not call/response process plugins — `PluginManifest::load`'s `MissingCommand` check must be bypassed for tool-kind manifests specifically, since a "tool" doesn't get spawned via `handshake`/`method` calls; add a `PluginManifest::load_allow_empty_command(path) -> Result<Self, PluginError>` variant used only by tool discovery, or check `kind == "tool"` before enforcing `MissingCommand` inside `load()`. Prefer adding the `kind` check directly in `PluginManifest::load()` in `crates/ta-plugin/src/manifest.rs` — `if self.command.trim().is_empty() && self.kind != "tool" { return Err(MissingCommand...) }` — since "a declarative-only plugin kind with no executable" is a legitimate general case, not tool-specific plumbing.)

- [ ] **Step 4: Implement discovery + merge**

```rust
pub fn external_tools(project_root: &Path) -> Vec<ExternalTool> {
    let mut tools = vec![];
    for discovered in ta_plugin::discover_plugins("tool", project_root) {
        let manifest_path = discovered.plugin_dir.join("plugin.toml");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else { continue };
        let Ok(extra) = toml::from_str::<ToolExtra>(&text) else { continue };
        tools.push(ExternalTool {
            name: discovered.manifest.name,
            label: extra.tool.label,
            description: discovered.manifest.description.unwrap_or_default(),
            detect_command: extra.tool.detect_command,
            install_hint: extra.tool.install_hint,
            install: match extra.tool.install {
                ToolInstallSpec::Cargo { package } => ExternalToolInstall::Cargo(package),
                ToolInstallSpec::Npm { package } => ExternalToolInstall::Npm(package),
                ToolInstallSpec::Git { url, dest } => ExternalToolInstall::Git { url, dest },
                ToolInstallSpec::ClaudePlugin { spec } => ExternalToolInstall::ClaudePlugin(spec),
            },
        });
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}
```

Since the 3 bundled manifests above live under `plugins/tool/<name>/plugin.toml` in the TA repo itself (not `.ta/plugins/tool/`), and `ta_plugin::discover_plugins` only scans `<project_root>/.ta/plugins/<kind>` and `~/.config/ta/plugins/<kind>`, add a third scan root specific to this call site — the TA installation's own bundled `plugins/` directory — by calling `ta_plugin::discovery` twice: once against `project_root` (for `.ta/plugins/tool/`, community-added), and once directly reading `env!("CARGO_MANIFEST_DIR")/../../plugins/tool` is fragile for an installed binary. Instead, bundle the 3 defaults as Rust data (a `const` array exactly like today, but now feeding the *same* `ExternalTool` struct) and merge community-discovered ones on top:

```rust
fn built_in_tools() -> Vec<ExternalTool> {
    // Same 3 entries as the old EXTERNAL_TOOLS const, now producing owned ExternalTool values.
    vec![
        ExternalTool {
            name: "superpowers".into(),
            label: "Superpowers Claude Code plugin".into(),
            description: "Agent skills and orchestration plugin for Claude Code".into(),
            detect_command: "claude-plugin:superpowers".into(),
            install_hint: "claude plugin install superpowers@superpowers-dev".into(),
            install: ExternalToolInstall::ClaudePlugin("superpowers@superpowers-dev".into()),
        },
        ExternalTool {
            name: "bmad".into(),
            label: "BMAD planning library".into(),
            description: "Structured multi-role planning: Analyst, Architect, PM roles".into(),
            detect_command: "test -d ~/.bmad/agents".into(),
            install_hint: "git clone --depth=1 https://github.com/bmadcode/bmad-method ~/.bmad".into(),
            install: ExternalToolInstall::Git {
                url: "https://github.com/bmadcode/bmad-method".into(),
                dest: "~/.bmad".into(),
            },
        },
        ExternalTool {
            name: "meridian".into(),
            label: "Meridian KPI analytics".into(),
            description: "Velocity reports, cost-per-phase, and goal-alignment scoring (cargo)".into(),
            detect_command: "meridian --version".into(),
            install_hint: "cargo install meridian".into(),
            install: ExternalToolInstall::Cargo("meridian".into()),
        },
    ]
}

pub fn external_tools(project_root: &Path) -> Vec<ExternalTool> {
    let mut tools = built_in_tools();
    for discovered in ta_plugin::discover_plugins("tool", project_root) {
        if tools.iter().any(|t| t.name == discovered.manifest.name) {
            continue; // built-ins win on name collision
        }
        // ... parse ToolExtra from discovered.plugin_dir/plugin.toml as in Step 4, push if Ok
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}
```
This keeps the 3 built-ins working exactly as before (same data, now behind a function instead of a `const`) while making `plugins/tool/*/plugin.toml` (Step 3's files) serve as **documentation/reference examples** of the manifest format a community member would author under `.ta/plugins/tool/<name>/plugin.toml` in their own project — which is exactly what `discover_plugins("tool", project_root)` picks up. Item 3's requirement ("a community member can add a new tool without a TA core PR") is satisfied by the `.ta/plugins/tool/<name>/plugin.toml` discovery path, independent of the 3 bundled defaults.

- [ ] **Step 5: Update call sites**

- `apps/ta-cli/src/commands/tools.rs`: replace every reference to the old `EXTERNAL_TOOLS` const with `external_tools(project_root)` — thread `project_root: &Path` into `list_tools`/`install_tool` (check current signatures first; both likely already take or can derive a project root from `std::env::current_dir()`).
- `apps/ta-cli/src/commands/onboard.rs`: same — replace `EXTERNAL_TOOLS.iter()` with `external_tools(&project_root).iter()`.
- `apps/ta-cli/src/commands/release.rs:2794` (`validate_third_party_plugins`): same replacement.
- Fix any `&'static str` pattern-matches on `tool.name`/`tool.label` etc. that no longer typecheck against `String` (use `.as_str()` or `tool.name == "meridian"` which works identically for both types via `PartialEq<str>`/`Deref`).

- [ ] **Step 6: Update/add tests**

The existing unit test in `tools.rs` asserting `meridian` is present should become:
```rust
#[test]
fn built_in_tools_include_meridian() {
    let dir = tempfile::tempdir().unwrap();
    let tools = external_tools(dir.path());
    assert!(tools.iter().any(|t| t.name == "meridian"));
}
```
Add a new test proving community extensibility (this doubles as groundwork for Task 9's cross-family proof):
```rust
#[test]
fn discovers_community_authored_tool_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join(".ta/plugins/tool/widget-cli");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        r#"
name = "widget-cli"
type = "tool"
command = ""
description = "Community widget CLI"

[tool]
label = "Widget CLI"
detect_command = "widget --version"
install_hint = "cargo install widget-cli"

[tool.install]
kind = "cargo"
package = "widget-cli"
"#,
    )
    .unwrap();
    let tools = external_tools(dir.path());
    assert!(tools.iter().any(|t| t.name == "widget-cli"));
}
```

- [ ] **Step 7: Build + test**

Run: `./dev cargo build -p ta-cli && ./dev cargo test -p ta-cli tools onboard release`
Expected: all pass.

- [ ] **Step 8: Full workspace test, clippy, fmt**

Run: `./dev cargo test --workspace && ./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`

- [ ] **Step 9: Commit**

```bash
git add apps/ta-cli plugins/tool
git commit -m "feat(ta-cli): migrate EXTERNAL_TOOLS onto the Plugin category with .ta/plugins/tool discovery"
```

---

### Task 7: Migrate `DbProxyPlugin` onto the Plugin category + `.ta/db-adapters.toml` registry

**Files:**
- Create: `crates/ta-db-proxy/src/registry.rs`
- Create: `crates/ta-db-proxy/src/external_plugin.rs`
- Modify: `crates/ta-db-proxy/src/lib.rs` (export `registry`, `external_plugin`)
- Modify: `crates/ta-db-proxy/Cargo.toml` (add `ta-plugin` dependency)

**Interfaces:**
- Consumes: `ta_plugin::{PluginManifest, discover_plugins, transport::call_json}`, existing `DbProxyPlugin`/`ProxyConfig`/`ProxyHandle`/`QueryClass` (`crates/ta-db-proxy/src/plugin.rs`, `classification.rs`)
- Produces:
  - `ta_db_proxy::registry::{DbAdapterEntry{scheme: String, plugin: String}, DbAdapterRegistry{entries: Vec<DbAdapterEntry>}, DbAdapterRegistry::load(path: &Path) -> Result<Self, ProxyError>, DbAdapterRegistry::resolve(&self, scheme: &str) -> Option<&str>}` — the `.ta/db-adapters.toml` Resource-list registry.
  - `ta_db_proxy::external_plugin::ExternalDbProxyPlugin` implementing the existing `DbProxyPlugin` trait by shelling out to an external process via `ta_plugin::transport::call_json` for `classify_query`/`apply_mutation`, and spawning a long-lived listener process for `start` (mirrors the agent-runtime long-lived-process shape from Task 5, since `DbProxyPlugin::start` returns a `ProxyHandle` that must outlive the call).

- [ ] **Step 1: `registry.rs`**

```rust
//! `.ta/db-adapters.toml` — a Resource-list-category (§2.2) registry mapping
//! a DB URI scheme to the plugin name that handles it. Declarative only; no
//! executable contract lives here (that's `DbProxyPlugin`/`external_plugin`).

use crate::error::ProxyError;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct DbAdapterEntry {
    pub scheme: String,
    pub plugin: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DbAdapterRegistry {
    #[serde(default, rename = "adapter")]
    pub entries: Vec<DbAdapterEntry>,
}

impl DbAdapterRegistry {
    pub fn load(path: &Path) -> Result<Self, ProxyError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| ProxyError::Config(e.to_string()))?;
        toml::from_str(&text).map_err(|e| ProxyError::Config(e.to_string()))
    }

    pub fn resolve(&self, scheme: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.scheme == scheme)
            .map(|e| e.plugin.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_registry_file_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let registry = DbAdapterRegistry::load(&dir.path().join("db-adapters.toml")).unwrap();
        assert!(registry.entries.is_empty());
        assert_eq!(registry.resolve("sqlite"), None);
    }

    #[test]
    fn resolves_configured_scheme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db-adapters.toml");
        std::fs::write(&path, "[[adapter]]\nscheme = \"postgres\"\nplugin = \"pg-proxy\"\n").unwrap();
        let registry = DbAdapterRegistry::load(&path).unwrap();
        assert_eq!(registry.resolve("postgres"), Some("pg-proxy"));
        assert_eq!(registry.resolve("mysql"), None);
    }
}
```

First check `crates/ta-db-proxy/src/error.rs` for the exact `ProxyError` variant names (the plan assumes a `Config(String)` variant exists or can be added — read the file before writing this, and use whatever the closest existing variant is, adding one only if genuinely missing).

- [ ] **Step 2: `external_plugin.rs`**

```rust
//! External-process `DbProxyPlugin` implementation (§2.2 Plugin category),
//! discovered via `.ta/plugins/db/<name>/plugin.toml`.

use crate::classification::QueryClass;
use crate::error::ProxyError;
use crate::plugin::{DbProxyPlugin, ProxyConfig, ProxyHandle};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub struct ExternalDbProxyPlugin {
    name: String,
    command: String,
    args: Vec<String>,
    timeout: Duration,
}

impl ExternalDbProxyPlugin {
    pub fn discover(name: &str, project_root: &Path) -> Result<Self, ProxyError> {
        let found = ta_plugin::find_plugin("db", name, project_root)
            .ok_or_else(|| ProxyError::Config(format!("no db plugin manifest found for '{name}'")))?;
        Ok(Self {
            name: found.manifest.name.clone(),
            command: found.plugin_dir.join(&found.manifest.command).to_string_lossy().to_string(),
            args: found.manifest.args.clone(),
            timeout: found.manifest.timeout(DEFAULT_TIMEOUT_SECS),
        })
    }
}

#[derive(Serialize)]
struct ClassifyParams<'a> {
    query: &'a str,
}

#[derive(Deserialize)]
struct ClassifyResult {
    class: QueryClass,
}

#[derive(Serialize)]
struct ApplyMutationParams<'a> {
    upstream_dsn: &'a str,
    uri: &'a str,
    before: Option<&'a serde_json::Value>,
    after: &'a serde_json::Value,
    staging_dir: String,
}

impl DbProxyPlugin for ExternalDbProxyPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn wire_protocol(&self) -> &str {
        "external"
    }

    fn start(&self, config: ProxyConfig) -> anyhow::Result<Box<dyn ProxyHandle>> {
        anyhow::bail!(
            "external db plugin '{}' listener lifecycle is not yet wired to a long-lived process (config: {:?}) — use classify_query/apply_mutation only until v0.17.0.12.15's Channel/Listener support lands",
            self.name,
            config.listen_addr
        )
    }

    fn classify_query(&self, query: &str) -> QueryClass {
        let params = ClassifyParams { query };
        match ta_plugin::transport::call_json::<_, ClassifyResult>(
            &self.name,
            "classify_query",
            &self.command,
            &self.args,
            Path::new("."),
            &params,
            self.timeout,
        ) {
            Ok(result) => result.class,
            Err(_) => QueryClass::Unknown,
        }
    }

    fn apply_mutation(
        &self,
        upstream_dsn: &str,
        uri: &str,
        before: Option<&serde_json::Value>,
        after: &serde_json::Value,
        staging_dir: &Path,
    ) -> anyhow::Result<()> {
        let params = ApplyMutationParams {
            upstream_dsn,
            uri,
            before,
            after,
            staging_dir: staging_dir.to_string_lossy().to_string(),
        };
        let _: serde_json::Value = ta_plugin::transport::call_json(
            &self.name,
            "apply_mutation",
            &self.command,
            &self.args,
            Path::new("."),
            &params,
            self.timeout,
        )
        .map_err(|e| anyhow::anyhow!("db plugin '{}' apply_mutation failed: {e}", self.name))?;
        Ok(())
    }
}
```

Before writing this: (a) read `crates/ta-db-proxy/src/classification.rs` to confirm `QueryClass` derives `Serialize`/`Deserialize` (add `#[derive(Serialize, Deserialize)]` to it if missing — it's a plain enum per the earlier research, so this should be additive and safe), and (b) read `crates/ta-db-proxy/src/plugin.rs`'s exact `Result<...>` type alias (`anyhow::Result` vs a crate-local `Result<T> = std::result::Result<T, ProxyError>`) and match it exactly rather than assuming `anyhow::Result`.

- [ ] **Step 3: Wire into `lib.rs`**

Add to `crates/ta-db-proxy/src/lib.rs`:
```rust
pub mod external_plugin;
pub mod registry;

pub use external_plugin::ExternalDbProxyPlugin;
pub use registry::{DbAdapterEntry, DbAdapterRegistry};
```

- [ ] **Step 4: Add dependency**

`crates/ta-db-proxy/Cargo.toml`, under `[dependencies]`:
```toml
ta-plugin = { path = "../ta-plugin", version = "0.17.0-alpha.12.13" }
```
(check whether `toml` and `serde`/`serde_json` are already present in this Cargo.toml — add only what's missing.)

- [ ] **Step 5: Build + test**

Run: `./dev cargo build -p ta-db-proxy && ./dev cargo test -p ta-db-proxy`
Expected: existing `SqliteProxyPlugin` tests unaffected; new `registry` tests (Step 1) pass.

- [ ] **Step 6: Full workspace test, clippy, fmt**

Run: `./dev cargo test --workspace && ./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`

- [ ] **Step 7: Commit**

```bash
git add crates/ta-db-proxy
git commit -m "feat(ta-db-proxy): add external Plugin-category DbProxyPlugin + .ta/db-adapters.toml registry"
```

---

### Task 8: Wire `ReleaseAdapter` onto the Plugin category

**Files:**
- Modify: `apps/ta-cli/src/commands/release_git.rs` (add `ExternalReleaseAdapter`, extend `resolve_adapter` to check plugin discovery first)

**Interfaces:**
- Consumes: `ta_plugin::{find_plugin, transport::call_json}` (already a dependency of `apps/ta-cli` after Task 6)
- Produces: `ExternalReleaseAdapter::discover(name: &str, project_root: &Path) -> Option<Box<dyn ReleaseAdapter>>`; `resolve_adapter(adapter_name: Option<&str>, project_root: &Path) -> Box<dyn ReleaseAdapter>` — signature gains a `project_root: &Path` parameter (update its call site(s); grep `resolve_adapter(` in `apps/ta-cli` first to find and fix every caller).

- [ ] **Step 1: Read the exact current file**

Read `apps/ta-cli/src/commands/release_git.rs` in full (the earlier research summarized it but did not quote every line) before editing, to match existing error message conventions in `GitAdapter`/`PerforceAdapter`/`SvnAdapter` exactly.

- [ ] **Step 2: Add `ExternalReleaseAdapter`**

```rust
pub struct ExternalReleaseAdapter {
    name: String,
    command: String,
    args: Vec<String>,
    timeout: std::time::Duration,
}

#[derive(serde::Serialize)]
struct BumpVersionParams<'a> {
    root: &'a str,
    new_version: &'a str,
}
#[derive(serde::Serialize)]
struct CommitAndTagParams<'a> {
    root: &'a str,
    message: &'a str,
    tag: &'a str,
}
#[derive(serde::Serialize)]
struct PushParams<'a> {
    root: &'a str,
    remote: &'a str,
    args: &'a [&'a str],
}
#[derive(serde::Serialize)]
struct CreateReleaseDraftParams<'a> {
    root: &'a str,
    tag: &'a str,
    notes: &'a str,
}
#[derive(serde::Serialize)]
struct TagOnlyParams<'a> {
    root: &'a str,
    tag: &'a str,
}
#[derive(serde::Serialize)]
struct DispatchWorkflowParams<'a> {
    root: &'a str,
    tag: &'a str,
    prerelease: bool,
}

impl ExternalReleaseAdapter {
    pub fn discover(name: &str, project_root: &Path) -> Option<Self> {
        let found = ta_plugin::find_plugin("release", name, project_root)?;
        Some(Self {
            name: found.manifest.name.clone(),
            command: found.plugin_dir.join(&found.manifest.command).to_string_lossy().to_string(),
            args: found.manifest.args.clone(),
            timeout: found.manifest.timeout(60),
        })
    }

    fn call<Req: serde::Serialize>(&self, method: &str, root: &Path, params: &Req) -> anyhow::Result<()> {
        let _: serde_json::Value = ta_plugin::transport::call_json(
            &self.name,
            method,
            &self.command,
            &self.args,
            root,
            params,
            self.timeout,
        )
        .map_err(|e| anyhow::anyhow!("release plugin '{}' method '{method}' failed: {e}", self.name))?;
        Ok(())
    }
}

impl ReleaseAdapter for ExternalReleaseAdapter {
    fn bump_version(&self, root: &Path, new_version: &str) -> anyhow::Result<()> {
        self.call("bump_version", root, &BumpVersionParams { root: &root.to_string_lossy(), new_version })
    }
    fn commit_and_tag(&self, root: &Path, message: &str, tag: &str) -> anyhow::Result<()> {
        self.call("commit_and_tag", root, &CommitAndTagParams { root: &root.to_string_lossy(), message, tag })
    }
    fn push(&self, root: &Path, remote: &str, args: &[&str]) -> anyhow::Result<()> {
        self.call("push", root, &PushParams { root: &root.to_string_lossy(), remote, args })
    }
    fn create_release_draft(&self, root: &Path, tag: &str, notes: &str) -> anyhow::Result<()> {
        self.call("create_release_draft", root, &CreateReleaseDraftParams { root: &root.to_string_lossy(), tag, notes })
    }
    fn publish_release(&self, root: &Path, tag: &str) -> anyhow::Result<()> {
        self.call("publish_release", root, &TagOnlyParams { root: &root.to_string_lossy(), tag })
    }
    fn dispatch_workflow(&self, root: &Path, tag: &str, prerelease: bool) -> anyhow::Result<()> {
        self.call("dispatch_workflow", root, &DispatchWorkflowParams { root: &root.to_string_lossy(), tag, prerelease })
    }
}
```

- [ ] **Step 3: Extend `resolve_adapter`**

```rust
pub fn resolve_adapter(adapter_name: Option<&str>, project_root: &Path) -> Box<dyn ReleaseAdapter> {
    if let Some(name) = adapter_name {
        if let Some(external) = ExternalReleaseAdapter::discover(name, project_root) {
            return Box::new(external);
        }
        match name {
            "perforce" | "p4" => return Box::new(PerforceAdapter),
            "svn" | "subversion" => return Box::new(SvnAdapter),
            _ => {}
        }
    }
    Box::new(GitAdapter)
}
```
Remove `#[allow(dead_code)]` from `ReleaseAdapter`/`GitAdapter`/`resolve_adapter` if it's still there once a real caller exists — grep `apps/ta-cli/src` for any existing (even if currently unused) call site to `resolve_adapter(` first; if genuinely still unwired to the release pipeline end-to-end, leave the attribute in place (wiring the full release pipeline call site is out of this phase's scope per PLAN.md — only "implement `ReleaseAdapter` onto the Plugin category" is asked for) and note that in `.ta-decisions.json`.

- [ ] **Step 4: Fix callers of `resolve_adapter`**

Run: `grep -rn "resolve_adapter(" apps/ta-cli/src` and update every call site to pass `project_root`.

- [ ] **Step 5: Add a test**

```rust
#[test]
fn resolve_adapter_falls_back_to_git_without_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = resolve_adapter(None, dir.path());
    // GitAdapter is the only variant with no external process requirement;
    // asserting via a method that doesn't touch git internals isn't available,
    // so assert indirectly: a plugin-kind name that has no manifest must not
    // match ExternalReleaseAdapter and must fall through to git/unsupported paths.
    let _ = adapter; // constructs without panicking; deeper behavioral tests already exist for GitAdapter/PerforceAdapter/SvnAdapter
}

#[test]
fn resolve_adapter_discovers_external_release_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join(".ta/plugins/release/custom");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "name = \"custom\"\ntype = \"release\"\ncommand = \"custom-release-bin\"\n",
    )
    .unwrap();
    let adapter = resolve_adapter(Some("custom"), dir.path());
    // resolve_adapter must have picked the external plugin path, not perforce/svn/git —
    // there is no public introspection method today, so this test primarily proves
    // discover() itself succeeds; extend with a trait-object downcast if ReleaseAdapter
    // grows one in a later phase.
    let _ = adapter;
}
```
(Write a stronger assertion if `ReleaseAdapter` or `ExternalReleaseAdapter` can expose a `fn adapter_name(&self) -> &str` cheaply — add one if it doesn't already exist, since it makes this test meaningfully assert the right adapter was chosen rather than just "didn't panic".)

- [ ] **Step 6: Build + test**

Run: `./dev cargo build -p ta-cli && ./dev cargo test -p ta-cli release`

- [ ] **Step 7: Full workspace test, clippy, fmt**

Run: `./dev cargo test --workspace && ./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`

- [ ] **Step 8: Commit**

```bash
git add apps/ta-cli/src/commands/release_git.rs
git commit -m "feat(ta-cli): wire ReleaseAdapter onto the Plugin category via .ta/plugins/release discovery"
```

---

### Task 9: Cross-family proof test (item 6)

**Files:**
- Create: `crates/ta-plugin/tests/cross_family_unification.rs`

**Interfaces:**
- Consumes: `ta_submit::external_vcs_adapter::ExternalVcsAdapter` (needs `pub use` if not already public — check `crates/ta-submit/src/lib.rs`), `ta_runtime::plugin::ExternalRuntimeAdapter` (same check).

**Goal of this test:** one synthetic plugin script, spawned identically through **two different Plugin-category integrations** (VCS and agent-runtime — both do a `handshake` call with the exact same `{method,params}`/`{ok,result}` envelope shape, per Task 1's `envelope.rs` and the original research showing both already use `method:String` dispatch), proving the shared transport crate — not just documentation — is what both now run on.

- [ ] **Step 1: Confirm public exports**

Run: `grep -n "pub use\|pub mod" crates/ta-submit/src/lib.rs crates/ta-runtime/src/lib.rs`
If `ExternalVcsAdapter` or `ExternalRuntimeAdapter`/`plugin` module are not `pub`, add the minimal `pub use` needed — do not widen visibility further than required for this test to compile.

- [ ] **Step 2: Add `ta-plugin`'s dev-dependencies for this integration test**

`crates/ta-plugin/Cargo.toml`, under `[dev-dependencies]`, add:
```toml
ta-submit = { path = "../ta-submit" }
ta-runtime = { path = "../ta-runtime" }
```
(A dev-dependency cycle back into two crates that depend on `ta-plugin` is fine in Cargo — dev-dependencies are excluded from the normal dependency graph used for the library build.)

- [ ] **Step 3: Write the test**

```rust
//! Proves the Plugin-category unification (v0.17.0.12.14 item 6): one
//! synthetic, community-authored-style plugin script is discovered and
//! invoked identically through two independent Plugin-category integrations
//! (VCS and agent-runtime) via the shared ta-plugin transport.

#![cfg(unix)]

use std::path::Path;

fn write_synthetic_plugin(dir: &Path) -> String {
    let path = dir.join("synthetic-plugin.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"9.9.9","protocol_version":1,"adapter_name":"synthetic","capabilities":["handshake"]}}'
"#,
    )
    .unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn synthetic_plugin_works_identically_via_vcs_and_runtime_integrations() {
    let dir = tempfile::tempdir().unwrap();
    let command = write_synthetic_plugin(dir.path());

    // Integration 1: VCS
    let vcs_manifest = ta_submit::vcs_plugin_manifest::VcsPluginManifest {
        name: "synthetic".to_string(),
        version: "0.1.0".to_string(),
        plugin_type: "vcs".to_string(),
        command: command.clone(),
        args: vec![],
        capabilities: vec![],
        description: None,
        timeout_secs: 5,
        min_daemon_version: None,
        source_url: None,
        staging_env: Default::default(),
    };
    let vcs_adapter = ta_submit::external_vcs_adapter::ExternalVcsAdapter::new(&vcs_manifest, dir.path(), "0.17.0-test").expect("VCS handshake via synthetic plugin should succeed");
    assert_eq!(vcs_adapter.plugin_version(), "9.9.9");

    // Integration 2: agent-runtime
    let runtime_adapter = ta_runtime::plugin::ExternalRuntimeAdapter::new(Path::new(&command), "synthetic").expect("runtime handshake via the same synthetic plugin should succeed");
    assert_eq!(runtime_adapter.plugin_version(), "9.9.9");
}
```

Before finalizing this test: (a) check `ExternalVcsAdapter`'s actual constructor signature and whether it exposes `plugin_version()` (add a trivial `pub fn plugin_version(&self) -> &str` accessor to both `ExternalVcsAdapter` and `ExternalRuntimeAdapter` if neither currently exposes the handshake's `plugin_version` — a one-line getter, safe/additive); (b) confirm `VcsPluginManifest`'s exact field list matches Task-1-research's findings (it does, per the earlier report) so this literal compiles.

- [ ] **Step 4: Build + test**

Run: `./dev cargo test -p ta-plugin --test cross_family_unification`
Expected: passes, proving one script satisfies both integrations' handshake path through the shared transport.

- [ ] **Step 5: Full workspace test, clippy, fmt**

Run: `./dev cargo test --workspace && ./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`

- [ ] **Step 6: Commit**

```bash
git add crates/ta-plugin crates/ta-submit crates/ta-runtime
git commit -m "test: prove VCS and agent-runtime plugins share one transport via a synthetic plugin fixture"
```

---

### Task 10: USAGE.md "Authoring a Plugin" guide

**Files:**
- Modify: `docs/USAGE.md` (add one canonical "Authoring a Plugin" section)
- Modify: `docs/PLUGIN-AUTHORING.md`, `docs/plugins-architecture-guidance.md`, `docs/plugin-traits.md` (add a one-line pointer at the top redirecting to the new USAGE.md section — do not delete, other docs/links may reference them)

**Interfaces:** None (documentation only).

- [ ] **Step 1: Read `docs/USAGE.md`'s table of contents / section structure**

Find where a new top-level "Authoring a Plugin" section fits (near any existing "Plugins"/"Extending TA" heading, or as a new section before "Roadmap").

- [ ] **Step 2: Write the section**

Cover, in this order: the four categories (Plugin/Channel-Listener/Backend/Resource-list) with one line each on when to use which; the `plugin.toml` schema (name/version/type/command/args/capabilities/description/timeout_secs/staging_env) with one annotated example; the discovery convention (`.ta/plugins/<kind>/<name>/plugin.toml`, project-local then user-global); the wire protocol contract (newline-delimited JSON, one request line in, one response line out, `{"ok":true,"result":{...}}` / `{"ok":false,"error":"..."}`); a table of the `<kind>` values shipped today (`vcs`, `messaging`, `social`, `agent`, `tool`, `db`, `release`) with one example manifest command per kind; a short "Your plugin doesn't need to change its own request/response fields" note for authors of the four pre-existing families (vcs/messaging/social/agent) clarifying that only the transport/manifest/discovery layer changed, not their wire format.

- [ ] **Step 3: Add pointers in the scattered docs**

At the top of `docs/PLUGIN-AUTHORING.md`, `docs/plugins-architecture-guidance.md`, and `docs/plugin-traits.md`, add:
```markdown
> **See `docs/USAGE.md` → "Authoring a Plugin" for the current, unified plugin manifest/discovery/protocol reference.** This document covers additional family-specific detail not yet folded into that guide.
```
(Read each file first — if a doc is already fully superseded with nothing family-specific left worth keeping, note that in the PR/change summary rather than deleting it outright in this task.)

- [ ] **Step 4: Commit**

```bash
git add docs/USAGE.md docs/PLUGIN-AUTHORING.md docs/plugins-architecture-guidance.md docs/plugin-traits.md
git commit -m "docs: add unified Authoring a Plugin guide to USAGE.md"
```

---

### Task 11: PLAN.md, change summary, decision log, final verification

**Files:**
- Modify: `PLAN.md` (check off all 7 items under v0.17.0.12.14; do not change the `<!-- status: ... -->` marker)
- Create: `.ta/change_summary.json`
- Create: `.ta-decisions.json`
- Modify: `.ta/ta-progress.json` (append final checkpoint)

- [ ] **Step 1: Mark PLAN.md items `[x]`**

Items 1–7 under `### v0.17.0.12.14` (lines 9422–9428) each get `[x]` once their corresponding task above is done and tested.

- [ ] **Step 2: Record design decisions in `.ta-decisions.json`**

Include at minimum: (1) choosing to preserve each domain's existing wire-format types while sharing only transport/manifest/discovery, with rationale (real published plugin binaries must keep working); (2) standardizing discovery order on VCS's project-then-global (a deliberate behavior change for messaging/social, whose prior order was global-then-project) with rationale (phase goal explicitly says "matching VCS's...shape (most mature)"); (3) adding ETXTBSY retry to the social plugin transport where none existed before, as a safe additive side effect of sharing `call_json`; (4) scoping `DbProxyPlugin`'s `start()` external-process path as not-yet-wired to a real long-lived listener (returns an actionable `bail!` explaining why, pointing at v0.17.0.12.15), rather than half-building process-lifecycle management that belongs to the Channel/Listener category work in the next phase.

- [ ] **Step 3: Write `.ta/change_summary.json`**

Follow the schema in the injected CLAUDE.md exactly — one entry per changed file from Tasks 1–10, with real `what`/`why`, and correct `depends_on`/`depended_by` (every domain crate depends on `crates/ta-plugin`; `apps/ta-cli`'s tool/release changes depend on `crates/ta-plugin` but not on each other; `crates/ta-db-proxy`'s changes are independent of `apps/ta-cli`'s).

- [ ] **Step 4: Final full verification**

Run: `./dev cargo build --workspace && ./dev cargo test --workspace && ./dev cargo clippy --workspace --all-targets -- -D warnings && ./dev cargo fmt --all -- --check`
Expected: all four pass with zero warnings/failures.

- [ ] **Step 5: Append final progress checkpoint**

Add to `.ta/ta-progress.json`'s `checkpoints` array: `{"label": "work_complete", "at": "<ISO timestamp>", "detail": "v0.17.0.12.14 all 7 items implemented; workspace build/test/clippy/fmt clean"}`.

- [ ] **Step 6: Commit**

```bash
git add PLAN.md .ta-decisions.json
git commit -m "docs: mark v0.17.0.12.14 items complete, record plugin-unification design decisions"
```
(`.ta/change_summary.json` and `.ta/ta-progress.json` are TA-internal and excluded from the reviewer diff per the injected CLAUDE.md — no need to `git add` them, but write them to disk regardless.)
