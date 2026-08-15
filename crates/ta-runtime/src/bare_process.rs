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

        let mut cmd = build_command(&request.command, &request.args);
        cmd.current_dir(&request.working_dir);

        // Scope-narrowed spawns (v0.17.6.1) start from nothing rather than
        // the full parent environment — `request.env` is the complete,
        // already-narrowed environment for this child, not an addition to
        // whatever this process happened to inherit.
        if request.clear_env {
            cmd.env_clear();
        }

        // Strip dynamic-linker preload variables before merging any env overrides.
        // These can be injected into the parent process environment by a PATH-masquerade
        // attack and would be inherited by the agent subprocess, allowing arbitrary
        // code injection. Removing them eliminates the injection surface entirely.
        // (v0.17.0.9 — binary masquerade hardening)
        cmd.env_remove("LD_PRELOAD");
        cmd.env_remove("LD_LIBRARY_PATH");
        cmd.env_remove("DYLD_INSERT_LIBRARIES");
        cmd.env_remove("DYLD_LIBRARY_PATH");

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

// ── Helper: build a Command, handling Windows .cmd/.bat wrappers ─────────────

/// Build a `std::process::Command` for the given command and args.
///
/// On Windows, tools installed via npm (e.g., Claude Code, npx) are `.cmd`
/// batch-file wrappers.  `Command::new("claude")` only finds `claude.exe`,
/// not `claude.cmd`, so spawn fails with NotFound even when the tool is
/// on `PATH`.  We resolve via `which::which()` (which respects `PATHEXT`)
/// and wrap `.cmd`/`.bat` files in `cmd.exe /c` so they execute correctly.
///
/// On non-Windows, or when `which` doesn't find the command, falls back to
/// `Command::new(command)` (original behaviour — unchanged).
fn build_command(command: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        if let Ok(resolved) = which::which(command) {
            let ext = resolved
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "cmd" || ext == "bat" {
                tracing::debug!(
                    command = command,
                    resolved = %resolved.display(),
                    "Wrapping .cmd/.bat in cmd.exe /c"
                );
                let mut cmd = Command::new("cmd");
                cmd.arg("/c").arg(resolved);
                for arg in args {
                    cmd.arg(arg);
                }
                return cmd;
            }
        }
    }
    let mut cmd = Command::new(command);
    for arg in args {
        cmd.arg(arg);
    }
    cmd
}

// ── Helper: build env map with credentials ───────────────────────────────────

/// Merge scoped credentials into an existing env map, enforcing declared
/// scopes (v0.17.6.1).
///
/// A credential is only injected if it is eligible for `required_scopes`:
/// - A credential with **no** declared scopes (`cred.scopes.is_empty()`) is
///   unrestricted — per `ScopedCredential`'s own contract ("no scope
///   restrictions") it is always injected.
/// - A credential with declared scopes is injected only if at least one of
///   them appears in `required_scopes`. A credential whose scopes don't
///   intersect `required_scopes` at all is silently omitted — the caller
///   never sees it, not even as an unusable placeholder.
///
/// For an eligible credential, what actually lands in `env` depends on
/// `cred.broker_mediated` (v0.17.6.3, PLAN item 5):
/// - `false` (default): the **reduced-security fallback** — `cred.value`
///   (the raw secret) is injected directly as `cred.name = cred.value`,
///   exactly as before v0.17.6.3. This path is explicitly flagged via a
///   `tracing::warn!` so it shows up in normal operation, not just an audit
///   grep — it remains the only delivery path until a per-tool credential
///   shim (v0.17.6.7) or an explicit `ConnectorRegistry` opt-in closes it
///   for a given connector.
/// - `true`: the raw secret is withheld entirely. Instead
///   `TA_SESSION_TOKEN_<cred.name> = cred.session_token_id` is injected —
///   an opaque reference the agent can present to `ta_external_action`,
///   which the gateway broker independently validates and resolves the
///   real secret for server-side. A `broker_mediated` credential with no
///   `session_token_id` is a caller bug (nothing to hand the agent) and is
///   skipped with a `tracing::warn!` rather than silently leaking nothing
///   useful or crashing the spawn.
///
/// Call this before building a `SpawnRequest` to include credentials.
pub fn apply_credentials_to_env(
    env: &mut HashMap<String, String>,
    creds: &[ScopedCredential],
    required_scopes: &[String],
) {
    for cred in creds {
        let in_scope =
            cred.scopes.is_empty() || cred.scopes.iter().any(|s| required_scopes.contains(s));
        if !in_scope {
            continue;
        }
        if cred.broker_mediated {
            match &cred.session_token_id {
                Some(token_id) => {
                    let key = format!("TA_SESSION_TOKEN_{}", cred.name);
                    // v0.17.6.5: the expiry rides alongside the token as a
                    // plain, unenforced hint -- a downstream process that
                    // inherits this credential (e.g. attenuating it further
                    // for its own swarm sub-goal) uses it only to estimate
                    // `parent_remaining_ttl`. The real bound stays whatever
                    // check is cryptographically embedded in the token
                    // itself, so a tampered/missing hint here can only make
                    // that estimate pessimistic, never unsafe.
                    if let Some(expires_at) = cred.session_token_expires_at {
                        env.insert(format!("{key}_EXPIRES_AT"), expires_at.to_rfc3339());
                    }
                    env.insert(key, token_id.clone());
                }
                None => {
                    tracing::warn!(
                        credential = %cred.name,
                        "credential is broker_mediated but carries no session_token_id; \
                         withheld from agent env entirely (neither raw secret nor token)"
                    );
                }
            }
        } else {
            // debug, not warn: this is still the default path for every
            // credential that has no `.ta/connectors.toml` entry opting it
            // into broker mediation, so warning here would fire on every
            // spawn for the common case rather than flagging something
            // unusual. It is still explicitly flagged (a distinct,
            // greppable message), just at a level that doesn't page anyone
            // for expected behavior — `ta doctor` is the surfaced,
            // human-facing flag for "N credentials use the reduced-security
            // fallback" (see docs/USAGE.md).
            tracing::debug!(
                credential = %cred.name,
                "injecting raw secret directly into agent environment: reduced-security \
                 fallback for a non-broker-mediated connector (see ConnectorRegistry / \
                 .ta/connectors.toml to migrate this credential to broker mediation)"
            );
            env.insert(cred.name.clone(), cred.value.clone());
        }
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
            clear_env: false,
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
            clear_env: false,
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
            clear_env: false,
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
            clear_env: false,
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
            clear_env: false,
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
    fn apply_credentials_to_env_merges_unscoped_credentials() {
        let mut env = HashMap::new();
        env.insert("EXISTING".into(), "yes".into());

        // Unscoped credentials ("no scope restrictions") are always injected,
        // regardless of what's required.
        let creds = vec![ScopedCredential::new("API_KEY", "secret-key")];
        apply_credentials_to_env(&mut env, &creds, &[]);

        assert_eq!(env.get("EXISTING"), Some(&"yes".to_string()));
        assert_eq!(env.get("API_KEY"), Some(&"secret-key".to_string()));
    }

    #[test]
    fn apply_credentials_to_env_includes_credential_with_matching_scope() {
        let mut env = HashMap::new();
        let creds = vec![ScopedCredential::with_scopes(
            "GITHUB",
            "ghp_token",
            vec!["repo.read".into()],
        )];
        apply_credentials_to_env(&mut env, &creds, &["repo.read".into()]);

        assert_eq!(env.get("GITHUB"), Some(&"ghp_token".to_string()));
    }

    #[test]
    fn apply_credentials_to_env_excludes_credential_out_of_required_scope() {
        let mut env = HashMap::new();
        let creds = vec![ScopedCredential::with_scopes(
            "GITHUB",
            "ghp_token",
            vec!["repo.write".into()],
        )];
        // Required scope doesn't overlap the credential's declared scope —
        // it must be entirely absent from the resulting env, not merely
        // present-but-empty.
        apply_credentials_to_env(&mut env, &creds, &["repo.read".into()]);

        assert!(!env.contains_key("GITHUB"));
    }

    #[test]
    fn apply_credentials_to_env_excludes_scoped_credential_when_no_scopes_required() {
        let mut env = HashMap::new();
        let creds = vec![ScopedCredential::with_scopes(
            "GITHUB",
            "ghp_token",
            vec!["repo.read".into()],
        )];
        apply_credentials_to_env(&mut env, &creds, &[]);

        assert!(!env.contains_key("GITHUB"));
    }

    #[test]
    fn apply_credentials_to_env_includes_credential_matching_any_of_several_scopes() {
        let mut env = HashMap::new();
        let creds = vec![ScopedCredential::with_scopes(
            "GITHUB",
            "ghp_token",
            vec!["repo.read".into(), "issues.write".into()],
        )];
        apply_credentials_to_env(
            &mut env,
            &creds,
            &["issues.write".into(), "ci.trigger".into()],
        );

        assert_eq!(env.get("GITHUB"), Some(&"ghp_token".to_string()));
    }

    #[test]
    fn apply_credentials_to_env_withholds_raw_secret_for_broker_mediated_credential() {
        let mut env = HashMap::new();
        let creds = vec![ScopedCredential::new("GITHUB_TOKEN", "ghp_real_secret")
            .with_broker_mediation("22222222-2222-2222-2222-222222222222")];
        apply_credentials_to_env(&mut env, &creds, &[]);

        assert!(
            !env.values().any(|v| v == "ghp_real_secret"),
            "the raw secret must never reach the agent env for a broker-mediated credential"
        );
        assert!(
            !env.contains_key("GITHUB_TOKEN"),
            "broker-mediated credentials are not injected under their own name"
        );
        assert_eq!(
            env.get("TA_SESSION_TOKEN_GITHUB_TOKEN"),
            Some(&"22222222-2222-2222-2222-222222222222".to_string())
        );
    }

    #[test]
    fn apply_credentials_to_env_broker_mediated_without_token_id_injects_nothing() {
        let mut env = HashMap::new();
        // Constructed directly (not via `with_broker_mediation`) to model a
        // caller bug: `broker_mediated = true` but no token was ever minted.
        let cred = ScopedCredential::new("GITHUB_TOKEN", "ghp_real_secret");
        let mut cred = cred;
        cred.broker_mediated = true;
        apply_credentials_to_env(&mut env, &[cred], &[]);

        assert!(
            env.is_empty(),
            "must withhold entirely, not fall back to the raw secret"
        );
    }

    #[test]
    fn spawn_with_clear_env_does_not_inherit_parent_environment() {
        let rt = BareProcessRuntime::new();
        let mut env = HashMap::new();
        env.insert("SCOPED_ONLY".into(), "present".into());

        // A var that's set in *this test process* but must NOT leak through
        // when clear_env is set — only what's explicitly in `env` survives.
        std::env::set_var("TA_TEST_LEAK_CANARY", "should-not-be-inherited");

        let req = SpawnRequest {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "test \"$SCOPED_ONLY\" = present && [ -z \"$TA_TEST_LEAK_CANARY\" ]".into(),
            ],
            env,
            working_dir: std::env::temp_dir(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
            clear_env: true,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        let status = handle.wait().expect("wait should succeed");
        std::env::remove_var("TA_TEST_LEAK_CANARY");

        assert!(
            status.success(),
            "clear_env spawn must see only the explicit env, not the parent's"
        );
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
            clear_env: false,
        };
        let mut handle = rt.spawn(req).expect("spawn should succeed");
        let creds = vec![ScopedCredential::new("K", "v")];
        rt.inject_credentials(handle.as_mut(), &creds)
            .expect("inject_credentials should succeed");
        let _ = handle.wait();
    }
}
