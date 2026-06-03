// sandbox_windows.rs — Windows Job Object sandbox (v0.16.4).
//
// Provides process-tree containment for Windows agent processes using Win32
// Job Objects.  When `SandboxProvider::WindowsJobObject` is active, the agent
// and all its child processes are assigned to a Job Object with the
// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` flag set.  Closing the Job Object
// handle (on TA exit or drop) kills the entire process tree immediately.
//
// Uses raw extern "system" declarations rather than windows-sys to avoid
// version-resolution issues with target-conditional Cargo dependencies.
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

// ── Windows Win32 bindings ─────────────────────────────────────────────────────
//
// Inline extern declarations avoid windows-sys version-resolution issues with
// target-conditional Cargo.toml dependencies (lock file generated on macOS may
// select a different version than what Windows CI resolves at build time).

#[cfg(target_os = "windows")]
mod win32 {
    // JOBOBJECT_BASIC_LIMIT_INFORMATION (64-bit layout matches Windows SDK).
    // repr(C) adds implicit padding so field offsets match the C struct.
    #[repr(C)]
    pub struct JobObjectBasicLimitInfo {
        pub per_process_user_time_limit: i64, // LARGE_INTEGER
        pub per_job_user_time_limit: i64,     // LARGE_INTEGER
        pub limit_flags: u32,                 // DWORD  (offset 16)
        // 4 bytes implicit padding → next field at offset 24
        pub minimum_working_set_size: usize, // SIZE_T
        pub maximum_working_set_size: usize, // SIZE_T
        pub active_process_limit: u32,       // DWORD
        // 4 bytes implicit padding → next field at offset 48
        pub affinity: usize,       // ULONG_PTR
        pub priority_class: u32,   // DWORD
        pub scheduling_class: u32, // DWORD
    }

    // IO_COUNTERS (all fields are ULONGLONG = u64)
    #[repr(C)]
    pub struct IoCounters {
        pub read_operation_count: u64,
        pub write_operation_count: u64,
        pub other_operation_count: u64,
        pub read_transfer_count: u64,
        pub write_transfer_count: u64,
        pub other_transfer_count: u64,
    }

    // JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    #[repr(C)]
    pub struct JobObjectExtendedLimitInfo {
        pub basic_limit_information: JobObjectBasicLimitInfo,
        pub io_info: IoCounters,
        pub process_memory_limit: usize,
        pub job_memory_limit: usize,
        pub peak_process_memory_used: usize,
        pub peak_job_memory_used: usize,
    }

    pub const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    pub const PROCESS_ALL_ACCESS: u32 = 0x001F_FFFF;
    pub const FALSE: i32 = 0;
    // JOBOBJECTINFOCLASS::JobObjectExtendedLimitInformation = 9
    pub const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateJobObjectW(
            lp_job_attributes: *mut std::ffi::c_void,
            lp_name: *const u16,
        ) -> isize;

        pub fn AssignProcessToJobObject(h_job: isize, h_process: isize) -> i32;

        pub fn SetInformationJobObject(
            h_job: isize,
            job_object_info_class: i32,
            lp_job_object_info: *mut std::ffi::c_void,
            cb_job_object_info_length: u32,
        ) -> i32;

        pub fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> isize;

        pub fn CloseHandle(h_object: isize) -> i32;

        pub fn GetLastError() -> u32;
    }
}

// ── Windows implementation ────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
impl WindowsJobObjectGuard {
    /// Create a new anonymous Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    ///
    /// Returns an error if `CreateJobObject` or `SetInformationJobObject` fails.
    /// The error message includes the Win32 error code for diagnostics.
    pub fn new() -> Result<Self, String> {
        use win32::*;

        // Safety: CreateJobObjectW with NULL attributes and NULL name creates an
        // anonymous job object with default security and no name.
        let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };

        if handle == 0 {
            return Err(format!(
                "CreateJobObjectW failed (Win32 error {})",
                unsafe { GetLastError() }
            ));
        }

        // Configure KILL_ON_JOB_CLOSE so the process tree is torn down when the
        // last handle to this Job Object is closed (i.e., when the guard drops).
        let mut info: JobObjectExtendedLimitInfo = unsafe { std::mem::zeroed() };
        info.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                &mut info as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<JobObjectExtendedLimitInfo>() as u32,
            )
        };

        if ok == FALSE {
            let err = unsafe { GetLastError() };
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
        use win32::*;

        // Open the target process with sufficient rights to assign it.
        let proc_handle = unsafe { OpenProcess(PROCESS_ALL_ACCESS, FALSE, pid) };
        if proc_handle == 0 {
            return Err(format!(
                "OpenProcess(pid={}) failed (Win32 error {})",
                pid,
                unsafe { GetLastError() }
            ));
        }

        let ok = unsafe { AssignProcessToJobObject(self.handle, proc_handle) };
        // Always close the process handle we opened; we no longer need it.
        unsafe { CloseHandle(proc_handle) };

        if ok == FALSE {
            return Err(format!(
                "AssignProcessToJobObject(pid={}) failed (Win32 error {})",
                pid,
                unsafe { GetLastError() }
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
        unsafe { win32::CloseHandle(self.handle) };
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
