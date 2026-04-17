// conflict.rs — Memory conflict resolution strategies (v0.15.13.3).
//
// When `.ta/project-memory/` entries with the same key appear from two different
// VCS branches, a ConflictPair is produced. This module provides:
//
// - `TimestampResolver`: last-write-wins for entries with timestamps >60s apart
// - `AgentResolver`: stub — escalates to human (full LLM synthesis is a future phase)
// - `ConflictResolverConfig`: parsed from [memory.conflict_resolution] in workflow.toml

use std::cmp::Reverse;

use serde::{Deserialize, Serialize};

use crate::store::{ConflictPair, ConflictResolution, MemoryConflictResolver};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for the conflict resolution pipeline.
///
/// Parsed from `.ta/workflow.toml`:
/// ```toml
/// [memory.conflict_resolution]
/// strategy = "agent"           # "timestamp" | "agent" | "human" | plugin name
/// agent_confidence_threshold = 0.85
/// escalate_to_human = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolverConfig {
    /// Resolution strategy to use.
    /// - `"timestamp"`: last-write-wins (default, no-agent)
    /// - `"agent"`: LLM synthesis (escalates to human when confidence < threshold)
    /// - `"human"`: always escalate to human
    #[serde(default = "default_strategy")]
    pub strategy: String,
    /// Minimum confidence from the agent resolver to auto-accept (0.0–1.0).
    #[serde(default = "default_confidence_threshold")]
    pub agent_confidence_threshold: f64,
    /// Whether to escalate to human when agent confidence is below threshold.
    #[serde(default = "default_true")]
    pub escalate_to_human: bool,
    /// Minimum seconds between timestamps to treat as a clear winner.
    /// Below this threshold, both timestamp-resolver and agent-resolver are called.
    #[serde(default = "default_timestamp_threshold_secs")]
    pub timestamp_threshold_secs: i64,
}

fn default_strategy() -> String {
    "timestamp".to_string()
}
fn default_confidence_threshold() -> f64 {
    0.85
}
fn default_true() -> bool {
    true
}
fn default_timestamp_threshold_secs() -> i64 {
    60
}

impl Default for ConflictResolverConfig {
    fn default() -> Self {
        Self {
            strategy: default_strategy(),
            agent_confidence_threshold: default_confidence_threshold(),
            escalate_to_human: true,
            timestamp_threshold_secs: default_timestamp_threshold_secs(),
        }
    }
}

// ---------------------------------------------------------------------------
// TimestampResolver
// ---------------------------------------------------------------------------

/// Last-write-wins conflict resolver (default strategy).
///
/// When two entries differ by more than `threshold_secs` seconds, automatically
/// accepts the newer one. When timestamps are close (within threshold), escalates
/// to human review.
pub struct TimestampResolver {
    /// Minimum seconds gap required to auto-accept the newer entry.
    pub threshold_secs: i64,
}

impl Default for TimestampResolver {
    fn default() -> Self {
        Self { threshold_secs: 60 }
    }
}

impl MemoryConflictResolver for TimestampResolver {
    fn resolve(&self, conflict: &ConflictPair) -> ConflictResolution {
        let ours_ts = conflict.ours.updated_at;
        let theirs_ts = conflict.theirs.updated_at;
        let diff_secs = (ours_ts - theirs_ts).num_seconds().abs();

        if diff_secs > self.threshold_secs {
            // Clear winner: take the newer entry.
            if ours_ts >= theirs_ts {
                ConflictResolution::AcceptOurs
            } else {
                ConflictResolution::AcceptTheirs
            }
        } else {
            // Timestamps too close — cannot auto-resolve. Escalate to human.
            ConflictResolution::EscalateToHuman {
                reason: format!(
                    "Timestamps differ by only {}s (threshold: {}s). \
                     Manual review required for key '{}'.",
                    diff_secs, self.threshold_secs, conflict.key
                ),
            }
        }
    }

    fn name(&self) -> &str {
        "timestamp"
    }
}

// ---------------------------------------------------------------------------
// AgentResolver (stub)
// ---------------------------------------------------------------------------

/// LLM-based conflict resolver (stub implementation).
///
/// Full agent-driven synthesis is a future enhancement. This stub always
/// escalates to human review with a message explaining how to enable it.
pub struct AgentResolver;

impl MemoryConflictResolver for AgentResolver {
    fn resolve(&self, conflict: &ConflictPair) -> ConflictResolution {
        ConflictResolution::EscalateToHuman {
            reason: format!(
                "Agent resolution for key '{}' is not yet available. \
                 Review both versions manually with `ta memory conflicts`. \
                 To configure auto-resolution, set strategy = \"timestamp\" in \
                 [memory.conflict_resolution] in .ta/workflow.toml.",
                conflict.key
            ),
        }
    }

    fn name(&self) -> &str {
        "agent"
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build the appropriate resolver from config.
pub fn resolver_from_config(config: &ConflictResolverConfig) -> Box<dyn MemoryConflictResolver> {
    match config.strategy.as_str() {
        "agent" => Box::new(AgentResolver),
        "human" => Box::new(HumanResolver),
        _ => Box::new(TimestampResolver {
            threshold_secs: config.timestamp_threshold_secs,
        }),
    }
}

/// Always-escalate resolver. Use when strategy = "human".
pub struct HumanResolver;

impl MemoryConflictResolver for HumanResolver {
    fn resolve(&self, conflict: &ConflictPair) -> ConflictResolution {
        ConflictResolution::EscalateToHuman {
            reason: format!(
                "Strategy is set to 'human'. Review both versions manually \
                 with `ta memory conflicts` for key '{}'.",
                conflict.key
            ),
        }
    }

    fn name(&self) -> &str {
        "human"
    }
}

// ---------------------------------------------------------------------------
// Conflict store helpers
// ---------------------------------------------------------------------------

/// Directory within `.ta/project-memory/` where unresolved conflicts are stored.
pub const CONFLICTS_SUBDIR: &str = ".conflicts";

/// Write a `ConflictPair` to `.ta/project-memory/.conflicts/<key>.json`.
pub fn write_conflict(
    project_memory_dir: &std::path::Path,
    conflict: &ConflictPair,
) -> std::io::Result<()> {
    let conflicts_dir = project_memory_dir.join(CONFLICTS_SUBDIR);
    std::fs::create_dir_all(&conflicts_dir)?;

    let filename = crate::fs_store::FsMemoryStore::key_to_filename_pub(&conflict.key);
    let path = conflicts_dir.join(filename);
    let content = serde_json::to_string_pretty(conflict).map_err(std::io::Error::other)?;
    std::fs::write(path, content)
}

/// Load all unresolved `ConflictPair`s from `.ta/project-memory/.conflicts/`.
pub fn load_conflicts(project_memory_dir: &std::path::Path) -> Vec<ConflictPair> {
    let conflicts_dir = project_memory_dir.join(CONFLICTS_SUBDIR);
    if !conflicts_dir.exists() {
        return vec![];
    }

    let mut conflicts = Vec::new();
    let Ok(entries) = std::fs::read_dir(&conflicts_dir) else {
        return vec![];
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(conflict) = serde_json::from_str::<ConflictPair>(&content) {
                    conflicts.push(conflict);
                }
            }
        }
    }
    conflicts.sort_by_key(|c| Reverse(c.detected_at));
    conflicts
}

/// Remove a resolved conflict from `.ta/project-memory/.conflicts/`.
pub fn remove_conflict(project_memory_dir: &std::path::Path, key: &str) {
    let conflicts_dir = project_memory_dir.join(CONFLICTS_SUBDIR);
    let filename = crate::fs_store::FsMemoryStore::key_to_filename_pub(key);
    let path = conflicts_dir.join(filename);
    let _ = std::fs::remove_file(path);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemoryCategory, MemoryEntry};
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    fn make_entry(key: &str, value: &str, updated_at: chrono::DateTime<Utc>) -> MemoryEntry {
        let now = Utc::now();
        MemoryEntry {
            entry_id: Uuid::new_v4(),
            key: key.to_string(),
            value: serde_json::Value::String(value.to_string()),
            tags: vec![],
            source: "test".to_string(),
            goal_id: None,
            category: Some(MemoryCategory::Architecture),
            expires_at: None,
            confidence: 0.9,
            phase_id: None,
            scope: Some("project".to_string()),
            file_paths: vec![],
            created_at: now,
            updated_at,
        }
    }

    #[test]
    fn timestamp_resolver_accepts_newer_ours() {
        let now = Utc::now();
        let conflict = ConflictPair {
            key: "arch:design".to_string(),
            ours: make_entry("arch:design", "new value", now),
            theirs: make_entry("arch:design", "old value", now - Duration::seconds(120)),
            base: None,
            detected_at: now,
        };
        let resolver = TimestampResolver::default();
        let result = resolver.resolve(&conflict);
        assert!(
            matches!(result, ConflictResolution::AcceptOurs),
            "expected AcceptOurs for newer ours"
        );
    }

    #[test]
    fn timestamp_resolver_accepts_newer_theirs() {
        let now = Utc::now();
        let conflict = ConflictPair {
            key: "arch:design".to_string(),
            ours: make_entry("arch:design", "old value", now - Duration::seconds(120)),
            theirs: make_entry("arch:design", "new value", now),
            base: None,
            detected_at: now,
        };
        let resolver = TimestampResolver::default();
        let result = resolver.resolve(&conflict);
        assert!(
            matches!(result, ConflictResolution::AcceptTheirs),
            "expected AcceptTheirs for newer theirs"
        );
    }

    #[test]
    fn timestamp_resolver_escalates_close_timestamps() {
        let now = Utc::now();
        let conflict = ConflictPair {
            key: "arch:design".to_string(),
            ours: make_entry("arch:design", "v1", now - Duration::seconds(10)),
            theirs: make_entry("arch:design", "v2", now),
            base: None,
            detected_at: now,
        };
        let resolver = TimestampResolver::default();
        let result = resolver.resolve(&conflict);
        assert!(
            matches!(result, ConflictResolution::EscalateToHuman { .. }),
            "expected escalation for close timestamps"
        );
    }

    #[test]
    fn agent_resolver_always_escalates() {
        let now = Utc::now();
        let conflict = ConflictPair {
            key: "key".to_string(),
            ours: make_entry("key", "a", now),
            theirs: make_entry("key", "b", now - Duration::seconds(200)),
            base: None,
            detected_at: now,
        };
        let resolver = AgentResolver;
        assert!(matches!(
            resolver.resolve(&conflict),
            ConflictResolution::EscalateToHuman { .. }
        ));
    }

    #[test]
    fn resolver_from_config_timestamp() {
        let config = ConflictResolverConfig::default();
        let resolver = resolver_from_config(&config);
        assert_eq!(resolver.name(), "timestamp");
    }

    #[test]
    fn resolver_from_config_agent() {
        let config = ConflictResolverConfig {
            strategy: "agent".to_string(),
            ..Default::default()
        };
        let resolver = resolver_from_config(&config);
        assert_eq!(resolver.name(), "agent");
    }

    #[test]
    fn write_and_load_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let pm_dir = dir.path().join("project-memory");
        std::fs::create_dir_all(&pm_dir).unwrap();

        let now = Utc::now();
        let conflict = ConflictPair {
            key: "test:key".to_string(),
            ours: make_entry("test:key", "ours", now),
            theirs: make_entry("test:key", "theirs", now - Duration::seconds(200)),
            base: None,
            detected_at: now,
        };

        write_conflict(&pm_dir, &conflict).unwrap();
        let loaded = load_conflicts(&pm_dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].key, "test:key");
    }

    #[test]
    fn remove_conflict_clears_file() {
        let dir = tempfile::tempdir().unwrap();
        let pm_dir = dir.path().join("project-memory");
        std::fs::create_dir_all(&pm_dir).unwrap();

        let now = Utc::now();
        let conflict = ConflictPair {
            key: "test:key".to_string(),
            ours: make_entry("test:key", "ours", now),
            theirs: make_entry("test:key", "theirs", now - Duration::seconds(200)),
            base: None,
            detected_at: now,
        };

        write_conflict(&pm_dir, &conflict).unwrap();
        assert_eq!(load_conflicts(&pm_dir).len(), 1);
        remove_conflict(&pm_dir, "test:key");
        assert_eq!(load_conflicts(&pm_dir).len(), 0);
    }
}
