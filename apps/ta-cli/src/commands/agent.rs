// agent.rs — CLI commands for agent config authoring (v0.10.5).
//
// Commands:
//   ta agent new <name>             — scaffold a new agent config
//   ta agent validate <path>        — validate an agent config YAML
//   ta agent list [--templates|--source external]  — list agents
//   ta agent add <name> --from <source>  — install from external source
//   ta agent remove <name>          — remove an external agent config

use std::path::PathBuf;

use clap::Subcommand;
use ta_changeset::sources::{ExternalSource, Lockfile, SourceCache};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum AgentCommands {
    /// Scaffold a new agent configuration YAML file.
    New {
        /// Agent name (used as the file name).
        name: String,
        /// Agent type: developer, auditor, orchestrator, planner.
        #[arg(long, default_value = "developer")]
        r#type: String,
    },
    /// Validate an agent configuration YAML file.
    Validate {
        /// Path to the agent config YAML file.
        path: PathBuf,
    },
    /// List configured agents or browse templates.
    List {
        /// Show available agent templates instead of configured agents.
        #[arg(long)]
        templates: bool,
        /// Show only externally-sourced agents.
        #[arg(long)]
        source: Option<String>,
    },
    /// Install an agent config from an external source (registry, GitHub, URL).
    Add {
        /// Agent name to install as.
        name: String,
        /// Source to fetch from: registry:org/name, gh:org/repo, or https://...
        #[arg(long)]
        from: String,
    },
    /// Remove an externally-installed agent config.
    Remove {
        /// Agent name to remove.
        name: String,
    },
}

pub fn execute(command: &AgentCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        AgentCommands::New { name, r#type } => new_agent(name, r#type, config),
        AgentCommands::Validate { path } => validate_agent(path),
        AgentCommands::List { templates, source } => {
            if *templates {
                list_templates()
            } else if source.as_deref() == Some("external") {
                list_external_agents(config)
            } else {
                list_agents(config)
            }
        }
        AgentCommands::Add { name, from } => add_agent(name, from, config),
        AgentCommands::Remove { name } => remove_agent(name, config),
    }
}

fn new_agent(name: &str, agent_type: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    let agents_dir = config.workspace_root.join(".ta").join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    let file_path = agents_dir.join(format!("{}.yaml", name));
    if file_path.exists() {
        anyhow::bail!(
            "Agent config already exists: {}\n\
             Edit the existing file or choose a different name.",
            file_path.display()
        );
    }

    let content = match agent_type {
        "developer" => generate_developer_config(name),
        "auditor" => generate_auditor_config(name),
        "orchestrator" => generate_orchestrator_config(name),
        "planner" => generate_planner_config(name),
        _ => {
            anyhow::bail!(
                "Unknown agent type: '{}'\n\
                 Available types: developer, auditor, orchestrator, planner",
                agent_type
            );
        }
    };

    std::fs::write(&file_path, &content)?;

    println!("Created agent config: {}", file_path.display());
    println!("  Type: {}", agent_type);
    println!();
    println!("Next steps:");
    println!("  1. Edit the config to customize for your project");
    println!("  2. Validate: ta agent validate {}", file_path.display());
    println!("  3. Use in a workflow role: agent: {}", name);

    Ok(())
}

fn validate_agent(path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "File not found: {}\n\
             Provide a path to an agent config YAML file.",
            path.display()
        );
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;

    let result = ta_workflow::validate::validate_agent_config(&content);

    if result.findings.is_empty() {
        println!("Agent config is valid: {}", path.display());

        // Show a summary of what was parsed.
        if let Ok(doc) =
            serde_yaml::from_str::<std::collections::HashMap<String, serde_yaml::Value>>(&content)
        {
            if let Some(serde_yaml::Value::String(name)) = doc.get("name") {
                println!("  Name: {}", name);
            }
            if let Some(serde_yaml::Value::String(cmd)) = doc.get("command") {
                // Check if command exists on PATH.
                let on_path = std::process::Command::new("which")
                    .arg(cmd)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                println!(
                    "  Command: {} {}",
                    cmd,
                    if on_path {
                        "(found on PATH)"
                    } else {
                        "(not found on PATH)"
                    }
                );
            }
        }
        return Ok(());
    }

    println!("Validation results for {}:", path.display());
    println!();

    for finding in &result.findings {
        let icon = match finding.severity {
            ta_workflow::validate::ValidationSeverity::Error => "ERROR",
            ta_workflow::validate::ValidationSeverity::Warning => "WARN ",
        };
        println!("  [{}] {}: {}", icon, finding.location, finding.message);
        if let Some(suggestion) = &finding.suggestion {
            println!("         -> {}", suggestion);
        }
    }

    println!();
    println!(
        "  {} error(s), {} warning(s)",
        result.error_count(),
        result.warning_count()
    );

    Ok(())
}

fn list_agents(config: &GatewayConfig) -> anyhow::Result<()> {
    let agents_dir = config.workspace_root.join(".ta").join("agents");

    println!("Configured agents:");
    if agents_dir.exists() {
        let mut found = false;
        let mut entries: Vec<_> = std::fs::read_dir(&agents_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "yaml" || ext == "yml")
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            found = true;
            let name = entry
                .path()
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            // Try to read the command field.
            let cmd = std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|c| {
                    serde_yaml::from_str::<std::collections::HashMap<String, serde_yaml::Value>>(&c)
                        .ok()
                })
                .and_then(|doc| {
                    doc.get("command")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                })
                .unwrap_or_else(|| "?".to_string());

            println!("  {} (command: {})", name, cmd);
        }

        if !found {
            println!("  (none configured)");
        }
    } else {
        println!("  (none configured)");
    }

    println!();
    println!("Scaffold a new agent:");
    println!("  ta agent new my-agent --type developer");
    println!();
    println!("Browse templates:");
    println!("  ta agent list --templates");

    Ok(())
}

fn list_templates() -> anyhow::Result<()> {
    println!("Agent templates:");
    println!();
    println!("  developer     Full read/write developer agent with test permissions");
    println!("  auditor       Read-only auditor agent for security/code review");
    println!("  orchestrator  Multi-agent orchestrator with elevated permissions");
    println!("  planner       Technical planner focused on decomposition and design");
    println!();
    println!("Create from template:");
    println!("  ta agent new my-agent --type developer");
    println!("  ta agent new security-bot --type auditor");
    println!();
    println!("Template files: templates/agents/");

    Ok(())
}

fn generate_developer_config(name: &str) -> String {
    format!(
        r#"# Agent Configuration: {name}
# Type: developer — Full read/write access for building features and fixes.
#
# Validate: ta agent validate .ta/agents/{name}.yaml

name: {name}
command: claude
args_template:
  - "{{prompt}}"

# Context injection settings.
injects_context_file: true
injects_settings: true

# Alignment profile — controls what this agent is allowed to do.
# alignment:
#   security_level: checkpoint
#   allowed_actions:
#     - read
#     - write
#     - execute
#   forbidden_patterns:
#     - "rm -rf /"
#     - "DROP TABLE"
"#,
        name = name
    )
}

fn generate_auditor_config(name: &str) -> String {
    format!(
        r#"# Agent Configuration: {name}
# Type: auditor — Read-only access for security and code review.
#
# Validate: ta agent validate .ta/agents/{name}.yaml

name: {name}
command: claude
args_template:
  - "{{prompt}}"

# Context injection settings.
injects_context_file: true
injects_settings: false

# Alignment profile — auditors are read-only by default.
alignment:
  security_level: supervised
  allowed_actions:
    - read
    - list
    - search
  forbidden_patterns:
    - "write"
    - "delete"
    - "execute"
"#,
        name = name
    )
}

fn generate_orchestrator_config(name: &str) -> String {
    format!(
        r#"# Agent Configuration: {name}
# Type: orchestrator — Coordinates multiple agents and workflows.
#
# Validate: ta agent validate .ta/agents/{name}.yaml

name: {name}
command: claude
args_template:
  - "{{prompt}}"

# Context injection settings.
injects_context_file: true
injects_settings: true

# Alignment profile — orchestrators can read, plan, and delegate.
alignment:
  security_level: checkpoint
  allowed_actions:
    - read
    - list
    - search
    - plan
    - delegate
"#,
        name = name
    )
}

// ── External source commands (v0.10.5) ──────────────────────────────

/// Install an agent config from an external source.
fn add_agent(name: &str, from: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    let source = ExternalSource::parse(from).map_err(|e| {
        anyhow::anyhow!(
            "Invalid source '{}': {}\n\
             Expected formats:\n  \
             registry:org/name\n  \
             gh:org/repo\n  \
             https://example.com/agent.yaml",
            from,
            e
        )
    })?;

    let agents_dir = config.workspace_root.join(".ta").join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    let target_path = agents_dir.join(format!("{}.yaml", name));
    if target_path.exists() {
        anyhow::bail!(
            "Agent config '{}' already exists at {}.\n\
             Remove it first: ta agent remove {}",
            name,
            target_path.display(),
            name
        );
    }

    println!("Fetching agent config '{}' from {} ...", name, from);

    let url = source.fetch_url();
    let content = fetch_agent_content(&url)?;

    // Basic validation: must be valid YAML.
    let result = ta_workflow::validate::validate_agent_config(&content);
    if result.has_errors() {
        println!("Warning: fetched agent config has validation issues:");
        for finding in &result.findings {
            if matches!(
                finding.severity,
                ta_workflow::validate::ValidationSeverity::Error
            ) {
                println!("  [ERROR] {}: {}", finding.location, finding.message);
            }
        }
        println!();
    }

    std::fs::write(&target_path, &content)?;

    // Compute checksum and record in lockfile.
    let checksum = compute_agent_checksum(&content);
    let lock_path = config.workspace_root.join(".ta").join("agents.lock");
    let mut lockfile = Lockfile::load(&lock_path).unwrap_or_default();
    lockfile.add(ta_changeset::sources::LockEntry {
        name: name.to_string(),
        version: "latest".to_string(),
        source: from.to_string(),
        checksum,
    });
    lockfile.save(&lock_path)?;

    // Cache for offline use.
    {
        let cache = SourceCache::new("agents");
        let _ = cache.store(name, &content, &source, "latest");
    }

    println!("Installed agent config: {}", target_path.display());
    println!("  Source: {}", from);
    println!();
    println!("Next steps:");
    println!("  Validate: ta agent validate {}", target_path.display());
    println!("  Use in a workflow role: agent: {}", name);

    Ok(())
}

/// Remove an externally-installed agent config.
fn remove_agent(name: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    let agents_dir = config.workspace_root.join(".ta").join("agents");
    let target_path = agents_dir.join(format!("{}.yaml", name));

    if !target_path.exists() {
        anyhow::bail!(
            "Agent config '{}' not found at {}.\n\
             List agents with: ta agent list",
            name,
            target_path.display()
        );
    }

    std::fs::remove_file(&target_path)?;

    // Remove from lockfile.
    let lock_path = config.workspace_root.join(".ta").join("agents.lock");
    if let Ok(mut lockfile) = Lockfile::load(&lock_path) {
        lockfile.remove(name);
        let _ = lockfile.save(&lock_path);
    }

    // Remove from cache.
    {
        let cache = SourceCache::new("agents");
        let _ = cache.remove(name);
    }

    println!("Removed agent config: {}", name);

    Ok(())
}

/// List externally-sourced agent configs.
fn list_external_agents(config: &GatewayConfig) -> anyhow::Result<()> {
    let lock_path = config.workspace_root.join(".ta").join("agents.lock");

    println!("External agent configs:");

    match Lockfile::load(&lock_path) {
        Ok(lockfile) => {
            let entries = lockfile.entries();
            if entries.is_empty() {
                println!("  (none installed)");
            } else {
                for entry in entries {
                    println!(
                        "  {} v{} (from: {})",
                        entry.name, entry.version, entry.source
                    );
                }
            }
        }
        Err(_) => {
            println!("  (none installed)");
        }
    }

    println!();
    println!("Install an agent config:");
    println!("  ta agent add security-reviewer --from registry:trustedautonomy/agents");
    println!("  ta agent add code-auditor --from https://example.com/ta-agents/auditor.yaml");

    Ok(())
}

/// Fetch content from an external source URL.
fn fetch_agent_content(url: &str) -> anyhow::Result<String> {
    let response = reqwest::blocking::get(url).map_err(|e| {
        anyhow::anyhow!(
            "Failed to fetch from '{}': {}\n\
             Check your network connection and the source URL.",
            url,
            e
        )
    })?;

    if !response.status().is_success() {
        anyhow::bail!(
            "HTTP {} when fetching '{}'.\n\
             Check that the source exists and is accessible.",
            response.status(),
            url
        );
    }

    response
        .text()
        .map_err(|e| anyhow::anyhow!("Failed to read response body from '{}': {}", url, e))
}

/// Compute SHA-256 checksum of content.
fn compute_agent_checksum(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn generate_planner_config(name: &str) -> String {
    format!(
        r#"# Agent Configuration: {name}
# Type: planner — Technical planning, decomposition, and design.
#
# Validate: ta agent validate .ta/agents/{name}.yaml

name: {name}
command: claude
args_template:
  - "{{prompt}}"

# Context injection settings.
injects_context_file: true
injects_settings: true

# Alignment profile — planners focus on analysis and design.
alignment:
  security_level: checkpoint
  allowed_actions:
    - read
    - list
    - search
    - plan
"#,
        name = name
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> GatewayConfig {
        GatewayConfig::for_project(dir.path())
    }

    #[test]
    fn new_agent_developer() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        new_agent("test-dev", "developer", &config).unwrap();

        let path = dir.path().join(".ta/agents/test-dev.yaml");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("name: test-dev"));
        assert!(content.contains("command: claude"));
        assert!(content.contains("developer"));
    }

    #[test]
    fn new_agent_auditor() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        new_agent("test-audit", "auditor", &config).unwrap();

        let path = dir.path().join(".ta/agents/test-audit.yaml");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("auditor"));
        assert!(content.contains("supervised"));
    }

    #[test]
    fn new_agent_planner() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        new_agent("test-plan", "planner", &config).unwrap();

        let path = dir.path().join(".ta/agents/test-plan.yaml");
        assert!(path.exists());
    }

    #[test]
    fn new_agent_orchestrator() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        new_agent("test-orch", "orchestrator", &config).unwrap();

        let path = dir.path().join(".ta/agents/test-orch.yaml");
        assert!(path.exists());
    }

    #[test]
    fn new_agent_unknown_type_error() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = new_agent("test", "unknown", &config);
        assert!(result.is_err());
    }

    #[test]
    fn new_agent_already_exists_error() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        new_agent("dup", "developer", &config).unwrap();
        let result = new_agent("dup", "developer", &config);
        assert!(result.is_err());
    }

    #[test]
    fn validate_valid_agent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.yaml");
        std::fs::write(
            &path,
            "name: test\ncommand: claude\nargs_template:\n  - \"{prompt}\"\n",
        )
        .unwrap();
        let result = validate_agent(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_invalid_agent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.yaml");
        std::fs::write(&path, "command: claude\n").unwrap();
        // Should not error (prints findings instead).
        let result = validate_agent(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_nonexistent_error() {
        let result = validate_agent(&PathBuf::from("/no/such/file.yaml"));
        assert!(result.is_err());
    }

    #[test]
    fn list_agents_empty() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = list_agents(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn list_agents_with_configs() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        new_agent("my-agent", "developer", &config).unwrap();
        let result = list_agents(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn remove_agent_not_found() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = remove_agent("nonexistent", &config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not found"));
    }

    #[test]
    fn remove_agent_success() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        new_agent("to-remove", "developer", &config).unwrap();
        let path = dir.path().join(".ta/agents/to-remove.yaml");
        assert!(path.exists());
        remove_agent("to-remove", &config).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn list_external_agents_empty() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = list_external_agents(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn compute_agent_checksum_deterministic() {
        let a = compute_agent_checksum("test content");
        let b = compute_agent_checksum("test content");
        assert_eq!(a, b);
        let c = compute_agent_checksum("different");
        assert_ne!(a, c);
    }
}
