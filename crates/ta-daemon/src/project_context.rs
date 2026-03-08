// project_context.rs — Per-project state encapsulation for multi-project daemon.
//
// Each `ProjectContext` holds the stores, policy, workspace path, and plan
// metadata for a single TA-managed project. The daemon can manage many
// `ProjectContext` instances simultaneously.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Encapsulates all per-project state for a single TA-managed project.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// Human-readable project name (used as the routing key).
    pub name: String,
    /// Absolute path to the project root on disk.
    pub path: PathBuf,
    /// Path to the project's plan file (relative to project root).
    pub plan_file: String,
    /// Default git branch for this project.
    pub default_branch: String,
    /// Per-project overrides loaded from `.ta/office-override.yaml`.
    pub overrides: ProjectOverrides,
    /// Whether this project is currently active (healthy on disk).
    pub active: bool,
}

/// Per-project overrides from `.ta/office-override.yaml` in each project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectOverrides {
    /// Override the daemon's default security level for this project.
    pub security_level: Option<String>,
    /// Override the default agent for this project.
    pub default_agent: Option<String>,
    /// Override max concurrent sessions for this project.
    pub max_sessions: Option<usize>,
    /// Custom tags for routing and filtering.
    pub tags: Vec<String>,
}

impl ProjectContext {
    /// Create a new `ProjectContext` from a name and path.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let overrides = Self::load_overrides(&path);
        Self {
            name: name.into(),
            path,
            plan_file: "PLAN.md".to_string(),
            default_branch: "main".to_string(),
            overrides,
            active: true,
        }
    }

    /// Create from config entry with explicit plan/branch settings.
    pub fn from_config(
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        plan: Option<String>,
        default_branch: Option<String>,
    ) -> Self {
        let mut ctx = Self::new(name, path);
        if let Some(p) = plan {
            ctx.plan_file = p;
        }
        if let Some(b) = default_branch {
            ctx.default_branch = b;
        }
        ctx
    }

    /// The `.ta` directory for this project.
    pub fn ta_dir(&self) -> PathBuf {
        self.path.join(".ta")
    }

    /// The PR packages directory for this project.
    pub fn pr_packages_dir(&self) -> PathBuf {
        self.ta_dir().join("pr_packages")
    }

    /// The memory directory for this project.
    pub fn memory_dir(&self) -> PathBuf {
        self.ta_dir().join("memory")
    }

    /// The events directory for this project.
    pub fn events_dir(&self) -> PathBuf {
        self.ta_dir().join("events")
    }

    /// The goals directory for this project.
    pub fn goals_dir(&self) -> PathBuf {
        self.ta_dir().join("goals")
    }

    /// Check if the project directory exists and has a `.ta` directory.
    pub fn validate(&self) -> Result<(), String> {
        if !self.path.exists() {
            return Err(format!(
                "Project '{}' path does not exist: {}",
                self.name,
                self.path.display()
            ));
        }
        if !self.path.is_dir() {
            return Err(format!(
                "Project '{}' path is not a directory: {}",
                self.name,
                self.path.display()
            ));
        }
        Ok(())
    }

    /// Load per-project overrides from `.ta/office-override.yaml`.
    fn load_overrides(project_root: &Path) -> ProjectOverrides {
        let override_path = project_root.join(".ta").join("office-override.yaml");
        if override_path.exists() {
            match std::fs::read_to_string(&override_path) {
                Ok(content) => match serde_yaml::from_str(&content) {
                    Ok(overrides) => return overrides,
                    Err(e) => {
                        tracing::warn!(
                            path = %override_path.display(),
                            error = %e,
                            "Invalid office-override.yaml, using defaults"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %override_path.display(),
                        error = %e,
                        "Cannot read office-override.yaml, using defaults"
                    );
                }
            }
        }
        ProjectOverrides::default()
    }

    /// Summary for status display.
    pub fn status_summary(&self) -> ProjectStatusSummary {
        let has_ta_dir = self.ta_dir().exists();
        let goal_count = if self.goals_dir().exists() {
            std::fs::read_dir(self.goals_dir())
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path().extension().and_then(|ext| ext.to_str()) == Some("json")
                        })
                        .count()
                })
                .unwrap_or(0)
        } else {
            0
        };

        ProjectStatusSummary {
            name: self.name.clone(),
            path: self.path.display().to_string(),
            active: self.active,
            initialized: has_ta_dir,
            goal_count,
            default_branch: self.default_branch.clone(),
        }
    }
}

/// Summary information for a project's status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatusSummary {
    pub name: String,
    pub path: String,
    pub active: bool,
    pub initialized: bool,
    pub goal_count: usize,
    pub default_branch: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_project_context() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ProjectContext::new("test-project", dir.path());
        assert_eq!(ctx.name, "test-project");
        assert_eq!(ctx.plan_file, "PLAN.md");
        assert_eq!(ctx.default_branch, "main");
        assert!(ctx.active);
    }

    #[test]
    fn from_config_with_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ProjectContext::from_config(
            "my-project",
            dir.path(),
            Some("ROADMAP.md".into()),
            Some("develop".into()),
        );
        assert_eq!(ctx.plan_file, "ROADMAP.md");
        assert_eq!(ctx.default_branch, "develop");
    }

    #[test]
    fn validate_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ProjectContext::new("test", dir.path());
        assert!(ctx.validate().is_ok());
    }

    #[test]
    fn validate_missing_dir() {
        let ctx = ProjectContext::new("test", "/nonexistent/path");
        let result = ctx.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn ta_dir_paths() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ProjectContext::new("test", dir.path());
        assert_eq!(ctx.ta_dir(), dir.path().join(".ta"));
        assert_eq!(ctx.pr_packages_dir(), dir.path().join(".ta/pr_packages"));
        assert_eq!(ctx.memory_dir(), dir.path().join(".ta/memory"));
        assert_eq!(ctx.events_dir(), dir.path().join(".ta/events"));
        assert_eq!(ctx.goals_dir(), dir.path().join(".ta/goals"));
    }

    #[test]
    fn project_overrides_default() {
        let overrides = ProjectOverrides::default();
        assert!(overrides.security_level.is_none());
        assert!(overrides.default_agent.is_none());
        assert!(overrides.max_sessions.is_none());
        assert!(overrides.tags.is_empty());
    }

    #[test]
    fn load_overrides_from_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("office-override.yaml"),
            "security_level: strict\ndefault_agent: codex\ntags:\n  - backend\n",
        )
        .unwrap();

        let ctx = ProjectContext::new("test", dir.path());
        assert_eq!(ctx.overrides.security_level.as_deref(), Some("strict"));
        assert_eq!(ctx.overrides.default_agent.as_deref(), Some("codex"));
        assert_eq!(ctx.overrides.tags, vec!["backend"]);
    }

    #[test]
    fn status_summary() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ProjectContext::new("test", dir.path());
        let summary = ctx.status_summary();
        assert_eq!(summary.name, "test");
        assert!(summary.active);
        assert!(!summary.initialized);
        assert_eq!(summary.goal_count, 0);
    }
}
