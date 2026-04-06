//! Windows optional-feature detection helpers.
//!
//! Provides runtime detection of Windows platform features needed by TA staging.
//! Currently detects the Windows Projected File System (ProjFS, `Client-ProjFS`)
//! optional feature introduced in Windows 10 version 1809.
//!
//! All detection functions are safe to call on non-Windows platforms — they
//! return `false` unconditionally.

/// Check whether the Windows Projected File System (ProjFS) feature is
/// available and enabled on the current machine.
///
/// Detection strategy (Windows only):
/// 1. Try to load `ProjectedFSLib.dll` — present only when `Client-ProjFS` is
///    enabled. If `LoadLibraryW` succeeds the feature is enabled.
/// 2. If the DLL load fails, fall back to a registry probe: check
///    `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\Packages`
///    for a package whose name contains `Client-ProjFS` with `CurrentState == 112`
///    (the "installed" sentinel). This covers edge cases where the DLL path
///    changes across Windows builds.
///
/// On non-Windows platforms always returns `false`.
pub fn is_projfs_available() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows_check()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
fn windows_check() -> bool {
    // Primary check: try to load ProjectedFSLib.dll.
    // The DLL is present in System32 only when Client-ProjFS is installed.
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::LoadLibraryW;

    let dll_name: Vec<u16> = "ProjectedFSLib.dll\0".encode_utf16().collect();

    // SAFETY: dll_name is a valid null-terminated wide string. LoadLibraryW
    // is safe to call with a well-formed string — it returns null on failure.
    let hmod = unsafe { LoadLibraryW(PCWSTR(dll_name.as_ptr())) };

    let dll_present = match hmod {
        Ok(h) if !h.is_invalid() => {
            // Intentionally do not call FreeLibrary — this is a one-time
            // feature-detection call at startup; the handle is released when
            // the process exits.
            let _ = h;
            true
        }
        _ => false,
    };

    if dll_present {
        tracing::debug!("ProjFS available: ProjectedFSLib.dll loaded successfully");
        return true;
    }

    // Fallback: registry probe under CBS Packages.
    if registry_probe_projfs() {
        tracing::debug!("ProjFS available: registry probe found Client-ProjFS installed");
        return true;
    }

    tracing::info!(
        "ProjFS not available: ProjectedFSLib.dll absent and registry probe negative; \
         enable with: Dism.exe /Online /Enable-Feature /FeatureName:Client-ProjFS /NoRestart"
    );
    false
}

/// Registry fallback: scan CBS packages for Client-ProjFS with CurrentState=112.
#[cfg(target_os = "windows")]
fn registry_probe_projfs() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE,
        KEY_READ, REG_DWORD, REG_VALUE_TYPE,
    };

    let packages_path: Vec<u16> = concat!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\",
        "Component Based Servicing\\Packages\0"
    )
    .encode_utf16()
    .collect();

    let mut hpackages = HKEY::default();
    // SAFETY: standard registry open — key path is a valid wide string.
    let open_res = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(packages_path.as_ptr()),
            0,
            KEY_READ,
            &mut hpackages,
        )
    };
    if open_res.is_err() {
        return false;
    }

    let mut index = 0u32;
    let mut found = false;

    loop {
        let mut name_buf = vec![0u16; 256];
        let mut name_len = name_buf.len() as u32;
        // SAFETY: name_buf is sized and name_len reflects that size.
        let enum_res = unsafe {
            RegEnumKeyExW(
                hpackages,
                index,
                windows::core::PWSTR(name_buf.as_mut_ptr()),
                &mut name_len,
                None,
                windows::core::PWSTR::null(),
                None,
                None,
            )
        };

        if enum_res.is_err() {
            break; // No more keys.
        }

        let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        if name.contains("Client-ProjFS") {
            // Check CurrentState == 112 (installed).
            let mut sub_hkey = HKEY::default();
            let sub_path: Vec<u16> = format!("{}\0", name).encode_utf16().collect();
            // SAFETY: sub_path is a valid wide string derived from enumerated key name.
            let sub_res = unsafe {
                RegOpenKeyExW(
                    hpackages,
                    PCWSTR(sub_path.as_ptr()),
                    0,
                    KEY_READ,
                    &mut sub_hkey,
                )
            };
            if sub_res.is_ok() {
                let value_name: Vec<u16> = "CurrentState\0".encode_utf16().collect();
                let mut data = 0u32;
                let mut data_size = std::mem::size_of::<u32>() as u32;
                let mut reg_type = REG_VALUE_TYPE(0);
                // SAFETY: data is valid u32 storage for a REG_DWORD value.
                let query_res = unsafe {
                    RegQueryValueExW(
                        sub_hkey,
                        PCWSTR(value_name.as_ptr()),
                        None,
                        Some(&mut reg_type),
                        Some(&mut data as *mut u32 as *mut u8),
                        Some(&mut data_size),
                    )
                };
                // SAFETY: sub_hkey is a valid open key.
                unsafe {
                    let _ = RegCloseKey(sub_hkey);
                }
                if query_res.is_ok() && reg_type == REG_DWORD && data == 112 {
                    found = true;
                    break;
                }
            }
        }

        index += 1;
    }

    // SAFETY: hpackages is a valid open key.
    unsafe {
        let _ = RegCloseKey(hpackages);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_windows_returns_false() {
        // On the CI platforms (macOS/Linux) is_projfs_available must always be
        // false — the feature does not exist outside Windows.
        #[cfg(not(target_os = "windows"))]
        assert!(!is_projfs_available());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_check_does_not_panic() {
        // Just verify it runs without panicking and returns a bool.
        let _ = is_projfs_available();
    }
}
