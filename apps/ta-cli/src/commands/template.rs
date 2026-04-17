// template.rs — Creative template management: install, list, remove, search, publish.
//
// Templates are project-scaffolding packages containing workflow.toml, .taignore,
// memory.toml (optional), policy.yaml (optional), and a template.toml manifest.
//
// Storage hierarchy:
//   ~/.config/ta/templates/<name>/   — globally installed templates
//   .ta/templates/<name>/            — project-local templates (highest priority)
//
// Sources:
//   registry name  → $TA_TEMPLATE_REGISTRY_URL/templates/<name>.tar.gz
//   github:u/r     → https://github.com/u/r/archive/refs/heads/main.tar.gz
//   https://...    → direct download
//   ./local/path   → copy directory
//
// Remote installs download the archive and provide manual extraction guidance
// when tar/flate2 is not linked (to avoid a C-dep pull). Local path install
// is fully automated.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use sha2::{Digest, Sha256};

/// Built-in template names always available without installation.
const BUILTIN_TEMPLATES: &[(&str, &str)] = &[
    ("rust-cli", "Rust CLI application with clap argument parser"),
    ("rust-lib", "Rust library crate with doctests"),
    ("python-script", "Python scripting project with pytest"),
    (
        "blender-addon",
        "Blender Python addon (bl_info, register/unregister, panel, operator)",
    ),
    ("web-api", "REST API server (language-agnostic scaffold)"),
    ("data-pipeline", "Data pipeline with input/output stages"),
];

// ── Config helpers ────────────────────────────────────────────────

fn ta_config_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("ta")
}

fn global_templates_dir() -> PathBuf {
    ta_config_dir().join("templates")
}

fn project_templates_dir(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("templates")
}

fn template_registry_url() -> String {
    std::env::var("TA_TEMPLATE_REGISTRY_URL")
        .unwrap_or_else(|_| "https://templates.trustedautonomy.dev".to_string())
}

// ── Template manifest ────────────────────────────────────────────

/// Parsed `template.toml` manifest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TemplateManifest {
    /// Short identifier (no spaces).
    pub name: String,
    /// Semver string, e.g. "1.0.0".
    pub version: String,
    /// One-line description.
    pub description: String,
    /// Searchable tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Template author.
    #[serde(default)]
    pub author: String,
    /// Minimum TA version required.
    #[serde(default)]
    pub ta_version_min: String,
    /// Optional shell script to run after files are copied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_copy_script: Option<String>,
    /// Verification commands.
    #[serde(default)]
    pub verify: TemplateVerify,
    /// File mappings.
    #[serde(default)]
    pub files: TemplateFiles,
    /// Optional guided onboarding config.
    #[serde(default)]
    pub onboarding: TemplateOnboarding,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TemplateVerify {
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TemplateFiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_toml: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taignore: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_toml: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_yaml: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_json: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TemplateOnboarding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_prompt: Option<String>,
}

/// Load a template manifest from a directory.
pub fn load_manifest(dir: &Path) -> anyhow::Result<TemplateManifest> {
    let path = dir.join("template.toml");
    let content = std::fs::read_to_string(&path).map_err(|e| {
        anyhow::anyhow!(
            "Could not read template manifest at '{}': {e}\n\
             A valid template directory must contain a template.toml file.",
            path.display()
        )
    })?;
    let manifest: TemplateManifest = toml::from_str(&content).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse template.toml at '{}': {e}\n\
             Check the TOML syntax and required fields (name, version, description).",
            path.display()
        )
    })?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Validate required manifest fields.
fn validate_manifest(m: &TemplateManifest) -> anyhow::Result<()> {
    if m.name.is_empty() {
        anyhow::bail!("template.toml: 'name' field is required and must not be empty");
    }
    if m.name.contains(' ') {
        anyhow::bail!(
            "template.toml: 'name' field '{}' must not contain spaces — use hyphens instead",
            m.name
        );
    }
    if m.version.is_empty() {
        anyhow::bail!("template.toml: 'version' field is required and must not be empty");
    }
    if m.description.is_empty() {
        anyhow::bail!("template.toml: 'description' field is required and must not be empty");
    }
    Ok(())
}

// ── Resolution ───────────────────────────────────────────────────

/// Resolve an installed template by name, searching project-local then global.
///
/// Returns the path to the template directory if found (containing template.toml).
pub fn resolve_installed_template(name: &str, project_root: &Path) -> Option<PathBuf> {
    // 1. Project-local: .ta/templates/<name>/
    let local = project_templates_dir(project_root).join(name);
    if local.join("template.toml").exists() {
        return Some(local);
    }
    // 2. Global: ~/.config/ta/templates/<name>/
    let global = global_templates_dir().join(name);
    if global.join("template.toml").exists() {
        return Some(global);
    }
    None
}

// ── SHA-256 helpers ──────────────────────────────────────────────

fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn sha256_dir(dir: &Path) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    let mut paths: Vec<_> = walkdir_sorted(dir)?;
    paths.sort();
    for rel_path in &paths {
        hasher.update(rel_path.as_bytes());
        let abs = dir.join(rel_path);
        if abs.is_file() {
            let data = std::fs::read(&abs)?;
            hasher.update(&data);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn walkdir_sorted(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut result = Vec::new();
    for entry in walkdir(dir)? {
        let path = entry?;
        if path.is_file() {
            if let Ok(rel) = path.strip_prefix(dir) {
                result.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    Ok(result)
}

fn walkdir(dir: &Path) -> anyhow::Result<impl Iterator<Item = anyhow::Result<PathBuf>>> {
    let mut stack = vec![dir.to_path_buf()];
    let mut files = Vec::new();
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)
            .map_err(|e| anyhow::anyhow!("Failed to read directory '{}': {e}", current.display()))?
        {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    Ok(files.into_iter().map(Ok))
}

// ── Install from local path ──────────────────────────────────────

fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn install_from_local(src: &Path, install_dir: &PathBuf) -> anyhow::Result<TemplateManifest> {
    // Validate manifest before installing.
    let manifest = load_manifest(src)?;
    if install_dir.exists() {
        std::fs::remove_dir_all(install_dir).map_err(|e| {
            anyhow::anyhow!(
                "Failed to remove existing template at '{}': {e}",
                install_dir.display()
            )
        })?;
    }
    copy_dir_all(src, install_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to copy template from '{}' to '{}': {e}",
            src.display(),
            install_dir.display()
        )
    })?;
    Ok(manifest)
}

// ── Install from URL / GitHub / registry ─────────────────────────

fn download_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("ta-cli/template-installer")
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {e}"))?;
    let resp = client.get(url).send().map_err(|e| {
        anyhow::anyhow!(
            "Failed to download template from '{url}': {e}\n\
             Check your network connection and the URL."
        )
    })?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "HTTP {status} downloading '{url}'.\n\
             Check the source URL and try again.",
            status = resp.status()
        );
    }
    resp.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| anyhow::anyhow!("Failed to read response body from '{url}': {e}"))
}

fn install_from_remote(url: &str, install_dir: &Path) -> anyhow::Result<()> {
    let bytes = download_bytes(url)?;
    let sha = sha256_bytes(&bytes);

    // Save the archive to a temp file for manual extraction guidance.
    let tmp = std::env::temp_dir().join(format!("ta-template-{}.tar.gz", &sha[..12]));
    std::fs::write(&tmp, &bytes).map_err(|e| {
        anyhow::anyhow!(
            "Failed to write template archive to '{}': {e}",
            tmp.display()
        )
    })?;

    println!(
        "  Downloaded {} bytes (sha256: {}...)",
        bytes.len(),
        &sha[..16]
    );
    println!();
    println!("  The template archive has been saved to:");
    println!("    {}", tmp.display());
    println!();
    println!("  To complete installation, extract it and use the local path form:");
    println!(
        "    tar -xzf {} -C /tmp/my-template --strip-components=1",
        tmp.display()
    );
    println!("    ta template install /tmp/my-template");
    println!();
    println!(
        "  Once extracted, the template will be installed at: {}",
        install_dir.display()
    );

    Ok(())
}

// ── Subcommands ──────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum TemplateCommands {
    /// Install a template from a registry name, github:user/repo, URL, or local path.
    ///
    /// Examples:
    ///   ta template install blender-addon
    ///   ta template install github:myorg/my-template
    ///   ta template install https://example.com/template.tar.gz
    ///   ta template install ./my-local-template
    Install {
        /// Source: registry name, github:user/repo, https URL, or local path.
        source: String,
        /// Install as project-local (.ta/templates/) instead of global (~/.config/ta/templates/).
        #[arg(long)]
        local: bool,
    },
    /// List installed and built-in templates.
    ///
    /// Pass --available to query the community registry for available templates.
    List {
        /// Query the community registry for available templates.
        #[arg(long)]
        available: bool,
    },
    /// Remove an installed template by name.
    Remove {
        /// Template name to remove.
        name: String,
        /// Remove from project-local store (.ta/templates/) instead of global.
        #[arg(long)]
        local: bool,
    },
    /// Publish a template to the community registry.
    ///
    /// Computes a SHA-256 of the template directory and prints the submission
    /// manifest. Actual publishing requires a registry API token.
    Publish {
        /// Path to the template directory (must contain template.toml).
        path: PathBuf,
    },
    /// Search community templates by keyword.
    Search {
        /// Search query (matches name, description, and tags).
        query: String,
    },
}

pub fn execute(
    cmd: &TemplateCommands,
    config: &ta_mcp_gateway::GatewayConfig,
) -> anyhow::Result<()> {
    match cmd {
        TemplateCommands::Install { source, local } => {
            install_template(source, *local, &config.workspace_root)
        }
        TemplateCommands::List { available } => list_templates(*available, &config.workspace_root),
        TemplateCommands::Remove { name, local } => {
            remove_template(name, *local, &config.workspace_root)
        }
        TemplateCommands::Publish { path } => publish_template(path),
        TemplateCommands::Search { query } => search_templates(query),
    }
}

// ── install ──────────────────────────────────────────────────────

fn install_template(source: &str, local: bool, project_root: &Path) -> anyhow::Result<()> {
    // Determine source kind.
    let is_github = source.starts_with("github:");
    let is_url = source.starts_with("https://") || source.starts_with("http://");
    let is_local = source.starts_with('.') || source.starts_with('/') || {
        let p = Path::new(source);
        p.exists() && p.is_dir()
    };

    if is_local {
        let src = Path::new(source);
        if !src.exists() {
            anyhow::bail!(
                "Local template path '{}' does not exist.\n\
                 Provide an existing directory containing a template.toml file.",
                src.display()
            );
        }
        // Load manifest to get the canonical name.
        let manifest = load_manifest(src)?;
        let install_dir = if local {
            project_templates_dir(project_root).join(&manifest.name)
        } else {
            global_templates_dir().join(&manifest.name)
        };
        std::fs::create_dir_all(install_dir.parent().unwrap_or(&install_dir))?;
        let m = install_from_local(src, &install_dir)?;
        println!(
            "Installed template '{}' v{} to {}",
            m.name,
            m.version,
            install_dir.display()
        );
        println!("  Description: {}", m.description);
        if !m.tags.is_empty() {
            println!("  Tags: {}", m.tags.join(", "));
        }
        return Ok(());
    }

    // For remote sources, determine the URL.
    let (url, inferred_name) = if is_github {
        // github:user/repo
        let repo = source.strip_prefix("github:").unwrap_or(source);
        let url = format!("https://github.com/{}/archive/refs/heads/main.tar.gz", repo);
        let name = repo.split('/').next_back().unwrap_or(repo).to_string();
        (url, name)
    } else if is_url {
        let name = source
            .split('/')
            .next_back()
            .unwrap_or("template")
            .trim_end_matches(".tar.gz")
            .trim_end_matches(".zip")
            .to_string();
        (source.to_string(), name)
    } else {
        // Registry name lookup.
        let registry = template_registry_url();
        let url = format!("{}/templates/{}.tar.gz", registry, source);
        (url, source.to_string())
    };

    let install_dir = if local {
        project_templates_dir(project_root).join(&inferred_name)
    } else {
        global_templates_dir().join(&inferred_name)
    };
    std::fs::create_dir_all(install_dir.parent().unwrap_or(&install_dir))?;

    println!("Downloading template from: {}", url);
    install_from_remote(&url, &install_dir)?;

    Ok(())
}

// ── list ─────────────────────────────────────────────────────────

fn list_templates(available: bool, project_root: &Path) -> anyhow::Result<()> {
    if available {
        let registry = template_registry_url();
        let index_url = format!("{}/templates/index.json", registry);
        println!("Fetching available templates from {}...", registry);
        match download_bytes(&index_url) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                // Try to parse as JSON array of objects with name+description.
                if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                    println!("\nAvailable community templates:\n");
                    for item in &items {
                        let name = item["name"].as_str().unwrap_or("?");
                        let desc = item["description"].as_str().unwrap_or("");
                        println!("  {:<20} {}", name, desc);
                    }
                    if items.is_empty() {
                        println!("  (no templates in registry)");
                    }
                } else {
                    println!("{}", text);
                }
            }
            Err(e) => {
                println!(
                    "Could not fetch registry index: {e}\n\
                     Check your network connection or set TA_TEMPLATE_REGISTRY_URL."
                );
            }
        }
        println!();
    }

    // Installed templates.
    let global_dir = global_templates_dir();
    let project_dir = project_templates_dir(project_root);

    // Project-local
    if project_dir.exists() {
        let entries = read_installed_templates(&project_dir);
        if !entries.is_empty() {
            println!("Project-local templates (.ta/templates/):\n");
            for (name, manifest) in &entries {
                let ver = manifest.as_ref().map(|m| m.version.as_str()).unwrap_or("?");
                let desc = manifest
                    .as_ref()
                    .map(|m| m.description.as_str())
                    .unwrap_or("(no manifest)");
                println!("  {:<20} v{}  {}", name, ver, desc);
            }
            println!();
        }
    }

    // Global
    if global_dir.exists() {
        let entries = read_installed_templates(&global_dir);
        if !entries.is_empty() {
            println!("Globally installed templates (~/.config/ta/templates/):\n");
            for (name, manifest) in &entries {
                let ver = manifest.as_ref().map(|m| m.version.as_str()).unwrap_or("?");
                let desc = manifest
                    .as_ref()
                    .map(|m| m.description.as_str())
                    .unwrap_or("(no manifest)");
                println!("  {:<20} v{}  {}", name, ver, desc);
            }
            println!();
        }
    }

    // Built-ins
    println!("Built-in templates (always available):\n");
    for (name, desc) in BUILTIN_TEMPLATES {
        println!("  {:<20} {}", name, desc);
    }
    println!();

    println!("Use `ta template install <name>` to install from the registry.");
    println!("Use `ta new run --template <name>` to create a project from a template.");

    Ok(())
}

fn read_installed_templates(dir: &Path) -> Vec<(String, Option<TemplateManifest>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let manifest = load_manifest(&path).ok();
            result.push((name, manifest));
        }
    }
    result.sort_by_key(|r| r.0.clone());
    result
}

// ── remove ───────────────────────────────────────────────────────

fn remove_template(name: &str, local: bool, project_root: &Path) -> anyhow::Result<()> {
    let dir = if local {
        project_templates_dir(project_root).join(name)
    } else {
        global_templates_dir().join(name)
    };

    if !dir.exists() {
        let other = if local {
            global_templates_dir().join(name)
        } else {
            project_templates_dir(project_root).join(name)
        };
        if other.exists() {
            let hint = if local {
                "without --local"
            } else {
                "with --local"
            };
            anyhow::bail!(
                "Template '{}' not found at '{}' but exists in the other store.\n\
                 Try: ta template remove {} {}",
                name,
                dir.display(),
                name,
                hint
            );
        }
        anyhow::bail!(
            "Template '{}' is not installed.\n\
             Run `ta template list` to see installed templates.",
            name
        );
    }

    std::fs::remove_dir_all(&dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to remove template '{}' at '{}': {e}",
            name,
            dir.display()
        )
    })?;
    println!("Removed template '{}' from {}", name, dir.display());
    Ok(())
}

// ── publish ──────────────────────────────────────────────────────

fn publish_template(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!("Template directory '{}' does not exist.", path.display());
    }
    let manifest = load_manifest(path)?;
    let sha = sha256_dir(path)?;

    println!("Template: {} v{}", manifest.name, manifest.version);
    println!("Description: {}", manifest.description);
    if !manifest.tags.is_empty() {
        println!("Tags: {}", manifest.tags.join(", "));
    }
    if !manifest.author.is_empty() {
        println!("Author: {}", manifest.author);
    }
    println!("SHA-256: {}", sha);
    println!();

    let submission = serde_json::json!({
        "name": manifest.name,
        "version": manifest.version,
        "description": manifest.description,
        "tags": manifest.tags,
        "author": manifest.author,
        "ta_version_min": manifest.ta_version_min,
        "sha256": sha,
        "path": path.display().to_string(),
    });
    println!("Submission manifest:");
    println!("{}", serde_json::to_string_pretty(&submission)?);
    println!();
    println!("To publish to the community registry:");
    println!(
        "  Set TA_TEMPLATE_REGISTRY_TOKEN and POST the manifest to {}/templates",
        template_registry_url()
    );
    println!();
    println!("Next: create a GitHub release or submit a PR to the community registry repository.");

    Ok(())
}

// ── search ───────────────────────────────────────────────────────

fn search_templates(query: &str) -> anyhow::Result<()> {
    let registry = template_registry_url();
    let search_url = format!("{}/templates/search?q={}", registry, urlencoded(query));
    println!("Searching for '{}' in community registry...", query);
    match download_bytes(&search_url) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
                if items.is_empty() {
                    println!("No templates found matching '{}'.", query);
                    println!("Try a broader search term or browse at {}", registry);
                } else {
                    println!("\nResults for '{}':\n", query);
                    for item in &items {
                        let name = item["name"].as_str().unwrap_or("?");
                        let desc = item["description"].as_str().unwrap_or("");
                        let tags = item["tags"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        println!("  {:<20} {}", name, desc);
                        if !tags.is_empty() {
                            println!("  {:<20} tags: {}", "", tags);
                        }
                    }
                }
            } else {
                println!("{}", text);
            }
        }
        Err(e) => {
            println!(
                "Could not reach registry: {e}\n\
                 Check your network connection or set TA_TEMPLATE_REGISTRY_URL."
            );
        }
    }
    Ok(())
}

fn urlencoded(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            c => format!("%{:02X}", c as u8),
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_template_dir(dir: &Path, name: &str, version: &str, description: &str) {
        fs::create_dir_all(dir).unwrap();
        let manifest = format!(
            r#"name = "{name}"
version = "{version}"
description = "{description}"
tags = ["test"]
author = "Test Author"
ta_version_min = "0.14.8-alpha"
"#
        );
        fs::write(dir.join("template.toml"), &manifest).unwrap();
        fs::write(dir.join("workflow.toml"), "[agent]\n").unwrap();
    }

    #[test]
    fn test_template_install_from_local_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("my-tpl");
        make_template_dir(&src, "my-tpl", "1.0.0", "Test template");

        let install_root = tmp.path().join("global");
        fs::create_dir_all(&install_root).unwrap();
        let install_dir = install_root.join("my-tpl");
        let manifest = install_from_local(&src, &install_dir).unwrap();
        assert_eq!(manifest.name, "my-tpl");
        assert!(install_dir.join("template.toml").exists());
        assert!(install_dir.join("workflow.toml").exists());
    }

    #[test]
    fn test_template_validates_manifest_fields() {
        let tmp = tempfile::tempdir().unwrap();
        // Missing description → should fail validation.
        let bad = tmp.path().join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(
            bad.join("template.toml"),
            "name = \"bad\"\nversion = \"1.0\"\ndescription = \"\"\n",
        )
        .unwrap();
        let result = load_manifest(&bad);
        assert!(result.is_err(), "should fail: empty description");
        assert!(
            result.unwrap_err().to_string().contains("description"),
            "error should mention 'description'"
        );
    }

    #[test]
    fn test_template_list_includes_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let tpl_dir = tmp.path().join("installed-tpl");
        make_template_dir(&tpl_dir, "installed-tpl", "2.0.0", "Installed template");

        let entries = read_installed_templates(tmp.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "installed-tpl");
        let m = entries[0].1.as_ref().unwrap();
        assert_eq!(m.version, "2.0.0");
    }

    #[test]
    fn test_new_resolves_installed_before_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        // Install a template that shadows a built-in name.
        let tpl_dir = tmp.path().join(".ta").join("templates").join("rust-cli");
        make_template_dir(&tpl_dir, "rust-cli", "99.0.0", "Custom rust-cli override");

        let resolved = resolve_installed_template("rust-cli", tmp.path());
        assert!(
            resolved.is_some(),
            "should resolve installed template before falling through to built-in"
        );
        assert_eq!(
            resolved.unwrap(),
            tmp.path().join(".ta").join("templates").join("rust-cli")
        );
    }

    #[test]
    fn test_template_publish_computes_sha256() {
        let tmp = tempfile::tempdir().unwrap();
        let tpl_dir = tmp.path().join("my-pkg");
        make_template_dir(&tpl_dir, "my-pkg", "1.0.0", "Publish test");

        let sha = sha256_dir(&tpl_dir).unwrap();
        assert!(!sha.is_empty());
        assert_eq!(sha.len(), 64, "SHA-256 hex should be 64 chars");

        // Recompute — must be stable.
        let sha2 = sha256_dir(&tpl_dir).unwrap();
        assert_eq!(sha, sha2, "SHA-256 must be deterministic");
    }

    #[test]
    fn test_builtin_template_list_has_expected_names() {
        let names: Vec<&str> = BUILTIN_TEMPLATES.iter().map(|(n, _)| *n).collect();
        assert!(
            names.contains(&"blender-addon"),
            "blender-addon must be built-in"
        );
        assert!(names.contains(&"rust-cli"), "rust-cli must be built-in");
        assert!(
            names.contains(&"python-script"),
            "python-script must be built-in"
        );
    }
}
