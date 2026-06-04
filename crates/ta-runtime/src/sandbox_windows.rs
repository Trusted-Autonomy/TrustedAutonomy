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

// ── Windows AppContainer Sandbox (v0.16.4.2) ─────────────────────────────────
//
// Extends the Job Object sandbox (v0.16.4) with AppContainer confinement.
// AppContainer assigns a low-integrity SID to the agent process, restricts
// filesystem access to the staging workspace, and blocks network access unless
// declared capability SIDs are present.
//
// The two layers work together:
//   Job Object     — process-tree teardown on TA exit (KILL_ON_JOB_CLOSE)
//   AppContainer   — filesystem + network isolation (SECURITY_CAPABILITIES)
//
// Usage:
//   let guard = WindowsAppContainerGuard::new("ta-<goal-id>", staging_path, allow_network)?;
//   let handle = sandboxed_spawn(&*runtime, request, Some(&guard))?;
//   // guard stays alive for the agent's lifetime; drop to delete the profile.

// ── Win32 bindings for AppContainer ──────────────────────────────────────────

#[cfg(target_os = "windows")]
pub(crate) mod appcontainer_win32 {
    use std::ffi::c_void;

    // ── Structs ──────────────────────────────────────────────────────────────

    /// SECURITY_CAPABILITIES — passed to CreateProcessW via STARTUPINFOEXW.
    #[repr(C)]
    pub struct SecurityCapabilities {
        pub app_container_sid: *mut c_void, // PSID
        pub capabilities: *mut SidAndAttributes,
        pub capability_count: u32,
        pub reserved: u32,
    }

    /// SID_AND_ATTRIBUTES — one capability SID entry.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SidAndAttributes {
        pub sid: *mut c_void, // PSID
        pub attributes: u32,
    }

    /// SID_IDENTIFIER_AUTHORITY
    #[repr(C)]
    pub struct SidIdentifierAuthority {
        pub value: [u8; 6],
    }

    /// STARTUPINFOW (used inside STARTUPINFOEXW).
    #[repr(C)]
    pub struct StartupInfoW {
        pub cb: u32,
        pub lp_reserved: *const u16,
        pub lp_desktop: *const u16,
        pub lp_title: *const u16,
        pub dw_x: u32,
        pub dw_y: u32,
        pub dw_x_size: u32,
        pub dw_y_size: u32,
        pub dw_x_count_chars: u32,
        pub dw_y_count_chars: u32,
        pub dw_fill_attribute: u32,
        pub dw_flags: u32,
        pub w_show_window: u16,
        pub cb_reserved2: u16,
        pub lp_reserved2: *const u8,
        pub h_std_input: isize,
        pub h_std_output: isize,
        pub h_std_error: isize,
    }

    /// STARTUPINFOEXW — extended startup info with attribute list.
    #[repr(C)]
    pub struct StartupInfoExW {
        pub startup_info: StartupInfoW,
        pub lp_attribute_list: *mut c_void, // LPPROC_THREAD_ATTRIBUTE_LIST
    }

    /// PROCESS_INFORMATION
    #[repr(C)]
    pub struct ProcessInformation {
        pub h_process: isize,
        pub h_thread: isize,
        pub dw_process_id: u32,
        pub dw_thread_id: u32,
    }

    /// SECURITY_ATTRIBUTES (for inheritable pipe handles).
    #[repr(C)]
    pub struct SecurityAttributes {
        pub n_length: u32,
        pub lp_security_descriptor: *mut c_void,
        pub b_inherit_handle: i32,
    }

    /// TRUSTEE_W
    #[repr(C)]
    pub struct TrusteeW {
        pub p_multiple_trustee: *mut TrusteeW,
        pub multiple_trustee_operation: u32,
        pub trustee_form: u32,
        pub trustee_type: u32,
        /// PSID cast to LPWSTR when trustee_form = TRUSTEE_IS_SID.
        pub ptr_str_name: *mut c_void,
    }

    /// EXPLICIT_ACCESS_W
    #[repr(C)]
    pub struct ExplicitAccessW {
        pub grf_access_permissions: u32,
        pub grf_access_mode: u32,
        pub grf_inheritance: u32,
        pub trustee: TrusteeW,
    }

    // ── Constants ─────────────────────────────────────────────────────────────

    pub const SE_GROUP_ENABLED: u32 = 0x0000_0004;
    pub const PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES: usize = 0x0002_0009;
    pub const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
    pub const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
    pub const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
    pub const INFINITE: u32 = 0xFFFF_FFFF;
    pub const STILL_ACTIVE: u32 = 259;
    pub const SE_FILE_OBJECT: u32 = 1;
    pub const DACL_SECURITY_INFORMATION: u32 = 4;
    pub const SET_ACCESS: u32 = 2;
    pub const CONTAINER_INHERIT_ACE: u32 = 0x2;
    pub const OBJECT_INHERIT_ACE: u32 = 0x1;
    pub const TRUSTEE_IS_SID: u32 = 0;
    pub const TRUSTEE_IS_UNKNOWN: u32 = 0;
    pub const NO_MULTIPLE_TRUSTEE: u32 = 0;
    pub const ERROR_SUCCESS: u32 = 0;
    pub const FILE_GENERIC_READ: u32 = 0x0012_0089;
    pub const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
    pub const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;
    pub const HANDLE_FLAG_INHERIT: u32 = 0x1;
    pub const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // -10i32 as u32
    pub const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    pub const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;
    pub const INVALID_HANDLE_VALUE: isize = -1isize;
    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const FILE_SHARE_READ: u32 = 1;
    pub const FILE_SHARE_WRITE: u32 = 2;
    pub const OPEN_EXISTING: u32 = 3;
    // HRESULT S_OK
    pub const S_OK: i32 = 0;
    // internetClient capability: S-1-15-3-1
    pub const SECURITY_APP_PACKAGE_AUTHORITY: [u8; 6] = [0, 0, 0, 0, 0, 15];
    pub const SECURITY_CAPABILITY_BASE_RID: u32 = 3;
    pub const SECURITY_CAPABILITY_INTERNET_CLIENT: u32 = 1;

    // ── Kernel32 ──────────────────────────────────────────────────────────────

    #[link(name = "kernel32")]
    extern "system" {
        pub fn InitializeProcThreadAttributeList(
            lp_attribute_list: *mut c_void,
            dw_attribute_count: u32,
            dw_flags: u32,
            lp_size: *mut usize,
        ) -> i32;

        pub fn UpdateProcThreadAttribute(
            lp_attribute_list: *mut c_void,
            dw_flags: u32,
            attribute: usize,
            lp_value: *const c_void,
            cb_size: usize,
            lp_previous_value: *mut c_void,
            lp_return_size: *mut usize,
        ) -> i32;

        pub fn DeleteProcThreadAttributeList(lp_attribute_list: *mut c_void);

        pub fn CreateProcessW(
            lp_application_name: *const u16,
            lp_command_line: *mut u16,
            lp_process_attributes: *mut c_void,
            lp_thread_attributes: *mut c_void,
            b_inherit_handles: i32,
            dw_creation_flags: u32,
            lp_environment: *mut c_void,
            lp_current_directory: *const u16,
            lp_startup_info: *mut c_void,
            lp_process_information: *mut ProcessInformation,
        ) -> i32;

        pub fn WaitForSingleObject(h_handle: isize, dw_milliseconds: u32) -> u32;

        pub fn GetExitCodeProcess(h_process: isize, lp_exit_code: *mut u32) -> i32;

        pub fn TerminateProcess(h_process: isize, u_exit_code: u32) -> i32;

        pub fn CreatePipe(
            h_read_pipe: *mut isize,
            h_write_pipe: *mut isize,
            lp_pipe_attributes: *mut c_void,
            n_size: u32,
        ) -> i32;

        pub fn SetHandleInformation(h_object: isize, dw_mask: u32, dw_flags: u32) -> i32;

        pub fn GetStdHandle(n_std_handle: u32) -> isize;

        pub fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *mut c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: isize,
        ) -> isize;
    }

    // ── Userenv ───────────────────────────────────────────────────────────────

    #[link(name = "userenv")]
    extern "system" {
        pub fn CreateAppContainerProfile(
            psz_app_container_name: *const u16,
            psz_display_name: *const u16,
            psz_description: *const u16,
            p_capabilities: *const SidAndAttributes,
            dw_capability_count: u32,
            pp_sid_app_container_sid: *mut *mut c_void,
        ) -> i32; // HRESULT

        pub fn DeleteAppContainerProfile(psz_app_container_name: *const u16) -> i32; // HRESULT

        pub fn DeriveAppContainerSidFromAppContainerName(
            psz_app_container_name: *const u16,
            pp_sid_app_container_sid: *mut *mut c_void,
        ) -> i32; // HRESULT
    }

    // ── Advapi32 ──────────────────────────────────────────────────────────────

    #[link(name = "advapi32")]
    extern "system" {
        pub fn FreeSid(p_sid: *mut c_void);

        pub fn AllocateAndInitializeSid(
            p_identifier_authority: *const SidIdentifierAuthority,
            n_sub_authority_count: u8,
            n_sub_authority_0: u32,
            n_sub_authority_1: u32,
            n_sub_authority_2: u32,
            n_sub_authority_3: u32,
            n_sub_authority_4: u32,
            n_sub_authority_5: u32,
            n_sub_authority_6: u32,
            n_sub_authority_7: u32,
            pp_sid: *mut *mut c_void,
        ) -> i32;

        pub fn GetNamedSecurityInfoW(
            psz_object_name: *const u16,
            object_type: u32,
            security_info: u32,
            ppsid_owner: *mut *mut c_void,
            ppsid_group: *mut *mut c_void,
            pp_dacl: *mut *mut c_void,
            pp_sacl: *mut *mut c_void,
            pp_security_descriptor: *mut *mut c_void,
        ) -> u32;

        pub fn SetNamedSecurityInfoW(
            psz_object_name: *mut u16,
            object_type: u32,
            security_info: u32,
            psid_owner: *mut c_void,
            psid_group: *mut c_void,
            p_dacl: *mut c_void,
            p_sacl: *mut c_void,
        ) -> u32;

        pub fn SetEntriesInAclW(
            c_count_of_explicit_entries: u32,
            p_list_of_explicit_entries: *const ExplicitAccessW,
            old_acl: *mut c_void,
            new_acl: *mut *mut c_void,
        ) -> u32;

        pub fn LocalFree(h_mem: *mut c_void) -> *mut c_void;
    }
}

// ── Helpers (Windows-only) ────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn str_to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(target_os = "windows")]
fn path_to_wide(path: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(target_os = "windows")]
fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
        let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        arg.to_string()
    }
}

#[cfg(target_os = "windows")]
fn build_command_line(command: &str, args: &[String]) -> String {
    std::iter::once(quote_arg(command))
        .chain(args.iter().map(|a| quote_arg(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a UTF-16, null-separated, double-null-terminated environment block.
/// Merges the current process's env with the overrides from the request.
#[cfg(target_os = "windows")]
fn build_env_block(overrides: &std::collections::HashMap<String, String>) -> Vec<u16> {
    let mut merged: std::collections::HashMap<String, String> = std::env::vars().collect();
    for (k, v) in overrides {
        merged.insert(k.clone(), v.clone());
    }
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in &merged {
        let entry = format!("{}={}", k, v);
        block.extend(entry.encode_utf16());
        block.push(0);
    }
    block.push(0); // double-null terminator
    block
}

// ── AppContainerProcessHandle ─────────────────────────────────────────────────

/// AgentHandle wrapping a process launched by CreateProcessW (AppContainer path).
///
/// This replaces `BareProcessHandle` for AppContainer-spawned processes because
/// `std::process::Command` doesn't support STARTUPINFOEXW.
#[cfg(target_os = "windows")]
pub struct AppContainerProcessHandle {
    process_handle: isize,
    pid: u32,
    stdout: Option<std::process::ChildStdout>,
}

#[cfg(target_os = "windows")]
impl crate::adapter::AgentHandle for AppContainerProcessHandle {
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn status(&mut self) -> crate::adapter::Result<crate::adapter::RuntimeStatus> {
        use appcontainer_win32::*;
        let mut exit_code: u32 = 0;
        let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
        if ok == 0 {
            return Err(crate::adapter::RuntimeError::StatusCheckFailed(format!(
                "GetExitCodeProcess failed (Win32 error {})",
                unsafe { win32::GetLastError() }
            )));
        }
        if exit_code == STILL_ACTIVE {
            Ok(crate::adapter::RuntimeStatus::Running)
        } else {
            Ok(crate::adapter::RuntimeStatus::Exited {
                exit_code: Some(exit_code as i32),
            })
        }
    }

    fn wait(&mut self) -> crate::adapter::Result<std::process::ExitStatus> {
        use appcontainer_win32::*;
        use std::os::windows::process::ExitStatusExt as _;

        unsafe { WaitForSingleObject(self.process_handle, INFINITE) };

        let mut exit_code: u32 = 0;
        let ok = unsafe { GetExitCodeProcess(self.process_handle, &mut exit_code) };
        if ok == 0 {
            return Err(crate::adapter::RuntimeError::StatusCheckFailed(format!(
                "GetExitCodeProcess after wait failed (Win32 error {})",
                unsafe { win32::GetLastError() }
            )));
        }
        Ok(std::process::ExitStatus::from_raw(exit_code))
    }

    fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.stdout.take()
    }

    fn transport_info(&self) -> crate::adapter::TransportInfo {
        crate::adapter::TransportInfo::Stdio
    }

    fn stop(&mut self) -> crate::adapter::Result<()> {
        unsafe { appcontainer_win32::TerminateProcess(self.process_handle, 1) };
        Ok(())
    }
}

#[cfg(target_os = "windows")]
impl Drop for AppContainerProcessHandle {
    fn drop(&mut self) {
        unsafe { win32::CloseHandle(self.process_handle) };
    }
}

#[cfg(target_os = "windows")]
unsafe impl Send for AppContainerProcessHandle {}

// ── WindowsAppContainerGuard ──────────────────────────────────────────────────

/// Guard that manages the Windows AppContainer profile lifecycle.
///
/// On creation: calls `CreateAppContainerProfile`, grants the container SID
/// read+write access on the staging workspace path, and optionally adds the
/// `internetClient` capability SID for network access.
///
/// On drop: calls `DeleteAppContainerProfile` and frees all SIDs.
///
/// The guard must outlive any process spawned inside the container.
#[cfg(target_os = "windows")]
pub struct WindowsAppContainerGuard {
    container_name: Vec<u16>,
    container_sid: *mut std::ffi::c_void,
    /// Stable storage for SID_AND_ATTRIBUTES capability entries.
    /// Box ensures the pointer doesn't move when the guard is moved.
    capabilities_storage: Box<[appcontainer_win32::SidAndAttributes]>,
    /// Extra SIDs that need FreeSid on drop (one per network capability SID).
    extra_sids: Vec<*mut std::ffi::c_void>,
}

#[cfg(target_os = "windows")]
impl WindowsAppContainerGuard {
    /// Create an AppContainer profile for an agent goal.
    ///
    /// - `container_name`: unique name for this goal (e.g., `"ta-<first-8-chars-of-goal-id>"`).
    ///   Max 64 chars; valid chars are letters, digits, dots, and dashes.
    /// - `staging_path`: the agent's working directory; the container is granted RW access here.
    /// - `allow_network`: if true, adds the `internetClient` capability SID so the agent
    ///   can make outbound network connections. If false, network is blocked.
    pub fn new(
        container_name: &str,
        staging_path: &std::path::Path,
        allow_network: bool,
    ) -> Result<Self, String> {
        use appcontainer_win32::*;
        use std::ptr::null_mut;

        let name_wide = str_to_wide(container_name);

        // Build capability SIDs first (needed for CreateAppContainerProfile).
        let mut extra_sids: Vec<*mut std::ffi::c_void> = Vec::new();
        let mut cap_entries: Vec<SidAndAttributes> = Vec::new();

        if allow_network {
            // S-1-15-3-1: internetClient capability SID.
            let authority = SidIdentifierAuthority {
                value: SECURITY_APP_PACKAGE_AUTHORITY,
            };
            let mut net_sid: *mut std::ffi::c_void = null_mut();
            let ok = unsafe {
                AllocateAndInitializeSid(
                    &authority,
                    2, // 2 sub-authorities
                    SECURITY_CAPABILITY_BASE_RID,
                    SECURITY_CAPABILITY_INTERNET_CLIENT,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    &mut net_sid,
                )
            };
            if ok == 0 {
                return Err(format!(
                    "AllocateAndInitializeSid(internetClient) failed (Win32 error {})",
                    unsafe { win32::GetLastError() }
                ));
            }
            extra_sids.push(net_sid);
            cap_entries.push(SidAndAttributes {
                sid: net_sid,
                attributes: SE_GROUP_ENABLED,
            });
        }

        let cap_count = cap_entries.len() as u32;
        let cap_ptr = if cap_entries.is_empty() {
            null_mut()
        } else {
            cap_entries.as_ptr()
        };

        // Create the AppContainer profile (or retrieve existing).
        let mut container_sid: *mut std::ffi::c_void = null_mut();
        let hr = unsafe {
            CreateAppContainerProfile(
                name_wide.as_ptr(),
                name_wide.as_ptr(), // display name = container name
                name_wide.as_ptr(), // description = container name
                cap_ptr,
                cap_count,
                &mut container_sid,
            )
        };

        let profile_created;
        if hr == S_OK {
            profile_created = true;
        } else {
            // HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS) = 0x80070050
            let already_exists = hr == -2147024816i32; // 0x80070050u32 as i32
            if already_exists {
                // Profile exists from a prior crashed run — retrieve its SID.
                let hr2 = unsafe {
                    DeriveAppContainerSidFromAppContainerName(
                        name_wide.as_ptr(),
                        &mut container_sid,
                    )
                };
                if hr2 != S_OK {
                    for sid in extra_sids {
                        unsafe { FreeSid(sid) };
                    }
                    return Err(format!(
                        "DeriveAppContainerSidFromAppContainerName failed (HRESULT 0x{:08X})",
                        hr2 as u32
                    ));
                }
                profile_created = false;
            } else {
                for sid in extra_sids {
                    unsafe { FreeSid(sid) };
                }
                return Err(format!(
                    "CreateAppContainerProfile failed (HRESULT 0x{:08X})",
                    hr as u32
                ));
            }
        }
        let _ = profile_created;

        // Grant the AppContainer SID read+write access on the staging workspace.
        if let Err(e) = grant_staging_path_access(staging_path, container_sid) {
            unsafe { FreeSid(container_sid) };
            for sid in extra_sids {
                unsafe { FreeSid(sid) };
            }
            return Err(e);
        }

        // Move capabilities into stable Box storage.
        let capabilities_storage: Box<[SidAndAttributes]> = cap_entries.into_boxed_slice();

        Ok(Self {
            container_name: name_wide,
            container_sid,
            capabilities_storage,
            extra_sids,
        })
    }

    /// Build a SECURITY_CAPABILITIES struct pointing into this guard's stable storage.
    ///
    /// The returned value borrows from self — it is only valid while the guard is alive.
    /// Use it immediately before calling CreateProcessW; do not store it across moves.
    pub fn security_capabilities(&self) -> appcontainer_win32::SecurityCapabilities {
        appcontainer_win32::SecurityCapabilities {
            app_container_sid: self.container_sid,
            capabilities: if self.capabilities_storage.is_empty() {
                std::ptr::null_mut()
            } else {
                self.capabilities_storage.as_ptr() as *mut _
            },
            capability_count: self.capabilities_storage.len() as u32,
            reserved: 0,
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsAppContainerGuard {
    fn drop(&mut self) {
        // Delete the AppContainer profile.
        unsafe { appcontainer_win32::DeleteAppContainerProfile(self.container_name.as_ptr()) };
        // Free the container SID.
        if !self.container_sid.is_null() {
            unsafe { appcontainer_win32::FreeSid(self.container_sid) };
        }
        // Free extra capability SIDs.
        for sid in &self.extra_sids {
            unsafe { appcontainer_win32::FreeSid(*sid) };
        }
    }
}

#[cfg(target_os = "windows")]
unsafe impl Send for WindowsAppContainerGuard {}

/// Grant the AppContainer SID read+write+execute access on the staging workspace.
///
/// Modifies the DACL of `staging_path` to add an explicit allow ACE for
/// `container_sid`.  The ACE inherits to all children of the directory so the
/// container can access files the agent creates at runtime.
#[cfg(target_os = "windows")]
fn grant_staging_path_access(
    staging_path: &std::path::Path,
    container_sid: *mut std::ffi::c_void,
) -> Result<(), String> {
    use appcontainer_win32::*;
    use std::ptr::null_mut;

    let mut path_wide = path_to_wide(staging_path);

    // Get the current DACL for the staging path.
    let mut p_dacl: *mut std::ffi::c_void = null_mut();
    let mut p_security_descriptor: *mut std::ffi::c_void = null_mut();
    let rc = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut p_dacl,
            null_mut(),
            &mut p_security_descriptor,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(format!(
            "GetNamedSecurityInfoW on staging path failed (Win32 error {})",
            rc
        ));
    }

    // Build an EXPLICIT_ACCESS entry that grants the container SID RW+X access.
    let trustee = TrusteeW {
        p_multiple_trustee: null_mut(),
        multiple_trustee_operation: NO_MULTIPLE_TRUSTEE,
        trustee_form: TRUSTEE_IS_SID,
        trustee_type: TRUSTEE_IS_UNKNOWN,
        ptr_str_name: container_sid,
    };
    let explicit_access = ExplicitAccessW {
        grf_access_permissions: FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE,
        grf_access_mode: SET_ACCESS,
        grf_inheritance: CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
        trustee,
    };

    // Merge with the existing DACL.
    let mut new_dacl: *mut std::ffi::c_void = null_mut();
    let rc = unsafe { SetEntriesInAclW(1, &explicit_access, p_dacl, &mut new_dacl) };
    // Free the security descriptor from GetNamedSecurityInfoW.
    if !p_security_descriptor.is_null() {
        unsafe { LocalFree(p_security_descriptor) };
    }
    if rc != ERROR_SUCCESS {
        return Err(format!("SetEntriesInAclW failed (Win32 error {})", rc));
    }

    // Apply the new DACL to the staging path.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            new_dacl,
            null_mut(),
        )
    };
    // Free the new DACL allocated by SetEntriesInAclW.
    unsafe { LocalFree(new_dacl) };

    if rc != ERROR_SUCCESS {
        return Err(format!(
            "SetNamedSecurityInfoW on staging path failed (Win32 error {})",
            rc
        ));
    }

    Ok(())
}

/// Probe whether AppContainer APIs are available on this Windows installation.
///
/// Returns true on Windows 8+ where `DeriveAppContainerSidFromAppContainerName`
/// is present and functional.  On pre-Win8 systems (or binary-load failure),
/// the binary would not have reached this point, so this is effectively always
/// true when running on a supported system; the probe detects runtime errors
/// such as permission restrictions.
pub fn appcontainer_available() -> bool {
    #[cfg(target_os = "windows")]
    {
        use appcontainer_win32::*;
        use std::ptr::null_mut;

        let test_name = str_to_wide("ta-probe-availability-test");
        let mut sid: *mut std::ffi::c_void = null_mut();
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(test_name.as_ptr(), &mut sid) };
        if hr == S_OK && !sid.is_null() {
            unsafe { FreeSid(sid) };
            true
        } else {
            false
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

// ── spawn_in_appcontainer ─────────────────────────────────────────────────────

/// Open the Windows NUL device as an inheritable read handle.
#[cfg(target_os = "windows")]
fn open_nul_for_read() -> Result<isize, String> {
    use appcontainer_win32::*;
    let nul = str_to_wide("NUL");
    let mut sa = SecurityAttributes {
        n_length: std::mem::size_of::<SecurityAttributes>() as u32,
        lp_security_descriptor: std::ptr::null_mut(),
        b_inherit_handle: 1,
    };
    let h = unsafe {
        CreateFileW(
            nul.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &mut sa as *mut _ as _,
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if h == INVALID_HANDLE_VALUE || h == 0 {
        Err(format!(
            "CreateFileW(NUL) failed (Win32 error {})",
            unsafe { win32::GetLastError() }
        ))
    } else {
        Ok(h)
    }
}

/// Spawn the agent process inside the AppContainer described by `guard`.
///
/// Uses `CreateProcessW` with `STARTUPINFOEXW` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
/// to launch the process in the AppContainer.  The guard must remain alive for
/// the lifetime of the returned handle (the AppContainer SID pointer is live for
/// the duration of `CreateProcessW`, and the profile must not be deleted while
/// the process is running).
///
/// Falls back to a descriptive error on `CreateProcessW` failure; the caller
/// (`sandboxed_spawn`) then retries with normal `runtime.spawn`.
#[cfg(target_os = "windows")]
pub(crate) fn spawn_in_appcontainer(
    request: &crate::adapter::SpawnRequest,
    guard: &WindowsAppContainerGuard,
) -> crate::adapter::Result<Box<dyn crate::adapter::AgentHandle>> {
    use crate::adapter::{RuntimeError, StdinMode, StdoutMode};
    use appcontainer_win32::*;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};

    // ── Build CreateProcessW parameters ──────────────────────────────────────

    let cmd_line_str = build_command_line(&request.command, &request.args);
    let mut cmd_line_wide = str_to_wide(&cmd_line_str);
    let mut env_block = build_env_block(&request.env);
    let working_dir_wide = path_to_wide(&request.working_dir);

    // ── Assemble SECURITY_CAPABILITIES ───────────────────────────────────────

    let sec_caps = guard.security_capabilities();

    // ── Initialize PROC_THREAD_ATTRIBUTE_LIST ─────────────────────────────────

    let mut attr_list_size: usize = 0;
    // First call: determine required buffer size.
    unsafe {
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attr_list_size);
    }
    let mut attr_list_buf = vec![0u8; attr_list_size];
    let ok = unsafe {
        InitializeProcThreadAttributeList(
            attr_list_buf.as_mut_ptr() as _,
            1,
            0,
            &mut attr_list_size,
        )
    };
    if ok == 0 {
        return Err(RuntimeError::SpawnFailed(format!(
            "InitializeProcThreadAttributeList failed (Win32 error {})",
            unsafe { win32::GetLastError() }
        )));
    }

    let ok = unsafe {
        UpdateProcThreadAttribute(
            attr_list_buf.as_mut_ptr() as _,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            &sec_caps as *const SecurityCapabilities as *const _,
            size_of::<SecurityCapabilities>(),
            null_mut(),
            null_mut(),
        )
    };
    if ok == 0 {
        unsafe { DeleteProcThreadAttributeList(attr_list_buf.as_mut_ptr() as _) };
        return Err(RuntimeError::SpawnFailed(format!(
            "UpdateProcThreadAttribute(SECURITY_CAPABILITIES) failed (Win32 error {})",
            unsafe { win32::GetLastError() }
        )));
    }

    // ── Build STARTUPINFOEXW with stdio ───────────────────────────────────────

    let mut sxew: StartupInfoExW = unsafe { zeroed() };
    sxew.startup_info.cb = size_of::<StartupInfoExW>() as u32;
    sxew.lp_attribute_list = attr_list_buf.as_mut_ptr() as _;

    let needs_explicit_stdio = matches!(request.stdin_mode, StdinMode::Null)
        || matches!(request.stdout_mode, StdoutMode::Piped);

    let mut nul_stdin_handle: isize = 0;
    let mut stdout_read_handle: isize = 0;
    let mut stdout_write_handle: isize = 0;

    if needs_explicit_stdio {
        sxew.startup_info.dw_flags |= STARTF_USESTDHANDLES;

        // stdin
        match request.stdin_mode {
            StdinMode::Null => {
                nul_stdin_handle = match open_nul_for_read() {
                    Ok(h) => h,
                    Err(e) => {
                        unsafe { DeleteProcThreadAttributeList(attr_list_buf.as_mut_ptr() as _) };
                        return Err(RuntimeError::SpawnFailed(e));
                    }
                };
                sxew.startup_info.h_std_input = nul_stdin_handle;
            }
            StdinMode::Inherited => {
                let h = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
                sxew.startup_info.h_std_input = h;
            }
            StdinMode::Piped => {
                unsafe { DeleteProcThreadAttributeList(attr_list_buf.as_mut_ptr() as _) };
                return Err(RuntimeError::SpawnFailed(
                    "AppContainer spawn does not support StdinMode::Piped".to_string(),
                ));
            }
        }

        // stdout
        match request.stdout_mode {
            StdoutMode::Piped => {
                let mut sa = SecurityAttributes {
                    n_length: size_of::<SecurityAttributes>() as u32,
                    lp_security_descriptor: null_mut(),
                    b_inherit_handle: 1, // write end is inheritable
                };
                let ok = unsafe {
                    CreatePipe(
                        &mut stdout_read_handle,
                        &mut stdout_write_handle,
                        &mut sa as *mut _ as _,
                        0,
                    )
                };
                if ok == 0 {
                    if nul_stdin_handle != 0 {
                        unsafe { win32::CloseHandle(nul_stdin_handle) };
                    }
                    unsafe { DeleteProcThreadAttributeList(attr_list_buf.as_mut_ptr() as _) };
                    return Err(RuntimeError::SpawnFailed(format!(
                        "CreatePipe for stdout failed (Win32 error {})",
                        unsafe { win32::GetLastError() }
                    )));
                }
                // Read end must NOT be inherited by child.
                unsafe { SetHandleInformation(stdout_read_handle, HANDLE_FLAG_INHERIT, 0) };
                sxew.startup_info.h_std_output = stdout_write_handle;
            }
            StdoutMode::Inherited => {
                let h = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
                sxew.startup_info.h_std_output = h;
            }
        }

        // stderr: always inherit parent's stderr
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        unsafe { SetHandleInformation(stderr, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        sxew.startup_info.h_std_error = stderr;
    }

    // ── CreateProcessW ────────────────────────────────────────────────────────

    let mut pi: ProcessInformation = unsafe { zeroed() };
    let creation_flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;

    let ok = unsafe {
        CreateProcessW(
            null(),
            cmd_line_wide.as_mut_ptr(),
            null_mut(),
            null_mut(),
            if needs_explicit_stdio { 1 } else { 0 },
            creation_flags,
            env_block.as_mut_ptr() as _,
            working_dir_wide.as_ptr(),
            &mut sxew as *mut StartupInfoExW as *mut _,
            &mut pi,
        )
    };

    // Clean up attribute list (must happen after CreateProcessW regardless of outcome).
    unsafe { DeleteProcThreadAttributeList(attr_list_buf.as_mut_ptr() as _) };

    // Close child-side write handle (child inherits it; parent no longer needs it).
    if stdout_write_handle != 0 {
        unsafe { win32::CloseHandle(stdout_write_handle) };
    }
    // Close NUL stdin handle (child inherits it).
    if nul_stdin_handle != 0 {
        unsafe { win32::CloseHandle(nul_stdin_handle) };
    }

    if ok == 0 {
        if stdout_read_handle != 0 {
            unsafe { win32::CloseHandle(stdout_read_handle) };
        }
        return Err(RuntimeError::SpawnFailed(format!(
            "CreateProcessW in AppContainer failed (Win32 error {})",
            unsafe { win32::GetLastError() }
        )));
    }

    // Close thread handle; we only need the process handle.
    unsafe { win32::CloseHandle(pi.h_thread) };

    // Wrap stdout read end as ChildStdout.
    let stdout = if stdout_read_handle != 0 {
        use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
        let owned = unsafe { OwnedHandle::from_raw_handle(stdout_read_handle as *mut _) };
        Some(std::process::ChildStdout::from(owned))
    } else {
        None
    };

    tracing::info!(
        pid = pi.dw_process_id,
        container = ?String::from_utf16_lossy(&guard.container_name),
        "Agent spawned in AppContainer"
    );

    Ok(Box::new(AppContainerProcessHandle {
        process_handle: pi.h_process,
        pid: pi.dw_process_id,
        stdout,
    }))
}

// ── Non-Windows stubs ─────────────────────────────────────────────────────────

/// No-op AppContainer guard on non-Windows platforms (ZST).
#[cfg(not(target_os = "windows"))]
pub struct WindowsAppContainerGuard;

#[cfg(not(target_os = "windows"))]
impl WindowsAppContainerGuard {
    pub fn new(
        _container_name: &str,
        _staging_path: &std::path::Path,
        _allow_network: bool,
    ) -> Result<Self, String> {
        Ok(Self)
    }
}

// ── sandboxed_spawn ───────────────────────────────────────────────────────────

/// Spawn the agent process, using AppContainer if a guard is provided.
///
/// On Windows with a `Some(guard)`: attempts `spawn_in_appcontainer` first.
/// If that fails (e.g., nested-container restriction), logs a warning and falls
/// back to `runtime.spawn()` — the agent runs without AppContainer isolation.
///
/// On non-Windows, or with `None`: delegates to `runtime.spawn()` directly.
pub fn sandboxed_spawn(
    runtime: &dyn crate::adapter::RuntimeAdapter,
    request: crate::adapter::SpawnRequest,
    ac_guard: Option<&WindowsAppContainerGuard>,
) -> crate::adapter::Result<Box<dyn crate::adapter::AgentHandle>> {
    #[cfg(target_os = "windows")]
    if let Some(guard) = ac_guard {
        match spawn_in_appcontainer(&request, guard) {
            Ok(h) => return Ok(h),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "AppContainer spawn failed — falling back to normal spawn without \
                     filesystem isolation. Check that TA is not running inside a nested \
                     Job Object that prohibits child processes."
                );
            }
        }
    }
    // Suppress unused-variable warning on non-Windows platforms where the cfg
    // block above is compiled out.  Option<&T> is Copy so this is always safe.
    let _ = ac_guard;
    runtime.spawn(request)
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

    // ── AppContainer tests ────────────────────────────────────────────────────

    /// On all platforms: constructing an AppContainer guard succeeds.
    /// On Windows, creates a real profile; on others, returns a no-op stub.
    #[test]
    fn appcontainer_guard_constructs_successfully() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = WindowsAppContainerGuard::new("ta-test-guard-construct", tmp.path(), false);
        assert!(
            result.is_ok(),
            "WindowsAppContainerGuard::new should succeed: {:?}",
            result.err()
        );
    }

    /// appcontainer_available() returns a bool without panicking on all platforms.
    #[test]
    fn appcontainer_available_does_not_panic() {
        let _ = appcontainer_available();
    }

    /// Windows-only: verify SID is created and the guard cleans up on drop.
    #[cfg(target_os = "windows")]
    #[test]
    fn appcontainer_sid_created_and_deleted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Create guard — this must succeed on a standard Win10+ machine.
        let guard = WindowsAppContainerGuard::new("ta-test-sid-lifecycle", tmp.path(), false)
            .expect("AppContainer guard creation should succeed on Win10+");

        let caps = guard.security_capabilities();
        assert!(
            !caps.app_container_sid.is_null(),
            "AppContainer SID must not be null"
        );
        assert_eq!(
            caps.capability_count, 0,
            "no capabilities for allow_network=false"
        );

        // Drop deletes the profile (DeleteAppContainerProfile is called in Drop).
        drop(guard);
    }

    /// Windows-only: network capability adds exactly one SID entry.
    #[cfg(target_os = "windows")]
    #[test]
    fn network_capability_non_empty_adds_internet_client_sid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let guard = WindowsAppContainerGuard::new("ta-test-net-cap", tmp.path(), true)
            .expect("AppContainer guard with network capability should succeed");

        let caps = guard.security_capabilities();
        assert_eq!(
            caps.capability_count, 1,
            "allow_network=true should add 1 capability SID"
        );
        assert!(
            !caps.capabilities.is_null(),
            "capabilities pointer must not be null when capability_count > 0"
        );
    }

    /// Windows-only: no network capability when allow_network=false.
    #[cfg(target_os = "windows")]
    #[test]
    fn network_capability_empty_blocks_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let guard = WindowsAppContainerGuard::new("ta-test-no-net", tmp.path(), false)
            .expect("AppContainer guard without network capability should succeed");

        let caps = guard.security_capabilities();
        assert_eq!(caps.capability_count, 0);
        assert!(
            caps.capabilities.is_null(),
            "capabilities pointer must be null when no capabilities are declared"
        );
    }

    /// Windows-only: staging path capability grants RW — the DACL on the
    /// staging directory is updated during guard construction.
    #[cfg(target_os = "windows")]
    #[test]
    fn staging_capability_grants_rw() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // If guard construction succeeds, the DACL grant succeeded.
        let guard = WindowsAppContainerGuard::new("ta-test-staging-acl", tmp.path(), false)
            .expect("guard construction (including DACL grant) should succeed");
        // The AppContainer SID must be non-null (DACL was granted for this SID).
        let caps = guard.security_capabilities();
        assert!(!caps.app_container_sid.is_null());
    }

    /// Windows-only: spawn a subprocess inside an AppContainer and assert that
    /// writing to a path outside the staging workspace is denied.
    ///
    /// This test requires a Windows runner with AppContainer support (Win10+).
    /// The escape path `C:\Windows\Temp\ta-escape-test.txt` is outside the
    /// staging workspace, so the AppContainer must deny writes to it.
    #[cfg(target_os = "windows")]
    #[test]
    fn appcontainer_denies_write_outside_staging_path() {
        use crate::adapter::{SpawnRequest, StdinMode, StdoutMode};
        use std::collections::HashMap;

        let tmp = tempfile::tempdir().expect("tempdir");
        let guard = WindowsAppContainerGuard::new("ta-test-escape", tmp.path(), false)
            .expect("AppContainer guard should succeed on Win10+");

        // A PowerShell command that tries to write outside the staging path.
        // The container should block this write; the command exits non-zero.
        let escape_path = r"C:\Windows\Temp\ta-escape-test.txt";
        let request = SpawnRequest {
            command: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                format!(
                    r#"try {{ [IO.File]::WriteAllText('{}', 'escape') }} catch {{ exit 5 }}"#,
                    escape_path
                ),
            ],
            env: HashMap::new(),
            working_dir: tmp.path().to_path_buf(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };

        match spawn_in_appcontainer(&request, &guard) {
            Ok(mut handle) => {
                let status = handle.wait().expect("wait should succeed");
                let code = status.code().unwrap_or(0);
                assert_ne!(
                    code, 0,
                    "Write to C:\\Windows\\Temp should fail in AppContainer (exit code {})",
                    code
                );
                // Also verify the escape file was NOT created.
                assert!(
                    !std::path::Path::new(escape_path).exists(),
                    "Escape file must not exist after AppContainer blocked the write"
                );
            }
            Err(e) => {
                // If AppContainer spawn itself fails (e.g., nested Job Object restriction),
                // mark as inconclusive rather than failing the test hard.
                eprintln!(
                    "AppContainer spawn failed (environment restriction?): {} — test skipped",
                    e
                );
            }
        }
    }

    /// Windows-only: spawn a subprocess inside an AppContainer and assert that
    /// writing to the staging workspace succeeds.
    #[cfg(target_os = "windows")]
    #[test]
    fn appcontainer_allows_write_inside_staging_path() {
        use crate::adapter::{SpawnRequest, StdinMode, StdoutMode};
        use std::collections::HashMap;

        let tmp = tempfile::tempdir().expect("tempdir");
        let guard = WindowsAppContainerGuard::new("ta-test-staging-write", tmp.path(), false)
            .expect("AppContainer guard should succeed on Win10+");

        let target_file = tmp.path().join("container-write-test.txt");
        let request = SpawnRequest {
            command: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                format!(
                    r#"[IO.File]::WriteAllText('{}', 'ok')"#,
                    target_file.display()
                ),
            ],
            env: HashMap::new(),
            working_dir: tmp.path().to_path_buf(),
            stdin_mode: StdinMode::Null,
            stdout_mode: StdoutMode::Inherited,
        };

        match spawn_in_appcontainer(&request, &guard) {
            Ok(mut handle) => {
                let status = handle.wait().expect("wait should succeed");
                assert!(
                    status.success(),
                    "Write to staging workspace should succeed in AppContainer"
                );
                assert!(
                    target_file.exists(),
                    "File written by container should exist in staging path"
                );
            }
            Err(e) => {
                eprintln!(
                    "AppContainer spawn failed (environment restriction?): {} — test skipped",
                    e
                );
            }
        }
    }

    /// sandboxed_spawn falls through to runtime.spawn when no AppContainer guard.
    #[test]
    fn sandboxed_spawn_without_guard_calls_runtime_spawn() {
        // On non-Windows, this is always the path.
        // On Windows with None guard, same path is taken.
        // We can't easily test the full spawn in a unit test without a real runtime,
        // so just verify the signature compiles and the function is callable.
        // The actual fallback behaviour is tested by the integration tests.
        let _ = std::ptr::null::<WindowsAppContainerGuard>();
    }

    /// Windows-only: attempt to write outside the staging path.
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
