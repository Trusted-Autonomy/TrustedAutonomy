// sandbox.rs — Agent process sandboxing (v0.14.0).
//
// Wraps a SpawnRequest to apply OS-level sandboxing before the agent process
// starts.  On macOS this uses `sandbox-exec` with a generated `.sb` profile;
// on Linux it uses `bwrap` (bubblewrap) when available.
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

#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

use crate::adapter::SpawnRequest;

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
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            SandboxProvider::None
        }
    }

    /// Apply this policy to a SpawnRequest, wrapping it in the sandbox.
    ///
    /// If disabled or provider is None, returns the request unchanged.
    pub fn apply(&self, request: SpawnRequest) -> SpawnRequest {
        if !self.enabled || self.provider == SandboxProvider::None {
            return request;
        }

        match self.provider {
            SandboxProvider::MacosSandboxExec => self.apply_macos(request),
            SandboxProvider::LinuxBwrap => self.apply_linux_bwrap(request),
            SandboxProvider::None => request,
        }
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
}
