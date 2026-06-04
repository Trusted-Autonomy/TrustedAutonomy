// sandbox.rs — Agent process sandboxing (v0.14.0; Windows Job Objects v0.16.4).
//
// Wraps a SpawnRequest to apply OS-level sandboxing before the agent process
// starts.  On macOS this uses `sandbox-exec` with a generated `.sb` profile;
// on Linux it uses `bwrap` (bubblewrap) when available; on Windows it uses
// Job Objects (see `sandbox_windows.rs`).
//
// Sandboxing is opt-in: `SandboxPolicy::disabled()` is the default and
// passes requests through unchanged.  Enable via `[sandbox] enabled = true`
// in `.ta/workflow.toml`.
//
// ## macOS sandbox-exec
//
// The generated profile:
// 1. Denies all access by default (`(deny default)`).
// 2. Allows read of the OS system libraries (`/usr`, `/System`, `/Library`).
// 3. Allows read+write of the staging workspace (the agent's working dir).
// 4. Allows additional paths declared in `allow_read` / `allow_write`.
// 5. Allows network to declared `allow_network` hosts (or all if "*" present).
//
// ## Linux bubblewrap (bwrap)
//
// When `bwrap` is on PATH, wraps the agent with filesystem namespacing:
// - Bind-mounts declared readable paths as ro
// - Bind-mounts the working dir as rw
// - Creates tmpfs for /tmp
// - Network: unshared by default (--unshare-net) unless allow_network is non-empty
//
// ## Windows Job Objects
//
// Creates a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.  The agent
// process (and all its children) are assigned to the Job Object after spawn.
// When TA exits or the guard is dropped, the entire process tree is killed.
// Note: Job Objects do not restrict filesystem access — that requires AppContainer.

#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

use crate::adapter::SpawnRequest;
use crate::sandbox_windows::WindowsJobObjectGuard;

/// A resolved sandbox policy derived from `SandboxConfig`.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Whether sandboxing is active.
    pub enabled: bool,

    /// Which sandbox implementation to use.
    pub provider: SandboxProvider,

    /// Paths the agent may read (in addition to system libraries and its workspace).
    pub allow_read: Vec<PathBuf>,

    /// Paths the agent may write (workspace root is always included).
    pub allow_write: Vec<PathBuf>,

    /// Network destinations the agent may reach.
    /// If empty, all outbound network is blocked.
    /// If contains "*", all outbound network is allowed.
    pub allow_network: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxProvider {
    /// No sandboxing — pass request through unchanged.
    None,
    /// macOS sandbox-exec (Seatbelt).
    MacosSandboxExec,
    /// Linux bubblewrap (bwrap).
    LinuxBwrap,
    /// Windows Job Objects — process-tree teardown on TA exit.
    ///
    /// Does not restrict filesystem access (AppContainer handles that).
    /// Applied post-spawn via `SandboxPolicy::post_spawn_apply()`.
    WindowsJobObject,
    /// Windows AppContainer — filesystem + network isolation (v0.16.4.2).
    ///
    /// Spawns the agent inside a named AppContainer that:
    ///   - Restricts filesystem writes to the staging workspace (via DACL grant)
    ///   - Blocks outbound network unless `internetClient` capability is declared
    ///
    /// A Job Object is also attached post-spawn for process-tree teardown.
    /// Applied pre-spawn via `SandboxPolicy::pre_spawn_appcontainer()`.
    WindowsAppContainer,
}

impl SandboxPolicy {
    /// A no-op policy (sandboxing disabled).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            provider: SandboxProvider::None,
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: Vec::new(),
        }
    }

    /// Detect which provider is available on the current platform.
    ///
    /// On macOS: always use sandbox-exec (built-in).
    /// On Linux: use bwrap if available on PATH.
    /// On Windows: always use Job Objects (built-in; no external tool needed).
    /// Elsewhere: no sandboxing.
    pub fn detect_provider() -> SandboxProvider {
        #[cfg(target_os = "macos")]
        {
            SandboxProvider::MacosSandboxExec
        }
        #[cfg(target_os = "linux")]
        {
            if which_bwrap() {
                SandboxProvider::LinuxBwrap
            } else {
                SandboxProvider::None
            }
        }
        #[cfg(target_os = "windows")]
        {
            // Prefer AppContainer (filesystem + network isolation) over Job Objects
            // (process-tree teardown only).  AppContainer requires Windows 8+; the
            // binary only loads there, so appcontainer_available() is a runtime guard
            // against restricted execution environments (e.g., nested containers).
            if crate::sandbox_windows::appcontainer_available() {
                SandboxProvider::WindowsAppContainer
            } else {
                SandboxProvider::WindowsJobObject
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            SandboxProvider::None
        }
    }

    /// Apply this policy to a SpawnRequest, wrapping it in the sandbox.
    ///
    /// If disabled or provider is None, returns the request unchanged.
    ///
    /// For `WindowsJobObject`, this is a no-op — Job Objects are applied
    /// after the process is spawned via `post_spawn_apply()`.
    pub fn apply(&self, request: SpawnRequest) -> SpawnRequest {
        if !self.enabled || self.provider == SandboxProvider::None {
            return request;
        }

        match self.provider {
            SandboxProvider::MacosSandboxExec => self.apply_macos(request),
            SandboxProvider::LinuxBwrap => self.apply_linux_bwrap(request),
            // Job Objects are applied post-spawn; the SpawnRequest is unchanged.
            SandboxProvider::WindowsJobObject => request,
            // AppContainer is applied pre-spawn via pre_spawn_appcontainer(); unchanged here.
            SandboxProvider::WindowsAppContainer => request,
            SandboxProvider::None => request,
        }
    }

    /// Set up an AppContainer profile before the agent process is spawned (Windows only).
    ///
    /// Returns `Some(guard)` when the provider is `WindowsAppContainer` and setup succeeds.
    /// Returns `None` when not on Windows, provider is not `WindowsAppContainer`, or disabled.
    ///
    /// The guard must be kept alive for the agent's entire lifetime — it is passed to
    /// `sandboxed_spawn` to launch the process inside the container, and its Drop
    /// implementation deletes the profile.
    ///
    /// `goal_id` is used to derive a unique container name (first 8 hex chars).
    pub fn pre_spawn_appcontainer(
        &self,
        staging_path: &std::path::Path,
        goal_id: &str,
    ) -> Result<Option<crate::sandbox_windows::WindowsAppContainerGuard>, String> {
        if !self.enabled || self.provider != SandboxProvider::WindowsAppContainer {
            return Ok(None);
        }
        // Container name: "ta-" + first 8 chars of goal ID (unique per goal).
        let container_name = format!("ta-{}", goal_id.chars().take(8).collect::<String>());
        let allow_network = !self.allow_network.is_empty();
        let guard = crate::sandbox_windows::WindowsAppContainerGuard::new(
            &container_name,
            staging_path,
            allow_network,
        )?;
        Ok(Some(guard))
    }

    /// Create a Job Object guard and assign the agent process to it (Windows only).
    ///
    /// Call immediately after spawning the agent process. Hold the returned guard alive for
    /// the duration of the agent run — dropping it closes the Job Object handle and kills
    /// the entire process tree (`KILL_ON_JOB_CLOSE`).
    ///
    /// Activates for both `WindowsJobObject` and `WindowsAppContainer` providers so
    /// process-tree teardown works alongside AppContainer filesystem/network isolation.
    ///
    /// Returns `None` when sandboxing is disabled, provider is not a Windows variant,
    /// or Job Object creation fails (a structured warning is logged in that case).
    pub fn post_spawn_apply(&self, pid: u32) -> Option<WindowsJobObjectGuard> {
        // Attach a Job Object for process-tree teardown on both WindowsJobObject
        // and WindowsAppContainer providers (AppContainer + Job Object work together).
        if !self.enabled
            || !matches!(
                self.provider,
                SandboxProvider::WindowsJobObject | SandboxProvider::WindowsAppContainer
            )
        {
            return None;
        }

        let guard = match WindowsJobObjectGuard::new() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to create Windows Job Object — agent process tree will not be \
                     auto-killed on TA exit. Check that TA is running without AppContainer \
                     nesting restrictions."
                );
                return None;
            }
        };

        if let Err(e) = guard.assign_process(pid) {
            tracing::warn!(
                pid = pid,
                error = %e,
                "Failed to assign agent process to Job Object — process tree teardown \
                 will not be enforced."
            );
            return None;
        }

        tracing::info!(
            pid = pid,
            "Agent process assigned to Windows Job Object (KILL_ON_JOB_CLOSE)"
        );
        Some(guard)
    }

    /// Wrap the request in `sandbox-exec -p <profile> -- <cmd> <args>`.
    #[cfg(target_os = "macos")]
    fn apply_macos(&self, mut request: SpawnRequest) -> SpawnRequest {
        let profile = self.generate_macos_profile(&request.working_dir);

        // Build: sandbox-exec -p "<profile>" -- <original_cmd> <original_args>
        let mut new_args = vec![
            "-p".to_string(),
            profile,
            "--".to_string(),
            request.command.clone(),
        ];
        new_args.extend(request.args.iter().cloned());

        request.command = "sandbox-exec".to_string();
        request.args = new_args;
        request
    }

    #[cfg(not(target_os = "macos"))]
    fn apply_macos(&self, request: SpawnRequest) -> SpawnRequest {
        request
    }

    /// Generate a macOS Seatbelt (.sb) profile string.
    #[cfg(target_os = "macos")]
    fn generate_macos_profile(&self, working_dir: &Path) -> String {
        let mut lines = vec![
            // Deny everything by default.
            "(version 1)".to_string(),
            "(deny default)".to_string(),
            // Allow reading system libraries and tools.
            r#"(allow file-read* (subpath "/usr"))"#.to_string(),
            r#"(allow file-read* (subpath "/System"))"#.to_string(),
            r#"(allow file-read* (subpath "/Library/Frameworks"))"#.to_string(),
            r#"(allow file-read* (subpath "/private/etc"))"#.to_string(),
            // Allow reading the home directory's nix profile (for Nix devShell tools).
            r#"(allow file-read* (subpath "/nix"))"#.to_string(),
            // Process and signal operations the agent needs.
            r#"(allow process-exec*)"#.to_string(),
            r#"(allow process-fork)"#.to_string(),
            r#"(allow signal (target self))"#.to_string(),
            // Mach IPC needed for basic OS operations.
            r#"(allow mach-lookup)"#.to_string(),
            r#"(allow ipc-posix-shm)"#.to_string(),
            // Allow writing to /dev/null and /dev/tty.
            r#"(allow file-write* (subpath "/dev"))"#.to_string(),
            r#"(allow file-read* (subpath "/dev"))"#.to_string(),
            // Allow writing temp files.
            r#"(allow file-write* (subpath "/private/tmp"))"#.to_string(),
            r#"(allow file-read* (subpath "/private/tmp"))"#.to_string(),
        ];

        // Allow read+write of the staging workspace.
        let workspace = working_dir.to_string_lossy();
        lines.push(format!(
            r#"(allow file-read* file-write* (subpath "{}"))"#,
            sandbox_escape(&workspace)
        ));

        // Additional allowed read paths.
        for path in &self.allow_read {
            let p = path.to_string_lossy();
            lines.push(format!(
                r#"(allow file-read* (subpath "{}"))"#,
                sandbox_escape(&p)
            ));
        }

        // Additional allowed write paths.
        for path in &self.allow_write {
            let p = path.to_string_lossy();
            lines.push(format!(
                r#"(allow file-read* file-write* (subpath "{}"))"#,
                sandbox_escape(&p)
            ));
        }

        // Network: allow outbound if any destinations declared; deny if empty.
        if !self.allow_network.is_empty() {
            if self.allow_network.iter().any(|h| h == "*") {
                // Wildcard — allow all network.
                lines.push(r#"(allow network*)"#.to_string());
            } else {
                // Allow DNS + outbound connections to declared hosts.
                // macOS sandbox profiles can't filter by hostname directly;
                // we allow all network for now and rely on policy auditing.
                // TODO(v0.14.1): L7 proxy for hostname-scoped network filtering.
                lines.push(r#"(allow network-outbound)"#.to_string());
                lines.push(r#"(allow network-inbound (local localhost))"#.to_string());
                lines.push(r#"(allow system-socket)"#.to_string());
            }
        }
        // If allow_network is empty: no network rules added → network is denied by default.

        lines.join("\n")
    }

    /// Wrap the request in `bwrap` with filesystem namespacing.
    #[cfg(target_os = "linux")]
    fn apply_linux_bwrap(&self, mut request: SpawnRequest) -> SpawnRequest {
        let mut bwrap_args: Vec<String> = Vec::new();

        // Bind-mount essential system paths as read-only.
        for ro_path in &["/usr", "/lib", "/lib64", "/etc/ssl", "/etc/resolv.conf"] {
            if std::path::Path::new(ro_path).exists() {
                bwrap_args.push("--ro-bind".to_string());
                bwrap_args.push(ro_path.to_string());
                bwrap_args.push(ro_path.to_string());
            }
        }
        // /nix (for Nix devShell environments).
        if std::path::Path::new("/nix").exists() {
            bwrap_args.push("--ro-bind".to_string());
            bwrap_args.push("/nix".to_string());
            bwrap_args.push("/nix".to_string());
        }

        // Bind-mount the workspace as read-write.
        let workspace = request.working_dir.to_string_lossy().to_string();
        bwrap_args.push("--bind".to_string());
        bwrap_args.push(workspace.clone());
        bwrap_args.push(workspace);

        // Additional allowed read paths.
        for path in &self.allow_read {
            let p = path.to_string_lossy().to_string();
            bwrap_args.push("--ro-bind".to_string());
            bwrap_args.push(p.clone());
            bwrap_args.push(p);
        }

        // Additional writable paths.
        for path in &self.allow_write {
            let p = path.to_string_lossy().to_string();
            bwrap_args.push("--bind".to_string());
            bwrap_args.push(p.clone());
            bwrap_args.push(p);
        }

        // Tmpfs for /tmp.
        bwrap_args.push("--tmpfs".to_string());
        bwrap_args.push("/tmp".to_string());

        // Proc filesystem (required by many tools).
        bwrap_args.push("--proc".to_string());
        bwrap_args.push("/proc".to_string());

        // Network: unshare unless allow_network is non-empty.
        if self.allow_network.is_empty() {
            bwrap_args.push("--unshare-net".to_string());
        }

        // Terminate bwrap args, then original command.
        bwrap_args.push("--".to_string());
        bwrap_args.push(request.command.clone());
        bwrap_args.extend(request.args.iter().cloned());

        request.command = "bwrap".to_string();
        request.args = bwrap_args;
        request
    }

    #[cfg(not(target_os = "linux"))]
    fn apply_linux_bwrap(&self, request: SpawnRequest) -> SpawnRequest {
        request
    }
}

/// Escape a path for inclusion in a macOS sandbox profile string.
#[cfg(target_os = "macos")]
fn sandbox_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "linux")]
fn which_bwrap() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    fn dummy_request(working_dir: &Path) -> SpawnRequest {
        SpawnRequest {
            command: "claude".to_string(),
            args: vec!["--print".to_string(), "hello".to_string()],
            env: HashMap::new(),
            working_dir: working_dir.to_path_buf(),
            stdin_mode: crate::adapter::StdinMode::Null,
            stdout_mode: crate::adapter::StdoutMode::Inherited,
        }
    }

    #[test]
    fn disabled_policy_passthrough() {
        let policy = SandboxPolicy::disabled();
        let req = dummy_request(std::path::Path::new("/tmp/staging"));
        let wrapped = policy.apply(req.clone());
        assert_eq!(wrapped.command, req.command);
        assert_eq!(wrapped.args, req.args);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_escape_handles_quotes() {
        assert_eq!(
            sandbox_escape(r#"/path/with "quotes""#),
            r#"/path/with \"quotes\""#
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_escape_handles_backslash() {
        assert_eq!(sandbox_escape(r#"C:\path"#), r#"C:\\path"#);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_contains_working_dir() {
        let policy = SandboxPolicy {
            enabled: true,
            provider: SandboxProvider::MacosSandboxExec,
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: Vec::new(),
        };
        let working_dir = std::path::Path::new("/tmp/ta-staging/abc123");
        let profile = policy.generate_macos_profile(working_dir);
        assert!(
            profile.contains("/tmp/ta-staging/abc123"),
            "profile should include workspace"
        );
        assert!(
            profile.contains("(deny default)"),
            "profile should deny by default"
        );
        assert!(
            !profile.contains("network"),
            "no network rules when allow_network is empty"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_sandbox_exec_wraps_command() {
        let policy = SandboxPolicy {
            enabled: true,
            provider: SandboxProvider::MacosSandboxExec,
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: vec!["api.anthropic.com".to_string()],
        };
        let req = dummy_request(std::path::Path::new("/tmp/staging"));
        let wrapped = policy.apply(req);
        assert_eq!(wrapped.command, "sandbox-exec");
        assert_eq!(wrapped.args[0], "-p");
        assert_eq!(wrapped.args[2], "--");
        assert_eq!(wrapped.args[3], "claude");
        assert_eq!(wrapped.args[4], "--print");
    }

    /// WindowsJobObject apply() is a no-op — Job Objects are applied post-spawn.
    #[test]
    fn windows_job_object_apply_does_not_wrap_command() {
        let policy = SandboxPolicy {
            enabled: true,
            provider: SandboxProvider::WindowsJobObject,
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: Vec::new(),
        };
        let req = dummy_request(std::path::Path::new("/tmp/staging"));
        let wrapped = policy.apply(req.clone());
        assert_eq!(
            wrapped.command, req.command,
            "WindowsJobObject must not change the command"
        );
        assert_eq!(
            wrapped.args, req.args,
            "WindowsJobObject must not change the args"
        );
    }

    /// detect_provider() returns a Windows provider on Win10/Win11, macOS provider on macOS.
    #[test]
    fn detect_provider_windows() {
        let provider = SandboxPolicy::detect_provider();
        #[cfg(target_os = "windows")]
        // On Win10/Win11 (the supported range) AppContainer APIs are available, so
        // WindowsAppContainer is preferred. WindowsJobObject is the fallback for
        // environments where AppContainer is restricted (e.g. nested containers).
        assert!(
            provider == SandboxProvider::WindowsAppContainer
                || provider == SandboxProvider::WindowsJobObject,
            "Windows provider must be WindowsAppContainer or WindowsJobObject, got {:?}",
            provider
        );
        #[cfg(target_os = "macos")]
        assert_eq!(provider, SandboxProvider::MacosSandboxExec);
        // Linux returns LinuxBwrap (if bwrap present) or None — just verify no panic.
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = provider;
    }

    /// post_spawn_apply returns None when sandboxing is disabled.
    #[test]
    fn post_spawn_apply_disabled_returns_none() {
        let policy = SandboxPolicy::disabled();
        let guard = policy.post_spawn_apply(1234);
        assert!(guard.is_none(), "disabled policy should return None");
    }

    /// post_spawn_apply returns None for non-WindowsJobObject providers.
    #[test]
    fn post_spawn_apply_non_windows_provider_returns_none() {
        let policy = SandboxPolicy {
            enabled: true,
            provider: SandboxProvider::MacosSandboxExec,
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: Vec::new(),
        };
        let guard = policy.post_spawn_apply(1234);
        assert!(
            guard.is_none(),
            "non-WindowsJobObject provider should return None"
        );
    }

    /// post_spawn_apply with WindowsJobObject and an invalid PID:
    /// On Windows returns None (OpenProcess fails).
    /// On non-Windows returns None (assign_process is no-op but the guard
    /// IS returned for a valid-looking PID — so use an impossible PID).
    #[test]
    fn post_spawn_apply_windows_job_object_invalid_pid() {
        let policy = SandboxPolicy {
            enabled: true,
            provider: SandboxProvider::WindowsJobObject,
            allow_read: Vec::new(),
            allow_write: Vec::new(),
            allow_network: Vec::new(),
        };
        // PID 0 / 0xFFFFFFFF are never valid on any platform.
        let guard = policy.post_spawn_apply(0);
        // On Windows: assign_process(0) fails → returns None.
        // On non-Windows: stub assign_process always Ok → Some(guard) is returned.
        // The key assertion: no panic.
        let _ = guard;
    }
}
