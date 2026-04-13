// human_review.rs — Human Review Item tracking (v0.15.14.1).
//
// When `ta draft apply` marks a plan phase as done, it extracts items from the
// `#### Human Review` subsection of that phase and appends them to
// `.ta/human-review.jsonl`. These items require a human to verify, test, or
// sign off — an agent must never check them off.
//
// CLI surface: `ta plan review` lists/completes/defers these items.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Record ───────────────────────────────────────────────────────

/// Status of a human review item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanReviewStatus {
    Pending,
    Complete,
    Deferred,
}

impl std::fmt::Display for HumanReviewStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HumanReviewStatus::Pending => write!(f, "pending"),
            HumanReviewStatus::Complete => write!(f, "complete"),
            HumanReviewStatus::Deferred => write!(f, "deferred"),
        }
    }
}

/// One human review item extracted from a plan phase's `#### Human Review` subsection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanReviewRecord {
    /// Plan phase ID (e.g. "v0.15.3").
    pub phase: String,
    /// 0-based index within the phase's human review items.
    pub idx: usize,
    /// The item text (without the leading `- [ ] ` prefix).
    pub item: String,
    /// Current status.
    pub status: HumanReviewStatus,
    /// When this record was created (i.e., when the phase was applied).
    pub created_at: DateTime<Utc>,
    /// For `deferred` items: which phase they were deferred to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_to: Option<String>,
}

// ── Store ────────────────────────────────────────────────────────

/// JSONL-backed store at `.ta/human-review.jsonl`.
///
/// Append-only: each mutation appends a new record that overwrites
/// the canonical state for `(phase, idx)`.
pub struct HumanReviewStore {
    path: PathBuf,
}

impl HumanReviewStore {
    /// Open the store rooted at `project_root`.
    pub fn new(project_root: &Path) -> Self {
        Self {
            path: project_root.join(".ta/human-review.jsonl"),
        }
    }

    /// Append a new pending review item.
    pub fn append(&self, phase: &str, idx: usize, item_text: &str) -> anyhow::Result<()> {
        let record = HumanReviewRecord {
            phase: phase.to_string(),
            idx,
            item: item_text.to_string(),
            status: HumanReviewStatus::Pending,
            created_at: Utc::now(),
            deferred_to: None,
        };
        self.write_record(&record)
    }

    /// Load all records, collapsing to the latest state per `(phase, idx)`.
    pub fn list(&self) -> anyhow::Result<Vec<HumanReviewRecord>> {
        self.load_all()
    }

    /// Return only records with `status = pending`.
    pub fn pending(&self) -> anyhow::Result<Vec<HumanReviewRecord>> {
        Ok(self
            .load_all()?
            .into_iter()
            .filter(|r| r.status == HumanReviewStatus::Pending)
            .collect())
    }

    /// Mark item `idx` in `phase` as complete.
    pub fn complete(&self, phase: &str, idx: usize) -> anyhow::Result<()> {
        let records = self.load_all()?;
        let canonical = records
            .iter()
            .find(|r| r.phase == phase && r.idx == idx)
            .ok_or_else(|| {
                anyhow::anyhow!("No human review item found for phase={} idx={}", phase, idx)
            })?
            .clone();
        let updated = HumanReviewRecord {
            status: HumanReviewStatus::Complete,
            ..canonical
        };
        self.write_record(&updated)
    }

    /// Mark item `idx` in `phase` as deferred to `to_phase`.
    pub fn defer(&self, phase: &str, idx: usize, to_phase: &str) -> anyhow::Result<()> {
        let records = self.load_all()?;
        let canonical = records
            .iter()
            .find(|r| r.phase == phase && r.idx == idx)
            .ok_or_else(|| {
                anyhow::anyhow!("No human review item found for phase={} idx={}", phase, idx)
            })?
            .clone();
        let updated = HumanReviewRecord {
            status: HumanReviewStatus::Deferred,
            deferred_to: Some(to_phase.to_string()),
            ..canonical
        };
        self.write_record(&updated)
    }

    // ── Internal ─────────────────────────────────────────────────

    /// Write a record to the JSONL file. Creates parent directories as needed.
    fn write_record(&self, record: &HumanReviewRecord) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("create .ta/ dir: {}", e))?;
        }
        let line = serde_json::to_string(record)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| anyhow::anyhow!("open human-review.jsonl: {}", e))?;
        writeln!(file, "{}", line)
            .map_err(|e| anyhow::anyhow!("write human-review.jsonl: {}", e))?;
        Ok(())
    }

    /// Load all records and collapse to the canonical state per `(phase, idx)`.
    ///
    /// Later records in the file win (last-write-wins for each key).
    fn load_all(&self) -> anyhow::Result<Vec<HumanReviewRecord>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let file = std::fs::File::open(&self.path)
            .map_err(|e| anyhow::anyhow!("open human-review.jsonl: {}", e))?;
        let reader = BufReader::new(file);

        // Use a Vec<(key, record)> to preserve insertion order for display,
        // while applying last-write-wins semantics.
        let mut ordered_keys: Vec<(String, usize)> = Vec::new();
        let mut map: std::collections::HashMap<(String, usize), HumanReviewRecord> =
            std::collections::HashMap::new();

        for line in reader.lines() {
            let line = line.map_err(|e| anyhow::anyhow!("read human-review.jsonl: {}", e))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record: HumanReviewRecord = match serde_json::from_str(trimmed) {
                Ok(r) => r,
                Err(_) => continue, // skip malformed lines
            };
            let key = (record.phase.clone(), record.idx);
            if !map.contains_key(&key) {
                ordered_keys.push(key.clone());
            }
            map.insert(key, record);
        }

        Ok(ordered_keys
            .into_iter()
            .filter_map(|k| map.remove(&k))
            .collect())
    }
}

// ── Plan content parser ──────────────────────────────────────────

/// Extract human review items from a plan phase's `#### Human Review` subsection.
///
/// Returns items that are still unchecked (`- [ ]`). Checked items (`- [x]`)
/// are skipped — they were already verified.
pub fn extract_human_review_items(
    plan_content: &str,
    phase_id: &str,
    phase_title: &str,
) -> Vec<String> {
    let lines: Vec<&str> = plan_content.lines().collect();

    // Locate the phase header.
    let phase_start = find_phase_start(&lines, phase_id, phase_title);
    let phase_start = match phase_start {
        Some(idx) => idx,
        None => return vec![],
    };

    // Scan from phase_start to find `#### Human Review` heading.
    let mut in_human_review = false;
    let mut items = Vec::new();
    let mut i = phase_start + 1;

    while i < lines.len() {
        let line = lines[i].trim();

        // Stop if we hit the next phase (### or ##) or another section at the same level.
        if (line.starts_with("## ") || line.starts_with("### ")) && i > phase_start {
            break;
        }

        // Detect the `#### Human Review` heading.
        if line == "#### Human Review" {
            in_human_review = true;
            i += 1;
            continue;
        }

        // Once in human review, stop at the next #### heading.
        if in_human_review && line.starts_with("#### ") && line != "#### Human Review" {
            break;
        }

        if in_human_review {
            // Collect unchecked items.
            if let Some(rest) = line.strip_prefix("- [ ] ") {
                items.push(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("- [ ]") {
                // Handle `- [ ]` with no space after (bare checkbox).
                let t = rest.trim();
                if !t.is_empty() {
                    items.push(t.to_string());
                }
            }
            // Checked items (`- [x]`, `- [X]`) are intentionally skipped.
        }

        i += 1;
    }

    items
}

/// Locate the line index of a phase header in plan content.
///
/// Matches by ID (with or without leading `v`) and title. Both must appear in the same
/// `### <id> — <title>` line.
fn find_phase_start(lines: &[&str], phase_id: &str, phase_title: &str) -> Option<usize> {
    let id_norm = phase_id.strip_prefix('v').unwrap_or(phase_id);
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // Match either "### v0.15.3 — Title" or "### 0.15.3 — Title".
        if !trimmed.starts_with("### ") {
            continue;
        }
        let rest = &trimmed[4..];
        // Check that the ID appears at the start of rest (with or without 'v').
        let has_id = rest.starts_with(phase_id)
            || rest.starts_with(&format!("v{}", id_norm))
            || rest.starts_with(id_norm);
        let has_title = rest.contains(phase_title) || phase_title.is_empty();
        if has_id && has_title {
            return Some(idx);
        }
    }
    None
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_store(dir: &std::path::Path) -> HumanReviewStore {
        HumanReviewStore::new(dir)
    }

    #[test]
    fn append_and_list_roundtrip() {
        let dir = tempdir().unwrap();
        let store = make_store(dir.path());

        store
            .append("v0.15.3", 0, "Smoke-test connector in Editor")
            .unwrap();
        store.append("v0.15.3", 1, "Confirm UX wording").unwrap();

        let records = store.list().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].phase, "v0.15.3");
        assert_eq!(records[0].idx, 0);
        assert_eq!(records[0].item, "Smoke-test connector in Editor");
        assert_eq!(records[0].status, HumanReviewStatus::Pending);
        assert!(records[0].deferred_to.is_none());
    }

    #[test]
    fn complete_updates_status() {
        let dir = tempdir().unwrap();
        let store = make_store(dir.path());

        store.append("v0.15.3", 0, "Smoke-test").unwrap();
        store.complete("v0.15.3", 0).unwrap();

        let records = store.list().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, HumanReviewStatus::Complete);
    }

    #[test]
    fn defer_updates_status_and_target() {
        let dir = tempdir().unwrap();
        let store = make_store(dir.path());

        store.append("v0.15.3", 0, "Smoke-test").unwrap();
        store.defer("v0.15.3", 0, "v0.15.4").unwrap();

        let records = store.list().unwrap();
        assert_eq!(records[0].status, HumanReviewStatus::Deferred);
        assert_eq!(records[0].deferred_to.as_deref(), Some("v0.15.4"));
    }

    #[test]
    fn pending_filters_correctly() {
        let dir = tempdir().unwrap();
        let store = make_store(dir.path());

        store.append("v0.15.3", 0, "Item A").unwrap();
        store.append("v0.15.3", 1, "Item B").unwrap();
        store.complete("v0.15.3", 0).unwrap();

        let pending = store.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].item, "Item B");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let store = make_store(dir.path());
        let records = store.list().unwrap();
        assert!(records.is_empty());
        let pending = store.pending().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn complete_nonexistent_item_returns_error() {
        let dir = tempdir().unwrap();
        let store = make_store(dir.path());
        let result = store.complete("v0.15.3", 99);
        assert!(result.is_err());
    }

    #[test]
    fn extract_items_from_phase() {
        let plan = r#"
### v0.15.3 — Some Phase
<!-- status: done -->

#### Items
- [x] Agent writes code
- [x] Tests pass in CI

#### Human Review
- [ ] Smoke-test the connector against a real project
- [ ] Confirm UX wording with stakeholder
- [x] Already verified item

### v0.15.4 — Next Phase
<!-- status: pending -->
"#;
        let items = extract_human_review_items(plan, "v0.15.3", "Some Phase");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], "Smoke-test the connector against a real project");
        assert_eq!(items[1], "Confirm UX wording with stakeholder");
    }

    #[test]
    fn extract_items_no_human_review_section() {
        let plan = r#"
### v0.15.3 — Some Phase
<!-- status: done -->

#### Items
- [x] Agent writes code

### v0.15.4 — Next Phase
<!-- status: pending -->
"#;
        let items = extract_human_review_items(plan, "v0.15.3", "Some Phase");
        assert!(items.is_empty());
    }

    #[test]
    fn extract_items_stops_at_next_phase() {
        let plan = r#"
### v0.15.3 — Phase A
<!-- status: done -->

#### Human Review
- [ ] Item from phase A

### v0.15.4 — Phase B
<!-- status: pending -->

#### Human Review
- [ ] Item from phase B
"#;
        let items = extract_human_review_items(plan, "v0.15.3", "Phase A");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], "Item from phase A");
    }

    #[test]
    fn phase_id_without_v_prefix_also_matches() {
        let plan = r#"
### v0.15.3 — Some Phase
<!-- status: done -->

#### Human Review
- [ ] Do the thing

"#;
        // Pass phase_id without 'v' prefix.
        let items = extract_human_review_items(plan, "0.15.3", "Some Phase");
        assert_eq!(items.len(), 1);
    }
}
