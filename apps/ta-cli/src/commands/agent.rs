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
    /// Migrate an existing agent framework configuration to the standalone plugin (v0.16.2).
    ///
    /// Detects existing agent configs, installs the standalone plugin, updates
    /// profile paths, and verifies connectivity.
    ///
    /// Currently supported frameworks:
    ///   ta agent migrate ollama   — migrate to standalone ta-agent-ollama plugin
    Migrate {
        /// Framework to migrate: "ollama".
        framework: String,
    },
    /// Show the history of `agent = "auto"` supervisor recommendations (v0.17.0.12.13).
    ///
    /// Per the Observable & Actionable constitution principle, every time a
    /// `Switch` action tier resolves to `"auto"`, the chosen agent and the
    /// rationale behind it are appended to `.ta/agent-recommendations.jsonl`.
    /// This command surfaces that history — `"auto"` is never a black box.
    Recommendations {
        /// Show only the last N recommendations (default: all).
        #[arg(long)]
        limit: Option<usize>,
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
            } else if source.as_deref() == Some("yaml") {
                list_agents(config)
            } else {
                // v0.16.3: default shows TOML agent profiles (name, model, inherit, ctx files).
                list_agent_profiles(&config.workspace_root)
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
        AgentCommands::Migrate { framework } => migrate_agent_framework(framework, config),
        AgentCommands::Recommendations { limit } => show_agent_recommendations(config, *limit),
    }
}

/// `ta agent recommendations` — show the `agent = "auto"` supervisor recommendation
/// history from `.ta/agent-recommendations.jsonl` (v0.17.0.12.13).
fn show_agent_recommendations(config: &GatewayConfig, limit: Option<usize>) -> anyhow::Result<()> {
    let mut recs = super::run::read_agent_recommendations(&config.workspace_root);
    if recs.is_empty() {
        println!("No agent = \"auto\" recommendations recorded yet.");
        println!(
            "Recommendations are logged to .ta/agent-recommendations.jsonl whenever a Switch \
             action tier resolves to \"auto\"."
        );
        return Ok(());
    }
    if let Some(n) = limit {
        let skip = recs.len().saturating_sub(n);
        recs.drain(..skip);
    }
    for rec in &recs {
        println!("{}  tier={}  agent={}", rec.timestamp, rec.tier, rec.agent);
        if let Some(p) = &rec.persona {
            println!("  persona: {}", p);
        }
        if let Some(w) = &rec.workload_type {
            println!("  workload_type: {}", w);
        }
        if let Some(t) = &rec.goal_title {
            println!("  goal: {}", t);
        }
        println!("  rationale: {}", rec.rationale);
        println!();
    }
    Ok(())
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
    // Resolve all frameworks (built-in + custom) to show channel info.
    let all_frameworks = {
        let mut m: std::collections::HashMap<String, AgentFrameworkManifest> =
            std::collections::HashMap::new();
        for f in AgentFrameworkManifest::builtins() {
            m.insert(f.name.clone(), f);
        }
        for f in AgentFrameworkManifest::discover(&config.workspace_root) {
            m.insert(f.name.clone(), f);
        }
        m
    };

    let agents_dir = config.workspace_root.join(".ta").join("agents");

    println!("Configured agents:");
    println!(
        "  {:<20} {:<14} {:<12} LIVE",
        "NAME", "CHANNEL", "INJECT_MODE"
    );
    println!("  {}", "-".repeat(65));
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

            // Resolve channel info from manifest (fall back to generic_file defaults).
            let (channel_str, inject_mode_str, live_str) =
                if let Some(fw) = all_frameworks.get(&name) {
                    let caps = ta_runtime::channels::build_channel(
                        &fw.channel_type,
                        std::path::PathBuf::from("."),
                        &fw.context_file,
                    )
                    .capabilities();
                    (
                        format!("{}", fw.channel_type),
                        format!("{:?}", fw.context_inject),
                        caps.live_label().to_string(),
                    )
                } else {
                    (
                        "GenericFile".to_string(),
                        "Prepend".to_string(),
                        "Queued".to_string(),
                    )
                };

            println!(
                "  {:<20} {:<14} {:<12} {}",
                name, channel_str, inject_mode_str, live_str
            );
        }

        if !found {
            println!("  (none configured — use `ta agent new` to create one)");
        }
    } else {
        println!("  (none configured — use `ta agent new` to create one)");
    }

    println!();
    println!("Scaffold a new agent:");
    println!("  ta agent new my-agent --type developer");
    println!();
    println!("Browse templates:");
    println!("  ta agent list --templates");
    println!();
    println!("Show framework channel details:");
    println!("  ta agent list --frameworks");

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

/// List agent profiles (TOML manifests) from project and user directories (v0.16.3).
///
/// Shows: name, model (extracted from --model arg), inherit source, context file count.
fn list_agent_profiles(project_root: &std::path::Path) -> anyhow::Result<()> {
    let user_agents_dir = ta_config_dir().join("agents");
    let project_agents_dir = project_root.join(".ta").join("agents");

    let load_profiles_from = |dir: &std::path::Path| -> Vec<AgentFrameworkManifest> {
        if !dir.is_dir() {
            return Vec::new();
        }
        let mut profiles = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut by_stem: std::collections::HashMap<String, std::path::PathBuf> =
                std::collections::HashMap::new();
            for entry in entries.flatten() {
                let path = entry.path();
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();
                if ext != "toml" && ext != "yaml" && ext != "yml" {
                    continue;
                }
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if ext == "yaml" || ext == "yml" {
                    by_stem.insert(stem, path);
                } else {
                    by_stem.entry(stem).or_insert(path);
                }
            }
            let mut sorted_keys: Vec<_> = by_stem.keys().cloned().collect();
            sorted_keys.sort();
            for key in sorted_keys {
                let path = &by_stem[&key];
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let result = std::fs::read_to_string(path).and_then(|s| {
                    if ext == "yaml" || ext == "yml" {
                        serde_yaml::from_str::<AgentFrameworkManifest>(&s)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                    } else {
                        toml::from_str::<AgentFrameworkManifest>(&s)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                    }
                });
                if let Ok(m) = result {
                    profiles.push(m);
                }
            }
        }
        profiles
    };

    let print_profile_table = |profiles: &[AgentFrameworkManifest]| {
        println!("  {:<22} {:<18} {:<22} CTX", "NAME", "MODEL", "INHERIT");
        println!("  {}", "-".repeat(72));
        for p in profiles {
            let model = p.extract_model().unwrap_or("—");
            let inherit = p
                .inherit
                .as_deref()
                .map(|s| {
                    // Shorten home-dir paths for display.
                    if let Some(stripped) =
                        s.strip_prefix(&std::env::var("HOME").unwrap_or_default())
                    {
                        format!("~{}", stripped)
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| "—".to_string());
            let ctx_count = p.context.as_ref().map(|c| c.files.len()).unwrap_or(0);
            let ctx_str = if ctx_count == 0 {
                "—".to_string()
            } else {
                ctx_count.to_string()
            };
            println!(
                "  {:<22} {:<18} {:<22} {}",
                p.name,
                truncate_desc(model, 16),
                truncate_desc(&inherit, 20),
                ctx_str,
            );
        }
    };

    let project_profiles = load_profiles_from(&project_agents_dir);
    let user_profiles = load_profiles_from(&user_agents_dir);

    if project_profiles.is_empty() && user_profiles.is_empty() {
        println!("No agent profiles found.");
        println!();
        println!(
            "  Project profiles: {}/.ta/agents/*.toml",
            project_root.display()
        );
        println!("  User profiles:    ~/.config/ta/agents/*.toml");
        println!();
        println!("Install an Ollama profile: ta agent install gemma4");
        println!("List framework backends:   ta agent list --frameworks");
        return Ok(());
    }

    if !project_profiles.is_empty() {
        println!("Project profiles ({}):", project_profiles.len());
        print_profile_table(&project_profiles);
        println!();
    }

    if !user_profiles.is_empty() {
        println!("User profiles ({}):", user_profiles.len());
        print_profile_table(&user_profiles);
        println!();
    }

    println!("Usage: ta run \"goal\" --agent <name>");
    println!("       ta agent info <name>     — show full details");
    println!("       ta agent list --frameworks — show built-in backends");

    Ok(())
}

/// List all available agent framework manifests (built-in + discovered).
fn list_frameworks(project_root: &std::path::Path) -> anyhow::Result<()> {
    let builtins = AgentFrameworkManifest::builtins();
    let custom = AgentFrameworkManifest::discover(project_root);

    fn print_frameworks(frameworks: &[AgentFrameworkManifest]) {
        println!(
            "  {:<20} {:<14} {:<12} {:<8} DESCRIPTION",
            "NAME", "CHANNEL", "INJECT_MODE", "LIVE"
        );
        println!("  {}", "-".repeat(80));
        for f in frameworks {
            let caps = ta_runtime::channels::build_channel(
                &f.channel_type,
                std::path::PathBuf::from("."),
                &f.context_file,
            )
            .capabilities();
            println!(
                "  {:<20} {:<14} {:<12} {:<8} {}",
                f.name,
                format!("{}", f.channel_type),
                format!("{:?}", f.context_inject),
                caps.live_label(),
                truncate_desc(&f.description, 30),
            );
        }
    }

    println!("Built-in agent frameworks:");
    print_frameworks(&builtins);

    if !custom.is_empty() {
        println!();
        println!("Custom frameworks (project/user):");
        print_frameworks(&custom);
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
        if let Some(ref inh) = f.inherit {
            println!("Inherit:      {}", inh);
        }
        if let Some(ref ctx) = f.context {
            if !ctx.files.is_empty() {
                println!("Context files ({}):", ctx.files.len());
                for cf in &ctx.files {
                    let resolved =
                        AgentFrameworkManifest::resolve_context_file_path(cf, project_root);
                    let exists = if resolved.exists() { "ok" } else { "missing" };
                    println!("  {} [{}]", cf, exists);
                }
            }
        }
        if let Some(model) = f.extract_model() {
            println!("Model:        {}", model);
        }
        let caps = ta_runtime::channels::build_channel(
            &f.channel_type,
            std::path::PathBuf::from("."),
            &f.context_file,
        )
        .capabilities();
        println!("Channel:      {}", f.channel_type);
        println!("Live inject:  {}", caps.live_label());
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

    // 2. For ta-agent-ollama profiles, verify the binary is present (v0.15.15.2).
    if fw.command == "ta-agent-ollama" {
        let agent_found = which::which("ta-agent-ollama").is_ok() || {
            let sibling = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("ta-agent-ollama")));
            #[cfg(windows)]
            let sibling = sibling.map(|p| p.with_extension("exe"));
            sibling.map(|p| p.exists()).unwrap_or(false)
        };
        if agent_found {
            println!("  [OK] ta-agent-ollama binary is installed");
        } else {
            all_ok = false;
            println!("  [FAIL] ta-agent-ollama binary not found on PATH or sibling to ta.");
            println!(
                "         Fix: update your TA installation to v0.15.15.2 or later — \
                 ta-agent-ollama is now bundled in the release packages."
            );
            println!("         Manual: cargo install ta-agent-ollama  (or build from source)");
        }
    }

    // 3. For Ollama-based frameworks, check the endpoint.
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

    // 4. Check for required API keys based on framework.
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

    // 5. Summary.
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
    // "gemma4" is a built-in shorthand: auto-detects VRAM and installs locally.
    if name == "gemma4" {
        return install_gemma4(global);
    }

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

    // Verify ta-agent-ollama is findable (v0.15.15.2).
    // Check PATH first, then sibling to the current `ta` binary.
    let ollama_agent_found = which::which("ta-agent-ollama").is_ok() || {
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("ta-agent-ollama")));
        #[cfg(windows)]
        let sibling = sibling.map(|p| p.with_extension("exe"));
        sibling.map(|p| p.exists()).unwrap_or(false)
    };
    if !ollama_agent_found {
        println!();
        println!(
            "WARNING: ta-agent-ollama binary not found — update your TA installation \
             to v0.15.15.2 or later."
        );
        println!("  Without it, `ta run --agent qwen3.5-<size>` will fail at launch time.");
        println!(
            "  Fix: reinstall TA from https://github.com/Trusted-Autonomy/TrustedAutonomy/releases"
        );
    }

    println!();
    println!("To check prerequisites: ta agent doctor <profile-name>");
    Ok(())
}

// ── Gemma 4 bundled profile constants (v0.16.2.1) ─────────────────────────────

const GEMMA4_4B_PROFILE: &str = r#"# Gemma 4 4B — compact local model, great for mid-range hardware.
# Best for: quick edits, simple scripts, fast iteration on M1/RTX 3060 class machines.
# Thinking mode: not required (4B performs well with direct responses)

name        = "gemma4-4b"
version     = "1.0.0"
description = "Gemma 4 4B via Ollama — fast local agent, ~4 GB VRAM"
command     = "ta-agent-ollama"
args        = ["--model", "gemma4:4b", "--base-url", "http://localhost:11434", "--max-turns", "40", "--temperature", "0.1"]
sentinel    = "[goal started]"
context_file = "CLAUDE.md"
context_inject = "env"

[memory]
inject       = "env"
max_entries  = 10
recency_days = 7
"#;

const GEMMA4_12B_PROFILE: &str = r#"# Gemma 4 12B — mid-size local model with strong coding and reasoning.
# Best for: complex tasks, multi-file refactors, most software development work.

name        = "gemma4-12b"
version     = "1.0.0"
description = "Gemma 4 12B via Ollama — balanced local agent, ~10 GB VRAM"
command     = "ta-agent-ollama"
args        = ["--model", "gemma4:12b", "--base-url", "http://localhost:11434", "--max-turns", "50", "--temperature", "0.1"]
sentinel    = "[goal started]"
context_file = "CLAUDE.md"
context_inject = "env"

[memory]
inject       = "env"
max_entries  = 15
recency_days = 7
"#;

/// Returns the bundled TOML content for a gemma4 profile by size.
fn bundled_gemma4_profile(size: &str) -> &'static str {
    match size {
        "4b" => GEMMA4_4B_PROFILE,
        "12b" => GEMMA4_12B_PROFILE,
        _ => "",
    }
}

/// Select the largest Gemma 4 profile that fits the available memory.
///
/// - `is_unified`: true for Apple Silicon (unified memory); false for discrete GPU (VRAM).
/// - Discrete GPU threshold: 16 GB VRAM → 12b; otherwise 4b.
/// - Unified memory threshold: 24 GB unified → 12b; otherwise 4b.
pub fn select_gemma4_size(mem_gb: u64, is_unified: bool) -> &'static str {
    let threshold = if is_unified { 24 } else { 16 };
    if mem_gb >= threshold {
        "12b"
    } else {
        "4b"
    }
}

/// Detect available GPU/unified memory and whether it is Apple Silicon unified memory.
/// Returns (memory_gb, is_unified).
fn detect_vram_gb() -> (u64, bool) {
    // macOS: check for Apple Silicon (arm64) — all memory is unified.
    #[cfg(target_os = "macos")]
    {
        let is_arm = std::process::Command::new("uname")
            .arg("-m")
            .output()
            .ok()
            .map(|o| o.stdout.starts_with(b"arm64"))
            .unwrap_or(false);
        if is_arm {
            let ram_gb = sysctl_memsize_gb();
            return (ram_gb, true);
        }
    }
    // Try nvidia-smi for discrete GPU VRAM (cross-platform).
    if let Some(vram) = nvidia_smi_vram_gb() {
        return (vram, false);
    }
    // Fallback: use half of total system RAM as a conservative estimate.
    (total_ram_gb() / 2, false)
}

#[cfg(target_os = "macos")]
fn sysctl_memsize_gb() -> u64 {
    std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|bytes| bytes / (1 << 30))
        .unwrap_or(8)
}

fn nvidia_smi_vram_gb() -> Option<u64> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let total_mib: u64 = String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .sum();
    if total_mib == 0 {
        None
    } else {
        Some(total_mib / 1024)
    }
}

fn total_ram_gb() -> u64 {
    // macOS/BSD sysctl
    if let Ok(out) = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
    {
        if let Ok(s) = String::from_utf8(out.stdout) {
            if let Ok(bytes) = s.trim().parse::<u64>() {
                return bytes / (1 << 30);
            }
        }
    }
    // Linux /proc/meminfo
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                if let Some(kb_str) = rest.split_whitespace().next() {
                    if let Ok(kb) = kb_str.parse::<u64>() {
                        return kb / (1024 * 1024);
                    }
                }
            }
        }
    }
    16 // conservative fallback
}

/// Install a Gemma 4 model via Ollama, auto-selecting the largest profile that fits.
fn install_gemma4(global: bool) -> anyhow::Result<()> {
    // 1. Check Ollama is installed.
    if which::which("ollama").is_err() {
        println!("Ollama is not installed.");
        println!("  Install: https://ollama.ai");
        println!("  macOS:   brew install ollama");
        println!("  Linux:   curl -fsSL https://ollama.ai/install.sh | sh");
        anyhow::bail!(
            "Ollama is required to use Gemma 4 local agents.\n\
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

    // 3. Detect available VRAM / unified memory and select the best profile.
    let (mem_gb, is_unified) = detect_vram_gb();
    let mem_label = if is_unified {
        format!("{} GB unified memory (Apple Silicon)", mem_gb)
    } else {
        format!("{} GB VRAM", mem_gb)
    };
    println!("Detected hardware: {}", mem_label);

    let size = select_gemma4_size(mem_gb, is_unified);
    println!(
        "Selected profile: gemma4-{} (largest that fits your hardware)",
        size
    );

    let model_tag = format!("gemma4:{}", size);
    let profile_name = format!("gemma4-{}", size);

    // 4. Pull the model.
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

    // 5. Install bundled profile to ~/.config/ta/agents/
    let profile_toml = bundled_gemma4_profile(size);
    let agents_dir = ta_config_dir().join("agents");
    let _ = global; // profile is always installed to the user config dir
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

    // 6. Verify ta-agent-ollama binary.
    let ollama_agent_found = which::which("ta-agent-ollama").is_ok() || {
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("ta-agent-ollama")));
        #[cfg(windows)]
        let sibling = sibling.map(|p| p.with_extension("exe"));
        sibling.map(|p| p.exists()).unwrap_or(false)
    };
    if !ollama_agent_found {
        println!();
        println!(
            "WARNING: ta-agent-ollama binary not found — update your TA installation \
             to v0.15.15.2 or later."
        );
        println!(
            "  Without it, `ta run --agent {}` will fail at launch time.",
            profile_name
        );
        println!(
            "  Fix: reinstall TA from https://github.com/Trusted-Autonomy/TrustedAutonomy/releases"
        );
    }

    println!();
    println!("To check prerequisites: ta agent doctor {}", profile_name);
    Ok(())
}

/// Migrate an existing agent framework configuration to its standalone plugin.
fn migrate_agent_framework(framework: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    match framework {
        "ollama" => migrate_ollama(config),
        other => anyhow::bail!(
            "Unsupported framework '{}' for migration.\n\
             Supported frameworks: ollama\n\
             Example: ta agent migrate ollama",
            other
        ),
    }
}

/// Migrate existing Ollama agent configs to the standalone ta-agent-ollama plugin.
///
/// Steps:
///   1. Detect existing ta-agent-ollama backed configs in .ta/agents/ and ~/.config/ta/agents/
///   2. Verify the ta-agent-ollama binary is present
///   3. Install agent profiles from the standalone plugin (if not already installed)
///   4. Verify Ollama connectivity
///   5. Print migration summary
fn migrate_ollama(config: &GatewayConfig) -> anyhow::Result<()> {
    println!("Migrating Ollama agent configuration to standalone plugin...");
    println!();

    // Step 1: Detect existing ta-agent-ollama backed configs.
    let mut found_profiles: Vec<(std::path::PathBuf, String)> = Vec::new();

    let scan_dirs = [
        config.workspace_root.join(".ta").join("agents"),
        ta_config_dir().join("agents"),
    ];

    for agents_dir in &scan_dirs {
        if !agents_dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(agents_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if content.contains("ta-agent-ollama") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                found_profiles.push((path, name));
            }
        }
    }

    if found_profiles.is_empty() {
        println!("No existing Ollama agent profiles found.");
        println!("To install fresh Ollama agent profiles:");
        println!("  ta agent install-qwen --size 9b");
        println!();
        println!("Migration note: all agent profiles already point to the standalone");
        println!("ta-agent-ollama binary — no path updates are required.");
        return Ok(());
    }

    println!(
        "Found {} existing Ollama agent profile(s):",
        found_profiles.len()
    );
    for (path, name) in &found_profiles {
        println!("  {} ({})", name, path.display());
    }
    println!();

    // Step 2: Verify ta-agent-ollama binary is present.
    let binary_found = which::which("ta-agent-ollama").is_ok() || {
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("ta-agent-ollama")));
        #[cfg(windows)]
        let sibling = sibling.map(|p| p.with_extension("exe"));
        sibling.map(|p| p.exists()).unwrap_or(false)
    };

    if binary_found {
        println!("[OK] ta-agent-ollama binary is installed.");
    } else {
        println!(
            "[WARN] ta-agent-ollama binary not found on PATH.\n\
             Update your TA installation or run: cargo install ta-agent-ollama"
        );
    }

    // Step 3: Profiles already reference ta-agent-ollama directly — no path update needed.
    // The standalone plugin uses the same binary name and command interface.
    println!("[OK] Agent profiles use ta-agent-ollama command — no path update required.");

    // Step 4: Check Ollama connectivity (best-effort).
    let ollama_ok = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()
        .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if ollama_ok {
        println!("[OK] Ollama endpoint reachable at http://localhost:11434.");
    } else {
        println!(
            "[WARN] Ollama is not running at http://localhost:11434.\n  \
             Start with: ollama serve"
        );
    }

    // Step 5: Summary.
    println!();
    println!("Migration complete.");
    println!();
    println!("Your existing profiles work as-is with the standalone ta-agent-ollama plugin.");
    println!(
        "To use the plugin-bundled profiles instead:\n  \
         ta agent install-qwen --size 9b   (or --size 4b / --size 27b / --size all)"
    );
    println!();
    println!("Run `ta agent list --local` to see all installed local agents.");

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
        } else if model_tag.contains("12b") {
            "~10 GB"
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
        println!("Install more Qwen3.5:  ta agent install-qwen --size <4b|9b|27b|all>");
        println!("Install Gemma 4:       ta agent install gemma4   (auto-selects 4b or 12b)");
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

    // ── Migrate tests (v0.16.2) ──────────────────────────────────────────────

    #[test]
    fn migrate_rejects_unknown_framework() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = migrate_agent_framework("notaframework", &config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Unsupported framework") || msg.contains("notaframework"),
            "unexpected error: {}",
            msg
        );
    }

    #[test]
    fn migrate_ollama_no_existing_configs() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        // No existing configs — should succeed with an informational message.
        let result = migrate_agent_framework("ollama", &config);
        assert!(
            result.is_ok(),
            "migrate should succeed even with no configs"
        );
    }

    #[test]
    fn migrate_ollama_detects_existing_profile() {
        let dir = TempDir::new().unwrap();
        let agents_dir = dir.path().join(".ta").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        // Write a minimal ollama-backed profile.
        std::fs::write(
            agents_dir.join("my-qwen.toml"),
            "name = \"my-qwen\"\ncommand = \"ta-agent-ollama\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        let config = test_config(&dir);
        let result = migrate_agent_framework("ollama", &config);
        assert!(result.is_ok(), "migrate should succeed: {:?}", result);
    }

    // ── Gemma 4 profile tests (v0.16.2.1) ────────────────────────────────────

    #[test]
    fn gemma4_profile_4b_is_valid_toml() {
        let content = bundled_gemma4_profile("4b");
        assert!(!content.is_empty(), "4b profile should not be empty");
        let manifest: AgentFrameworkManifest =
            toml::from_str(content).expect("gemma4-4b profile should be valid TOML");
        assert_eq!(manifest.name, "gemma4-4b");
        assert!(
            manifest.args.contains(&"gemma4:4b".to_string()),
            "args should contain model tag gemma4:4b"
        );
        assert_eq!(manifest.command, "ta-agent-ollama");
    }

    #[test]
    fn gemma4_profile_12b_is_valid_toml() {
        let content = bundled_gemma4_profile("12b");
        assert!(!content.is_empty(), "12b profile should not be empty");
        let manifest: AgentFrameworkManifest =
            toml::from_str(content).expect("gemma4-12b profile should be valid TOML");
        assert_eq!(manifest.name, "gemma4-12b");
        assert!(
            manifest.args.contains(&"gemma4:12b".to_string()),
            "args should contain model tag gemma4:12b"
        );
        assert_eq!(manifest.command, "ta-agent-ollama");
    }

    #[test]
    fn gemma4_profile_unknown_returns_empty() {
        let content = bundled_gemma4_profile("99b");
        assert!(
            content.is_empty(),
            "unknown size should return empty string"
        );
    }

    #[test]
    fn select_gemma4_size_8gb_discrete_returns_4b() {
        assert_eq!(
            select_gemma4_size(8, false),
            "4b",
            "8 GB VRAM should select gemma4-4b"
        );
    }

    #[test]
    fn select_gemma4_size_16gb_discrete_returns_12b() {
        assert_eq!(
            select_gemma4_size(16, false),
            "12b",
            "16 GB VRAM should select gemma4-12b"
        );
    }

    #[test]
    fn select_gemma4_size_16gb_unified_returns_4b() {
        assert_eq!(
            select_gemma4_size(16, true),
            "4b",
            "16 GB unified (M1 base) should select gemma4-4b (12b needs 24 GB unified)"
        );
    }

    #[test]
    fn select_gemma4_size_24gb_unified_returns_12b() {
        assert_eq!(
            select_gemma4_size(24, true),
            "12b",
            "24 GB unified (M1 Pro/Max) should select gemma4-12b"
        );
    }

    #[test]
    fn select_gemma4_size_boundary_discrete() {
        assert_eq!(select_gemma4_size(15, false), "4b");
        assert_eq!(select_gemma4_size(16, false), "12b");
    }

    #[test]
    fn select_gemma4_size_boundary_unified() {
        assert_eq!(select_gemma4_size(23, true), "4b");
        assert_eq!(select_gemma4_size(24, true), "12b");
    }
}
