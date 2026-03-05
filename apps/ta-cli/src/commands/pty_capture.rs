// pty_capture.rs — PTY-based agent subprocess wrapper for real-time output capture.
//
// Wraps the agent process in a pseudo-terminal so:
// 1. Agent output streams to the human via a pluggable OutputSink
// 2. TA captures all output for session history/audit
// 3. Human can inject guidance mid-session via stdin interleaving
//
// Output routing is pluggable via the `OutputSink` trait. The default
// `TerminalSink` writes to stdout (CLI behavior). Future adapters (Slack,
// email, Discord) implement OutputSink to route output elsewhere — see
// SessionChannel trait in ta-changeset for the abstract protocol.
//
// This is the core of Phase v0.4.4 (Interactive Session Completion).

use std::io::{self, BufRead, Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

// ── Output routing ──────────────────────────────────────────────

/// Pluggable output sink for PTY session output.
///
/// The PTY reader thread sends all agent output through this trait.
/// Implement this to route output to different mediums:
/// - `TerminalSink` (default): writes to stdout for CLI users
/// - Future: `SlackSink`, `DiscordSink`, `EmailSink`, `WebhookSink`
///
/// This is the CLI-layer equivalent of the `SessionChannel` trait in
/// `ta-changeset::session_channel`. When building a non-terminal adapter,
/// implement both: `OutputSink` for real-time streaming and `ReviewChannel`
/// for structured interactions (approve/reject/discuss).
pub trait OutputSink: Send + Sync {
    /// Called for each chunk of agent output (may be partial lines).
    /// The data is raw bytes from the PTY — typically UTF-8 terminal output.
    fn emit_output(&self, data: &[u8]) -> io::Result<()>;

    /// Called when the agent process exits.
    #[allow(dead_code)]
    fn on_agent_exit(&self, _status: &ExitStatus) -> io::Result<()> {
        Ok(())
    }
}

/// Default OutputSink that writes to stdout (terminal passthrough).
pub struct TerminalSink;

impl OutputSink for TerminalSink {
    fn emit_output(&self, data: &[u8]) -> io::Result<()> {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        out.write_all(data)?;
        out.flush()
    }
}

/// An OutputSink that collects all output in memory (for testing).
#[cfg(test)]
pub struct CollectorSink {
    pub collected: std::sync::Mutex<Vec<u8>>,
}

#[cfg(test)]
impl CollectorSink {
    pub fn new() -> Self {
        Self {
            collected: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn get_output(&self) -> String {
        let data = self.collected.lock().unwrap();
        String::from_utf8_lossy(&data).to_string()
    }
}

#[cfg(test)]
impl OutputSink for CollectorSink {
    fn emit_output(&self, data: &[u8]) -> io::Result<()> {
        self.collected.lock().unwrap().extend_from_slice(data);
        Ok(())
    }
}

/// Events produced by the PTY session for the caller to consume.
#[derive(Debug)]
#[allow(dead_code)]
pub enum PtyEvent {
    /// Agent wrote output (already forwarded to the terminal).
    AgentOutput(Vec<u8>),
    /// Agent process exited.
    AgentExited(ExitStatus),
    /// Error reading from PTY.
    ReadError(io::Error),
}

/// A running PTY session wrapping an agent subprocess.
pub struct PtySession {
    /// File descriptor for the master side of the PTY.
    master_fd: RawFd,
    /// Owned fd to ensure cleanup.
    _master_owned: OwnedFd,
    /// Child process ID.
    child_pid: libc::pid_t,
    /// Whether the session is still alive.
    alive: Arc<AtomicBool>,
    /// Channel receiving PTY events from the reader thread.
    event_rx: mpsc::Receiver<PtyEvent>,
    /// Handle to the reader thread (joined on drop).
    _reader_handle: Option<thread::JoinHandle<()>>,
}

/// Allocate a PTY pair using libc::openpty.
fn open_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut master: RawFd = 0;
    let mut slave: RawFd = 0;

    // Safety: openpty writes to the provided pointers.
    let ret = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if ret != 0 {
        return Err(io::Error::last_os_error());
    }

    // Safety: fds are valid, just allocated by openpty.
    let master_owned = unsafe { OwnedFd::from_raw_fd(master) };
    let slave_owned = unsafe { OwnedFd::from_raw_fd(slave) };

    Ok((master_owned, slave_owned))
}

impl PtySession {
    /// Spawn an agent process in a PTY.
    ///
    /// The agent's stdin/stdout/stderr are all connected to the slave side
    /// of the PTY. The master side is used for reading output and writing input.
    ///
    /// Output is routed through the provided `OutputSink`. Pass `TerminalSink`
    /// for CLI behavior, or a custom sink for Slack/email/webhook routing.
    #[allow(dead_code)]
    pub fn spawn(
        command: &str,
        args: &[String],
        working_dir: &Path,
        env_vars: &std::collections::HashMap<String, String>,
    ) -> io::Result<Self> {
        Self::spawn_with_sink(command, args, working_dir, env_vars, Arc::new(TerminalSink))
    }

    /// Spawn with a custom output sink for pluggable output routing.
    pub fn spawn_with_sink(
        command: &str,
        args: &[String],
        working_dir: &Path,
        env_vars: &std::collections::HashMap<String, String>,
        output_sink: Arc<dyn OutputSink>,
    ) -> io::Result<Self> {
        let (master_owned, slave_owned) = open_pty()?;
        let master_fd = master_owned.as_raw_fd();
        let slave_fd = slave_owned.as_raw_fd();

        // Fork the process.
        // Safety: fork is safe when we immediately exec in the child.
        let pid = unsafe { libc::fork() };

        if pid < 0 {
            return Err(io::Error::last_os_error());
        }

        if pid == 0 {
            // ── Child process ──
            // Close master side in child.
            drop(master_owned);

            // Create a new session and set controlling terminal.
            unsafe {
                libc::setsid();
                // Linux-musl ioctl() expects c_int for the request parameter,
                // while glibc/macOS use c_ulong. Use platform-conditional cast.
                #[cfg(target_env = "musl")]
                libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_int, 0);
                #[cfg(not(target_env = "musl"))]
                libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0);
            }

            // Redirect stdio to slave PTY.
            unsafe {
                libc::dup2(slave_fd, libc::STDIN_FILENO);
                libc::dup2(slave_fd, libc::STDOUT_FILENO);
                libc::dup2(slave_fd, libc::STDERR_FILENO);
            }

            // Close the original slave fd if it's not one of stdin/stdout/stderr.
            if slave_fd > 2 {
                drop(slave_owned);
            } else {
                // Prevent the OwnedFd from closing a stdio fd.
                std::mem::forget(slave_owned);
            }

            // Change working directory.
            if std::env::set_current_dir(working_dir).is_err() {
                unsafe { libc::_exit(127) };
            }

            // Build the command.
            let c_command = match std::ffi::CString::new(command) {
                Ok(c) => c,
                Err(_) => unsafe { libc::_exit(127) },
            };

            let mut c_args: Vec<std::ffi::CString> = Vec::with_capacity(args.len() + 1);
            c_args.push(c_command.clone());
            for arg in args {
                match std::ffi::CString::new(arg.as_str()) {
                    Ok(c) => c_args.push(c),
                    Err(_) => unsafe { libc::_exit(127) },
                };
            }

            let c_arg_ptrs: Vec<*const libc::c_char> = c_args
                .iter()
                .map(|a| a.as_ptr())
                .chain(std::iter::once(std::ptr::null()))
                .collect();

            // Set environment variables.
            for (key, value) in env_vars {
                std::env::set_var(key, value);
            }

            // Exec — this never returns on success.
            unsafe {
                libc::execvp(c_command.as_ptr(), c_arg_ptrs.as_ptr());
                // If we get here, exec failed.
                libc::_exit(127);
            }
        }

        // ── Parent process ──
        // Close slave side in parent.
        drop(slave_owned);

        let alive = Arc::new(AtomicBool::new(true));
        let (event_tx, event_rx) = mpsc::channel();

        // Spawn reader thread: reads from master, forwards to sink, sends events.
        let reader_alive = alive.clone();
        let reader_master_fd = master_fd;
        let reader_handle = thread::spawn(move || {
            let mut buf = [0u8; 4096];
            // Safety: we're reading from a valid fd.
            let mut master_file = unsafe { std::fs::File::from_raw_fd(reader_master_fd) };

            loop {
                if !reader_alive.load(Ordering::Relaxed) {
                    break;
                }

                match master_file.read(&mut buf) {
                    Ok(0) => break, // EOF — child closed its side.
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        // Route output through the pluggable sink.
                        let _ = output_sink.emit_output(&data);
                        // Send to event channel for capture.
                        let _ = event_tx.send(PtyEvent::AgentOutput(data));
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        let _ = event_tx.send(PtyEvent::ReadError(e));
                        break;
                    }
                }
            }

            // Don't let the File close the fd — master_owned handles that.
            std::mem::forget(master_file);
        });

        Ok(PtySession {
            master_fd,
            _master_owned: master_owned,
            child_pid: pid,
            alive,
            event_rx,
            _reader_handle: Some(reader_handle),
        })
    }

    /// Write data to the agent's stdin (through the PTY master).
    pub fn write_stdin(&self, data: &[u8]) -> io::Result<()> {
        // Safety: master_fd is valid while PtySession is alive.
        let written = unsafe {
            libc::write(
                self.master_fd,
                data.as_ptr() as *const libc::c_void,
                data.len(),
            )
        };
        if written < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Wait for the child process to exit and return its status.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let mut status: libc::c_int = 0;
        // Safety: waiting on our child pid.
        let ret = unsafe { libc::waitpid(self.child_pid, &mut status, 0) };
        self.alive.store(false, Ordering::Relaxed);

        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // Convert libc status to std::process::ExitStatus.
        // Safety: from_raw is available on Unix.
        use std::os::unix::process::ExitStatusExt;
        Ok(ExitStatus::from_raw(status))
    }

    /// Try to receive the next event (non-blocking).
    pub fn try_recv_event(&self) -> Option<PtyEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Receive the next event with a timeout.
    pub fn recv_event_timeout(&self, timeout: Duration) -> Option<PtyEvent> {
        self.event_rx.recv_timeout(timeout).ok()
    }

    /// Check if the session is still alive.
    #[allow(dead_code)]
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Get the child PID.
    pub fn child_pid(&self) -> libc::pid_t {
        self.child_pid
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        // Send SIGHUP to child if still running (standard PTY behavior).
        unsafe {
            libc::kill(self.child_pid, libc::SIGHUP);
        }
    }
}

/// Check if a process with the given PID is still alive.
///
/// Uses `kill(pid, 0)` which checks for process existence without sending a signal.
/// Returns `true` if the process exists and we have permission to signal it.
#[allow(dead_code)]
pub fn is_process_alive(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return false;
    }
    // Safety: kill with signal 0 is a safe existence check.
    let ret = unsafe { libc::kill(pid, 0) };
    ret == 0
}

/// Configuration for an interactive PTY launch.
pub struct PtyLaunchConfig<'a> {
    pub command: &'a str,
    pub args: Vec<String>,
    pub working_dir: &'a Path,
    pub env_vars: &'a std::collections::HashMap<String, String>,
    /// Optional output sink. Defaults to `TerminalSink` (stdout) if `None`.
    /// Provide a custom sink to route output to Slack, email, webhook, etc.
    pub output_sink: Option<Arc<dyn OutputSink>>,
}

/// Result of an interactive PTY session, including captured output.
pub struct PtySessionResult {
    pub exit_status: ExitStatus,
    pub captured_output: Vec<CapturedChunk>,
    pub human_inputs: Vec<CapturedInput>,
}

/// A chunk of captured agent output with timestamp.
#[derive(Debug, Clone)]
pub struct CapturedChunk {
    #[allow(dead_code)]
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub data: String,
}

/// A captured human input with timestamp.
#[derive(Debug, Clone)]
pub struct CapturedInput {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub text: String,
}

/// Run an agent in a PTY with stdin interleaving.
///
/// This is the main entry point for interactive sessions:
/// 1. Spawns the agent in a PTY
/// 2. Agent output streams to terminal in real-time AND is captured
/// 3. Human input from stdin is forwarded to the agent's PTY stdin
/// 4. All I/O is logged with timestamps for audit
pub fn run_interactive_pty(config: PtyLaunchConfig<'_>) -> io::Result<PtySessionResult> {
    let sink = config.output_sink.unwrap_or_else(|| Arc::new(TerminalSink));

    let mut pty = PtySession::spawn_with_sink(
        config.command,
        &config.args,
        config.working_dir,
        config.env_vars,
        sink,
    )?;

    let mut captured_output: Vec<CapturedChunk> = Vec::new();
    let mut human_inputs: Vec<CapturedInput> = Vec::new();

    // Set up stdin reader in a separate thread.
    let alive = pty.alive.clone();
    let (stdin_tx, stdin_rx) = mpsc::channel::<String>();

    let stdin_handle = thread::spawn(move || {
        let stdin = io::stdin();
        let mut line = String::new();
        while alive.load(Ordering::Relaxed) {
            line.clear();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if stdin_tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Main event loop: poll for PTY events and stdin input.
    loop {
        // Check for stdin input (non-blocking).
        if let Ok(text) = stdin_rx.try_recv() {
            // Forward to agent's PTY stdin.
            if let Err(e) = pty.write_stdin(text.as_bytes()) {
                eprintln!("\n[ta] Failed to send input to agent: {}", e);
            } else {
                human_inputs.push(CapturedInput {
                    timestamp: chrono::Utc::now(),
                    text: text.trim_end().to_string(),
                });
            }
        }

        // Check for PTY events.
        match pty.recv_event_timeout(Duration::from_millis(50)) {
            Some(PtyEvent::AgentOutput(data)) => {
                if let Ok(text) = String::from_utf8(data) {
                    captured_output.push(CapturedChunk {
                        timestamp: chrono::Utc::now(),
                        data: text,
                    });
                }
            }
            Some(PtyEvent::ReadError(_)) => break,
            Some(PtyEvent::AgentExited(_)) => break,
            None => {}
        }

        // Check if child is still running.
        let mut status: libc::c_int = 0;
        let ret = unsafe { libc::waitpid(pty.child_pid(), &mut status, libc::WNOHANG) };
        if ret > 0 {
            // Child has exited. Drain remaining output.
            while let Some(PtyEvent::AgentOutput(data)) = pty.try_recv_event() {
                if let Ok(text) = String::from_utf8(data) {
                    captured_output.push(CapturedChunk {
                        timestamp: chrono::Utc::now(),
                        data: text,
                    });
                }
            }

            use std::os::unix::process::ExitStatusExt;
            let exit_status = ExitStatus::from_raw(status);
            pty.alive.store(false, Ordering::Relaxed);

            // Wait for stdin thread to finish.
            let _ = stdin_handle.join();

            return Ok(PtySessionResult {
                exit_status,
                captured_output,
                human_inputs,
            });
        }
    }

    // Agent exited via PTY close. Wait for exit status.
    let exit_status = pty.wait()?;

    // Wait for stdin thread to finish.
    let _ = stdin_handle.join();

    Ok(PtySessionResult {
        exit_status,
        captured_output,
        human_inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect all output events from a session, waiting for the process to finish.
    /// Uses recv_event_timeout to drain events before and after wait().
    /// Has a 30-second overall timeout to prevent hanging in CI (no real terminal).
    fn collect_pty_output(session: &mut PtySession) -> (String, ExitStatus) {
        let mut output = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);

        // Drain events while process is running.
        loop {
            if std::time::Instant::now() > deadline {
                // Kill the child and return what we have.
                unsafe { libc::kill(session.child_pid(), libc::SIGKILL) };
                session.alive.store(false, Ordering::Relaxed);
                use std::os::unix::process::ExitStatusExt;
                return (output, ExitStatus::from_raw(1));
            }
            match session.recv_event_timeout(Duration::from_millis(200)) {
                Some(PtyEvent::AgentOutput(data)) => {
                    if let Ok(text) = String::from_utf8(data) {
                        output.push_str(&text);
                    }
                }
                Some(PtyEvent::ReadError(_)) => break,
                Some(PtyEvent::AgentExited(_)) => break,
                None => {
                    // Timeout — check if child has exited.
                    let mut status: libc::c_int = 0;
                    let ret =
                        unsafe { libc::waitpid(session.child_pid(), &mut status, libc::WNOHANG) };
                    if ret > 0 {
                        // Child exited. Drain remaining events.
                        while let Some(PtyEvent::AgentOutput(data)) = session.try_recv_event() {
                            if let Ok(text) = String::from_utf8(data) {
                                output.push_str(&text);
                            }
                        }
                        use std::os::unix::process::ExitStatusExt;
                        session.alive.store(false, Ordering::Relaxed);
                        return (output, ExitStatus::from_raw(status));
                    }
                }
            }
        }

        let status = session.wait().unwrap_or_else(|_| {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(1)
        });

        // Final drain after wait.
        while let Some(PtyEvent::AgentOutput(data)) = session.try_recv_event() {
            if let Ok(text) = String::from_utf8(data) {
                output.push_str(&text);
            }
        }

        (output, status)
    }

    #[test]
    fn open_pty_returns_valid_fds() {
        let (master, slave) = open_pty().expect("openpty should succeed");
        assert!(master.as_raw_fd() >= 0);
        assert!(slave.as_raw_fd() >= 0);
    }

    #[test]
    #[ignore] // Requires real terminal — hangs in CI
    fn pty_session_runs_echo_command() {
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir();
        let mut session = PtySession::spawn("/bin/echo", &["hello pty".to_string()], &dir, &env)
            .expect("spawn should succeed");

        let (output, status) = collect_pty_output(&mut session);
        assert!(status.success());
        assert!(output.contains("hello pty"), "output was: {:?}", output);
    }

    #[test]
    #[ignore] // Requires real terminal — hangs in CI
    fn pty_session_captures_stderr() {
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir();
        let mut session = PtySession::spawn(
            "/bin/sh",
            &["-c".to_string(), "echo err_msg >&2".to_string()],
            &dir,
            &env,
        )
        .expect("spawn should succeed");

        let (output, status) = collect_pty_output(&mut session);
        assert!(status.success());
        assert!(output.contains("err_msg"), "output was: {:?}", output);
    }

    #[test]
    #[ignore] // Requires real terminal — hangs in CI
    fn pty_session_nonexistent_command() {
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir();
        let mut session = PtySession::spawn("/nonexistent/command", &[], &dir, &env)
            .expect("spawn should succeed (fork succeeds, exec fails in child)");

        let (_output, status) = collect_pty_output(&mut session);
        assert!(!status.success());
    }

    #[test]
    #[ignore] // Requires real terminal — hangs in CI
    fn pty_session_write_stdin() {
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir();
        let mut session =
            PtySession::spawn("/bin/cat", &[], &dir, &env).expect("spawn should succeed");

        // Write to cat's stdin via PTY.
        session
            .write_stdin(b"hello from stdin\n")
            .expect("write should succeed");
        // Give cat a moment to echo it back.
        thread::sleep(Duration::from_millis(100));

        // Send EOF to cat (Ctrl+D).
        session.write_stdin(&[4]).expect("write EOF should succeed");

        let (output, status) = collect_pty_output(&mut session);
        assert!(status.success());
        assert!(
            output.contains("hello from stdin"),
            "output was: {:?}",
            output
        );
    }

    #[test]
    #[ignore] // Requires real terminal — hangs in CI
    fn pty_session_env_vars() {
        let mut env = std::collections::HashMap::new();
        env.insert("MY_TEST_VAR".to_string(), "pty_test_value".to_string());
        let dir = std::env::temp_dir();
        let mut session = PtySession::spawn(
            "/bin/sh",
            &["-c".to_string(), "echo $MY_TEST_VAR".to_string()],
            &dir,
            &env,
        )
        .expect("spawn should succeed");

        let (output, status) = collect_pty_output(&mut session);
        assert!(status.success());
        assert!(
            output.contains("pty_test_value"),
            "output was: {:?}",
            output
        );
    }

    #[test]
    fn captured_chunk_stores_timestamp() {
        let chunk = CapturedChunk {
            timestamp: chrono::Utc::now(),
            data: "test output".to_string(),
        };
        assert_eq!(chunk.data, "test output");
    }

    #[test]
    fn captured_input_stores_timestamp() {
        let input = CapturedInput {
            timestamp: chrono::Utc::now(),
            text: "human guidance".to_string(),
        };
        assert_eq!(input.text, "human guidance");
    }

    #[test]
    #[ignore] // Requires real terminal — hangs in CI
    fn run_interactive_pty_echo() {
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir();
        let result = run_interactive_pty(PtyLaunchConfig {
            command: "/bin/echo",
            args: vec!["interactive test".to_string()],
            working_dir: &dir,
            env_vars: &env,
            output_sink: None,
        })
        .expect("run should succeed");

        assert!(result.exit_status.success());
        let all_output: String = result
            .captured_output
            .iter()
            .map(|c| c.data.clone())
            .collect();
        assert!(
            all_output.contains("interactive test"),
            "output was: {:?}",
            all_output
        );
    }

    #[test]
    #[ignore] // Requires real terminal — hangs in CI
    fn custom_output_sink_receives_output() {
        let sink = Arc::new(CollectorSink::new());
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir();
        let mut session = PtySession::spawn_with_sink(
            "/bin/echo",
            &["sink test".to_string()],
            &dir,
            &env,
            sink.clone(),
        )
        .expect("spawn should succeed");

        let (_output, status) = collect_pty_output(&mut session);
        assert!(status.success());

        // The CollectorSink should have received the output.
        let collected = sink.get_output();
        assert!(
            collected.contains("sink test"),
            "sink collected: {:?}",
            collected
        );
    }

    #[test]
    fn is_process_alive_current_process() {
        let pid = std::process::id() as libc::pid_t;
        assert!(is_process_alive(pid));
    }

    #[test]
    fn is_process_alive_invalid_pid() {
        assert!(!is_process_alive(0));
        assert!(!is_process_alive(-1));
    }

    #[test]
    fn terminal_sink_implements_output_sink() {
        // Verify TerminalSink can be used as a trait object.
        let sink: Arc<dyn OutputSink> = Arc::new(TerminalSink);
        // Writing an empty slice should succeed.
        sink.emit_output(b"").unwrap();
    }
}
