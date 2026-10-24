// store.rs -- Event persistence: trait + filesystem NDJSON implementation.
//
// Events are stored as NDJSON (newline-delimited JSON) files rotated by date.
// Path layout: .ta/events/<YYYY-MM-DD>.jsonl

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::EventError;
use crate::identity::ProjectIdentity;
use crate::schema::EventEnvelope;

/// Fill an envelope's missing project fields. Fields already set are kept.
fn stamp(envelope: &mut EventEnvelope, id: &ProjectIdentity) {
    if envelope.project_id.is_none() {
        envelope.project_id = id.project_id.clone();
    }
    if envelope.project_root.is_none() {
        envelope.project_root = Some(id.project_root.clone());
    }
}

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

    /// Read all envelopes from a single log file. Events written before
    /// envelopes carried a project identity get this store's project filled
    /// in, so every event a consumer sees names its project.
    fn read_file(
        &self,
        path: &Path,
        identity: Option<&ProjectIdentity>,
    ) -> Result<Vec<EventEnvelope>, EventError> {
        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<EventEnvelope>(&line) {
                Ok(mut envelope) => {
                    if let Some(id) = identity {
                        stamp(&mut envelope, id);
                    }
                    events.push(envelope)
                }
                Err(e) => {
                    tracing::warn!("skipping malformed event line: {}", e);
                }
            }
        }
        Ok(events)
    }

    /// The project this store belongs to (`<root>/.ta/events` layout only).
    fn identity(&self) -> Option<ProjectIdentity> {
        ProjectIdentity::from_events_dir(&self.events_dir)
    }
}

impl EventStore for FsEventStore {
    fn append(&self, event: &EventEnvelope) -> Result<(), EventError> {
        fs::create_dir_all(&self.events_dir)?;

        let date = event.timestamp.date_naive();
        let path = self.log_path(&date);

        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

        // The single place every persisted event gets its project identity.
        let json = match self.identity() {
            Some(id) if event.project_id.is_none() || event.project_root.is_none() => {
                let mut stamped = event.clone();
                stamp(&mut stamped, &id);
                serde_json::to_string(&stamped)?
            }
            _ => serde_json::to_string(event)?,
        };
        writeln!(file, "{}", json)?;
        Ok(())
    }

    fn query(&self, filter: &EventQueryFilter) -> Result<Vec<EventEnvelope>, EventError> {
        let files = self.log_files()?;
        let identity = self.identity();
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

            let events = self.read_file(path, identity.as_ref())?;
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
                title: String::new(),
            }))
            .unwrap();
        store
            .append(&make_envelope(SessionEvent::DraftBuilt {
                goal_id: uuid::Uuid::new_v4(),
                draft_id: uuid::Uuid::new_v4(),
                artifact_count: 1,
                title: String::new(),
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

    const ID_A: &str = "p_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID_B: &str = "p_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn project(id: &str) -> tempfile::TempDir {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join(".ta")).unwrap();
        fs::write(d.path().join(".ta/project-id"), id).unwrap();
        d
    }

    fn mem_event() -> EventEnvelope {
        make_envelope(SessionEvent::MemoryStored {
            key: "k".into(),
            category: None,
            source: "cli".into(),
        })
    }

    #[test]
    fn every_appended_event_carries_its_projects_identity_on_disk_and_on_read() {
        let a = project(ID_A);
        let b = project(ID_B);
        let sa = FsEventStore::new(a.path().join(".ta/events"));
        let sb = FsEventStore::new(b.path().join(".ta/events"));
        sa.append(&mem_event()).unwrap();
        sb.append(&mem_event()).unwrap();

        let raw = fs::read_to_string(&sa.log_files().unwrap()[0]).unwrap();
        assert!(raw.contains(ID_A) && !raw.contains(ID_B), "{raw}");

        let ea = sa.query(&EventQueryFilter::default()).unwrap();
        let eb = sb.query(&EventQueryFilter::default()).unwrap();
        assert_eq!(ea[0].project_id.as_deref(), Some(ID_A));
        assert_eq!(eb[0].project_id.as_deref(), Some(ID_B));
        let root_a = fs::canonicalize(a.path()).unwrap().display().to_string();
        assert_eq!(ea[0].project_root.as_deref(), Some(root_a.as_str()));
        assert_ne!(ea[0].project_root, eb[0].project_root);
    }

    #[test]
    fn events_written_before_the_fields_existed_are_filled_on_read() {
        let a = project(ID_A);
        let store = FsEventStore::new(a.path().join(".ta/events"));
        // Old shape on disk: no project fields at all.
        let old = serde_json::to_value(mem_event()).unwrap();
        assert!(old.get("project_id").is_none());
        fs::create_dir_all(a.path().join(".ta/events")).unwrap();
        let day = chrono::Utc::now().format("%Y-%m-%d");
        fs::write(
            a.path().join(format!(".ta/events/{day}.jsonl")),
            format!("{old}\n"),
        )
        .unwrap();
        let got = store.query(&EventQueryFilter::default()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].project_id.as_deref(), Some(ID_A));
    }

    #[test]
    fn an_envelope_without_project_fields_still_parses_and_a_set_identity_is_kept() {
        let mut v = serde_json::to_value(mem_event()).unwrap();
        v.as_object_mut().unwrap().remove("actions");
        let e: EventEnvelope = serde_json::from_value(v).unwrap();
        assert!(e.project_id.is_none() && e.project_root.is_none());

        let a = project(ID_A);
        let store = FsEventStore::new(a.path().join(".ta/events"));
        let mut preset = mem_event();
        preset.project_id = Some(ID_B.into());
        preset.project_root = Some("/elsewhere".into());
        store.append(&preset).unwrap();
        let got = store.query(&EventQueryFilter::default()).unwrap();
        assert_eq!(got[0].project_id.as_deref(), Some(ID_B));
    }

    #[test]
    fn a_store_outside_the_standard_layout_invents_no_project() {
        let dir = tempdir().unwrap();
        let store = FsEventStore::new(dir.path());
        store.append(&mem_event()).unwrap();
        let got = store.query(&EventQueryFilter::default()).unwrap();
        assert!(got[0].project_id.is_none() && got[0].project_root.is_none());
    }
}
