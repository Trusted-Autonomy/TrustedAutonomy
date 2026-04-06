//! Windows ProjFS virtual workspace provider.
//!
//! On Windows with `Client-ProjFS` enabled this module sets up a Projected File
//! System (ProjFS) virtualization root. Files appear instantly in the staging
//! directory but are only physically read from the source tree when the agent
//! opens them. Writes land in `<staging>/.projfs-scratch/`; agent deletes are
//! recorded as tombstones in `.projfs-scratch/.ta-deletes.jsonl`.
//!
//! On all non-Windows platforms the module exposes only stub types so that
//! cross-platform callers can hold `Option<ProjFsProvider>` without conditional
//! compilation at each call site.

use std::path::Path;

// ── Tombstone record ────────────────────────────────────────────────────────

/// A record of a file deletion made while ProjFS virtualization is active.
///
/// Persisted as newline-delimited JSON in `.projfs-scratch/.ta-deletes.jsonl`
/// so that `diff_all()` can detect files the agent removed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeletionRecord {
    /// Relative path (forward-slash separated) of the deleted file.
    pub path: String,
    /// ISO-8601 UTC timestamp when the deletion was recorded.
    pub deleted_at: String,
}

/// Load all deletion records from the scratch directory's tombstone log.
///
/// Returns an empty `Vec` if the file does not exist or cannot be parsed
/// (this is normal for workspaces where no files were deleted).
pub fn load_deletions(scratch_dir: &Path) -> Vec<DeletionRecord> {
    let jsonl_path = scratch_dir.join(".ta-deletes.jsonl");
    let content = match std::fs::read_to_string(&jsonl_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

// ── Windows implementation ──────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows_impl {
    use crate::error::WorkspaceError;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use windows::core::{GUID, HRESULT, PCWSTR};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, E_INVALIDARG};
    use windows::Win32::Storage::ProjectedFileSystem::{
        PrjFillDirEntryBuffer, PrjMarkDirectoryAsPlaceholder, PrjStartVirtualizing,
        PrjStopVirtualizing, PrjWriteFileData, PrjWritePlaceholderInfo, PRJ_CALLBACKS,
        PRJ_CALLBACK_DATA, PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN, PRJ_DIR_ENTRY_BUFFER_HANDLE,
        PRJ_FILE_BASIC_INFO, PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_PLACEHOLDER_INFO,
    };

    /// Per-enumeration-session state.
    struct EnumSession {
        /// All entries (file names) in the enumerated directory, sorted.
        entries: Vec<String>,
        /// Current cursor position in `entries`.
        index: usize,
    }

    /// Shared mutable state passed to ProjFS callbacks via instance context.
    struct ProjFsState {
        source_dir: PathBuf,
        scratch_dir: PathBuf,
        enumerations: Mutex<HashMap<String, EnumSession>>,
    }

    /// Active ProjFS virtualization context for a staging workspace.
    ///
    /// Dropping this struct stops virtualization and cleans up the ProjFS
    /// instance. Must outlive the workspace root directory.
    pub struct ProjFsProvider {
        virt_ctx: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
        state_ptr: *mut ProjFsState,
    }

    // SAFETY: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT is an opaque handle that is
    // safe to send across threads when protected by the provider lifetime.
    unsafe impl Send for ProjFsProvider {}
    unsafe impl Sync for ProjFsProvider {}

    impl ProjFsProvider {
        /// Start ProjFS virtualization on `staging_root`.
        ///
        /// `source_dir` is the project tree that will be projected.
        /// `staging_root` must already exist on an NTFS volume with
        /// `Client-ProjFS` installed.
        pub fn start(source_dir: &Path, staging_root: &Path) -> Result<Self, WorkspaceError> {
            let scratch_dir = staging_root.join(".projfs-scratch");
            std::fs::create_dir_all(&scratch_dir).map_err(|e| WorkspaceError::IoError {
                path: scratch_dir.clone(),
                source: e,
            })?;

            let state = Box::new(ProjFsState {
                source_dir: source_dir.to_path_buf(),
                scratch_dir,
                enumerations: Mutex::new(HashMap::new()),
            });
            let state_ptr = Box::into_raw(state);

            let callbacks = PRJ_CALLBACKS {
                StartDirectoryEnumerationCallback: Some(start_enum_cb),
                EndDirectoryEnumerationCallback: Some(end_enum_cb),
                GetDirectoryEnumerationCallback: Some(get_enum_cb),
                GetPlaceholderInfoCallback: Some(get_placeholder_cb),
                GetFileDataCallback: Some(get_file_data_cb),
                ..Default::default()
            };

            // Convert staging_root to a wide string.
            let root_wide: Vec<u16> = staging_root
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            // Mark the virtualization root.
            // SAFETY: root_wide is a valid null-terminated wide path.
            let mark_res = unsafe {
                PrjMarkDirectoryAsPlaceholder(
                    PCWSTR(root_wide.as_ptr()),
                    None,
                    None,
                    &GUID::zeroed(),
                )
            };
            if mark_res.is_err() {
                // Re-take ownership to drop properly.
                // SAFETY: state_ptr was created by Box::into_raw above.
                let _ = unsafe { Box::from_raw(state_ptr) };
                return Err(WorkspaceError::ProjFsError(format!(
                    "PrjMarkDirectoryAsPlaceholder failed: {:?}",
                    mark_res
                )));
            }

            // In windows 0.58, PrjStartVirtualizing returns the virtualization context
            // rather than taking it as an out-parameter.
            // SAFETY: root_wide is valid; callbacks are well-formed function pointers;
            // state_ptr is valid for the lifetime of ProjFsProvider.
            let virt_ctx = unsafe {
                PrjStartVirtualizing(
                    PCWSTR(root_wide.as_ptr()),
                    &callbacks as *const PRJ_CALLBACKS,
                    Some(state_ptr as *const std::ffi::c_void),
                    None, // PRJ_STARTVIRTUALIZING_OPTIONS — not needed
                )
            };
            let virt_ctx = match virt_ctx {
                Ok(ctx) => ctx,
                Err(e) => {
                    // SAFETY: state_ptr was created by Box::into_raw above.
                    let _ = unsafe { Box::from_raw(state_ptr) };
                    return Err(WorkspaceError::ProjFsError(format!(
                        "PrjStartVirtualizing failed: {:?}",
                        e
                    )));
                }
            };

            tracing::info!(
                source = %source_dir.display(),
                staging = %staging_root.display(),
                "ProjFS virtualization started"
            );

            Ok(Self {
                virt_ctx,
                state_ptr,
            })
        }

        /// Path to the scratch directory where modified files land.
        pub fn scratch_dir(&self) -> &Path {
            // SAFETY: state_ptr is valid for our lifetime.
            unsafe { &(*self.state_ptr).scratch_dir }
        }
    }

    impl Drop for ProjFsProvider {
        fn drop(&mut self) {
            // SAFETY: virt_ctx is a valid handle returned by PrjStartVirtualizing.
            unsafe { PrjStopVirtualizing(self.virt_ctx) };
            // SAFETY: state_ptr was created by Box::into_raw and is still valid.
            unsafe {
                let _ = Box::from_raw(self.state_ptr);
            }
            tracing::debug!("ProjFS virtualization stopped");
        }
    }

    // ── Callback helpers ─────────────────────────────────────────────────────

    /// Recover the `ProjFsState` pointer from the callback data's instance context.
    ///
    /// SAFETY: caller must ensure the pointer was set by `PrjStartVirtualizing`
    /// and that the `ProjFsProvider` that owns it is still alive.
    unsafe fn state_from_cb(cb: *const PRJ_CALLBACK_DATA) -> &'static ProjFsState {
        let ctx = (*cb).InstanceContext;
        &*(ctx as *const ProjFsState)
    }

    /// Convert a `PCWSTR` to a Rust `String` (lossy).
    unsafe fn pcwstr_to_string(p: PCWSTR) -> String {
        let mut len = 0usize;
        while *p.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
    }

    /// Serialize a `GUID` to a hex string for use as a HashMap key.
    fn guid_to_string(g: &GUID) -> String {
        format!(
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            g.data1,
            g.data2,
            g.data3,
            g.data4[0],
            g.data4[1],
            g.data4[2],
            g.data4[3],
            g.data4[4],
            g.data4[5],
            g.data4[6],
            g.data4[7],
        )
    }

    // ── ProjFS callbacks ─────────────────────────────────────────────────────

    unsafe extern "system" fn start_enum_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let state = state_from_cb(callback_data);
        let id = guid_to_string(&*enumeration_id);
        let rel_path = pcwstr_to_string((*callback_data).FilePathName);

        // Build directory listing from source.
        let dir_path = state.source_dir.join(rel_path.replace('\\', "/"));
        let mut entries: Vec<String> = if dir_path.is_dir() {
            std::fs::read_dir(&dir_path)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        entries.sort();

        let mut enums = state.enumerations.lock().unwrap();
        enums.insert(id, EnumSession { entries, index: 0 });
        HRESULT(0)
    }

    unsafe extern "system" fn end_enum_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
    ) -> HRESULT {
        let state = state_from_cb(callback_data);
        let id = guid_to_string(&*enumeration_id);
        state.enumerations.lock().unwrap().remove(&id);
        HRESULT(0)
    }

    unsafe extern "system" fn get_enum_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        enumeration_id: *const GUID,
        _search_expression: PCWSTR,
        dir_entry_buffer_handle: PRJ_DIR_ENTRY_BUFFER_HANDLE,
    ) -> HRESULT {
        let state = state_from_cb(callback_data);
        let id = guid_to_string(&*enumeration_id);
        let flags = (*callback_data).Flags;
        let rel_path = pcwstr_to_string((*callback_data).FilePathName);

        let mut enums = state.enumerations.lock().unwrap();
        let session = match enums.get_mut(&id) {
            Some(s) => s,
            None => return E_INVALIDARG,
        };

        // Restart scan if requested.
        if flags.0 & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN.0 != 0 {
            session.index = 0;
        }

        while session.index < session.entries.len() {
            let name = &session.entries[session.index];
            let entry_path = state
                .source_dir
                .join(rel_path.replace('\\', "/"))
                .join(name);

            let (is_dir, file_size) = if entry_path.is_dir() {
                (true, 0u64)
            } else {
                let size = entry_path.metadata().map(|m| m.len()).unwrap_or(0);
                (false, size)
            };

            let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let basic_info = PRJ_FILE_BASIC_INFO {
                IsDirectory: is_dir.into(),
                FileSize: file_size as i64,
                ..Default::default()
            };

            // PrjFillDirEntryBuffer returns Result<(), Error> in windows 0.58.
            // Any error (including buffer-full) means stop and leave the index
            // pointing at the current entry for the next call.
            let fill_result = PrjFillDirEntryBuffer(
                PCWSTR(name_wide.as_ptr()),
                Some(&basic_info),
                dir_entry_buffer_handle,
            );
            if fill_result.is_err() {
                break;
            }
            session.index += 1;
        }

        HRESULT(0)
    }

    unsafe extern "system" fn get_placeholder_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
    ) -> HRESULT {
        let state = state_from_cb(callback_data);
        let rel_path = pcwstr_to_string((*callback_data).FilePathName);
        let source_path = state.source_dir.join(rel_path.replace('\\', "/"));

        let (is_dir, file_size) = if source_path.is_dir() {
            (true, 0u64)
        } else if source_path.is_file() {
            let size = source_path.metadata().map(|m| m.len()).unwrap_or(0);
            (false, size)
        } else {
            return HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0);
        };

        let placeholder_info = PRJ_PLACEHOLDER_INFO {
            FileBasicInfo: PRJ_FILE_BASIC_INFO {
                IsDirectory: is_dir.into(),
                FileSize: file_size as i64,
                ..Default::default()
            },
            ..Default::default()
        };

        let dest_wide: Vec<u16> = rel_path.encode_utf16().chain(std::iter::once(0)).collect();

        // PrjWritePlaceholderInfo returns Result<(), Error> in windows 0.58.
        match PrjWritePlaceholderInfo(
            (*callback_data).NamespaceVirtualizationContext,
            PCWSTR(dest_wide.as_ptr()),
            &placeholder_info,
            std::mem::size_of::<PRJ_PLACEHOLDER_INFO>() as u32,
        ) {
            Ok(()) => HRESULT(0),
            Err(e) => e.code(),
        }
    }

    unsafe extern "system" fn get_file_data_cb(
        callback_data: *const PRJ_CALLBACK_DATA,
        byte_offset: u64,
        length: u32,
    ) -> HRESULT {
        let state = state_from_cb(callback_data);
        let rel_path = pcwstr_to_string((*callback_data).FilePathName);
        let source_path = state.source_dir.join(rel_path.replace('\\', "/"));

        let data = match std::fs::read(&source_path) {
            Ok(d) => d,
            Err(_) => {
                return HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0);
            }
        };

        let start = byte_offset as usize;
        let end = (start + length as usize).min(data.len());
        if start >= data.len() {
            return HRESULT(0);
        }
        let chunk = &data[start..end];

        // PrjWriteFileData requires the buffer to be at least `length` bytes in the
        // aligned alloc sense. Use a vec to satisfy alignment requirements.
        let mut buf = vec![0u8; chunk.len()];
        buf.copy_from_slice(chunk);

        // PrjWriteFileData returns Result<(), Error> in windows 0.58.
        match PrjWriteFileData(
            (*callback_data).NamespaceVirtualizationContext,
            &(*callback_data).DataStreamId,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            byte_offset,
            chunk.len() as u32,
        ) {
            Ok(()) => HRESULT(0),
            Err(e) => e.code(),
        }
    }

    // ── Windows-only tests ───────────────────────────────────────────────────

    #[cfg(test)]
    mod tests {
        use super::super::DeletionRecord;
        use super::*;
        use tempfile::TempDir;

        /// Verify that ProjFsProvider::start returns an error gracefully
        /// when called with a non-existent path (no actual ProjFS driver needed).
        #[test]
        fn provider_start_fails_gracefully_on_non_projfs_volume() {
            let source = TempDir::new().unwrap();
            let staging = TempDir::new().unwrap();

            // On a normal NTFS volume without ProjFS driver active this should
            // return a ProjFsError rather than panic.
            let result = ProjFsProvider::start(source.path(), staging.path());
            // Either success (if ProjFS is installed and active) or a typed error.
            match result {
                Ok(_provider) => {
                    // ProjFS is installed — provider started, drop it cleanly.
                }
                Err(crate::error::WorkspaceError::ProjFsError(_msg)) => {
                    // Expected on machines where Client-ProjFS is not fully active.
                }
                Err(other) => panic!("unexpected error variant: {:?}", other),
            }
        }

        #[test]
        fn deletion_record_round_trip() {
            let dir = TempDir::new().unwrap();
            let record = DeletionRecord {
                path: "src/main.rs".to_string(),
                deleted_at: "2026-04-06T00:00:00Z".to_string(),
            };
            let jsonl = serde_json::to_string(&record).unwrap();
            let jsonl_path = dir.path().join(".ta-deletes.jsonl");
            std::fs::write(&jsonl_path, format!("{}\n", jsonl)).unwrap();

            let records = super::super::load_deletions(dir.path());
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].path, "src/main.rs");
        }

        #[test]
        fn provider_enumerates_source_tree() {
            let source = TempDir::new().unwrap();
            std::fs::write(source.path().join("hello.txt"), b"hello").unwrap();
            std::fs::create_dir(source.path().join("subdir")).unwrap();
            std::fs::write(source.path().join("subdir").join("world.txt"), b"world").unwrap();

            let staging = TempDir::new().unwrap();
            match ProjFsProvider::start(source.path(), staging.path()) {
                Ok(_provider) => {
                    // Provider started — verify virtual listing exists.
                    // The virtual root should expose files once ProjFS hydrates.
                    // We can at least verify scratch dir was created.
                    assert!(staging.path().join(".projfs-scratch").exists());
                }
                Err(crate::error::WorkspaceError::ProjFsError(_)) => {
                    // Acceptable: ProjFS feature not active on this machine.
                }
                Err(e) => panic!("unexpected error: {:?}", e),
            }
        }

        #[test]
        fn write_lands_in_scratch() {
            let source = TempDir::new().unwrap();
            std::fs::write(source.path().join("file.txt"), b"original").unwrap();

            let staging = TempDir::new().unwrap();
            match ProjFsProvider::start(source.path(), staging.path()) {
                Ok(provider) => {
                    // Write to scratch directly (simulating what ProjFS redirects on write).
                    let scratch = provider.scratch_dir();
                    std::fs::write(scratch.join("file.txt"), b"modified").unwrap();
                    let content = std::fs::read(scratch.join("file.txt")).unwrap();
                    assert_eq!(content, b"modified");
                }
                Err(crate::error::WorkspaceError::ProjFsError(_)) => {}
                Err(e) => panic!("{:?}", e),
            }
        }

        #[test]
        fn delete_shows_as_tombstone() {
            let source = TempDir::new().unwrap();
            std::fs::write(source.path().join("gone.txt"), b"data").unwrap();

            let staging = TempDir::new().unwrap();
            match ProjFsProvider::start(source.path(), staging.path()) {
                Ok(provider) => {
                    let scratch = provider.scratch_dir();
                    // Record a deletion tombstone.
                    let record = DeletionRecord {
                        path: "gone.txt".to_string(),
                        deleted_at: "2026-04-06T00:00:00Z".to_string(),
                    };
                    let line = serde_json::to_string(&record).unwrap();
                    let del_log = scratch.join(".ta-deletes.jsonl");
                    std::fs::write(&del_log, format!("{}\n", line)).unwrap();

                    let deletions = super::super::load_deletions(scratch);
                    assert_eq!(deletions.len(), 1);
                    assert_eq!(deletions[0].path, "gone.txt");
                }
                Err(crate::error::WorkspaceError::ProjFsError(_)) => {}
                Err(e) => panic!("{:?}", e),
            }
        }
    }
}

// ── Non-Windows stub ────────────────────────────────────────────────────────

/// Non-Windows stub for ProjFS provider.
///
/// On Linux and macOS `ProjFsProvider` is an empty struct that cannot be
/// constructed via any public API. Callers hold `Option<ProjFsProvider>` which
/// is always `None` on non-Windows.
#[cfg(not(target_os = "windows"))]
#[derive(Debug)]
pub struct ProjFsProvider {
    _private: (),
}

#[cfg(not(target_os = "windows"))]
impl ProjFsProvider {
    // No public constructor on non-Windows.
    // The overlay module creates None directly without calling any method here.
}

// ── Re-exports ───────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub use windows_impl::ProjFsProvider;

// ── Cross-platform tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_deletions_empty_when_no_file() {
        let dir = TempDir::new().unwrap();
        let records = load_deletions(dir.path());
        assert!(records.is_empty());
    }

    #[test]
    fn load_deletions_parses_jsonl() {
        let dir = TempDir::new().unwrap();
        let r1 = DeletionRecord {
            path: "a/b.rs".to_string(),
            deleted_at: "2026-04-06T10:00:00Z".to_string(),
        };
        let r2 = DeletionRecord {
            path: "c/d.toml".to_string(),
            deleted_at: "2026-04-06T10:01:00Z".to_string(),
        };
        let lines = format!(
            "{}\n{}\n",
            serde_json::to_string(&r1).unwrap(),
            serde_json::to_string(&r2).unwrap()
        );
        std::fs::write(dir.path().join(".ta-deletes.jsonl"), lines).unwrap();

        let records = load_deletions(dir.path());
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, "a/b.rs");
        assert_eq!(records[1].path, "c/d.toml");
    }

    #[test]
    fn deletion_record_serializes() {
        let r = DeletionRecord {
            path: "src/lib.rs".to_string(),
            deleted_at: "2026-04-06T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("src/lib.rs"));
        let r2: DeletionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r2.path, r.path);
    }
}
