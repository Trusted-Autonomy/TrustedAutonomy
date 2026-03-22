// bare_process.rs — BareProcessRuntime: spawn agents as child OS processes.
//
// This is the default runtime — the same behavior TA has always had, but now
// expressed through the RuntimeAdapter trait so the rest of the code doesn't
// care how agents are actually launched.
//
// Credentials are injected as environment variables at spawn time.  There is
// no post-spawn credential injection for bare processes because the OS process
// environment is immutable after spawn.  If a credential needs to be scoped to
// a subset of operations, the policy layer enforces that; the agent simply sees
// an env var.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use tracing::debug;

use crate::adapter::{
    AgentHandle, Result, RuntimeAdapter, RuntimeError, RuntimeStatus, SpawnRequest, StdinMode,
    StdoutMode, TransportInfo,
};
use crate::credential::ScopedCredential;

// ── BareProcessHandle ────────────────────────────────────────────────────────

/// Handle to an agent running as a bare OS child process.
pub struct BareProcessHandle {
    child: Child,
    #[allow(dead_code)]
    working_dir: PathBuf,
}

impl BareProcessHandle {
    fn new(child: Child, working_dir: PathBuf) -> Self {
        Self { child, working_dir }
    }
}

impl AgentHandle for BareProcessHandle {
    fn pid(&self) -> Option<u32> {
        Some(self.child.id())
    }

    fn status(&mut self) -> Result<RuntimeStatus> {
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(RuntimeStatus::Exited {
                exit_code: status.code(),
            }),
            Ok(None) => Ok(RuntimeStatus::Running),
            Err(e) => Err(RuntimeError::StatusCheckFailed(e.to_string())),
        }
    }

    fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().map_err(RuntimeError::Io)
    }

    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    fn transport_info(&self) -> TransportInfo {
        // BareProcess agents connect to the TA gateway via stdio (the existing
        // .mcp.json stdio transport config).
        TransportInfo::Stdio
    }

    fn stop(&mut self) -> Result<()> {
        // On Unix: SIGTERM first, then SIGKILL.
        // On Windows: TerminateProcess.
        #[cfg(unix)]
        {
            // Send SIGTERM to request graceful shutdown.
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            // Give the process up to 5 seconds to exit cleanly.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => return Ok(()),
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    _ => break,
                }
            }
            // Force kill after timeout.
            self.child.kill().map_err(RuntimeError::Io)
        }
        #[cfg(not(unix))]
        {
            self.child.kill().map_err(RuntimeError::Io)
        }
    }
}

// ── BareProcessRuntime ────────────────────────────────────────────────────────

/// RuntimeAdapter that spawns agents as bare OS child processes.
///
/// This is the default runtime used when no `runtime` field is set in the
/// agent YAML config (or when `runtime = "process"` is explicitly set).
///
/// No container or VM isolation is applied.  The agent runs as the same user
/// as TA in the same network namespace.
pub struct BareProcessRuntime;

impl BareProcessRuntime {
    pub fn new() -> Self {
        BareProcessRuntime
    }
}

impl Default for BareProcessRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeAdapter for BareProcessRuntime {
    fn name(&self) -> &str {
        "process"
    }

    fn spawn(&self, request: SpawnRequest) -> Result<Box<dyn AgentHandle>> {
        debug!(
            command = %request.command,
            args = ?request.args,
            working_dir = %request.working_dir.display(),
            "BareProcessRuntime: spawning agent"
        );

        let mut cmd = Command::new(&request.command);
        cmd.current_dir(&request.working_dir);

        for arg in &request.args {
            cmd.arg(arg);
        }

        for (key, value) in &request.env {
            cmd.env(key, value);
        }

        match request.stdin_mode {
            StdinMode::Null => {
                cmd.stdin(Stdio::null());
            }
            StdinMode::Inherited => {}
            StdinMode::Piped => {
                cmd.stdin(Stdio::piped());
            }
        }

        match request.stdout_mode {
            StdoutMode::Inherited => {}
            StdoutMode::Piped => {
                cmd.stdout(Stdio::piped());
            }
        }

        let child = cmd
            .spawn()
            .map_err(|e| RuntimeError::SpawnFailed(format!("{}: {}", request.command, e)))?;

        Ok(Box::new(BareProcessHandle::new(child, request.working_dir)))
    }

    fn inject_credentials(
        &self,
        _handle: &mut dyn AgentHandle,
        _creds: &[ScopedCredential],
    ) -> Result<()> {
        // BareProcess credentials are injected as env vars at spawn time.
        // Post-spawn injection is not possible for OS processes.
        // This is a no-op — callers should pass credentials in SpawnRequest.env.
        Ok(())
    }
}

// ── Helper: build env map with credentials ───────────────────────────────────

/// Merge scoped credentials into an existing env map.
///
/// Each credential becomes an environment variable: `cred.name = cred.value`.
/// Call this before building a `SpawnRequest` to include credentials.
pub fn apply_credentials_to_env(env: &mut HashMap<String, String>, creds: &[ScopedCredential]) {
    for cred in creds {
        env.insert(cred.name.clone(), cred.value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn bare_process_runtime_name() {
        assert_eq!(BareProcessRuntime::new().name(), "process");
    }

    #[test]
    fn spawn_simple_command() {
        let rt = BareProcessRuntime::new();
        let req = SpawnRequest {
            command: "true".into(),
            args: vec![],
            env: HashMap::new(),
            working_dir: std::env::temp_dir(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        let status = handle.wait().expect("wait should succeed");
        assert!(status.success());
    }

    #[test]
    fn spawn_with_env() {
        let rt = BareProcessRuntime::new();
        let mut env = HashMap::new();
        env.insert("TEST_VAR".into(), "hello_runtime".into());

        // Use 'env' command to check the variable is set.
        let req = SpawnRequest {
            command: "sh".into(),
            args: vec!["-c".into(), "test \"$TEST_VAR\" = hello_runtime".into()],
            env,
            working_dir: std::env::temp_dir(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        let status = handle.wait().expect("wait should succeed");
        assert!(status.success(), "TEST_VAR should be visible to the child");
    }

    #[test]
    fn spawn_piped_stdout() {
        use std::io::Read;

        let rt = BareProcessRuntime::new();
        let req = SpawnRequest {
            command: "echo".into(),
            args: vec!["hello_piped".into()],
            env: HashMap::new(),
            working_dir: std::env::temp_dir(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Piped,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        let mut output = String::new();
        if let Some(mut stdout) = handle.take_stdout() {
            stdout.read_to_string(&mut output).expect("read stdout");
        }
        handle.wait().expect("wait should succeed");
        assert_eq!(output.trim(), "hello_piped");
    }

    #[test]
    fn transport_info_is_stdio() {
        let rt = BareProcessRuntime::new();
        let req = SpawnRequest {
            command: "true".into(),
            args: vec![],
            env: HashMap::new(),
            working_dir: std::env::temp_dir(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        assert_eq!(handle.transport_info(), TransportInfo::Stdio);
        let _ = handle.wait();
    }

    #[test]
    fn status_running_then_exited() {
        let rt = BareProcessRuntime::new();
        // Use 'sleep 0' so the process exits quickly.
        let req = SpawnRequest {
            command: "sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            env: HashMap::new(),
            working_dir: std::env::temp_dir(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        // Wait for the child, then check status.
        let _ = handle.wait();
        let status = handle.status().expect("status should succeed");
        assert!(matches!(
            status,
            RuntimeStatus::Exited { exit_code: Some(0) }
        ));
    }

    #[test]
    fn apply_credentials_to_env_merges() {
        let mut env = HashMap::new();
        env.insert("EXISTING".into(), "yes".into());

        let creds = vec![
            ScopedCredential::new("API_KEY", "secret-key"),
            ScopedCredential::with_scopes("GITHUB", "ghp_token", vec!["repo.read".into()]),
        ];
        apply_credentials_to_env(&mut env, &creds);

        assert_eq!(env.get("EXISTING"), Some(&"yes".to_string()));
        assert_eq!(env.get("API_KEY"), Some(&"secret-key".to_string()));
        assert_eq!(env.get("GITHUB"), Some(&"ghp_token".to_string()));
    }

    #[test]
    fn inject_credentials_is_noop_for_bare_process() {
        let rt = BareProcessRuntime::new();
        let req = SpawnRequest {
            command: "true".into(),
            args: vec![],
            env: HashMap::new(),
            working_dir: std::env::temp_dir(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        let creds = vec![ScopedCredential::new("K", "v")];
        rt.inject_credentials(handle.as_mut(), &creds)
            .expect("inject_credentials should succeed");
        let _ = handle.wait();
    }
}
