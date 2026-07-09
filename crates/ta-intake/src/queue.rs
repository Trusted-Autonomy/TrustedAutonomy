//! The `.ta/intake-queue.jsonl` batch queue: `TriggerEvent`s whose config's
//! `dispatch = "queue"` (see `manifest::Dispatch`) defers goal creation for
//! later processing instead of creating one immediately.
//!
//! Owned here (not duplicated per consumer) so both the `ta intake fire`/
//! `ta intake queue` CLI glue and the `ta-advisor` team-coordinator
//! extension (v0.17.0.12.20, `docs/design/ta-concepts-and-architecture.md`
//! §3) read/write the same file the same way.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::event::TriggerEvent;

/// Path to the batch queue file for `project_root`.
pub fn queue_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("intake-queue.jsonl")
}

/// Read all queued events, in file order (oldest-appended first).
/// Returns an empty `Vec` if the queue file doesn't exist yet.
pub fn read_queue(project_root: &Path) -> Vec<TriggerEvent> {
    let path = queue_path(project_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Append `events` to the queue file, creating it (and `.ta/`) if needed.
pub fn append_to_queue(project_root: &Path, events: &[TriggerEvent]) -> io::Result<()> {
    let path = queue_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for event in events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }
    Ok(())
}

/// Overwrite the queue file with exactly `events` — used after dispatching
/// a subset of queued events, to remove only those from the queue.
pub fn write_queue(project_root: &Path, events: &[TriggerEvent]) -> io::Result<()> {
    let path = queue_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = String::new();
    for event in events {
        out.push_str(&serde_json::to_string(event)?);
        out.push('\n');
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn make_event(title: &str) -> TriggerEvent {
        TriggerEvent {
            id: Uuid::new_v4(),
            trigger_type: "schedule".to_string(),
            source: "test".to_string(),
            occurred_at: Utc::now(),
            payload: serde_json::json!({}),
            suggested_goal_title: title.to_string(),
            dedupe_key: None,
        }
    }

    #[test]
    fn read_queue_missing_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_queue(tmp.path()).is_empty());
    }

    #[test]
    fn append_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let events = vec![make_event("a"), make_event("b")];
        append_to_queue(tmp.path(), &events).unwrap();
        let read = read_queue(tmp.path());
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].suggested_goal_title, "a");
        assert_eq!(read[1].suggested_goal_title, "b");
    }

    #[test]
    fn write_queue_replaces_contents() {
        let tmp = tempfile::tempdir().unwrap();
        append_to_queue(tmp.path(), &[make_event("a"), make_event("b")]).unwrap();
        write_queue(tmp.path(), &[make_event("c")]).unwrap();
        let read = read_queue(tmp.path());
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].suggested_goal_title, "c");
    }
}
