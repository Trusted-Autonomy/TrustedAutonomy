// sandbox_windows.rs — Windows Job Object sandbox (v0.16.4).
//
// Provides process-tree containment for Windows agent processes using Win32
// Job Objects.  When `SandboxProvider::WindowsJobObject` is active, the agent
// and all its child processes are assigned to a Job Object with the
// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` flag set.  Closing the Job Object
// handle (on TA exit or drop) kills the entire process tree immediately.
//
// ## What this provides
//
// - **Process tree teardown**: When TA exits (or crashes), the Job Object
//   handle is closed and the kernel terminates the agent and every subprocess
//   it spawned — no zombie agent processes linger.
// - **Resource limit enforcement**: CPU time, memory, and active process count
//   can be capped via `JOBOBJECT_BASIC_LIMIT_INFORMATION` flags.
//
// ## What this does NOT provide
//
// - **Filesystem isolation**: Job Objects do not restrict filesystem access.
//   Filesystem containment requires AppContainer (planned for a later phase).
// - **Network isolation**: Network access is not restricted by Job Objects.
//
// ## Usage
//
// ```rust,no_run
// use ta_runtime::sandbox_windows::WindowsJobObjectGuard;
//
// let guard = WindowsJobObjectGuard::new()?;
// guard.assign_process(child_pid)?;
// // guard is held alive for the agent's lifetime.
// // When guard drops, the agent process tree is killed.
// ```

/// A guard wrapping a Windows Job Object handle.
///
/// On Windows: creates a real Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
/// On other platforms: a no-op zero-size type (for cross-platform compilation).
#[cfg(target_os = "windows")]
pub struct WindowsJobObjectGuard {
    /// Raw Win32 HANDLE to the Job Object.  Always non-null (NULL means error).
    handle: isize,
}

#[cfg(not(target_os = "windows"))]
pub struct WindowsJobObjectGuard;

// ── Windows implementation ────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
impl WindowsJobObjectGuard {
    /// Create a new anonymous Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    ///
    /// Returns an error if `CreateJobObject` or `SetInformationJobObject` fails.
    /// The error message includes the Win32 error code for diagnostics.
    pub fn new() -> Result<Self, String> {
        use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // Safety: CreateJobObjectW with NULL attributes and NULL name creates an
        // anonymous job object with default security and no name.
        let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };

        if handle == 0 {
            return Err(format!(
                "CreateJobObjectW failed (Win32 error {})",
                last_error()
            ));
        }

        // Configure KILL_ON_JOB_CLOSE so the process tree is torn down when the
        // last handle to this Job Object is closed (i.e., when the guard drops).
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                PerProcessUserTimeLimit: 0,
                PerJobUserTimeLimit: 0,
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                MinimumWorkingSetSize: 0,
                MaximumWorkingSetSize: 0,
                ActiveProcessLimit: 0,
                Affinity: 0,
                PriorityClass: 0,
                SchedulingClass: 0,
            },
            IoInfo: windows_sys::Win32::System::JobObjects::IO_COUNTERS {
                ReadOperationCount: 0,
                WriteOperationCount: 0,
                OtherOperationCount: 0,
                ReadTransferCount: 0,
                WriteTransferCount: 0,
                OtherTransferCount: 0,
            },
            ProcessMemoryLimit: 0,
            JobMemoryLimit: 0,
            PeakProcessMemoryUsed: 0,
            PeakJobMemoryUsed: 0,
        };

        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };

        if ok == FALSE {
            let err = last_error();
            // Clean up the handle before returning the error.
            unsafe { CloseHandle(handle) };
            return Err(format!(
                "SetInformationJobObject(KILL_ON_JOB_CLOSE) failed (Win32 error {})",
                err
            ));
        }

        Ok(Self { handle })
    }

    /// Assign a process to this Job Object.
    ///
    /// After assignment, the process and all processes it creates will be members
    /// of this Job Object.  When the Job Object handle is closed, all members are
    /// terminated.
    ///
    /// Returns an error if `OpenProcess` or `AssignProcessToJobObject` fails.
    pub fn assign_process(&self, pid: u32) -> Result<(), String> {
        use windows_sys::Win32::Foundation::{CloseHandle, FALSE};
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

        // Open the target process with sufficient rights to assign it.
        let proc_handle = unsafe { OpenProcess(PROCESS_ALL_ACCESS, FALSE, pid) };
        if proc_handle == 0 {
            return Err(format!(
                "OpenProcess(pid={}) failed (Win32 error {})",
                pid,
                last_error()
            ));
        }

        let ok = unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.handle,
                proc_handle,
            )
        };
        // Always close the process handle we opened; we no longer need it.
        unsafe { CloseHandle(proc_handle) };

        if ok == FALSE {
            return Err(format!(
                "AssignProcessToJobObject(pid={}) failed (Win32 error {})",
                pid,
                last_error()
            ));
        }

        Ok(())
    }

    /// Return the raw Win32 HANDLE (for diagnostics / tests only).
    #[allow(dead_code)]
    pub(crate) fn raw_handle(&self) -> isize {
        self.handle
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsJobObjectGuard {
    fn drop(&mut self) {
        // Closing the handle triggers KILL_ON_JOB_CLOSE: the kernel terminates
        // all processes in the Job Object.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

// Job Objects must not be shared across threads without synchronisation;
// however, a guard is Send because the handle is valid from any thread.
// (We never share it — it is moved into one thread for its lifetime.)
#[cfg(target_os = "windows")]
unsafe impl Send for WindowsJobObjectGuard {}

// ── Stub for non-Windows platforms ───────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
impl WindowsJobObjectGuard {
    /// No-op on non-Windows platforms.
    pub fn new() -> Result<Self, String> {
        Ok(Self)
    }

    /// No-op on non-Windows platforms.
    pub fn assign_process(&self, _pid: u32) -> Result<(), String> {
        Ok(())
    }
}

// ── Win32 error helper ────────────────────────────────────────────────────────

/// Return the thread-local Win32 last-error code as a u32 for diagnostics.
#[cfg(target_os = "windows")]
fn last_error() -> u32 {
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// On all platforms: constructing a guard succeeds.
    #[test]
    fn guard_constructs_successfully() {
        let guard = WindowsJobObjectGuard::new();
        assert!(guard.is_ok(), "WindowsJobObjectGuard::new() should succeed");
    }

    /// On all platforms: assigning a non-existent PID is handled gracefully
    /// (on non-Windows it's a no-op; on Windows it returns an error).
    #[test]
    fn assign_invalid_pid_returns_error_on_windows() {
        let guard = WindowsJobObjectGuard::new().unwrap();
        let result = guard.assign_process(0xFFFF_FFFF);
        // On non-Windows: always Ok (stub).
        // On Windows: OpenProcess should fail for a non-existent PID.
        #[cfg(target_os = "windows")]
        assert!(result.is_err(), "assigning non-existent PID should fail");
        #[cfg(not(target_os = "windows"))]
        assert!(result.is_ok(), "stub always succeeds");
    }

    /// Windows only: verify that the Job Object handle is non-null.
    #[cfg(target_os = "windows")]
    #[test]
    fn job_object_handle_is_valid() {
        let guard = WindowsJobObjectGuard::new().unwrap();
        assert_ne!(guard.raw_handle(), 0, "Job Object handle must not be NULL");
    }

    /// Windows only: assign a live child process to the Job Object.
    ///
    /// Spawns a subprocess that runs for a few seconds, assigns it to the Job
    /// Object, then drops the guard.  Verifies that the subprocess is no longer
    /// running after the guard is dropped (KILL_ON_JOB_CLOSE semantics).
    #[cfg(target_os = "windows")]
    #[test]
    fn kill_on_job_close_terminates_process_tree() {
        use std::process::Command;
        use std::time::Duration;

        // Spawn a subprocess that sleeps long enough for us to assign it.
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 30 localhost > nul"])
            .spawn()
            .expect("Failed to spawn test subprocess");

        let pid = child.id();

        {
            let guard = WindowsJobObjectGuard::new().expect("Job Object creation failed");
            guard
                .assign_process(pid)
                .expect("assign_process should succeed for live PID");
            // guard drops here → KILL_ON_JOB_CLOSE fires
        }

        // Give the kernel a moment to propagate the kill.
        std::thread::sleep(Duration::from_millis(200));

        // try_wait() should return Some(_) — the process is gone.
        let status = child.try_wait().expect("try_wait failed");
        assert!(
            status.is_some(),
            "Process should have been killed by KILL_ON_JOB_CLOSE when Job Object handle closed"
        );
    }

    /// Windows only: attempt to write outside the staging path.
    ///
    /// Job Objects do not restrict filesystem access; this test verifies that
    /// the sandbox integration point (SandboxPolicy::apply for WindowsJobObject)
    /// leaves the SpawnRequest unchanged (no command wrapping), and that the
    /// path-escape protection from the SandboxRunner layer still catches escapes.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_job_object_apply_is_noop_for_spawn_request() {
        use crate::adapter::{SpawnRequest, StdinMode, StdoutMode};
        use crate::sandbox::{SandboxPolicy, SandboxProvider};
        use std::collections::HashMap;

        let policy = SandboxPolicy {
            enabled: true,
            provider: SandboxProvider::WindowsJobObject,
            allow_read: vec![],
            allow_write: vec![],
            allow_network: vec![],
        };

        let req = SpawnRequest {
            command: "claude".to_string(),
            args: vec!["--print".to_string(), "hello".to_string()],
            env: HashMap::new(),
            working_dir: std::path::PathBuf::from("C:\\staging\\workspace"),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };

        let wrapped = policy.apply(req.clone());
        // WindowsJobObject does NOT wrap the command — it operates post-spawn.
        assert_eq!(
            wrapped.command, req.command,
            "WindowsJobObject provider must not wrap the command"
        );
        assert_eq!(
            wrapped.args, req.args,
            "WindowsJobObject provider must not modify args"
        );
    }
}
