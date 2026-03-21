// init.rs — Project initialization and template bootstrapping (v0.7.3).
//
// `ta init` creates a new TA-managed project from a template. It detects the
// project type, generates appropriate .ta/ configuration, and outputs everything
// as a reviewable draft.
//
// Templates are bundled in the binary. Users can also use `--template <name>`.

use std::path::Path;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_memory::key_schema::{detect_project_type, ProjectType};

#[derive(Subcommand, Debug)]
pub enum InitCommands {
    /// Initialize a new TA-managed project in the current directory.
    Run {
        /// Use a specific template: rust-workspace, typescript-monorepo, python-ml, go-service, generic.
        #[arg(long)]
        template: Option<String>,
        /// Auto-detect project type and generate config (default behavior).
        #[arg(long)]
        detect: bool,
        /// Project name (defaults to directory name).
        #[arg(long)]
        name: Option<String>,
    },
    /// List available project templates.
    Templates,
}

pub fn execute(command: &InitCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        InitCommands::Run {
            template,
            detect: _,
            name,
        } => run_init(config, template.as_deref(), name.as_deref()),
        InitCommands::Templates => list_templates(),
    }
}

/// Available templates.
const TEMPLATES: &[(&str, &str)] = &[
    (
        "rust-workspace",
        "Rust workspace with crate-based module structure",
    ),
    (
        "typescript-monorepo",
        "TypeScript monorepo with package-based structure",
    ),
    (
        "python-ml",
        "Python ML project with notebook and model management",
    ),
    ("go-service", "Go microservice with module-based structure"),
    (
        "unreal-cpp",
        "Unreal Engine C++ project (requires BMAD + Claude Flow)",
    ),
    (
        "unity-csharp",
        "Unity C# project (requires BMAD + Claude Flow)",
    ),
    ("generic", "Generic project with minimal defaults"),
];

fn list_templates() -> anyhow::Result<()> {
    println!("Available project templates:\n");
    for (name, desc) in TEMPLATES {
        println!("  {:<22} {}", name, desc);
    }
    println!();
    println!("Usage: ta init run --template <name>");
    println!("       ta init run --detect      (auto-detect project type)");
    println!();
    println!("Game engine templates require prerequisites:");
    println!("  claude     — Claude Code CLI (claude.ai/code)");
    println!("  claude-flow — npm install -g @ruvnet/claude-flow");
    println!("  BMAD        — git clone https://github.com/bmad-code-org/BMAD-METHOD ~/.bmad");
    Ok(())
}

fn run_init(
    config: &GatewayConfig,
    template_name: Option<&str>,
    project_name: Option<&str>,
) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let ta_dir = project_root.join(".ta");

    // Check if already initialized.
    if ta_dir.exists() {
        let has_config = ta_dir.join("workflow.toml").exists()
            || ta_dir.join("policy.yaml").exists()
            || ta_dir.join("memory.toml").exists();
        if has_config {
            println!("Project already has TA configuration in .ta/");
            println!("Use `ta setup refine <topic>` to update specific config.");
            return Ok(());
        }
    }

    // Resolve project type.
    let project_type = if let Some(tmpl) = template_name {
        match tmpl {
            "rust-workspace" => ProjectType::RustWorkspace,
            "typescript-monorepo" | "typescript" => ProjectType::TypeScript,
            "python-ml" | "python" => ProjectType::Python,
            "go-service" | "go" => ProjectType::Go,
            "unreal-cpp" | "unreal" => ProjectType::UnrealCpp,
            "unity-csharp" | "unity" => ProjectType::UnityCsharp,
            "generic" => ProjectType::Generic,
            _ => {
                anyhow::bail!(
                    "Unknown template: '{}'. Run `ta init templates` for available options.",
                    tmpl
                );
            }
        }
    } else {
        detect_project_type(project_root)
    };

    let name = project_name.unwrap_or_else(|| {
        project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
    });

    println!("Initializing TA project: {}", name);
    println!("Project type: {}", project_type);
    println!();

    // Generate all config files from template.
    std::fs::create_dir_all(&ta_dir)?;

    generate_workflow_toml(&ta_dir, &project_type)?;
    generate_memory_toml(&ta_dir, &project_type)?;
    generate_policy_yaml(&ta_dir, &project_type)?;
    generate_taignore(project_root, &project_type)?;
    generate_agent_configs(&ta_dir)?;
    generate_constitutions(&ta_dir)?;
    seed_memory_entries(&ta_dir, &project_type, project_root)?;

    // Game engine templates: additional files.
    let is_game_engine = matches!(
        project_type,
        ProjectType::UnrealCpp | ProjectType::UnityCsharp
    );
    if is_game_engine {
        generate_bmad_toml(&ta_dir)?;
        generate_bmad_agent_configs(&ta_dir)?;
        generate_mcp_json(project_root)?;
        generate_onboarding_goal(&ta_dir, &project_type, name)?;
    }

    println!();
    println!("TA project initialized successfully!");
    println!();
    println!("Generated files:");
    println!("  .ta/workflow.toml      — workflow configuration");
    println!("  .ta/memory.toml        — memory key schema and backend");
    println!("  .ta/policy.yaml        — security policy");
    println!("  .ta/agents/            — agent configurations");
    println!("  .ta/constitutions/     — starter constitutions");
    println!("  .taignore              — file exclusion patterns");
    if is_game_engine {
        println!("  .ta/bmad.toml          — BMAD machine-local install config");
        println!("  .ta/agents/bmad-*.toml — BMAD role agent configs");
        println!("  .mcp.json              — Claude Flow MCP server config");
        println!("  .ta/onboarding-goal.md — First-run discovery goal");
    }
    println!();
    println!("Next steps:");
    if is_game_engine {
        println!("  Ensure prerequisites are installed:");
        println!("    claude     — Claude Code CLI");
        println!("    claude-flow — npm install -g @ruvnet/claude-flow");
        println!(
            "    BMAD        — git clone https://github.com/bmad-code-org/BMAD-METHOD ~/.bmad"
        );
        println!();
        println!("  Then run the discovery goal:");
        println!("  ta run --objective-file .ta/onboarding-goal.md");
    } else {
        println!("  ta setup show          — inspect configuration");
        println!("  ta setup refine <topic> — adjust specific settings");
        println!("  ta run \"first goal\"     — start your first TA-mediated goal");
    }

    Ok(())
}

fn generate_workflow_toml(ta_dir: &Path, project_type: &ProjectType) -> anyhow::Result<()> {
    let path = ta_dir.join("workflow.toml");

    let verify_section = match project_type {
        ProjectType::RustWorkspace => {
            r#"
# Pre-draft verification gate (v0.10.8)
# Commands run after agent exits, before draft is created.
# All must pass (exit 0) for the draft to be created.
[verify]
commands = [
    "cargo build --workspace",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
]
# on_failure: "block" (no draft), "warn" (draft with warnings), "agent" (re-launch agent)
on_failure = "block"
# Timeout per command in seconds
timeout = 300
"#
        }
        _ => {
            r#"
# Pre-draft verification gate (v0.10.8)
# Commands run after agent exits, before draft is created.
# Uncomment and customize for your project:
# [verify]
# commands = ["make test", "make lint"]
# on_failure = "block"
# timeout = 300
"#
        }
    };

    let content = format!(
        r#"# TA Workflow Configuration
# Generated by `ta init`

[memory.auto_capture]
on_goal_complete = true
on_draft_reject = true
on_human_guidance = true
on_repeated_correction = true
correction_threshold = 3
max_context_entries = 10
{}"#,
        verify_section
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/workflow.toml");
    Ok(())
}

fn generate_memory_toml(ta_dir: &Path, project_type: &ProjectType) -> anyhow::Result<()> {
    let path = ta_dir.join("memory.toml");
    let schema = ta_memory::key_schema::KeyDomainMap::for_project_type(project_type);
    let content = format!(
        r#"# TA Memory Configuration
# Generated by `ta init` for {} project

backend = "ruvector"

[project]
type = "{}"

[key_domains]
module_map = "{}"
module = "{}"
type_system = "{}"
build_tool = "{}"
"#,
        project_type,
        project_type,
        schema.module_map,
        schema.module,
        schema.type_system,
        schema.build_tool,
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/memory.toml");
    Ok(())
}

fn generate_policy_yaml(ta_dir: &Path, project_type: &ProjectType) -> anyhow::Result<()> {
    let path = ta_dir.join("policy.yaml");
    let security_level = match project_type {
        ProjectType::Generic => "open",
        _ => "checkpoint",
    };

    let protected_paths = match project_type {
        ProjectType::UnrealCpp => {
            "\nprotected_paths:\n  - \"Config/DefaultEngine.ini\"\n  - \"*.uproject\"\n  - \"Source/**/*.Build.cs\"\n"
        }
        ProjectType::UnityCsharp => {
            "\nprotected_paths:\n  - \"ProjectSettings/**\"\n  - \"**/*.asmdef\"\n"
        }
        _ => "",
    };

    let content = format!(
        r#"# TA Policy Configuration
# Generated by `ta init` for {} project

security_level: {}

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
{}"#,
        project_type, security_level, protected_paths,
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/policy.yaml");
    Ok(())
}

fn generate_taignore(project_root: &Path, project_type: &ProjectType) -> anyhow::Result<()> {
    let path = project_root.join(".taignore");
    if path.exists() {
        return Ok(());
    }

    let common = "# TA staging and internal files\n.ta/staging/\n.ta/sessions/\n.ta/memory/\n\n";
    let language_specific = match project_type {
        ProjectType::RustWorkspace => "# Rust\ntarget/\n*.rs.bk\n",
        ProjectType::TypeScript => {
            "# TypeScript/JavaScript\nnode_modules/\ndist/\nbuild/\n.next/\n"
        }
        ProjectType::Python => "# Python\n__pycache__/\n*.pyc\n.venv/\nvenv/\n*.egg-info/\n",
        ProjectType::Go => "# Go\nvendor/\n",
        ProjectType::UnrealCpp => {
            "# Unreal Engine\nBinaries/\nIntermediate/\nSaved/\nDerivedDataCache/\n*.generated.h\n"
        }
        ProjectType::UnityCsharp => "# Unity\nLibrary/\nTemp/\nobj/\n*.csproj.user\n",
        ProjectType::Generic => "",
    };

    let content = format!(
        "# .taignore — Files excluded from TA draft staging\n# Generated by `ta init`\n\n{}{}\n# IDE and OS\n.idea/\n.vscode/\n*.swp\n.DS_Store\n",
        common, language_specific,
    );
    std::fs::write(&path, content)?;
    println!("  Created .taignore");
    Ok(())
}

fn generate_agent_configs(ta_dir: &Path) -> anyhow::Result<()> {
    let agents_dir = ta_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    // Use the framework registry to detect installed frameworks and
    // generate configs for them.
    let project_root = ta_dir.parent();
    let registry = crate::framework_registry::FrameworkRegistry::load(project_root);
    let installed = registry.detect_installed();

    if installed.is_empty() {
        // Fall back to default claude-code config.
        generate_single_agent_config(&agents_dir, "claude-code")?;
    } else {
        for (id, entry) in &installed {
            generate_single_agent_config(&agents_dir, id)?;
            println!(
                "    Detected: {} ({})",
                entry.name,
                entry.description.as_deref().unwrap_or("")
            );
        }
    }

    // Show available-but-not-installed frameworks.
    let available: Vec<_> = registry
        .detect_available()
        .into_iter()
        .filter(|(_, e)| !e.detect.is_empty()) // Only show detectable ones
        .collect();
    if !available.is_empty() {
        println!("  Available frameworks (not installed):");
        for (id, entry) in &available {
            let install_cmd = entry
                .install
                .as_ref()
                .map(|i| i.for_current_platform().to_string())
                .unwrap_or_else(|| "See framework homepage".to_string());
            println!("    {} — {}", id, install_cmd);
        }
    }

    Ok(())
}

/// Generate a single agent config YAML file if it doesn't already exist.
fn generate_single_agent_config(agents_dir: &Path, agent_id: &str) -> anyhow::Result<()> {
    let path = agents_dir.join(format!("{}.yaml", agent_id));
    if path.exists() {
        return Ok(());
    }

    // Try to copy from shipped agent configs (binary dir or known templates).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let shipped = bin_dir.join("agents").join(format!("{}.yaml", agent_id));
            if shipped.exists() {
                std::fs::copy(&shipped, &path)?;
                println!(
                    "  Created .ta/agents/{}.yaml (from shipped config)",
                    agent_id
                );
                return Ok(());
            }
        }
    }

    // Generate a minimal config from the generic template.
    let injects = agent_id == "claude-code" || agent_id == "bmad";
    let cmd = agent_id.split('-').next().unwrap_or(agent_id);
    let content = format!(
        "# Agent Configuration: {}\n# Generated by `ta init`\n\nname: {}\ncommand: {}\nargs_template:\n  - \"{{prompt}}\"\ninjects_context_file: {}\ninjects_settings: {}\n",
        agent_id, agent_id, cmd, injects, injects,
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/agents/{}.yaml", agent_id);
    Ok(())
}

fn generate_constitutions(ta_dir: &Path) -> anyhow::Result<()> {
    let const_dir = ta_dir.join("constitutions");
    std::fs::create_dir_all(&const_dir)?;

    let default_path = const_dir.join("default.yaml");
    if !default_path.exists() {
        let content = r#"# Default Constitution
# Generated by `ta init`
# Constitutions define behavioral boundaries for agents.

name: default
description: "Standard project development constitution"

rules:
  - "Do not modify files outside the project directory"
  - "Run tests before submitting drafts"
  - "Follow existing code style and patterns"
  - "Do not introduce new dependencies without justification"
  - "Keep changes focused on the stated objective"
"#;
        std::fs::write(&default_path, content)?;
    }
    println!("  Created .ta/constitutions/default.yaml");
    Ok(())
}

/// Seed initial memory entries based on project type and structure.
fn seed_memory_entries(
    ta_dir: &Path,
    project_type: &ProjectType,
    project_root: &Path,
) -> anyhow::Result<()> {
    let memory_dir = ta_dir.join("memory");
    std::fs::create_dir_all(&memory_dir)?;

    // Detect project structure and seed appropriate entries.
    let mut seeds: Vec<(String, serde_json::Value)> = Vec::new();

    match project_type {
        ProjectType::RustWorkspace => {
            // Parse Cargo.toml for workspace members.
            let cargo_toml = project_root.join("Cargo.toml");
            if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
                let members = extract_workspace_members(&content);
                if !members.is_empty() {
                    seeds.push((
                        "arch:crate-map".into(),
                        serde_json::json!({
                            "crates": members,
                            "source": "auto-detected from Cargo.toml"
                        }),
                    ));
                }
            }
        }
        ProjectType::TypeScript => {
            let pkg = project_root.join("package.json");
            if let Ok(content) = std::fs::read_to_string(&pkg) {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(name) = parsed.get("name").and_then(|n| n.as_str()) {
                        seeds.push((
                            "arch:package-map".into(),
                            serde_json::json!({
                                "name": name,
                                "source": "auto-detected from package.json"
                            }),
                        ));
                    }
                }
            }
        }
        ProjectType::UnrealCpp => {
            seeds.push((
                "conv:ue5-object-ownership".into(),
                serde_json::json!({
                    "convention": "Use TObjectPtr<T> for UPROPERTY references (UE5+). Raw pointers for non-UPROPERTY temporaries only.",
                    "source": "UE5 coding conventions"
                }),
            ));
            seeds.push((
                "conv:ue5-game-thread".into(),
                serde_json::json!({
                    "convention": "All UObject operations must occur on the game thread. Use AsyncTask(ENamedThreads::GameThread, ...) to marshal back from worker threads.",
                    "source": "UE5 threading rules"
                }),
            ));
            seeds.push((
                "conv:ue5-uproperties".into(),
                serde_json::json!({
                    "convention": "Expose editable properties with UPROPERTY(EditAnywhere, BlueprintReadWrite, Category=...). Use UFUNCTION(BlueprintCallable) for Blueprint-accessible methods.",
                    "source": "UE5 reflection system"
                }),
            ));
        }
        ProjectType::UnityCsharp => {
            seeds.push((
                "conv:unity-monobehaviour-lifecycle".into(),
                serde_json::json!({
                    "convention": "MonoBehaviour lifecycle order: Awake → OnEnable → Start → Update/FixedUpdate → OnDisable → OnDestroy. Awake initialises refs; Start accesses other components.",
                    "source": "Unity scripting reference"
                }),
            ));
            seeds.push((
                "conv:unity-coroutine-vs-jobs".into(),
                serde_json::json!({
                    "convention": "Use Coroutines (IEnumerator) for time-sliced game logic. Use Unity Job System + Burst for CPU-intensive parallel work (physics, pathfinding). Avoid async/await on the main thread without UniTask.",
                    "source": "Unity performance best practices"
                }),
            ));
        }
        _ => {}
    }

    // Write seeds as JSON files.
    for (key, value) in &seeds {
        let filename = key.replace([':', '/'], "_");
        let path = memory_dir.join(format!("{}.json", filename));
        let entry = serde_json::json!({
            "key": key,
            "value": value,
            "tags": ["init-seed", "auto-detected"],
            "source": "ta-init",
            "created_at": chrono::Utc::now().to_rfc3339(),
        });
        std::fs::write(&path, serde_json::to_string_pretty(&entry)?)?;
    }

    if !seeds.is_empty() {
        println!("  Seeded {} memory entries", seeds.len());
    }

    Ok(())
}

/// Generate `.ta/bmad.toml` — machine-local BMAD install config.
///
/// BMAD personas are cloned to `~/.bmad` (or `%USERPROFILE%\.bmad` on Windows).
/// The project itself stays clean — no BMAD files are committed to VCS.
fn generate_bmad_toml(ta_dir: &Path) -> anyhow::Result<()> {
    let path = ta_dir.join("bmad.toml");
    if path.exists() {
        return Ok(());
    }

    let default_home = if cfg!(target_os = "windows") {
        "%USERPROFILE%\\.bmad"
    } else {
        "~/.bmad"
    };

    let content = format!(
        r#"# BMAD machine-local configuration
# Generated by `ta init --template unreal-cpp/unity-csharp`
#
# BMAD is installed machine-locally (not in the project tree).
# Override with the TA_BMAD_HOME environment variable.

# Path to your local BMAD clone.
# Install: git clone https://github.com/bmad-code-org/BMAD-METHOD {default}
bmad_home = "{default}"

# Agent persona directory within bmad_home.
agents_dir = "agents"
"#,
        default = default_home,
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/bmad.toml");
    Ok(())
}

/// Generate BMAD role agent configs under `.ta/agents/`.
///
/// Each file points to a persona prompt in `${bmad_home}/agents/`.
/// The four standard roles: PM, Architect, Dev, QA.
fn generate_bmad_agent_configs(ta_dir: &Path) -> anyhow::Result<()> {
    let agents_dir = ta_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    let roles = [
        ("bmad-pm", "Product Manager", "pm"),
        ("bmad-architect", "Software Architect", "architect"),
        ("bmad-dev", "Developer", "dev"),
        ("bmad-qa", "QA Engineer", "qa"),
    ];

    for (id, role_name, persona) in &roles {
        let path = agents_dir.join(format!("{}.toml", id));
        if path.exists() {
            continue;
        }
        let content = format!(
            r#"# BMAD agent config: {role}
# Generated by `ta init`
#
# The persona prompt is loaded from ${{bmad_home}}/agents/{persona}.md at runtime.
# bmad_home is defined in .ta/bmad.toml (or TA_BMAD_HOME env var).

name = "{id}"
role = "{role}"
persona_file = "${{bmad_home}}/agents/{persona}.md"

# Claude Code is the underlying runtime.
command = "claude"
injects_context_file = true
injects_settings = true
"#,
            role = role_name,
            id = id,
            persona = persona,
        );
        std::fs::write(&path, content)?;
        println!("  Created .ta/agents/{}.toml", id);
    }

    Ok(())
}

/// Generate project-root `.mcp.json` for Claude Flow + TA MCP server entries.
fn generate_mcp_json(project_root: &Path) -> anyhow::Result<()> {
    let path = project_root.join(".mcp.json");
    if path.exists() {
        return Ok(());
    }

    let content = r#"{
  "mcpServers": {
    "ta": {
      "command": "ta",
      "args": ["mcp", "serve"],
      "description": "Trusted Autonomy governance server — staging, draft review, audit trail, policy"
    },
    "claude-flow": {
      "command": "claude-flow",
      "args": ["mcp", "serve"],
      "description": "Claude Flow swarm coordination — parallel agent tasks across module boundaries. Install: npm install -g @ruvnet/claude-flow"
    }
  }
}
"#;
    std::fs::write(&path, content)?;
    println!("  Created .mcp.json");
    Ok(())
}

/// Generate `.ta/onboarding-goal.md` — the first TA goal to run on a game project.
///
/// Describes a structured discovery goal: survey the project, produce architecture doc,
/// PRD, and sprint-1 stories using BMAD roles.
fn generate_onboarding_goal(
    ta_dir: &Path,
    project_type: &ProjectType,
    project_name: &str,
) -> anyhow::Result<()> {
    let path = ta_dir.join("onboarding-goal.md");
    if path.exists() {
        return Ok(());
    }

    let (engine, lang, source_ext) = match project_type {
        ProjectType::UnrealCpp => ("Unreal Engine", "C++", "*.cpp / *.h"),
        ProjectType::UnityCsharp => ("Unity", "C#", "*.cs"),
        _ => ("Game Engine", "code", "*"),
    };

    let content = format!(
        r#"# Onboarding Goal: {project} Discovery
#
# Run with: ta run --objective-file .ta/onboarding-goal.md
#           (title is derived from the first heading above)
#
# Prerequisites (must be installed before running):
#   - claude   : Claude Code CLI  (claude.ai/code)
#   - claude-flow : npm install -g @ruvnet/claude-flow
#   - BMAD     : git clone https://github.com/bmad-code-org/BMAD-METHOD ~/.bmad
#                (or set TA_BMAD_HOME to your clone path)
#
# What this goal produces:
#   docs/architecture.md         — Module map and key systems
#   docs/bmad/prd.md             — Product Requirements Document
#   docs/bmad/stories/sprint-1/  — Sprint-1 user stories

## Goal

Survey the **{project}** {engine} {lang} project and produce structured onboarding artefacts
using BMAD planning roles, Claude Flow swarm coordination, and TA draft governance.

## Scope

1. **Discovery** (bmad-architect role)
   - Walk the source tree ({src_ext}) and identify major modules, subsystems, and dependencies
   - Document current architecture in `docs/architecture.md`
   - Note technical debt, missing tests, and build warnings

2. **PRD** (bmad-pm role)
   - Interview the codebase to infer current feature set and gaps
   - Produce `docs/bmad/prd.md` with Goals, Non-Goals, and Success Metrics

3. **Sprint-1 Stories** (bmad-dev + bmad-qa roles)
   - Decompose the PRD into actionable stories
   - Write to `docs/bmad/stories/sprint-1/`
   - Each story: title, acceptance criteria, implementation notes, test plan

## Constraints

- Do not modify game source files during this discovery goal
- Write only to `docs/` and `.ta/`
- Flag any files that require human review before modification (especially {engine} config files)

## Done when

- [ ] `docs/architecture.md` created and reviewed
- [ ] `docs/bmad/prd.md` created and reviewed
- [ ] At least 3 sprint-1 stories written to `docs/bmad/stories/sprint-1/`
"#,
        project = project_name,
        engine = engine,
        lang = lang,
        src_ext = source_ext,
    );
    std::fs::write(&path, content)?;
    println!("  Created .ta/onboarding-goal.md");
    Ok(())
}

/// Extract workspace members from Cargo.toml content.
fn extract_workspace_members(content: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_members = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("members") && trimmed.contains('[') {
            in_members = true;
            // Check for inline array on same line.
            if let Some(start) = trimmed.find('[') {
                let rest = &trimmed[start + 1..];
                if let Some(end) = rest.find(']') {
                    let items = &rest[..end];
                    for item in items.split(',') {
                        let item = item.trim().trim_matches('"').trim_matches('\'');
                        if !item.is_empty() {
                            members.push(item.to_string());
                        }
                    }
                    in_members = false;
                }
            }
            continue;
        }
        if in_members {
            if trimmed.starts_with(']') {
                in_members = false;
                continue;
            }
            let item = trimmed
                .trim_matches(',')
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if !item.is_empty() && !item.starts_with('#') {
                members.push(item.to_string());
            }
        }
    }

    members
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> GatewayConfig {
        GatewayConfig::for_project(dir.path())
    }

    #[test]
    fn init_rust_project() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\n  \"crates/foo\",\n  \"crates/bar\",\n]\n",
        )
        .unwrap();

        let config = test_config(&dir);
        run_init(&config, None, Some("test-project")).unwrap();

        let ta_dir = dir.path().join(".ta");
        assert!(ta_dir.join("workflow.toml").exists());
        assert!(ta_dir.join("memory.toml").exists());
        assert!(ta_dir.join("policy.yaml").exists());
        assert!(ta_dir.join("agents").join("claude-code.yaml").exists());
        assert!(ta_dir.join("constitutions").join("default.yaml").exists());
        assert!(dir.path().join(".taignore").exists());

        // Check memory seed.
        let memory_dir = ta_dir.join("memory");
        assert!(memory_dir.exists());
    }

    #[test]
    fn init_already_configured() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("workflow.toml"), "# existing\n").unwrap();

        let config = test_config(&dir);
        // Should not error, just skip.
        run_init(&config, None, None).unwrap();
    }

    #[test]
    fn init_with_template() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        run_init(&config, Some("typescript"), Some("my-ts-app")).unwrap();

        let memory = std::fs::read_to_string(dir.path().join(".ta").join("memory.toml")).unwrap();
        assert!(memory.contains("typescript"));
        assert!(memory.contains("package-map"));
    }

    #[test]
    fn init_unknown_template_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = run_init(&config, Some("haskell"), None);
        assert!(result.is_err());
    }

    #[test]
    fn list_templates_succeeds() {
        list_templates().unwrap();
    }

    #[test]
    fn extract_workspace_members_inline() {
        let content = r#"
[workspace]
members = ["crates/foo", "crates/bar"]
"#;
        let members = extract_workspace_members(content);
        assert_eq!(members, vec!["crates/foo", "crates/bar"]);
    }

    #[test]
    fn extract_workspace_members_multiline() {
        let content = r#"
[workspace]
members = [
    "crates/foo",
    "crates/bar",
    "apps/cli",
]
"#;
        let members = extract_workspace_members(content);
        assert_eq!(members, vec!["crates/foo", "crates/bar", "apps/cli"]);
    }

    #[test]
    fn extract_workspace_members_empty() {
        let content = "[package]\nname = \"foo\"\n";
        let members = extract_workspace_members(content);
        assert!(members.is_empty());
    }

    #[test]
    fn taignore_rust_has_target() {
        let dir = TempDir::new().unwrap();
        generate_taignore(dir.path(), &ProjectType::RustWorkspace).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert!(content.contains("target/"));
    }

    #[test]
    fn taignore_typescript_has_node_modules() {
        let dir = TempDir::new().unwrap();
        generate_taignore(dir.path(), &ProjectType::TypeScript).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert!(content.contains("node_modules/"));
    }

    #[test]
    fn taignore_skips_if_exists() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".taignore"), "# custom\n").unwrap();
        generate_taignore(dir.path(), &ProjectType::Generic).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert_eq!(content, "# custom\n");
    }

    #[test]
    fn init_unreal_template() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        run_init(&config, Some("unreal-cpp"), Some("MyGame")).unwrap();

        let ta_dir = dir.path().join(".ta");
        assert!(ta_dir.join("workflow.toml").exists());
        assert!(ta_dir.join("memory.toml").exists());
        assert!(ta_dir.join("policy.yaml").exists());
        assert!(ta_dir.join("bmad.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-pm.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-architect.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-dev.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-qa.toml").exists());
        assert!(dir.path().join(".mcp.json").exists());
        assert!(ta_dir.join("onboarding-goal.md").exists());

        let taignore = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert!(taignore.contains("Binaries/"));
        assert!(taignore.contains("Intermediate/"));
        assert!(taignore.contains("*.generated.h"));

        let policy = std::fs::read_to_string(ta_dir.join("policy.yaml")).unwrap();
        assert!(policy.contains("DefaultEngine.ini"));
        assert!(policy.contains("*.uproject"));

        let memory = std::fs::read_to_string(ta_dir.join("memory.toml")).unwrap();
        assert!(memory.contains("unreal-cpp"));
        assert!(memory.contains("ubt"));

        let goal = std::fs::read_to_string(ta_dir.join("onboarding-goal.md")).unwrap();
        assert!(goal.contains("Unreal Engine"));
        assert!(goal.contains("MyGame"));
    }

    #[test]
    fn init_unity_template() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        run_init(&config, Some("unity-csharp"), Some("MyUnityGame")).unwrap();

        let ta_dir = dir.path().join(".ta");
        assert!(ta_dir.join("bmad.toml").exists());
        assert!(ta_dir.join("agents").join("bmad-pm.toml").exists());
        assert!(dir.path().join(".mcp.json").exists());
        assert!(ta_dir.join("onboarding-goal.md").exists());

        let taignore = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert!(taignore.contains("Library/"));
        assert!(taignore.contains("Temp/"));
        assert!(taignore.contains("*.csproj.user"));

        let policy = std::fs::read_to_string(ta_dir.join("policy.yaml")).unwrap();
        assert!(policy.contains("ProjectSettings/"));
        assert!(policy.contains("*.asmdef"));

        let memory = std::fs::read_to_string(ta_dir.join("memory.toml")).unwrap();
        assert!(memory.contains("unity-csharp"));
        assert!(memory.contains("msbuild"));

        let goal = std::fs::read_to_string(ta_dir.join("onboarding-goal.md")).unwrap();
        assert!(goal.contains("Unity"));
        assert!(goal.contains("MyUnityGame"));
    }

    #[test]
    fn taignore_unreal_has_binaries() {
        let dir = TempDir::new().unwrap();
        generate_taignore(dir.path(), &ProjectType::UnrealCpp).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert!(content.contains("Binaries/"));
        assert!(content.contains("DerivedDataCache/"));
        assert!(content.contains("*.generated.h"));
    }

    #[test]
    fn taignore_unity_has_library() {
        let dir = TempDir::new().unwrap();
        generate_taignore(dir.path(), &ProjectType::UnityCsharp).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".taignore")).unwrap();
        assert!(content.contains("Library/"));
        assert!(content.contains("*.csproj.user"));
    }

    #[test]
    fn bmad_toml_created() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        generate_bmad_toml(&ta_dir).unwrap();
        let content = std::fs::read_to_string(ta_dir.join("bmad.toml")).unwrap();
        assert!(content.contains("bmad_home"));
        assert!(content.contains(".bmad"));
    }

    #[test]
    fn bmad_agent_configs_created() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        generate_bmad_agent_configs(&ta_dir).unwrap();
        for role in &["bmad-pm", "bmad-architect", "bmad-dev", "bmad-qa"] {
            let path = ta_dir.join("agents").join(format!("{}.toml", role));
            assert!(path.exists(), "{} not created", role);
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.contains("persona_file"));
            assert!(content.contains("bmad_home"));
        }
    }

    #[test]
    fn mcp_json_created() {
        let dir = TempDir::new().unwrap();
        generate_mcp_json(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert!(content.contains("claude-flow"));
        assert!(content.contains("\"ta\""));
    }

    #[test]
    fn onboarding_goal_unreal_content() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        generate_onboarding_goal(&ta_dir, &ProjectType::UnrealCpp, "ShooterGame").unwrap();
        let content = std::fs::read_to_string(ta_dir.join("onboarding-goal.md")).unwrap();
        assert!(content.contains("ShooterGame"));
        assert!(content.contains("Unreal Engine"));
        assert!(content.contains("*.cpp / *.h"));
    }

    #[test]
    fn onboarding_goal_unity_content() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        generate_onboarding_goal(&ta_dir, &ProjectType::UnityCsharp, "PlatformerGame").unwrap();
        let content = std::fs::read_to_string(ta_dir.join("onboarding-goal.md")).unwrap();
        assert!(content.contains("PlatformerGame"));
        assert!(content.contains("Unity"));
        assert!(content.contains("*.cs"));
    }
}
