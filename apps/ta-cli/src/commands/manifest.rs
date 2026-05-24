// manifest.rs — `ta manifest` CLI commands (v0.16.1.5).
//
// Provides:
//   - `ta manifest init`           — scaffold .ta/project-manifest.md
//   - `ta manifest validate`       — check structure and required fields
//   - `ta manifest show`           — print this project's manifest
//   - `ta manifest show <name>`    — print a linked project's manifest

use std::path::Path;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

/// Maximum allowed line count for a project manifest.
const MAX_MANIFEST_LINES: usize = 600;

#[derive(Debug, Subcommand)]
pub enum ManifestCommands {
    /// Scaffold `.ta/project-manifest.md` from project metadata.
    ///
    /// Detects project language (Cargo.toml, package.json, go.mod, etc.),
    /// fills in name from the directory, and generates section stubs ready
    /// for you to fill in.
    Init,
    /// Validate the manifest format and required fields.
    ///
    /// Checks for required frontmatter fields (name, type, language) and
    /// the four required sections (Purpose, Architecture, Public API,
    /// Integration Notes). Reports warnings for optional best practices.
    Validate,
    /// Print a project manifest.
    ///
    /// With no arguments, prints this project's manifest.
    /// With a link name, resolves and prints the linked project's manifest.
    Show {
        /// Name of a linked project (from .ta/links.toml). Omit for this project.
        link_name: Option<String>,
    },
}

pub fn execute(command: &ManifestCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    match command {
        ManifestCommands::Init => execute_init(project_root),
        ManifestCommands::Validate => execute_validate(project_root),
        ManifestCommands::Show { link_name } => execute_show(project_root, link_name.as_deref()),
    }
}

// ── init ──────────────────────────────────────────────────────────────────────

fn execute_init(project_root: &Path) -> anyhow::Result<()> {
    let manifest_path = project_root.join(".ta").join("project-manifest.md");
    if manifest_path.exists() {
        println!("Manifest already exists at {}", manifest_path.display());
        println!("Edit it directly or delete it and re-run `ta manifest init`.");
        return Ok(());
    }

    let name = detect_project_name(project_root);
    let language = detect_language(project_root);
    let project_type = infer_project_type(project_root, &language);

    let content = format!(
        r#"name: {name}
type: {project_type}
language: {language}
---

## Purpose

One paragraph describing what this project does and who it serves.

## Architecture

Key components and how they fit together. 3–5 sentences describing the main
modules, their responsibilities, and how data flows between them.

## Public API / Interface

What external callers depend on. For a service: key endpoints and payloads.
For a library: exported types and functions. For a CLI: commands and flags.

## Integration Notes

What callers need to know: auth conventions, data formats, error handling,
versioning guarantees, known quirks.
"#,
        name = name,
        project_type = project_type,
        language = language,
    );

    std::fs::create_dir_all(manifest_path.parent().unwrap())?;
    std::fs::write(&manifest_path, &content)?;
    println!("Created {}", manifest_path.display());
    println!();
    println!("Fill in the four sections, then run `ta manifest validate` to check it.");
    Ok(())
}

fn detect_project_name(project_root: &Path) -> String {
    // Try Cargo.toml [package] name.
    if let Ok(content) = std::fs::read_to_string(project_root.join("Cargo.toml")) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("name") {
                if let Some(val) = extract_toml_string_value(line) {
                    return val;
                }
            }
        }
    }
    // Try package.json name.
    if let Ok(content) = std::fs::read_to_string(project_root.join("package.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(name) = json["name"].as_str() {
                return name.to_string();
            }
        }
    }
    // Fall back to directory name.
    project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_string()
}

fn detect_language(project_root: &Path) -> String {
    if project_root.join("Cargo.toml").exists() {
        "rust".to_string()
    } else if project_root.join("package.json").exists() {
        "typescript".to_string()
    } else if project_root.join("go.mod").exists() {
        "go".to_string()
    } else if project_root.join("pyproject.toml").exists()
        || project_root.join("setup.py").exists()
        || project_root.join("setup.cfg").exists()
    {
        "python".to_string()
    } else if project_root.join("build.gradle").exists()
        || project_root.join("build.gradle.kts").exists()
    {
        "kotlin".to_string()
    } else if project_root.join("CMakeLists.txt").exists() {
        "cpp".to_string()
    } else {
        "unknown".to_string()
    }
}

fn infer_project_type(project_root: &Path, language: &str) -> &'static str {
    // Heuristics based on common patterns.
    if project_root.join("Cargo.toml").exists() {
        if let Ok(content) = std::fs::read_to_string(project_root.join("Cargo.toml")) {
            if content.contains("[[bin]]") || content.contains("[bin]") {
                return "cli";
            }
            if content.contains("[lib]") {
                return "library";
            }
        }
    }
    if language == "python" && project_root.join("pyproject.toml").exists() {
        return "library";
    }
    "app"
}

fn extract_toml_string_value(line: &str) -> Option<String> {
    // Parse simple `key = "value"` or `key = 'value'`.
    let after_eq = line.split_once('=')?.1.trim();
    if (after_eq.starts_with('"') && after_eq.ends_with('"'))
        || (after_eq.starts_with('\'') && after_eq.ends_with('\''))
    {
        let inner = &after_eq[1..after_eq.len() - 1];
        return Some(inner.to_string());
    }
    None
}

// ── validate ──────────────────────────────────────────────────────────────────

/// Validation result for a manifest.
#[derive(Debug)]
pub struct ValidationResult {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn validate_manifest(content: &str) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let lines: Vec<&str> = content.lines().collect();

    // Check maximum length.
    if lines.len() > MAX_MANIFEST_LINES {
        errors.push(format!(
            "manifest exceeds maximum length ({} lines, limit {})",
            lines.len(),
            MAX_MANIFEST_LINES
        ));
    }

    // Parse frontmatter: lines before the `---` separator.
    let fm_end = lines.iter().position(|l| l.trim() == "---").unwrap_or(0);

    let frontmatter: Vec<&str> = lines[..fm_end].to_vec();
    let body = &lines[fm_end..];

    // Required frontmatter fields.
    for required in &["name", "type", "language"] {
        if !frontmatter
            .iter()
            .any(|l| l.starts_with(&format!("{}:", required)))
        {
            errors.push(format!(
                "missing required frontmatter field: '{}'",
                required
            ));
        }
    }

    // Optional frontmatter fields.
    if !frontmatter.iter().any(|l| l.starts_with("version:")) {
        warnings.push("optional field 'version' not set in frontmatter".to_string());
    }

    // Required sections.
    let body_text = body.join("\n");
    for section in &[
        "## Purpose",
        "## Architecture",
        "## Public API",
        "## Integration Notes",
    ] {
        if !body_text.contains(section) {
            errors.push(format!("missing required section: '{}'", section));
        }
    }

    // Warn about unfilled stub content.
    if body_text.contains("One paragraph describing what this project does") {
        warnings.push("Purpose section contains scaffold placeholder text".to_string());
    }
    if body_text.contains("Key components and how they fit together") {
        warnings.push("Architecture section contains scaffold placeholder text".to_string());
    }

    ValidationResult { errors, warnings }
}

fn execute_validate(project_root: &Path) -> anyhow::Result<()> {
    let manifest_path = project_root.join(".ta").join("project-manifest.md");
    if !manifest_path.exists() {
        return Err(anyhow::anyhow!(
            "No manifest found at {}. Run `ta manifest init` to create one.",
            manifest_path.display()
        ));
    }

    let content = std::fs::read_to_string(&manifest_path)?;
    let result = validate_manifest(&content);

    if result.errors.is_empty() && result.warnings.is_empty() {
        println!("✓ Manifest is valid.");
        return Ok(());
    }

    for warn in &result.warnings {
        println!("[warn] {}", warn);
    }
    for err in &result.errors {
        println!("[FAIL] {}", err);
    }
    println!();

    if result.is_valid() {
        println!("Manifest is valid ({} warning(s)).", result.warnings.len());
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} validation error(s). Fix before using this manifest.",
            result.errors.len()
        ))
    }
}

// ── show ──────────────────────────────────────────────────────────────────────

fn execute_show(project_root: &Path, link_name: Option<&str>) -> anyhow::Result<()> {
    let content = match link_name {
        None => {
            let path = project_root.join(".ta").join("project-manifest.md");
            if !path.exists() {
                return Err(anyhow::anyhow!(
                    "No manifest at {}. Run `ta manifest init` to create one.",
                    path.display()
                ));
            }
            std::fs::read_to_string(&path)?
        }
        Some(name) => {
            let links = ta_workspace::links::load(project_root);
            let link = links.iter().find(|l| l.name == name).ok_or_else(|| {
                anyhow::anyhow!(
                    "No link named '{}' in .ta/links.toml. Run `ta link list` to see all links.",
                    name
                )
            })?;
            let cache_dir = project_root.join(".ta").join("link-cache");
            link.read_manifest(project_root, &cache_dir).ok_or_else(|| {
                anyhow::anyhow!(
                    "No manifest found for '{}'. Check that the path exists and has .ta/project-manifest.md, or run `ta link refresh {}` to fetch a remote manifest.",
                    name, name
                )
            })?
        }
    };

    println!("{}", content);
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_validate_rejects_missing_sections() {
        let content = "name: test\ntype: app\nlanguage: rust\n---\n\n## Purpose\n\nSome purpose.\n";
        let result = validate_manifest(content);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("## Architecture")));
        assert!(result.errors.iter().any(|e| e.contains("## Public API")));
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("## Integration Notes")));
    }

    #[test]
    fn manifest_validate_rejects_missing_frontmatter() {
        let content = "---\n\n## Purpose\n\nP.\n## Architecture\n\nA.\n## Public API\n\nB.\n## Integration Notes\n\nC.\n";
        let result = validate_manifest(content);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("name")));
    }

    #[test]
    fn manifest_validate_valid_content() {
        let content = "name: myapp\ntype: service\nlanguage: rust\nversion: 1.0.0\n---\n\n## Purpose\n\nReal description.\n\n## Architecture\n\nReal arch.\n\n## Public API / Interface\n\nReal API.\n\n## Integration Notes\n\nReal notes.\n";
        let result = validate_manifest(content);
        assert!(result.is_valid(), "errors: {:?}", result.errors);
    }

    #[test]
    fn manifest_init_creates_valid_scaffold() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        execute_init(dir.path()).unwrap();
        let manifest_path = dir.path().join(".ta").join("project-manifest.md");
        assert!(manifest_path.exists());
        let content = std::fs::read_to_string(&manifest_path).unwrap();
        // Scaffold has all four required sections.
        assert!(content.contains("## Purpose"));
        assert!(content.contains("## Architecture"));
        assert!(content.contains("## Public API"));
        assert!(content.contains("## Integration Notes"));
        // Has required frontmatter.
        assert!(content.contains("name:"));
        assert!(content.contains("type:"));
        assert!(content.contains("language:"));
    }

    #[test]
    fn manifest_init_detects_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        execute_init(dir.path()).unwrap();
        let content =
            std::fs::read_to_string(dir.path().join(".ta").join("project-manifest.md")).unwrap();
        assert!(content.contains("language: rust"));
        assert!(content.contains("name: my-crate"));
    }
}
