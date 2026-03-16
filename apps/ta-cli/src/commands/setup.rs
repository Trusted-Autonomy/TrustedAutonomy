// setup.rs — Agent-guided setup command (v0.7.2, extended v0.11.4).
//
// `ta setup` launches a conversational flow where a TA agent helps configure
// workflows. The resulting config is a TA draft the user reviews.
//
// - `ta setup wizard` — full interactive setup, generates .ta/ config files
// - `ta setup refine <topic>` — refine a specific aspect of config
// - `ta setup show` — display current resolved configuration
// - `ta setup resolve` — resolve plugins from project.toml (v0.11.4)

use std::path::Path;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand, Debug)]
pub enum SetupCommands {
    /// Run the interactive setup wizard to generate .ta/ configuration.
    Wizard {
        /// Which config sections to generate: all, workflow, memory, policy, agents.
        #[arg(long, default_value = "all")]
        template: String,
    },
    /// Refine a specific configuration topic (e.g., "policy", "memory", "agents").
    Refine {
        /// Configuration topic to refine.
        topic: String,
    },
    /// Show the current resolved configuration for this project.
    Show {
        /// Which section to show: all, workflow, memory, policy, channels, plugins.
        #[arg(long, default_value = "all")]
        section: String,
    },
    /// Resolve and install plugins declared in .ta/project.toml.
    ///
    /// Reads the project manifest, checks which plugins are installed,
    /// downloads or builds missing ones, verifies integrity, and reports
    /// environment variable requirements.
    Resolve {
        /// CI mode: non-interactive, fail hard on missing plugins or env vars.
        #[arg(long)]
        ci: bool,
    },
}

pub fn execute(command: &SetupCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        SetupCommands::Wizard { template } => run_wizard(config, template),
        SetupCommands::Refine { topic } => run_refine(config, topic),
        SetupCommands::Show { section } => show_config(config, section),
        SetupCommands::Resolve { ci } => resolve_plugins(config, *ci),
    }
}

/// Run the interactive setup wizard.
///
/// Detects project type, generates sensible defaults, and writes them
/// as proposed config files. In full TA mode, this would launch an agent
/// goal that produces a draft. For now, generates config directly.
fn run_wizard(config: &GatewayConfig, template: &str) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let ta_dir = project_root.join(".ta");

    // Detect project type.
    let schema = ta_memory::key_schema::KeySchema::resolve(project_root);
    println!("Detected project type: {}", schema.project_type);
    println!("Backend: {}", schema.backend);
    println!();

    let sections: Vec<&str> = if template == "all" {
        vec!["workflow", "memory", "policy", "agents"]
    } else {
        vec![template]
    };

    std::fs::create_dir_all(&ta_dir)?;

    for section in &sections {
        match *section {
            "workflow" => generate_workflow_config(&ta_dir, &schema)?,
            "memory" => generate_memory_config(&ta_dir, &schema)?,
            "policy" => generate_policy_config(&ta_dir)?,
            "agents" => generate_agent_config(&ta_dir)?,
            other => {
                eprintln!("Unknown template section: {}", other);
            }
        }
    }

    println!();
    println!("Setup complete. Review the generated files in .ta/");
    println!("Run `ta setup show` to inspect the resolved configuration.");

    Ok(())
}

/// Refine a specific configuration topic.
fn run_refine(config: &GatewayConfig, topic: &str) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;
    let schema = ta_memory::key_schema::KeySchema::resolve(&config.workspace_root);

    match topic {
        "workflow" => {
            println!("Refining workflow configuration...");
            generate_workflow_config(&ta_dir, &schema)?;
        }
        "memory" => {
            println!("Refining memory configuration...");
            generate_memory_config(&ta_dir, &schema)?;
        }
        "policy" => {
            println!("Refining policy configuration...");
            generate_policy_config(&ta_dir)?;
        }
        "agents" => {
            println!("Refining agent configuration...");
            generate_agent_config(&ta_dir)?;
        }
        "channels" => {
            println!("Refining channel configuration...");
            generate_channel_config(&ta_dir)?;
        }
        _ => {
            anyhow::bail!(
                "Unknown topic: '{}'. Available: workflow, memory, policy, agents, channels",
                topic
            );
        }
    }

    Ok(())
}

/// Show resolved configuration.
fn show_config(config: &GatewayConfig, section: &str) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let ta_dir = project_root.join(".ta");

    let sections: Vec<&str> = if section == "all" {
        vec![
            "project", "workflow", "memory", "policy", "channels", "plugins",
        ]
    } else {
        vec![section]
    };

    for s in &sections {
        match *s {
            "project" => {
                let schema = ta_memory::key_schema::KeySchema::resolve(project_root);
                println!("=== Project ===");
                println!("  Type: {}", schema.project_type);
                println!("  Backend: {}", schema.backend);
                println!("  Key domains:");
                println!("    module_map: {}", schema.domains.module_map);
                println!("    module: {}", schema.domains.module);
                println!("    type_system: {}", schema.domains.type_system);
                println!("    build_tool: {}", schema.domains.build_tool);
                println!();
            }
            "workflow" => {
                show_file_if_exists(&ta_dir.join("workflow.toml"), "Workflow")?;
            }
            "memory" => {
                show_file_if_exists(&ta_dir.join("memory.toml"), "Memory")?;
            }
            "policy" => {
                show_file_if_exists(&ta_dir.join("policy.yaml"), "Policy")?;
            }
            "channels" => {
                show_file_if_exists(&ta_dir.join("config.yaml"), "Channels")?;
            }
            "plugins" => {
                show_plugins(project_root)?;
            }
            other => {
                eprintln!("Unknown section: {}", other);
            }
        }
    }

    Ok(())
}

fn show_file_if_exists(path: &Path, label: &str) -> anyhow::Result<()> {
    println!("=== {} ===", label);
    if path.exists() {
        let content = std::fs::read_to_string(path)?;
        println!("{}", content);
    } else {
        println!("  (not configured — run `ta setup wizard` to generate)");
    }
    println!();
    Ok(())
}

fn generate_workflow_config(
    ta_dir: &Path,
    _schema: &ta_memory::key_schema::KeySchema,
) -> anyhow::Result<()> {
    let path = ta_dir.join("workflow.toml");
    if path.exists() {
        println!(
            "  workflow.toml already exists — skipping (use `ta setup refine workflow` to update)"
        );
        return Ok(());
    }

    let content = r#"# TA Workflow Configuration
# Generated by `ta setup wizard`

[memory.auto_capture]
on_goal_complete = true
on_draft_reject = true
on_human_guidance = true
on_repeated_correction = true
correction_threshold = 3
max_context_entries = 10
"#;
    std::fs::write(&path, content)?;
    println!("  Created .ta/workflow.toml");
    Ok(())
}

fn generate_memory_config(
    ta_dir: &Path,
    schema: &ta_memory::key_schema::KeySchema,
) -> anyhow::Result<()> {
    let path = ta_dir.join("memory.toml");
    if path.exists() {
        println!("  memory.toml already exists — skipping");
        return Ok(());
    }

    let content = format!(
        r#"# TA Memory Configuration
# Generated by `ta setup wizard`
# Project type: {}

backend = "{}"

[project]
type = "{}"

[key_domains]
module_map = "{}"
module = "{}"
type_system = "{}"
build_tool = "{}"
"#,
        schema.project_type,
        schema.backend,
        schema.project_type,
        schema.domains.module_map,
        schema.domains.module,
        schema.domains.type_system,
        schema.domains.build_tool,
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/memory.toml");
    Ok(())
}

fn generate_policy_config(ta_dir: &Path) -> anyhow::Result<()> {
    let path = ta_dir.join("policy.yaml");
    if path.exists() {
        println!("  policy.yaml already exists — skipping");
        return Ok(());
    }

    let content = r#"# TA Policy Configuration
# Generated by `ta setup wizard`
# See docs/USAGE.md for full policy reference.

security_level: checkpoint

defaults:
  enforcement: error
  auto_approve:
    verbs:
      - read
      - list
      - search

schemes:
  fs:
    auto_approve_verbs:
      - read
      - list

escalation:
  drift_threshold: 0.7
  action_limit: 100
"#;
    std::fs::write(&path, content)?;
    println!("  Created .ta/policy.yaml");
    Ok(())
}

fn generate_agent_config(ta_dir: &Path) -> anyhow::Result<()> {
    let agents_dir = ta_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    // Use framework registry to detect installed frameworks.
    let project_root = ta_dir.parent();
    let registry = crate::framework_registry::FrameworkRegistry::load(project_root);
    let installed = registry.detect_installed();

    println!("  Framework detection:");
    if installed.is_empty() {
        println!("    No frameworks detected on PATH.");
        println!("    Generating default claude-code config.");

        let path = agents_dir.join("claude-code.yaml");
        if !path.exists() {
            let content = r#"# Agent Configuration: Claude Code
# Generated by `ta setup wizard`

name: claude-code
command: claude
args_template:
  - "{prompt}"
injects_context_file: true
injects_settings: true
"#;
            std::fs::write(&path, content)?;
            println!("    Created .ta/agents/claude-code.yaml");
        } else {
            println!("    agents/claude-code.yaml already exists — skipping");
        }
    } else {
        for (id, entry) in &installed {
            let path = agents_dir.join(format!("{}.yaml", id));
            if path.exists() {
                println!(
                    "    [installed] {} — agents/{}.yaml already exists",
                    entry.name, id
                );
                continue;
            }
            let content = format!(
                "# Agent Configuration: {}\n# Generated by `ta setup wizard`\n# Detected on PATH\n\nname: {}\ncommand: {}\nargs_template:\n  - \"{{prompt}}\"\ninjects_context_file: {}\ninjects_settings: {}\n",
                entry.name,
                id,
                entry.detect.first().unwrap_or(&id.to_string()),
                *id == "claude-code" || *id == "bmad",
                *id == "claude-code" || *id == "bmad",
            );
            std::fs::write(&path, content)?;
            println!(
                "    [installed] {} — created agents/{}.yaml",
                entry.name, id
            );
        }
    }

    // Show available-but-not-installed frameworks.
    let available: Vec<_> = registry
        .detect_available()
        .into_iter()
        .filter(|(_, e)| !e.detect.is_empty())
        .collect();
    if !available.is_empty() {
        println!();
        println!("  Available frameworks (not yet installed):");
        for (id, entry) in &available {
            let install = entry
                .install
                .as_ref()
                .map(|i| i.for_current_platform().to_string())
                .unwrap_or_else(|| "See homepage".to_string());
            println!("    {} — install: {}", id, install);
            if let Some(ref hp) = entry.homepage {
                println!("      {}", hp);
            }
        }
        println!("    Run `ta setup refine agents` after installing to generate configs.");
    }

    Ok(())
}

fn generate_channel_config(ta_dir: &Path) -> anyhow::Result<()> {
    let path = ta_dir.join("config.yaml");
    if path.exists() {
        println!("  config.yaml already exists — skipping");
        return Ok(());
    }

    let content = r#"# TA Channel Configuration
# Generated by `ta setup refine channels`

channels:
  review:
    type: terminal
  session:
    type: terminal
  notify: []
"#;
    std::fs::write(&path, content)?;
    println!("  Created .ta/config.yaml");
    Ok(())
}

/// Show plugin status: manifest requirements vs installed plugins.
fn show_plugins(project_root: &Path) -> anyhow::Result<()> {
    use ta_changeset::project_manifest::ProjectManifest;
    use ta_changeset::registry_client::detect_platform;

    println!("=== Plugins ===");
    println!("  Platform: {}", detect_platform());

    if !ProjectManifest::exists(project_root) {
        println!("  (no .ta/project.toml — run `ta setup resolve` after creating one)");
        println!();
        return Ok(());
    }

    match ProjectManifest::load(project_root) {
        Ok(manifest) => {
            println!("  Project: {}", manifest.project.name);
            if manifest.plugins.is_empty() {
                println!("  No plugins declared.");
            } else {
                let installed = ta_changeset::plugin::discover_plugins(project_root);
                println!("  Declared plugins:");
                for (name, req) in &manifest.plugins {
                    let status = match installed.iter().find(|p| p.manifest.name == *name) {
                        Some(p) => {
                            if ta_changeset::project_manifest::version_satisfies(
                                &p.manifest.version,
                                &req.version,
                            ) {
                                format!("installed (v{})", p.manifest.version)
                            } else {
                                format!("outdated (v{}, needs {})", p.manifest.version, req.version)
                            }
                        }
                        None => "missing".to_string(),
                    };
                    let req_label = if req.required { "" } else { " (optional)" };
                    println!(
                        "    {} [{}] {} — {}{}",
                        name, req.plugin_type, req.version, status, req_label
                    );
                }
            }
        }
        Err(e) => {
            println!("  Error loading project.toml: {}", e);
        }
    }

    println!();
    Ok(())
}

/// Resolve and install plugins from .ta/project.toml.
fn resolve_plugins(config: &GatewayConfig, ci_mode: bool) -> anyhow::Result<()> {
    use ta_changeset::plugin_resolver::{resolve_all, PluginResolveResult};
    use ta_changeset::project_manifest::ProjectManifest;
    use ta_changeset::registry_client::detect_platform;

    let project_root = &config.workspace_root;

    println!("Platform: {}", detect_platform());

    let manifest = match ProjectManifest::load(project_root) {
        Ok(m) => m,
        Err(ta_changeset::project_manifest::ManifestError::NotFound { path }) => {
            if ci_mode {
                anyhow::bail!(
                    "No project manifest found at {}. \
                     Create .ta/project.toml to declare plugin requirements.",
                    path.display()
                );
            }
            println!("No .ta/project.toml found.");
            println!();
            println!("Create one to declare plugin requirements:");
            println!();
            println!("  [project]");
            println!("  name = \"my-project\"");
            println!();
            println!("  [plugins.discord]");
            println!("  type = \"channel\"");
            println!("  version = \">=0.1.0\"");
            println!("  source = \"registry:ta-channel-discord\"");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    println!("Project: {}", manifest.project.name);
    if manifest.plugins.is_empty() {
        println!("No plugins declared in project.toml.");
        return Ok(());
    }

    println!("Resolving {} plugin(s)...", manifest.plugins.len());
    println!();

    let report = resolve_all(&manifest, project_root, ci_mode);

    // Print results.
    for result in &report.results {
        match result {
            PluginResolveResult::AlreadyInstalled {
                name,
                installed_version,
            } => {
                println!(
                    "  [ok]      {} v{} — already installed",
                    name, installed_version
                );
            }
            PluginResolveResult::Installed {
                name,
                version,
                source,
            } => {
                println!("  [install] {} v{} — from {}", name, version, source);
            }
            PluginResolveResult::BuiltFromSource { name, source_path } => {
                println!(
                    "  [build]   {} — built from {}",
                    name,
                    source_path.display()
                );
            }
            PluginResolveResult::Failed { name, reason } => {
                println!("  [FAIL]    {} — {}", name, reason);
            }
            PluginResolveResult::Skipped { name, reason } => {
                println!("  [skip]    {} — {} (optional)", name, reason);
            }
        }
    }

    // Print environment variable warnings.
    if !report.missing_env_vars.is_empty() {
        println!();
        println!("Missing environment variables:");
        for (plugin, vars) in &report.missing_env_vars {
            for var in vars {
                println!("  {} needs ${}", plugin, var);
            }
        }
        if ci_mode {
            anyhow::bail!(
                "Missing required environment variables. Set them and re-run `ta setup resolve --ci`."
            );
        } else {
            println!();
            println!("Set these variables before starting the daemon.");
            println!("Plugins may still work partially without them.");
        }
    }

    // Summary.
    println!();
    println!(
        "Resolved: {} ok, {} failed, {} skipped",
        report.success_count(),
        report.failure_count(),
        report
            .results
            .iter()
            .filter(|r| matches!(r, PluginResolveResult::Skipped { .. }))
            .count()
    );

    if !report.all_ok() {
        if ci_mode {
            anyhow::bail!(
                "Plugin resolution failed. {} plugin(s) could not be installed.",
                report.failure_count()
            );
        } else {
            println!();
            println!("Some plugins failed to install. Check the errors above.");
            println!("You can re-run `ta setup resolve` after fixing the issues.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> GatewayConfig {
        GatewayConfig::for_project(dir.path())
    }

    #[test]
    fn wizard_generates_all_configs() {
        let dir = TempDir::new().unwrap();
        // Create Cargo.toml to detect Rust project.
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let config = test_config(&dir);
        run_wizard(&config, "all").unwrap();

        let ta_dir = dir.path().join(".ta");
        assert!(ta_dir.join("workflow.toml").exists());
        assert!(ta_dir.join("memory.toml").exists());
        assert!(ta_dir.join("policy.yaml").exists());
        assert!(ta_dir.join("agents").join("claude-code.yaml").exists());

        // Verify memory.toml has Rust-specific content.
        let memory_content = std::fs::read_to_string(ta_dir.join("memory.toml")).unwrap();
        assert!(memory_content.contains("rust-workspace"));
        assert!(memory_content.contains("crate-map"));
    }

    #[test]
    fn wizard_skips_existing_files() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("workflow.toml"), "# existing\n").unwrap();

        let config = test_config(&dir);
        run_wizard(&config, "workflow").unwrap();

        // Should not overwrite.
        let content = std::fs::read_to_string(ta_dir.join("workflow.toml")).unwrap();
        assert_eq!(content, "# existing\n");
    }

    #[test]
    fn show_config_no_files() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // Should not panic when no .ta/ files exist.
        show_config(&config, "all").unwrap();
    }

    #[test]
    fn refine_single_section() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        run_refine(&config, "policy").unwrap();
        assert!(dir.path().join(".ta").join("policy.yaml").exists());
    }

    #[test]
    fn refine_unknown_topic_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = run_refine(&config, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_no_manifest_interactive() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // Should not error in interactive mode when no manifest exists.
        let result = resolve_plugins(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_no_manifest_ci_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // CI mode should error when no manifest exists.
        let result = resolve_plugins(&config, true);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_empty_manifest() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("project.toml"), "[project]\nname = \"test\"\n").unwrap();

        let config = test_config(&dir);
        let result = resolve_plugins(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_with_already_installed_plugin() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();

        // Create project.toml.
        std::fs::write(
            ta_dir.join("project.toml"),
            r#"[project]
name = "test"

[plugins.test-plug]
type = "channel"
version = ">=0.1.0"
source = "path:./nonexistent"
"#,
        )
        .unwrap();

        // Install the plugin manually.
        let plugin_dir = ta_dir.join("plugins").join("channels").join("test-plug");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("channel.toml"),
            r#"
name = "test-plug"
version = "0.2.0"
command = "test"
protocol = "json-stdio"
"#,
        )
        .unwrap();

        let config = test_config(&dir);
        let result = resolve_plugins(&config, false);
        assert!(result.is_ok());
    }

    #[test]
    fn show_plugins_no_manifest() {
        let dir = TempDir::new().unwrap();
        // Should not panic.
        show_plugins(dir.path()).unwrap();
    }

    #[test]
    fn show_plugins_with_manifest() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(
            ta_dir.join("project.toml"),
            r#"[project]
name = "test"

[plugins.slack]
type = "channel"
version = ">=0.1.0"
source = "registry:ta-channel-slack"
"#,
        )
        .unwrap();

        show_plugins(dir.path()).unwrap();
    }
}
