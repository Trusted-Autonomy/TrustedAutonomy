// key_schema.rs — Project-aware key schema for memory entries (v0.6.3).
//
// Keys use `{domain}:{topic}` format. The domain is derived from auto-detected
// project type (Rust workspace, TypeScript, Python, Go, generic fallback).
// Configurable via optional `.ta/memory.toml`.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Detected project type based on filesystem signals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectType {
    /// Rust workspace (`Cargo.toml` with `[workspace]`).
    RustWorkspace,
    /// TypeScript project (`package.json` + `tsconfig.json`).
    TypeScript,
    /// Python project (`pyproject.toml` or `setup.py`).
    Python,
    /// Go project (`go.mod`).
    Go,
    /// Unreal Engine C++ project (`*.uproject`).
    UnrealCpp,
    /// Unity C# project (`Assets/` directory + `*.sln`).
    UnityCsharp,
    /// Fallback for unrecognized projects.
    Generic,
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RustWorkspace => write!(f, "rust-workspace"),
            Self::TypeScript => write!(f, "typescript"),
            Self::Python => write!(f, "python"),
            Self::Go => write!(f, "go"),
            Self::UnrealCpp => write!(f, "unreal-cpp"),
            Self::UnityCsharp => write!(f, "unity-csharp"),
            Self::Generic => write!(f, "generic"),
        }
    }
}

/// Domain mapping for memory keys.
///
/// Maps abstract concepts (module map, type system, build tool) to
/// project-specific key prefixes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyDomainMap {
    /// Key prefix for module/crate/package map entries.
    pub module_map: String,
    /// Key prefix for individual modules.
    pub module: String,
    /// Key prefix for type system entries (trait, interface, protocol).
    pub type_system: String,
    /// Key prefix for build tool conventions.
    pub build_tool: String,
}

impl KeyDomainMap {
    /// Default domain mapping for a project type.
    pub fn for_project_type(project_type: &ProjectType) -> Self {
        match project_type {
            ProjectType::RustWorkspace => Self {
                module_map: "crate-map".into(),
                module: "crate".into(),
                type_system: "trait".into(),
                build_tool: "cargo".into(),
            },
            ProjectType::TypeScript => Self {
                module_map: "package-map".into(),
                module: "package".into(),
                type_system: "interface".into(),
                build_tool: "npm".into(),
            },
            ProjectType::Python => Self {
                module_map: "module-map".into(),
                module: "module".into(),
                type_system: "protocol".into(),
                build_tool: "pip".into(),
            },
            ProjectType::Go => Self {
                module_map: "package-map".into(),
                module: "package".into(),
                type_system: "interface".into(),
                build_tool: "go".into(),
            },
            ProjectType::UnrealCpp => Self {
                module_map: "module-map".into(),
                module: "module".into(),
                type_system: "uclass".into(),
                build_tool: "ubt".into(),
            },
            ProjectType::UnityCsharp => Self {
                module_map: "assembly-map".into(),
                module: "assembly".into(),
                type_system: "monobehaviour".into(),
                build_tool: "msbuild".into(),
            },
            ProjectType::Generic => Self {
                module_map: "component-map".into(),
                module: "component".into(),
                type_system: "type".into(),
                build_tool: "build".into(),
            },
        }
    }
}

/// Optional configuration from `.ta/memory.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Project type override (auto-detected when absent).
    #[serde(default)]
    pub project: Option<ProjectConfig>,
    /// Key domain overrides.
    #[serde(default)]
    pub key_domains: Option<KeyDomainsConfig>,
    /// Backend selection: "ruvector" (default), "file", or "plugin".
    #[serde(default)]
    pub backend: Option<String>,
    /// Plugin name when `backend = "plugin"` (e.g., "supermemory").
    ///
    /// The binary is resolved as:
    /// 1. `.ta/plugins/memory/<name>/memory.toml`
    /// 2. `~/.config/ta/plugins/memory/<name>/memory.toml`
    /// 3. `ta-memory-<name>` on `$PATH`
    #[serde(default)]
    pub plugin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(rename = "type")]
    pub project_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyDomainsConfig {
    pub module_map: Option<String>,
    pub module: Option<String>,
    pub type_system: Option<String>,
    pub build_tool: Option<String>,
}

/// Resolved key schema: project type + domain mapping.
#[derive(Debug, Clone)]
pub struct KeySchema {
    pub project_type: ProjectType,
    pub domains: KeyDomainMap,
    pub backend: String,
}

impl KeySchema {
    /// Resolve key schema from project root.
    ///
    /// 1. Load `.ta/memory.toml` if present.
    /// 2. Auto-detect project type (or use config override).
    /// 3. Build domain mapping (with optional overrides).
    pub fn resolve(project_root: &Path) -> Self {
        let config = load_memory_config(project_root);
        let project_type = detect_project_type_with_config(project_root, &config);
        let mut domains = KeyDomainMap::for_project_type(&project_type);

        // Apply overrides from config.
        if let Some(ref kd) = config.key_domains {
            if let Some(ref v) = kd.module_map {
                domains.module_map = v.clone();
            }
            if let Some(ref v) = kd.module {
                domains.module = v.clone();
            }
            if let Some(ref v) = kd.type_system {
                domains.type_system = v.clone();
            }
            if let Some(ref v) = kd.build_tool {
                domains.build_tool = v.clone();
            }
        }

        let backend = config.backend.unwrap_or_else(|| "ruvector".to_string());

        Self {
            project_type,
            domains,
            backend,
        }
    }

    /// Build a `{domain}:{topic}` key for the module map.
    pub fn module_map_key(&self) -> String {
        format!("arch:{}", self.domains.module_map)
    }

    /// Build a `{domain}:{topic}` key for a specific module.
    pub fn module_key(&self, name: &str) -> String {
        format!("arch:{}:{}", self.domains.module, name)
    }

    /// Build a `{domain}:{topic}` key for a type system entry.
    pub fn type_key(&self, name: &str) -> String {
        format!("arch:{}:{}", self.domains.type_system, name)
    }

    /// Build a negative path key for a phase.
    pub fn negative_path_key(phase: &str, slug: &str) -> String {
        format!("neg:{}:{}", phase, slug)
    }

    /// Build a state key.
    pub fn state_key(topic: &str) -> String {
        format!("state:{}", topic)
    }
}

/// Auto-detect project type from filesystem signals.
pub fn detect_project_type(project_root: &Path) -> ProjectType {
    detect_project_type_with_config(project_root, &MemoryConfig::default())
}

fn detect_project_type_with_config(project_root: &Path, config: &MemoryConfig) -> ProjectType {
    // Check config override first.
    if let Some(ref pc) = config.project {
        if let Some(ref pt) = pc.project_type {
            return match pt.as_str() {
                "rust-workspace" => ProjectType::RustWorkspace,
                "typescript" => ProjectType::TypeScript,
                "python" => ProjectType::Python,
                "go" => ProjectType::Go,
                "unreal-cpp" => ProjectType::UnrealCpp,
                "unity-csharp" => ProjectType::UnityCsharp,
                _ => ProjectType::Generic,
            };
        }
    }

    // Auto-detect from filesystem.
    let cargo_toml = project_root.join("Cargo.toml");
    if cargo_toml.exists() {
        if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
            if content.contains("[workspace]") {
                return ProjectType::RustWorkspace;
            }
        }
        // Single-crate Rust project also counts as Rust, just not workspace.
        return ProjectType::RustWorkspace;
    }

    let package_json = project_root.join("package.json");
    let tsconfig = project_root.join("tsconfig.json");
    if package_json.exists() && tsconfig.exists() {
        return ProjectType::TypeScript;
    }

    if project_root.join("pyproject.toml").exists() || project_root.join("setup.py").exists() {
        return ProjectType::Python;
    }

    if project_root.join("go.mod").exists() {
        return ProjectType::Go;
    }

    // Unreal Engine: *.uproject file in the project root.
    if let Ok(entries) = std::fs::read_dir(project_root) {
        for entry in entries.flatten() {
            if let Some(ext) = entry.path().extension() {
                if ext == "uproject" {
                    return ProjectType::UnrealCpp;
                }
            }
        }
    }

    // Unity: Assets/ directory + *.sln file in project root.
    if project_root.join("Assets").is_dir() {
        if let Ok(entries) = std::fs::read_dir(project_root) {
            let has_sln = entries
                .flatten()
                .any(|e| e.path().extension().map(|x| x == "sln").unwrap_or(false));
            if has_sln {
                return ProjectType::UnityCsharp;
            }
        }
    }

    // TypeScript without tsconfig (JS project) — still use TS conventions.
    if package_json.exists() {
        return ProjectType::TypeScript;
    }

    ProjectType::Generic
}

/// Load `.ta/memory.toml` configuration (optional).
pub fn load_memory_config(project_root: &Path) -> MemoryConfig {
    let config_path = project_root.join(".ta").join("memory.toml");
    if !config_path.exists() {
        return MemoryConfig::default();
    }

    match std::fs::read_to_string(&config_path) {
        Ok(content) => parse_memory_config(&content),
        Err(_) => MemoryConfig::default(),
    }
}

/// Parse memory config from TOML content.
///
/// Uses simple line-based parsing to avoid pulling in a full TOML crate.
fn parse_memory_config(content: &str) -> MemoryConfig {
    let mut config = MemoryConfig::default();
    let mut current_section = "";

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "[project]" {
            current_section = "project";
            continue;
        }
        if trimmed == "[key_domains]" {
            current_section = "key_domains";
            continue;
        }
        if trimmed.starts_with('[') {
            current_section = "";
            continue;
        }

        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');

            match current_section {
                "project" => {
                    if key == "type" {
                        config.project = Some(ProjectConfig {
                            project_type: Some(value.to_string()),
                        });
                    }
                }
                "key_domains" => {
                    let kd = config.key_domains.get_or_insert(KeyDomainsConfig {
                        module_map: None,
                        module: None,
                        type_system: None,
                        build_tool: None,
                    });
                    match key {
                        "module_map" => kd.module_map = Some(value.to_string()),
                        "module" => kd.module = Some(value.to_string()),
                        "type_system" => kd.type_system = Some(value.to_string()),
                        "build_tool" => kd.build_tool = Some(value.to_string()),
                        _ => {}
                    }
                }
                _ => {
                    // Top-level keys.
                    if key == "backend" {
                        config.backend = Some(value.to_string());
                    }
                }
            }
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detect_rust_workspace() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::RustWorkspace);
    }

    #[test]
    fn detect_typescript() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::TypeScript);
    }

    #[test]
    fn detect_python() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::Python);
    }

    #[test]
    fn detect_go() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/foo\n").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::Go);
    }

    #[test]
    fn detect_generic_fallback() {
        let dir = TempDir::new().unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::Generic);
    }

    #[test]
    fn detect_unreal() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("MyGame.uproject"), "{}").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::UnrealCpp);
    }

    #[test]
    fn detect_unity() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("Assets")).unwrap();
        std::fs::write(dir.path().join("MyGame.sln"), "").unwrap();
        assert_eq!(detect_project_type(dir.path()), ProjectType::UnityCsharp);
    }

    #[test]
    fn unreal_cpp_domain_map() {
        let domains = KeyDomainMap::for_project_type(&ProjectType::UnrealCpp);
        assert_eq!(domains.module_map, "module-map");
        assert_eq!(domains.type_system, "uclass");
        assert_eq!(domains.build_tool, "ubt");
    }

    #[test]
    fn unity_csharp_domain_map() {
        let domains = KeyDomainMap::for_project_type(&ProjectType::UnityCsharp);
        assert_eq!(domains.module_map, "assembly-map");
        assert_eq!(domains.type_system, "monobehaviour");
        assert_eq!(domains.build_tool, "msbuild");
    }

    #[test]
    fn config_override_project_type() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("memory.toml"),
            "[project]\ntype = \"typescript\"\n",
        )
        .unwrap();

        // Even though Cargo.toml exists, config overrides detection.
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();

        let schema = KeySchema::resolve(dir.path());
        assert_eq!(schema.project_type, ProjectType::TypeScript);
        assert_eq!(schema.domains.module_map, "package-map");
        assert_eq!(schema.domains.type_system, "interface");
    }

    #[test]
    fn config_custom_domains() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("memory.toml"),
            "[key_domains]\nmodule_map = \"service-map\"\ntype_system = \"schema\"\n",
        )
        .unwrap();

        let schema = KeySchema::resolve(dir.path());
        assert_eq!(schema.domains.module_map, "service-map");
        assert_eq!(schema.domains.type_system, "schema");
        // Non-overridden fields use defaults for Generic.
        assert_eq!(schema.domains.module, "component");
    }

    #[test]
    fn key_generation() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

        let schema = KeySchema::resolve(dir.path());
        assert_eq!(schema.module_map_key(), "arch:crate-map");
        assert_eq!(schema.module_key("ta-memory"), "arch:crate:ta-memory");
        assert_eq!(schema.type_key("MemoryStore"), "arch:trait:MemoryStore");
        assert_eq!(
            KeySchema::negative_path_key("v0.6.3", "redis-caching"),
            "neg:v0.6.3:redis-caching"
        );
        assert_eq!(KeySchema::state_key("plan-progress"), "state:plan-progress");
    }

    #[test]
    fn parse_backend_config() {
        let config = parse_memory_config("backend = \"fs\"\n");
        assert_eq!(config.backend.as_deref(), Some("fs"));
    }

    #[test]
    fn empty_config_uses_defaults() {
        let config = parse_memory_config("");
        assert!(config.project.is_none());
        assert!(config.key_domains.is_none());
        assert!(config.backend.is_none());
    }
}
