//! External VCS adapter — SourceAdapter implementation over JSON-over-stdio.
//!
//! Wraps an external VCS plugin process and implements the `SourceAdapter`
//! trait by translating every method call into a single JSON-over-stdio
//! request/response exchange.
//!
//! ## Lifecycle
//!
//! For each method call, `ExternalVcsAdapter`:
//!
//! 1. Spawns the plugin process (fresh per call — plugins are stateless).
//! 2. Sends a `VcsPluginRequest` JSON line to the process's stdin.
//! 3. Reads the `VcsPluginResponse` JSON line from stdout.
//! 4. Waits for the process to exit.
//! 5. Returns the parsed result or a `SubmitError` on failure.
//!
//! ## Handshake
//!
//! The first call in `ExternalVcsAdapter::new()` performs a `handshake` to
//! validate protocol compatibility. An incompatible protocol version causes
//! construction to fail with `SubmitError::ConfigError`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use ta_changeset::DraftPackage;
use ta_goal::GoalRun;

use crate::adapter::{
    CommitResult, MergeResult, PushResult, Result, ReviewResult, ReviewStatus, SavedVcsState,
    SourceAdapter, SubmitError, SyncResult,
};
use crate::config::SubmitConfig;
use crate::vcs_plugin_manifest::VcsPluginManifest;
use crate::vcs_plugin_protocol::{
    CheckReviewParams, CheckReviewResult, CommitParams, CommitResult as PluginCommitResult,
    DetectParams, DetectResult, ExcludePatternsResult, HandshakeParams, HandshakeResult,
    MergeReviewParams, MergeReviewResult, OpenReviewParams, OpenReviewResult, PrepareParams,
    ProtectedTargetsResult, PushParams, PushResult as PluginPushResult, RestoreStateParams,
    SaveStateResult, SyncUpstreamResult, VcsPluginRequest, VcsPluginResponse, PROTOCOL_VERSION,
};

/// In-process representation of saved state from an external plugin.
struct ExternalSavedState {
    /// Raw JSON blob returned by the plugin's `save_state` response.
    state_json: serde_json::Value,
}

/// SourceAdapter that delegates all operations to an external plugin process.
#[derive(Debug)]
pub struct ExternalVcsAdapter {
    /// Plugin command to spawn (first word) and any pre-configured args.
    command: String,
    args: Vec<String>,
    /// Working directory for plugin invocations.
    work_dir: PathBuf,
    /// Plugin's self-reported adapter name (from handshake).
    adapter_name: String,
    /// Plugin version (from handshake; retained for diagnostics/logging).
    #[allow(dead_code)]
    plugin_version: String,
    /// Per-call process timeout.
    timeout: Duration,
    /// Capabilities reported by the plugin at handshake time.
    capabilities: Vec<String>,
    /// Static environment variables from the manifest's [staging_env] section (v0.13.17.3).
    staging_env: std::collections::HashMap<String, String>,
}

impl ExternalVcsAdapter {
    /// Create a new adapter and perform the initial handshake.
    ///
    /// Returns `SubmitError::ConfigError` if the plugin is not found,
    /// the handshake fails, or protocol versions are incompatible.
    pub fn new(
        manifest: &VcsPluginManifest,
        work_dir: impl Into<PathBuf>,
        ta_version: &str,
    ) -> Result<Self> {
        let work_dir = work_dir.into();
        let timeout = Duration::from_secs(manifest.timeout_secs);

        let handshake_params = HandshakeParams {
            ta_version: ta_version.to_string(),
            protocol_version: PROTOCOL_VERSION,
        };
        let request = VcsPluginRequest {
            method: "handshake".to_string(),
            params: serde_json::to_value(&handshake_params).map_err(|e| {
                SubmitError::ConfigError(format!("Failed to serialize handshake params: {}", e))
            })?,
        };

        let response = call_plugin(
            &manifest.command,
            &manifest.args,
            &work_dir,
            &request,
            timeout,
        )?;

        if !response.ok {
            return Err(SubmitError::ConfigError(format!(
                "VCS plugin '{}' handshake failed: {}",
                manifest.name,
                response.error.as_deref().unwrap_or("unknown error")
            )));
        }

        let result: HandshakeResult = serde_json::from_value(response.result).map_err(|e| {
            SubmitError::ConfigError(format!(
                "VCS plugin '{}' returned invalid handshake response: {}",
                manifest.name, e
            ))
        })?;

        if result.protocol_version != PROTOCOL_VERSION {
            return Err(SubmitError::ConfigError(format!(
                "VCS plugin '{}' uses protocol version {} but TA requires version {}. \
                 Upgrade the plugin or downgrade TA.",
                manifest.name, result.protocol_version, PROTOCOL_VERSION
            )));
        }

        tracing::info!(
            plugin = %manifest.name,
            plugin_version = %result.plugin_version,
            adapter = %result.adapter_name,
            "VCS plugin handshake successful"
        );

        Ok(Self {
            command: manifest.command.clone(),
            args: manifest.args.clone(),
            work_dir,
            adapter_name: result.adapter_name,
            plugin_version: result.plugin_version,
            timeout,
            capabilities: result.capabilities,
            staging_env: manifest.staging_env.clone(),
        })
    }

    /// Auto-detect using the plugin's `detect` method.
    pub fn detect_with_plugin(
        manifest: &VcsPluginManifest,
        project_root: &Path,
        ta_version: &str,
    ) -> bool {
        let timeout = Duration::from_secs(manifest.timeout_secs);
        let params = DetectParams {
            project_root: project_root.display().to_string(),
        };
        let request = VcsPluginRequest {
            method: "detect".to_string(),
            params: match serde_json::to_value(&params) {
                Ok(v) => v,
                Err(_) => return false,
            },
        };

        // Perform handshake first (required by protocol).
        let handshake_req = VcsPluginRequest {
            method: "handshake".to_string(),
            params: serde_json::json!({
                "ta_version": ta_version,
                "protocol_version": PROTOCOL_VERSION
            }),
        };
        if call_plugin(
            &manifest.command,
            &manifest.args,
            project_root,
            &handshake_req,
            timeout,
        )
        .map(|r| r.ok)
        .unwrap_or(false)
        {
            // Now call detect
            match call_plugin(
                &manifest.command,
                &manifest.args,
                project_root,
                &request,
                timeout,
            ) {
                Ok(resp) if resp.ok => serde_json::from_value::<DetectResult>(resp.result)
                    .map(|r| r.detected)
                    .unwrap_or(false),
                _ => false,
            }
        } else {
            false
        }
    }

    /// Call a plugin method and return the parsed result JSON value.
    fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let request = VcsPluginRequest {
            method: method.to_string(),
            params,
        };
        let response = call_plugin(
            &self.command,
            &self.args,
            &self.work_dir,
            &request,
            self.timeout,
        )?;

        if !response.ok {
            return Err(SubmitError::VcsError(format!(
                "VCS plugin '{}' method '{}' failed: {}",
                self.adapter_name,
                method,
                response.error.as_deref().unwrap_or("unknown error")
            )));
        }

        serde_json::from_value(response.result).map_err(|e| {
            SubmitError::VcsError(format!(
                "VCS plugin '{}' method '{}' returned invalid response: {}",
                self.adapter_name, method, e
            ))
        })
    }

    /// Whether this plugin declares a given capability.
    fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

impl SourceAdapter for ExternalVcsAdapter {
    fn prepare(&self, goal: &GoalRun, config: &SubmitConfig) -> Result<()> {
        let params = PrepareParams {
            goal_id: goal.goal_run_id.to_string(),
            goal_title: goal.title.clone(),
            workspace_path: goal.workspace_path.display().to_string(),
            branch_prefix: config.git.branch_prefix.clone(),
            co_author: if config.co_author.is_empty() {
                None
            } else {
                Some(config.co_author.clone())
            },
        };
        self.call::<serde_json::Value>("prepare", serde_json::to_value(&params).unwrap())?;
        Ok(())
    }

    fn commit(&self, goal: &GoalRun, pr: &DraftPackage, message: &str) -> Result<CommitResult> {
        let changed_files: Vec<String> = pr
            .changes
            .artifacts
            .iter()
            .map(|a| {
                a.resource_uri
                    .trim_start_matches("fs://workspace/")
                    .to_string()
            })
            .collect();

        let params = CommitParams {
            goal_id: goal.goal_run_id.to_string(),
            goal_title: goal.title.clone(),
            message: message.to_string(),
            changed_files,
        };

        let result: PluginCommitResult =
            self.call("commit", serde_json::to_value(&params).unwrap())?;

        Ok(CommitResult {
            commit_id: result.commit_id,
            message: result.message,
            metadata: result.metadata,
            ignored_artifacts: vec![],
        })
    }

    fn push(&self, goal: &GoalRun) -> Result<PushResult> {
        let params = PushParams {
            goal_id: goal.goal_run_id.to_string(),
        };
        let result: PluginPushResult = self.call("push", serde_json::to_value(&params).unwrap())?;

        Ok(PushResult {
            remote_ref: result.remote_ref,
            message: result.message,
            metadata: result.metadata,
        })
    }

    fn open_review(&self, goal: &GoalRun, pr: &DraftPackage) -> Result<ReviewResult> {
        let changed_files: Vec<String> = pr
            .changes
            .artifacts
            .iter()
            .map(|a| {
                a.resource_uri
                    .trim_start_matches("fs://workspace/")
                    .to_string()
            })
            .collect();

        let draft_summary = format!("{}\n{}", pr.summary.what_changed, pr.summary.why);

        let params = OpenReviewParams {
            goal_id: goal.goal_run_id.to_string(),
            goal_title: goal.title.clone(),
            draft_summary,
            changed_files,
        };
        let result: OpenReviewResult =
            self.call("open_review", serde_json::to_value(&params).unwrap())?;

        Ok(ReviewResult {
            review_url: result.review_url,
            review_id: result.review_id,
            message: result.message,
            metadata: result.metadata,
        })
    }

    fn sync_upstream(&self) -> Result<SyncResult> {
        let result: SyncUpstreamResult = self.call(
            "sync_upstream",
            serde_json::Value::Object(Default::default()),
        )?;

        Ok(SyncResult {
            updated: result.updated,
            conflicts: result.conflicts,
            new_commits: result.new_commits,
            message: result.message,
            metadata: result.metadata,
        })
    }

    fn name(&self) -> &str {
        &self.adapter_name
    }

    fn exclude_patterns(&self) -> Vec<String> {
        self.call::<ExcludePatternsResult>(
            "exclude_patterns",
            serde_json::Value::Object(Default::default()),
        )
        .map(|r| r.patterns)
        .unwrap_or_else(|e| {
            tracing::warn!(
                adapter = %self.adapter_name,
                error = %e,
                "VCS plugin exclude_patterns failed — using empty list"
            );
            vec![]
        })
    }

    fn commit_diff(&self) -> Option<String> {
        if !self.has_capability("commit_diff") {
            return None;
        }
        self.call::<serde_json::Value>("commit_diff", serde_json::Value::Object(Default::default()))
            .ok()
            .and_then(|v| {
                v.get("diff")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string())
            })
    }

    fn save_state(&self) -> Result<Option<SavedVcsState>> {
        let result: SaveStateResult =
            self.call("save_state", serde_json::Value::Object(Default::default()))?;

        if result.state.is_null() {
            return Ok(None);
        }

        Ok(Some(SavedVcsState {
            adapter: self.adapter_name.clone(),
            data: Box::new(ExternalSavedState {
                state_json: result.state,
            }),
        }))
    }

    fn restore_state(&self, state: Option<SavedVcsState>) -> Result<()> {
        let state_json = match state {
            None => serde_json::Value::Null,
            Some(s) => {
                if s.adapter != self.adapter_name {
                    return Err(SubmitError::InvalidState(format!(
                        "Cannot restore state from adapter '{}' in ExternalVcsAdapter for '{}'",
                        s.adapter, self.adapter_name
                    )));
                }
                match s.data.downcast::<ExternalSavedState>() {
                    Ok(ext) => ext.state_json,
                    Err(_) => {
                        return Err(SubmitError::InvalidState(
                            "State data is not ExternalSavedState".to_string(),
                        ));
                    }
                }
            }
        };

        let params = RestoreStateParams { state: state_json };
        self.call::<serde_json::Value>("restore_state", serde_json::to_value(&params).unwrap())?;
        Ok(())
    }

    fn revision_id(&self) -> Result<String> {
        let result: crate::vcs_plugin_protocol::RevisionIdResult =
            self.call("revision_id", serde_json::Value::Object(Default::default()))?;
        Ok(result.revision_id)
    }

    fn check_review(&self, review_id: &str) -> Result<Option<ReviewStatus>> {
        let params = CheckReviewParams {
            review_id: review_id.to_string(),
        };
        let result: CheckReviewResult =
            self.call("check_review", serde_json::to_value(&params).unwrap())?;

        if !result.found {
            return Ok(None);
        }

        Ok(Some(ReviewStatus {
            state: result.state,
            checks_passing: result.checks_passing,
        }))
    }

    fn merge_review(&self, review_id: &str) -> Result<MergeResult> {
        let params = MergeReviewParams {
            review_id: review_id.to_string(),
        };
        let result: MergeReviewResult =
            self.call("merge_review", serde_json::to_value(&params).unwrap())?;

        Ok(MergeResult {
            merged: result.merged,
            merge_commit: result.merge_commit,
            message: result.message,
            metadata: result.metadata,
        })
    }

    fn protected_submit_targets(&self) -> Vec<String> {
        if !self.has_capability("protected_targets") {
            return vec![];
        }
        self.call::<ProtectedTargetsResult>(
            "protected_targets",
            serde_json::Value::Object(Default::default()),
        )
        .map(|r| r.targets)
        .unwrap_or_else(|e| {
            tracing::warn!(
                adapter = %self.adapter_name,
                error = %e,
                "VCS plugin protected_targets failed — returning empty list"
            );
            vec![]
        })
    }

    fn verify_not_on_protected_target(&self) -> Result<()> {
        if !self.has_capability("protected_targets") {
            // Plugin doesn't claim §15 compliance — skip check, log notice.
            tracing::debug!(
                adapter = %self.adapter_name,
                "VCS plugin does not declare 'protected_targets' capability; \
                 skipping §15 verify_target check"
            );
            return Ok(());
        }

        let response = {
            let request = VcsPluginRequest {
                method: "verify_target".to_string(),
                params: serde_json::Value::Object(Default::default()),
            };
            call_plugin(
                &self.command,
                &self.args,
                &self.work_dir,
                &request,
                self.timeout,
            )?
        };

        if response.ok {
            Ok(())
        } else {
            Err(SubmitError::InvalidState(response.error.unwrap_or_else(
                || "VCS plugin verify_target returned ok=false".to_string(),
            )))
        }
    }

    fn stage_env(
        &self,
        _staging_dir: &Path,
        _config: &crate::config::VcsAgentConfig,
    ) -> Result<std::collections::HashMap<String, String>> {
        // Return static vars from the manifest's [staging_env] section.
        Ok(self.staging_env.clone())
    }
}

// ---------------------------------------------------------------------------
// Low-level plugin call
// ---------------------------------------------------------------------------

/// Spawn the plugin, send one JSON request, read one JSON response.
///
/// Returns `SubmitError::IoError` / `SubmitError::VcsError` on infrastructure
/// failures. Never panics.
fn call_plugin(
    command: &str,
    extra_args: &[String],
    work_dir: &Path,
    request: &VcsPluginRequest,
    timeout: Duration,
) -> Result<VcsPluginResponse> {
    let request_json = serde_json::to_string(request).map_err(|e| {
        SubmitError::VcsError(format!(
            "Failed to serialize VCS plugin request for method '{}': {}",
            request.method, e
        ))
    })?;

    // Split command into program + built-in args.
    let mut parts = command.split_whitespace();
    let program = parts.next().ok_or_else(|| {
        SubmitError::ConfigError(format!(
            "VCS plugin command is empty for method '{}'",
            request.method
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

    // Retry on ETXTBSY (os error 26): on Linux the kernel can return this when a
    // freshly-written executable has not yet been fully flushed through the page
    // cache / copy-up layer (common on overlayfs in Nix CI). A short backoff is
    // sufficient; real plugin binaries never trigger this in production.
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
                    return Err(SubmitError::VcsError(format!(
                        "Failed to spawn VCS plugin '{}' for method '{}': {}. \
                         Ensure the plugin is installed and on PATH.",
                        command, request.method, e
                    )));
                }
            }
        }
        spawned.ok_or_else(|| {
            let e = last_err.unwrap();
            SubmitError::VcsError(format!(
                "Failed to spawn VCS plugin '{}' for method '{}': {}. \
                 Ensure the plugin is installed and on PATH.",
                command, request.method, e
            ))
        })?
    };

    // Write request to stdin.
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request_json.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .map_err(|e| {
                SubmitError::VcsError(format!(
                    "Failed to write to VCS plugin '{}' stdin: {}",
                    command, e
                ))
            })?;
    }

    // Wait with timeout (blocking — VCS operations are called synchronously).
    // We use a thread with a join timeout since std::process::Child has no
    // built-in timeout.
    let timeout_millis = timeout.as_millis() as u64;
    let output = wait_with_timeout(child, timeout_millis).map_err(|e| {
        SubmitError::VcsError(format!(
            "VCS plugin '{}' timed out or failed for method '{}': {}. \
             Increase timeout_secs in plugin.toml.",
            command, request.method, e
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SubmitError::VcsError(format!(
            "VCS plugin '{}' exited with status {} for method '{}'. stderr: {}",
            command,
            output.status,
            request.method,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("").trim();

    if first_line.is_empty() {
        return Err(SubmitError::VcsError(format!(
            "VCS plugin '{}' produced no output for method '{}'. \
             Plugin must write one JSON line to stdout.",
            command, request.method
        )));
    }

    serde_json::from_str(first_line).map_err(|e| {
        SubmitError::VcsError(format!(
            "VCS plugin '{}' produced invalid JSON for method '{}': {}. Got: '{}'",
            command,
            request.method,
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
///
/// Uses an `mpsc` channel to signal the watchdog thread as soon as the child
/// exits, so `join()` returns immediately rather than blocking for the full
/// `timeout_ms` on every successful (fast) invocation.
fn wait_with_timeout(
    child: std::process::Child,
    timeout_ms: u64,
) -> std::result::Result<std::process::Output, String> {
    use std::sync::mpsc;

    let child_id = child.id();
    let (tx, rx) = mpsc::channel::<()>();

    // Watchdog thread: waits for the "done" signal or the timeout, then kills.
    let watchdog = std::thread::spawn(move || {
        match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(()) => {
                // Child exited normally — nothing to do.
            }
            Err(_) => {
                // Timeout expired (or sender dropped on early `?` return) — kill the child.
                #[cfg(unix)]
                unsafe {
                    libc::kill(child_id as libc::pid_t, libc::SIGKILL);
                }
                #[cfg(not(unix))]
                {
                    let _ = child_id;
                }
            }
        }
    });

    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait_with_output failed: {}", e))?;

    // Signal the watchdog that the child has exited — it will wake immediately.
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

    /// Write a shell-script mock plugin to a temp file and make it executable.
    ///
    /// On Linux we write to `/tmp` directly (always tmpfs) rather than using the
    /// test's `tempdir()` path.  `tempdir()` respects `$TMPDIR` which Nix's devShell
    /// sets to an overlayfs-backed directory; exec-ing a newly created file there
    /// races against the kernel's copy-up and returns ETXTBSY (os error 26).
    /// On macOS and other platforms the provided `dir` is used as normal.
    fn write_mock_plugin(_dir: &Path, script: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let name = format!("ta-submit-mock-{}-{}", pid, n);
        // On Linux use /tmp (tmpfs) to avoid ETXTBSY on overlayfs-backed TMPDIR.
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
        // Read back metadata to ensure the chmod is visible before exec.
        let _ = std::fs::metadata(&path).unwrap();
        path
    }

    fn mock_manifest(command: &str, _dir: &Path) -> VcsPluginManifest {
        VcsPluginManifest {
            name: "mock".to_string(),
            version: "0.1.0".to_string(),
            plugin_type: "vcs".to_string(),
            command: command.to_string(),
            args: vec![],
            capabilities: vec!["commit".to_string(), "protected_targets".to_string()],
            description: None,
            timeout_secs: 30,
            min_daemon_version: None,
            source_url: None,
            staging_env: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn call_plugin_with_echo_script() {
        let dir = tempfile::tempdir().unwrap();

        // A minimal shell script that echoes a valid handshake response.
        let plugin_path = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"0.1.0","protocol_version":1,"adapter_name":"mock","capabilities":["commit","protected_targets"]}}'
"#,
        );

        let manifest = mock_manifest(&plugin_path.display().to_string(), dir.path());

        let adapter = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap();
        assert_eq!(adapter.name(), "mock");
        assert_eq!(adapter.plugin_version, "0.1.0");
    }

    #[test]
    fn handshake_protocol_mismatch_returns_error() {
        let dir = tempfile::tempdir().unwrap();

        // Plugin claims protocol version 99 — incompatible.
        let plugin_path = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo '{"ok":true,"result":{"plugin_version":"0.1.0","protocol_version":99,"adapter_name":"bad","capabilities":[]}}'
"#,
        );

        let manifest = mock_manifest(&plugin_path.display().to_string(), dir.path());

        let err = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap_err();
        assert!(
            err.to_string().contains("protocol version"),
            "Expected protocol version error, got: {}",
            err
        );
    }

    #[test]
    fn handshake_error_response_returns_error() {
        let dir = tempfile::tempdir().unwrap();

        let plugin_path = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo '{"ok":false,"error":"plugin initialization failed"}'
"#,
        );

        let manifest = mock_manifest(&plugin_path.display().to_string(), dir.path());

        let err = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("handshake failed") || msg.contains("timed out") || msg.contains("error"),
            "Expected handshake failure, got: {}",
            msg
        );
    }

    #[test]
    fn missing_command_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = mock_manifest("ta-submit-nonexistent-binary-xyz", dir.path());

        let err = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap_err();
        assert!(
            err.to_string().contains("spawn") || err.to_string().contains("No such file"),
            "Expected spawn error, got: {}",
            err
        );
    }

    #[test]
    fn plugin_non_zero_exit_returns_error() {
        let dir = tempfile::tempdir().unwrap();

        let plugin_path = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo "some error to stderr" >&2
exit 1
"#,
        );

        let manifest = mock_manifest(&plugin_path.display().to_string(), dir.path());

        let err = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap_err();
        assert!(
            err.to_string().contains("exited with status"),
            "Got: {}",
            err
        );
    }

    #[test]
    fn plugin_invalid_json_output_returns_error() {
        let dir = tempfile::tempdir().unwrap();

        let plugin_path = write_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
read -r line
echo "this is not json"
"#,
        );

        let manifest = mock_manifest(&plugin_path.display().to_string(), dir.path());

        let err = ExternalVcsAdapter::new(&manifest, dir.path(), "0.13.5-alpha").unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "Got: {}", err);
    }
}
