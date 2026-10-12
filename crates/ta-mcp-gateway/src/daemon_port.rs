//! Which port a project's daemon API uses when nothing configures one.
//!
//! Every project used to default to 7700, so the second project on a machine
//! either failed to start or, worse, talked to the first project's daemon.
//! A project without an explicit `[server] port` in `.ta/daemon.toml` now gets
//! a port in a documented range, derived from the project's identity so it is
//! stable, checked for availability when the daemon starts, and then recorded
//! in `.ta/daemon.toml` so every client resolves the same value.
//!
//! * Range: [`PORT_RANGE_START`] to [`PORT_RANGE_END`] inclusive (100 ports).
//! * Identity: the project id in `.ta/project-id` when present, otherwise the
//!   canonical project path. The derivation is FNV-1a over that text, so it
//!   does not change between Rust releases or platforms.
//! * Explicit ports (including 7700) are never touched.
//!
//! This module is the single place that answers "which port?". The CLI, the
//! daemon, the MCP gateway's daemon client and the install/office commands all
//! resolve through [`default_port_for`] or [`choose_start_port`].

use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::path::{Path, PathBuf};

use crate::daemon_pid::daemon_toml_path;

/// First port of the range projects without an explicit port choose from.
pub const PORT_RANGE_START: u16 = 7700;
/// Last port (inclusive) of the range.
pub const PORT_RANGE_END: u16 = 7799;
const RANGE_LEN: u16 = PORT_RANGE_END - PORT_RANGE_START + 1;

/// Marker written above the recorded port so a reader knows who wrote it.
pub const RECORDED_PORT_COMMENT: &str =
    "# Port chosen by `ta daemon start` for this project (range 7700-7799). Edit to pin a different port.";

/// A usable project identity string: the recorded project id, else the
/// canonical path with `/` separators (so Windows `\\?\` prefixes and case
/// differences do not matter to the caller).
pub fn project_identity(project_root: &Path) -> String {
    if let Ok(id) = std::fs::read_to_string(project_root.join(".ta").join("project-id")) {
        let id = id.trim();
        if id.len() == 34
            && id.starts_with("p_")
            && id[2..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return id.to_string();
        }
    }
    canonical_text(project_root)
}

/// Canonical form of a path for comparison: symlinks resolved (macOS
/// `/private/tmp` vs `/tmp`), Windows verbatim prefix (`\\?\`) removed,
/// separators forward, trailing slash removed, and lower-cased on Windows
/// where the file system is case-insensitive.
pub fn canonical_text(path: &Path) -> String {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    normalize_path_text(&canon.to_string_lossy())
}

/// Text-only half of [`canonical_text`], usable on a path string received from
/// another process (a daemon's `project_root`).
pub fn normalize_path_text(raw: &str) -> String {
    let mut s = raw.replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/UNC/") {
        s = format!("//{rest}");
    } else if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) || looks_like_windows_drive_path(&s) {
        s = s.to_lowercase();
    }
    s
}

fn looks_like_windows_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

/// Two project roots (one possibly reported by another process) name the same
/// directory.
pub fn same_project_root(a: &Path, b: &str) -> bool {
    canonical_text(a) == canonical_text(Path::new(b))
}

fn fnv1a(text: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The port this identity starts probing from. Pure and stable.
pub fn derive_start_port(identity: &str) -> u16 {
    PORT_RANGE_START + (fnv1a(identity) % u64::from(RANGE_LEN)) as u16
}

/// `[server] port` from `.ta/daemon.toml`. `Ok(None)` when the file or the key
/// is absent. An unreadable or invalid value is an error, never a guess.
pub fn explicit_server_port(project_root: &Path) -> Result<Option<u16>, String> {
    let (_, port) = read_server_table(project_root)?;
    Ok(port)
}

/// `([server] bind, [server] port)` from `.ta/daemon.toml`, both optional.
pub fn read_server_table(project_root: &Path) -> Result<(Option<String>, Option<u16>), String> {
    let path = daemon_toml_path(project_root);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(e) => {
            return Err(format!(
                "Could not read {}: {e}. Refusing to guess a daemon port. Check the file's \
                 permissions.",
                path.display()
            ))
        }
    };
    let table: toml::Table = content.parse().map_err(|e| {
        format!(
            "{} is not valid TOML ({e}), so the daemon port cannot be determined. Refusing to \
             guess a port. Fix the file, then retry.",
            path.display()
        )
    })?;
    let server = table.get("server");
    let bind = server
        .and_then(|s| s.get("bind"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let port = match server.and_then(|s| s.get("port")) {
        None => None,
        Some(v) => Some(
            v.as_integer()
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p != 0)
                .ok_or_else(|| {
                    format!(
                        "{} has `[server] port = {v}`, which is not a usable TCP port \
                         (1-65535). Refusing to guess a daemon port. Fix the value, then retry.",
                        path.display()
                    )
                })?,
        ),
    };
    Ok((bind, port))
}

/// The port clients should assume for `project_root` when no pid file says
/// otherwise: the explicit `[server] port`, else the derived start port.
pub fn default_port_for(project_root: &Path) -> Result<u16, String> {
    Ok(match explicit_server_port(project_root)? {
        Some(p) => p,
        None => derive_start_port(&project_identity(project_root)),
    })
}

/// Infallible variant for display paths: a broken daemon.toml falls back to
/// the derived port (callers that send credentials use [`default_port_for`]).
pub fn default_port_lenient(project_root: &Path) -> u16 {
    default_port_for(project_root)
        .unwrap_or_else(|_| derive_start_port(&project_identity(project_root)))
}

/// Result of choosing a port for a daemon start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPort {
    pub port: u16,
    /// True when `[server] port` was set by the user or an earlier start.
    pub explicit: bool,
    /// The port derivation started from (equals `port` unless it was busy).
    pub derived: u16,
}

/// Choose the port a daemon for `project_root` should bind.
///
/// * Explicit `[server] port`: returned unchanged, whether or not it is free
///   (the caller reports a collision with the right context).
/// * Otherwise probe from the derived port, wrapping inside the range, and
///   return the first port `is_free` accepts.
///
/// Errors name the project, the range and the fix when every port is taken.
pub fn choose_start_port(
    project_root: &Path,
    is_free: &dyn Fn(u16) -> bool,
) -> Result<StartPort, String> {
    if let Some(p) = explicit_server_port(project_root)? {
        return Ok(StartPort {
            port: p,
            explicit: true,
            derived: p,
        });
    }
    let derived = derive_start_port(&project_identity(project_root));
    for offset in 0..RANGE_LEN {
        let port = PORT_RANGE_START + (derived - PORT_RANGE_START + offset) % RANGE_LEN;
        if is_free(port) {
            return Ok(StartPort {
                port,
                explicit: false,
                derived,
            });
        }
    }
    Err(format!(
        "No free daemon port for {}: all {RANGE_LEN} ports from {PORT_RANGE_START} to \
         {PORT_RANGE_END} are in use. Stop another TA daemon (`ta daemon status` in that \
         project), or pin a port outside the range with `[server] port = <port>` in {}.",
        project_root.display(),
        daemon_toml_path(project_root).display()
    ))
}

/// True when nothing is listening on `bind:port` (a bind attempt succeeds).
/// An unresolvable `bind` counts as not free so the caller reports it.
pub fn port_is_free(bind: &str, port: u16) -> bool {
    let host = if bind.contains(':') && !bind.starts_with('[') {
        format!("[{bind}]")
    } else {
        bind.to_string()
    };
    let addrs: Vec<SocketAddr> = match (host.trim_matches(['[', ']']), port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(_) => return false,
    };
    !addrs.is_empty() && addrs.iter().all(|a| TcpListener::bind(a).is_ok())
}

/// Record `port` as `[server] port` in `.ta/daemon.toml`, preserving every
/// other line. Returns the path written. No-op error if a port is already
/// recorded (callers check [`explicit_server_port`] first). The result is
/// re-read and verified; on mismatch the original file content is restored.
pub fn record_port(project_root: &Path, port: u16) -> Result<PathBuf, String> {
    let path = daemon_toml_path(project_root);
    let original = match std::fs::read_to_string(&path) {
        Ok(c) => Some(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(format!(
                "Could not read {} to record port {port}: {e}. Set `[server] port = {port}` \
                 there by hand.",
                path.display()
            ))
        }
    };
    let line = format!("{RECORDED_PORT_COMMENT}\nport = {port}\n");
    let updated = match &original {
        None => format!("[server]\n{line}"),
        Some(text) => {
            let mut out = String::new();
            let mut inserted = false;
            for l in text.lines() {
                out.push_str(l);
                out.push('\n');
                if !inserted && l.trim() == "[server]" {
                    out.push_str(&line);
                    inserted = true;
                }
            }
            if !inserted {
                if !out.is_empty() && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                out.push_str("[server]\n");
                out.push_str(&line);
            }
            out
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &updated).map_err(|e| {
        format!(
            "Could not write {}: {e}. Set `[server] port = {port}` there by hand.",
            path.display()
        )
    })?;
    match explicit_server_port(project_root) {
        Ok(Some(p)) if p == port => Ok(path),
        other => {
            match &original {
                Some(text) => {
                    let _ = std::fs::write(&path, text);
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(format!(
                "Recording port {port} in {} did not verify (read back {other:?}); the file was \
                 restored. Set `[server] port = {port}` there by hand.",
                path.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        d
    }

    #[test]
    fn derived_port_is_stable_and_inside_the_range() {
        let a = derive_start_port("p_0123456789abcdef0123456789abcdef");
        assert_eq!(a, derive_start_port("p_0123456789abcdef0123456789abcdef"));
        for i in 0..500 {
            let p = derive_start_port(&format!("project-{i}"));
            assert!((PORT_RANGE_START..=PORT_RANGE_END).contains(&p), "{p}");
        }
    }

    #[test]
    fn different_identities_spread_across_the_range() {
        let ports: std::collections::HashSet<u16> = (0..200)
            .map(|i| derive_start_port(&format!("project-{i}")))
            .collect();
        assert!(ports.len() > 40, "poor spread: {}", ports.len());
    }

    #[test]
    fn explicit_port_including_7700_is_untouched() {
        for want in [7700u16, 9100] {
            let d = project();
            std::fs::write(
                daemon_toml_path(d.path()),
                format!("[server]\nport = {want}\n"),
            )
            .unwrap();
            let got = choose_start_port(d.path(), &|_| false).unwrap();
            assert_eq!(got.port, want);
            assert!(got.explicit);
            assert_eq!(default_port_for(d.path()).unwrap(), want);
        }
    }

    #[test]
    fn busy_derived_port_moves_to_the_next_free_one() {
        let d = project();
        let start = derive_start_port(&project_identity(d.path()));
        let got = choose_start_port(d.path(), &|p| p != start).unwrap();
        assert_eq!(got.derived, start);
        assert_ne!(got.port, start);
        assert!(!got.explicit);
        assert!((PORT_RANGE_START..=PORT_RANGE_END).contains(&got.port));
    }

    #[test]
    fn exhausted_range_gives_an_actionable_collision_message() {
        let d = project();
        let err = choose_start_port(d.path(), &|_| false).unwrap_err();
        assert!(err.contains("7700") && err.contains("7799"), "{err}");
        assert!(err.contains("ta daemon status"), "{err}");
        assert!(err.contains("[server] port"), "{err}");
        assert!(err.contains(&d.path().display().to_string()), "{err}");
    }

    #[test]
    fn project_id_drives_the_identity_when_present() {
        let d = project();
        let id = "p_0123456789abcdef0123456789abcdef";
        std::fs::write(d.path().join(".ta/project-id"), format!("{id}\n")).unwrap();
        assert_eq!(project_identity(d.path()), id);
        // A malformed id file falls back to the path.
        std::fs::write(d.path().join(".ta/project-id"), "nope\n").unwrap();
        assert_eq!(project_identity(d.path()), canonical_text(d.path()));
    }

    #[test]
    fn record_port_preserves_the_rest_of_the_file_and_verifies() {
        let d = project();
        std::fs::write(
            daemon_toml_path(d.path()),
            "# my notes\n[agent]\ndefault = \"codex\"\n\n[server]\nbind = \"127.0.0.1\"\n",
        )
        .unwrap();
        record_port(d.path(), 7742).unwrap();
        let text = std::fs::read_to_string(daemon_toml_path(d.path())).unwrap();
        assert!(text.contains("# my notes"), "{text}");
        assert!(text.contains("default = \"codex\""), "{text}");
        assert!(text.contains("bind = \"127.0.0.1\""), "{text}");
        assert_eq!(explicit_server_port(d.path()).unwrap(), Some(7742));
    }

    #[test]
    fn record_port_creates_the_file_and_appends_a_missing_server_table() {
        let d = project();
        record_port(d.path(), 7701).unwrap();
        assert_eq!(explicit_server_port(d.path()).unwrap(), Some(7701));

        let d2 = project();
        std::fs::write(daemon_toml_path(d2.path()), "[agent]\ndefault = \"x\"\n").unwrap();
        record_port(d2.path(), 7702).unwrap();
        assert_eq!(explicit_server_port(d2.path()).unwrap(), Some(7702));
    }

    #[test]
    fn invalid_port_value_is_an_error_not_a_guess() {
        let d = project();
        std::fs::write(daemon_toml_path(d.path()), "[server]\nport = 70000\n").unwrap();
        assert!(explicit_server_port(d.path()).is_err());
        assert!(default_port_for(d.path()).is_err());
    }

    #[test]
    fn path_normalisation_handles_windows_and_trailing_separators() {
        assert_eq!(normalize_path_text(r"\\?\C:\Work\Proj\"), "c:/work/proj");
        assert_eq!(normalize_path_text("C:/Work/Proj"), "c:/work/proj");
        assert_eq!(normalize_path_text("/tmp/a/"), "/tmp/a");
        assert_eq!(normalize_path_text(r"\\?\UNC\srv\share"), "//srv/share");
    }

    #[test]
    fn same_root_sees_through_symlinks_and_trailing_slashes() {
        let d = tempfile::tempdir().unwrap();
        let canon = std::fs::canonicalize(d.path()).unwrap();
        assert!(same_project_root(d.path(), &canon.display().to_string()));
        assert!(same_project_root(
            d.path(),
            &format!("{}/", canon.display())
        ));
        let other = tempfile::tempdir().unwrap();
        assert!(!same_project_root(
            d.path(),
            &other.path().display().to_string()
        ));
    }

    #[test]
    fn port_is_free_sees_a_held_port() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(!port_is_free("127.0.0.1", port));
        drop(l);
        assert!(port_is_free("127.0.0.1", port));
    }
}
