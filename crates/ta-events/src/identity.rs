// identity.rs -- Which project an event, API response or daemon record belongs to.
//
// "The daemon I reached is the project" stops being a safe assumption the day
// one daemon serves several projects. Every event and every daemon API response
// therefore names its project: `project_id` (the stable, non-secret id in
// `<project>/.ta/project-id`) and `project_root` (canonical absolute path).
//
// This module is the one place that derives the pair, read-only: it never mints
// a project id (read paths must not write; the daemon mints at startup through
// `ta_credentials::state::ProjectState::open`).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// File (inside `<project>/.ta/`) that holds the project id.
pub const PROJECT_ID_FILENAME: &str = "project-id";

/// HTTP header carrying the project id on every daemon response (including
/// bodies that are not JSON objects: SSE streams, HTML, binary, arrays).
pub const PROJECT_ID_HEADER: &str = "x-ta-project-id";
/// HTTP header carrying the percent-encoded canonical project root.
pub const PROJECT_ROOT_HEADER: &str = "x-ta-project-root";

/// The identity of one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectIdentity {
    /// `p_` + 32 lowercase hex characters, or `None` when the project has no
    /// valid `.ta/project-id` yet.
    pub project_id: Option<String>,
    /// Canonical absolute project root.
    pub project_root: String,
}

impl ProjectIdentity {
    /// Identity of the project rooted at `project_root`. Read-only.
    pub fn read(project_root: &Path) -> Self {
        Self {
            project_id: read_project_id(project_root),
            project_root: canonical_root_string(project_root),
        }
    }

    /// Identity of the project whose event directory is `events_dir`
    /// (`<root>/.ta/events`). `None` for any other layout, so a store pointed
    /// at an arbitrary directory never invents a project.
    pub fn from_events_dir(events_dir: &Path) -> Option<Self> {
        if events_dir.file_name()? != "events" {
            return None;
        }
        let ta = events_dir.parent()?;
        if ta.file_name()? != ".ta" {
            return None;
        }
        Some(Self::read(ta.parent()?))
    }
}

/// The valid project id recorded in `<project_root>/.ta/project-id`, if any.
pub fn read_project_id(project_root: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(project_root.join(".ta").join(PROJECT_ID_FILENAME)).ok()?;
    let id = raw.trim();
    let valid = id.len() == 34
        && id.starts_with("p_")
        && id[2..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    valid.then(|| id.to_string())
}

/// Canonical absolute form of a project root, as the daemon reports it.
pub fn canonical_root_string(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

/// Percent-encode `s` for an HTTP header value: printable ASCII passes
/// through, `%` and everything else (non-ASCII, control characters) becomes
/// `%XX` per UTF-8 byte. Decode with any standard percent-decoder.
pub fn header_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (0x20..0x7f).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "p_0123456789abcdef0123456789abcdef";

    fn project(id: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ta")).unwrap();
        if let Some(id) = id {
            std::fs::write(d.path().join(".ta/project-id"), format!("{id}\n")).unwrap();
        }
        d
    }

    #[test]
    fn reads_a_valid_id_and_canonical_root() {
        let d = project(Some(ID));
        let got = ProjectIdentity::read(d.path());
        assert_eq!(got.project_id.as_deref(), Some(ID));
        assert_eq!(
            got.project_root,
            std::fs::canonicalize(d.path())
                .unwrap()
                .display()
                .to_string()
        );
    }

    #[test]
    fn missing_or_malformed_id_is_none_and_never_minted() {
        let d = project(None);
        assert_eq!(ProjectIdentity::read(d.path()).project_id, None);
        assert!(!d.path().join(".ta/project-id").exists());
        for bad in ["nope", "p_ABCDEF0123456789abcdef0123456789", "p_0123"] {
            std::fs::write(d.path().join(".ta/project-id"), bad).unwrap();
            assert_eq!(read_project_id(d.path()), None, "{bad}");
        }
    }

    #[test]
    fn events_dir_maps_to_its_project_only_in_the_standard_layout() {
        let d = project(Some(ID));
        let got = ProjectIdentity::from_events_dir(&d.path().join(".ta/events")).unwrap();
        assert_eq!(got.project_id.as_deref(), Some(ID));
        assert!(ProjectIdentity::from_events_dir(&d.path().join("elsewhere")).is_none());
        assert!(ProjectIdentity::from_events_dir(&d.path().join("events")).is_none());
    }

    #[test]
    fn header_encoding_is_reversible_ascii() {
        assert_eq!(header_encode("/tmp/a b"), "/tmp/a b");
        assert_eq!(header_encode("/tmp/é%"), "/tmp/%C3%A9%25");
        assert!(header_encode("/tmp/\n").contains("%0A"));
    }
}
