//! Append-only URI journal at `.ta/governed/journal.jsonl`.
//!
//! Every governed-path event is written as a single JSON line.  The journal is
//! the authoritative record of which SHA blobs are still "live" (referenced by
//! non-DENIED, non-rolled-back entries).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::error::GovernedPathError;

/// Every event that gets appended to the journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalAction {
    /// Pre-goal baseline snapshot of the file's current state.
    Snapshot,
    /// The agent (or intercept layer) wrote to the path.
    Write,
    /// A `ta draft deny` prevented further replay of the associated write.
    Denied,
    /// A `ta draft apply` confirmed the write reached the real filesystem.
    Applied,
    /// Rollback: the pre-goal snapshot was restored to the real path.
    RolledBack,
}

impl std::fmt::Display for JournalAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            JournalAction::Snapshot => "snapshot",
            JournalAction::Write => "write",
            JournalAction::Denied => "denied",
            JournalAction::Applied => "applied",
            JournalAction::RolledBack => "rolled_back",
        };
        write!(f, "{}", s)
    }
}

/// A single line in the URI journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// File URI: `fs://workspace/<relative-path>`.
    pub uri: String,
    /// SHA-256 hex of the file content at the time of this event.
    pub sha256: String,
    /// The event kind.
    pub action: JournalAction,
    /// Wall-clock time of the event (ISO 8601 / RFC 3339).
    pub at: DateTime<Utc>,
    /// Goal run ID that triggered this event, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<Uuid>,
    /// Draft package ID associated with an `apply` or `denied` event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<Uuid>,
}

impl JournalEntry {
    pub fn new(
        uri: impl Into<String>,
        sha256: impl Into<String>,
        action: JournalAction,
        goal_id: Option<Uuid>,
    ) -> Self {
        Self {
            uri: uri.into(),
            sha256: sha256.into(),
            action,
            at: Utc::now(),
            goal_id,
            draft_id: None,
        }
    }

    pub fn with_draft_id(mut self, draft_id: Uuid) -> Self {
        self.draft_id = Some(draft_id);
        self
    }
}

/// Build the canonical URI for a workspace-relative path.
pub fn path_to_uri(rel_path: &Path) -> String {
    format!("fs://workspace/{}", rel_path.display())
}

/// Append-only URI journal stored at `<workspace_root>/.ta/governed/journal.jsonl`.
pub struct UriJournal {
    path: PathBuf,
}

impl UriJournal {
    /// Open (or create) the journal at `<workspace_root>/.ta/governed/journal.jsonl`.
    pub fn open(workspace_root: &Path) -> Result<Self, GovernedPathError> {
        let dir = workspace_root.join(".ta").join("governed");
        std::fs::create_dir_all(&dir)
            .map_err(|e| GovernedPathError::io(dir.display().to_string(), e))?;
        Ok(Self {
            path: dir.join("journal.jsonl"),
        })
    }

    /// Append a single entry to the journal.
    pub fn append(&self, entry: &JournalEntry) -> Result<(), GovernedPathError> {
        let line = serde_json::to_string(entry)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| GovernedPathError::io(self.path.display().to_string(), e))?;
        writeln!(f, "{}", line)
            .map_err(|e| GovernedPathError::io(self.path.display().to_string(), e))?;
        Ok(())
    }

    /// Read all entries from the journal.
    pub fn entries(&self) -> Result<Vec<JournalEntry>, GovernedPathError> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let f = std::fs::File::open(&self.path)
            .map_err(|e| GovernedPathError::io(self.path.display().to_string(), e))?;
        let mut entries = Vec::new();
        for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
            let line =
                line.map_err(|e| GovernedPathError::io(self.path.display().to_string(), e))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: JournalEntry =
                serde_json::from_str(trimmed).map_err(|e| GovernedPathError::CorruptJournal {
                    line: i + 1,
                    detail: e.to_string(),
                })?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Return the set of SHA-256 digests that are "live" — referenced by at least
    /// one entry that is not `Denied` or `RolledBack`, and whose `at` timestamp
    /// is within `retain_days` of the cutoff.
    ///
    /// The caller uses this to decide which blobs to keep during GC.
    pub fn live_shas(
        &self,
        retain_days: u32,
    ) -> Result<std::collections::HashSet<String>, GovernedPathError> {
        let cutoff = Utc::now() - chrono::Duration::days(retain_days as i64);
        let mut live = std::collections::HashSet::new();
        for entry in self.entries()? {
            // Entries within the retain window are always live regardless of action.
            // Entries outside the window are only live if they are not terminal
            // (i.e. the blob might still be needed for rollback).
            let in_window = entry.at > cutoff;
            let terminal = matches!(
                entry.action,
                JournalAction::Denied | JournalAction::RolledBack
            );
            if in_window || !terminal {
                live.insert(entry.sha256.clone());
            }
        }
        Ok(live)
    }

    /// Return the most recent `snapshot` entry for a given URI, if any.
    pub fn last_snapshot(&self, uri: &str) -> Result<Option<JournalEntry>, GovernedPathError> {
        Ok(self
            .entries()?
            .into_iter()
            .rev()
            .find(|e| e.uri == uri && matches!(e.action, JournalAction::Snapshot)))
    }

    /// Return all `write` entries for a given goal ID that have not been denied.
    pub fn pending_writes_for_goal(
        &self,
        goal_id: Uuid,
    ) -> Result<Vec<JournalEntry>, GovernedPathError> {
        let entries = self.entries()?;
        // Collect denied URIs so we can exclude them.
        let denied_uris: std::collections::HashSet<String> = entries
            .iter()
            .filter(|e| e.goal_id == Some(goal_id) && matches!(e.action, JournalAction::Denied))
            .map(|e| e.uri.clone())
            .collect();

        Ok(entries
            .into_iter()
            .filter(|e| {
                e.goal_id == Some(goal_id)
                    && matches!(e.action, JournalAction::Write)
                    && !denied_uris.contains(&e.uri)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_entry(uri: &str, sha: &str, action: JournalAction) -> JournalEntry {
        JournalEntry::new(uri, sha, action, None)
    }

    #[test]
    fn append_and_read() {
        let dir = tempdir().unwrap();
        let journal = UriJournal::open(dir.path()).unwrap();
        journal
            .append(&make_entry(
                "fs://workspace/a.txt",
                "aaa",
                JournalAction::Snapshot,
            ))
            .unwrap();
        journal
            .append(&make_entry(
                "fs://workspace/a.txt",
                "bbb",
                JournalAction::Write,
            ))
            .unwrap();
        let entries = journal.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].action, JournalAction::Snapshot));
        assert!(matches!(entries[1].action, JournalAction::Write));
    }

    #[test]
    fn empty_journal_returns_empty() {
        let dir = tempdir().unwrap();
        let journal = UriJournal::open(dir.path()).unwrap();
        assert!(journal.entries().unwrap().is_empty());
    }

    #[test]
    fn live_shas_excludes_denied_old_entries() {
        let dir = tempdir().unwrap();
        let journal = UriJournal::open(dir.path()).unwrap();

        // Write a recent snapshot (should be live regardless).
        journal
            .append(&make_entry(
                "fs://workspace/b.txt",
                "live_sha",
                JournalAction::Snapshot,
            ))
            .unwrap();

        // Write a denied entry (should not be live if outside window).
        let mut denied = make_entry("fs://workspace/b.txt", "dead_sha", JournalAction::Denied);
        denied.at = Utc::now() - chrono::Duration::days(60);
        journal.append(&denied).unwrap();

        let live = journal.live_shas(30).unwrap();
        assert!(live.contains("live_sha"));
        assert!(!live.contains("dead_sha"));
    }

    #[test]
    fn last_snapshot_returns_most_recent() {
        let dir = tempdir().unwrap();
        let journal = UriJournal::open(dir.path()).unwrap();
        journal
            .append(&make_entry(
                "fs://workspace/c.txt",
                "snap1",
                JournalAction::Snapshot,
            ))
            .unwrap();
        journal
            .append(&make_entry(
                "fs://workspace/c.txt",
                "snap2",
                JournalAction::Snapshot,
            ))
            .unwrap();
        let snap = journal.last_snapshot("fs://workspace/c.txt").unwrap();
        assert_eq!(snap.unwrap().sha256, "snap2");
    }

    #[test]
    fn pending_writes_excludes_denied_uris() {
        let dir = tempdir().unwrap();
        let journal = UriJournal::open(dir.path()).unwrap();
        let goal_id = Uuid::new_v4();
        let mut write = make_entry("fs://workspace/d.txt", "w1", JournalAction::Write);
        write.goal_id = Some(goal_id);
        journal.append(&write).unwrap();

        let mut deny = make_entry("fs://workspace/d.txt", "w1", JournalAction::Denied);
        deny.goal_id = Some(goal_id);
        journal.append(&deny).unwrap();

        let pending = journal.pending_writes_for_goal(goal_id).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn path_to_uri_format() {
        let p = std::path::Path::new("data/outputs/frame.png");
        assert_eq!(path_to_uri(p), "fs://workspace/data/outputs/frame.png");
    }
}
