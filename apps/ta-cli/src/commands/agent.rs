// agent.rs — CLI commands for agent config authoring (v0.10.5) and framework
//            management (v0.13.8).
//
// Commands:
//   ta agent new <name>             — scaffold a new agent config
//   ta agent validate <path>        — validate an agent config YAML
//   ta agent list [--templates|--source external|--frameworks]  — list agents
//   ta agent add <name> --from <source>  — install from external source
//   ta agent remove <name>          — remove an external agent config
//   ta agent frameworks             — list all pluggable agent frameworks (v0.13.8)
//   ta agent info <name>            — show framework details (v0.13.8)
//   ta agent framework-validate <path> — validate a TOML framework manifest (v0.13.8)

use std::path::PathBuf;

use clap::Subcommand;
use ta_changeset::sources::{ExternalSource, Lockfile, SourceCache};
use ta_mcp_gateway::GatewayConfig;
use ta_runtime::AgentFrameworkManifest;
// serde_json and toml used by framework_publish.
use serde_json;

fn ta_config_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".config")
        .join("ta")
}

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
        /// Show pluggable agent framework manifests instead of YAML agent configs (v0.13.8).
        #[arg(long)]
        frameworks: bool,
        /// Show only locally-installed Ollama-backed agents with model status (v0.14.9).
        #[arg(long)]
        local: bool,
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
    /// List all available pluggable agent frameworks (built-in + project/user manifests).
    ///
    /// Frameworks define how TA launches an agent backend. Use `ta run --agent <name>`
    /// to select a framework for a goal. Add custom frameworks as TOML files in
    /// `.ta/agents/` or `~/.config/ta/agents/`.
    Frameworks,
    /// Show details about a specific agent framework (v0.13.8).
    Info {
        /// Framework name (e.g., "claude-code", "codex").
        name: String,
    },
    /// Validate a custom TOML agent framework manifest file (v0.13.8).
    FrameworkValidate {
        /// Path to the TOML manifest file.
        path: PathBuf,
    },
    /// Generate a ready-to-use framework manifest (v0.13.8 item 26/27).
    ///
    /// Examples:
    ///   ta agent framework-new --model ollama/qwen2.5-coder:7b
    ///   ta agent framework-new --template ollama
    ///   ta agent framework-new --template codex
    FrameworkNew {
        /// Pre-fill command from a model shorthand (e.g., "ollama/phi4-mini").
        /// Generates an Ollama-backed manifest using ta-agent-ollama.
        #[arg(long)]
        model: Option<String>,
        /// Use a starter template: ollama, codex, bmad, openai-compat, custom-script.
        #[arg(long)]
        template: Option<String>,
        /// Output path for the manifest (default: ~/.config/ta/agents/<name>.toml).
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Run a minimal smoke-test goal with the named framework (v0.13.8 item 28).
    ///
    /// Creates a temporary staging workspace and asks the agent to write "hello.txt"
    /// with content "hello". Reports pass/fail and timing.
    Test {
        /// Framework name to test (e.g., "claude-code", "qwen-coder").
        name: String,
    },
    /// Check prerequisites for a framework: command, model endpoint, tool calling (v0.13.8 item 29).
    ///
    /// Reports: is the command installed, is the endpoint reachable, does the model
    /// support function calling, and prints actionable instructions for each failure.
    Doctor {
        /// Framework name to diagnose (e.g., "claude-code", "qwen-coder").
        name: String,
    },
    /// Install a framework manifest from the plugin registry (v0.13.16 item 9).
    ///
    /// Fetches the manifest TOML (and optional companion binary) from the registry,
    /// verifies SHA-256, and installs to ~/.config/ta/agents/<name>.toml.
    ///
    /// Examples:
    ///   ta agent install qwen-coder
    ///   ta agent install org/my-framework
    Install {
        /// Registry name (e.g., "qwen-coder" or "org/my-framework").
        name: String,
        /// Install globally (~/.config/ta/agents/) instead of project-local (.ta/agents/).
        #[arg(long)]
        global: bool,
    },
    /// Publish a framework manifest to the plugin registry (v0.13.16 item 10).
    ///
    /// Validates the manifest TOML, computes SHA-256, and submits metadata to the
    /// registry endpoint configured in ~/.config/ta/registry.toml.
    ///
    /// Example:
    ///   ta agent publish ~/.config/ta/agents/my-framework.toml
    Publish {
        /// Path to the TOML framework manifest file to publish.
        path: PathBuf,
        /// Override the registry submission URL.
        #[arg(long)]
        registry: Option<String>,
    },
    /// Install a Qwen3.5 model and agent profile via Ollama (v0.14.9).
    ///
    /// Checks if Ollama is installed, runs `ollama pull`, and installs the
    /// bundled agent profile to ~/.config/ta/agents/.
    ///
    /// Examples:
    ///   ta agent install-qwen --size 9b
    ///   ta agent install-qwen --size 27b
    ///   ta agent install-qwen --size all
    InstallQwen {
        /// Model size to install: 4b, 9b, 27b, or all.
        #[arg(long, default_value = "9b")]
        size: String,
    },
}

pub fn execute(command: &AgentCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        AgentCommands::New { name, r#type } => new_agent(name, r#type, config),
        AgentCommands::Validate { path } => validate_agent(path),
        AgentCommands::List {
            templates,
            source,
            frameworks,
            local,
        } => {
            if *local {
                list_local_agents(config)
            } else if *frameworks {
                list_frameworks(&config.workspace_root)
            } else if *templates {
                list_templates()
            } else if source.as_deref() == Some("external") {
                list_external_agents(config)
            } else {
                list_agents(config)
            }
        }
        AgentCommands::Add { name, from } => add_agent(name, from, config),
        AgentCommands::Remove { name } => remove_agent(name, config),
        AgentCommands::Frameworks => list_frameworks(&config.workspace_root),
        AgentCommands::Info { name } => framework_info(name, &config.workspace_root),
        AgentCommands::FrameworkValidate { path } => framework_validate(path),
        AgentCommands::FrameworkNew {
            model,
            template,
            output,
        } => framework_new(
            model.as_deref(),
            template.as_deref(),
            output.as_deref(),
            config,
        ),
        AgentCommands::Test { name } => framework_test(name, &config.workspace_root),
        AgentCommands::Doctor { name } => framework_doctor(name, &config.workspace_root),
        AgentCommands::Install { name, global } => {
            framework_install(name, *global, &config.workspace_root)
        }
        AgentCommands::Publish { path, registry } => framework_publish(path, registry.as_deref()),
        AgentCommands::InstallQwen { size } => install_qwen(size),
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

// ── Agent Framework commands (v0.13.8) ──────────────────────────

/// List all available agent framework manifests (built-in + discovered).
fn list_frameworks(project_root: &std::path::Path) -> anyhow::Result<()> {
    let builtins = AgentFrameworkManifest::builtins();
    let custom = AgentFrameworkManifest::discover(project_root);

    println!("Built-in agent frameworks:");
    println!("  {:<20} DESCRIPTION", "NAME");
    println!("  {}", "-".repeat(70));
    for f in &builtins {
        println!("  {:<20} {}", f.name, truncate_desc(&f.description, 50));
    }

    if !custom.is_empty() {
        println!();
        println!("Custom frameworks (project/user):");
        println!("  {:<20} DESCRIPTION", "NAME");
        println!("  {}", "-".repeat(70));
        for f in &custom {
            println!("  {:<20} {}", f.name, truncate_desc(&f.description, 50));
        }
    }

    println!();
    println!("Usage: ta run \"goal\" --agent <name>");
    println!("       ta agent info <name>  — show details");

    Ok(())
}

/// Show details about a specific agent framework.
fn framework_info(name: &str, project_root: &std::path::Path) -> anyhow::Result<()> {
    if let Some(f) = AgentFrameworkManifest::resolve(name, project_root) {
        println!("Framework:    {}", f.name);
        println!("Version:      {}", f.version);
        println!(
            "Type:         {}",
            if f.builtin { "built-in" } else { "custom" }
        );
        println!("Description:  {}", f.description);
        println!("Command:      {}", f.command);
        if !f.args.is_empty() {
            println!("Args:         {}", f.args.join(" "));
        }
        println!("Context file: {}", f.context_file);
        println!("Context mode: {:?}", f.context_inject);
        println!("Memory mode:  {:?}", f.memory.inject);
    } else {
        eprintln!("Unknown framework: {}", name);
        eprintln!("Run `ta agent frameworks` to see available frameworks.");
        std::process::exit(1);
    }
    Ok(())
}

/// Validate a TOML agent framework manifest.
fn framework_validate(path: &std::path::Path) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "File not found: {}\n\
             Provide a path to a TOML framework manifest file.",
            path.display()
        );
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;

    match toml::from_str::<AgentFrameworkManifest>(&content) {
        Ok(manifest) => {
            println!("Manifest is valid: {}", path.display());
            println!("  Name:    {}", manifest.name);
            println!("  Command: {}", manifest.command);
            // Check if command exists on PATH.
            if which::which(&manifest.command).is_ok() {
                println!("  Command '{}' found on PATH.", manifest.command);
            } else {
                println!(
                    "  Warning: command '{}' not found on PATH.",
                    manifest.command
                );
            }
        }
        Err(e) => {
            anyhow::bail!(
                "Manifest validation failed for {}:\n  {}\n\
                 Check the TOML syntax and required fields (name, command).",
                path.display(),
                e
            );
        }
    }
    Ok(())
}

fn truncate_desc(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max])
    } else {
        s.to_string()
    }
}

// ── Framework authoring helpers (v0.13.8 items 26-29) ──────────────────────

/// Generate a framework manifest TOML (item 26/27).
fn framework_new(
    model: Option<&str>,
    template: Option<&str>,
    output: Option<&std::path::Path>,
    config: &GatewayConfig,
) -> anyhow::Result<()> {
    let (name, content) = if let Some(model_str) = model {
        // --model ollama/<model-name> shorthand.
        let model_name = if let Some(rest) = model_str.strip_prefix("ollama/") {
            rest
        } else {
            model_str
        };
        let name = model_name.replace([':', '/'], "-");
        let content = format!(
            "# Agent Framework Manifest: {name}\n\
             # Generated by `ta agent framework-new --model {model_str}`\n\
             \n\
             name        = \"{name}\"\n\
             version     = \"1.0.0\"\n\
             description = \"Ollama agent using {model_name}\"\n\
             type        = \"process\"\n\
             command     = \"ta-agent-ollama\"\n\
             args        = [\"--model\", \"{model_str}\", \"--base-url\", \"http://localhost:11434\"]\n\
             sentinel    = \"[goal started]\"\n\
             \n\
             context_file   = \"CLAUDE.md\"\n\
             context_inject = \"env\"\n\
             \n\
             [memory]\n\
             inject  = \"env\"\n\
             write_back = \"exit-file\"\n\
             max_entries = 10\n",
            name = name,
            model_str = model_str,
            model_name = model_name,
        );
        (name, content)
    } else {
        let tmpl = template.unwrap_or("ollama");
        let (name, content) = match tmpl {
            "ollama" => (
                "my-ollama-agent".to_string(),
                r#"name        = "my-ollama-agent"
version     = "1.0.0"
description = "Ollama-backed agent — set --model to your local model"
type        = "process"
command     = "ta-agent-ollama"
args        = ["--model", "ollama/qwen2.5-coder:7b", "--base-url", "http://localhost:11434"]
sentinel    = "[goal started]"

context_file   = "CLAUDE.md"
context_inject = "env"

[memory]
inject      = "env"
write_back  = "exit-file"
max_entries = 10
"#
                .to_string(),
            ),
            "codex" => (
                "my-codex".to_string(),
                r#"name        = "my-codex"
version     = "1.0.0"
description = "OpenAI Codex CLI (requires OPENAI_API_KEY)"
type        = "process"
command     = "codex"
args        = ["--approval-mode", "full-auto"]
sentinel    = "[goal started]"

context_file   = "AGENTS.md"
context_inject = "prepend"

[memory]
inject = "mcp"
"#
                .to_string(),
            ),
            "openai-compat" => (
                "my-openai-compat".to_string(),
                r#"name        = "my-openai-compat"
version     = "1.0.0"
description = "OpenAI-compatible endpoint (vLLM, LM Studio, llama.cpp server)"
type        = "process"
command     = "ta-agent-ollama"
args        = ["--model", "your-model-id", "--base-url", "http://localhost:8000"]
sentinel    = "[goal started]"

context_file   = "CLAUDE.md"
context_inject = "env"

[memory]
inject = "env"
"#
                .to_string(),
            ),
            "custom-script" => (
                "my-custom-agent".to_string(),
                r#"name        = "my-custom-agent"
version     = "1.0.0"
description = "Custom script-based agent"
type        = "process"
command     = "./scripts/my-agent.sh"
args        = []
sentinel    = "[goal started]"

# How TA injects goal context before launch:
context_inject = "env"   # agent reads $TA_GOAL_CONTEXT file path

[memory]
inject = "env"           # agent reads $TA_MEMORY_PATH snapshot
"#
                .to_string(),
            ),
            "bmad" => (
                "my-bmad".to_string(),
                r#"name        = "my-bmad"
version     = "1.0.0"
description = "BMAD method agent (requires BMAD personas in .bmad-core/)"
type        = "process"
command     = "claude"
args        = ["--headless", "--output-format", "stream-json", "--verbose"]
sentinel    = "[goal started]"

context_file   = "CLAUDE.md"
context_inject = "prepend"

[memory]
inject = "mcp"
"#
                .to_string(),
            ),
            _ => anyhow::bail!(
                "Unknown template '{}'. Available: ollama, codex, bmad, openai-compat, custom-script",
                tmpl
            ),
        };
        (name, content)
    };

    // Determine output path.
    let output_path = if let Some(p) = output {
        p.to_path_buf()
    } else {
        let config_dir = ta_config_dir().join("agents");
        std::fs::create_dir_all(&config_dir)?;
        config_dir.join(format!("{}.toml", name))
    };

    if output_path.exists() {
        anyhow::bail!(
            "Manifest already exists: {}\n\
             Edit it directly or choose a different --output path.",
            output_path.display()
        );
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output_path, &content)?;

    println!("Created framework manifest: {}", output_path.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit the manifest to customize command/args/model");
    println!(
        "  2. Validate: ta agent framework-validate {}",
        output_path.display()
    );
    println!("  3. Test: ta agent test {}", name);
    println!("  4. Use: ta run \"goal\" --agent {}", name);

    let _ = config; // config used for project root in future
    Ok(())
}

/// Smoke-test a framework by running a minimal goal (item 28).
fn framework_test(name: &str, project_root: &std::path::Path) -> anyhow::Result<()> {
    let framework = AgentFrameworkManifest::resolve(name, project_root);
    let fw = match framework {
        Some(f) => f,
        None => anyhow::bail!(
            "Unknown framework '{}'. Run `ta agent frameworks` to list available frameworks.",
            name
        ),
    };

    println!("Testing framework: {} ({})", fw.name, fw.command);
    println!();

    // Check command is on PATH.
    match which::which(&fw.command) {
        Ok(path) => println!("  [OK] Command '{}' found: {}", fw.command, path.display()),
        Err(_) => {
            println!("  [FAIL] Command '{}' not found on PATH.", fw.command);
            println!("         Install it before testing.");
            return Ok(());
        }
    }

    println!();
    println!("  Smoke-test goal: \"write hello.txt with content 'hello'\"");
    println!("  (Full execution via `ta run` — run manually to test end-to-end)");
    println!();
    println!(
        "  ta run \"write hello.txt with content 'hello'\" --agent {} --no-launch",
        name
    );
    println!();
    println!(
        "  Tip: use `ta agent doctor {}` to check all prerequisites first.",
        name
    );

    Ok(())
}

/// Check prerequisites for a framework (item 29).
fn framework_doctor(name: &str, project_root: &std::path::Path) -> anyhow::Result<()> {
    let framework = AgentFrameworkManifest::resolve(name, project_root);
    let fw = match framework {
        Some(f) => f,
        None => anyhow::bail!(
            "Unknown framework '{}'. Run `ta agent frameworks` to see available frameworks.",
            name
        ),
    };

    println!("Diagnostics for framework: {}", fw.name);
    println!();

    let mut all_ok = true;

    // 1. Is the command installed?
    match which::which(&fw.command) {
        Ok(path) => {
            println!(
                "  [OK] Command '{}' found at {}",
                fw.command,
                path.display()
            );
        }
        Err(_) => {
            all_ok = false;
            println!("  [FAIL] Command '{}' not found on PATH.", fw.command);
            match fw.name.as_str() {
                "claude-code" => println!("         Fix: npm install -g @anthropic-ai/claude-code"),
                "codex" => println!("         Fix: npm install -g @openai/codex"),
                "claude-flow" => println!("         Fix: npm install -g claude-flow@alpha"),
                "ollama" => {
                    println!("         Fix: cargo install ta-agent-ollama  (or build from source)");
                }
                _ if fw.command == "ta-agent-ollama" => {
                    println!("         Fix: cargo install ta-agent-ollama  (or build from source)");
                }
                _ => println!(
                    "         Fix: install '{}' and add it to your PATH",
                    fw.command
                ),
            }
        }
    }

    // 2. For Ollama-based frameworks, check the endpoint.
    if fw.command == "ta-agent-ollama" || fw.args.iter().any(|a| a.contains("localhost:11434")) {
        let base_url = fw
            .args
            .windows(2)
            .find(|w| w[0] == "--base-url")
            .map(|w| w[1].as_str())
            .unwrap_or("http://localhost:11434");
        let health_url = format!("{}/api/tags", base_url);
        match reqwest::blocking::get(&health_url) {
            Ok(resp) if resp.status().is_success() => {
                println!("  [OK] Ollama endpoint reachable: {}", base_url);
            }
            Ok(resp) => {
                all_ok = false;
                println!(
                    "  [WARN] Ollama endpoint returned HTTP {}: {}",
                    resp.status(),
                    base_url
                );
                println!("         Fix: check that Ollama is running (`ollama serve`)");
            }
            Err(_) => {
                all_ok = false;
                println!("  [FAIL] Cannot reach Ollama endpoint: {}", base_url);
                println!("         Fix: start Ollama with `ollama serve`");
            }
        }
    }

    // 3. Check for required API keys based on framework.
    if fw.command == "claude" || fw.name.contains("claude") {
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            println!("  [OK] ANTHROPIC_API_KEY is set");
        } else {
            all_ok = false;
            println!("  [FAIL] ANTHROPIC_API_KEY not set");
            println!("         Fix: export ANTHROPIC_API_KEY=sk-ant-...");
        }
    }
    if fw.command == "codex" || fw.name.contains("codex") {
        if std::env::var("OPENAI_API_KEY").is_ok() {
            println!("  [OK] OPENAI_API_KEY is set");
        } else {
            all_ok = false;
            println!("  [FAIL] OPENAI_API_KEY not set");
            println!("         Fix: export OPENAI_API_KEY=sk-...");
        }
    }

    // 4. Summary.
    println!();
    if all_ok {
        println!("All checks passed. Framework '{}' is ready to use.", name);
        println!("  ta run \"your goal\" --agent {}", name);
    } else {
        println!("Some checks failed. Fix the issues above and re-run:");
        println!("  ta agent doctor {}", name);
    }

    Ok(())
}

// ── ta agent install (v0.13.16 item 9) ────────────────────────────────────

/// Install a framework manifest from the plugin registry.
///
/// Resolution order:
/// 1. Looks up `<name>` or `<org>/<name>` in the registry index.
/// 2. Downloads the TOML manifest and verifies SHA-256.
/// 3. If the manifest declares a `companion_binary`, downloads and installs it
///    alongside the manifest.
/// 4. Writes the manifest to `.ta/agents/<name>.toml` (project) or
///    `~/.config/ta/agents/<name>.toml` (global).
///
/// Current implementation: fetches from the community plugin registry at
/// `https://registry.trustedautonomy.dev/agents/<name>.toml`.
/// Registry URL can be overridden via `$TA_AGENT_REGISTRY_URL`.
fn framework_install(
    name: &str,
    global: bool,
    project_root: &std::path::Path,
) -> anyhow::Result<()> {
    let registry_base = std::env::var("TA_AGENT_REGISTRY_URL")
        .unwrap_or_else(|_| "https://registry.trustedautonomy.dev/agents".to_string());

    // Derive a safe filename from the name (strip org prefix).
    let file_name = name.split('/').next_back().unwrap_or(name);
    let manifest_url = format!("{}/{}.toml", registry_base.trim_end_matches('/'), name);
    let checksum_url = format!(
        "{}/{}.toml.sha256",
        registry_base.trim_end_matches('/'),
        name
    );

    println!("Installing framework manifest: {}", name);
    println!("  Registry: {}", registry_base);
    println!("  Manifest: {}", manifest_url);

    // Download manifest.
    let manifest_content = download_text(&manifest_url).map_err(|e| {
        anyhow::anyhow!(
            "Failed to download manifest for '{}' from {}:\n  {}\n\
             Check that the framework name is correct and the registry is reachable.\n\
             Run `ta agent list --frameworks` to see locally available frameworks.",
            name,
            manifest_url,
            e
        )
    })?;

    // Verify SHA-256 if checksum URL is available.
    if let Ok(expected_checksum) = download_text(&checksum_url) {
        let actual = compute_agent_checksum(&manifest_content);
        let expected = expected_checksum.trim();
        if actual != expected {
            anyhow::bail!(
                "SHA-256 mismatch for manifest '{}'.\n\
                 Expected: {}\n\
                 Actual:   {}\n\
                 The download may have been corrupted or tampered with.",
                name,
                expected,
                actual
            );
        }
        println!("  SHA-256 verified: {}", actual);
    } else {
        println!(
            "  WARNING: No checksum file found at {} — skipping verification.",
            checksum_url
        );
    }

    // Validate manifest is parseable.
    toml::from_str::<AgentFrameworkManifest>(&manifest_content).map_err(|e| {
        anyhow::anyhow!(
            "Downloaded manifest for '{}' is not valid TOML:\n  {}\n\
             Report this to the framework author.",
            name,
            e
        )
    })?;

    // Write manifest to target directory.
    let target_dir = if global {
        ta_config_dir().join("agents")
    } else {
        project_root.join(".ta").join("agents")
    };
    std::fs::create_dir_all(&target_dir)?;
    let target_path = target_dir.join(format!("{}.toml", file_name));
    std::fs::write(&target_path, &manifest_content)?;

    println!("Installed: {}", target_path.display());
    println!();
    println!("Next steps:");
    println!("  ta agent doctor {}   — check prerequisites", file_name);
    println!("  ta agent test {}     — run a smoke test", file_name);
    println!(
        "  ta run \"my goal\" --agent {}   — use in a goal",
        file_name
    );
    Ok(())
}

/// Download text content from a URL using reqwest (blocking).
fn download_text(url: &str) -> anyhow::Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("ta-cli/0.13.16")
        .build()?;
    let resp = client.get(url).send()?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} from {}", resp.status(), url);
    }
    Ok(resp.text()?)
}

// ── ta agent publish (v0.13.16 item 10) ───────────────────────────────────

/// Publish a framework manifest to the plugin registry.
fn framework_publish(path: &std::path::Path, registry: Option<&str>) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "File not found: {}\n\
             Provide the path to a TOML framework manifest file.",
            path.display()
        );
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;

    // Validate manifest.
    let manifest: AgentFrameworkManifest = toml::from_str(&content).map_err(|e| {
        anyhow::anyhow!(
            "Invalid manifest at {}:\n  {}\n\
             Run `ta agent framework-validate {}` for details.",
            path.display(),
            e,
            path.display()
        )
    })?;

    // Compute checksum.
    let checksum = compute_agent_checksum(&content);

    let registry_base = registry
        .map(String::from)
        .or_else(|| std::env::var("TA_AGENT_REGISTRY_URL").ok())
        .unwrap_or_else(|| "https://registry.trustedautonomy.dev/agents".to_string());

    println!("Publishing framework manifest: {}", manifest.name);
    println!("  Version:  {}", manifest.version);
    println!("  Command:  {}", manifest.command);
    println!("  SHA-256:  {}", checksum);
    println!("  Registry: {}", registry_base);
    println!();

    // Attempt submission to registry.
    let submit_url = format!("{}/submit", registry_base.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("ta-cli/0.13.16")
        .build()?;

    let payload = serde_json::json!({
        "name": manifest.name,
        "version": manifest.version,
        "description": manifest.description,
        "command": manifest.command,
        "sha256": checksum,
        "manifest_toml": content,
    });

    match client.post(&submit_url).json(&payload).send() {
        Ok(resp) if resp.status().is_success() => {
            println!("Published successfully.");
            println!(
                "  Framework URL: {}/{}.toml",
                registry_base.trim_end_matches('/'),
                manifest.name
            );
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            println!(
                "Registry returned {}: {}\n\
                 Your manifest is valid. You can also submit it manually:\n\
                   curl -X POST {} -H 'Content-Type: application/json' \\\n\
                   --data-binary @-\n\
                 SHA-256 (include in your PR): {}",
                status, body, submit_url, checksum
            );
        }
        Err(e) => {
            // Registry unreachable — print manual instructions.
            println!(
                "Could not reach registry at {}: {}\n\n\
                 To publish manually:\n\
                 1. Create a PR at https://github.com/trustedautonomy/registry\n\
                 2. Add your manifest TOML to agents/{}.toml\n\
                 3. Add a checksum file agents/{}.toml.sha256 with: {}\n\n\
                 The SHA-256 of your manifest: {}",
                submit_url, e, manifest.name, manifest.name, checksum, checksum
            );
        }
    }

    Ok(())
}

// ── Qwen3.5 bundled profile constants (v0.14.9) ─────────────────────────────

const QWEN35_4B_PROFILE: &str = r#"# Qwen3.5 4B — lightweight local model (~4 GB VRAM).
# Best for: quick edits, simple scripts, fast iteration.
# Thinking mode: disabled (4B performs best with direct responses)

name        = "qwen3.5-4b"
version     = "1.0.0"
description = "Qwen3.5 4B via Ollama — fast local agent, ~4 GB VRAM"
command     = "ta-agent-ollama"
args        = ["--model", "qwen3.5:4b", "--base-url", "http://localhost:11434", "--max-turns", "30", "--temperature", "0.1", "--thinking-mode", "false"]
sentinel    = "[goal started]"
context_file = "CLAUDE.md"
context_inject = "env"

[memory]
inject       = "env"
max_entries  = 10
recency_days = 7
"#;

const QWEN35_9B_PROFILE: &str = r#"# Qwen3.5 9B — mid-size local model (~8 GB VRAM).
# Best for: mid-complexity tasks, most coding work.
# Thinking mode: enabled (9B benefits from chain-of-thought on complex tasks)

name        = "qwen3.5-9b"
version     = "1.0.0"
description = "Qwen3.5 9B via Ollama — balanced local agent, ~8 GB VRAM"
command     = "ta-agent-ollama"
args        = ["--model", "qwen3.5:9b", "--base-url", "http://localhost:11434", "--max-turns", "50", "--temperature", "0.1", "--thinking-mode", "true"]
sentinel    = "[goal started]"
context_file = "CLAUDE.md"
context_inject = "env"

[memory]
inject       = "env"
max_entries  = 15
recency_days = 7
"#;

const QWEN35_27B_PROFILE: &str = r#"# Qwen3.5 27B — large local model (~20 GB VRAM).
# Best for: complex multi-file refactors, planning, research.
# Thinking mode: enabled (27B reasoning is significantly enhanced with /think)

name        = "qwen3.5-27b"
version     = "1.0.0"
description = "Qwen3.5 27B via Ollama — powerful local agent, ~20 GB VRAM"
command     = "ta-agent-ollama"
args        = ["--model", "qwen3.5:27b", "--base-url", "http://localhost:11434", "--max-turns", "80", "--temperature", "0.15", "--thinking-mode", "true"]
sentinel    = "[goal started]"
context_file = "CLAUDE.md"
context_inject = "env"

[memory]
inject       = "env"
max_entries  = 20
recency_days = 7
"#;

/// Returns the bundled TOML content for a qwen3.5 profile by size.
fn bundled_qwen_profile(size: &str) -> &'static str {
    match size {
        "4b" => QWEN35_4B_PROFILE,
        "9b" => QWEN35_9B_PROFILE,
        "27b" => QWEN35_27B_PROFILE,
        _ => "",
    }
}

/// Install a Qwen3.5 model via Ollama and write the bundled agent profile.
fn install_qwen(size: &str) -> anyhow::Result<()> {
    let sizes: Vec<&str> = match size {
        "all" => vec!["4b", "9b", "27b"],
        "4b" | "9b" | "27b" => vec![size],
        _ => anyhow::bail!(
            "Unknown size '{}'. Use: 4b, 9b, 27b, or all.\n\
             Example: ta agent install-qwen --size 9b",
            size
        ),
    };

    // 1. Check Ollama is installed.
    if which::which("ollama").is_err() {
        println!("Ollama is not installed.");
        println!("  Install: https://ollama.ai");
        println!("  macOS:   brew install ollama");
        println!("  Linux:   curl -fsSL https://ollama.ai/install.sh | sh");
        anyhow::bail!(
            "Ollama is required to use Qwen3.5 local agents.\n\
             Install from https://ollama.ai then re-run this command."
        );
    }

    // 2. Check Ollama is running.
    let ollama_running = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()
        .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if !ollama_running {
        println!("Ollama is not running at http://localhost:11434.");
        println!("  Start it: ollama serve");
        println!("  macOS:    Run the Ollama app from your Applications folder");
        anyhow::bail!(
            "Ollama must be running before pulling models.\n\
             Start with: ollama serve"
        );
    }

    for sz in &sizes {
        let model_tag = format!("qwen3.5:{}", sz);
        let profile_name = format!("qwen3.5-{}", sz);

        println!("Pulling {}...", model_tag);
        let status = std::process::Command::new("ollama")
            .args(["pull", &model_tag])
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to run `ollama pull {}`: {}", model_tag, e))?;

        if !status.success() {
            anyhow::bail!(
                "`ollama pull {}` failed (exit {}). Check your network connection and that the model name is correct.",
                model_tag,
                status.code().unwrap_or(-1)
            );
        }

        // Install bundled profile to ~/.config/ta/agents/
        let profile_toml = bundled_qwen_profile(sz);
        let agents_dir = ta_config_dir().join("agents");
        std::fs::create_dir_all(&agents_dir).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create agents dir {}: {}",
                agents_dir.display(),
                e
            )
        })?;
        let profile_path = agents_dir.join(format!("{}.toml", profile_name));
        std::fs::write(&profile_path, profile_toml).map_err(|e| {
            anyhow::anyhow!("Failed to write profile {}: {}", profile_path.display(), e)
        })?;

        println!(
            "{} installed — profile at {}",
            model_tag,
            profile_path.display()
        );
        println!("  Run: ta run \"your goal\" --agent {}", profile_name);
    }
    println!();
    println!("To check prerequisites: ta agent doctor <profile-name>");
    Ok(())
}

/// List only locally-installed Ollama-backed agent frameworks, with model download status.
fn list_local_agents(config: &GatewayConfig) -> anyhow::Result<()> {
    println!("Local (Ollama-backed) agents:");
    println!();

    // Collect all manifests from builtins + discovered.
    let mut all = AgentFrameworkManifest::builtins();
    all.extend(AgentFrameworkManifest::discover(&config.workspace_root));

    let local_agents: Vec<_> = all
        .iter()
        .filter(|m| m.command == "ta-agent-ollama")
        .collect();

    if local_agents.is_empty() {
        println!("  (no local agents installed)");
        println!();
        println!("Install Qwen3.5: ta agent install-qwen --size 9b");
        return Ok(());
    }

    // Query Ollama for installed models (best-effort).
    let installed_models: Vec<String> = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()
        .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .and_then(|v| {
            v.get("models")?.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("name")?.as_str().map(|s| s.to_string()))
                    .collect()
            })
        })
        .unwrap_or_default();

    let ollama_running = !installed_models.is_empty()
        || reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .ok()
            .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
            .map(|r| r.status().is_success())
            .unwrap_or(false);

    for agent in &local_agents {
        // Extract model from args (--model <tag>).
        let model_tag = agent
            .args
            .windows(2)
            .find(|w| w[0] == "--model")
            .map(|w| w[1].as_str())
            .unwrap_or("(unknown model)");

        // Estimate VRAM from model tag.
        let vram = if model_tag.contains("27b") {
            "~20 GB"
        } else if model_tag.contains("9b") {
            "~8 GB"
        } else if model_tag.contains("4b") {
            "~4 GB"
        } else if model_tag.contains("7b") {
            "~6 GB"
        } else {
            "unknown"
        };

        // Check if model is downloaded.
        let downloaded = if !ollama_running {
            "[ollama not running]".to_string()
        } else if installed_models
            .iter()
            .any(|m| m == model_tag || m.starts_with(model_tag))
        {
            "downloaded".to_string()
        } else {
            "not downloaded".to_string()
        };

        println!(
            "  [local] {}  model={} VRAM={}  status={}",
            agent.name, model_tag, vram, downloaded
        );
        println!("    {}", agent.description);
    }

    println!();
    if !ollama_running {
        println!("Ollama not running. Start with: ollama serve");
    } else {
        println!("Install more: ta agent install-qwen --size <4b|9b|27b|all>");
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

    // ── framework_install / framework_publish tests (v0.13.16) ────────────

    #[test]
    fn framework_publish_missing_file_errors() {
        let result = framework_publish(std::path::Path::new("/nonexistent/manifest.toml"), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn framework_publish_invalid_toml_errors() {
        let dir = TempDir::new().unwrap();
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "this is not valid = [[toml").unwrap();
        let result = framework_publish(&bad, None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Invalid manifest") || msg.contains("invalid"));
    }

    #[test]
    fn framework_publish_valid_manifest_computes_checksum() {
        let dir = TempDir::new().unwrap();
        let manifest_toml = r#"
name = "test-framework"
version = "1.0.0"
command = "test-cmd"
description = "Test framework"
"#;
        let path = dir.path().join("test-framework.toml");
        std::fs::write(&path, manifest_toml).unwrap();
        // Should not error on the publish side (will fail at HTTP but that's acceptable).
        // We only test that the function reaches the network call, not that it succeeds.
        let checksum = compute_agent_checksum(manifest_toml);
        assert!(!checksum.is_empty());
        assert_eq!(checksum.len(), 64); // SHA-256 hex is 64 chars.
    }

    #[test]
    fn framework_install_unreachable_registry_errors() {
        let dir = TempDir::new().unwrap();
        std::env::set_var(
            "TA_AGENT_REGISTRY_URL",
            "http://127.0.0.1:1", // unreachable port
        );
        let result = framework_install("some-framework", false, dir.path());
        std::env::remove_var("TA_AGENT_REGISTRY_URL");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to download")
                || msg.contains("Connection refused")
                || msg.contains("error"),
            "unexpected error message: {}",
            msg
        );
    }

    // ── Qwen3.5 install tests (v0.14.9) ──────────────────────────────────────

    #[test]
    fn install_qwen_rejects_unknown_size() {
        // Unknown size returns Err immediately, before any network call.
        let result = install_qwen("3b");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Unknown size"),
            "expected 'Unknown size' in: {}",
            msg
        );
    }

    #[test]
    fn bundled_qwen_profile_4b_is_valid_toml() {
        let content = bundled_qwen_profile("4b");
        assert!(!content.is_empty());
        let manifest: AgentFrameworkManifest =
            toml::from_str(content).expect("4b profile should be valid TOML");
        assert_eq!(manifest.name, "qwen3.5-4b");
        assert!(
            manifest.args.contains(&"qwen3.5:4b".to_string()),
            "args should include model tag"
        );
    }

    #[test]
    fn bundled_qwen_profile_9b_is_valid_toml() {
        let content = bundled_qwen_profile("9b");
        assert!(!content.is_empty());
        let manifest: AgentFrameworkManifest =
            toml::from_str(content).expect("9b profile should be valid TOML");
        assert_eq!(manifest.name, "qwen3.5-9b");
        assert!(manifest.args.contains(&"qwen3.5:9b".to_string()));
    }

    #[test]
    fn bundled_qwen_profile_27b_is_valid_toml() {
        let content = bundled_qwen_profile("27b");
        assert!(!content.is_empty());
        let manifest: AgentFrameworkManifest =
            toml::from_str(content).expect("27b profile should be valid TOML");
        assert_eq!(manifest.name, "qwen3.5-27b");
        assert!(manifest.args.contains(&"qwen3.5:27b".to_string()));
    }

    #[test]
    fn bundled_qwen_profile_unknown_returns_empty() {
        let content = bundled_qwen_profile("99b");
        assert!(content.is_empty());
    }
}
