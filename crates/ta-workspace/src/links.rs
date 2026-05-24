// links.rs — Cross-project TA link resolver (v0.16.1.5).
//
// Reads `.ta/links.toml` and resolves local + remote linked project manifests.
// Invalid/unreachable links are logged as warnings — never hard failures.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Relationship type between this project and a linked project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Relationship {
    /// This project calls into the linked project's API.
    Dependency,
    /// The linked project depends on this project's interface.
    Consumer,
    /// Co-developed sibling; coordinate types, naming, and protocols.
    WorkspaceMember,
    /// Parallel development; shared conventions, no direct API coupling.
    Sibling,
    /// Architecturally related; background context only, no code coupling.
    Reference,
}

impl Relationship {
    /// Short framing sentence injected before the manifest in CLAUDE.md context.
    pub fn framing(&self, name: &str) -> String {
        match self {
            Relationship::Dependency => format!(
                "This project calls into `{}`. Do not break its API contract.",
                name
            ),
            Relationship::Consumer => format!(
                "`{}` depends on this project's interface. Changes here may break it.",
                name
            ),
            Relationship::WorkspaceMember => format!(
                "`{}` is a co-developed sibling. Coordinate types, naming, and protocols.",
                name
            ),
            Relationship::Sibling => format!(
                "`{}` is a parallel sibling. Shared conventions but no direct API coupling.",
                name
            ),
            Relationship::Reference => format!(
                "`{}` is architecturally related. Background context only — no code-level coupling.",
                name
            ),
        }
    }

    /// Badge label for Studio UI.
    pub fn badge(&self) -> &'static str {
        match self {
            Relationship::Dependency => "dependency",
            Relationship::Consumer => "consumer",
            Relationship::WorkspaceMember => "workspace-member",
            Relationship::Sibling => "sibling",
            Relationship::Reference => "reference",
        }
    }
}

impl std::fmt::Display for Relationship {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.badge())
    }
}

/// A single entry in `.ta/links.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    /// Display name for the linked project.
    pub name: String,
    /// Relative or absolute path to the linked project's root directory.
    #[serde(default)]
    pub path: Option<String>,
    /// GitHub remote: `"github:org/repo"` string. Fetched and cached.
    #[serde(default)]
    pub repo: Option<String>,
    /// How this project relates to the linked project.
    pub relationship: Relationship,
    /// Human-readable description of the link's purpose.
    #[serde(default)]
    pub description: String,
    /// Whether to inject this manifest into agent context at goal start.
    #[serde(default = "default_inject")]
    pub inject: bool,
}

fn default_inject() -> bool {
    true
}

impl Link {
    /// Resolve the absolute path to the linked project's root, given the
    /// current project's root directory.
    pub fn resolve_path(&self, project_root: &Path) -> Option<PathBuf> {
        self.path.as_ref().map(|p| {
            let p = Path::new(p);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                project_root.join(p)
            }
        })
    }

    /// Read the manifest for this link.
    ///
    /// For local links: reads `<path>/.ta/project-manifest.md`.
    /// For remote links: reads from the link-cache if present.
    /// Returns `None` if the manifest cannot be found (logged at debug).
    pub fn read_manifest(&self, project_root: &Path, link_cache_dir: &Path) -> Option<String> {
        if let Some(local_path) = self.resolve_path(project_root) {
            let manifest_path = local_path.join(".ta").join("project-manifest.md");
            match std::fs::read_to_string(&manifest_path) {
                Ok(content) => {
                    debug!(name = %self.name, path = %manifest_path.display(), "loaded local manifest");
                    return Some(content);
                }
                Err(e) => {
                    debug!(name = %self.name, path = %manifest_path.display(), err = %e, "local manifest not found");
                }
            }
        }

        // Fall back to link cache (covers both remote links and offline local).
        let cache_path = link_cache_dir.join(format!("{}.md", sanitize_name(&self.name)));
        match std::fs::read_to_string(&cache_path) {
            Ok(content) => {
                debug!(name = %self.name, "loaded manifest from link cache");
                Some(content)
            }
            Err(_) => {
                debug!(name = %self.name, "no cached manifest found");
                None
            }
        }
    }

    /// Check whether the linked project is reachable (local path exists and has manifest).
    pub fn status(&self, project_root: &Path, link_cache_dir: &Path) -> LinkStatus {
        if let Some(local_path) = self.resolve_path(project_root) {
            if !local_path.exists() {
                return LinkStatus::Unreachable {
                    reason: format!("path not found: {}", local_path.display()),
                };
            }
            let manifest_path = local_path.join(".ta").join("project-manifest.md");
            if manifest_path.exists() {
                return LinkStatus::Ok { cached: false };
            }
            return LinkStatus::MissingManifest;
        }

        if self.repo.is_some() {
            let cache_path = link_cache_dir.join(format!("{}.md", sanitize_name(&self.name)));
            if cache_path.exists() {
                if let Ok(meta) = std::fs::metadata(&cache_path) {
                    if let Ok(modified) = meta.modified() {
                        let age = std::time::SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or_default();
                        let stale = age > std::time::Duration::from_secs(24 * 3600);
                        return LinkStatus::Ok { cached: !stale };
                    }
                }
                return LinkStatus::Ok { cached: true };
            }
            return LinkStatus::MissingManifest;
        }

        LinkStatus::Unreachable {
            reason: "no path or repo configured".to_string(),
        }
    }

    /// Last refreshed time from cache file, if applicable.
    pub fn last_refreshed(&self, link_cache_dir: &Path) -> Option<DateTime<Utc>> {
        let cache_path = link_cache_dir.join(format!("{}.md", sanitize_name(&self.name)));
        let meta = std::fs::metadata(&cache_path).ok()?;
        let modified = meta.modified().ok()?;
        let secs = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        DateTime::from_timestamp(secs as i64, 0)
    }
}

/// Reachability status of a linked project.
#[derive(Debug, Clone)]
pub enum LinkStatus {
    /// Manifest found and readable.
    Ok { cached: bool },
    /// Project path is reachable but manifest is missing.
    MissingManifest,
    /// Project is not reachable.
    Unreachable { reason: String },
}

impl LinkStatus {
    pub fn indicator(&self) -> &'static str {
        match self {
            LinkStatus::Ok { cached: false } => "✓",
            LinkStatus::Ok { cached: true } => "~",
            LinkStatus::MissingManifest => "✗",
            LinkStatus::Unreachable { .. } => "—",
        }
    }

    pub fn description(&self) -> String {
        match self {
            LinkStatus::Ok { cached: false } => "manifest found".to_string(),
            LinkStatus::Ok { cached: true } => "cached (may be stale)".to_string(),
            LinkStatus::MissingManifest => {
                "manifest not found — run `ta manifest init`".to_string()
            }
            LinkStatus::Unreachable { reason } => format!("unreachable: {}", reason),
        }
    }
}

/// Parsed representation of `.ta/links.toml`.
#[derive(Debug, Default, Deserialize)]
struct LinksFile {
    #[serde(default, rename = "link")]
    links: Vec<Link>,
}

/// Load all project links from `.ta/links.toml`.
///
/// Returns an empty vec if the file doesn't exist.
/// Invalid paths or TOML parse errors are logged as warnings.
pub fn load(project_root: &Path) -> Vec<Link> {
    let path = project_root.join(".ta").join("links.toml");
    if !path.exists() {
        return Vec::new();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            warn!(path = %path.display(), err = %e, "failed to read links.toml");
            return Vec::new();
        }
    };
    match toml::from_str::<LinksFile>(&content) {
        Ok(parsed) => parsed.links,
        Err(e) => {
            warn!(path = %path.display(), err = %e, "failed to parse links.toml");
            Vec::new()
        }
    }
}

/// Add a link entry to `.ta/links.toml`, creating the file if needed.
pub fn add_link(project_root: &Path, link: &Link) -> anyhow::Result<()> {
    let path = project_root.join(".ta").join("links.toml");
    let mut content = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };

    // Check for duplicate name.
    let existing = load(project_root);
    if existing.iter().any(|l| l.name == link.name) {
        return Err(anyhow::anyhow!(
            "A link named '{}' already exists in .ta/links.toml. Remove it first with `ta link remove {}`.",
            link.name, link.name
        ));
    }

    // Serialize the new entry.
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');
    content.push_str(&serialize_link(link));

    std::fs::write(&path, &content)?;
    Ok(())
}

/// Remove a named link from `.ta/links.toml`.
pub fn remove_link(project_root: &Path, name: &str) -> anyhow::Result<bool> {
    let path = project_root.join(".ta").join("links.toml");
    if !path.exists() {
        return Ok(false);
    }
    let links = load(project_root);
    let original_count = links.len();
    let remaining: Vec<Link> = links.into_iter().filter(|l| l.name != name).collect();
    if remaining.len() == original_count {
        return Ok(false);
    }
    // Rewrite the file.
    let mut content = String::new();
    for link in &remaining {
        content.push_str(&serialize_link(link));
        content.push('\n');
    }
    std::fs::write(&path, content.trim_start())?;
    Ok(true)
}

fn serialize_link(link: &Link) -> String {
    let mut s = String::from("[[link]]\n");
    s.push_str(&format!("name = {:?}\n", link.name));
    if let Some(path) = &link.path {
        s.push_str(&format!("path = {:?}\n", path));
    }
    if let Some(repo) = &link.repo {
        s.push_str(&format!("repo = {:?}\n", repo));
    }
    s.push_str(&format!("relationship = {:?}\n", link.relationship.badge()));
    if !link.description.is_empty() {
        s.push_str(&format!("description = {:?}\n", link.description));
    }
    if !link.inject {
        s.push_str("inject = false\n");
    }
    s
}

/// Sanitize a link name for use as a filename (alphanumeric + hyphens only).
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_links_toml(dir: &Path, content: &str) {
        let ta_dir = dir.join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("links.toml"), content).unwrap();
    }

    #[test]
    fn links_load_valid_toml() {
        let dir = tempdir().unwrap();
        write_links_toml(
            dir.path(),
            r#"
[[link]]
name = "cinepipe-train"
path = "../cinepipe-train"
relationship = "workspace-member"
description = "Training pipeline"
inject = true
"#,
        );
        let links = load(dir.path());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].name, "cinepipe-train");
        assert_eq!(links[0].relationship, Relationship::WorkspaceMember);
        assert!(links[0].inject);
    }

    #[test]
    fn links_load_missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let links = load(dir.path());
        assert!(links.is_empty());
    }

    #[test]
    fn links_load_multiple_entries() {
        let dir = tempdir().unwrap();
        write_links_toml(
            dir.path(),
            r#"
[[link]]
name = "alpha"
path = "../alpha"
relationship = "dependency"

[[link]]
name = "beta"
repo = "github:org/beta"
relationship = "consumer"
"#,
        );
        let links = load(dir.path());
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].name, "alpha");
        assert_eq!(links[1].name, "beta");
    }

    #[test]
    fn link_status_reports_missing_manifest() {
        let dir = tempdir().unwrap();
        // Create a linked project dir with no manifest.
        let linked = dir.path().join("other-project");
        std::fs::create_dir_all(&linked).unwrap();
        let link = Link {
            name: "other".to_string(),
            path: Some(linked.to_str().unwrap().to_string()),
            repo: None,
            relationship: Relationship::Dependency,
            description: String::new(),
            inject: true,
        };
        let cache_dir = dir.path().join(".ta").join("link-cache");
        let status = link.status(dir.path(), &cache_dir);
        assert!(matches!(status, LinkStatus::MissingManifest));
    }

    #[test]
    fn add_and_remove_link() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let link = Link {
            name: "foo".to_string(),
            path: Some("../foo".to_string()),
            repo: None,
            relationship: Relationship::Sibling,
            description: "A sibling".to_string(),
            inject: true,
        };
        add_link(dir.path(), &link).unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "foo");

        let removed = remove_link(dir.path(), "foo").unwrap();
        assert!(removed);
        let after = load(dir.path());
        assert!(after.is_empty());
    }

    #[test]
    fn remove_nonexistent_link_returns_false() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        let removed = remove_link(dir.path(), "no-such-link").unwrap();
        assert!(!removed);
    }
}
