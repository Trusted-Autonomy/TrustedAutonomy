//! Schema loader with multi-location resolution.
//!
//! Resolution order:
//! 1. Project-local: `<project>/.ta/agents/output-schemas/<name>.yaml`
//! 2. User global: `~/.config/ta/agents/output-schemas/<name>.yaml`
//! 3. Embedded defaults compiled into the binary
//! 4. Passthrough fallback

use crate::schema::{OutputSchema, OutputSchemaError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Embedded schema YAML files for built-in agents.
/// Stored in `crates/ta-output-schema/schemas/` so they are included in the
/// published crate package (workspace-relative paths are not packaged by cargo publish).
const EMBEDDED_CLAUDE_CODE_V2: &str = include_str!("../schemas/claude-code.yaml");
const EMBEDDED_CLAUDE_CODE_V1: &str = include_str!("../schemas/claude-code-v1.yaml");
const EMBEDDED_CODEX: &str = include_str!("../schemas/codex.yaml");

/// Schema loader with configurable search paths and embedded defaults.
#[derive(Debug)]
pub struct SchemaLoader {
    /// Project-local schema directory (highest priority).
    project_dir: Option<PathBuf>,
    /// User-global schema directory.
    user_dir: Option<PathBuf>,
    /// Cache of loaded schemas.
    cache: std::sync::Mutex<HashMap<String, OutputSchema>>,
}

impl SchemaLoader {
    /// Create a loader that searches project-local, user-global, and embedded schemas.
    pub fn new(project_root: &Path) -> Self {
        let project_dir = project_root.join(".ta/agents/output-schemas");
        let user_dir = dirs_config_path().map(|d| d.join("agents/output-schemas"));

        Self {
            project_dir: Some(project_dir),
            user_dir,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Create a loader that only uses embedded schemas (for testing).
    pub fn embedded_only() -> Self {
        Self {
            project_dir: None,
            user_dir: None,
            cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Load a schema by agent name. Uses cache for repeated lookups.
    ///
    /// Resolution order: project-local → user-global → embedded → passthrough.
    /// Agent name aliases: "claude-code" also tries "claude-code-v2".
    pub fn load(&self, agent_name: &str) -> Result<OutputSchema, OutputSchemaError> {
        // Check cache first.
        if let Ok(cache) = self.cache.lock() {
            if let Some(schema) = cache.get(agent_name) {
                return Ok(schema.clone());
            }
        }

        let schema = self.load_uncached(agent_name)?;

        // Cache for future lookups.
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(agent_name.to_string(), schema.clone());
        }

        Ok(schema)
    }

    /// Load without cache.
    fn load_uncached(&self, agent_name: &str) -> Result<OutputSchema, OutputSchemaError> {
        // Normalize name: "claude-code" is an alias for the v2 schema file.
        let names_to_try = match agent_name {
            "claude-code" => vec!["claude-code", "claude-code-v2"],
            other => vec![other],
        };

        // 1. Project-local.
        if let Some(ref dir) = self.project_dir {
            for name in &names_to_try {
                let path = dir.join(format!("{}.yaml", name));
                if path.exists() {
                    tracing::debug!(path = %path.display(), "loading project-local output schema");
                    return load_and_validate(&path);
                }
            }
        }

        // 2. User-global.
        if let Some(ref dir) = self.user_dir {
            for name in &names_to_try {
                let path = dir.join(format!("{}.yaml", name));
                if path.exists() {
                    tracing::debug!(path = %path.display(), "loading user-global output schema");
                    return load_and_validate(&path);
                }
            }
        }

        // 3. Embedded defaults.
        for name in &names_to_try {
            if let Some(yaml) = embedded_schema(name) {
                tracing::debug!(agent = name, "loading embedded output schema");
                return parse_and_validate(yaml, name);
            }
        }

        // 4. Passthrough fallback — no schema found, relay raw output.
        tracing::info!(
            agent = agent_name,
            "no output schema found, using passthrough"
        );
        Ok(OutputSchema::passthrough())
    }

    /// List all available schema names (embedded + filesystem).
    pub fn available_schemas(&self) -> Vec<String> {
        let mut names: Vec<String> = vec![
            "claude-code".into(),
            "claude-code-v1".into(),
            "codex".into(),
        ];

        // Add filesystem schemas.
        for dir in [&self.project_dir, &self.user_dir]
            .iter()
            .copied()
            .flatten()
        {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Some(stem) = entry.path().file_stem() {
                        let name = stem.to_string_lossy().to_string();
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                }
            }
        }

        names.sort();
        names
    }
}

/// Load a schema from a YAML file and validate it.
fn load_and_validate(path: &Path) -> Result<OutputSchema, OutputSchemaError> {
    let content = std::fs::read_to_string(path)?;
    let file_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    parse_and_validate(&content, &file_name)
}

/// Parse YAML and validate the schema.
fn parse_and_validate(yaml: &str, source: &str) -> Result<OutputSchema, OutputSchemaError> {
    let schema: OutputSchema = serde_yaml::from_str(yaml)
        .map_err(|e| OutputSchemaError::Parse(format!("{}: {}", source, e)))?;
    schema.validate()?;
    Ok(schema)
}

/// Look up an embedded schema by name.
fn embedded_schema(name: &str) -> Option<&'static str> {
    match name {
        "claude-code" | "claude-code-v2" => Some(EMBEDDED_CLAUDE_CODE_V2),
        "claude-code-v1" => Some(EMBEDDED_CLAUDE_CODE_V1),
        "codex" => Some(EMBEDDED_CODEX),
        _ => None,
    }
}

/// Get the user config directory (~/.config/ta on Unix, ~/AppData/Roaming/ta on Windows).
fn dirs_config_path() -> Option<PathBuf> {
    // Use $XDG_CONFIG_HOME or ~/.config on Unix, %APPDATA% on Windows.
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("ta"));
    }
    #[cfg(unix)]
    {
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".config/ta"))
    }
    #[cfg(windows)]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|a| PathBuf::from(a).join("ta"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn embedded_schemas_parse_and_validate() {
        let loader = SchemaLoader::embedded_only();
        for name in &["claude-code", "claude-code-v1", "codex"] {
            let schema = loader.load(name).unwrap();
            schema.validate().unwrap();
            assert!(!schema.agent.is_empty());
        }
    }

    #[test]
    fn unknown_agent_returns_passthrough() {
        let loader = SchemaLoader::embedded_only();
        let schema = loader.load("unknown-agent").unwrap();
        assert_eq!(schema.agent, "passthrough");
    }

    #[test]
    fn project_local_schema_takes_priority() {
        let dir = tempdir().unwrap();
        let schema_dir = dir.path().join(".ta/agents/output-schemas");
        std::fs::create_dir_all(&schema_dir).unwrap();

        // Write a custom schema that overrides embedded.
        let custom = r#"
agent: claude-code
schema_version: 99
format: stream-json
extractors: []
suppress: []
model_paths: []
"#;
        std::fs::write(schema_dir.join("claude-code.yaml"), custom).unwrap();

        let loader = SchemaLoader::new(dir.path());
        let schema = loader.load("claude-code").unwrap();
        assert_eq!(schema.schema_version, 99);
    }

    #[test]
    fn cached_schemas_are_reused() {
        let loader = SchemaLoader::embedded_only();
        let s1 = loader.load("claude-code").unwrap();
        let s2 = loader.load("claude-code").unwrap();
        assert_eq!(s1.agent, s2.agent);
    }

    #[test]
    fn available_schemas_includes_builtins() {
        let loader = SchemaLoader::embedded_only();
        let names = loader.available_schemas();
        assert!(names.contains(&"claude-code".to_string()));
        assert!(names.contains(&"codex".to_string()));
    }

    #[test]
    fn invalid_yaml_returns_parse_error() {
        let result = parse_and_validate("not: valid: yaml: [", "test");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_schema_returns_validation_error() {
        let yaml = r#"
agent: ""
schema_version: 0
format: stream-json
extractors: []
"#;
        let result = parse_and_validate(yaml, "test");
        assert!(result.is_err());
    }
}
