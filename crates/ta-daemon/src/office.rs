// office.rs — Office configuration parsing, project registry, and lifecycle.
//
// The office config (`office.yaml`) defines a multi-project daemon setup:
// - Named projects with paths, plan files, and branches
// - Channel-to-project routing for Discord, Slack, and email
// - Daemon socket and port configuration
//
// When no `office.yaml` exists, the daemon runs in single-project mode
// (backward compatible).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::project_context::ProjectContext;

/// Top-level office configuration, loaded from `office.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficeConfig {
    /// Office metadata.
    #[serde(default)]
    pub office: OfficeMetadata,
    /// Named projects managed by this office.
    #[serde(default)]
    pub projects: HashMap<String, ProjectEntry>,
    /// Channel routing configuration.
    #[serde(default)]
    pub channels: HashMap<String, ChannelRouting>,
}

/// Office-level metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OfficeMetadata {
    /// Human-readable office name.
    pub name: String,
    /// Daemon connection settings.
    pub daemon: DaemonSettings,
}

/// Daemon connection settings within office config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// Unix socket path for local IPC.
    pub socket: Option<String>,
    /// HTTP port for the daemon API.
    pub http_port: u16,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            socket: None,
            http_port: 3140,
        }
    }
}

/// A project entry in the office config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Absolute or `~`-expanded path to the project root.
    pub path: String,
    /// Plan file name (defaults to PLAN.md).
    pub plan: Option<String>,
    /// Default git branch (defaults to main).
    pub default_branch: Option<String>,
}

/// Channel routing configuration — maps channel identifiers to projects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelRouting {
    /// Environment variable holding the channel's auth token.
    pub token_env: Option<String>,
    /// Route map: channel identifier → route target.
    pub routes: HashMap<String, RouteTarget>,
}

/// Where a channel route points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteTarget {
    /// The project name this route targets (omit for broadcast routes).
    pub project: Option<String>,
    /// Route type: "review", "session", "notify".
    #[serde(rename = "type")]
    pub route_type: String,
    /// For notify routes: target all projects.
    pub projects: Option<String>,
}

impl OfficeConfig {
    /// Load office config from a YAML file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "Cannot read office config at {}: {}. \
                 Create an office.yaml or use single-project mode with `ta daemon`.",
                path.display(),
                e
            )
        })?;
        serde_yaml::from_str(&content).map_err(|e| {
            format!(
                "Invalid office config at {}: {}. \
                 Check the YAML syntax and required fields.",
                path.display(),
                e
            )
        })
    }

    /// Build `ProjectContext` instances from the config.
    pub fn build_project_contexts(&self) -> Result<Vec<ProjectContext>, String> {
        let mut contexts = Vec::new();
        for (name, entry) in &self.projects {
            let expanded_path = expand_tilde(&entry.path);
            let path = PathBuf::from(&expanded_path);
            let ctx = ProjectContext::from_config(
                name.clone(),
                path,
                entry.plan.clone(),
                entry.default_branch.clone(),
            );
            if let Err(e) = ctx.validate() {
                tracing::warn!(
                    project = %name,
                    error = %e,
                    "Project validation failed; project will be marked inactive"
                );
            }
            contexts.push(ctx);
        }
        Ok(contexts)
    }

    /// Resolve which project a channel message should route to.
    ///
    /// Returns `None` if the route is ambiguous or broadcast.
    pub fn resolve_channel_route(&self, channel_type: &str, channel_id: &str) -> Option<String> {
        if let Some(channel_config) = self.channels.get(channel_type) {
            if let Some(target) = channel_config.routes.get(channel_id) {
                return target.project.clone();
            }
        }
        None
    }
}

/// The project registry manages active projects at runtime.
pub struct ProjectRegistry {
    projects: RwLock<HashMap<String, ProjectContext>>,
}

impl Default for ProjectRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            projects: RwLock::new(HashMap::new()),
        }
    }

    /// Create a registry from an office config.
    pub fn from_config(config: &OfficeConfig) -> Result<Self, String> {
        let registry = Self::new();
        let contexts = config.build_project_contexts()?;
        {
            let mut projects = registry.projects.write().map_err(|e| e.to_string())?;
            for ctx in contexts {
                projects.insert(ctx.name.clone(), ctx);
            }
        }
        Ok(registry)
    }

    /// Create a single-project registry (backward compat).
    pub fn single_project(project_root: PathBuf) -> Self {
        let registry = Self::new();
        let name = project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default")
            .to_string();
        let ctx = ProjectContext::new(name.clone(), project_root);
        {
            let mut projects = registry.projects.write().unwrap();
            projects.insert(name, ctx);
        }
        registry
    }

    /// List all managed projects.
    pub fn list(&self) -> Vec<ProjectContext> {
        self.projects.read().unwrap().values().cloned().collect()
    }

    /// Get a project by name.
    pub fn get(&self, name: &str) -> Option<ProjectContext> {
        self.projects.read().unwrap().get(name).cloned()
    }

    /// Get the default (or only) project.
    ///
    /// Returns the single project in single-project mode, or `None` in
    /// multi-project mode (caller must specify).
    pub fn default_project(&self) -> Option<ProjectContext> {
        let projects = self.projects.read().unwrap();
        if projects.len() == 1 {
            projects.values().next().cloned()
        } else {
            None
        }
    }

    /// Add a project at runtime.
    pub fn add(&self, ctx: ProjectContext) -> Result<(), String> {
        let mut projects = self.projects.write().map_err(|e| e.to_string())?;
        if projects.contains_key(&ctx.name) {
            return Err(format!(
                "Project '{}' already exists. Use `ta office project remove` first to replace it.",
                ctx.name
            ));
        }
        tracing::info!(
            project = %ctx.name,
            path = %ctx.path.display(),
            "Added project to office"
        );
        projects.insert(ctx.name.clone(), ctx);
        Ok(())
    }

    /// Remove a project at runtime.
    pub fn remove(&self, name: &str) -> Result<ProjectContext, String> {
        let mut projects = self.projects.write().map_err(|e| e.to_string())?;
        projects.remove(name).ok_or_else(|| {
            format!(
                "Project '{}' not found. Available projects: {:?}",
                name,
                projects.keys().collect::<Vec<_>>()
            )
        })
    }

    /// Number of managed projects.
    pub fn len(&self) -> usize {
        self.projects.read().unwrap().len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.projects.read().unwrap().is_empty()
    }

    /// Check if running in multi-project mode.
    pub fn is_multi_project(&self) -> bool {
        self.projects.read().unwrap().len() > 1
    }

    /// Project names.
    pub fn names(&self) -> Vec<String> {
        self.projects.read().unwrap().keys().cloned().collect()
    }
}

/// Expand `~` to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") || path == "~" {
        if let Some(home) = std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok())
        {
            return path.replacen('~', &home, 1);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_office_yaml() -> &'static str {
        r##"
office:
  name: "Test Office"
  daemon:
    http_port: 3140
projects:
  project-a:
    path: "/tmp/project-a"
    plan: PLAN.md
    default_branch: main
  project-b:
    path: "/tmp/project-b"
channels:
  discord:
    token_env: TA_DISCORD_TOKEN
    routes:
      "#backend-reviews":
        project: project-a
        type: review
      "#frontend-reviews":
        project: project-b
        type: review
      "#office-status":
        type: notify
        projects: all
"##
    }

    #[test]
    fn parse_office_config() {
        let config: OfficeConfig = serde_yaml::from_str(sample_office_yaml()).unwrap();
        assert_eq!(config.office.name, "Test Office");
        assert_eq!(config.office.daemon.http_port, 3140);
        assert_eq!(config.projects.len(), 2);
        assert!(config.projects.contains_key("project-a"));
        assert!(config.projects.contains_key("project-b"));
    }

    #[test]
    fn resolve_channel_route() {
        let config: OfficeConfig = serde_yaml::from_str(sample_office_yaml()).unwrap();
        assert_eq!(
            config.resolve_channel_route("discord", "#backend-reviews"),
            Some("project-a".to_string())
        );
        assert_eq!(
            config.resolve_channel_route("discord", "#frontend-reviews"),
            Some("project-b".to_string())
        );
        // Broadcast route has no specific project.
        assert_eq!(
            config.resolve_channel_route("discord", "#office-status"),
            None
        );
        // Unknown channel.
        assert_eq!(config.resolve_channel_route("slack", "#general"), None);
    }

    #[test]
    fn project_registry_single_project() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ProjectRegistry::single_project(dir.path().to_path_buf());
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_multi_project());
        assert!(registry.default_project().is_some());
    }

    #[test]
    fn project_registry_add_remove() {
        let registry = ProjectRegistry::new();
        assert!(registry.is_empty());

        let dir = tempfile::tempdir().unwrap();
        let ctx = ProjectContext::new("test", dir.path());
        registry.add(ctx).unwrap();

        assert_eq!(registry.len(), 1);
        assert!(registry.get("test").is_some());
        assert!(registry.get("nonexistent").is_none());

        // Duplicate add should fail.
        let ctx2 = ProjectContext::new("test", dir.path());
        assert!(registry.add(ctx2).is_err());

        // Remove.
        let removed = registry.remove("test").unwrap();
        assert_eq!(removed.name, "test");
        assert!(registry.is_empty());

        // Remove nonexistent should fail.
        assert!(registry.remove("test").is_err());
    }

    #[test]
    fn project_registry_multi_project() {
        let registry = ProjectRegistry::new();
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        registry.add(ProjectContext::new("a", dir1.path())).unwrap();
        registry.add(ProjectContext::new("b", dir2.path())).unwrap();

        assert!(registry.is_multi_project());
        assert!(registry.default_project().is_none()); // Ambiguous in multi-project.
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn expand_tilde_basic() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, "/absolute/path");

        // Tilde expansion depends on HOME env var.
        if let Ok(home) = std::env::var("HOME") {
            let expanded = expand_tilde("~/projects/foo");
            assert_eq!(expanded, format!("{}/projects/foo", home));
        }
    }

    #[test]
    fn office_config_roundtrip() {
        let config: OfficeConfig = serde_yaml::from_str(sample_office_yaml()).unwrap();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: OfficeConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.office.name, config.office.name);
        assert_eq!(parsed.projects.len(), config.projects.len());
    }

    #[test]
    fn project_registry_names() {
        let registry = ProjectRegistry::new();
        let dir = tempfile::tempdir().unwrap();
        registry
            .add(ProjectContext::new("alpha", dir.path()))
            .unwrap();
        let names = registry.names();
        assert_eq!(names, vec!["alpha"]);
    }
}
