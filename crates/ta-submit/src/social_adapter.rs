//! Social media adapter plugin discovery and external plugin wrapper.
//!
//! ## Plugin discovery
//!
//! Plugins are searched in order:
//! 1. `~/.config/ta/plugins/social/` — user-global
//! 2. `.ta/plugins/social/` — project-local
//! 3. `$PATH` — bare executable with prefix `ta-social-`
//!
//! The first matching plugin for the given platform name is used.
//!
//! ## ExternalSocialAdapter
//!
//! Wraps an external plugin process and translates calls into
//! JSON-over-stdio request/response exchanges. Each method call spawns
//! a fresh process (plugins are stateless per-call).
//!
//! ## Credentials
//!
//! Credentials (OAuth2 tokens) are stored in the OS keychain under the
//! key `ta-social:<platform>:<handle>`. Plugins retrieve them via
//! `ta adapter credentials get <key>`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::social_plugin_protocol::{
    CreateScheduledParams, CreateSocialDraftParams, SocialDraftStatusParams, SocialHealthParams,
    SocialPluginError, SocialPluginRequest, SocialPluginResponse, SocialPostContent,
    SocialPostState, SOCIAL_PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// Plugin manifest
// ---------------------------------------------------------------------------

/// Parsed `plugin.toml` manifest for a social media adapter plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialPluginManifest {
    /// Platform name (e.g., "linkedin", "x", "buffer").
    pub name: String,

    /// Plugin version (semver).
    #[serde(default = "default_version")]
    pub version: String,

    /// Plugin type — must be `"social"`.
    #[serde(rename = "type", default = "default_type")]
    pub plugin_type: String,

    /// Executable command to spawn.
    pub command: String,

    /// Additional arguments passed on every invocation.
    #[serde(default)]
    pub args: Vec<String>,

    /// Capabilities this plugin exposes.
    ///
    /// Standard values: `"create_draft"`, `"create_scheduled"`, `"draft_status"`, `"health"`.
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
    "social".to_string()
}

fn default_timeout_secs() -> u64 {
    60
}

fn default_protocol_version() -> u32 {
    SOCIAL_PROTOCOL_VERSION
}

impl SocialPluginManifest {
    /// Load a manifest from a `plugin.toml` file.
    pub fn load(path: &Path) -> Result<Self, SocialPluginError> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content).map_err(|e| {
            SocialPluginError::Io(std::io::Error::new(
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

/// Where a social plugin was discovered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocialPluginSource {
    /// `~/.config/ta/plugins/social/` (user-global).
    UserGlobal,
    /// `.ta/plugins/social/` in the project root.
    ProjectLocal,
    /// Bare executable on `$PATH` (prefix `ta-social-`).
    Path,
}

impl std::fmt::Display for SocialPluginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SocialPluginSource::UserGlobal => write!(f, "global"),
            SocialPluginSource::ProjectLocal => write!(f, "project"),
            SocialPluginSource::Path => write!(f, "PATH"),
        }
    }
}

/// A discovered social media plugin with its manifest and origin.
#[derive(Debug, Clone)]
pub struct DiscoveredSocialPlugin {
    /// Parsed manifest.
    pub manifest: SocialPluginManifest,
    /// Directory containing `plugin.toml` (None for PATH-discovered plugins).
    pub plugin_dir: Option<PathBuf>,
    /// Discovery source.
    pub source: SocialPluginSource,
}

/// Discover all social media adapter plugins.
///
/// Resolution order:
/// 1. `~/.config/ta/plugins/social/` — user-global (highest priority)
/// 2. `.ta/plugins/social/` — project-local
///
/// PATH discovery (`ta-social-<name>`) is performed on-demand in
/// [`find_social_plugin`] when a named plugin is not found above.
pub fn discover_social_plugins(project_root: &Path) -> Vec<DiscoveredSocialPlugin> {
    let mut plugins = Vec::new();

    // 1. User-global
    if let Some(config_dir) = user_config_dir() {
        let global_dir = config_dir.join("ta").join("plugins").join("social");
        scan_social_plugin_dir(&global_dir, SocialPluginSource::UserGlobal, &mut plugins);
    }

    // 2. Project-local
    let project_dir = project_root.join(".ta").join("plugins").join("social");
    scan_social_plugin_dir(&project_dir, SocialPluginSource::ProjectLocal, &mut plugins);

    plugins
}

/// Scan a directory for social plugin subdirectories containing `plugin.toml`.
fn scan_social_plugin_dir(
    dir: &Path,
    source: SocialPluginSource,
    out: &mut Vec<DiscoveredSocialPlugin>,
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
                "Failed to read social plugin directory"
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

        match SocialPluginManifest::load(&manifest_path) {
            Ok(manifest) => {
                tracing::debug!(
                    plugin = %manifest.name,
                    source = %source,
                    "Discovered social plugin"
                );
                out.push(DiscoveredSocialPlugin {
                    manifest,
                    plugin_dir: Some(path),
                    source: source.clone(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "Skipping invalid social plugin manifest"
                );
            }
        }
    }
}

/// Find a social plugin by platform name.
///
/// Searches user-global → project-local → PATH.
/// Returns `None` if no plugin is found for the given platform.
pub fn find_social_plugin(platform: &str, project_root: &Path) -> Option<DiscoveredSocialPlugin> {
    // Search manifest-based plugins.
    let all = discover_social_plugins(project_root);
    if let Some(p) = all.into_iter().find(|p| p.manifest.name == platform) {
        return Some(p);
    }

    // Fall back to bare PATH executable: `ta-social-<name>`.
    let bare_cmd = format!("ta-social-{}", platform);
    if which_on_path(&bare_cmd) {
        tracing::info!(
            platform = %platform,
            command = %bare_cmd,
            "Found social plugin as bare executable on PATH"
        );
        return Some(DiscoveredSocialPlugin {
            manifest: SocialPluginManifest {
                name: platform.to_string(),
                version: "unknown".to_string(),
                plugin_type: "social".to_string(),
                command: bare_cmd,
                args: vec![],
                capabilities: vec![
                    "create_draft".to_string(),
                    "create_scheduled".to_string(),
                    "draft_status".to_string(),
                    "health".to_string(),
                ],
                description: None,
                timeout_secs: 60,
                protocol_version: SOCIAL_PROTOCOL_VERSION,
            },
            plugin_dir: None,
            source: SocialPluginSource::Path,
        });
    }

    None
}

// ---------------------------------------------------------------------------
// ExternalSocialAdapter
// ---------------------------------------------------------------------------

/// Social media adapter that delegates all operations to an external plugin process.
///
/// Each method call spawns a fresh process, sends one JSON request line to
/// stdin, reads one JSON response line from stdout, then waits for exit.
#[derive(Debug)]
pub struct ExternalSocialAdapter {
    /// Plugin command to spawn.
    command: String,
    /// Additional pre-configured args.
    args: Vec<String>,
    /// Platform name (from manifest).
    platform: String,
    /// Per-call timeout.
    timeout: Duration,
}

impl ExternalSocialAdapter {
    /// Create a new adapter from a discovered plugin manifest.
    pub fn new(manifest: &SocialPluginManifest) -> Self {
        Self {
            command: manifest.command.clone(),
            args: manifest.args.clone(),
            platform: manifest.name.clone(),
            timeout: Duration::from_secs(manifest.timeout_secs),
        }
    }

    /// Platform name (e.g., "linkedin", "x", "buffer").
    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// Create a draft in the platform's native draft state.
    ///
    /// Returns the platform-assigned draft ID (e.g., "linkedin-draft-abc123").
    ///
    /// NOTE: There is intentionally no `publish` method on this type.
    /// TA never publishes social media posts on behalf of the user.
    pub fn create_draft(&self, post: SocialPostContent) -> Result<String, SocialPluginError> {
        let req = SocialPluginRequest::CreateDraft(CreateSocialDraftParams { post });
        let resp = self.call_plugin(&req, "create_draft")?;
        resp.draft_id
            .ok_or_else(|| SocialPluginError::InvalidResponse {
                name: self.platform.clone(),
                op: "create_draft".to_string(),
                reason: "response missing draft_id".to_string(),
            })
    }

    /// Schedule a post at a future time.
    ///
    /// Returns `(scheduled_id, confirmed_scheduled_at)`.
    ///
    /// The platform (or its scheduler) controls the actual publication.
    pub fn create_scheduled(
        &self,
        post: SocialPostContent,
        scheduled_at: &str,
    ) -> Result<(String, String), SocialPluginError> {
        let req = SocialPluginRequest::CreateScheduled(CreateScheduledParams {
            post,
            scheduled_at: scheduled_at.to_string(),
        });
        let resp = self.call_plugin(&req, "create_scheduled")?;
        let id = resp
            .scheduled_id
            .ok_or_else(|| SocialPluginError::InvalidResponse {
                name: self.platform.clone(),
                op: "create_scheduled".to_string(),
                reason: "response missing scheduled_id".to_string(),
            })?;
        let at = resp
            .scheduled_at
            .unwrap_or_else(|| scheduled_at.to_string());
        Ok((id, at))
    }

    /// Poll the current state of a previously created draft or scheduled post.
    pub fn draft_status(&self, draft_id: &str) -> Result<SocialPostState, SocialPluginError> {
        let req = SocialPluginRequest::DraftStatus(SocialDraftStatusParams {
            draft_id: draft_id.to_string(),
        });
        let resp = self.call_plugin(&req, "draft_status")?;
        Ok(resp.state.unwrap_or(SocialPostState::Unknown))
    }

    /// Run a health check: verify credentials and connectivity.
    ///
    /// Returns `(handle, provider_name)` on success.
    pub fn health(&self) -> Result<(String, String), SocialPluginError> {
        let req = SocialPluginRequest::Health(SocialHealthParams {});
        let resp = self.call_plugin(&req, "health")?;
        let handle = resp.handle.unwrap_or_else(|| "<unknown>".to_string());
        let provider = resp.provider.unwrap_or_else(|| self.platform.clone());
        Ok((handle, provider))
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn call_plugin(
        &self,
        req: &SocialPluginRequest,
        op: &str,
    ) -> Result<SocialPluginResponse, SocialPluginError> {
        let req_json = serde_json::to_string(req)?;

        let mut parts = self.command.split_whitespace();
        let program = parts.next().ok_or_else(|| SocialPluginError::SpawnFailed {
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

        let mut child = cmd.spawn().map_err(|e| SocialPluginError::SpawnFailed {
            command: self.command.clone(),
            reason: e.to_string(),
        })?;

        // Write request to stdin.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(req_json.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .map_err(|e| {
                    SocialPluginError::Io(std::io::Error::new(
                        e.kind(),
                        format!("failed to write to plugin stdin: {}", e),
                    ))
                })?;
        }

        // Wait with timeout.
        let timeout_ms = self.timeout.as_millis() as u64;
        let output =
            wait_with_timeout(child, timeout_ms).map_err(|_| SocialPluginError::Timeout {
                name: self.platform.clone(),
                op: op.to_string(),
                timeout_secs: self.timeout.as_secs(),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SocialPluginError::OpFailed {
                name: self.platform.clone(),
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
            return Err(SocialPluginError::InvalidResponse {
                name: self.platform.clone(),
                op: op.to_string(),
                reason: "plugin produced no output (expected one JSON line)".to_string(),
            });
        }

        let resp: SocialPluginResponse =
            serde_json::from_str(first_line).map_err(|e| SocialPluginError::InvalidResponse {
                name: self.platform.clone(),
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
            })?;

        if !resp.ok {
            return Err(SocialPluginError::OpFailed {
                name: self.platform.clone(),
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
// Supervisor check for social content
// ---------------------------------------------------------------------------

/// Result of a social content supervisor check.
#[derive(Debug, Clone)]
pub struct SocialSupervisorResult {
    /// Whether the content passed all checks.
    pub passed: bool,
    /// Human-readable reason for flagging (None if passed).
    pub flag_reason: Option<String>,
    /// Confidence score [0.0, 1.0].
    pub confidence: f64,
}

/// Social supervisor configuration.
#[derive(Debug, Clone, Default)]
pub struct SocialSupervisorConfig {
    /// Confidence below this threshold → review queue.
    pub min_confidence: f64,
    /// Substrings that trigger a flag if found in the post body.
    pub flag_if_contains: Vec<String>,
    /// Whether to check for patterns that look like unverified claims.
    pub check_unverified_claims: bool,
    /// Client names that must NOT appear unless explicitly allowed.
    pub blocked_client_names: Vec<String>,
}

/// Check social media post content against the supervisor policy.
///
/// Checks:
/// - `confidence >= min_confidence`
/// - No `flag_if_contains` substring appears in the post body
/// - No blocked client names in the post body (unless `allow_client_names` is true)
/// - Optionally checks for patterns that look like unverified claims
pub fn social_supervisor_check(
    body: &str,
    confidence: f64,
    config: &SocialSupervisorConfig,
    allow_client_names: bool,
) -> SocialSupervisorResult {
    // 1. Confidence threshold check.
    if confidence < config.min_confidence {
        return SocialSupervisorResult {
            passed: false,
            flag_reason: Some(format!(
                "supervisor confidence {:.2} below threshold {:.2}",
                confidence, config.min_confidence
            )),
            confidence,
        };
    }

    // 2. flag_if_contains checks.
    let body_lower = body.to_lowercase();
    for phrase in &config.flag_if_contains {
        if body_lower.contains(&phrase.to_lowercase()) {
            return SocialSupervisorResult {
                passed: false,
                flag_reason: Some(format!("post body contains flagged phrase: '{}'", phrase)),
                confidence,
            };
        }
    }

    // 3. Blocked client names.
    if !allow_client_names {
        for client in &config.blocked_client_names {
            if body_lower.contains(&client.to_lowercase()) {
                return SocialSupervisorResult {
                    passed: false,
                    flag_reason: Some(format!(
                        "post body contains client name '{}' (not allowed without explicit permission)",
                        client
                    )),
                    confidence,
                };
            }
        }
    }

    // 4. Unverified claims check (heuristic).
    if config.check_unverified_claims {
        let claim_patterns = [
            "guaranteed to",
            "100% proven",
            "scientifically proven",
            "always works",
            "never fails",
            "zero risk",
        ];
        for pattern in &claim_patterns {
            if body_lower.contains(pattern) {
                return SocialSupervisorResult {
                    passed: false,
                    flag_reason: Some(format!(
                        "post body contains potentially unverified claim: '{}'",
                        pattern
                    )),
                    confidence,
                };
            }
        }
    }

    SocialSupervisorResult {
        passed: true,
        flag_reason: None,
        confidence,
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
    fn discover_social_plugins_finds_manifests() {
        let root = tempfile::tempdir().unwrap();
        let social_dir = root.path().join(".ta").join("plugins").join("social");

        let linkedin_dir = social_dir.join("linkedin");
        std::fs::create_dir_all(&linkedin_dir).unwrap();
        write_manifest(
            &linkedin_dir,
            r#"
name = "linkedin"
version = "0.1.0"
type = "social"
command = "ta-social-linkedin"
capabilities = ["create_draft", "create_scheduled", "draft_status", "health"]
description = "LinkedIn social media adapter"
"#,
        );

        let plugins = discover_social_plugins(root.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "linkedin");
        assert_eq!(plugins[0].source, SocialPluginSource::ProjectLocal);
    }

    #[test]
    fn discover_social_plugins_skips_invalid_manifest() {
        let root = tempfile::tempdir().unwrap();
        let social_dir = root.path().join(".ta").join("plugins").join("social");

        let good_dir = social_dir.join("linkedin");
        std::fs::create_dir_all(&good_dir).unwrap();
        write_manifest(
            &good_dir,
            r#"name = "linkedin"
type = "social"
command = "ta-social-linkedin"
"#,
        );

        let bad_dir = social_dir.join("bad");
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(bad_dir.join("plugin.toml"), "{{not valid toml}}").unwrap();

        let plugins = discover_social_plugins(root.path());
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest.name, "linkedin");
    }

    #[test]
    fn discover_social_plugins_empty_dir_returns_empty() {
        let root = tempfile::tempdir().unwrap();
        let plugins = discover_social_plugins(root.path());
        assert!(plugins.is_empty());
    }

    #[test]
    fn find_social_plugin_project_local() {
        let root = tempfile::tempdir().unwrap();
        let social_dir = root.path().join(".ta").join("plugins").join("social");

        let x_dir = social_dir.join("x");
        std::fs::create_dir_all(&x_dir).unwrap();
        write_manifest(
            &x_dir,
            r#"name = "x"
type = "social"
command = "ta-social-x"
"#,
        );

        let found = find_social_plugin("x", root.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap().manifest.name, "x");
    }

    #[test]
    fn find_social_plugin_missing_returns_none() {
        let root = tempfile::tempdir().unwrap();
        let found = find_social_plugin("nonexistent-platform", root.path());
        assert!(found.is_none());
    }

    #[test]
    fn social_plugin_source_display() {
        assert_eq!(SocialPluginSource::UserGlobal.to_string(), "global");
        assert_eq!(SocialPluginSource::ProjectLocal.to_string(), "project");
        assert_eq!(SocialPluginSource::Path.to_string(), "PATH");
    }

    #[test]
    fn supervisor_check_passes_clean_content() {
        let config = SocialSupervisorConfig {
            min_confidence: 0.8,
            flag_if_contains: vec!["I promise".to_string()],
            check_unverified_claims: true,
            blocked_client_names: vec!["AcmeCorp".to_string()],
        };
        let result = social_supervisor_check(
            "Excited to share our new AI pipeline feature!",
            0.95,
            &config,
            false,
        );
        assert!(result.passed);
        assert!(result.flag_reason.is_none());
    }

    #[test]
    fn supervisor_check_fails_low_confidence() {
        let config = SocialSupervisorConfig {
            min_confidence: 0.8,
            ..Default::default()
        };
        let result = social_supervisor_check("Good content here", 0.5, &config, false);
        assert!(!result.passed);
        assert!(result.flag_reason.unwrap().contains("below threshold"));
    }

    #[test]
    fn supervisor_check_fails_flag_phrase() {
        let config = SocialSupervisorConfig {
            min_confidence: 0.0,
            flag_if_contains: vec!["I promise".to_string()],
            ..Default::default()
        };
        let result =
            social_supervisor_check("I promise this will work perfectly.", 1.0, &config, false);
        assert!(!result.passed);
        assert!(result.flag_reason.unwrap().contains("I promise"));
    }

    #[test]
    fn supervisor_check_fails_client_name() {
        let config = SocialSupervisorConfig {
            min_confidence: 0.0,
            blocked_client_names: vec!["SecretClient".to_string()],
            ..Default::default()
        };
        let result = social_supervisor_check(
            "Working with SecretClient on this amazing project!",
            1.0,
            &config,
            false,
        );
        assert!(!result.passed);
        assert!(result.flag_reason.unwrap().contains("client name"));
    }

    #[test]
    fn supervisor_check_allows_client_name_when_permitted() {
        let config = SocialSupervisorConfig {
            min_confidence: 0.0,
            blocked_client_names: vec!["SecretClient".to_string()],
            ..Default::default()
        };
        let result = social_supervisor_check(
            "Working with SecretClient on this amazing project!",
            1.0,
            &config,
            true, // explicitly allowed
        );
        assert!(result.passed);
    }

    #[test]
    fn supervisor_check_fails_unverified_claim() {
        let config = SocialSupervisorConfig {
            min_confidence: 0.0,
            check_unverified_claims: true,
            ..Default::default()
        };
        let result = social_supervisor_check(
            "This is guaranteed to increase your revenue by 500%!",
            1.0,
            &config,
            false,
        );
        assert!(!result.passed);
        assert!(result.flag_reason.unwrap().contains("unverified claim"));
    }

    /// Return the path to a shared mock social plugin binary.
    #[cfg(unix)]
    fn shared_mock_social_plugin_path() -> &'static std::path::Path {
        use std::io::Write as W;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::OnceLock;

        static MOCK_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();
        MOCK_PATH.get_or_init(|| {
            let pid = std::process::id();
            let name = format!("ta-social-mock-shared-{}", pid);

            #[cfg(target_os = "linux")]
            let path = {
                let shm = std::path::Path::new("/dev/shm");
                if shm.exists() {
                    shm.join(&name)
                } else {
                    std::path::PathBuf::from("/tmp").join(&name)
                }
            };
            #[cfg(not(target_os = "linux"))]
            let path = std::env::temp_dir().join(&name);

            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(
                br#"#!/bin/sh
read -r line
case "$line" in
  *create_draft*)    echo '{"ok":true,"draft_id":"linkedin-draft-abc123"}' ;;
  *create_scheduled*) echo '{"ok":true,"scheduled_id":"buffer-post-xyz","scheduled_at":"2026-04-07T14:00:00Z"}' ;;
  *)                 echo '{"ok":true,"handle":"@testuser","provider":"mock"}' ;;
esac
"#,
            )
            .unwrap();
            f.sync_all().unwrap();
            drop(f);

            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            let _ = std::fs::metadata(&path).unwrap();
            path
        })
    }

    #[cfg(unix)]
    #[test]
    fn external_adapter_health_returns_handle() {
        let plugin_path = shared_mock_social_plugin_path();
        let manifest = SocialPluginManifest {
            name: "mock".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "social".to_string(),
            command: plugin_path.display().to_string(),
            args: vec![],
            capabilities: vec!["health".to_string()],
            description: None,
            timeout_secs: 30,
            protocol_version: SOCIAL_PROTOCOL_VERSION,
        };

        let adapter = ExternalSocialAdapter::new(&manifest);
        let (handle, provider) = adapter.health().unwrap();
        assert_eq!(handle, "@testuser");
        assert_eq!(provider, "mock");
    }

    #[cfg(unix)]
    #[test]
    fn external_adapter_create_draft_returns_id() {
        let plugin_path = shared_mock_social_plugin_path();
        let manifest = SocialPluginManifest {
            name: "mock".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "social".to_string(),
            command: plugin_path.display().to_string(),
            args: vec![],
            capabilities: vec!["create_draft".to_string()],
            description: None,
            timeout_secs: 30,
            protocol_version: SOCIAL_PROTOCOL_VERSION,
        };

        let adapter = ExternalSocialAdapter::new(&manifest);
        let draft_id = adapter
            .create_draft(SocialPostContent {
                body: "Excited to share this!".to_string(),
                media_urls: vec![],
                reply_to_id: None,
            })
            .unwrap();
        assert_eq!(draft_id, "linkedin-draft-abc123");
    }

    #[cfg(unix)]
    #[test]
    fn external_adapter_create_scheduled_returns_id_and_time() {
        let plugin_path = shared_mock_social_plugin_path();
        let manifest = SocialPluginManifest {
            name: "mock".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "social".to_string(),
            command: plugin_path.display().to_string(),
            args: vec![],
            capabilities: vec!["create_scheduled".to_string()],
            description: None,
            timeout_secs: 30,
            protocol_version: SOCIAL_PROTOCOL_VERSION,
        };

        let adapter = ExternalSocialAdapter::new(&manifest);
        let (scheduled_id, scheduled_at) = adapter
            .create_scheduled(
                SocialPostContent {
                    body: "Scheduled post content".to_string(),
                    media_urls: vec![],
                    reply_to_id: None,
                },
                "2026-04-07T14:00:00Z",
            )
            .unwrap();
        assert_eq!(scheduled_id, "buffer-post-xyz");
        assert_eq!(scheduled_at, "2026-04-07T14:00:00Z");
    }
}
