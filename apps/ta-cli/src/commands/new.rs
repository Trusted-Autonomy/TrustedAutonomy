// new.rs — Conversational project bootstrapping (v0.10.17).
//
// `ta new` starts a multi-turn conversation with a planner agent to:
// 1. Understand what the user wants to build
// 2. Generate a project scaffold (directory structure, config files)
// 3. Produce a PLAN.md with versioned phases
// 4. Initialize the TA workspace
// 5. Offer to start the first goal
//
// Modes:
//   `ta new`                      — interactive conversation from scratch
//   `ta new --from brief.md`      — seed from a written description
//   `ta new --template rust-cli`  — use a project template as starting point
//   `ta new --name my-project`    — pre-set the project name
//   `ta new --output-dir ~/dev`   — create project in a specific directory

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;

use super::init;

/// Available project scaffolding templates for `ta new --template`.
const PROJECT_TEMPLATES: &[(&str, &str, &str)] = &[
    (
        "rust-cli",
        "Rust CLI application with clap and workspace structure",
        "rust-workspace",
    ),
    (
        "rust-lib",
        "Rust library crate with documentation and CI",
        "rust-workspace",
    ),
    (
        "ts-api",
        "TypeScript REST API with Express/Fastify",
        "typescript-monorepo",
    ),
    (
        "ts-app",
        "TypeScript web application (React/Next.js)",
        "typescript-monorepo",
    ),
    ("python-cli", "Python CLI with Click/Typer", "python-ml"),
    ("python-api", "Python API with FastAPI", "python-ml"),
    (
        "go-service",
        "Go microservice with module structure",
        "go-service",
    ),
    (
        "generic",
        "Generic project with minimal defaults",
        "generic",
    ),
];

/// Available version schemas for `--version-schema`.
const VERSION_SCHEMAS: &[(&str, &str)] = &[
    (
        "semver",
        "Semantic versioning (MAJOR.MINOR.PATCH-prerelease)",
    ),
    ("calver", "Calendar-based versioning (YYYY.MM.PATCH)"),
    ("sprint", "Sprint-based versioning (sprint-N.PATCH)"),
    (
        "milestone",
        "Milestone-based versioning (milestone-N.PATCH)",
    ),
];

#[derive(Subcommand, Debug)]
pub enum NewCommands {
    /// Start a conversational project bootstrapping session.
    Run {
        /// Project name (prompted if not provided).
        #[arg(long)]
        name: Option<String>,
        /// Seed from a written description file (PRD, spec, brief).
        #[arg(long)]
        from: Option<PathBuf>,
        /// Use a project template as starting point.
        #[arg(long)]
        template: Option<String>,
        /// Output directory for the new project (defaults to current directory).
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Agent system to use (default: claude-code).
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// Version schema to use for the plan (semver, calver, sprint, milestone).
        #[arg(long)]
        version_schema: Option<String>,
        /// Skip the interactive conversation — generate from template/brief only.
        #[arg(long)]
        non_interactive: bool,
    },
    /// List available project templates.
    Templates,
    /// List available version schemas.
    VersionSchemas,
}

pub fn execute(command: &NewCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        NewCommands::Run {
            name,
            from,
            template,
            output_dir,
            agent,
            version_schema,
            non_interactive,
        } => run_new(
            config,
            name.as_deref(),
            from.as_deref(),
            template.as_deref(),
            output_dir.as_deref(),
            agent,
            version_schema.as_deref(),
            *non_interactive,
        ),
        NewCommands::Templates => list_templates(),
        NewCommands::VersionSchemas => list_version_schemas(),
    }
}

fn list_templates() -> anyhow::Result<()> {
    println!("Available project templates:\n");
    for (name, desc, _init_template) in PROJECT_TEMPLATES {
        println!("  {:<16} {}", name, desc);
    }
    println!();
    println!("Usage: ta new run --template <name>");
    println!("       ta new run --template rust-cli --name my-project");
    Ok(())
}

fn list_version_schemas() -> anyhow::Result<()> {
    println!("Available version schemas:\n");
    for (name, desc) in VERSION_SCHEMAS {
        println!("  {:<12} {}", name, desc);
    }
    println!();
    println!("Usage: ta new run --version-schema <name>");
    println!("       ta plan create --version-schema <name>");
    Ok(())
}

/// Resolve the project name — use provided value, prompt interactively, or derive from path.
fn resolve_project_name(
    provided: Option<&str>,
    output_dir: &Path,
    non_interactive: bool,
) -> anyhow::Result<String> {
    if let Some(name) = provided {
        validate_project_name(name)?;
        return Ok(name.to_string());
    }

    if non_interactive {
        // Derive from directory name.
        let name = output_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project");
        return Ok(name.to_string());
    }

    // Interactive prompt.
    print!("Project name: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let name = input.trim();

    if name.is_empty() {
        anyhow::bail!("Project name is required. Use --name <name> or provide it interactively.");
    }

    validate_project_name(name)?;
    Ok(name.to_string())
}

/// Validate that a project name is suitable for use as a directory name.
fn validate_project_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("Project name cannot be empty.");
    }
    if name.len() > 64 {
        anyhow::bail!(
            "Project name '{}' is too long (max 64 characters, got {}).",
            name,
            name.len()
        );
    }
    // Allow alphanumeric, hyphens, underscores, dots.
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        anyhow::bail!(
            "Project name '{}' contains invalid characters. \
             Use only alphanumeric characters, hyphens, underscores, and dots.",
            name
        );
    }
    // Must not start with a dot or hyphen.
    if name.starts_with('.') || name.starts_with('-') {
        anyhow::bail!("Project name '{}' must not start with '.' or '-'.", name);
    }
    Ok(())
}

/// Resolve the template to its init template name.
fn resolve_template(template: &str) -> anyhow::Result<(&'static str, &'static str)> {
    for (name, desc, init_name) in PROJECT_TEMPLATES {
        if *name == template {
            return Ok((init_name, desc));
        }
    }
    let available: Vec<&str> = PROJECT_TEMPLATES.iter().map(|(n, _, _)| *n).collect();
    anyhow::bail!(
        "Unknown template: '{}'. Available: {}\n\nRun `ta new templates` for details.",
        template,
        available.join(", ")
    );
}

/// Validate a version schema name.
fn validate_version_schema(schema: &str) -> anyhow::Result<()> {
    if VERSION_SCHEMAS.iter().any(|(name, _)| *name == schema) {
        return Ok(());
    }
    let available: Vec<&str> = VERSION_SCHEMAS.iter().map(|(n, _)| *n).collect();
    anyhow::bail!(
        "Unknown version schema: '{}'. Available: {}\n\nRun `ta new version-schemas` for details.",
        schema,
        available.join(", ")
    );
}

/// Copy a version schema file into the project's .ta/ directory.
fn install_version_schema(project_dir: &Path, schema_name: &str) -> anyhow::Result<()> {
    let ta_dir = project_dir.join(".ta");
    std::fs::create_dir_all(&ta_dir)?;

    let dest = ta_dir.join("version-schema.yaml");

    // Try to find the schema template in the binary's templates directory.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let src = bin_dir
                .join("templates")
                .join("version-schemas")
                .join(format!("{}.yaml", schema_name));
            if src.exists() {
                std::fs::copy(&src, &dest)?;
                println!(
                    "  Installed version schema: {} (from shipped templates)",
                    schema_name
                );
                return Ok(());
            }
        }
    }

    // Fall back to generating a minimal schema inline.
    let content = match schema_name {
        "semver" => {
            "name: semver\ndescription: Semantic versioning (MAJOR.MINOR.PATCH-prerelease)\n\
             format:\n  pattern: \"^v?(?P<major>\\\\d+)\\\\.(?P<minor>\\\\d+)\\\\.(?P<patch>\\\\d+)(?:-(?P<pre>[a-zA-Z0-9.]+))?$\"\n  \
             template: \"v{major}.{minor}.{patch}\"\n  pre_release_template: \"v{major}.{minor}.{patch}-{pre}\"\n\
             initial_version: \"0.1.0-alpha\"\n"
        }
        "calver" => {
            "name: calver\ndescription: Calendar-based versioning (YYYY.MM.PATCH)\n\
             format:\n  pattern: \"^v?(?P<year>\\\\d{4})\\\\.(?P<month>\\\\d{2})\\\\.(?P<patch>\\\\d+)$\"\n  \
             template: \"v{year}.{month}.{patch}\"\n\
             initial_version: \"2026.01.0\"\n"
        }
        "sprint" => {
            "name: sprint\ndescription: Sprint-based versioning\n\
             format:\n  pattern: \"^sprint-(?P<sprint>\\\\d+)\\\\.(?P<patch>\\\\d+)$\"\n  \
             template: \"sprint-{sprint}.{patch}\"\n\
             initial_version: \"sprint-1.0\"\n"
        }
        "milestone" => {
            "name: milestone\ndescription: Milestone-based versioning\n\
             format:\n  pattern: \"^milestone-(?P<milestone>\\\\d+)\\\\.(?P<patch>\\\\d+)$\"\n  \
             template: \"milestone-{milestone}.{patch}\"\n\
             initial_version: \"milestone-1.0\"\n"
        }
        _ => {
            anyhow::bail!("No built-in schema for '{}'", schema_name);
        }
    };
    std::fs::write(&dest, content)?;
    println!("  Installed version schema: {}", schema_name);
    Ok(())
}

/// Generate the scaffold for a project directory.
fn generate_scaffold(
    project_dir: &Path,
    project_name: &str,
    template: Option<&str>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(project_dir)?;

    // Generate a minimal README.
    let readme_path = project_dir.join("README.md");
    if !readme_path.exists() {
        let content = format!(
            "# {}\n\nProject bootstrapped with `ta new`.\n",
            project_name
        );
        std::fs::write(&readme_path, content)?;
        println!("  Created README.md");
    }

    // Generate language-specific scaffold files based on template.
    match template {
        Some("rust-cli") | Some("rust-lib") => {
            generate_rust_scaffold(project_dir, project_name, template.unwrap())?;
        }
        Some("ts-api") | Some("ts-app") => {
            generate_typescript_scaffold(project_dir, project_name, template.unwrap())?;
        }
        Some("python-cli") | Some("python-api") => {
            generate_python_scaffold(project_dir, project_name, template.unwrap())?;
        }
        Some("go-service") => {
            generate_go_scaffold(project_dir, project_name)?;
        }
        _ => {
            // Generic — just the README and .ta/ init.
        }
    }

    Ok(())
}

fn generate_rust_scaffold(
    project_dir: &Path,
    project_name: &str,
    template: &str,
) -> anyhow::Result<()> {
    let is_cli = template == "rust-cli";
    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // Cargo.toml
    let cargo_toml = if is_cli {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nclap = {{ version = \"4\", features = [\"derive\"] }}\nanyhow = \"1\"\n",
            name = project_name
        )
    } else {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\n",
            name = project_name
        )
    };
    std::fs::write(project_dir.join("Cargo.toml"), cargo_toml)?;
    println!("  Created Cargo.toml");

    // Main source file.
    let main_file = if is_cli { "main.rs" } else { "lib.rs" };
    let main_content = if is_cli {
        "fn main() {\n    println!(\"Hello, world!\");\n}\n"
    } else {
        "/// Library root.\npub fn hello() -> &'static str {\n    \"Hello, world!\"\n}\n\n\
         #[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn it_works() {\n        \
         assert_eq!(hello(), \"Hello, world!\");\n    }\n}\n"
    };
    std::fs::write(src_dir.join(main_file), main_content)?;
    println!("  Created src/{}", main_file);

    // .gitignore
    let gitignore = project_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "/target\n")?;
        println!("  Created .gitignore");
    }

    Ok(())
}

fn generate_typescript_scaffold(
    project_dir: &Path,
    project_name: &str,
    template: &str,
) -> anyhow::Result<()> {
    let is_api = template == "ts-api";
    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // package.json
    let pkg = format!(
        "{{\n  \"name\": \"{name}\",\n  \"version\": \"0.1.0\",\n  \"private\": true,\n  \
         \"scripts\": {{\n    \"build\": \"tsc\",\n    \"start\": \"node dist/index.js\",\n    \
         \"dev\": \"ts-node src/index.ts\",\n    \"test\": \"jest\"\n  }}\n}}\n",
        name = project_name
    );
    std::fs::write(project_dir.join("package.json"), pkg)?;
    println!("  Created package.json");

    // tsconfig.json
    let tsconfig = "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \
                    \"module\": \"commonjs\",\n    \"outDir\": \"./dist\",\n    \
                    \"rootDir\": \"./src\",\n    \"strict\": true,\n    \
                    \"esModuleInterop\": true\n  },\n  \"include\": [\"src\"]\n}\n";
    std::fs::write(project_dir.join("tsconfig.json"), tsconfig)?;
    println!("  Created tsconfig.json");

    // Main source file.
    let main_content = if is_api {
        "// API entry point\nconsole.log('Server starting...');\n"
    } else {
        "// Application entry point\nconsole.log('Hello, world!');\n"
    };
    std::fs::write(src_dir.join("index.ts"), main_content)?;
    println!("  Created src/index.ts");

    // .gitignore
    let gitignore = project_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "node_modules/\ndist/\n")?;
        println!("  Created .gitignore");
    }

    Ok(())
}

fn generate_python_scaffold(
    project_dir: &Path,
    project_name: &str,
    template: &str,
) -> anyhow::Result<()> {
    let is_cli = template == "python-cli";
    let pkg_name = project_name.replace('-', "_");
    let pkg_dir = project_dir.join(&pkg_name);
    std::fs::create_dir_all(&pkg_dir)?;

    // __init__.py
    std::fs::write(pkg_dir.join("__init__.py"), "")?;

    // Main module.
    let main_content = if is_cli {
        format!(
            "\"\"\"CLI entry point for {}.\"\"\"\n\n\ndef main():\n    print(\"Hello, world!\")\n\n\n\
             if __name__ == \"__main__\":\n    main()\n",
            project_name
        )
    } else {
        format!(
            "\"\"\"API entry point for {}.\"\"\"\n\n\ndef create_app():\n    \"\"\"Create and configure the application.\"\"\"\n    pass\n",
            project_name
        )
    };
    std::fs::write(pkg_dir.join("__main__.py"), main_content)?;
    println!("  Created {}/", pkg_name);

    // pyproject.toml
    let pyproject = format!(
        "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"\"\n\
         requires-python = \">=3.10\"\n\n[build-system]\nrequires = [\"setuptools\"]\n\
         build-backend = \"setuptools.backends._legacy:_Backend\"\n",
        name = project_name
    );
    std::fs::write(project_dir.join("pyproject.toml"), pyproject)?;
    println!("  Created pyproject.toml");

    // .gitignore
    let gitignore = project_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "__pycache__/\n*.pyc\n.venv/\n*.egg-info/\n")?;
        println!("  Created .gitignore");
    }

    Ok(())
}

fn generate_go_scaffold(project_dir: &Path, project_name: &str) -> anyhow::Result<()> {
    // go.mod
    let go_mod = format!("module {}\n\ngo 1.22\n", project_name);
    std::fs::write(project_dir.join("go.mod"), go_mod)?;
    println!("  Created go.mod");

    // main.go
    let main_go =
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"Hello, world!\")\n}\n";
    std::fs::write(project_dir.join("main.go"), main_go)?;
    println!("  Created main.go");

    // .gitignore
    let gitignore = project_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "/vendor\n")?;
        println!("  Created .gitignore");
    }

    Ok(())
}

/// Build the planning prompt for the bootstrapping agent.
fn build_bootstrapping_prompt(
    project_name: &str,
    description: Option<&str>,
    template: Option<&str>,
    version_schema: Option<&str>,
) -> String {
    let desc_section = match description {
        Some(desc) => format!(
            "\n## Project Description (from user-provided document)\n\n{}\n",
            desc
        ),
        None => String::new(),
    };

    let template_section = match template {
        Some(tmpl) => format!(
            "\n## Template\n\nThe project uses the `{}` template. The scaffold has already been \
             created. Focus on understanding the user's goals and generating a development plan.\n",
            tmpl
        ),
        None => String::new(),
    };

    let schema_section = match version_schema {
        Some(schema) => format!(
            "\n## Version Schema\n\nUse the `{}` versioning scheme for plan phases.\n",
            schema
        ),
        None => "\n## Version Schema\n\nUse semantic versioning (semver) by default: v0.1.0, v0.2.0, etc.\n".to_string(),
    };

    format!(
        r#"You are a project bootstrapping assistant. Your task is to help the user define and scaffold a new software project called **{name}**.

## Your Workflow

1. **Understand the project**: If a description document is provided below, read it carefully. Otherwise, ask the user what they want to build using `ta_ask_human`.

2. **Ask clarifying questions**: Use `ta_ask_human` to ask 2-4 focused questions about:
   - Key features and scope
   - Technology preferences (if not already specified by template)
   - Integration requirements
   - Target users or deployment environment

3. **Propose a plan**: Show the user your proposed development plan (list of phases with descriptions) using `ta_ask_human` with `response_hint: "choice"` and choices like "Looks good", "Adjust phases", "Add more detail".

4. **Generate PLAN.md**: Write a PLAN.md file in the workspace root following this format:

   ```markdown
   # {{Project Name}} — Development Plan

   ## Versioning & Release Policy
   Phases map to versions using [schema].
   Version format: `MAJOR.MINOR.PATCH-alpha`.

   ---

   ### v0.1.0 — Phase Title
   <!-- status: pending -->
   **Goal**: One-sentence description.

   #### Items
   1. **Item title**: Description of the deliverable.
   2. **Item title**: Description of the deliverable.

   #### Version: `0.1.0-alpha`

   ---
   ```

5. **Confirm and finalize**: After writing PLAN.md, tell the user what was created using `ta_ask_human` and ask if they want to start the first phase.
{desc_section}{template_section}{schema_section}
## Rules

- Always use `ta_ask_human` for user interaction (never assume answers).
- Keep phases scoped to 1-3 working sessions each.
- Include 2-6 concrete items per phase.
- Phase 1 should always be project scaffold / setup.
- Include a testing/documentation phase.
- Mark all phases `<!-- status: pending -->`.
- Only create/modify PLAN.md — scaffold files are already handled.
- The project name is: **{name}**"#,
        name = project_name,
        desc_section = desc_section,
        template_section = template_section,
        schema_section = schema_section,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_new(
    config: &GatewayConfig,
    name: Option<&str>,
    from: Option<&Path>,
    template: Option<&str>,
    output_dir: Option<&Path>,
    agent: &str,
    version_schema: Option<&str>,
    non_interactive: bool,
) -> anyhow::Result<()> {
    // 1. Validate inputs.
    if let Some(schema) = version_schema {
        validate_version_schema(schema)?;
    }
    let (init_template_name, _template_desc) = if let Some(tmpl) = template {
        let (init_name, desc) = resolve_template(tmpl)?;
        (Some(init_name), Some(desc))
    } else {
        (None, None)
    };

    // 2. Determine output directory.
    let base_dir = output_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config.workspace_root.clone());

    // 3. Resolve project name.
    let project_name = resolve_project_name(name, &base_dir, non_interactive)?;
    let project_dir = if name.is_some() || output_dir.is_some() {
        // If a name was given or we're using a specific output dir,
        // create a subdirectory with the project name.
        base_dir.join(&project_name)
    } else {
        // If using current directory without explicit name, use it directly.
        base_dir.clone()
    };

    // Check if the project directory already has TA configuration.
    let ta_dir = project_dir.join(".ta");
    if ta_dir.join("workflow.toml").exists() {
        anyhow::bail!(
            "Directory '{}' already has TA configuration.\n\
             Use `ta init run` to reinitialize, or choose a different directory with --output-dir.",
            project_dir.display()
        );
    }

    println!("Creating new project: {}", project_name);
    println!("  Directory: {}", project_dir.display());
    if let Some(tmpl) = template {
        println!("  Template:  {}", tmpl);
    }
    if let Some(schema) = version_schema {
        println!("  Schema:    {}", schema);
    }
    println!();

    // 4. Generate project scaffold.
    println!("Generating project scaffold...");
    generate_scaffold(&project_dir, &project_name, template)?;

    // 5. Initialize TA workspace using the init module.
    println!();
    println!("Initializing TA workspace...");
    let project_config = GatewayConfig::for_project(&project_dir);
    // Use the init template that maps from our project template.
    let init_result = init::execute(
        &init::InitCommands::Run {
            template: init_template_name.map(|s| s.to_string()),
            detect: init_template_name.is_none(),
            name: Some(project_name.clone()),
        },
        &project_config,
    );
    if let Err(e) = init_result {
        tracing::warn!(
            project = %project_name,
            error = %e,
            "TA init returned an error (may be expected if already initialized)"
        );
    }

    // 6. Install version schema if specified.
    if let Some(schema) = version_schema {
        install_version_schema(&project_dir, schema)?;
    }

    // 7. Read description document if --from was provided.
    let description = if let Some(from_path) = from {
        let resolved = if from_path.is_absolute() {
            from_path.to_path_buf()
        } else {
            config.workspace_root.join(from_path)
        };
        if !resolved.exists() {
            anyhow::bail!(
                "Description file not found: {}\n\
                 Provide the full path to your project brief, PRD, or spec.",
                resolved.display()
            );
        }
        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", resolved.display(), e))?;
        if content.trim().is_empty() {
            anyhow::bail!(
                "Description file '{}' is empty. Provide a document with project requirements.",
                resolved.display()
            );
        }
        println!("  Loaded description from: {}", resolved.display());
        println!("  Document size: {} bytes", content.len());
        // Truncate very large docs for the prompt.
        let truncated = if content.len() > 100_000 {
            let mut t = content[..100_000].to_string();
            t.push_str("\n\n[... document truncated at 100KB ...]");
            t
        } else {
            content
        };
        Some(truncated)
    } else {
        None
    };

    // 8. Launch the planner agent for interactive bootstrapping.
    if non_interactive && from.is_none() {
        // Non-interactive without a description — just scaffold and init.
        println!();
        print_post_creation_summary(&project_dir, &project_name, false);
        return Ok(());
    }

    let objective = build_bootstrapping_prompt(
        &project_name,
        description.as_deref(),
        template,
        version_schema,
    );
    let title = format!("Bootstrap project: {}", project_name);

    println!();
    println!("Launching interactive bootstrapping session...");
    println!("  The agent will ask questions about your project and generate a development plan.");
    println!();

    // Delegate to `ta run` with --interactive in the new project directory.
    let run_config = GatewayConfig::for_project(&project_dir);
    super::run::execute(
        &run_config,
        Some(&title),
        agent,
        Some(&project_dir),
        &objective,
        None,  // no phase
        None,  // no follow_up
        None,  // follow_up_draft
        None,  // follow_up_goal
        None,  // no objective file
        false, // no_launch = false
        true,  // interactive = true
        false, // macro_goal = false
        None,  // resume = None
        false, // headless = false
        false, // skip_verify = false
        None,  // existing_goal_id = None
    )?;

    // 9. Post-creation handoff.
    println!();
    let has_plan = project_dir.join("PLAN.md").exists();
    print_post_creation_summary(&project_dir, &project_name, has_plan);

    Ok(())
}

/// Print the post-creation summary with next steps.
fn print_post_creation_summary(project_dir: &Path, project_name: &str, has_plan: bool) {
    println!("Project '{}' created successfully!", project_name);
    println!();
    println!("  Directory: {}", project_dir.display());
    if has_plan {
        println!("  Plan:      PLAN.md");
    }
    println!();
    println!("Next steps:");
    println!(
        "  cd {}",
        project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(".")
    );
    if has_plan {
        println!("  ta plan list                  — view development phases");
        println!("  ta plan next                  — see the first phase to implement");
        println!("  ta run --phase v0.1.0         — start implementing Phase 1");
    } else {
        println!("  ta plan from docs/PRD.md      — generate a plan from a document");
        println!("  ta run \"first goal\"            — start your first goal");
    }
    println!("  ta shell                      — open the interactive TUI");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── Name validation ──────────────────────────────────────────────

    #[test]
    fn valid_project_names() {
        assert!(validate_project_name("my-project").is_ok());
        assert!(validate_project_name("my_project").is_ok());
        assert!(validate_project_name("project123").is_ok());
        assert!(validate_project_name("a").is_ok());
        assert!(validate_project_name("v1.0.0").is_ok());
    }

    #[test]
    fn invalid_project_names() {
        assert!(validate_project_name("").is_err());
        assert!(validate_project_name(".hidden").is_err());
        assert!(validate_project_name("-starts-with-dash").is_err());
        assert!(validate_project_name("has spaces").is_err());
        assert!(validate_project_name("has/slashes").is_err());
        let long_name = "a".repeat(65);
        assert!(validate_project_name(&long_name).is_err());
    }

    // ── Template resolution ──────────────────────────────────────────

    #[test]
    fn resolve_known_templates() {
        let (init_name, _) = resolve_template("rust-cli").unwrap();
        assert_eq!(init_name, "rust-workspace");

        let (init_name, _) = resolve_template("ts-api").unwrap();
        assert_eq!(init_name, "typescript-monorepo");

        let (init_name, _) = resolve_template("python-cli").unwrap();
        assert_eq!(init_name, "python-ml");
    }

    #[test]
    fn resolve_unknown_template_errors() {
        assert!(resolve_template("haskell").is_err());
    }

    // ── Version schema validation ────────────────────────────────────

    #[test]
    fn validate_known_schemas() {
        assert!(validate_version_schema("semver").is_ok());
        assert!(validate_version_schema("calver").is_ok());
        assert!(validate_version_schema("sprint").is_ok());
        assert!(validate_version_schema("milestone").is_ok());
    }

    #[test]
    fn validate_unknown_schema_errors() {
        assert!(validate_version_schema("custom").is_err());
    }

    // ── Scaffold generation ──────────────────────────────────────────

    #[test]
    fn scaffold_rust_cli() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("my-rust-cli");
        generate_scaffold(&project_dir, "my-rust-cli", Some("rust-cli")).unwrap();

        assert!(project_dir.join("Cargo.toml").exists());
        assert!(project_dir.join("src/main.rs").exists());
        assert!(project_dir.join("README.md").exists());
        assert!(project_dir.join(".gitignore").exists());

        let cargo = std::fs::read_to_string(project_dir.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("my-rust-cli"));
        assert!(cargo.contains("clap"));
    }

    #[test]
    fn scaffold_rust_lib() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("my-rust-lib");
        generate_scaffold(&project_dir, "my-rust-lib", Some("rust-lib")).unwrap();

        assert!(project_dir.join("Cargo.toml").exists());
        assert!(project_dir.join("src/lib.rs").exists());

        let cargo = std::fs::read_to_string(project_dir.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("my-rust-lib"));
        assert!(!cargo.contains("clap"));
    }

    #[test]
    fn scaffold_typescript_api() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("my-ts-api");
        generate_scaffold(&project_dir, "my-ts-api", Some("ts-api")).unwrap();

        assert!(project_dir.join("package.json").exists());
        assert!(project_dir.join("tsconfig.json").exists());
        assert!(project_dir.join("src/index.ts").exists());

        let pkg = std::fs::read_to_string(project_dir.join("package.json")).unwrap();
        assert!(pkg.contains("my-ts-api"));
    }

    #[test]
    fn scaffold_python_cli() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("my-python-cli");
        generate_scaffold(&project_dir, "my-python-cli", Some("python-cli")).unwrap();

        assert!(project_dir.join("my_python_cli/__init__.py").exists());
        assert!(project_dir.join("my_python_cli/__main__.py").exists());
        assert!(project_dir.join("pyproject.toml").exists());
    }

    #[test]
    fn scaffold_go_service() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("my-go-svc");
        generate_scaffold(&project_dir, "my-go-svc", Some("go-service")).unwrap();

        assert!(project_dir.join("go.mod").exists());
        assert!(project_dir.join("main.go").exists());
    }

    #[test]
    fn scaffold_generic() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("my-generic");
        generate_scaffold(&project_dir, "my-generic", Some("generic")).unwrap();

        assert!(project_dir.join("README.md").exists());
        // Generic template should not generate language-specific files.
        assert!(!project_dir.join("Cargo.toml").exists());
        assert!(!project_dir.join("package.json").exists());
    }

    #[test]
    fn scaffold_no_template() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("bare");
        generate_scaffold(&project_dir, "bare", None).unwrap();

        assert!(project_dir.join("README.md").exists());
    }

    // ── Version schema installation ──────────────────────────────────

    #[test]
    fn install_version_schema_semver() {
        let dir = TempDir::new().unwrap();
        install_version_schema(dir.path(), "semver").unwrap();

        let schema_path = dir.path().join(".ta/version-schema.yaml");
        assert!(schema_path.exists());
        let content = std::fs::read_to_string(&schema_path).unwrap();
        assert!(content.contains("semver"));
    }

    #[test]
    fn install_version_schema_calver() {
        let dir = TempDir::new().unwrap();
        install_version_schema(dir.path(), "calver").unwrap();

        let schema_path = dir.path().join(".ta/version-schema.yaml");
        assert!(schema_path.exists());
        let content = std::fs::read_to_string(&schema_path).unwrap();
        assert!(content.contains("calver"));
    }

    // ── Bootstrapping prompt ─────────────────────────────────────────

    #[test]
    fn build_prompt_basic() {
        let prompt = build_bootstrapping_prompt("my-app", None, None, None);
        assert!(prompt.contains("my-app"));
        assert!(prompt.contains("ta_ask_human"));
        assert!(prompt.contains("PLAN.md"));
    }

    #[test]
    fn build_prompt_with_description() {
        let prompt = build_bootstrapping_prompt("my-app", Some("Build a DNS manager"), None, None);
        assert!(prompt.contains("Build a DNS manager"));
        assert!(prompt.contains("Project Description"));
    }

    #[test]
    fn build_prompt_with_template() {
        let prompt = build_bootstrapping_prompt("my-app", None, Some("rust-cli"), None);
        assert!(prompt.contains("rust-cli"));
        assert!(prompt.contains("Template"));
    }

    #[test]
    fn build_prompt_with_schema() {
        let prompt = build_bootstrapping_prompt("my-app", None, None, Some("calver"));
        assert!(prompt.contains("calver"));
    }

    // ── Non-interactive scaffold-only mode ───────────────────────────

    #[test]
    fn run_new_non_interactive_no_from() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("test-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let config = GatewayConfig::for_project(&project_dir);

        // Non-interactive without --from should just scaffold + init.
        let result = run_new(
            &config,
            Some("test-project"),
            None,             // no --from
            Some("rust-cli"), // template
            Some(dir.path()), // output_dir
            "claude-code",
            None, // no version schema
            true, // non_interactive
        );
        assert!(result.is_ok());
        assert!(project_dir.join("Cargo.toml").exists());
        assert!(project_dir.join("README.md").exists());
    }

    #[test]
    fn run_new_rejects_existing_ta_project() {
        let dir = TempDir::new().unwrap();
        let project_dir = dir.path().join("existing-project");
        let ta_dir = project_dir.join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("workflow.toml"), "# existing config").unwrap();

        let config = GatewayConfig::for_project(dir.path());
        let result = run_new(
            &config,
            Some("existing-project"),
            None,
            None,
            Some(dir.path()),
            "claude-code",
            None,
            true,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already has TA configuration"));
    }

    // ── List commands ────────────────────────────────────────────────

    #[test]
    fn list_templates_succeeds() {
        assert!(list_templates().is_ok());
    }

    #[test]
    fn list_version_schemas_succeeds() {
        assert!(list_version_schemas().is_ok());
    }

    // ── Post-creation summary ────────────────────────────────────────

    #[test]
    fn post_creation_with_plan() {
        // Just ensure it doesn't panic.
        print_post_creation_summary(Path::new("/tmp/test"), "test-project", true);
    }

    #[test]
    fn post_creation_without_plan() {
        print_post_creation_summary(Path::new("/tmp/test"), "test-project", false);
    }

    // ── Name resolution ──────────────────────────────────────────────

    #[test]
    fn resolve_name_from_provided() {
        let name = resolve_project_name(Some("my-app"), Path::new("/tmp"), false).unwrap();
        assert_eq!(name, "my-app");
    }

    #[test]
    fn resolve_name_non_interactive_from_dir() {
        let name = resolve_project_name(None, Path::new("/tmp/my-project"), true).unwrap();
        assert_eq!(name, "my-project");
    }
}
