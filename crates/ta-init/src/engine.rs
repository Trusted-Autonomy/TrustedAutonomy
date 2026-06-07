// engine.rs — Template manifest and InitTemplate engine (v0.16.5).
//
// TemplateManifest is the deserialized form of a <name>.toml template file.
// InitTemplate combines a manifest with resolved Feature trait objects and
// embedded scaffold file content.

use serde::Deserialize;
use std::collections::HashMap;

// ── Manifest types ────────────────────────────────────────────────────────────

/// Top-level template manifest — deserialized from `templates/<name>.toml`.
#[derive(Debug, Deserialize)]
pub struct TemplateManifest {
    /// `[template]` header with name, description, version.
    pub template: TemplateHeader,
    /// List of named feature components (e.g. "kotlin-verify", "bmad-planner").
    #[serde(default)]
    pub features: Vec<String>,
    /// User-promptable variable definitions.
    #[serde(default)]
    pub vars: HashMap<String, VarDef>,
    /// Scaffold file mappings — rendered at init time.
    #[serde(rename = "scaffold", default)]
    pub scaffold: Vec<ScaffoldEntry>,
}

/// `[template]` section.
#[derive(Debug, Deserialize)]
pub struct TemplateHeader {
    pub name: String,
    pub description: String,
    pub version: String,
}

/// `[vars.<name>]` section entry.
#[derive(Debug, Deserialize)]
pub struct VarDef {
    pub r#type: String,
    pub prompt: String,
    #[serde(default)]
    pub default: String,
}

/// `[[scaffold]]` entry.
#[derive(Debug, Deserialize, Default)]
pub struct ScaffoldEntry {
    /// Template source filename (relative to the template's scaffold directory).
    pub source: String,
    /// Destination path relative to the project root.
    pub dest: String,
    /// If true, missing source files are silently skipped.
    #[serde(default)]
    pub optional: bool,
}

// ── InitTemplate ─────────────────────────────────────────────────────────────

/// A fully resolved template ready to apply to a project directory.
pub struct InitTemplate {
    pub manifest: TemplateManifest,
    /// Resolved feature implementations.
    pub features: Vec<Box<dyn crate::feature::Feature>>,
    /// Embedded scaffold content: source_filename → template_text.
    pub embedded_scaffold: HashMap<String, String>,
    /// Optional filesystem directory containing scaffold source files.
    pub scaffold_dir: Option<std::path::PathBuf>,
}

impl InitTemplate {
    /// Apply this template to the given project context.
    ///
    /// 1. Renders and writes scaffold files (skipping already-present files).
    /// 2. Applies each feature in declaration order.
    pub fn apply(&self, ctx: &crate::feature::TemplateContext) -> anyhow::Result<()> {
        let ta_dir = ctx.ta_dir();
        std::fs::create_dir_all(&ta_dir)?;

        // Render scaffold files.
        for entry in &self.manifest.scaffold {
            let dest = ctx.project_root.join(&entry.dest);

            // Don't overwrite files that already exist.
            if dest.exists() {
                continue;
            }

            let tmpl_content = if let Some(s) = self.embedded_scaffold.get(&entry.source) {
                s.clone()
            } else if let Some(dir) = &self.scaffold_dir {
                let src = dir.join(&entry.source);
                if !src.exists() {
                    if entry.optional {
                        continue;
                    }
                    anyhow::bail!("Scaffold source file not found: {}", src.display());
                }
                std::fs::read_to_string(&src).map_err(|e| {
                    anyhow::anyhow!("Failed to read scaffold source '{}': {e}", src.display())
                })?
            } else {
                if entry.optional {
                    continue;
                }
                anyhow::bail!(
                        "No scaffold source available for: {} (no embedded content and no scaffold_dir)",
                        entry.source
                    );
            };

            let rendered = crate::scaffold::render(&tmpl_content, ctx)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to create parent directory for '{}': {e}",
                        dest.display()
                    )
                })?;
            }
            std::fs::write(&dest, rendered).map_err(|e| {
                anyhow::anyhow!("Failed to write scaffold file '{}': {e}", dest.display())
            })?;
        }

        // Apply features.
        for feature in &self.features {
            feature.apply(ctx)?;
        }

        Ok(())
    }
}

// ── parse_manifest ────────────────────────────────────────────────────────────

/// Parse a template TOML string into a `TemplateManifest`.
pub fn parse_manifest(content: &str) -> anyhow::Result<TemplateManifest> {
    toml::from_str(content).map_err(|e| anyhow::anyhow!("Failed to parse template TOML: {e}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[template]
name = "test-template"
description = "A test template"
version = "1.0.0"

[vars.project_name]
type = "string"
prompt = "Project name"
default = ""

[[scaffold]]
source = "CLAUDE.md.tera"
dest = "CLAUDE.md"

[[scaffold]]
source = "optional.tera"
dest = "optional.txt"
optional = true
"#;

    const MINIMAL_TOML_WITH_FEATURES: &str = r#"
features = ["kotlin-verify"]

[template]
name = "test-template"
description = "A test template"
version = "1.0.0"

[vars.project_name]
type = "string"
prompt = "Project name"
default = ""

[[scaffold]]
source = "CLAUDE.md.tera"
dest = "CLAUDE.md"

[[scaffold]]
source = "optional.tera"
dest = "optional.txt"
optional = true
"#;

    #[test]
    fn parse_minimal_manifest() {
        let m = parse_manifest(MINIMAL_TOML).unwrap();
        assert_eq!(m.template.name, "test-template");
        assert_eq!(m.template.version, "1.0.0");
        // features is empty because the TOML has no top-level features key.
        assert!(m.features.is_empty());
        assert!(m.vars.contains_key("project_name"));
        assert_eq!(m.scaffold.len(), 2);
        assert!(!m.scaffold[0].optional);
        assert!(m.scaffold[1].optional);
    }

    #[test]
    fn parse_manifest_with_features() {
        let m = parse_manifest(MINIMAL_TOML_WITH_FEATURES).unwrap();
        assert_eq!(m.template.name, "test-template");
        assert_eq!(m.template.version, "1.0.0");
        assert_eq!(m.features, vec!["kotlin-verify"]);
        assert!(m.vars.contains_key("project_name"));
    }

    #[test]
    fn parse_invalid_toml_errors() {
        let result = parse_manifest("this is not toml {{{{");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn apply_renders_scaffold_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let ctx = crate::feature::TemplateContext {
            project_root: dir.path().to_path_buf(),
            project_name: "MyApp".to_string(),
            vars: std::collections::HashMap::new(),
        };

        let manifest = parse_manifest(MINIMAL_TOML).unwrap();
        let mut embedded = HashMap::new();
        embedded.insert(
            "CLAUDE.md.tera".to_string(),
            "# {{ project_name }}\n".to_string(),
        );

        let tmpl = InitTemplate {
            manifest,
            features: vec![],
            embedded_scaffold: embedded,
            scaffold_dir: None,
        };

        tmpl.apply(&ctx).unwrap();

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, "# MyApp\n");
    }

    #[test]
    fn apply_skips_optional_missing_scaffold() {
        let dir = tempfile::TempDir::new().unwrap();
        let ctx = crate::feature::TemplateContext {
            project_root: dir.path().to_path_buf(),
            project_name: "MyApp".to_string(),
            vars: std::collections::HashMap::new(),
        };

        let manifest = parse_manifest(MINIMAL_TOML).unwrap();
        let mut embedded = HashMap::new();
        // Only provide CLAUDE.md.tera — optional.tera is intentionally absent.
        embedded.insert(
            "CLAUDE.md.tera".to_string(),
            "# {{ project_name }}\n".to_string(),
        );

        let tmpl = InitTemplate {
            manifest,
            features: vec![],
            embedded_scaffold: embedded,
            scaffold_dir: None,
        };

        // Should not error even though optional.tera is missing.
        tmpl.apply(&ctx).unwrap();
        assert!(!dir.path().join("optional.txt").exists());
    }

    #[test]
    fn apply_skips_existing_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# existing\n").unwrap();

        let ctx = crate::feature::TemplateContext {
            project_root: dir.path().to_path_buf(),
            project_name: "MyApp".to_string(),
            vars: std::collections::HashMap::new(),
        };

        let manifest = parse_manifest(MINIMAL_TOML).unwrap();
        let mut embedded = HashMap::new();
        embedded.insert(
            "CLAUDE.md.tera".to_string(),
            "# {{ project_name }}\n".to_string(),
        );

        let tmpl = InitTemplate {
            manifest,
            features: vec![],
            embedded_scaffold: embedded,
            scaffold_dir: None,
        };

        tmpl.apply(&ctx).unwrap();

        // Should not overwrite the existing file.
        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, "# existing\n");
    }
}
