//! Messaging adapter plugin discovery and external plugin wrapper.
//!
//! ## Plugin discovery
//!
//! Plugins are searched in order:
//! 1. `~/.config/ta/plugins/messaging/` — user-global
//! 2. `.ta/plugins/messaging/` — project-local
//! 3. `$PATH` — bare executable with prefix `ta-messaging-`
//!
//! The first matching plugin for the given provider name is used.
//!
//! ## ExternalMessagingAdapter
//!
//! Wraps an external plugin process and translates trait calls into
//! JSON-over-stdio request/response exchanges. Each method call spawns
//! a fresh process (plugins are stateless per-call).
//!
//! ## Credentials
//!
//! Credentials (OAuth2 tokens, IMAP passwords) are stored in the OS
//! keychain under the key `ta-messaging:<provider>:<address>`. Plugins
//! retrieve them via the `keyring` crate or by calling
//! `ta adapter credentials get <key>`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::messaging_plugin_protocol::{
    CreateDraftParams, DraftEnvelope, DraftState, DraftStatusParams, FetchParams, FetchedMessage,
    HealthParams, MessagingPluginError, MessagingPluginRequest, MessagingPluginResponse,
    MESSAGING_PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// Plugin manifest
// ---------------------------------------------------------------------------

/// Parsed `plugin.toml` manifest for a messaging adapter plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagingPluginManifest {
    /// Provider name (e.g., "gmail", "outlook", "imap").
    pub name: String,

    /// Plugin version (semver).
    #[serde(default = "default_version")]
    pub version: String,

    /// Plugin type — must be `"messaging"`.
    #[serde(rename = "type", default = "default_type")]
    pub plugin_type: String,

    /// Executable command to spawn.
    pub command: String,

    /// Additional arguments passed on every invocation.
    #[serde(default)]
    pub args: Vec<String>,

    /// Capabilities this plugin exposes.
    ///
    /// Standard values: `"fetch"`, `"create_draft"`, `"draft_status"`, `"health"`.
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Per-call timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// Protocol version this plugin implements.
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_type() -> String {
    "messaging".to_string()
}

fn default_timeout_secs() -> u64 {
    60
}

fn default_protocol_version() -> u32 {
    MESSAGING_PROTOCOL_VERSION
}

impl MessagingPluginManifest {
    /// Load a manifest from a `plugin.toml` file.
    pub fn load(path: &Path) -> Result<Self, MessagingPluginError> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content).map_err(|e| {
            MessagingPluginError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid manifest at {}: {}", path.display(), e),
            ))
        })?;
        Ok(manifest)
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Where a messaging plugin was discovered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessagingPluginSource {
    /// `~/.config/ta/plugins/messaging/` (user-global).
    UserGlobal,
    /// `.ta/plugins/messaging/` in the project root.
    ProjectLocal,
    /// Bare executable on `$PATH` (prefix `ta-messaging-`).
    Path,
}

impl std::fmt::Display for MessagingPluginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessagingPluginSource::UserGlobal => write!(f, "global"),
            MessagingPluginSource::ProjectLocal => write!(f, "project"),
            MessagingPluginSource::Path => write!(f, "PATH"),
        }
    }
}

/// A discovered messaging plugin with its manifest and origin.
#[derive(Debug, Clone)]
pub struct DiscoveredMessagingPlugin {
    /// Parsed manifest.
    pub manifest: MessagingPluginManifest,
    /// Directory containing `plugin.toml` (None for PATH-discovered plugins).
    pub plugin_dir: Option<PathBuf>,
    /// Discovery source.
    pub source: MessagingPluginSource,
}

/// Discover all messaging adapter plugins.
///
/// Resolution order:
/// 1. `~/.config/ta/plugins/messaging/` — user-global (highest priority)
/// 2. `.ta/plugins/messaging/` — project-local
///
/// PATH discovery (`ta-messaging-<name>`) is performed on-demand in
/// [`find_messaging_plugin`] when a named plugin is not found above.
pub fn discover_messaging_plugins(project_root: &Path) -> Vec<DiscoveredMessagingPlugin> {
    let mut plugins = Vec::new();

    // 1. User-global
    if let Some(config_dir) = user_config_dir() {
        let global_dir = config_dir.join("ta").join("plugins").join("messaging");
        scan_messaging_plugin_dir(&global_dir, MessagingPluginSource::UserGlobal, &mut plugins);
    }

    // 2. Project-local
    let project_dir = project_root.join(".ta").join("plugins").join("messaging");
    scan_messaging_plugin_dir(
        &project_dir,
        MessagingPluginSource::ProjectLocal,
        &mut plugins,
    );

    plugins
}

/// Scan a directory for messaging plugin subdirectories containing `plugin.toml`.
fn scan_messaging_plugin_dir(
    dir: &Path,
    source: MessagingPluginSource,
    out: &mut Vec<DiscoveredMessagingPlugin>,
) {
    if !dir.is_dir() {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                error = %e,
                "Failed to read messaging plugin directory"
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("plugin.toml");
        if !manifest_path.exists() {
            continue;
        }

        match MessagingPluginManifest::load(&manifest_path) {
            Ok(manifest) => {
                tracing::debug!(
                    plugin = %manifest.name,
                    source = %source,
                    "Discovered messaging plugin"
                );
                out.push(DiscoveredMessagingPlugin {
                    manifest,
                    plugin_dir: Some(path),
                    source: source.clone(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "Skipping invalid messaging plugin manifest"
                );
            }
        }
    }
}

/// Find a messaging plugin by provider name.
///
/// Searches user-global → project-local → PATH.
/// Returns `None` if no plugin is found for the given provider.
pub fn find_messaging_plugin(
    provider: &str,
    project_root: &Path,
) -> Option<DiscoveredMessagingPlugin> {
    // Search manifest-based plugins.
    let all = discover_messaging_plugins(project_root);
    if let Some(p) = all.into_iter().find(|p| p.manifest.name == provider) {
        return Some(p);
    }

    // Fall back to bare PATH executable: `ta-messaging-<name>`.
    let bare_cmd = format!("ta-messaging-{}", provider);
    if which_on_path(&bare_cmd) {
        tracing::info!(
            provider = %provider,
            command = %bare_cmd,
            "Found messaging plugin as bare executable on PATH"
        );
        return Some(DiscoveredMessagingPlugin {
            manifest: MessagingPluginManifest {
                name: provider.to_string(),
                version: "unknown".to_string(),
                plugin_type: "messaging".to_string(),
                command: bare_cmd,
                args: vec![],
                capabilities: vec![
                    "fetch".to_string(),
                    "create_draft".to_string(),
                    "draft_status".to_string(),
                    "health".to_string(),
                ],
                description: None,
                timeout_secs: 60,
                protocol_version: MESSAGING_PROTOCOL_VERSION,
            },
            plugin_dir: None,
            source: MessagingPluginSource::Path,
        });
    }

    None
}

// ---------------------------------------------------------------------------
// ExternalMessagingAdapter
// ---------------------------------------------------------------------------

/// Messaging adapter that delegates all operations to an external plugin process.
///
/// Each method call spawns a fresh process, sends one JSON request line to
/// stdin, reads one JSON response line from stdout, then waits for exit.
#[derive(Debug)]
pub struct ExternalMessagingAdapter {
    /// Plugin command to spawn.
    command: String,
    /// Additional pre-configured args.
    args: Vec<String>,
    /// Provider name (from manifest).
    provider: String,
    /// Per-call timeout.
    timeout: Duration,
}

impl ExternalMessagingAdapter {
    /// Create a new adapter from a discovered plugin manifest.
    pub fn new(manifest: &MessagingPluginManifest) -> Self {
        Self {
            command: manifest.command.clone(),
            args: manifest.args.clone(),
            provider: manifest.name.clone(),
            timeout: Duration::from_secs(manifest.timeout_secs),
        }
    }

    /// Provider name (e.g., "gmail", "outlook", "imap").
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Fetch messages received since `since_iso8601`.
    ///
    /// `account` is the email address to fetch from (None = plugin default).
    pub fn fetch(
        &self,
        since_iso8601: &str,
        account: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<FetchedMessage>, MessagingPluginError> {
        let req = MessagingPluginRequest::Fetch(FetchParams {
            since: since_iso8601.to_string(),
            account: account.map(str::to_string),
            limit,
        });
        let resp = self.call_plugin(&req, "fetch")?;
        Ok(resp.messages.unwrap_or_default())
    }

    /// Create a draft in the provider's native Drafts folder.
    ///
    /// Returns the provider-assigned draft ID (e.g., "gmail-draft-abc123").
    ///
    /// NOTE: There is intentionally no `send` method on this type.
    /// TA never sends messages on behalf of the user.
    pub fn create_draft(&self, draft: DraftEnvelope) -> Result<String, MessagingPluginError> {
        let req = MessagingPluginRequest::CreateDraft(CreateDraftParams { draft });
        let resp = self.call_plugin(&req, "create_draft")?;
        resp.draft_id
            .ok_or_else(|| MessagingPluginError::InvalidResponse {
                name: self.provider.clone(),
                op: "create_draft".to_string(),
                reason: "response missing draft_id".to_string(),
            })
    }

    /// Poll the current state of a previously created draft.
    pub fn draft_status(&self, draft_id: &str) -> Result<DraftState, MessagingPluginError> {
        let req = MessagingPluginRequest::DraftStatus(DraftStatusParams {
            draft_id: draft_id.to_string(),
        });
        let resp = self.call_plugin(&req, "draft_status")?;
        Ok(resp.state.unwrap_or(DraftState::Unknown))
    }

    /// Run a health check: verify credentials and connectivity.
    ///
    /// Returns `(address, provider_name)` on success.
    pub fn health(&self) -> Result<(String, String), MessagingPluginError> {
        let req = MessagingPluginRequest::Health(HealthParams {});
        let resp = self.call_plugin(&req, "health")?;
        let address = resp.address.unwrap_or_else(|| "<unknown>".to_string());
        let provider = resp.provider.unwrap_or_else(|| self.provider.clone());
        Ok((address, provider))
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn call_plugin(
        &self,
        req: &MessagingPluginRequest,
        op: &str,
    ) -> Result<MessagingPluginResponse, MessagingPluginError> {
        let req_json = serde_json::to_string(req)?;

        let mut parts = self.command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| MessagingPluginError::SpawnFailed {
                command: self.command.clone(),
                reason: "command string is empty".to_string(),
            })?;

        let mut cmd = Command::new(program);
        for arg in parts {
            cmd.arg(arg);
        }
        for arg in &self.args {
            cmd.arg(arg);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| MessagingPluginError::SpawnFailed {
            command: self.command.clone(),
            reason: e.to_string(),
        })?;

        // Write request to stdin.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(req_json.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .map_err(|e| {
                    MessagingPluginError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to write to plugin stdin: {}", e),
                    ))
                })?;
        }

        // Wait with timeout.
        let timeout_ms = self.timeout.as_millis() as u64;
        let output =
            wait_with_timeout(child, timeout_ms).map_err(|_| MessagingPluginError::Timeout {
                name: self.provider.clone(),
                op: op.to_string(),
                timeout_secs: self.timeout.as_secs(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MessagingPluginError::OpFailed {
                name: self.provider.clone(),
                op: op.to_string(),
                reason: format!(
                    "plugin exited with status {}. stderr: {}",
                    output.status,
                    stderr.trim()
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_line = stdout.lines().next().unwrap_or("").trim();

        if first_line.is_empty() {
            return Err(MessagingPluginError::InvalidResponse {
                name: self.provider.clone(),
                op: op.to_string(),
                reason: "plugin produced no output (expected one JSON line)".to_string(),
            });
        }

        let resp: MessagingPluginResponse = serde_json::from_str(first_line).map_err(|e| {
            MessagingPluginError::InvalidResponse {
                name: self.provider.clone(),
                op: op.to_string(),
                reason: format!(
                    "invalid JSON: {}. Got: '{}'",
                    e,
                    if first_line.len() > 200 {
                        &first_line[..200]
                    } else {
                        first_line
                    }
                ),
            }
        })?;

        if !resp.ok {
            return Err(MessagingPluginError::OpFailed {
                name: self.provider.clone(),
                op: op.to_string(),
                reason: resp
                    .error
                    .unwrap_or_else(|| "plugin returned ok=false".to_string()),
            });
        }

        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Check whether a binary exists on PATH.
fn which_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path_var| std::env::split_paths(&path_var).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Get the user's config directory.
fn user_config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg));
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config"))
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
                    let _ = child_id;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::write(dir.join("plugin.toml"), content).unwrap();
    }

    #[test]
    fn discover_messaging_plugins_finds_manifests() {
        let root = tempfile::tempdir().unwrap();
        let msg_dir = root.path().join(".ta").join("plugins").join("messaging");

        let gmail_dir = msg_dir.join("gmail");
        std::fs::create_dir_all(&gmail_dir).unwrap();
        write_manifest(
            &gmail_dir,
            r#"
name = "gmail"
version = "0.1.0"
type = "messaging"
command = "ta-messaging-gmail"
capabilities = ["fetch", "create_draft", "draft_status", "health"]
description = "Gmail messaging adapter"
"#,
        );

        let plugins = discover_messaging_plugins(root.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "gmail");
        assert_eq!(plugins[0].source, MessagingPluginSource::ProjectLocal);
    }

    #[test]
    fn discover_messaging_plugins_skips_invalid_manifest() {
        let root = tempfile::tempdir().unwrap();
        let msg_dir = root.path().join(".ta").join("plugins").join("messaging");

        // Valid
        let good_dir = msg_dir.join("gmail");
        std::fs::create_dir_all(&good_dir).unwrap();
        write_manifest(
            &good_dir,
            r#"name = "gmail"
type = "messaging"
command = "ta-messaging-gmail"
"#,
        );

        // Invalid (bad TOML)
        let bad_dir = msg_dir.join("bad");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("plugin.toml"), "{{not valid toml}}").unwrap();

        let plugins = discover_messaging_plugins(root.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "gmail");
    }

    #[test]
    fn discover_messaging_plugins_empty_dir_returns_empty() {
        let root = tempfile::tempdir().unwrap();
        let plugins = discover_messaging_plugins(root.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn find_messaging_plugin_project_local() {
        let root = tempfile::tempdir().unwrap();
        let msg_dir = root.path().join(".ta").join("plugins").join("messaging");

        let imap_dir = msg_dir.join("imap");
        std::fs::create_dir_all(&imap_dir).unwrap();
        write_manifest(
            &imap_dir,
            r#"name = "imap"
type = "messaging"
command = "ta-messaging-imap"
"#,
        );

        let found = find_messaging_plugin("imap", root.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap().manifest.name, "imap");
    }

    #[test]
    fn find_messaging_plugin_missing_returns_none() {
        let root = tempfile::tempdir().unwrap();
        // "nonexistent-provider" is not a real plugin binary — discovery returns None
        // without any PATH manipulation (which would race with parallel tests that need git).
        let found = find_messaging_plugin("nonexistent-provider", root.path());
        assert!(found.is_none());
    }

    #[test]
    fn messaging_plugin_source_display() {
        assert_eq!(MessagingPluginSource::UserGlobal.to_string(), "global");
        assert_eq!(MessagingPluginSource::ProjectLocal.to_string(), "project");
        assert_eq!(MessagingPluginSource::Path.to_string(), "PATH");
    }

    /// Return the path to a shared mock plugin binary, writing it exactly once per process.
    ///
    /// Using OnceLock prevents concurrent write+exec races (ETXTBSY) on overlayfs-backed
    /// /tmp in Nix CI, where writing and immediately exec-ing a file can race kernel copy-up.
    /// The script dispatches on op type so both health and create_draft tests can share it.
    #[cfg(unix)]
    fn shared_mock_plugin_path() -> &'static std::path::Path {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::OnceLock;

        static MOCK_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
        MOCK_PATH.get_or_init(|| {
            let pid = std::process::id();
            let name = format!("ta-msg-mock-shared-{}", pid);

            #[cfg(target_os = "linux")]
            let path = std::path::PathBuf::from("/tmp").join(&name);
            #[cfg(not(target_os = "linux"))]
            let path = std::env::temp_dir().join(&name);

            let mut f = std::fs::File::create(&path).unwrap();
            // Dispatch on op type so both health and create_draft tests use this binary.
            f.write_all(
                br#"#!/bin/sh
read -r line
case "$line" in
  *create_draft*) echo '{"ok":true,"draft_id":"mock-draft-abc123"}' ;;
  *fetch*)        echo '{"ok":true,"messages":[]}' ;;
  *)              echo '{"ok":true,"address":"me@example.com","provider":"mock"}' ;;
esac
"#,
            )
            .unwrap();
            f.sync_all().unwrap();
            drop(f);

            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            // Read back to force page-cache flush on overlayfs before any exec.
            let _ = std::fs::metadata(&path).unwrap();
            path
        })
    }

    #[cfg(unix)]
    #[test]
    fn external_adapter_calls_mock_plugin() {
        let plugin_path = shared_mock_plugin_path();
        let manifest = MessagingPluginManifest {
            name: "mock".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "messaging".to_string(),
            command: plugin_path.display().to_string(),
            args: vec![],
            capabilities: vec!["health".to_string()],
            description: None,
            timeout_secs: 30,
            protocol_version: MESSAGING_PROTOCOL_VERSION,
        };

        let adapter = ExternalMessagingAdapter::new(&manifest);
        let (address, provider) = adapter.health().unwrap();
        assert_eq!(address, "me@example.com");
        assert_eq!(provider, "mock");
    }

    #[cfg(unix)]
    #[test]
    fn external_adapter_create_draft_returns_id() {
        let plugin_path = shared_mock_plugin_path();
        let manifest = MessagingPluginManifest {
            name: "mock".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "messaging".to_string(),
            command: plugin_path.display().to_string(),
            args: vec![],
            capabilities: vec!["create_draft".to_string()],
            description: None,
            timeout_secs: 30,
            protocol_version: MESSAGING_PROTOCOL_VERSION,
        };

        let adapter = ExternalMessagingAdapter::new(&manifest);
        let draft_id = adapter
            .create_draft(DraftEnvelope {
                to: "bob@example.com".to_string(),
                subject: "Hello".to_string(),
                body_html: "<p>Hi!</p>".to_string(),
                in_reply_to: None,
                thread_id: None,
                body_text: None,
            })
            .unwrap();
        assert_eq!(draft_id, "mock-draft-abc123");
    }
}
