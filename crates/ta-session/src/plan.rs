// plan.rs — PlanDocument and PlanItem types for project-session planning (v0.14.11).
//
// A PlanDocument is produced by `ta new plan --from brief.md` and consumed by
// `ta session start <plan-id>` to instantiate a WorkflowSession.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ta_changeset::ArtifactType;
use uuid::Uuid;

/// A structured plan document produced by parsing a project brief.
///
/// Created by `ta new plan --from brief.md` and persisted to
/// `.ta/memory/plan/<uuid>.json`. The `plan_id` is used as a stable reference
/// in `ta session start <plan-id>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDocument {
    pub plan_id: Uuid,
    pub title: String,
    /// First 200 chars of the source brief (for display purposes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_source: Option<String>,
    pub items: Vec<PlanItem>,
    pub created_at: DateTime<Utc>,
}

/// A single deliverable unit within a PlanDocument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub item_id: Uuid,
    pub title: String,
    /// Conditions that must be met for this item to be considered done.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    /// Rough effort estimate (e.g., "small", "medium", "1-2h").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_effort: Option<String>,
    /// Artifact types this item is expected to produce.
    #[serde(default)]
    pub artifact_outputs: Vec<ArtifactType>,
    /// IDs of other items in this plan that must complete before this one starts.
    #[serde(default)]
    pub depends_on: Vec<Uuid>,
}

impl PlanDocument {
    /// Create a new empty PlanDocument.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            plan_id: Uuid::new_v4(),
            title: title.into(),
            brief_source: None,
            items: Vec::new(),
            created_at: Utc::now(),
        }
    }

    /// Parse a freeform brief into a PlanDocument.
    ///
    /// This is a stub planner — it extracts structure from markdown headings
    /// and list items. A production planner would use an LLM agent for richer
    /// acceptance criteria generation.
    pub fn from_brief(brief: &str) -> Self {
        let mut doc = Self::new(extract_title(brief));
        doc.brief_source = Some(brief.chars().take(200).collect());
        doc.items = parse_items_from_brief(brief);
        doc
    }

    /// Append a plan item.
    pub fn add_item(&mut self, item: PlanItem) {
        self.items.push(item);
    }

    /// The memory key used to store/retrieve this plan.
    ///
    /// Follows the convention `plan/<uuid>` for the memory store.
    pub fn memory_key(&self) -> String {
        format!("plan/{}", self.plan_id)
    }
}

impl PlanItem {
    /// Create a new plan item with the given title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            item_id: Uuid::new_v4(),
            title: title.into(),
            acceptance_criteria: Vec::new(),
            estimated_effort: None,
            artifact_outputs: Vec::new(),
            depends_on: Vec::new(),
        }
    }
}

/// Extract a title from brief text — use the first H1 heading or first line.
fn extract_title(brief: &str) -> String {
    for line in brief.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            let title = stripped.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            return trimmed.chars().take(80).collect();
        }
    }
    "Untitled Plan".to_string()
}

/// Parse plan items from a freeform brief document.
///
/// Extraction rules (applied in order):
/// - H2 headings (`## `) become separate plan items.
/// - Bullet/numbered list items at the top level become items when no H2 is active.
/// - Bullets/criteria under an H2 become acceptance criteria for that item.
fn parse_items_from_brief(brief: &str) -> Vec<PlanItem> {
    let mut items: Vec<PlanItem> = Vec::new();
    let mut current_item: Option<PlanItem> = None;
    let mut in_h2_context = false;

    for line in brief.lines() {
        let trimmed = line.trim();

        // H2 → new plan item
        if let Some(title) = trimmed.strip_prefix("## ") {
            if let Some(item) = current_item.take() {
                items.push(item);
            }
            let t = title.trim();
            if !t.is_empty() {
                current_item = Some(PlanItem::new(t));
                in_h2_context = true;
            }
            continue;
        }

        // H3 under H2 context → acceptance criterion label (skip, or treat as criterion)
        if trimmed.starts_with("### ") {
            continue;
        }

        // List item under H2 → acceptance criterion
        if in_h2_context {
            if let Some(ref mut item) = current_item {
                if let Some(criterion) = extract_list_item(trimmed) {
                    if !criterion.is_empty() {
                        item.acceptance_criteria.push(criterion.to_string());
                    }
                }
            }
            continue;
        }

        // Numbered/bullet list at top level (no H2 context) → plan item
        if !in_h2_context && current_item.is_none() {
            if let Some(text) = extract_list_item(trimmed) {
                if !text.is_empty() {
                    items.push(PlanItem::new(text));
                }
            }
        }
    }

    // Flush last H2 item
    if let Some(item) = current_item {
        items.push(item);
    }

    // Fallback: if nothing was found, create a single item from the title
    if items.is_empty() {
        let title = extract_title(brief);
        if title != "Untitled Plan" {
            items.push(PlanItem::new(title));
        }
    }

    items
}

/// Extract list item text from a line. Returns `None` if the line is not a list item.
fn extract_list_item(line: &str) -> Option<&str> {
    // Checkbox list: "- [ ] text" or "- [x] text"
    for prefix in &["- [ ] ", "- [x] ", "- [X] "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest.trim());
        }
    }
    // Unordered: "- ", "* ", "+ "
    for prefix in &["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest.trim());
        }
    }
    // Numbered: "1. ", "2. " … "99. "
    if let Some(dot_pos) = line.find(". ") {
        let before = &line[..dot_pos];
        if !before.is_empty() && before.chars().all(|c| c.is_ascii_digit()) {
            return Some(line[dot_pos + 2..].trim());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_document_new() {
        let doc = PlanDocument::new("My Project");
        assert_eq!(doc.title, "My Project");
        assert!(doc.items.is_empty());
        assert!(doc.brief_source.is_none());
    }

    #[test]
    fn plan_item_new() {
        let item = PlanItem::new("Add authentication");
        assert_eq!(item.title, "Add authentication");
        assert!(item.acceptance_criteria.is_empty());
        assert!(item.depends_on.is_empty());
    }

    #[test]
    fn memory_key_format() {
        let doc = PlanDocument::new("test");
        let key = doc.memory_key();
        assert!(key.starts_with("plan/"));
        assert_eq!(key, format!("plan/{}", doc.plan_id));
    }

    #[test]
    fn from_brief_with_h2_headings() {
        let brief = "# My Project\n\nDescription here.\n\n## Add authentication\n- Uses JWT tokens\n- Stores tokens securely\n\n## Add database layer\n- PostgreSQL 15+\n- Migration support\n";
        let doc = PlanDocument::from_brief(brief);
        assert_eq!(doc.title, "My Project");
        assert_eq!(doc.items.len(), 2);
        assert_eq!(doc.items[0].title, "Add authentication");
        assert_eq!(doc.items[0].acceptance_criteria.len(), 2);
        assert!(doc.items[0].acceptance_criteria[0].contains("JWT"));
        assert_eq!(doc.items[1].title, "Add database layer");
    }

    #[test]
    fn from_brief_with_numbered_list() {
        let brief = "Build a REST API\n\n1. Implement CRUD endpoints\n2. Add rate limiting\n3. Write integration tests\n";
        let doc = PlanDocument::from_brief(brief);
        assert_eq!(doc.items.len(), 3);
        assert_eq!(doc.items[0].title, "Implement CRUD endpoints");
        assert_eq!(doc.items[1].title, "Add rate limiting");
        assert_eq!(doc.items[2].title, "Write integration tests");
    }

    #[test]
    fn from_brief_with_bullet_list() {
        let brief = "CLI Tool\n\n- Parse arguments\n- Execute commands\n- Output results\n";
        let doc = PlanDocument::from_brief(brief);
        assert_eq!(doc.items.len(), 3);
        assert_eq!(doc.items[0].title, "Parse arguments");
    }

    #[test]
    fn from_brief_with_checkbox_list() {
        let brief = "Tasks\n\n- [ ] Write tests\n- [ ] Deploy to staging\n";
        let doc = PlanDocument::from_brief(brief);
        assert_eq!(doc.items.len(), 2);
        assert_eq!(doc.items[0].title, "Write tests");
    }

    #[test]
    fn from_brief_brief_source_truncated() {
        let long_brief = "x".repeat(300);
        let doc = PlanDocument::from_brief(&long_brief);
        assert!(doc.brief_source.unwrap().len() <= 200);
    }

    #[test]
    fn extract_title_h1() {
        assert_eq!(extract_title("# My Title\nBody"), "My Title");
    }

    #[test]
    fn extract_title_first_line() {
        assert_eq!(extract_title("My Title\nBody"), "My Title");
    }

    #[test]
    fn extract_title_empty_returns_default() {
        assert_eq!(extract_title(""), "Untitled Plan");
        assert_eq!(extract_title("   \n\n"), "Untitled Plan");
    }

    #[test]
    fn from_brief_fallback_single_item() {
        let brief = "Build something great";
        let doc = PlanDocument::from_brief(brief);
        // No lists/headings → single item from title
        assert_eq!(doc.items.len(), 1);
        assert_eq!(doc.items[0].title, "Build something great");
    }

    #[test]
    fn from_brief_empty_has_no_items() {
        let doc = PlanDocument::from_brief("");
        assert!(doc.items.is_empty());
    }

    #[test]
    fn serialization_round_trip() {
        let mut doc = PlanDocument::new("Round-trip test");
        let mut item = PlanItem::new("Step 1");
        item.acceptance_criteria.push("Tests pass".to_string());
        doc.add_item(item);

        let json = serde_json::to_string_pretty(&doc).unwrap();
        let restored: PlanDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.plan_id, doc.plan_id);
        assert_eq!(restored.title, "Round-trip test");
        assert_eq!(restored.items.len(), 1);
        assert_eq!(restored.items[0].acceptance_criteria[0], "Tests pass");
    }

    #[test]
    fn add_item_appends() {
        let mut doc = PlanDocument::new("Test");
        doc.add_item(PlanItem::new("A"));
        doc.add_item(PlanItem::new("B"));
        assert_eq!(doc.items.len(), 2);
        assert_eq!(doc.items[1].title, "B");
    }
}
