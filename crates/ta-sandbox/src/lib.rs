//! # ta-sandbox
//!
//! Allowlisted command execution for Trusted Autonomy.
//!
//! Provides a controlled execution environment where agents can run
//! pre-approved commands (search, format, test) without access to a
//! real shell or host filesystem outside the staging workspace.
//!
//! ## Architecture
//!
//! The sandbox has three enforcement layers:
//! 1. **Command allowlist**: Only pre-approved binaries can execute
//! 2. **CWD enforcement**: All execution is confined to the staging workspace
//! 3. **Network policy**: Per-domain allow/deny for outbound connections
//!
//! ## Usage
//!
//! ```rust,no_run
//! use ta_sandbox::{SandboxConfig, SandboxRunner, CommandPolicy};
//!
//! let config = SandboxConfig::default();
//! let mut runner = SandboxRunner::new(config, "/path/to/workspace");
//!
//! // Execute an allowed command
//! let result = runner.execute("rg", &["TODO", "src/"]).unwrap();
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use ta_policy::AccessFilter;

/// Sandbox configuration defining what commands are permitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Allowed commands and their policies.
    pub commands: HashMap<String, CommandPolicy>,
    /// Explicitly denied commands. Deny takes precedence over `commands` allowlist.
    /// Supports glob patterns (e.g., "rm", "curl*").
    #[serde(default)]
    pub denied_commands: Vec<String>,
    /// Network access policy.
    pub network: NetworkPolicy,
    /// Maximum execution time per command (seconds).
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Whether to hash command transcripts into the audit log.
    #[serde(default = "default_true")]
    pub audit_transcripts: bool,
}

fn default_timeout() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

/// Policy for a single allowed command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandPolicy {
    /// Human-readable description of why this command is allowed.
    pub description: String,
    /// Allowed argument patterns (glob-style). Empty = any args allowed.
    #[serde(default)]
    pub allowed_args: Vec<String>,
    /// Forbidden argument patterns (checked first, overrides allowed).
    #[serde(default)]
    pub forbidden_args: Vec<String>,
    /// Maximum number of invocations per session.
    #[serde(default)]
    pub max_invocations: Option<u32>,
    /// Whether the command can write to the filesystem.
    #[serde(default)]
    pub can_write: bool,
}

/// Network access policy for sandboxed commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Default action for domains not in the allow/deny lists.
    #[serde(default)]
    pub default_action: NetworkAction,
    /// Domains explicitly allowed (e.g., "crates.io", "github.com").
    #[serde(default)]
    pub allow_domains: Vec<String>,
    /// Domains explicitly denied.
    #[serde(default)]
    pub deny_domains: Vec<String>,
}

/// Default network action.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkAction {
    Allow,
    #[default]
    Deny,
}

/// Result of a sandboxed command execution.
#[derive(Debug, Clone)]
pub struct SandboxResult {
    /// The command that was executed.
    pub command: String,
    /// Arguments passed to the command.
    pub args: Vec<String>,
    /// Process exit code.
    pub exit_code: Option<i32>,
    /// Standard output (captured).
    pub stdout: Vec<u8>,
    /// Standard error (captured).
    pub stderr: Vec<u8>,
    /// Execution duration.
    pub duration: Duration,
    /// SHA-256 hash of the command transcript (command + args + stdout + stderr).
    pub transcript_hash: String,
    /// Timestamp of execution.
    pub executed_at: SystemTime,
}

/// Errors from sandbox operations.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("Command '{0}' is not in the allowlist")]
    CommandNotAllowed(String),

    #[error("Argument '{arg}' is forbidden for command '{command}'")]
    ForbiddenArgument { command: String, arg: String },

    #[error("Path '{path}' is outside the workspace root '{workspace}'")]
    PathEscape { path: String, workspace: String },

    #[error("Command '{0}' exceeded invocation limit ({1})")]
    InvocationLimitExceeded(String, u32),

    #[error("Command timed out after {0}s")]
    Timeout(u64),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// The sandbox runner — enforces command allowlisting, CWD confinement,
/// and audit transcript capture.
pub struct SandboxRunner {
    config: SandboxConfig,
    workspace_root: PathBuf,
    invocation_counts: HashMap<String, u32>,
    transcripts: Vec<SandboxResult>,
}

impl SandboxRunner {
    /// Create a new sandbox runner bound to a workspace directory.
    pub fn new(config: SandboxConfig, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            config,
            workspace_root: workspace_root.into(),
            invocation_counts: HashMap::new(),
            transcripts: Vec::new(),
        }
    }

    /// Execute a command within the sandbox.
    ///
    /// Checks the allowlist, validates arguments, enforces CWD, captures output,
    /// and hashes the transcript.
    pub fn execute(&mut self, command: &str, args: &[&str]) -> Result<SandboxResult, SandboxError> {
        // 0. Check denied commands first (deny takes precedence over allowlist).
        if !self.config.denied_commands.is_empty() {
            let filter = AccessFilter::new(vec![], self.config.denied_commands.clone());
            if !filter.permits(command) {
                return Err(SandboxError::CommandNotAllowed(format!(
                    "{} (explicitly denied)",
                    command
                )));
            }
        }

        // 1. Check allowlist.
        let policy = self
            .config
            .commands
            .get(command)
            .ok_or_else(|| SandboxError::CommandNotAllowed(command.to_string()))?;

        // 2. Check invocation limit.
        if let Some(max) = policy.max_invocations {
            let count = self
                .invocation_counts
                .entry(command.to_string())
                .or_insert(0);
            if *count >= max {
                return Err(SandboxError::InvocationLimitExceeded(
                    command.to_string(),
                    max,
                ));
            }
        }

        // 3. Validate arguments — check forbidden patterns.
        for arg in args {
            for forbidden in &policy.forbidden_args {
                if glob_match(forbidden, arg) {
                    return Err(SandboxError::ForbiddenArgument {
                        command: command.to_string(),
                        arg: arg.to_string(),
                    });
                }
            }
        }

        // 4. Check for path escapes — any arg that looks like a path
        //    must resolve within the workspace root.
        for arg in args {
            if arg.contains('/') || arg.contains('\\') {
                self.validate_path(arg)?;
            }
        }

        // 5. Execute the command.
        let start = std::time::Instant::now();
        let output = Command::new(command)
            .args(args)
            .current_dir(&self.workspace_root)
            .output()?;

        let duration = start.elapsed();

        // 6. Check timeout.
        if duration.as_secs() > self.config.timeout_secs {
            return Err(SandboxError::Timeout(self.config.timeout_secs));
        }

        // 7. Build transcript hash.
        let transcript_hash = self.hash_transcript(command, args, &output);

        // 8. Update invocation count.
        *self
            .invocation_counts
            .entry(command.to_string())
            .or_insert(0) += 1;

        let result = SandboxResult {
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
            duration,
            transcript_hash,
            executed_at: SystemTime::now(),
        };

        // 9. Store transcript for audit.
        if self.config.audit_transcripts {
            self.transcripts.push(result.clone());
        }

        Ok(result)
    }

    /// Validate that a path argument doesn't escape the workspace.
    fn validate_path(&self, path_str: &str) -> Result<(), SandboxError> {
        // Fast reject: any path containing ".." is suspicious. Normalize and check.
        // This catches ../../etc/passwd even when the intermediate dirs don't exist.
        let normalized = normalize_path_components(path_str);
        if normalized.starts_with("..")
            || normalized.contains("/../")
            || normalized.contains("\\..\\")
        {
            return Err(SandboxError::PathEscape {
                path: path_str.to_string(),
                workspace: self.workspace_root.display().to_string(),
            });
        }

        let path = self.workspace_root.join(path_str);

        // If the path or its parent exists, canonicalize for symlink resolution.
        let resolved = if path.exists() {
            path.canonicalize().unwrap_or(path)
        } else if let Some(parent) = path.parent() {
            if parent.exists() {
                let canonical_parent = parent.canonicalize().unwrap_or(parent.to_path_buf());
                canonical_parent.join(path.file_name().unwrap_or_default())
            } else {
                // Parent doesn't exist — use the workspace root + normalized path.
                self.workspace_root.join(&normalized)
            }
        } else {
            path
        };

        let workspace_canonical = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());

        if !resolved.starts_with(&workspace_canonical) {
            return Err(SandboxError::PathEscape {
                path: path_str.to_string(),
                workspace: self.workspace_root.display().to_string(),
            });
        }

        Ok(())
    }

    /// Hash the command transcript (command + args + stdout + stderr) using SHA-256.
    fn hash_transcript(&self, command: &str, args: &[&str], output: &Output) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(command.as_bytes());
        for arg in args {
            hasher.update(b"\0");
            hasher.update(arg.as_bytes());
        }
        hasher.update(b"\n--- stdout ---\n");
        hasher.update(&output.stdout);
        hasher.update(b"\n--- stderr ---\n");
        hasher.update(&output.stderr);

        format!("{:x}", hasher.finalize())
    }

    /// Get all captured transcripts (for audit logging).
    pub fn transcripts(&self) -> &[SandboxResult] {
        &self.transcripts
    }

    /// Get the workspace root this runner is bound to.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Check if a command is permitted (in allowlist and not denied).
    pub fn is_allowed(&self, command: &str) -> bool {
        // Check denied first.
        if !self.config.denied_commands.is_empty() {
            let filter = AccessFilter::new(vec![], self.config.denied_commands.clone());
            if !filter.permits(command) {
                return false;
            }
        }
        self.config.commands.contains_key(command)
    }

    /// Check if a domain is allowed by the network policy.
    ///
    /// Uses domain-specific matching (supports `*.github.com` subdomain wildcards)
    /// rather than generic glob patterns. The semantics follow AccessFilter's model:
    /// deny takes precedence, then allow, then default action.
    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        // Check deny list first (deny takes priority).
        for denied in &self.config.network.deny_domains {
            if domain_match(denied, domain) {
                return false;
            }
        }

        // Check allow list.
        for allowed in &self.config.network.allow_domains {
            if domain_match(allowed, domain) {
                return true;
            }
        }

        // Fall back to default action.
        self.config.network.default_action == NetworkAction::Allow
    }
}

impl Default for SandboxConfig {
    /// Default sandbox config with common developer tools allowed.
    fn default() -> Self {
        let mut commands = HashMap::new();

        commands.insert(
            "rg".to_string(),
            CommandPolicy {
                description: "ripgrep — fast code search".to_string(),
                allowed_args: vec![],
                forbidden_args: vec![],
                max_invocations: None,
                can_write: false,
            },
        );

        commands.insert(
            "grep".to_string(),
            CommandPolicy {
                description: "Text search".to_string(),
                allowed_args: vec![],
                forbidden_args: vec![],
                max_invocations: None,
                can_write: false,
            },
        );

        commands.insert(
            "find".to_string(),
            CommandPolicy {
                description: "File search".to_string(),
                allowed_args: vec![],
                forbidden_args: vec!["-exec".to_string(), "-delete".to_string()],
                max_invocations: None,
                can_write: false,
            },
        );

        commands.insert(
            "cat".to_string(),
            CommandPolicy {
                description: "Read file contents".to_string(),
                allowed_args: vec![],
                forbidden_args: vec![],
                max_invocations: None,
                can_write: false,
            },
        );

        commands.insert(
            "cargo".to_string(),
            CommandPolicy {
                description: "Rust build tool — test, clippy, fmt, build".to_string(),
                allowed_args: vec![],
                forbidden_args: vec!["publish".to_string()],
                max_invocations: None,
                can_write: true,
            },
        );

        commands.insert(
            "npm".to_string(),
            CommandPolicy {
                description: "Node package manager — test, lint, build".to_string(),
                allowed_args: vec![],
                forbidden_args: vec!["publish".to_string()],
                max_invocations: None,
                can_write: true,
            },
        );

        commands.insert(
            "git".to_string(),
            CommandPolicy {
                description: "Version control — status, diff, log".to_string(),
                allowed_args: vec![],
                forbidden_args: vec![
                    "push".to_string(),
                    "remote".to_string(),
                    "force".to_string(),
                ],
                max_invocations: None,
                can_write: false,
            },
        );

        commands.insert(
            "jq".to_string(),
            CommandPolicy {
                description: "JSON processor".to_string(),
                allowed_args: vec![],
                forbidden_args: vec![],
                max_invocations: None,
                can_write: false,
            },
        );

        Self {
            commands,
            denied_commands: vec![],
            network: NetworkPolicy {
                default_action: NetworkAction::Deny,
                allow_domains: vec![
                    "crates.io".to_string(),
                    "registry.npmjs.org".to_string(),
                    "github.com".to_string(),
                ],
                deny_domains: vec![],
            },
            timeout_secs: 300,
            audit_transcripts: true,
        }
    }
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            default_action: NetworkAction::Deny,
            allow_domains: vec![],
            deny_domains: vec![],
        }
    }
}

/// Normalize path components by resolving `.` and `..` lexically (without filesystem access).
/// Returns the normalized relative path as a string.
fn normalize_path_components(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                if parts.is_empty() || parts.last() == Some(&"..") {
                    parts.push("..");
                } else {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Simple glob matching: supports '*' as wildcard.
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if let Some(suffix) = pattern.strip_prefix('*') {
        return text.ends_with(suffix);
    }

    if let Some(prefix) = pattern.strip_suffix('*') {
        return text.starts_with(prefix);
    }

    pattern == text
}

/// Domain matching: supports wildcard subdomains (e.g., "*.github.com").
fn domain_match(pattern: &str, domain: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return domain == suffix || domain.ends_with(&format!(".{}", suffix));
    }

    pattern == domain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_common_tools() {
        let config = SandboxConfig::default();
        assert!(config.commands.contains_key("rg"));
        assert!(config.commands.contains_key("cargo"));
        assert!(config.commands.contains_key("git"));
        assert!(config.commands.contains_key("jq"));
    }

    #[test]
    fn command_not_allowed() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let mut runner = SandboxRunner::new(config, dir.path());

        let result = runner.execute("rm", &["-rf", "/"]);
        assert!(matches!(result, Err(SandboxError::CommandNotAllowed(_))));
    }

    #[test]
    fn forbidden_args_rejected() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let mut runner = SandboxRunner::new(config, dir.path());

        let result = runner.execute("find", &[".", "-delete"]);
        assert!(matches!(
            result,
            Err(SandboxError::ForbiddenArgument { .. })
        ));
    }

    #[test]
    fn path_escape_detected() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let mut runner = SandboxRunner::new(config, dir.path());

        let result = runner.execute("cat", &["../../etc/passwd"]);
        assert!(matches!(result, Err(SandboxError::PathEscape { .. })));
    }

    #[test]
    fn allowed_command_executes() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();

        // Create a file to search.
        std::fs::write(dir.path().join("test.txt"), "hello world").unwrap();

        let mut runner = SandboxRunner::new(config, dir.path());
        let result = runner.execute("cat", &["test.txt"]);
        assert!(result.is_ok());

        let result = result.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(String::from_utf8_lossy(&result.stdout).contains("hello world"));
        assert!(!result.transcript_hash.is_empty());
    }

    #[test]
    fn invocation_limit_enforced() {
        let mut config = SandboxConfig::default();
        config.commands.insert(
            "echo".to_string(),
            CommandPolicy {
                description: "test".to_string(),
                allowed_args: vec![],
                forbidden_args: vec![],
                max_invocations: Some(2),
                can_write: false,
            },
        );

        let dir = tempfile::tempdir().unwrap();
        let mut runner = SandboxRunner::new(config, dir.path());

        assert!(runner.execute("echo", &["1"]).is_ok());
        assert!(runner.execute("echo", &["2"]).is_ok());
        assert!(matches!(
            runner.execute("echo", &["3"]),
            Err(SandboxError::InvocationLimitExceeded(_, 2))
        ));
    }

    #[test]
    fn transcript_hash_deterministic() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "data").unwrap();

        let mut runner1 = SandboxRunner::new(config.clone(), dir.path());
        let mut runner2 = SandboxRunner::new(config, dir.path());

        let r1 = runner1.execute("cat", &["f.txt"]).unwrap();
        let r2 = runner2.execute("cat", &["f.txt"]).unwrap();

        assert_eq!(r1.transcript_hash, r2.transcript_hash);
    }

    #[test]
    fn transcripts_captured() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();

        let mut runner = SandboxRunner::new(config, dir.path());
        runner.execute("cat", &["a.txt"]).unwrap();

        assert_eq!(runner.transcripts().len(), 1);
        assert_eq!(runner.transcripts()[0].command, "cat");
    }

    #[test]
    fn network_policy_deny_by_default() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let runner = SandboxRunner::new(config, dir.path());

        assert!(runner.is_domain_allowed("github.com"));
        assert!(runner.is_domain_allowed("crates.io"));
        assert!(!runner.is_domain_allowed("evil.com"));
    }

    #[test]
    fn domain_match_wildcard() {
        assert!(domain_match("*.github.com", "api.github.com"));
        assert!(domain_match("*.github.com", "github.com"));
        assert!(!domain_match("*.github.com", "evil-github.com"));
    }

    #[test]
    fn glob_match_patterns() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("cargo*", "cargo"));
        assert!(glob_match("test", "test"));
        assert!(!glob_match("*.rs", "main.py"));
    }

    #[test]
    fn is_allowed_check() {
        let config = SandboxConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let runner = SandboxRunner::new(config, dir.path());

        assert!(runner.is_allowed("rg"));
        assert!(runner.is_allowed("cargo"));
        assert!(!runner.is_allowed("rm"));
        assert!(!runner.is_allowed("curl"));
    }

    #[test]
    fn denied_commands_take_precedence() {
        // cargo is in the allowlist by default, but deny it explicitly.
        let config = SandboxConfig {
            denied_commands: vec!["cargo".to_string()],
            ..SandboxConfig::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let runner = SandboxRunner::new(config, dir.path());

        assert!(!runner.is_allowed("cargo"));
        assert!(runner.is_allowed("rg")); // still allowed
    }

    #[test]
    fn denied_command_execution_blocked() {
        let config = SandboxConfig {
            denied_commands: vec!["cat".to_string()],
            ..SandboxConfig::default()
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();
        let mut runner = SandboxRunner::new(config, dir.path());

        let result = runner.execute("cat", &["test.txt"]);
        assert!(matches!(result, Err(SandboxError::CommandNotAllowed(_))));
    }
}
