//! Community resource registry — parses `.ta/community-resources.toml`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Access level for a community resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    /// Agent can search and fetch only.
    ReadOnly,
    /// Agent can also annotate gaps, rate content, and propose new docs.
    ReadWrite,
    /// Resource is registered but not queried.
    Disabled,
}

impl Default for Access {
    fn default() -> Self {
        Access::ReadOnly
    }
}

impl std::fmt::Display for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Access::ReadOnly => write!(f, "read-only"),
            Access::ReadWrite => write!(f, "read-write"),
            Access::Disabled => write!(f, "disabled"),
        }
    }
}

/// A single configured community resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    /// Short name used in CLI and API calls.
    pub name: String,
    /// The kind of knowledge this resource provides (e.g., "api-integration").
    pub intent: String,
    /// Human-readable description shown to agents.
    pub description: String,
    /// Source URI: "github:<owner>/<repo>" or "local:<relative-path>".
    pub source: String,
    /// Path within the source repo that contains the documents.
    #[serde(default = "default_content_path")]
    pub content_path: String,
    /// Whether the agent can contribute back (annotate/suggest/feedback).
    #[serde(default)]
    pub access: Access,
    /// If true, the plugin injects a prompt into CLAUDE.md to query before relevant ops.
    #[serde(default)]
    pub auto_query: bool,
    /// Language filter for fetching (empty = all languages).
    #[serde(default)]
    pub languages: Vec<String>,
    /// How often to sync: "daily", "weekly", "on-demand" (default).
    #[serde(default = "default_update_frequency")]
    pub update_frequency: String,
}

fn default_content_path() -> String {
    "content/".to_string()
}

fn default_update_frequency() -> String {
    "on-demand".to_string()
}

impl Resource {
    /// Parse a "github:<owner>/<repo>" source URI.
    pub fn github_repo(&self) -> Option<(&str, &str)> {
        let rest = self.source.strip_prefix("github:")?;
        let (owner, repo) = rest.split_once('/')?;
        Some((owner, repo))
    }

    /// Parse a "local:<relative-path>" source URI.
    pub fn local_path<'a>(&'a self, workspace: &'a Path) -> Option<std::path::PathBuf> {
        let rel = self.source.strip_prefix("local:")?;
        Some(workspace.join(rel))
    }
}

/// The full registry loaded from `.ta/community-resources.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub resources: Vec<Resource>,
}

impl Registry {
    /// Load the registry from a workspace root. Returns an empty registry if the
    /// file does not exist.
    pub fn load(workspace: &Path) -> Result<Self, String> {
        let path = workspace.join(".ta").join("community-resources.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read community-resources.toml: {}", e))?;
        toml::from_str::<Self>(&content)
            .map_err(|e| format!("failed to parse community-resources.toml: {}", e))
    }

    /// Find a resource by exact name.
    pub fn find(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.name == name)
    }

    /// Find all resources matching an intent (case-insensitive prefix/exact).
    pub fn by_intent(&self, intent: &str) -> Vec<&Resource> {
        let lower = intent.to_lowercase();
        self.resources
            .iter()
            .filter(|r| r.access != Access::Disabled && r.intent.to_lowercase() == lower)
            .collect()
    }

    /// All enabled (non-disabled) resources.
    pub fn enabled(&self) -> Vec<&Resource> {
        self.resources
            .iter()
            .filter(|r| r.access != Access::Disabled)
            .collect()
    }

    /// All resources with auto_query = true.
    pub fn auto_query_resources(&self) -> Vec<&Resource> {
        self.resources
            .iter()
            .filter(|r| r.access != Access::Disabled && r.auto_query)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_toml(dir: &std::path::Path, content: &str) {
        let ta = dir.join(".ta");
        std::fs::create_dir_all(&ta).unwrap();
        let mut f = std::fs::File::create(ta.join("community-resources.toml")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn load_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::load(dir.path()).unwrap();
        assert!(reg.resources.is_empty());
    }

    #[test]
    fn load_parses_resources() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[[resources]]
name = "api-docs"
intent = "api-integration"
description = "Curated API docs"
source = "github:andrewyng/context-hub"
content_path = "content/"
access = "read-write"
auto_query = true
languages = ["python", "javascript"]

[[resources]]
name = "project-local"
intent = "project-knowledge"
description = "Local knowledge base"
source = "local:.ta/community/"
access = "read-write"
auto_query = true
"#,
        );
        let reg = Registry::load(dir.path()).unwrap();
        assert_eq!(reg.resources.len(), 2);
        let api = &reg.resources[0];
        assert_eq!(api.name, "api-docs");
        assert_eq!(api.intent, "api-integration");
        assert_eq!(api.access, Access::ReadWrite);
        assert!(api.auto_query);
        assert_eq!(api.languages, vec!["python", "javascript"]);
        assert_eq!(api.github_repo(), Some(("andrewyng", "context-hub")));
    }

    #[test]
    fn access_defaults_to_read_only() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[[resources]]
name = "x"
intent = "misc"
description = "d"
source = "local:.ta/x/"
"#,
        );
        let reg = Registry::load(dir.path()).unwrap();
        assert_eq!(reg.resources[0].access, Access::ReadOnly);
    }

    #[test]
    fn by_intent_filters_correctly() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[[resources]]
name = "a"
intent = "api-integration"
description = "A"
source = "local:.ta/a/"

[[resources]]
name = "b"
intent = "security-intelligence"
description = "B"
source = "local:.ta/b/"

[[resources]]
name = "c"
intent = "api-integration"
description = "C"
source = "local:.ta/c/"
access = "disabled"
"#,
        );
        let reg = Registry::load(dir.path()).unwrap();
        let api = reg.by_intent("api-integration");
        // "c" is disabled — should be excluded
        assert_eq!(api.len(), 1);
        assert_eq!(api[0].name, "a");
    }

    #[test]
    fn disabled_resource_excluded_from_enabled() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[[resources]]
name = "active"
intent = "x"
description = "d"
source = "local:.ta/x/"

[[resources]]
name = "off"
intent = "y"
description = "d"
source = "local:.ta/y/"
access = "disabled"
"#,
        );
        let reg = Registry::load(dir.path()).unwrap();
        assert_eq!(reg.enabled().len(), 1);
        assert_eq!(reg.enabled()[0].name, "active");
    }

    #[test]
    fn github_repo_parses_owner_and_repo() {
        let r = Resource {
            name: "x".into(),
            intent: "y".into(),
            description: "d".into(),
            source: "github:andrewyng/context-hub".into(),
            content_path: "content/".into(),
            access: Access::ReadOnly,
            auto_query: false,
            languages: vec![],
            update_frequency: "on-demand".into(),
        };
        assert_eq!(r.github_repo(), Some(("andrewyng", "context-hub")));
    }

    #[test]
    fn local_path_resolves_relative() {
        let dir = tempfile::tempdir().unwrap();
        let r = Resource {
            name: "x".into(),
            intent: "y".into(),
            description: "d".into(),
            source: "local:.ta/community/".into(),
            content_path: "".into(),
            access: Access::ReadOnly,
            auto_query: false,
            languages: vec![],
            update_frequency: "on-demand".into(),
        };
        let resolved = r.local_path(dir.path()).unwrap();
        assert_eq!(resolved, dir.path().join(".ta/community/"));
    }
}
