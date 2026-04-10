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
        /// Override VCS backend: git, perforce, or none.
        #[arg(long)]
        vcs: Option<String>,
        /// GitHub remote URL (e.g., github.com/org/repo). When set, run `gh repo create`.
        #[arg(long)]
        remote: Option<String>,
        /// Skip all interactive prompts.
        #[arg(long)]
        non_interactive: bool,
        /// Overwrite an existing CLAUDE.md (default: skip if present).
        #[arg(long)]
        overwrite: bool,
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
            vcs,
            remote,
            non_interactive,
            overwrite,
        } => run_init(
            config,
            template.as_deref(),
            name.as_deref(),
            vcs.as_deref(),
            remote.as_deref(),
            *non_interactive,
            *overwrite,
        ),
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

fn is_interactive() -> bool {
    std::env::var("TERM").is_ok() && std::env::var("TA_NON_INTERACTIVE").is_err()
}

fn prompt(question: &str, default: &str) -> String {
    use std::io::{self, Write};
    print!("{} [{}]: ", question, default);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn prompt_yn(question: &str, default: bool) -> bool {
    let default_str = if default { "Y/n" } else { "y/N" };
    use std::io::{self, Write};
    print!("{} [{}] ", question, default_str);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let trimmed = input.trim().to_lowercase();
    if trimmed.is_empty() {
        default
    } else {
        trimmed.starts_with('y')
    }
}

fn run_init(
    config: &GatewayConfig,
    template_name: Option<&str>,
    project_name: Option<&str>,
    vcs_override: Option<&str>,
    remote_url: Option<&str>,
    non_interactive: bool,
    overwrite: bool,
) -> anyhow::Result<()> {
    let project_root = &config.workspace_root;
    let ta_dir = project_root.join(".ta");

    // Check if already initialized.
    if ta_dir.exists() {
        let has_config = ta_dir.join("workflow.toml").exists()
            || ta_dir.join("policy.yaml").exists()
            || ta_dir.join("memory.toml").exists();
        if has_config {
            // Still generate CLAUDE.md if it's missing (safe to re-run).
            let claude_md_path = project_root.join("CLAUDE.md");
            if !claude_md_path.exists() || overwrite {
                let default_name = project_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("project")
                    .to_string();
                let name = project_name.unwrap_or(&default_name);
                let detected_type = detect_project_type(project_root);
                let project_type = template_name
                    .and_then(|t| parse_template_name(t).ok())
                    .unwrap_or(detected_type);
                write_claude_md(project_root, name, &project_type, overwrite)?;
            } else {
                println!("Project already has TA configuration in .ta/");
                println!("Use `ta setup refine <topic>` to update specific config.");
            }
            return Ok(());
        }
    }

    let interactive = !non_interactive && is_interactive();

    // Derive default project name.
    let default_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string();

    let name_owned: String = if let Some(n) = project_name {
        n.to_string()
    } else if interactive {
        prompt("Project name", &default_name)
    } else {
        default_name.clone()
    };
    let name = name_owned.as_str();

    // Resolve project type.
    let detected_type = detect_project_type(project_root);
    let project_type = if let Some(tmpl) = template_name {
        parse_template_name(tmpl)?
    } else if interactive {
        let type_str = prompt(
            "Template (rust-workspace/typescript-monorepo/python-ml/go-service/generic)",
            &detected_type.to_string(),
        );
        match type_str.as_str() {
            "rust-workspace" | "rust" => ProjectType::RustWorkspace,
            "typescript-monorepo" | "typescript" => ProjectType::TypeScript,
            "python-ml" | "python" => ProjectType::Python,
            "go-service" | "go" => ProjectType::Go,
            "unreal-cpp" | "unreal" => ProjectType::UnrealCpp,
            "unity-csharp" | "unity" => ProjectType::UnityCsharp,
            _ => detected_type,
        }
    } else {
        detected_type
    };

    // Determine VCS.
    let effective_vcs: Option<String> = if let Some(v) = vcs_override {
        Some(v.to_string())
    } else if interactive {
        let has_git = project_root.join(".git").exists();
        let has_p4 = project_root.join(".p4config").exists();
        let has_svn = project_root.join(".svn").exists();
        let detected = if has_git {
            "git"
        } else if has_p4 {
            "perforce"
        } else if has_svn {
            "svn"
        } else {
            "none"
        };
        let vcs_ans = prompt("VCS backend (git/perforce/none)", detected);
        Some(vcs_ans)
    } else {
        None
    };

    // Determine GitHub remote.
    let effective_remote: Option<String> = if let Some(r) = remote_url {
        Some(r.to_string())
    } else if interactive {
        let want_remote = prompt_yn("Create GitHub remote?", false);
        if want_remote {
            let org_repo = prompt("Org/name (e.g., org/repo)", "org/repo");
            Some(format!("github.com/{}", org_repo))
        } else {
            None
        }
    } else {
        None
    };

    println!("Initializing TA project: {}", name);
    println!("Project type: {}", project_type);
    println!();

    // Generate all config files from template.
    std::fs::create_dir_all(&ta_dir)?;

    generate_workflow_toml(&ta_dir, &project_type)?;
    write_claude_md(project_root, name, &project_type, overwrite)?;
    generate_memory_toml(&ta_dir, &project_type)?;
    generate_policy_yaml(&ta_dir, &project_type)?;
    generate_taignore(project_root, &project_type)?;
    generate_agent_configs(&ta_dir)?;
    generate_constitutions(&ta_dir)?;
    seed_memory_entries(&ta_dir, &project_type, project_root)?;

    // Write .ta/project.toml with initial version.
    let project_toml_path = ta_dir.join("project.toml");
    if !project_toml_path.exists() {
        std::fs::write(&project_toml_path, "[project]\nversion = \"0.1.0-alpha\"\n")?;
    }

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

    // Run VCS setup automatically.
    {
        use super::setup::{execute as setup_execute, SetupCommands};
        let vcs_cmd = SetupCommands::Vcs {
            force: false,
            dry_run: false,
            vcs: effective_vcs,
            project_type: None,
        };
        // Best-effort: log failure but don't abort init.
        if let Err(e) = setup_execute(&vcs_cmd, config) {
            println!("  Warning: VCS setup encountered an issue: {}", e);
            println!("  Run `ta setup vcs` manually to retry.");
        }
    }

    println!();
    println!("TA project initialized successfully!");
    println!();
    println!("Generated files:");
    println!("  CLAUDE.md              — agent instructions (build, verify, git rules)");
    println!("  .ta/workflow.toml      — workflow configuration");
    println!("  .ta/memory.toml        — memory key schema and backend");
    println!("  .ta/policy.yaml        — security policy");
    println!("  .ta/agents/            — agent configurations");
    println!("  .ta/constitutions/     — starter constitutions");
    println!("  .ta/project.toml       — project version");
    println!("  .taignore              — file exclusion patterns");
    if is_game_engine {
        println!("  .ta/bmad.toml          — BMAD machine-local install config");
        println!("  .ta/agents/bmad-*.toml — BMAD role agent configs");
        println!("  .mcp.json              — Claude Flow MCP server config");
        println!("  .ta/onboarding-goal.md — First-run discovery goal");
    }

    // GitHub remote creation.
    if let Some(ref remote) = effective_remote {
        let repo_slug = remote
            .trim_start_matches("https://github.com/")
            .trim_start_matches("github.com/");
        println!();
        println!("Creating GitHub remote: {}", repo_slug);
        let status = std::process::Command::new("gh")
            .args([
                "repo",
                "create",
                repo_slug,
                "--private",
                "--source=.",
                "--remote=origin",
                "--push",
            ])
            .current_dir(project_root)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!(
                    "  GitHub remote created and pushed: github.com/{}",
                    repo_slug
                );
            }
            Ok(s) => {
                println!(
                    "  Warning: `gh repo create` exited with status {}.",
                    s.code().unwrap_or(-1)
                );
                println!(
                    "  Run manually: gh repo create {} --private --source=. --remote=origin --push",
                    repo_slug
                );
            }
            Err(e) => {
                println!("  Warning: Could not run `gh`: {}", e);
                println!(
                    "  Run manually: gh repo create {} --private --source=. --remote=origin --push",
                    repo_slug
                );
            }
        }
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
    }
    println!();
    println!("Next: generate your project plan");
    println!("  ta plan new \"description of your project\"");
    println!("  ta plan new --file product-spec.md");

    Ok(())
}

/// Parse a template name string into a `ProjectType`.
fn parse_template_name(tmpl: &str) -> anyhow::Result<ProjectType> {
    match tmpl {
        "rust-workspace" | "rust" => Ok(ProjectType::RustWorkspace),
        "typescript-monorepo" | "typescript" => Ok(ProjectType::TypeScript),
        "python-ml" | "python" => Ok(ProjectType::Python),
        "go-service" | "go" => Ok(ProjectType::Go),
        "unreal-cpp" | "unreal" => Ok(ProjectType::UnrealCpp),
        "unity-csharp" | "unity" => Ok(ProjectType::UnityCsharp),
        "generic" => Ok(ProjectType::Generic),
        _ => anyhow::bail!(
            "Unknown template: '{}'. Run `ta init templates` for available options.",
            tmpl
        ),
    }
}

/// Build CLAUDE.md content for the given project name and type.
///
/// The verify commands are derived from the same template logic used by
/// `generate_workflow_toml`, so both files stay in sync.
pub fn generate_claude_md(project_name: &str, project_type: &ProjectType) -> String {
    let (build_section, verify_section, extra_rules) = match project_type {
        ProjectType::RustWorkspace => (
            "./dev cargo build --workspace",
            "./dev cargo test --workspace\n./dev cargo clippy --workspace --all-targets -- -D warnings\n./dev cargo fmt --all -- --check",
            "- Use `tempfile::tempdir()` for test fixtures that need filesystem access",
        ),
        ProjectType::TypeScript => (
            "npm run build",
            "npm run typecheck\nnpm test\nnpm run lint",
            "- Do not commit generated files (dist/, build/)",
        ),
        ProjectType::Python => (
            "pip install -e .",
            "ruff check .\nmypy src/\npytest",
            "- Do not commit .venv/ or __pycache__/",
        ),
        ProjectType::Go => (
            "go build ./...",
            "go build ./...\ngo test ./...\ngo vet ./...",
            "- Run `go mod tidy` when adding or removing dependencies",
        ),
        ProjectType::UnrealCpp => (
            "# Run UnrealBuildTool for your target",
            "# See .ta/workflow.toml for verify commands",
            "- Never modify generated .generated.h files directly",
        ),
        ProjectType::UnityCsharp => (
            "# Open the project in Unity Editor to build",
            "# See .ta/workflow.toml for verify commands",
            "- Keep ProjectSettings/ under version control",
        ),
        ProjectType::Generic => (
            "# TODO: add build command",
            "# TODO: add verify commands, e.g.:\n# make test\n# make lint",
            "# TODO: add project-specific rules",
        ),
    };

    format!(
        r#"# {name}

## Build

{build}

## Verify (all must pass before committing)

{verify}

## Git

Always work on a feature branch. Never commit directly to main.
Branch prefixes: feature/, fix/, refactor/, docs/

## Rules

- Run verify after every code change, before committing
{extra}
"#,
        name = project_name,
        build = build_section,
        verify = verify_section,
        extra = extra_rules,
    )
}

/// Write CLAUDE.md to `project_root`. Skips if the file exists unless `overwrite` is true.
fn write_claude_md(
    project_root: &Path,
    project_name: &str,
    project_type: &ProjectType,
    overwrite: bool,
) -> anyhow::Result<()> {
    let path = project_root.join("CLAUDE.md");
    let already_exists = path.exists();
    if already_exists && !overwrite {
        println!("  CLAUDE.md already exists — skipping (use --overwrite to replace)");
        return Ok(());
    }
    let content = generate_claude_md(project_name, project_type);
    std::fs::write(&path, content)?;
    if already_exists {
        println!("  Replaced CLAUDE.md ({})", path.display());
    } else {
        println!("  Created CLAUDE.md — add project-specific rules before running ta run");
    }
    Ok(())
}

fn generate_workflow_toml(ta_dir: &Path, project_type: &ProjectType) -> anyhow::Result<()> {
    let path = ta_dir.join("workflow.toml");

    let verify_section = match project_type {
        ProjectType::RustWorkspace => {
            r#"
# Pre-draft verification gate
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

# VCS submit adapter
[submit.git]
branch_prefix = "feature/"
auto_review = true
"#
        }
        ProjectType::TypeScript => {
            r#"
# Pre-draft verification gate
[verify]
commands = [
    "npm run typecheck",
    "npm test",
    "npm run lint",
]
on_failure = "block"
timeout = 300

[submit.git]
branch_prefix = "feature/"
auto_review = true
"#
        }
        ProjectType::Python => {
            r#"
# Pre-draft verification gate
[verify]
commands = [
    "ruff check .",
    "mypy src/",
    "pytest",
]
on_failure = "block"
timeout = 300

[submit.git]
branch_prefix = "feature/"
auto_review = true
"#
        }
        ProjectType::Go => {
            r#"
# Pre-draft verification gate
[verify]
commands = [
    "go build ./...",
    "go test ./...",
    "go vet ./...",
]
on_failure = "block"
timeout = 300

[submit.git]
branch_prefix = "feature/"
auto_review = true
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

[submit.git]
branch_prefix = "feature/"
auto_review = true
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

    let common = "# TA staging and internal files\n.ta/staging/\n.ta/sessions/\n.ta/memory/\n.ta/review/\n\n";
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
        run_init(&config, None, Some("test-project"), None, None, true, false).unwrap();

        let ta_dir = dir.path().join(".ta");
        assert!(ta_dir.join("workflow.toml").exists());
        assert!(ta_dir.join("memory.toml").exists());
        assert!(ta_dir.join("policy.yaml").exists());
        assert!(ta_dir.join("agents").join("claude-code.yaml").exists());
        assert!(ta_dir.join("constitutions").join("default.yaml").exists());
        assert!(dir.path().join(".taignore").exists());
        assert!(dir.path().join("CLAUDE.md").exists());

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
        run_init(&config, None, None, None, None, true, false).unwrap();
    }

    #[test]
    fn init_with_template() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        run_init(
            &config,
            Some("typescript"),
            Some("my-ts-app"),
            None,
            None,
            true,
            false,
        )
        .unwrap();

        let memory = std::fs::read_to_string(dir.path().join(".ta").join("memory.toml")).unwrap();
        assert!(memory.contains("typescript"));
        assert!(memory.contains("package-map"));
    }

    #[test]
    fn init_unknown_template_errors() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let result = run_init(&config, Some("haskell"), None, None, None, true, false);
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
        run_init(
            &config,
            Some("unreal-cpp"),
            Some("MyGame"),
            None,
            None,
            true,
            false,
        )
        .unwrap();

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
        run_init(
            &config,
            Some("unity-csharp"),
            Some("MyUnityGame"),
            None,
            None,
            true,
            false,
        )
        .unwrap();

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

    // --- CLAUDE.md generation tests ---

    #[test]
    fn init_creates_claude_md_on_empty_dir() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        run_init(
            &config,
            Some("rust-workspace"),
            Some("my-project"),
            None,
            None,
            true,
            false,
        )
        .unwrap();

        let claude_md = dir.path().join("CLAUDE.md");
        assert!(claude_md.exists(), "CLAUDE.md should be created");
        let content = std::fs::read_to_string(&claude_md).unwrap();
        assert!(content.contains("# my-project"));
        assert!(content.contains("cargo test --workspace"));
        assert!(content.contains("cargo clippy"));
        assert!(content.contains("cargo fmt"));
    }

    #[test]
    fn init_skips_existing_claude_md() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# existing content\n").unwrap();
        let config = test_config(&dir);
        run_init(
            &config,
            Some("rust-workspace"),
            Some("my-project"),
            None,
            None,
            true,
            false,
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(
            content, "# existing content\n",
            "existing CLAUDE.md must not be overwritten"
        );
    }

    #[test]
    fn init_overwrites_claude_md_with_flag() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# old content\n").unwrap();
        let config = test_config(&dir);
        run_init(
            &config,
            Some("rust-workspace"),
            Some("my-project"),
            None,
            None,
            true,
            true, // --overwrite
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_ne!(content, "# old content\n", "CLAUDE.md should be replaced");
        assert!(
            content.contains("cargo test"),
            "replaced content should have verify commands"
        );
    }

    #[test]
    fn generate_claude_md_rust_has_cargo_verify() {
        let content = generate_claude_md("my-crate", &ProjectType::RustWorkspace);
        assert!(content.contains("# my-crate"));
        assert!(content.contains("cargo build --workspace"));
        assert!(content.contains("cargo test --workspace"));
        assert!(content.contains("cargo clippy --workspace --all-targets -- -D warnings"));
        assert!(content.contains("cargo fmt --all -- --check"));
        assert!(content.contains("tempfile::tempdir()"));
    }

    #[test]
    fn generate_claude_md_typescript_has_npm_verify() {
        let content = generate_claude_md("my-app", &ProjectType::TypeScript);
        assert!(content.contains("# my-app"));
        assert!(content.contains("npm run typecheck"));
        assert!(content.contains("npm test"));
        assert!(content.contains("npm run lint"));
    }

    #[test]
    fn generate_claude_md_python_has_pytest() {
        let content = generate_claude_md("my-pkg", &ProjectType::Python);
        assert!(content.contains("ruff check"));
        assert!(content.contains("mypy src/"));
        assert!(content.contains("pytest"));
    }

    #[test]
    fn generate_claude_md_go_has_go_test() {
        let content = generate_claude_md("my-svc", &ProjectType::Go);
        assert!(content.contains("go build ./..."));
        assert!(content.contains("go test ./..."));
        assert!(content.contains("go vet ./..."));
    }

    #[test]
    fn generate_claude_md_generic_has_placeholders() {
        let content = generate_claude_md("my-thing", &ProjectType::Generic);
        assert!(content.contains("# my-thing"));
        assert!(content.contains("TODO"));
    }

    #[test]
    fn write_claude_md_idempotent_without_overwrite() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# keep me\n").unwrap();
        write_claude_md(dir.path(), "proj", &ProjectType::RustWorkspace, false).unwrap();
        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(content, "# keep me\n");
    }

    #[test]
    fn init_already_configured_generates_missing_claude_md() {
        let dir = TempDir::new().unwrap();
        let ta_dir = dir.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("workflow.toml"), "# existing\n").unwrap();

        // CLAUDE.md does not exist yet — re-running init should create it.
        let config = test_config(&dir);
        run_init(
            &config,
            None,
            Some("re-run-project"),
            None,
            None,
            true,
            false,
        )
        .unwrap();

        assert!(
            dir.path().join("CLAUDE.md").exists(),
            "CLAUDE.md should be created on re-run when missing"
        );
    }
}
