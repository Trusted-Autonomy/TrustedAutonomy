// memory.rs — Memory bridge for ta-agent-ollama.
//
// Reads from a pre-written snapshot file (TA_MEMORY_PATH / --memory-path)
// and writes new entries to an exit-file (TA_MEMORY_OUT / --memory-out).
//
// The snapshot is a markdown file written by TA before agent launch.
// The exit-file is a JSON array ingested by TA after the agent exits.
//
// JSON format for exit-file (compatible with ingest_memory_out in run.rs):
//
// [
//   {
//     "key":   "arch:module-map:my-service",
//     "value": "The main entry point is src/main.rs",
//     "tags":  ["architecture", "module-map"],
//     "category": "architecture"
//   },
//   ...
// ]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A memory entry to be written to the exit-file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryWriteEntry {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
}

/// Bridge between the agent and TA's memory system.
///
/// Reads entries from a snapshot file and accumulates writes in memory
/// until `flush()` is called on agent exit.
pub struct MemoryBridge {
    /// Path to the snapshot markdown file (read-only).
    snapshot_path: Option<PathBuf>,
    /// Path to the exit-file to write new entries into.
    out_path: Option<PathBuf>,
    /// Accumulated memory entries to write on exit.
    pending: Mutex<Vec<MemoryWriteEntry>>,
}

impl MemoryBridge {
    pub fn new(out_path: Option<&Path>) -> Self {
        Self {
            snapshot_path: None,
            out_path: out_path.map(PathBuf::from),
            pending: Mutex::new(Vec::new()),
        }
    }

    #[allow(dead_code)]
    pub fn with_snapshot(mut self, snapshot_path: Option<&Path>) -> Self {
        self.snapshot_path = snapshot_path.map(PathBuf::from);
        self
    }

    /// Read a memory entry by key from the snapshot.
    ///
    /// Searches for a line like `- **[category] key**: value` in the markdown.
    /// Returns None if the key is not found.
    pub fn read(&self, key: &str) -> Option<String> {
        let snap_path = self.snapshot_path.as_deref()?;
        let content = std::fs::read_to_string(snap_path).ok()?;

        // Search for the key in the markdown snapshot.
        // Format: `- **[category] key**: value` or `- **key**: value`
        for line in content.lines() {
            if line.contains(key) {
                // Extract the value after the last `**: ` on the line.
                if let Some(pos) = line.rfind("**: ") {
                    let value = line[pos + 4..].trim();
                    return Some(value.to_string());
                }
                // Fallback: return the whole line.
                return Some(line.trim_start_matches("- ").to_string());
            }
        }
        None
    }

    /// Search memory entries matching a query string.
    ///
    /// Returns entries whose key or value contains the query (case-insensitive).
    pub fn search(&self, query: &str) -> Vec<(String, String)> {
        let Some(snap_path) = self.snapshot_path.as_deref() else {
            return Vec::new();
        };
        let Ok(content) = std::fs::read_to_string(snap_path) else {
            return Vec::new();
        };

        let query_lower = query.to_lowercase();
        let mut results = Vec::new();

        for line in content.lines() {
            if line.to_lowercase().contains(&query_lower) {
                // Try to extract key: value from `- **[cat] key**: value`
                if let Some(bold_start) = line.find("**") {
                    if let Some(bold_end) = line[bold_start + 2..].find("**") {
                        let key = line[bold_start + 2..bold_start + 2 + bold_end].to_string();
                        let rest = &line[bold_start + 2 + bold_end + 2..];
                        let value = rest.trim_start_matches(": ").to_string();
                        results.push((key, value));
                        continue;
                    }
                }
                results.push(("(unknown key)".to_string(), line.trim().to_string()));
            }
        }
        results
    }

    /// Queue a memory entry for writing to the exit-file on flush.
    pub fn write(&self, key: String, value: String, tags: Vec<String>, category: Option<String>) {
        let entry = MemoryWriteEntry {
            key,
            value,
            tags,
            category,
        };
        self.pending.lock().unwrap().push(entry);
    }

    /// Flush pending memory entries to the exit-file.
    ///
    /// If no out_path is configured, this is a no-op (entries are silently discarded).
    pub fn flush(&self) -> Result<()> {
        let pending = {
            let mut guard = self.pending.lock().unwrap();
            std::mem::take(&mut *guard)
        };

        if pending.is_empty() {
            return Ok(());
        }

        let Some(out_path) = self.out_path.as_deref() else {
            tracing::debug!(
                "No TA_MEMORY_OUT configured — {} memory entries discarded",
                pending.len()
            );
            return Ok(());
        };

        let json = serde_json::to_string_pretty(&pending)
            .map_err(|e| anyhow::anyhow!("Failed to serialize memory entries: {}", e))?;

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("Failed to create memory output directory: {}", e))?;
        }

        std::fs::write(out_path, &json)
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", out_path.display(), e))?;

        tracing::debug!(
            path = %out_path.display(),
            count = pending.len(),
            "Flushed memory entries to exit-file"
        );

        Ok(())
    }

    /// Parse a memory_write tool call argument value.
    /// Accepts either a string or a JSON value; returns the string form.
    pub fn parse_tags(tags_arg: &Value) -> Vec<String> {
        match tags_arg {
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            Value::String(s) => s.split(',').map(|t| t.trim().to_string()).collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn memory_bridge_flush_no_out_path() {
        let bridge = MemoryBridge::new(None);
        bridge.write("k".to_string(), "v".to_string(), vec![], None);
        assert!(bridge.flush().is_ok()); // should not error
    }

    #[test]
    fn memory_bridge_flush_writes_json() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("memory_out.json");
        let bridge = MemoryBridge::new(Some(&out));
        bridge.write(
            "arch:foo".to_string(),
            "bar".to_string(),
            vec!["architecture".to_string()],
            Some("architecture".to_string()),
        );
        bridge.flush().unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        let parsed: Vec<MemoryWriteEntry> = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].key, "arch:foo");
        assert_eq!(parsed[0].value, "bar");
        assert_eq!(parsed[0].tags, vec!["architecture"]);
    }

    #[test]
    fn memory_bridge_flush_empty_no_file() {
        let dir = tempdir().unwrap();
        let out = dir.path().join("memory_out.json");
        let bridge = MemoryBridge::new(Some(&out));
        bridge.flush().unwrap();
        // No entries → file should not be created.
        assert!(!out.exists());
    }

    #[test]
    fn memory_read_from_snapshot() {
        let dir = tempdir().unwrap();
        let snap = dir.path().join("snapshot.md");
        std::fs::write(
            &snap,
            "## Prior Context\n\n- **[architecture] arch:module-map**: Entry point is src/main.rs\n",
        )
        .unwrap();
        let bridge = MemoryBridge::new(None).with_snapshot(Some(&snap));
        let result = bridge.read("arch:module-map");
        assert_eq!(result, Some("Entry point is src/main.rs".to_string()));
    }

    #[test]
    fn memory_read_missing_key() {
        let dir = tempdir().unwrap();
        let snap = dir.path().join("snapshot.md");
        std::fs::write(
            &snap,
            "## Prior Context\n\n- **[architecture] arch:foo**: bar\n",
        )
        .unwrap();
        let bridge = MemoryBridge::new(None).with_snapshot(Some(&snap));
        assert!(bridge.read("nonexistent-key").is_none());
    }

    #[test]
    fn memory_search_finds_matching_entries() {
        let dir = tempdir().unwrap();
        let snap = dir.path().join("snapshot.md");
        std::fs::write(
            &snap,
            "## Prior Context\n\n\
             - **[architecture] arch:api**: REST API uses axum\n\
             - **[convention] conv:naming**: Use snake_case everywhere\n",
        )
        .unwrap();
        let bridge = MemoryBridge::new(None).with_snapshot(Some(&snap));
        let results = bridge.search("axum");
        assert!(!results.is_empty());
        assert!(results[0].1.contains("axum"));
    }

    #[test]
    fn parse_tags_from_array() {
        let tags = MemoryBridge::parse_tags(&serde_json::json!(["architecture", "module"]));
        assert_eq!(tags, vec!["architecture", "module"]);
    }

    #[test]
    fn parse_tags_from_string() {
        let tags = MemoryBridge::parse_tags(&serde_json::json!("arch, module"));
        assert_eq!(tags, vec!["arch", "module"]);
    }
}
