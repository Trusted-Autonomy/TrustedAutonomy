// windows_pipe.rs — security attributes for host-side named pipes (v0.17.12.9)
//
// The gateway pipe and the egress relay pipe are created by `ta` (outside the
// container) with a protected DACL from `windows_policy::pipe_sddl`: the owner
// and this goal's container SID only. This module turns that SDDL string into
// the SECURITY_ATTRIBUTES a pipe server needs, and reads the owner's SID.
//
// Everything here is Windows FFI and is compiled out elsewhere. The SDDL text
// itself is built and tested in `windows_policy` on every platform.

#![cfg(target_os = "windows")]

use std::ffi::c_void;
use std::ptr::null_mut;

#[repr(C)]
struct SecurityAttributes {
    n_length: u32,
    lp_security_descriptor: *mut c_void,
    b_inherit_handle: i32,
}

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

#[repr(C)]
struct TokenUser {
    user: SidAndAttributes,
}

const SDDL_REVISION_1: u32 = 1;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER_CLASS: u32 = 1;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_sd: *const u16,
        revision: u32,
        sd: *mut *mut c_void,
        sd_size: *mut u32,
    ) -> i32;
    fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
    fn GetTokenInformation(
        token: isize,
        class: u32,
        info: *mut c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, out: *mut *mut u16) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn CloseHandle(h: isize) -> i32;
    fn GetLastError() -> u32;
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

/// A security descriptor built from SDDL, usable as the `SECURITY_ATTRIBUTES`
/// of a named pipe instance.
pub struct PipeSecurity {
    descriptor: *mut c_void,
    attributes: SecurityAttributes,
}

// The descriptor is immutable after construction and owned by this value.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// Build from an SDDL string such as `windows_policy::pipe_sddl` returns.
    pub fn from_sddl(sddl: &str) -> Result<Self, String> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd: *mut c_void = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(format!(
                "could not build the named-pipe security descriptor from {sddl:?} (Win32 error \
                 {code}). The pipe was not created, so nothing is exposed; report this with the \
                 SDDL text."
            ));
        }
        Ok(Self {
            descriptor: sd,
            attributes: SecurityAttributes {
                n_length: std::mem::size_of::<SecurityAttributes>() as u32,
                lp_security_descriptor: sd,
                b_inherit_handle: 0,
            },
        })
    }

    /// Pointer for tokio's `ServerOptions::create_with_security_attributes_raw`.
    /// Valid while `self` is alive and not moved.
    pub fn as_raw(&mut self) -> *mut c_void {
        &mut self.attributes as *mut SecurityAttributes as *mut c_void
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe { LocalFree(self.descriptor) };
        }
    }
}

/// The SID of the user running `ta`, as `S-1-5-21-...`. This is the pipe owner
/// that keeps access when the DACL is protected.
pub fn current_user_sid_string() -> Result<String, String> {
    unsafe {
        let mut token: isize = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(format!(
                "OpenProcessToken failed (Win32 error {}) while reading the current user SID",
                GetLastError()
            ));
        }
        let result = (|| {
            let mut len = 0u32;
            GetTokenInformation(token, TOKEN_USER_CLASS, null_mut(), 0, &mut len);
            if len == 0 || GetLastError() != ERROR_INSUFFICIENT_BUFFER {
                return Err(format!(
                    "GetTokenInformation could not size TokenUser (Win32 error {})",
                    GetLastError()
                ));
            }
            // u64 storage keeps the buffer 8-byte aligned for TOKEN_USER.
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            if GetTokenInformation(
                token,
                TOKEN_USER_CLASS,
                buf.as_mut_ptr() as *mut c_void,
                len,
                &mut len,
            ) == 0
            {
                return Err(format!(
                    "GetTokenInformation(TokenUser) failed (Win32 error {})",
                    GetLastError()
                ));
            }
            let user = &*(buf.as_ptr() as *const TokenUser);
            let mut text: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.user.sid, &mut text) == 0 {
                return Err(format!(
                    "ConvertSidToStringSidW failed (Win32 error {})",
                    GetLastError()
                ));
            }
            let mut n = 0;
            while *text.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text as *mut c_void);
            Ok(s)
        })();
        CloseHandle(token);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_sid_is_a_valid_sid() {
        let sid = current_user_sid_string().unwrap();
        assert!(crate::windows_policy::is_valid_sid(&sid), "{sid}");
    }

    #[test]
    fn sddl_from_policy_parses() {
        let owner = current_user_sid_string().unwrap();
        let sddl = crate::windows_policy::pipe_sddl("S-1-15-2-1-2-3-4-5-6-7", &owner).unwrap();
        assert!(PipeSecurity::from_sddl(&sddl).is_ok());
    }

    #[test]
    fn malformed_sddl_is_an_actionable_error() {
        let e = PipeSecurity::from_sddl("not sddl").err().unwrap();
        assert!(e.contains("security descriptor"), "{e}");
    }
}
