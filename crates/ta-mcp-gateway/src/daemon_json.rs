//! `.ta/daemon.json`: the one place a project records where its daemon is.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "host": "127.0.0.1",
//!   "port": 7712,
//!   "bind": "127.0.0.1:7712",
//!   "pid": 41873,
//!   "project_id": "p_0123456789abcdef0123456789abcdef",
//!   "project_root": "/Users/me/work/acme",
//!   "started_at": "2026-10-23T09:14:02Z"
//! }
//! ```
//!
//! Every client (the `ta` CLI, the MCP gateway, the VT poller, editors,
//! Studio) resolves the daemon address through this file and never through a
//! literal port. A shared daemon later changes only what is written here.
//! `host` is connectable (a wildcard bind such as `0.0.0.0` is recorded as
//! `127.0.0.1`); `bind` is what the daemon actually bound.
//!
//! Lifecycle, so the file composes with `.ta/daemon.pid` and a crash:
//!
//! * The daemon writes it only AFTER it has bound its port. A daemon that
//!   fails to start (port in use, strict-posture refusal) never touches the
//!   file, so it can not clobber the record of the daemon that holds the port.
//! * The write is atomic (temp file in `.ta/`, fsync, rename) and, on Unix,
//!   mode 0600, so a reader never sees a half-written file.
//! * A graceful shutdown removes it, but only when the file still names this
//!   process (a successor's record is never deleted by its predecessor).
//! * A crash leaves it behind. [`read`] therefore NEVER trusts the file
//!   blindly: it is ignored (reported as [`Lookup::Ignored`] with the reason)
//!   when it is unparsable, when its `pid` is not running, or when it names a
//!   different project than the directory it was found in (a copied tree).
//!   [`clean_stale`] removes such a file; `ta daemon start` and `stop` call it.
//!
//! Anything in `.ta/` is writable by an agent running in the project, so a
//! client that sends credentials must still confirm the answering daemon's
//! `project_id`/`project_root` (every response carries both) before doing so.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ta_events::identity::ProjectIdentity;

/// File name inside `<project>/.ta/`.
pub const FILE_NAME: &str = "daemon.json";
/// Version of the record layout. Readers ignore unknown fields, so adding a
/// field does not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// `.ta/daemon.json` under `project_root`.
pub fn path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join(FILE_NAME)
}

/// The recorded daemon address and identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRecord {
    pub schema: u32,
    /// Host to connect to.
    pub host: String,
    pub port: u16,
    /// `host:port` the daemon bound.
    pub bind: String,
    pub pid: u32,
    /// Absent only when the project has no `.ta/project-id` yet.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Canonical absolute project root.
    pub project_root: String,
    /// RFC 3339 UTC start time.
    pub started_at: String,
}

impl DaemonRecord {
    /// `http://host:port`, bracketing IPv6 literals.
    pub fn base_url(&self) -> String {
        if self.host.contains(':') {
            format!("http://[{}]:{}", self.host, self.port)
        } else {
            format!("http://{}:{}", self.host, self.port)
        }
    }
}

/// What [`read`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// No `.ta/daemon.json`.
    Absent,
    /// A record whose process is running and whose identity matches this project.
    Live(DaemonRecord),
    /// A file exists but must not be used. `reason` says why and is safe to
    /// show a user ("pid 123 is not running (the daemon exited or crashed)").
    Ignored { reason: String },
}

/// Read `project_root`'s record and decide whether to trust it. Never writes.
pub fn read(project_root: &Path) -> Lookup {
    read_with(project_root, &ta_lifecycle::process_is_alive)
}

/// [`read`] with an injectable liveness probe (tests).
pub fn read_with(project_root: &Path, alive: &dyn Fn(u32) -> bool) -> Lookup {
    let file = path(project_root);
    let raw = match std::fs::read_to_string(&file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Lookup::Absent,
        Err(e) => {
            return Lookup::Ignored {
                reason: format!("{} could not be read: {e}", file.display()),
            }
        }
    };
    let record: DaemonRecord = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(e) => {
            return Lookup::Ignored {
                reason: format!(
                    "{} is not a valid daemon record ({e}); it was probably cut short by a crash",
                    file.display()
                ),
            }
        }
    };
    if record.port == 0 || record.host.trim().is_empty() {
        return Lookup::Ignored {
            reason: format!("{} has no usable host and port", file.display()),
        };
    }
    let here = crate::daemon_port::canonical_text(project_root);
    let recorded = crate::daemon_port::normalize_path_text(&record.project_root);
    if recorded != here {
        return Lookup::Ignored {
            reason: format!(
                "{} names the project {} but was found in {} (a copied project tree)",
                file.display(),
                record.project_root,
                project_root.display()
            ),
        };
    }
    if let (Some(recorded_id), Some(own_id)) = (
        record.project_id.as_deref(),
        ta_events::identity::read_project_id(project_root),
    ) {
        if recorded_id != own_id {
            return Lookup::Ignored {
                reason: format!(
                    "{} names project id {recorded_id} but this project's id is {own_id}",
                    file.display()
                ),
            };
        }
    }
    if record.pid == 0 || !alive(record.pid) {
        return Lookup::Ignored {
            reason: format!(
                "pid {} in {} is not running (the daemon exited or crashed)",
                record.pid,
                file.display()
            ),
        };
    }
    Lookup::Live(record)
}

/// Write `.ta/daemon.json` for the daemon with `pid`, bound on `bind_host:port`.
/// Call this only after the port is bound. Atomic, owner-only on Unix.
pub fn write(
    project_root: &Path,
    bind_host: &str,
    port: u16,
    pid: u32,
) -> std::io::Result<DaemonRecord> {
    let identity = ProjectIdentity::read(project_root);
    let host = crate::daemon_pid::connect_host(bind_host);
    let record = DaemonRecord {
        schema: SCHEMA_VERSION,
        bind: if bind_host.contains(':') && !bind_host.starts_with('[') {
            format!("[{bind_host}]:{port}")
        } else {
            format!("{bind_host}:{port}")
        },
        host,
        port,
        pid,
        project_id: identity.project_id,
        project_root: identity.project_root,
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let mut json = serde_json::to_string_pretty(&record).map_err(std::io::Error::other)?;
    json.push('\n');

    let dir = project_root.join(".ta");
    std::fs::create_dir_all(&dir)?;
    sweep_orphan_temp_files(&dir);
    let tmp = dir.join(format!(".{FILE_NAME}.{pid}.tmp"));
    // A planted file or symlink at the temp name must not be written through.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let result = (|| {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path(project_root))
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(record)
}

/// Remove `.ta/daemon.json` when it still names `pid`. A successor daemon's
/// record (a different pid) is left alone. Returns whether a file was removed.
pub fn remove_if_owned(project_root: &Path, pid: u32) -> bool {
    let file = path(project_root);
    let owned = std::fs::read_to_string(&file)
        .ok()
        .and_then(|raw| serde_json::from_str::<DaemonRecord>(&raw).ok())
        .is_some_and(|r| r.pid == pid);
    owned && std::fs::remove_file(&file).is_ok()
}

/// Remove `.ta/daemon.json` if (and only if) [`read`] says it must be ignored.
/// Returns the reason when a file was removed. A live record is never touched.
pub fn clean_stale(project_root: &Path) -> Option<String> {
    clean_stale_with(project_root, &ta_lifecycle::process_is_alive)
}

/// [`clean_stale`] with an injectable liveness probe (tests).
pub fn clean_stale_with(project_root: &Path, alive: &dyn Fn(u32) -> bool) -> Option<String> {
    match read_with(project_root, alive) {
        Lookup::Ignored { reason } => {
            let file = path(project_root);
            match std::fs::remove_file(&file) {
                Ok(()) => {
                    tracing::info!(path = %file.display(), reason = %reason, "removed stale daemon record");
                    Some(reason)
                }
                Err(e) => {
                    tracing::warn!(path = %file.display(), error = %e, reason = %reason, "could not remove stale daemon record");
                    None
                }
            }
        }
        Lookup::Absent | Lookup::Live(_) => None,
    }
}

/// Delete `.daemon.json.<pid>.tmp` files left by a writer that died between
/// creating and renaming. Only files whose pid is not running are removed.
fn sweep_orphan_temp_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!(".{FILE_NAME}.");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = name
            .strip_prefix(&prefix)
            .and_then(|r| r.strip_suffix(".tmp"))
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !ta_lifecycle::process_is_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "p_0123456789abcdef0123456789abcdef";

    fn project(id: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        if let Some(id) = id {
            std::fs::write(d.path().join(".ta/project-id"), id).unwrap();
        }
        d
    }

    fn alive(_: u32) -> bool {
        true
    }
    fn dead(_: u32) -> bool {
        false
    }

    #[test]
    fn write_then_read_round_trips_address_pid_and_identity() {
        let d = project(Some(ID));
        let rec = write(d.path(), "127.0.0.1", 7712, 4242).unwrap();
        assert_eq!(rec.project_id.as_deref(), Some(ID));
        match read_with(d.path(), &alive) {
            Lookup::Live(got) => {
                assert_eq!(got, rec);
                assert_eq!(got.base_url(), "http://127.0.0.1:7712");
                assert_eq!(got.pid, 4242);
                assert_eq!(got.bind, "127.0.0.1:7712");
                assert_eq!(
                    got.project_root,
                    std::fs::canonicalize(d.path())
                        .unwrap()
                        .display()
                        .to_string()
                );
            }
            other => panic!("expected Live, got {other:?}"),
        }
    }

    #[test]
    fn a_wildcard_bind_is_recorded_as_a_connectable_host() {
        let d = project(Some(ID));
        let rec = write(d.path(), "0.0.0.0", 7713, 1).unwrap();
        assert_eq!(rec.host, "127.0.0.1");
        assert_eq!(rec.bind, "0.0.0.0:7713");
        let v6 = write(d.path(), "::1", 7714, 1).unwrap();
        assert_eq!(v6.base_url(), "http://[::1]:7714");
        assert_eq!(v6.bind, "[::1]:7714");
    }

    #[test]
    fn absent_file_is_absent_not_an_error() {
        let d = project(None);
        assert_eq!(read_with(d.path(), &alive), Lookup::Absent);
        assert_eq!(clean_stale_with(d.path(), &dead), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = project(Some(ID));
        write(d.path(), "127.0.0.1", 7712, 4242).unwrap();
        let mode = std::fs::metadata(path(d.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(d.path().join(".ta"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_crashed_daemons_record_is_ignored_then_cleaned_but_a_live_one_is_not() {
        let d = project(Some(ID));
        write(d.path(), "127.0.0.1", 7712, 4242).unwrap();

        match read_with(d.path(), &dead) {
            Lookup::Ignored { reason } => {
                assert!(
                    reason.contains("4242") && reason.contains("not running"),
                    "{reason}"
                )
            }
            other => panic!("expected Ignored, got {other:?}"),
        }
        // A live record survives clean_stale untouched.
        assert_eq!(clean_stale_with(d.path(), &alive), None);
        assert!(path(d.path()).exists());
        // A dead one is removed, once.
        assert!(clean_stale_with(d.path(), &dead).unwrap().contains("4242"));
        assert!(!path(d.path()).exists());
        assert_eq!(clean_stale_with(d.path(), &dead), None);
    }

    #[test]
    fn a_truncated_or_garbage_file_is_ignored_not_trusted() {
        let d = project(Some(ID));
        for bad in [
            "",
            "{\"schema\":1,\"host\":\"127.0.0.1\",\"po",
            "not json",
            "[]",
        ] {
            std::fs::write(path(d.path()), bad).unwrap();
            assert!(
                matches!(read_with(d.path(), &alive), Lookup::Ignored { .. }),
                "{bad:?}"
            );
            assert!(clean_stale_with(d.path(), &alive).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn port_zero_is_not_an_address() {
        let d = project(Some(ID));
        let mut rec = write(d.path(), "127.0.0.1", 7712, 1).unwrap();
        rec.port = 0;
        std::fs::write(path(d.path()), serde_json::to_string(&rec).unwrap()).unwrap();
        assert!(matches!(
            read_with(d.path(), &alive),
            Lookup::Ignored { .. }
        ));
    }

    #[test]
    fn a_record_copied_into_another_project_is_never_this_projects_daemon() {
        let a = project(Some(ID));
        let b = project(Some("p_ffffffffffffffffffffffffffffffff"));
        write(a.path(), "127.0.0.1", 7712, 4242).unwrap();
        std::fs::copy(path(a.path()), path(b.path())).unwrap();
        match read_with(b.path(), &alive) {
            Lookup::Ignored { reason } => assert!(reason.contains("copied"), "{reason}"),
            other => panic!("project B must not adopt project A's daemon: {other:?}"),
        }
        // A's own lookup is unaffected.
        assert!(matches!(read_with(a.path(), &alive), Lookup::Live(_)));
    }

    #[test]
    fn a_matching_root_with_a_different_project_id_is_ignored() {
        let d = project(Some(ID));
        write(d.path(), "127.0.0.1", 7712, 4242).unwrap();
        std::fs::write(
            d.path().join(".ta/project-id"),
            "p_ffffffffffffffffffffffffffffffff",
        )
        .unwrap();
        match read_with(d.path(), &alive) {
            Lookup::Ignored { reason } => assert!(reason.contains("project id"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_project_that_gets_its_id_after_the_daemon_started_still_resolves() {
        let d = project(None);
        let rec = write(d.path(), "127.0.0.1", 7712, 4242).unwrap();
        assert_eq!(rec.project_id, None);
        std::fs::write(d.path().join(".ta/project-id"), ID).unwrap();
        assert!(matches!(read_with(d.path(), &alive), Lookup::Live(_)));
    }

    #[test]
    fn a_predecessor_never_deletes_its_successors_record() {
        let d = project(Some(ID));
        write(d.path(), "127.0.0.1", 7712, 200).unwrap();
        assert!(!remove_if_owned(d.path(), 100), "pid 100 does not own it");
        assert!(path(d.path()).exists());
        assert!(remove_if_owned(d.path(), 200));
        assert!(!path(d.path()).exists());
        assert!(!remove_if_owned(d.path(), 200), "already gone");
    }

    #[test]
    fn rewriting_replaces_the_record_atomically_with_the_new_address() {
        let d = project(Some(ID));
        write(d.path(), "127.0.0.1", 7712, 1).unwrap();
        write(d.path(), "127.0.0.1", 7799, 2).unwrap();
        match read_with(d.path(), &alive) {
            Lookup::Live(r) => assert_eq!((r.port, r.pid), (7799, 2)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_dead_writers_orphan_temp_file_is_swept_and_a_planted_one_is_not_written_through() {
        let d = project(Some(ID));
        let ta = d.path().join(".ta");
        // Orphan from a writer that died (pid assumed not running).
        let orphan = ta.join(format!(".{FILE_NAME}.2147483646.tmp"));
        std::fs::write(&orphan, "half").unwrap();
        // A file planted at OUR temp name must be replaced, not appended to.
        let ours = ta.join(format!(".{FILE_NAME}.77.tmp"));
        std::fs::write(&ours, "planted").unwrap();
        write(d.path(), "127.0.0.1", 7712, 77).unwrap();
        assert!(!orphan.exists());
        assert!(!ours.exists());
        assert!(matches!(read_with(d.path(), &alive), Lookup::Live(_)));
    }

    #[test]
    fn unknown_fields_from_a_newer_writer_are_tolerated() {
        let d = project(Some(ID));
        let rec = write(d.path(), "127.0.0.1", 7712, 4242).unwrap();
        let mut v = serde_json::to_value(&rec).unwrap();
        v["tls"] = serde_json::json!(true);
        v["schema"] = serde_json::json!(2);
        std::fs::write(path(d.path()), v.to_string()).unwrap();
        assert!(matches!(read_with(d.path(), &alive), Lookup::Live(_)));
    }
}
