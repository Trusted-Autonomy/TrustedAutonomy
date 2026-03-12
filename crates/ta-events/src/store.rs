// store.rs -- Event persistence: trait + filesystem NDJSON implementation.
//
// Events are stored as NDJSON (newline-delimited JSON) files rotated by date.
// Path layout: .ta/events/<YYYY-MM-DD>.jsonl

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::EventError;
use crate::schema::EventEnvelope;

/// Trait for event persistence backends.
pub trait EventStore: Send + Sync {
    /// Append an event to the store.
    fn append(&self, event: &EventEnvelope) -> Result<(), EventError>;

    /// Query events matching the given filter.
    fn query(&self, filter: &EventQueryFilter) -> Result<Vec<EventEnvelope>, EventError>;

    /// Count total events in the store.
    fn count(&self) -> Result<usize, EventError>;

    /// Prune events older than the given date (v0.10.15).
    ///
    /// Returns the number of event files removed. Only removes complete
    /// daily log files where ALL events predate the cutoff.
    fn prune(&self, before: chrono::DateTime<Utc>) -> Result<usize, EventError>;
}

/// Query filter for event retrieval.
#[derive(Debug, Clone, Default)]
pub struct EventQueryFilter {
    /// Filter by event type names.
    pub event_types: Vec<String>,
    /// Filter by goal ID.
    pub goal_id: Option<uuid::Uuid>,
    /// Filter by date range start (exclusive — returns events strictly after this time).
    pub since: Option<chrono::DateTime<Utc>>,
    /// Filter by date range end (inclusive).
    pub until: Option<chrono::DateTime<Utc>>,
    /// Maximum number of results.
    pub limit: Option<usize>,
}

/// Filesystem-based event store using NDJSON files rotated by date.
pub struct FsEventStore {
    events_dir: PathBuf,
}

impl FsEventStore {
    /// Create a new filesystem event store at the given directory.
    pub fn new(events_dir: impl AsRef<Path>) -> Self {
        Self {
            events_dir: events_dir.as_ref().to_path_buf(),
        }
    }

    /// Get the path for a specific date's event log file.
    fn log_path(&self, date: &chrono::NaiveDate) -> PathBuf {
        self.events_dir
            .join(format!("{}.jsonl", date.format("%Y-%m-%d")))
    }

    /// List all event log files sorted by date (oldest first).
    fn log_files(&self) -> Result<Vec<PathBuf>, EventError> {
        if !self.events_dir.exists() {
            return Ok(vec![]);
        }
        let mut files: Vec<PathBuf> = fs::read_dir(&self.events_dir)?
            .filter_map(|entry| entry.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|ext| ext == "jsonl").unwrap_or(false))
            .collect();
        files.sort();
        Ok(files)
    }

    /// Read all envelopes from a single log file.
    fn read_file(&self, path: &Path) -> Result<Vec<EventEnvelope>, EventError> {
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<EventEnvelope>(&line) {
                Ok(envelope) => events.push(envelope),
                Err(e) => {
                    tracing::warn!("skipping malformed event line: {}", e);
                }
            }
        }
        Ok(events)
    }
}

impl EventStore for FsEventStore {
    fn append(&self, event: &EventEnvelope) -> Result<(), EventError> {
        fs::create_dir_all(&self.events_dir)?;

        let date = event.timestamp.date_naive();
        let path = self.log_path(&date);

        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

        let json = serde_json::to_string(event)?;
        writeln!(file, "{}", json)?;
        Ok(())
    }

    fn query(&self, filter: &EventQueryFilter) -> Result<Vec<EventEnvelope>, EventError> {
        let files = self.log_files()?;
        let mut results = Vec::new();

        for path in &files {
            // Optimization: skip files outside the date range based on filename.
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(file_date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
                    if let Some(since) = filter.since {
                        if file_date < since.date_naive() {
                            continue;
                        }
                    }
                    if let Some(until) = filter.until {
                        if file_date > until.date_naive() {
                            continue;
                        }
                    }
                }
            }

            let events = self.read_file(path)?;
            for envelope in events {
                // Apply type filter.
                if !filter.event_types.is_empty()
                    && !filter.event_types.contains(&envelope.event_type)
                {
                    continue;
                }
                // Apply goal filter.
                if let Some(goal_id) = filter.goal_id {
                    if envelope.payload.goal_id() != Some(goal_id) {
                        continue;
                    }
                }
                // Apply time range filters.
                // v0.9.5.1: Use strictly-after (>) for `since` so cursor-based
                // polling doesn't re-fetch the last event every time.
                if let Some(since) = filter.since {
                    if envelope.timestamp <= since {
                        continue;
                    }
                }
                if let Some(until) = filter.until {
                    if envelope.timestamp > until {
                        continue;
                    }
                }

                results.push(envelope);

                if let Some(limit) = filter.limit {
                    if results.len() >= limit {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    fn count(&self) -> Result<usize, EventError> {
        let files = self.log_files()?;
        let mut total = 0;
        for path in &files {
            let file = fs::File::open(path)?;
            let reader = BufReader::new(file);
            total += reader.lines().filter(|l| l.is_ok()).count();
        }
        Ok(total)
    }

    fn prune(&self, before: chrono::DateTime<Utc>) -> Result<usize, EventError> {
        let cutoff_date = before.date_naive();
        let files = self.log_files()?;
        let mut removed = 0;

        for path in &files {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(file_date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
                    if file_date < cutoff_date && fs::remove_file(path).is_ok() {
                        removed += 1;
                    }
                }
            }
        }

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SessionEvent;
    use tempfile::tempdir;

    fn make_envelope(event: SessionEvent) -> EventEnvelope {
        EventEnvelope::new(event)
    }

    #[test]
    fn append_and_query_all() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        let e1 = make_envelope(SessionEvent::GoalStarted {
            goal_id: uuid::Uuid::new_v4(),
            title: "Test 1".into(),
            agent_id: "a".into(),
            phase: None,
        });
        let e2 = make_envelope(SessionEvent::MemoryStored {
            key: "k".into(),
            category: None,
            source: "cli".into(),
        });

        store.append(&e1).unwrap();
        store.append(&e2).unwrap();

        let all = store.query(&EventQueryFilter::default()).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn query_by_type() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        store
            .append(&make_envelope(SessionEvent::GoalStarted {
                goal_id: uuid::Uuid::new_v4(),
                title: "t".into(),
                agent_id: "a".into(),
                phase: None,
            }))
            .unwrap();
        store
            .append(&make_envelope(SessionEvent::MemoryStored {
                key: "k".into(),
                category: None,
                source: "cli".into(),
            }))
            .unwrap();

        let filter = EventQueryFilter {
            event_types: vec!["goal_started".into()],
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_type, "goal_started");
    }

    #[test]
    fn query_by_goal() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        let gid = uuid::Uuid::new_v4();
        store
            .append(&make_envelope(SessionEvent::DraftBuilt {
                goal_id: gid,
                draft_id: uuid::Uuid::new_v4(),
                artifact_count: 3,
            }))
            .unwrap();
        store
            .append(&make_envelope(SessionEvent::DraftBuilt {
                goal_id: uuid::Uuid::new_v4(),
                draft_id: uuid::Uuid::new_v4(),
                artifact_count: 1,
            }))
            .unwrap();

        let filter = EventQueryFilter {
            goal_id: Some(gid),
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn query_with_limit() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        for _ in 0..5 {
            store
                .append(&make_envelope(SessionEvent::MemoryStored {
                    key: "k".into(),
                    category: None,
                    source: "cli".into(),
                }))
                .unwrap();
        }

        let filter = EventQueryFilter {
            limit: Some(3),
            ..Default::default()
        };
        let results = store.query(&filter).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn count_events() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        assert_eq!(store.count().unwrap(), 0);

        for _ in 0..4 {
            store
                .append(&make_envelope(SessionEvent::MemoryStored {
                    key: "k".into(),
                    category: None,
                    source: "cli".into(),
                }))
                .unwrap();
        }

        assert_eq!(store.count().unwrap(), 4);
    }

    #[test]
    fn empty_store_returns_empty() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path().join("nonexistent"));
        let results = store.query(&EventQueryFilter::default()).unwrap();
        assert!(results.is_empty());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn prune_removes_old_files() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        // Create events that will be in today's file.
        store
            .append(&make_envelope(SessionEvent::MemoryStored {
                key: "k".into(),
                category: None,
                source: "cli".into(),
            }))
            .unwrap();

        // Manually create an old file.
        let old_date = "2025-01-01";
        let old_path = dir.path().join(format!("{}.jsonl", old_date));
        std::fs::write(&old_path, "{}\n").unwrap();

        let files_before = store.log_files().unwrap();
        assert_eq!(files_before.len(), 2);

        // Prune everything before today.
        let cutoff = chrono::Utc::now();
        let removed = store.prune(cutoff).unwrap();
        assert_eq!(removed, 1);

        let files_after = store.log_files().unwrap();
        assert_eq!(files_after.len(), 1);
    }

    #[test]
    fn prune_keeps_recent_files() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        store
            .append(&make_envelope(SessionEvent::MemoryStored {
                key: "k".into(),
                category: None,
                source: "cli".into(),
            }))
            .unwrap();

        // Prune with cutoff in the past — nothing should be removed.
        let cutoff = chrono::Utc::now() - chrono::Duration::days(365);
        let removed = store.prune(cutoff).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn ndjson_format() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());

        store
            .append(&make_envelope(SessionEvent::MemoryStored {
                key: "k".into(),
                category: None,
                source: "cli".into(),
            }))
            .unwrap();

        // Verify the file is proper NDJSON (one JSON object per line).
        let files = store.log_files().unwrap();
        assert_eq!(files.len(), 1);

        let content = fs::read_to_string(&files[0]).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        // Each line should be valid JSON.
        let _: EventEnvelope = serde_json::from_str(lines[0]).unwrap();
    }
}
