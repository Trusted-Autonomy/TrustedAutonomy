// rules/untrusted.rs: record content that was applied without a person
// (v0.17.12.4, R2.3 second-order injection).
//
// Whatever an unattended apply writes was produced by an agent no person
// reviewed. It is marked untrusted here, in the per-project state directory
// (outside the tree, so the content cannot clear its own mark). Anything that
// builds an agent prompt from project files can ask `is_untrusted` and treat
// such a file as data, never as instructions. Reading untrusted content never
// raises any agent's permissions: the rules table does not look at this log.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LOG: &str = "auto-applied.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoApplied {
    pub draft_id: String,
    /// Project-relative path, `/` separated.
    pub path: String,
    /// sha256 of the file as applied (empty when the file was deleted).
    pub sha256: String,
    pub applied_at: String,
    /// Always `untrusted`.
    pub trust: String,
    /// The rule that allowed it.
    pub rule: String,
}

impl AutoApplied {
    pub fn new(
        draft_id: &str,
        path: &str,
        sha256: String,
        rule: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        AutoApplied {
            draft_id: draft_id.into(),
            path: path.replace('\\', "/"),
            sha256,
            applied_at: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            trust: "untrusted".into(),
            rule: rule.into(),
        }
    }
}

pub fn sha256_of_file(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        Err(_) => String::new(),
    }
}

fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG)
}

/// Append the entries. An error is returned so the caller can say so loudly;
/// it must not be swallowed, because a missing mark is a missing safeguard.
pub fn record(dir: &Path, entries: &[AutoApplied]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = log_path(dir);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    for e in entries {
        let line = serde_json::to_string(e).map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    f.sync_all()
        .map_err(|e| format!("could not flush {}: {e}", path.display()))
}

pub fn load(dir: &Path) -> Vec<AutoApplied> {
    std::fs::read_to_string(log_path(dir))
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Was `rel` last written by an unattended apply?
pub fn is_untrusted(dir: &Path, rel: &str) -> bool {
    let want = ta_workspace::path_safety::fold_component(&rel.replace('\\', "/"));
    load(dir)
        .iter()
        .any(|e| ta_workspace::path_safety::fold_component(&e.path) == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_finds_untrusted_files() {
        let d = tempfile::tempdir().unwrap();
        let at = chrono::Utc::now();
        record(
            d.path(),
            &[AutoApplied::new(
                "d1",
                "docs\\research\\a.md",
                "ab".into(),
                "owner rules [[allow]] #1",
                at,
            )],
        )
        .unwrap();
        let all = load(d.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].path, "docs/research/a.md");
        assert_eq!(all[0].trust, "untrusted");
        assert!(is_untrusted(d.path(), "docs/research/a.md"));
        assert!(is_untrusted(d.path(), "Docs\\Research\\A.md"));
        assert!(!is_untrusted(d.path(), "docs/other.md"));
    }

    #[test]
    fn missing_log_means_nothing_is_marked() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path()).is_empty());
        assert!(!is_untrusted(d.path(), "x"));
    }
}
