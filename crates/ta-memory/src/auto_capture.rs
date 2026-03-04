// auto_capture.rs — Automatic state capture from goal/draft lifecycle events.
//
// Converts TA lifecycle events (goal completion, draft rejection, human
// guidance, repeated corrections) into persistent memory entries. This is
// what makes TA's memory framework-agnostic: every agent framework produces
// the same events, and auto-capture stores them in a unified memory store.
//
// The AutoCapture struct is stateless — it takes a MemoryStore reference
// and event data, and writes entries. Configuration comes from
// `.ta/workflow.toml` (AutoCaptureConfig).

use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

use crate::error::MemoryError;
use crate::store::{MemoryCategory, MemoryStore, StoreParams};

/// Configuration for automatic state capture, parsed from `.ta/workflow.toml`.
///
/// ```toml
/// [memory.auto_capture]
/// on_goal_complete = true
/// on_draft_reject = true
/// on_human_guidance = true
/// on_repeated_correction = true
/// correction_threshold = 3
/// max_context_entries = 10
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCaptureConfig {
    /// Capture "what worked" patterns from approved drafts.
    #[serde(default = "default_true")]
    pub on_goal_complete: bool,

    /// Store rejection reason + what the agent tried.
    #[serde(default = "default_true")]
    pub on_draft_reject: bool,

    /// Store human feedback from interactive sessions.
    #[serde(default = "default_true")]
    pub on_human_guidance: bool,

    /// Auto-promote to persistent memory when the same correction repeats.
    #[serde(default = "default_true")]
    pub on_repeated_correction: bool,

    /// How many times a correction must repeat before auto-promotion.
    #[serde(default = "default_correction_threshold")]
    pub correction_threshold: u32,

    /// Maximum number of memory entries to inject as context on agent launch.
    #[serde(default = "default_max_context")]
    pub max_context_entries: usize,
}

fn default_true() -> bool {
    true
}
fn default_correction_threshold() -> u32 {
    3
}
fn default_max_context() -> usize {
    10
}

impl Default for AutoCaptureConfig {
    fn default() -> Self {
        Self {
            on_goal_complete: true,
            on_draft_reject: true,
            on_human_guidance: true,
            on_repeated_correction: true,
            correction_threshold: 3,
            max_context_entries: 10,
        }
    }
}

/// Event data for goal completion capture.
#[derive(Debug, Clone)]
pub struct GoalCompleteEvent {
    pub goal_id: Uuid,
    pub title: String,
    pub agent_framework: String,
    /// JSON summary from `.ta/change_summary.json` (if available).
    pub change_summary: Option<serde_json::Value>,
    /// Files that were changed in this goal.
    pub changed_files: Vec<String>,
    /// Plan phase this goal was associated with (v0.6.3).
    pub phase_id: Option<String>,
}

/// Event data for draft rejection capture.
#[derive(Debug, Clone)]
pub struct DraftRejectEvent {
    pub goal_id: Uuid,
    pub draft_id: Uuid,
    pub agent_framework: String,
    /// What the agent tried (draft summary).
    pub attempted: String,
    /// Why the human rejected it.
    pub rejection_reason: String,
    /// Plan phase this rejection is associated with (v0.6.3).
    pub phase_id: Option<String>,
}

/// Event data for human guidance capture.
#[derive(Debug, Clone)]
pub struct HumanGuidanceEvent {
    pub goal_id: Option<Uuid>,
    pub agent_framework: String,
    /// The guidance/instruction the human gave.
    pub guidance: String,
    /// Tags to classify the guidance.
    pub tags: Vec<String>,
    /// Plan phase this guidance is associated with (v0.6.3).
    pub phase_id: Option<String>,
}

/// Captures lifecycle events into the memory store.
pub struct AutoCapture {
    config: AutoCaptureConfig,
}

impl AutoCapture {
    pub fn new(config: AutoCaptureConfig) -> Self {
        Self { config }
    }

    /// Capture a goal completion event into memory.
    ///
    /// Stores the goal completion history entry, and additionally extracts
    /// architectural knowledge (key types, module boundaries) from the
    /// change_summary when available (v0.6.3).
    pub fn on_goal_complete(
        &self,
        store: &mut dyn MemoryStore,
        event: &GoalCompleteEvent,
    ) -> Result<(), MemoryError> {
        if !self.config.on_goal_complete {
            return Ok(());
        }

        let key = format!("goal:{}:complete", event.goal_id);
        let value = serde_json::json!({
            "title": event.title,
            "changed_files": event.changed_files,
            "change_summary": event.change_summary,
        });
        let tags = vec![
            "goal".to_string(),
            "completed".to_string(),
            format!("framework:{}", event.agent_framework),
        ];
        let params = StoreParams {
            goal_id: Some(event.goal_id),
            category: Some(MemoryCategory::History),
            confidence: Some(0.8),
            phase_id: event.phase_id.clone(),
            ..Default::default()
        };

        store.store_with_params(&key, value, tags, "ta-system", params)?;

        // v0.6.3: Extract architectural knowledge from change_summary.
        if let Some(ref summary) = event.change_summary {
            self.extract_arch_knowledge(store, event, summary)?;
        }

        debug!(goal_id = %event.goal_id, "auto-captured goal completion");
        Ok(())
    }

    /// Extract architectural knowledge from a change_summary (v0.6.3).
    ///
    /// Parses the `changes` array to identify module/crate names and
    /// stores them as `Architecture` category entries.
    fn extract_arch_knowledge(
        &self,
        store: &mut dyn MemoryStore,
        event: &GoalCompleteEvent,
        summary: &serde_json::Value,
    ) -> Result<(), MemoryError> {
        let changes = match summary.get("changes").and_then(|c| c.as_array()) {
            Some(arr) => arr,
            None => return Ok(()),
        };

        // Collect unique module/crate names from file paths.
        let mut modules: Vec<String> = Vec::new();
        for change in changes {
            if let Some(path) = change.get("path").and_then(|p| p.as_str()) {
                // Extract crate/module name from paths like "crates/ta-memory/src/..."
                if let Some(module) = extract_module_name(path) {
                    if !modules.contains(&module) {
                        modules.push(module);
                    }
                }
            }
        }

        if modules.is_empty() {
            return Ok(());
        }

        // Store the module map as an Architecture entry.
        let summary_text = summary
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or(&event.title);

        let key = format!("arch:module-map:goal-{}", &event.goal_id.to_string()[..8]);
        let value = serde_json::json!({
            "modules": modules,
            "summary": summary_text,
            "file_count": changes.len(),
        });
        let params = StoreParams {
            goal_id: Some(event.goal_id),
            category: Some(MemoryCategory::Architecture),
            confidence: Some(0.8),
            phase_id: event.phase_id.clone(),
            ..Default::default()
        };
        store.store_with_params(
            &key,
            value,
            vec!["architecture".into(), "auto-extracted".into()],
            "ta-system",
            params,
        )?;
        debug!(
            modules = ?modules,
            "extracted architectural knowledge from goal completion"
        );
        Ok(())
    }

    /// Capture a draft rejection event as a NegativePath memory entry (v0.6.3).
    ///
    /// Records what was tried, why it failed, and the human's feedback
    /// using the `NegativePath` category. Key is `neg:{phase}:{slug}` when
    /// a phase is available, preventing agents from repeating mistakes.
    pub fn on_draft_reject(
        &self,
        store: &mut dyn MemoryStore,
        event: &DraftRejectEvent,
    ) -> Result<(), MemoryError> {
        if !self.config.on_draft_reject {
            return Ok(());
        }

        // v0.6.3: Use NegativePath category with phase-aware key.
        let slug = slug_from_text(&event.attempted, 60);
        let key = match &event.phase_id {
            Some(phase) => format!("neg:{}:{}", phase, slug),
            None => format!("neg:{}:{}", event.draft_id, slug),
        };
        let value = serde_json::json!({
            "attempted": event.attempted,
            "rejection_reason": event.rejection_reason,
        });
        let tags = vec![
            "negative-path".to_string(),
            "rejected".to_string(),
            format!("framework:{}", event.agent_framework),
        ];
        let params = StoreParams {
            goal_id: Some(event.goal_id),
            category: Some(MemoryCategory::NegativePath),
            confidence: Some(0.7),
            phase_id: event.phase_id.clone(),
            ..Default::default()
        };

        store.store_with_params(&key, value, tags, "ta-system", params)?;
        debug!(draft_id = %event.draft_id, "auto-captured draft rejection as negative path");
        Ok(())
    }

    /// Capture human guidance into persistent memory.
    pub fn on_human_guidance(
        &self,
        store: &mut dyn MemoryStore,
        event: &HumanGuidanceEvent,
    ) -> Result<(), MemoryError> {
        if !self.config.on_human_guidance {
            return Ok(());
        }

        // Use a content-based key so duplicate guidance is deduplicated.
        let key = format!("guidance:{}", slug_from_text(&event.guidance, 80));
        let value = serde_json::json!({
            "guidance": event.guidance,
        });
        let mut tags = event.tags.clone();
        tags.push("human-guidance".to_string());
        tags.push(format!("framework:{}", event.agent_framework));

        let params = StoreParams {
            goal_id: event.goal_id,
            category: Some(MemoryCategory::Preference),
            confidence: Some(0.9),
            phase_id: event.phase_id.clone(),
            ..Default::default()
        };

        store.store_with_params(&key, value, tags, "ta-system", params)?;
        debug!("auto-captured human guidance");
        Ok(())
    }

    /// Check if a correction has been repeated enough times to auto-promote.
    ///
    /// Returns true if the correction was promoted to persistent memory.
    pub fn check_repeated_correction(
        &self,
        store: &mut dyn MemoryStore,
        correction_key: &str,
        correction_value: &str,
    ) -> Result<bool, MemoryError> {
        if !self.config.on_repeated_correction {
            return Ok(false);
        }

        let counter_key = format!("correction-count:{}", correction_key);

        // Read current count.
        let current_count = match store.recall(&counter_key)? {
            Some(entry) => entry.value.as_u64().unwrap_or(0) as u32,
            None => 0,
        };

        let new_count = current_count + 1;

        // Update counter.
        store.store(
            &counter_key,
            serde_json::json!(new_count),
            vec!["correction-counter".to_string()],
            "ta-system",
        )?;

        // Promote if threshold reached.
        if new_count >= self.config.correction_threshold {
            let promo_key = format!("preference:{}", correction_key);
            let params = StoreParams {
                goal_id: None,
                category: Some(MemoryCategory::Preference),
                confidence: Some(0.9),
                ..Default::default()
            };
            store.store_with_params(
                &promo_key,
                serde_json::json!({"pattern": correction_value, "auto_promoted": true}),
                vec!["preference".to_string(), "auto-promoted".to_string()],
                "ta-system",
                params,
            )?;
            debug!(
                correction_key,
                count = new_count,
                "auto-promoted repeated correction to preference"
            );

            // Clean up counter.
            store.forget(&counter_key)?;
            return Ok(true);
        }

        Ok(false)
    }

    /// Get the config reference.
    pub fn config(&self) -> &AutoCaptureConfig {
        &self.config
    }
}

/// Build a context injection section from memory entries for CLAUDE.md (v0.6.3).
///
/// Phase-aware: filters entries matching the current phase or global entries.
/// Category-prioritized: Architecture > NegativePath > Convention > State > History.
/// Structured: groups entries by category with markdown headings.
pub fn build_memory_context_section(
    store: &dyn MemoryStore,
    goal_title: &str,
    max_entries: usize,
) -> Result<String, MemoryError> {
    build_memory_context_section_with_phase(store, goal_title, max_entries, None)
}

/// Phase-aware version of `build_memory_context_section` (v0.6.3).
pub fn build_memory_context_section_with_phase(
    store: &dyn MemoryStore,
    goal_title: &str,
    max_entries: usize,
    phase_id: Option<&str>,
) -> Result<String, MemoryError> {
    if max_entries == 0 {
        return Ok(String::new());
    }

    // Try semantic search first (returns results only with ruvector backend).
    let mut entries = store.semantic_search(goal_title, max_entries * 2)?;

    // Fall back to tag-based lookup if semantic search returned nothing.
    if entries.is_empty() {
        entries = store.lookup(crate::store::MemoryQuery {
            phase_id: phase_id.map(String::from),
            limit: Some(max_entries * 2),
            ..Default::default()
        })?;
    }

    // Phase filter: keep entries matching current phase or global (None).
    if let Some(phase) = phase_id {
        entries.retain(|e| match &e.phase_id {
            Some(ep) => ep == phase,
            None => true, // Global entries always included.
        });
    }

    if entries.is_empty() {
        return Ok(String::new());
    }

    // Category priority ordering (v0.6.3).
    fn category_priority(cat: &Option<MemoryCategory>) -> u8 {
        match cat {
            Some(MemoryCategory::Architecture) => 0,
            Some(MemoryCategory::NegativePath) => 1,
            Some(MemoryCategory::Convention) => 2,
            Some(MemoryCategory::State) => 3,
            Some(MemoryCategory::Preference) => 4,
            Some(MemoryCategory::History) => 5,
            Some(MemoryCategory::Relationship) => 6,
            Some(MemoryCategory::Other) | None => 7,
        }
    }
    entries.sort_by_key(|e| category_priority(&e.category));
    entries.truncate(max_entries);

    let mut section = String::from("\n## Prior Context (from TA memory)\n\n");
    section.push_str(
        "The following knowledge was captured from previous sessions across all agent frameworks.\n\n",
    );

    // Group entries by category for structured output.
    let mut current_category: Option<String> = None;
    for entry in &entries {
        let cat_label = entry
            .category
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "other".to_string());

        if current_category.as_deref() != Some(&cat_label) {
            current_category = Some(cat_label.clone());
            let heading = match entry.category {
                Some(MemoryCategory::Architecture) => "Architecture",
                Some(MemoryCategory::NegativePath) => "Negative Paths (avoid these)",
                Some(MemoryCategory::Convention) => "Conventions",
                Some(MemoryCategory::State) => "Project State",
                Some(MemoryCategory::Preference) => "Preferences",
                Some(MemoryCategory::History) => "History",
                Some(MemoryCategory::Relationship) => "Relationships",
                _ => "Other",
            };
            section.push_str(&format!("### {}\n\n", heading));
        }

        let source_label = if entry.source != "ta-system" {
            format!(" (source: {})", entry.source)
        } else {
            String::new()
        };

        let value_str = if let Some(s) = entry.value.as_str() {
            s.to_string()
        } else {
            serde_json::to_string(&entry.value).unwrap_or_default()
        };

        section.push_str(&format!(
            "- **[{}] {}**{}: {}\n",
            cat_label, entry.key, source_label, value_str,
        ));
    }

    section.push('\n');
    Ok(section)
}

/// Extract a module/crate name from a file path.
///
/// Recognizes patterns like:
/// - `crates/<name>/src/...` → `<name>`
/// - `apps/<name>/src/...` → `<name>`
/// - `packages/<name>/src/...` → `<name>`
/// - `src/<name>/...` → `<name>` (for flat structures)
fn extract_module_name(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    for (i, &part) in parts.iter().enumerate() {
        if matches!(part, "crates" | "apps" | "packages" | "libs") {
            if let Some(&name) = parts.get(i + 1) {
                return Some(name.to_string());
            }
        }
    }
    // Fallback: "src/<name>/..." for flat structures.
    if parts.first() == Some(&"src") && parts.len() >= 2 {
        return Some(parts[1].to_string());
    }
    None
}

/// Create a URL-safe slug from text (for use as memory keys).
fn slug_from_text(text: &str, max_len: usize) -> String {
    let slug: String = text
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse consecutive dashes.
    let mut prev_dash = false;
    let collapsed: String = slug
        .chars()
        .filter(|&c| {
            if c == '-' {
                if prev_dash {
                    return false;
                }
                prev_dash = true;
            } else {
                prev_dash = false;
            }
            true
        })
        .collect();
    let trimmed = collapsed.trim_matches('-');
    if trimmed.len() > max_len {
        trimmed[..max_len].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parse AutoCaptureConfig from a `.ta/workflow.toml` file.
///
/// If the file doesn't exist or doesn't contain `[memory.auto_capture]`,
/// returns the default config (all features enabled).
pub fn load_config(workflow_toml_path: &std::path::Path) -> AutoCaptureConfig {
    if !workflow_toml_path.exists() {
        return AutoCaptureConfig::default();
    }

    match std::fs::read_to_string(workflow_toml_path) {
        Ok(content) => parse_config_from_toml(&content),
        Err(_) => AutoCaptureConfig::default(),
    }
}

/// Parse the auto-capture section from TOML content.
fn parse_config_from_toml(content: &str) -> AutoCaptureConfig {
    // Minimal TOML parsing — extract [memory.auto_capture] section values.
    // We avoid pulling in a full TOML crate by doing simple line parsing.
    let mut config = AutoCaptureConfig::default();
    let mut in_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "[memory.auto_capture]" {
            in_section = true;
            continue;
        }

        // New section starts — stop parsing.
        if trimmed.starts_with('[') {
            if in_section {
                break;
            }
            continue;
        }

        if !in_section {
            continue;
        }

        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            match key {
                "on_goal_complete" => config.on_goal_complete = value == "true",
                "on_draft_reject" => config.on_draft_reject = value == "true",
                "on_human_guidance" => config.on_human_guidance = value == "true",
                "on_repeated_correction" => config.on_repeated_correction = value == "true",
                "correction_threshold" => {
                    if let Ok(n) = value.parse() {
                        config.correction_threshold = n;
                    }
                }
                "max_context_entries" => {
                    if let Ok(n) = value.parse() {
                        config.max_context_entries = n;
                    }
                }
                _ => {}
            }
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FsMemoryStore;
    use tempfile::TempDir;

    fn test_store(dir: &TempDir) -> FsMemoryStore {
        FsMemoryStore::new(dir.path().join("memory"))
    }

    #[test]
    fn auto_capture_goal_complete() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);
        let capture = AutoCapture::new(AutoCaptureConfig::default());

        let event = GoalCompleteEvent {
            goal_id: Uuid::new_v4(),
            title: "Fix auth bug".to_string(),
            agent_framework: "claude-code".to_string(),
            change_summary: Some(serde_json::json!({"summary": "Fixed JWT validation"})),
            changed_files: vec!["src/auth.rs".to_string()],
            phase_id: None,
        };

        capture.on_goal_complete(&mut store, &event).unwrap();

        let key = format!("goal:{}:complete", event.goal_id);
        let entry = store.recall(&key).unwrap().unwrap();
        assert_eq!(entry.goal_id, Some(event.goal_id));
        assert_eq!(entry.category, Some(MemoryCategory::History));
        assert!(entry.tags.contains(&"completed".to_string()));
        assert!(entry.tags.contains(&"framework:claude-code".to_string()));
    }

    #[test]
    fn auto_capture_goal_with_arch_extraction() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);
        let capture = AutoCapture::new(AutoCaptureConfig::default());

        let event = GoalCompleteEvent {
            goal_id: Uuid::new_v4(),
            title: "Implement memory system".to_string(),
            agent_framework: "claude-code".to_string(),
            change_summary: Some(serde_json::json!({
                "summary": "Added memory crate",
                "changes": [
                    {"path": "crates/ta-memory/src/store.rs", "action": "created"},
                    {"path": "crates/ta-memory/src/lib.rs", "action": "created"},
                    {"path": "apps/ta-cli/src/commands/context.rs", "action": "modified"},
                ]
            })),
            changed_files: vec![
                "crates/ta-memory/src/store.rs".into(),
                "apps/ta-cli/src/commands/context.rs".into(),
            ],
            phase_id: Some("v0.5.4".into()),
        };

        capture.on_goal_complete(&mut store, &event).unwrap();

        // Check that arch knowledge was extracted.
        let all = store.list(None).unwrap();
        let arch_entry = all.iter().find(|e| e.key.starts_with("arch:module-map:"));
        assert!(arch_entry.is_some(), "should have extracted arch knowledge");
        let arch = arch_entry.unwrap();
        assert_eq!(arch.category, Some(MemoryCategory::Architecture));
        let modules = arch.value["modules"].as_array().unwrap();
        assert!(modules.iter().any(|m| m == "ta-memory"));
        assert!(modules.iter().any(|m| m == "ta-cli"));
        assert_eq!(arch.phase_id.as_deref(), Some("v0.5.4"));
    }

    #[test]
    fn auto_capture_draft_rejection_as_negative_path() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);
        let capture = AutoCapture::new(AutoCaptureConfig::default());

        let event = DraftRejectEvent {
            goal_id: Uuid::new_v4(),
            draft_id: Uuid::new_v4(),
            agent_framework: "codex".to_string(),
            attempted: "Added Redis caching layer".to_string(),
            rejection_reason: "Too complex for MVP, use in-memory cache".to_string(),
            phase_id: Some("v0.5.4".into()),
        };

        capture.on_draft_reject(&mut store, &event).unwrap();

        // v0.6.3: uses NegativePath category and phase-aware key.
        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 1);
        let entry = &all[0];
        assert!(entry.key.starts_with("neg:v0.5.4:"));
        assert_eq!(entry.category, Some(MemoryCategory::NegativePath));
        assert!(entry.tags.contains(&"negative-path".to_string()));
        assert!(entry.tags.contains(&"rejected".to_string()));
        let reason = entry.value["rejection_reason"].as_str().unwrap();
        assert!(reason.contains("Too complex"));
        assert_eq!(entry.phase_id.as_deref(), Some("v0.5.4"));
    }

    #[test]
    fn context_injection_builds_markdown_section() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        // Store some entries.
        store
            .store_with_params(
                "convention:test-style",
                serde_json::json!("Use tempfile::tempdir() for all tests"),
                vec!["convention".into()],
                "claude-code",
                StoreParams {
                    goal_id: None,
                    category: Some(MemoryCategory::Convention),
                    ..Default::default()
                },
            )
            .unwrap();

        let section = build_memory_context_section(&store, "Fix tests", 10).unwrap();
        assert!(section.contains("Prior Context"));
        assert!(section.contains("tempfile::tempdir()"));
        assert!(section.contains("[convention]"));
    }

    #[test]
    fn cross_framework_recall() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        // Store from "claude-code".
        store
            .store(
                "shared-convention",
                serde_json::json!("Always run clippy before commit"),
                vec!["convention".into(), "framework:claude-code".into()],
                "claude-code",
            )
            .unwrap();

        // Recall as "codex" — the entry is framework-agnostic, accessible to all.
        let entry = store.recall("shared-convention").unwrap().unwrap();
        assert_eq!(entry.source, "claude-code");
        assert_eq!(
            entry.value,
            serde_json::json!("Always run clippy before commit")
        );
    }

    #[test]
    fn repeated_correction_auto_promotes() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);
        let config = AutoCaptureConfig {
            correction_threshold: 3,
            ..Default::default()
        };
        let capture = AutoCapture::new(config);

        // First two corrections — not yet promoted.
        let promoted = capture
            .check_repeated_correction(&mut store, "use-tempdir", "Use tempfile::tempdir()")
            .unwrap();
        assert!(!promoted);

        let promoted = capture
            .check_repeated_correction(&mut store, "use-tempdir", "Use tempfile::tempdir()")
            .unwrap();
        assert!(!promoted);

        // Third correction — promoted!
        let promoted = capture
            .check_repeated_correction(&mut store, "use-tempdir", "Use tempfile::tempdir()")
            .unwrap();
        assert!(promoted);

        // The preference entry should exist.
        let pref = store.recall("preference:use-tempdir").unwrap().unwrap();
        assert_eq!(pref.category, Some(MemoryCategory::Preference));
        assert!(pref.tags.contains(&"auto-promoted".to_string()));

        // The counter should have been cleaned up.
        assert!(store
            .recall("correction-count:use-tempdir")
            .unwrap()
            .is_none());
    }

    #[test]
    fn config_parsing_from_toml() {
        let toml = r#"
[memory.auto_capture]
on_goal_complete = true
on_draft_reject = false
on_human_guidance = true
on_repeated_correction = true
correction_threshold = 5
max_context_entries = 20
"#;
        let config = parse_config_from_toml(toml);
        assert!(config.on_goal_complete);
        assert!(!config.on_draft_reject);
        assert!(config.on_human_guidance);
        assert_eq!(config.correction_threshold, 5);
        assert_eq!(config.max_context_entries, 20);
    }

    #[test]
    fn config_defaults_when_no_section() {
        let config = parse_config_from_toml("[other]\nfoo = bar");
        assert!(config.on_goal_complete);
        assert!(config.on_draft_reject);
        assert_eq!(config.correction_threshold, 3);
    }

    #[test]
    fn disabled_capture_is_noop() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);
        let config = AutoCaptureConfig {
            on_goal_complete: false,
            on_draft_reject: false,
            ..Default::default()
        };
        let capture = AutoCapture::new(config);

        let event = GoalCompleteEvent {
            goal_id: Uuid::new_v4(),
            title: "test".to_string(),
            agent_framework: "test".to_string(),
            change_summary: None,
            changed_files: vec![],
            phase_id: None,
        };
        capture.on_goal_complete(&mut store, &event).unwrap();

        // Nothing stored.
        assert!(store.list(None).unwrap().is_empty());
    }

    #[test]
    fn phase_filtered_injection() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        // Global entry (no phase).
        store
            .store_with_params(
                "convention:test-style",
                serde_json::json!("Use tempfile for tests"),
                vec!["convention".into()],
                "ta-system",
                StoreParams {
                    category: Some(MemoryCategory::Convention),
                    ..Default::default()
                },
            )
            .unwrap();

        // Phase-specific entry matching.
        store
            .store_with_params(
                "arch:crate-map:v063",
                serde_json::json!("Module map for v0.6.3"),
                vec!["architecture".into()],
                "ta-system",
                StoreParams {
                    category: Some(MemoryCategory::Architecture),
                    phase_id: Some("v0.6.3".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        // Phase-specific entry NOT matching.
        store
            .store_with_params(
                "arch:crate-map:v050",
                serde_json::json!("Old module map"),
                vec!["architecture".into()],
                "ta-system",
                StoreParams {
                    category: Some(MemoryCategory::Architecture),
                    phase_id: Some("v0.5.0".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        let section =
            build_memory_context_section_with_phase(&store, "test", 10, Some("v0.6.3")).unwrap();
        // Should include global entry and v0.6.3 entry, but NOT v0.5.0 entry.
        assert!(section.contains("test-style"));
        assert!(section.contains("v063"));
        assert!(!section.contains("v050"));
    }

    #[test]
    fn backward_compat_entries_without_phase() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        // Old-style entry without phase_id.
        store
            .store_with_params(
                "old-convention",
                serde_json::json!("Legacy convention"),
                vec![],
                "ta-system",
                StoreParams {
                    category: Some(MemoryCategory::Convention),
                    ..Default::default()
                },
            )
            .unwrap();

        // Should be included regardless of phase filter (it's global).
        let section =
            build_memory_context_section_with_phase(&store, "test", 10, Some("v0.6.3")).unwrap();
        assert!(section.contains("Legacy convention"));
    }

    #[test]
    fn category_priority_ordering() {
        let dir = TempDir::new().unwrap();
        let mut store = test_store(&dir);

        // Store in reverse priority order.
        store
            .store_with_params(
                "history:old",
                serde_json::json!("Historical note"),
                vec![],
                "ta-system",
                StoreParams {
                    category: Some(MemoryCategory::History),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .store_with_params(
                "arch:layout",
                serde_json::json!("Architecture note"),
                vec![],
                "ta-system",
                StoreParams {
                    category: Some(MemoryCategory::Architecture),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .store_with_params(
                "neg:v1:mistake",
                serde_json::json!("Negative path"),
                vec![],
                "ta-system",
                StoreParams {
                    category: Some(MemoryCategory::NegativePath),
                    ..Default::default()
                },
            )
            .unwrap();

        let section = build_memory_context_section(&store, "test", 10).unwrap();
        // Architecture should appear before NegativePath, which should appear before History.
        let arch_pos = section.find("Architecture").unwrap();
        let neg_pos = section.find("Negative Paths").unwrap();
        let hist_pos = section.find("History").unwrap();
        assert!(arch_pos < neg_pos);
        assert!(neg_pos < hist_pos);
    }

    #[test]
    fn slug_generation() {
        assert_eq!(slug_from_text("Hello World!", 80), "hello-world");
        assert_eq!(
            slug_from_text("use tempfile::tempdir()", 80),
            "use-tempfile-tempdir"
        );
        assert_eq!(slug_from_text("a".repeat(100).as_str(), 50), "a".repeat(50));
    }
}
