// solutions.rs -- Solution entry store for git-committed problem/solution knowledge.
//
// Reads and writes `.ta/solutions/solutions.toml`, a curated datastore
// of reusable problem->solution pairs extracted from TA memory.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::MemoryError;

/// A single solution entry in the curated knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SolutionEntry {
    /// Unique solution ID (e.g., "sol_001").
    pub id: String,
    /// Description of the problem encountered.
    pub problem: String,
    /// How the problem was resolved.
    pub solution: String,
    /// Context in which the solution applies.
    #[serde(default)]
    pub context: SolutionContext,
    /// Tags for categorization and search.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The memory category this was exported from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_category: Option<String>,
    /// When the solution entry was created.
    pub created_at: DateTime<Utc>,
}

/// Context metadata for a solution entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SolutionContext {
    /// Programming language (e.g., "rust", "typescript").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Framework or tool (e.g., "clippy", "tokio", "react").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
}

/// TOML-serializable wrapper for the solutions file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SolutionsFile {
    #[serde(default)]
    pub solutions: Vec<SolutionEntry>,
}

/// CRUD operations on the solutions datastore.
pub struct SolutionStore {
    path: PathBuf,
}

impl SolutionStore {
    /// Create a new solution store at the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Load all solution entries from the TOML file.
    pub fn load(&self) -> Result<Vec<SolutionEntry>, MemoryError> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let content = fs::read_to_string(&self.path)?;
        let file: SolutionsFile = toml_parse(&content)?;
        Ok(file.solutions)
    }

    /// Save solution entries to the TOML file.
    pub fn save(&self, solutions: &[SolutionEntry]) -> Result<(), MemoryError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = SolutionsFile {
            solutions: solutions.to_vec(),
        };
        let content = toml_serialize(&file)?;
        fs::write(&self.path, content)?;
        Ok(())
    }

    /// Add a solution entry, deduplicating by problem similarity.
    /// Returns true if the entry was added (not a duplicate).
    pub fn add(&self, entry: SolutionEntry) -> Result<bool, MemoryError> {
        let mut solutions = self.load()?;

        // Check for duplicates by problem text similarity.
        if solutions
            .iter()
            .any(|s| is_similar_problem(&s.problem, &entry.problem))
        {
            return Ok(false);
        }

        solutions.push(entry);
        self.save(&solutions)?;
        Ok(true)
    }

    /// Remove a solution by ID.
    pub fn remove(&self, id: &str) -> Result<bool, MemoryError> {
        let mut solutions = self.load()?;
        let before = solutions.len();
        solutions.retain(|s| s.id != id);
        if solutions.len() < before {
            self.save(&solutions)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Find solutions matching any of the given tags.
    pub fn find_by_tag(&self, tags: &[String]) -> Result<Vec<SolutionEntry>, MemoryError> {
        let solutions = self.load()?;
        Ok(solutions
            .into_iter()
            .filter(|s| s.tags.iter().any(|t| tags.contains(t)))
            .collect())
    }

    /// Find solutions matching the given language/framework context.
    pub fn find_by_context(
        &self,
        language: Option<&str>,
        framework: Option<&str>,
    ) -> Result<Vec<SolutionEntry>, MemoryError> {
        let solutions = self.load()?;
        Ok(solutions
            .into_iter()
            .filter(|s| {
                let lang_match = match language {
                    Some(l) => s.context.language.as_deref() == Some(l),
                    None => true,
                };
                let fw_match = match framework {
                    Some(f) => s.context.framework.as_deref() == Some(f),
                    None => true,
                };
                lang_match && fw_match
            })
            .collect())
    }

    /// Generate the next solution ID based on existing entries.
    pub fn next_id(&self) -> Result<String, MemoryError> {
        let solutions = self.load()?;
        let max_num = solutions
            .iter()
            .filter_map(|s| {
                s.id.strip_prefix("sol_")
                    .and_then(|n| n.parse::<u32>().ok())
            })
            .max()
            .unwrap_or(0);
        Ok(format!("sol_{:03}", max_num + 1))
    }

    /// Merge entries from another solutions file, deduplicating by problem text.
    /// Returns (new_count, duplicate_count).
    pub fn merge(&self, incoming: &[SolutionEntry]) -> Result<(usize, usize), MemoryError> {
        let mut solutions = self.load()?;
        let mut new_count = 0;
        let mut dup_count = 0;

        for entry in incoming {
            if solutions
                .iter()
                .any(|s| is_similar_problem(&s.problem, &entry.problem))
            {
                dup_count += 1;
            } else {
                // Reassign ID to avoid conflicts.
                let max_num = solutions
                    .iter()
                    .filter_map(|s| {
                        s.id.strip_prefix("sol_")
                            .and_then(|n| n.parse::<u32>().ok())
                    })
                    .max()
                    .unwrap_or(0);
                let mut new_entry = entry.clone();
                new_entry.id = format!("sol_{:03}", max_num + 1);
                solutions.push(new_entry);
                new_count += 1;
            }
        }

        self.save(&solutions)?;
        Ok((new_count, dup_count))
    }

    /// Get the path to the solutions file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Simple string similarity check for deduplication.
/// Uses normalized Levenshtein-like comparison: returns true if the
/// strings are "similar enough" (>80% overlap by word set).
pub fn is_similar_problem(a: &str, b: &str) -> bool {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    // Exact match.
    if a_lower == b_lower {
        return true;
    }

    // Word-set overlap (Jaccard similarity > 0.8).
    let a_words: std::collections::HashSet<&str> = a_lower.split_whitespace().collect();
    let b_words: std::collections::HashSet<&str> = b_lower.split_whitespace().collect();

    if a_words.is_empty() || b_words.is_empty() {
        return false;
    }

    let intersection = a_words.intersection(&b_words).count();
    let union = a_words.union(&b_words).count();

    if union == 0 {
        return false;
    }

    let similarity = intersection as f64 / union as f64;
    similarity > 0.8
}

// --- TOML serialization helpers ---
// We use a simple custom serializer to avoid adding the `toml` crate dependency.
// The format is a subset of TOML suitable for `[[solutions]]` arrays.

fn toml_serialize(file: &SolutionsFile) -> Result<String, MemoryError> {
    let mut out = String::new();
    out.push_str("# TA Solutions — curated problem/solution knowledge\n");
    out.push_str("# Generated by `ta context export`. Edit freely.\n\n");

    for entry in &file.solutions {
        out.push_str("[[solutions]]\n");
        out.push_str(&format!("id = \"{}\"\n", escape_toml(&entry.id)));
        out.push_str(&format!("problem = \"{}\"\n", escape_toml(&entry.problem)));
        out.push_str(&format!(
            "solution = \"{}\"\n",
            escape_toml(&entry.solution)
        ));

        // Context as inline table.
        let mut ctx_parts = Vec::new();
        if let Some(ref lang) = entry.context.language {
            ctx_parts.push(format!("language = \"{}\"", escape_toml(lang)));
        }
        if let Some(ref fw) = entry.context.framework {
            ctx_parts.push(format!("framework = \"{}\"", escape_toml(fw)));
        }
        if !ctx_parts.is_empty() {
            out.push_str(&format!("context = {{ {} }}\n", ctx_parts.join(", ")));
        }

        if !entry.tags.is_empty() {
            let tags: Vec<String> = entry
                .tags
                .iter()
                .map(|t| format!("\"{}\"", escape_toml(t)))
                .collect();
            out.push_str(&format!("tags = [{}]\n", tags.join(", ")));
        }

        if let Some(ref cat) = entry.source_category {
            out.push_str(&format!("source_category = \"{}\"\n", escape_toml(cat)));
        }

        out.push_str(&format!(
            "created_at = \"{}\"\n",
            entry.created_at.to_rfc3339()
        ));

        out.push('\n');
    }

    Ok(out)
}

fn escape_toml(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn toml_parse(content: &str) -> Result<SolutionsFile, MemoryError> {
    let mut solutions = Vec::new();
    let mut current: Option<HashMap<String, String>> = None;
    let mut current_tags: Vec<String> = Vec::new();
    let mut current_context = SolutionContext::default();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "[[solutions]]" {
            if let Some(map) = current.take() {
                solutions.push(map_to_entry(
                    map,
                    current_tags.clone(),
                    current_context.clone(),
                ));
                current_tags.clear();
                current_context = SolutionContext::default();
            }
            current = Some(HashMap::new());
            continue;
        }

        if let Some(ref mut map) = current {
            if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                if key == "tags" {
                    // Parse array: ["tag1", "tag2"]
                    current_tags = parse_toml_string_array(value);
                } else if key == "context" {
                    // Parse inline table: { language = "rust", framework = "clippy" }
                    current_context = parse_toml_context(value);
                } else {
                    // Strip quotes.
                    let clean = value.trim_matches('"');
                    map.insert(key.to_string(), unescape_toml(clean));
                }
            }
        }
    }

    if let Some(map) = current {
        solutions.push(map_to_entry(map, current_tags, current_context));
    }

    Ok(SolutionsFile { solutions })
}

fn unescape_toml(s: &str) -> String {
    s.replace("\\\"", "\"").replace("\\\\", "\\")
}

fn map_to_entry(
    map: HashMap<String, String>,
    tags: Vec<String>,
    context: SolutionContext,
) -> SolutionEntry {
    SolutionEntry {
        id: map.get("id").cloned().unwrap_or_default(),
        problem: map.get("problem").cloned().unwrap_or_default(),
        solution: map.get("solution").cloned().unwrap_or_default(),
        context,
        tags,
        source_category: map.get("source_category").cloned(),
        created_at: map
            .get("created_at")
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now),
    }
}

fn parse_toml_string_array(value: &str) -> Vec<String> {
    let inner = value.trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_toml_context(value: &str) -> SolutionContext {
    let inner = value.trim_start_matches('{').trim_end_matches('}');
    let mut ctx = SolutionContext::default();
    for part in inner.split(',') {
        if let Some((key, val)) = part.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"');
            match key {
                "language" => ctx.language = Some(val.to_string()),
                "framework" => ctx.framework = Some(val.to_string()),
                _ => {}
            }
        }
    }
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_entry(id: &str, problem: &str, solution: &str) -> SolutionEntry {
        SolutionEntry {
            id: id.to_string(),
            problem: problem.to_string(),
            solution: solution.to_string(),
            context: SolutionContext {
                language: Some("rust".into()),
                framework: Some("clippy".into()),
            },
            tags: vec!["testing".into(), "lint".into()],
            source_category: Some("Convention".into()),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        let entry = make_entry("sol_001", "Problem A", "Solution A");
        store.save(std::slice::from_ref(&entry)).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "sol_001");
        assert_eq!(loaded[0].problem, "Problem A");
        assert_eq!(loaded[0].context.language, Some("rust".into()));
        assert_eq!(loaded[0].tags, vec!["testing", "lint"]);
    }

    #[test]
    fn add_deduplicates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        let e1 = make_entry(
            "sol_001",
            "Clippy warns about assert!(true)",
            "Use meaningful assertions",
        );
        let e2 = make_entry(
            "sol_002",
            "Clippy warns about assert!(true)",
            "Different solution",
        );

        assert!(store.add(e1).unwrap());
        assert!(!store.add(e2).unwrap()); // Duplicate problem

        assert_eq!(store.load().unwrap().len(), 1);
    }

    #[test]
    fn remove_by_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        let e1 = make_entry("sol_001", "Problem A", "Solution A");
        let e2 = make_entry("sol_002", "Problem B", "Solution B");
        store.save(&[e1, e2]).unwrap();

        assert!(store.remove("sol_001").unwrap());
        assert_eq!(store.load().unwrap().len(), 1);
        assert!(!store.remove("sol_999").unwrap());
    }

    #[test]
    fn find_by_tag() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        let mut e1 = make_entry("sol_001", "Problem A", "Solution A");
        e1.tags = vec!["testing".into()];
        let mut e2 = make_entry("sol_002", "Problem B", "Solution B");
        e2.tags = vec!["performance".into()];
        store.save(&[e1, e2]).unwrap();

        let found = store.find_by_tag(&["testing".into()]).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "sol_001");
    }

    #[test]
    fn find_by_context() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        let mut e1 = make_entry("sol_001", "Problem A", "Solution A");
        e1.context.language = Some("rust".into());
        let mut e2 = make_entry("sol_002", "Problem B", "Solution B");
        e2.context.language = Some("typescript".into());
        store.save(&[e1, e2]).unwrap();

        let found = store.find_by_context(Some("rust"), None).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn next_id_generation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        assert_eq!(store.next_id().unwrap(), "sol_001");

        let e = make_entry("sol_003", "P", "S");
        store.save(&[e]).unwrap();
        assert_eq!(store.next_id().unwrap(), "sol_004");
    }

    #[test]
    fn merge_with_dedup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        let e1 = make_entry("sol_001", "Problem A", "Solution A");
        store.save(&[e1]).unwrap();

        let incoming = vec![
            make_entry("sol_001", "Problem A", "Dup solution"), // duplicate
            make_entry("sol_002", "Problem C", "Solution C"),   // new
        ];

        let (new_count, dup_count) = store.merge(&incoming).unwrap();
        assert_eq!(new_count, 1);
        assert_eq!(dup_count, 1);
        assert_eq!(store.load().unwrap().len(), 2);
    }

    #[test]
    fn similarity_check() {
        assert!(is_similar_problem("foo bar baz", "foo bar baz"));
        assert!(is_similar_problem("Foo Bar Baz", "foo bar baz"));
        assert!(!is_similar_problem(
            "completely different",
            "nothing alike here"
        ));
    }

    #[test]
    fn toml_round_trip_with_special_chars() {
        let entry = SolutionEntry {
            id: "sol_001".into(),
            problem: "Problem with \"quotes\" and \\backslashes".into(),
            solution: "Escape them properly".into(),
            context: SolutionContext::default(),
            tags: vec![],
            source_category: None,
            created_at: Utc::now(),
        };

        let file = SolutionsFile {
            solutions: vec![entry],
        };
        let serialized = toml_serialize(&file).unwrap();
        let parsed = toml_parse(&serialized).unwrap();
        assert_eq!(parsed.solutions[0].problem, file.solutions[0].problem);
    }

    #[test]
    fn empty_file_loads_ok() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);
        let solutions = store.load().unwrap();
        assert!(solutions.is_empty());
    }

    #[test]
    fn load_existing_toml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");

        let content = r#"
[[solutions]]
id = "sol_001"
problem = "Clippy warns about assert!(true) in tests"
solution = "Use assert_eq!(2+2, 4) or meaningful assertions instead"
context = { language = "rust", framework = "clippy" }
tags = ["testing", "lint", "clippy"]
source_category = "Convention"
created_at = "2024-01-15T10:30:00Z"
"#;
        fs::write(&path, content).unwrap();

        let store = SolutionStore::new(&path);
        let solutions = store.load().unwrap();
        assert_eq!(solutions.len(), 1);
        assert_eq!(solutions[0].id, "sol_001");
        assert_eq!(solutions[0].tags, vec!["testing", "lint", "clippy"]);
        assert_eq!(solutions[0].context.language, Some("rust".into()));
    }

    #[test]
    fn merge_reassigns_ids() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("solutions.toml");
        let store = SolutionStore::new(&path);

        let e1 = make_entry("sol_001", "Existing problem", "Existing solution");
        store.save(&[e1]).unwrap();

        let incoming = vec![make_entry("sol_001", "New unique problem", "New solution")];
        let (new_count, _) = store.merge(&incoming).unwrap();
        assert_eq!(new_count, 1);

        let all = store.load().unwrap();
        assert_eq!(all.len(), 2);
        // The merged entry should have a new ID, not "sol_001".
        assert_eq!(all[1].id, "sol_002");
    }
}
