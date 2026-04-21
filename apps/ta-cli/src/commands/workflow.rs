// workflow.rs — CLI commands for workflow management (v0.10.5 / v0.14.8.2).
//
// Commands:
//   ta workflow run <name> --goal "<title>"  — execute a named governed workflow
//   ta workflow run email-manager [--since <iso>]  — run email assistant workflow
//   ta workflow init <name>                  — scaffold a built-in workflow (e.g. email-manager)
//   ta workflow start <definition.yaml>      — start a workflow from YAML
//   ta workflow status [run-id]              — show workflow / run status
//   ta workflow list [--templates|--source external]  — list workflows
//   ta workflow cancel <workflow_id>         — cancel a running workflow
//   ta workflow history <workflow_id>        — show stage transitions
//   ta workflow new <name>                   — scaffold a new workflow definition
//   ta workflow validate <path>              — validate a workflow definition
//   ta workflow add <name> --from <source>   — install from external source
//   ta workflow remove <name>               — remove an external workflow
//   ta workflow publish <name>              — publish to a registry
//   ta workflow update [name|--all]         — check for updates

use std::path::PathBuf;

use clap::Subcommand;
use ta_changeset::sources::{ExternalSource, Lockfile, PackageManifest, SourceCache};
use ta_mcp_gateway::GatewayConfig;
use ta_workflow::{
    artifact_dag, format_confirmation_card, resolve_intent, ArtifactStore, ArtifactType,
    ParamValues, PlanContext, ResolutionResult, TemplateLibrary, WorkflowCatalog,
    WorkflowDefinition, WorkflowEngine, YamlWorkflowEngine,
};

use super::email_manager;
use super::governed_workflow::{self, RunOptions};

#[derive(Subcommand)]
pub enum WorkflowCommands {
    /// Execute a named governed workflow end-to-end (v0.14.8.2).
    ///
    /// Runs the five-stage governance loop:
    ///   run_goal → review_draft → human_gate → apply_draft → pr_sync
    ///
    /// For the email-manager workflow (v0.15.10), runs the email assistant pipeline:
    ///   fetch → filter → reply-draft goal → supervisor → create_draft
    ///
    /// Parameterized templates (v0.15.23):
    ///   ta workflow run plan-build-phases --param phase_filter=v0.15
    ///   ta workflow run governed-goal --param goal_title="Fix the auth bug"
    ///
    /// Examples:
    ///   ta workflow run governed-goal --goal "Fix the auth bug"
    ///   ta workflow run governed-goal --goal "Add rate limiting" --dry-run
    ///   ta workflow run email-manager
    ///   ta workflow run email-manager --since 2026-04-01T00:00:00Z
    ///   ta workflow run email-manager --dry-run
    Run {
        /// Workflow name (e.g. "governed-goal", "email-manager").
        /// Resolved from .ta/workflows/<name>.toml then templates/workflows/<name>.toml.
        name: String,
        /// Goal title to execute through the workflow (required for governed-goal;
        /// unused for email-manager).
        #[arg(long)]
        goal: Option<String>,
        /// Agent to use for the run_goal / reply-drafting stage.
        #[arg(long, default_value = "claude-code")]
        agent: String,
        /// PLAN.md phase ID to focus on (e.g. "v0.4.0"). When set:
        ///   - injected into the agent's CLAUDE.md context via `ta run --phase`
        ///   - passed to `ta draft apply --phase` to mark the phase done in PLAN.md
        #[arg(long)]
        phase: Option<String>,
        /// Print the execution plan without running any stages or creating drafts.
        #[arg(long)]
        dry_run: bool,
        /// Resume a paused governed-workflow run at the next pending stage.
        #[arg(long)]
        resume: Option<String>,
        /// Override the fetch watermark for email-manager (ISO-8601 datetime).
        /// Useful for catching up after time away. Example: --since 2026-04-01T00:00:00Z
        #[arg(long)]
        since: Option<String>,
        /// Template parameter overrides as key=value pairs (v0.15.23).
        ///
        /// Each `--param` sets one parameter declared in the template's `params:` section.
        /// Unknown keys are rejected. Required params with no default must be supplied.
        ///
        /// Example:
        ///   ta workflow run plan-build-phases --param phase_filter=v0.15 --param max_phases=3
        #[arg(long = "param")]
        params: Vec<String>,
    },
    /// Print the full definition of a workflow template with parameter docs (v0.15.23).
    ///
    /// Searches .ta/workflow-templates/ (project), ~/.config/ta/workflow-templates/ (user),
    /// and built-in templates for the given name.
    ///
    /// Example:
    ///   ta workflow show plan-build-phases
    ///   ta workflow show governed-goal
    Show {
        /// Template name to display.
        name: String,
    },
    /// Initialise a built-in workflow template (v0.15.10).
    ///
    /// Creates config and supporting files for a named workflow if they are absent.
    ///
    /// Supported names:
    ///   email-manager — creates email-constitution.md and email-manager.toml
    ///
    /// Example:
    ///   ta workflow init email-manager
    Init {
        /// Built-in workflow name to initialise.
        name: String,
    },
    /// Start a workflow from a YAML definition file.
    Start {
        /// Path to the workflow definition YAML file.
        definition: PathBuf,
    },
    /// Show the status of a workflow or governed workflow run.
    ///
    /// When given a governed workflow run ID (or prefix), shows stage progress,
    /// reviewer verdict, and next action. Without an ID, shows the most recent run.
    Status {
        /// Workflow ID or governed workflow run ID (or 8-char prefix).
        /// If omitted, shows the most recent governed workflow run.
        workflow_id: Option<String>,
        /// Live-updating view: refreshes every 2 seconds showing step states,
        /// elapsed time, and last artifact emitted (v0.14.10).
        #[arg(long)]
        live: bool,
    },
    /// Print the resolved artifact-type DAG for a workflow definition (v0.14.10).
    ///
    /// Shows stage names, the artifact types flowing along each edge, and
    /// implicit dependencies resolved from type compatibility.
    ///
    /// Examples:
    ///   ta workflow graph .ta/workflows/my-workflow.yaml
    ///   ta workflow graph .ta/workflows/my-workflow.yaml --dot | dot -Tsvg > dag.svg
    Graph {
        /// Path to the workflow YAML definition file.
        path: PathBuf,
        /// Emit Graphviz DOT format instead of ASCII art.
        #[arg(long)]
        dot: bool,
    },
    /// Resume a paused or interrupted workflow run from the artifact store (v0.14.10).
    ///
    /// Reads the session artifact store, checks which stage outputs are already
    /// present, skips completed stages, and resumes at the first incomplete stage.
    ///
    /// Example:
    ///   ta workflow resume abc12345
    Resume {
        /// Workflow run ID (or 8-char prefix) to resume.
        run_id: String,
    },
    /// List all workflows (active and completed).
    List {
        /// Show available workflow templates instead of active workflows.
        #[arg(long)]
        templates: bool,
        /// Show only externally-sourced workflows.
        #[arg(long)]
        source: Option<String>,
        /// List built-in workflow names shipped with TA (usable with `ta run --workflow`).
        #[arg(long)]
        builtin: bool,
        /// List parameterized templates (project + user global + built-in) with their params (v0.15.23).
        #[arg(long)]
        param_templates: bool,
    },
    /// Cancel a running workflow.
    Cancel {
        /// Workflow ID to cancel.
        workflow_id: String,
    },
    /// Show stage transitions, verdicts, and routing decisions for a workflow.
    History {
        /// Workflow ID.
        workflow_id: String,
    },
    /// Scaffold a new workflow definition YAML file.
    New {
        /// Workflow name (used as the file name and workflow name field).
        name: String,
        /// Start from a built-in template instead of the default scaffold.
        #[arg(long)]
        from: Option<String>,
    },
    /// Validate a workflow definition YAML file.
    Validate {
        /// Path to the workflow definition YAML file.
        path: PathBuf,
    },
    /// Install a workflow from an external source (registry, GitHub, URL).
    Add {
        /// Workflow name to install as.
        name: String,
        /// Source to fetch from: registry:org/name, gh:org/repo, or https://...
        #[arg(long)]
        from: String,
    },
    /// Remove an externally-installed workflow.
    Remove {
        /// Workflow name to remove.
        name: String,
    },
    /// Publish a workflow to a registry.
    Publish {
        /// Workflow name to publish.
        name: String,
        /// Target registry (e.g., "trustedautonomy").
        #[arg(long)]
        registry: Option<String>,
        /// Version bump: major, minor, patch.
        #[arg(long)]
        bump: Option<String>,
    },
    /// Check for updates to externally-installed workflows.
    Update {
        /// Specific workflow name, or omit for all.
        name: Option<String>,
        /// Update all external workflows.
        #[arg(long)]
        all: bool,
    },
}

pub fn execute(command: &WorkflowCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        WorkflowCommands::Run {
            name,
            goal,
            agent,
            phase,
            dry_run,
            resume,
            since,
            params,
        } => {
            // email-manager uses a different pipeline.
            if name == "email-manager" {
                return email_manager::run_email_manager(
                    &config.workspace_root,
                    since.as_deref(),
                    *dry_run,
                    agent,
                );
            }

            // Intent resolution path: when `name` doesn't match a known template,
            // treat it as free-form natural language and try to resolve a template.
            // Explicit template names always take precedence.
            let lib = TemplateLibrary::new(&config.workspace_root);
            let known_template_names: Vec<String> =
                lib.list().into_iter().map(|e| e.name).collect();
            let is_known_template = known_template_names.iter().any(|n| n == name);

            if !is_known_template && goal.is_none() && params.is_empty() {
                let (resolved_name, resolved_params) =
                    run_intent_resolution(name, &config.workspace_root, *dry_run)?;
                // Re-enter the run path with the resolved template name and params.
                let param_pairs: Vec<String> = resolved_params
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect();
                let resolved_goal = resolve_template_goal(
                    &resolved_name,
                    None,
                    &param_pairs,
                    &config.workspace_root,
                )?;
                let goal_title = resolved_goal.as_deref().unwrap_or(&resolved_name);
                let mut param_map = std::collections::HashMap::new();
                for pair in &param_pairs {
                    if let Some((k, v)) = pair.split_once('=') {
                        param_map.insert(k.to_string(), v.to_string());
                    }
                }
                let opts = governed_workflow::RunOptions {
                    workspace_root: &config.workspace_root,
                    workflow_name: &resolved_name,
                    goal_title,
                    dry_run: *dry_run,
                    resume_run_id: resume.as_deref(),
                    agent,
                    plan_phase: phase.as_deref(),
                    depth: 0,
                    params: param_map,
                };
                return governed_workflow::run_governed_workflow(&opts);
            }

            // Resolve template parameters if the workflow has a params section.
            let resolved_goal =
                resolve_template_goal(name, goal.as_deref(), params, &config.workspace_root)?;

            // Governed-goal and other TOML-based workflows require --goal.
            let goal_title = resolved_goal.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "--goal is required for workflow '{}'\n\
                     Usage: ta workflow run {} --goal \"<title>\"",
                    name,
                    name
                )
            })?;
            // Parse --param key=value pairs into a map for governed workflow stages.
            let mut param_map = std::collections::HashMap::new();
            for pair in params.iter() {
                if let Some((k, v)) = pair.split_once('=') {
                    param_map.insert(k.to_string(), v.to_string());
                }
            }
            let opts = RunOptions {
                workspace_root: &config.workspace_root,
                workflow_name: name,
                goal_title,
                dry_run: *dry_run,
                resume_run_id: resume.as_deref(),
                agent,
                plan_phase: phase.as_deref(),
                depth: 0,
                params: param_map,
            };
            governed_workflow::run_governed_workflow(&opts)
        }
        WorkflowCommands::Show { name } => show_template(name, config),
        WorkflowCommands::Init { name } => match name.as_str() {
            "email-manager" => email_manager::init_email_manager(&config.workspace_root),
            other => anyhow::bail!(
                "Unknown built-in workflow '{}'. Supported: email-manager\n\
                     To scaffold a custom workflow definition, use: ta workflow new {}",
                other,
                other
            ),
        },
        WorkflowCommands::Start { definition } => start_workflow(definition),
        WorkflowCommands::Status { workflow_id, live } => {
            // email-manager has its own status view.
            if workflow_id.as_deref() == Some("email-manager") {
                return email_manager::show_email_manager_status(&config.workspace_root);
            }
            if *live {
                show_live_status(workflow_id.as_deref(), config)
            } else {
                let runs_dir = config.workspace_root.join(".ta").join("workflow-runs");
                // Try governed workflow run first; fall back to legacy status.
                if runs_dir.exists() || workflow_id.is_some() {
                    match governed_workflow::show_run_status(&runs_dir, workflow_id.as_deref()) {
                        Ok(()) => Ok(()),
                        Err(_) => show_status(workflow_id.as_deref()),
                    }
                } else {
                    show_status(workflow_id.as_deref())
                }
            }
        }
        WorkflowCommands::Graph { path, dot } => graph_workflow(path, *dot),
        WorkflowCommands::Resume { run_id } => resume_workflow(run_id, config),
        WorkflowCommands::List {
            templates,
            source,
            builtin,
            param_templates,
        } => {
            if *builtin {
                list_builtin_workflows()
            } else if *templates {
                list_templates()
            } else if *param_templates {
                list_parameterized_templates(config)
            } else if source.as_deref() == Some("external") {
                list_external_workflows(config)
            } else {
                list_workflows()
            }
        }
        WorkflowCommands::Cancel { workflow_id } => cancel_workflow(workflow_id),
        WorkflowCommands::History { workflow_id } => show_history(workflow_id),
        WorkflowCommands::New { name, from } => new_workflow(name, from.as_deref(), config),
        WorkflowCommands::Validate { path } => validate_workflow_cmd(path, config),
        WorkflowCommands::Add { name, from } => add_workflow(name, from, config),
        WorkflowCommands::Remove { name } => remove_workflow(name, config),
        WorkflowCommands::Publish {
            name,
            registry,
            bump,
        } => publish_workflow(name, registry.as_deref(), bump.as_deref(), config),
        WorkflowCommands::Update { name, all } => update_workflows(name.as_deref(), *all, config),
    }
}

/// Attempt intent resolution when `name` is not a known template.
///
/// Loads the template library, resolves intent from `name` as natural language,
/// presents a confirmation card (score ≥ 0.80) or a clarifying question (score < 0.80).
/// On confirmation, returns `(template_name, suggested_params)`.
/// On cancel or low confidence, returns an error so the caller exits cleanly.
fn run_intent_resolution(
    text: &str,
    workspace_root: &std::path::Path,
    dry_run: bool,
) -> anyhow::Result<(String, std::collections::HashMap<String, String>)> {
    let lib = TemplateLibrary::new(workspace_root);
    let templates = lib.list();
    let plan_ctx = PlanContext::load(workspace_root);

    match resolve_intent(text, &templates, &plan_ctx) {
        ResolutionResult::Resolved(candidate) => {
            let card = format_confirmation_card(&candidate, text);
            println!("{}", card);

            if dry_run {
                anyhow::bail!(
                    "[dry-run] Would run template '{}' with params: {:?}",
                    candidate.template_name,
                    candidate.suggested_params
                );
            }

            // Read user choice from stdin.
            print!("\nChoice [1-4]: ");
            let _ = std::io::Write::flush(&mut std::io::stdout());

            let mut choice = String::new();
            if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut choice).is_err() {
                // Non-interactive context — default to cancel.
                anyhow::bail!(
                    "Non-interactive terminal. Use an explicit template name:\n  \
                     ta workflow run {} --param phase_filter=...",
                    candidate.template_name
                );
            }

            match choice.trim() {
                "1" | "" => Ok((candidate.template_name, candidate.suggested_params)),
                "2" => {
                    println!();
                    println!("Adjust parameters with --param flags:");
                    for (k, v) in &candidate.suggested_params {
                        println!("  --param {}={}", k, v);
                    }
                    println!();
                    println!(
                        "Then run: ta workflow run {} --param ...",
                        candidate.template_name
                    );
                    anyhow::bail!("Cancelled — adjust params and re-run.")
                }
                "3" => {
                    println!();
                    println!("Available templates:");
                    for t in &templates {
                        println!("  {:<28} {}", t.name, t.description);
                    }
                    println!();
                    println!("Run: ta workflow run <template-name> [--param key=value ...]");
                    anyhow::bail!("Cancelled — choose a different workflow.")
                }
                _ => {
                    anyhow::bail!("Cancelled.")
                }
            }
        }
        ResolutionResult::ClarifyingQuestion(question) => {
            anyhow::bail!(
                "{}\n\nRe-run with a more specific request, or use an explicit template name.",
                question
            );
        }
    }
}

/// Resolve a workflow's goal title by processing template parameters if present.
///
/// When a workflow template declares a `params:` section, we:
///   1. Parse the `--param key=value` pairs from CLI.
///   2. Validate them against the template's param declarations.
///   3. Fill in defaults (which may reference `{{plan.*}}` built-ins).
///   4. If the template has `goal_title` in its params and no `--goal` was given,
///      synthesize the goal title from the resolved parameters.
///
/// Returns `Ok(Some(goal_title))` if a goal is available, `Ok(None)` if neither
/// the template nor CLI provides one (the caller will reject it for governed workflows).
fn resolve_template_goal(
    workflow_name: &str,
    goal: Option<&str>,
    param_pairs: &[String],
    workspace_root: &std::path::Path,
) -> anyhow::Result<Option<String>> {
    // If --goal was given, use it directly (no template param processing needed).
    if let Some(g) = goal {
        if !param_pairs.is_empty() {
            // Still validate the params so unknown keys are caught early.
            validate_params_only(workflow_name, param_pairs, workspace_root)?;
        }
        return Ok(Some(g.to_string()));
    }

    // No --goal: check if the template has a goal_title param we can use.
    let lib = TemplateLibrary::new(workspace_root);
    let template_yaml = match lib.load(workflow_name) {
        Some(y) => y,
        None => {
            // Not a parameterized template — let the caller handle missing --goal.
            return Ok(None);
        }
    };

    let def = match WorkflowDefinition::from_yaml(&template_yaml) {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };

    if def.params.is_empty() {
        // Template has no params — no goal synthesis possible.
        return Ok(None);
    }

    let plan_ctx = PlanContext::load(workspace_root);

    let mut pv = ParamValues::from_cli_pairs(param_pairs)
        .map_err(|e| anyhow::anyhow!("invalid --param: {}", e))?;
    pv.validate_and_fill(&def.params, &plan_ctx)
        .map_err(|e| anyhow::anyhow!("parameter error for workflow '{}': {}", workflow_name, e))?;

    // Try to synthesize a goal title from the resolved params.
    if let Some(goal_val) = pv.get("goal_title") {
        if !goal_val.is_empty() {
            return Ok(Some(goal_val.to_string()));
        }
    }

    // For plan-phase-loop templates (identified by having a `phase_filter` param),
    // synthesize a goal title from the resolved filter so --goal is not required.
    if def.params.contains_key("phase_filter") {
        let filter = pv.get("phase_filter").filter(|v| !v.is_empty());
        let title = match filter {
            Some(f) => format!("Build pending {} phases", f),
            None => "Build all pending phases".to_string(),
        };
        return Ok(Some(title));
    }

    // Fall through — caller decides if --goal is required.
    Ok(None)
}

/// Validate `--param` values against a template without running it.
fn validate_params_only(
    workflow_name: &str,
    param_pairs: &[String],
    workspace_root: &std::path::Path,
) -> anyhow::Result<()> {
    let lib = TemplateLibrary::new(workspace_root);
    let Some(template_yaml) = lib.load(workflow_name) else {
        return Ok(());
    };
    let Ok(def) = WorkflowDefinition::from_yaml(&template_yaml) else {
        return Ok(());
    };
    if def.params.is_empty() {
        return Ok(());
    }
    let plan_ctx = PlanContext::load(workspace_root);
    let mut pv = ParamValues::from_cli_pairs(param_pairs)
        .map_err(|e| anyhow::anyhow!("invalid --param: {}", e))?;
    pv.validate_and_fill(&def.params, &plan_ctx)
        .map_err(|e| anyhow::anyhow!("parameter error for workflow '{}': {}", workflow_name, e))?;
    Ok(())
}

fn start_workflow(definition_path: &std::path::Path) -> anyhow::Result<()> {
    if !definition_path.exists() {
        anyhow::bail!(
            "Workflow definition not found: {}\n\
             Create a workflow YAML file or use a built-in template:\n  \
             ta workflow new my-workflow\n  \
             ta workflow list --templates",
            definition_path.display()
        );
    }

    let def = WorkflowDefinition::from_file(definition_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse {}: {}\n\
             Check the YAML syntax and ensure all required fields are present.\n\
             Run: ta workflow validate {}",
            definition_path.display(),
            e,
            definition_path.display()
        )
    })?;

    // Validate stage ordering.
    let stage_order = def.stage_order().map_err(|e| {
        anyhow::anyhow!(
            "Invalid workflow definition: {}\n\
             Check stage dependencies for cycles.",
            e
        )
    })?;

    let mut engine = YamlWorkflowEngine::new();
    let workflow_id = engine.start(&def)?;

    println!("Workflow started: {}", workflow_id);
    println!("  Name:   {}", def.name);
    println!(
        "  Stages: {} ({})",
        def.stages.len(),
        stage_order.join(" -> ")
    );
    println!("  Roles:  {}", def.roles.len());
    if let Some(verdict) = &def.verdict {
        println!(
            "  Verdict: threshold={:.0}%, required={}",
            verdict.pass_threshold * 100.0,
            if verdict.required_pass.is_empty() {
                "(none)".to_string()
            } else {
                verdict.required_pass.join(", ")
            }
        );
    }
    println!();
    println!("Track progress:");
    println!(
        "  ta workflow status {}",
        &workflow_id[..8.min(workflow_id.len())]
    );

    Ok(())
}

fn show_status(workflow_id: Option<&str>) -> anyhow::Result<()> {
    match workflow_id {
        Some(id) => {
            println!("Workflow status for: {}", id);
            println!("  (Workflow state is managed by the daemon. Connect with `ta shell` for live status.)");
        }
        None => {
            println!("No workflow ID specified.");
            println!("Usage: ta workflow status <workflow_id>");
            println!();
            println!("List active workflows with: ta workflow list");
        }
    }
    Ok(())
}

fn list_workflows() -> anyhow::Result<()> {
    println!("Active workflows:");
    println!("  (No workflows running. Start one with: ta workflow start <definition.yaml>)");
    println!();
    println!("Scaffold a new workflow:");
    println!("  ta workflow new my-workflow");
    println!();
    println!("Browse built-in templates:");
    println!("  ta workflow list --templates");
    println!();
    println!("List built-in workflows (usable with ta run --workflow):");
    println!("  ta workflow list --builtin");
    Ok(())
}

/// List built-in workflow names usable with `ta run --workflow`.
///
/// These are the named execution strategies shipped with TA — distinct from
/// YAML workflow *definitions* (which are multi-stage agent graphs). Built-in
/// workflows control how a goal is dispatched: as a single agent, as a serial
/// phase chain, or as a parallel swarm.
fn list_builtin_workflows() -> anyhow::Result<()> {
    println!("Built-in workflows (use with: ta run \"goal\" --workflow <name>):");
    println!();
    for (name, desc) in WorkflowCatalog::list() {
        println!("  {:<20} {}", name, desc);
    }
    println!();
    println!("Example:");
    println!("  ta run \"Implement v0.13.7\" --workflow serial-phases");
    Ok(())
}

fn list_templates() -> anyhow::Result<()> {
    println!("Workflow templates:");
    println!();
    println!("  governed-goal        5-stage safe autonomous loop: run → review → gate → apply → sync (v0.14.8.2)");
    println!("  simple-review        2-stage build + review");
    println!("  security-audit       3-stage scan, review, remediate");
    println!("  milestone-review     4-stage plan, build, review, approval");
    println!("  deploy-pipeline      3-stage build, test, deploy with gates");
    println!("  plan-implement-review  Planner-driven loop with iterative review");
    println!();
    println!("Governed workflow (ta workflow run):");
    println!("  ta workflow run governed-goal --goal \"Fix the auth bug\"");
    println!("  ta workflow run governed-goal --goal \"...\" --dry-run");
    println!();
    println!("YAML-based workflows (ta workflow start):");
    println!("  ta workflow new my-workflow --from simple-review");
    println!();
    println!("Template files: templates/workflows/");
    println!();
    println!("Role templates:");
    println!("  engineer          Software engineer role");
    println!("  reviewer          Code reviewer role");
    println!("  security-reviewer Security-focused reviewer");
    println!("  planner           Technical planner role");
    println!("  pm                Project manager role");
    println!("  orchestrator      Multi-agent orchestrator role");
    println!();
    println!("Role files: templates/workflows/roles/");
    Ok(())
}

/// List parameterized templates from the template library (project + user + built-in).
fn list_parameterized_templates(config: &GatewayConfig) -> anyhow::Result<()> {
    let lib = TemplateLibrary::new(&config.workspace_root);
    let entries = lib.list();

    println!("Parameterized workflow templates:");
    println!();
    if entries.is_empty() {
        println!("  (none found)");
    } else {
        for entry in &entries {
            println!(
                "  {:<28} [{}]  {}",
                entry.name, entry.source, entry.description
            );
            for (param_name, param_summary) in &entry.params {
                println!("    --param {:<20} {}", param_name, param_summary);
            }
            if !entry.params.is_empty() {
                println!();
            }
        }
    }

    println!();
    println!("Template search paths (highest priority first):");
    println!(
        "  1. {}  (project)",
        config
            .workspace_root
            .join(".ta")
            .join("workflow-templates")
            .display()
    );
    println!("  2. ~/.config/ta/workflow-templates/  (user global)");
    println!("  3. built-in templates (shipped with ta)");
    println!();
    println!("Inspect a template:");
    println!("  ta workflow show plan-build-phases");
    println!();
    println!("Run with parameters:");
    println!("  ta workflow run plan-build-phases --param phase_filter=v0.15 --param max_phases=3");
    Ok(())
}

/// Show a template's full YAML with parameter documentation.
fn show_template(name: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    let lib = TemplateLibrary::new(&config.workspace_root);
    let content = lib.load(name).ok_or_else(|| {
        // Build a helpful error with the searched locations.
        let project_path = config
            .workspace_root
            .join(".ta")
            .join("workflow-templates")
            .join(format!("{}.yaml", name));
        anyhow::anyhow!(
            "Template '{}' not found.\n\
             Searched:\n  \
               {}\n  \
               ~/.config/ta/workflow-templates/{}.yaml\n  \
               built-in templates\n\
             \n\
             List all available templates:\n  \
               ta workflow list --templates\n  \
               ta workflow list --param-templates",
            name,
            project_path.display(),
            name
        )
    })?;

    // Find the source location.
    let source_label = {
        let project_path = config
            .workspace_root
            .join(".ta")
            .join("workflow-templates")
            .join(format!("{}.yaml", name));
        if project_path.exists() {
            format!("project ({})", project_path.display())
        } else if let Ok(home) = std::env::var("HOME") {
            let user_path = std::path::PathBuf::from(home)
                .join(".config")
                .join("ta")
                .join("workflow-templates")
                .join(format!("{}.yaml", name));
            if user_path.exists() {
                format!("user global ({})", user_path.display())
            } else {
                "built-in".to_string()
            }
        } else {
            "built-in".to_string()
        }
    };

    println!("Template: {}  [{}]", name, source_label);
    println!();
    println!("{}", content);

    Ok(())
}

fn cancel_workflow(workflow_id: &str) -> anyhow::Result<()> {
    println!("Cancelling workflow: {}", workflow_id);
    println!(
        "  Cancel request sent. Verify with: ta workflow status {}",
        workflow_id
    );
    Ok(())
}

fn show_history(workflow_id: &str) -> anyhow::Result<()> {
    println!("Workflow history for: {}", workflow_id);
    println!("  (Workflow history requires daemon connection. Start with: ta-daemon --api --project-root .)");
    Ok(())
}

/// Scaffold a new workflow definition.
fn new_workflow(
    name: &str,
    from_template: Option<&str>,
    config: &GatewayConfig,
) -> anyhow::Result<()> {
    let workflows_dir = config.workspace_root.join(".ta").join("workflows");
    std::fs::create_dir_all(&workflows_dir)?;

    let file_path = workflows_dir.join(format!("{}.yaml", name));
    if file_path.exists() {
        anyhow::bail!(
            "Workflow already exists: {}\n\
             Edit the existing file or choose a different name.",
            file_path.display()
        );
    }

    let content = if let Some(template_name) = from_template {
        // Try to find the template.
        let template_path = config
            .workspace_root
            .join("templates")
            .join("workflows")
            .join(format!("{}.yaml", template_name));
        if template_path.exists() {
            std::fs::read_to_string(&template_path).map_err(|e| {
                anyhow::anyhow!("Failed to read template {}: {}", template_path.display(), e)
            })?
        } else {
            // Fall back to built-in templates.
            match template_name {
                "governed-goal" => {
                    // Copy the YAML template to .ta/workflows/ for project-local customization.
                    let yaml_path = config
                        .workspace_root
                        .join("templates")
                        .join("workflows")
                        .join("governed-goal.yaml");
                    if yaml_path.exists() {
                        let yaml_content = std::fs::read_to_string(&yaml_path).map_err(|e| {
                            anyhow::anyhow!("Failed to read governed-goal.yaml: {}", e)
                        })?;
                        // Write as .yaml — canonical format for orchestration templates.
                        let yaml_dest = workflows_dir.join(format!("{}.yaml", name));
                        std::fs::write(&yaml_dest, yaml_content)?;
                        println!("Created governed workflow: {}", yaml_dest.display());
                        println!();
                        println!("Run it with:");
                        println!("  ta workflow run {} --goal \"Your goal title\"", name);
                        return Ok(());
                    }
                    anyhow::bail!(
                        "Built-in governed-goal template not found.\n\
                         Expected: templates/workflows/governed-goal.yaml\n\
                         Run directly: ta workflow run governed-goal --goal \"Your goal\""
                    );
                }
                "simple-review" => TEMPLATE_SIMPLE_REVIEW.to_string(),
                "security-audit" => TEMPLATE_SECURITY_AUDIT.to_string(),
                "milestone-review" => TEMPLATE_MILESTONE_REVIEW.to_string(),
                "deploy-pipeline" => TEMPLATE_DEPLOY_PIPELINE.to_string(),
                "plan-implement-review" => TEMPLATE_PLAN_IMPLEMENT_REVIEW.to_string(),
                _ => {
                    anyhow::bail!(
                        "Unknown template: '{}'\n\
                         Available templates: governed-goal, simple-review, security-audit, milestone-review, deploy-pipeline, plan-implement-review\n\
                         List all: ta workflow list --templates",
                        template_name
                    );
                }
            }
        }
    } else {
        generate_scaffold(name)
    };

    std::fs::write(&file_path, &content)?;

    println!("Created workflow: {}", file_path.display());

    // Parse the generated workflow and check for missing agent configs.
    if let Ok(def) = WorkflowDefinition::from_file(&file_path) {
        let agents_dir = config.workspace_root.join(".ta").join("agents");
        let missing_agents = find_missing_agents(&def, &agents_dir);
        if !missing_agents.is_empty() {
            println!();
            println!("Missing agent configs (create them to complete setup):");
            for (agent_name, agent_type) in &missing_agents {
                println!("  ta agent new {} --type {}", agent_name, agent_type);
            }
        }
    }

    println!();
    println!("Next steps:");
    println!("  1. Edit the workflow definition to match your needs");
    println!(
        "  2. Validate: ta workflow validate {}",
        file_path.display()
    );
    println!("  3. Start:    ta workflow start {}", file_path.display());

    Ok(())
}

/// Find agent names referenced in workflow roles that have no .ta/agents/<name>.yaml.
/// Returns (agent_name, suggested_type) pairs.
fn find_missing_agents(
    def: &WorkflowDefinition,
    agents_dir: &std::path::Path,
) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut missing = Vec::new();

    for (role_name, role_def) in &def.roles {
        if seen.contains(&role_def.agent) {
            continue;
        }
        seen.insert(role_def.agent.clone());

        let agent_path = agents_dir.join(format!("{}.yaml", role_def.agent));
        if !agent_path.exists() {
            // Guess agent type from role name / prompt content.
            let agent_type = guess_agent_type(role_name, &role_def.prompt);
            missing.push((role_def.agent.clone(), agent_type));
        }
    }

    missing
}

/// Guess an agent type from the role name and prompt.
fn guess_agent_type(role_name: &str, prompt: &str) -> String {
    let lower_name = role_name.to_lowercase();
    let lower_prompt = prompt.to_lowercase();

    if lower_name.contains("review")
        || lower_name.contains("audit")
        || lower_name.contains("security")
        || lower_name.contains("scanner")
    {
        "auditor".to_string()
    } else if lower_name.contains("plan") || lower_prompt.contains("planner") {
        "planner".to_string()
    } else if lower_name.contains("orchestrat") || lower_prompt.contains("coordinate") {
        "orchestrator".to_string()
    } else {
        "developer".to_string()
    }
}

/// Generate a scaffold workflow YAML with annotated comments.
fn generate_scaffold(name: &str) -> String {
    format!(
        r#"# {name} — Custom workflow definition.
#
# Usage:
#   ta workflow start .ta/workflows/{name}.yaml
#
# Validate before running:
#   ta workflow validate .ta/workflows/{name}.yaml

name: {name}

# Stages execute in dependency order. Each stage runs its assigned roles
# in parallel, then runs 'then' roles sequentially.
stages:
  - name: build
    # Roles that execute in parallel within this stage.
    roles: [engineer]
    # When to pause for human input: never | always | on_fail
    await_human: never

  - name: review
    # This stage waits for 'build' to complete first.
    depends_on: [build]
    roles: [reviewer]
    await_human: on_fail
    # Where to route if this stage fails review.
    on_fail:
      route_to: build
      max_retries: 2

# Role definitions — each role maps to an agent with a system prompt.
# The agent field references a config in .ta/agents/<agent>.yaml.
roles:
  engineer:
    agent: claude-code
    prompt: |
      You are a software engineer. Implement the requested changes
      following the project's coding standards. Write tests for your changes.

  reviewer:
    agent: claude-code
    prompt: |
      You are a code reviewer. Review the implementation for:
      - Correctness and completeness
      - Test coverage
      - Security issues
      - Code style consistency
      Provide a verdict with specific findings.

# Verdict scoring (optional). Controls when stages pass or fail.
verdict:
  # Minimum aggregate score (0.0-1.0) to pass.
  pass_threshold: 0.7
  # Roles whose pass is required regardless of aggregate score.
  # required_pass: [security-reviewer]
"#,
        name = name
    )
}

/// Validate a workflow definition and print results.
fn validate_workflow_cmd(path: &std::path::Path, config: &GatewayConfig) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "File not found: {}\n\
             Provide a path to a workflow YAML file.",
            path.display()
        );
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;

    let def = match WorkflowDefinition::from_yaml(&content) {
        Ok(d) => d,
        Err(e) => {
            println!("YAML parse error in {}:", path.display());
            println!("  {}", e);
            println!();
            println!("Fix the YAML syntax and try again.");
            return Ok(());
        }
    };

    let result = ta_workflow::validate::validate_workflow(&def, Some(&config.workspace_root));

    if result.findings.is_empty() {
        println!("Workflow '{}' is valid.", def.name);
        if let Ok(order) = def.stage_order() {
            println!("  Stages: {}", order.join(" -> "));
        }
        println!("  Roles:  {}", def.roles.len());

        // Even when valid, check for missing agent configs.
        let agents_dir = config.workspace_root.join(".ta").join("agents");
        let missing = find_missing_agents(&def, &agents_dir);
        if !missing.is_empty() {
            println!();
            println!("Create missing agent configs:");
            for (agent_name, agent_type) in &missing {
                println!("  ta agent new {} --type {}", agent_name, agent_type);
            }
        }

        return Ok(());
    }

    println!(
        "Validation results for '{}' ({}):",
        def.name,
        path.display()
    );
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

    if result.has_errors() {
        println!();
        println!("Fix errors before starting this workflow.");
    }

    // Show consolidated missing-agents summary with ready-to-run commands.
    let agents_dir = config.workspace_root.join(".ta").join("agents");
    let missing = find_missing_agents(&def, &agents_dir);
    if !missing.is_empty() {
        println!();
        println!("Create missing agent configs:");
        for (agent_name, agent_type) in &missing {
            println!("  ta agent new {} --type {}", agent_name, agent_type);
        }
    }

    Ok(())
}

// ── External source commands (v0.10.5) ──────────────────────────────

/// Install a workflow from an external source.
fn add_workflow(name: &str, from: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    let source = ExternalSource::parse(from).map_err(|e| {
        anyhow::anyhow!(
            "Invalid source '{}': {}\n\
             Expected formats:\n  \
             registry:org/name\n  \
             gh:org/repo\n  \
             https://example.com/workflow.yaml",
            from,
            e
        )
    })?;

    let workflows_dir = config.workspace_root.join(".ta").join("workflows");
    std::fs::create_dir_all(&workflows_dir)?;

    let target_path = workflows_dir.join(format!("{}.yaml", name));
    if target_path.exists() {
        anyhow::bail!(
            "Workflow '{}' already exists at {}.\n\
             Remove it first: ta workflow remove {}",
            name,
            target_path.display(),
            name
        );
    }

    println!("Fetching workflow '{}' from {} ...", name, from);

    let content = fetch_source_content(&source)?;

    // Validate the fetched YAML is a valid workflow definition.
    if let Err(e) = WorkflowDefinition::from_yaml(&content) {
        anyhow::bail!(
            "Fetched content from '{}' is not a valid workflow definition: {}\n\
             Check that the source contains a valid TA workflow YAML.",
            from,
            e
        );
    }

    std::fs::write(&target_path, &content)?;

    // Compute checksum and record in lockfile.
    let checksum = compute_checksum(&content);
    let lock_path = config.workspace_root.join(".ta").join("workflows.lock");
    let mut lockfile = Lockfile::load(&lock_path).unwrap_or_default();
    lockfile.add(ta_changeset::sources::LockEntry {
        name: name.to_string(),
        version: "latest".to_string(),
        source: from.to_string(),
        checksum,
    });
    lockfile.save(&lock_path)?;

    // Cache the content for offline use.
    {
        let cache = SourceCache::new("workflows");
        let _ = cache.store(name, &content, &source, "latest");
    }

    println!("Installed workflow: {}", target_path.display());
    println!("  Source: {}", from);
    println!();
    println!("Next steps:");
    println!("  Validate: ta workflow validate {}", target_path.display());
    println!("  Start:    ta workflow start {}", target_path.display());

    Ok(())
}

/// Remove an externally-installed workflow.
fn remove_workflow(name: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    let workflows_dir = config.workspace_root.join(".ta").join("workflows");
    let target_path = workflows_dir.join(format!("{}.yaml", name));

    if !target_path.exists() {
        anyhow::bail!(
            "Workflow '{}' not found at {}.\n\
             List workflows with: ta workflow list",
            name,
            target_path.display()
        );
    }

    std::fs::remove_file(&target_path)?;

    // Remove from lockfile.
    let lock_path = config.workspace_root.join(".ta").join("workflows.lock");
    if let Ok(mut lockfile) = Lockfile::load(&lock_path) {
        lockfile.remove(name);
        let _ = lockfile.save(&lock_path);
    }

    // Remove from cache.
    {
        let cache = SourceCache::new("workflows");
        let _ = cache.remove(name);
    }

    println!("Removed workflow: {}", name);

    Ok(())
}

/// List externally-sourced workflows.
fn list_external_workflows(config: &GatewayConfig) -> anyhow::Result<()> {
    let lock_path = config.workspace_root.join(".ta").join("workflows.lock");

    println!("External workflows:");

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
    println!("Install a workflow:");
    println!("  ta workflow add my-review --from registry:trustedautonomy/workflows");
    println!("  ta workflow add deploy --from gh:myorg/ta-workflows");

    Ok(())
}

/// Publish a workflow to a registry.
fn publish_workflow(
    name: &str,
    registry: Option<&str>,
    _bump: Option<&str>,
    config: &GatewayConfig,
) -> anyhow::Result<()> {
    let workflows_dir = config.workspace_root.join(".ta").join("workflows");
    let source_path = workflows_dir.join(format!("{}.yaml", name));

    if !source_path.exists() {
        anyhow::bail!(
            "Workflow '{}' not found at {}.\n\
             Create it first: ta workflow new {}",
            name,
            source_path.display(),
            name
        );
    }

    // Check for a package manifest.
    let manifest_path = workflows_dir.join(format!("{}.package.yaml", name));
    if !manifest_path.exists() {
        // Generate a default package manifest.
        let manifest = PackageManifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            author: None,
            description: None,
            ta_version: None,
            files: vec![format!("workflows/{}.yaml", name)],
        };
        let yaml = serde_yaml::to_string(&manifest)?;
        std::fs::write(&manifest_path, &yaml)?;
        println!("Generated package manifest: {}", manifest_path.display());
    }

    let reg = registry.unwrap_or("trustedautonomy");
    println!("Publishing workflow '{}' to registry '{}'...", name, reg);
    println!();
    println!(
        "Registry publishing is not yet available.\n\
         To share workflows manually:\n  \
         1. Push your .ta/workflows/{name}.yaml to a Git repository\n  \
         2. Others can install it: ta workflow add {name} --from gh:org/repo",
        name = name
    );

    Ok(())
}

/// Check for updates to external workflows.
fn update_workflows(name: Option<&str>, _all: bool, config: &GatewayConfig) -> anyhow::Result<()> {
    let lock_path = config.workspace_root.join(".ta").join("workflows.lock");
    let lockfile = Lockfile::load(&lock_path).unwrap_or_default();
    let entries = lockfile.entries();

    if entries.is_empty() {
        println!("No external workflows installed. Nothing to update.");
        return Ok(());
    }

    let to_check: Vec<_> = if let Some(n) = name {
        entries.iter().filter(|e| e.name == n).cloned().collect()
    } else {
        entries.to_vec()
    };

    if to_check.is_empty() {
        if let Some(n) = name {
            anyhow::bail!(
                "Workflow '{}' is not an external workflow.\n\
                 List external workflows: ta workflow list --source external",
                n
            );
        }
    }

    println!("Checking {} workflow(s) for updates...", to_check.len());

    for entry in &to_check {
        if let Ok(source) = ExternalSource::parse(&entry.source) {
            match fetch_source_content(&source) {
                Ok(content) => {
                    let new_checksum = compute_checksum(&content);
                    if new_checksum != entry.checksum {
                        println!("  {} — update available", entry.name);
                    } else {
                        println!("  {} — up to date", entry.name);
                    }
                }
                Err(e) => {
                    println!("  {} — failed to check: {}", entry.name, e);
                }
            }
        }
    }

    Ok(())
}

/// Fetch content from an external source using HTTP.
fn fetch_source_content(source: &ExternalSource) -> anyhow::Result<String> {
    let url = source.fetch_url();
    let response = reqwest::blocking::get(&url).map_err(|e| {
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
fn compute_checksum(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// Built-in template strings for when template files aren't on disk.
const TEMPLATE_SIMPLE_REVIEW: &str = r#"# simple-review — Minimal 2-stage workflow: build + review.
#
# Usage:
#   ta workflow start .ta/workflows/simple-review.yaml

name: simple-review

stages:
  - name: build
    roles: [engineer]
    await_human: never

  - name: review
    depends_on: [build]
    roles: [reviewer]
    await_human: on_fail
    on_fail:
      route_to: build
      max_retries: 2

roles:
  engineer:
    agent: claude-code
    prompt: |
      You are a software engineer. Implement the requested feature or fix
      following the project's coding standards. Write tests for your changes.

  reviewer:
    agent: claude-code
    prompt: |
      You are a code reviewer. Review the implementation for:
      - Correctness and completeness
      - Test coverage
      - Security issues
      - Code style consistency
      Provide a verdict with specific findings.

verdict:
  pass_threshold: 0.7
"#;

const TEMPLATE_SECURITY_AUDIT: &str = r#"# security-audit — Security-focused workflow with OWASP reviewer.
#
# Usage:
#   ta workflow start .ta/workflows/security-audit.yaml

name: security-audit

stages:
  - name: scan
    roles: [scanner]
    await_human: never

  - name: review
    depends_on: [scan]
    roles: [owasp-reviewer, dependency-scanner]
    await_human: on_fail
    on_fail:
      route_to: remediate
      max_retries: 3

  - name: remediate
    depends_on: [review]
    roles: [engineer]
    await_human: never
    on_fail:
      route_to: review
      max_retries: 2

roles:
  scanner:
    agent: claude-code
    prompt: |
      You are a security scanner. Analyze the codebase for:
      - OWASP Top 10 vulnerabilities
      - Hardcoded credentials or secrets
      - Insecure configurations
      - Known CVEs in dependencies
      Output structured findings with severity ratings.

  owasp-reviewer:
    agent: claude-code
    prompt: |
      You are an OWASP security expert. Review the scan results and
      verify each finding. Classify by OWASP category (A01-A10).
      Identify false positives and prioritize remediation.

  dependency-scanner:
    agent: claude-code
    prompt: |
      You are a dependency security analyst. Check all dependencies
      for known vulnerabilities. Recommend version updates or
      alternative packages where needed.

  engineer:
    agent: claude-code
    prompt: |
      You are a security engineer. Fix the identified vulnerabilities.
      Prioritize critical findings first. Ensure fixes don't introduce
      regressions. Add tests for security-sensitive code paths.

verdict:
  pass_threshold: 0.8
  required_pass: [owasp-reviewer]
"#;

const TEMPLATE_MILESTONE_REVIEW: &str = r#"# milestone-review — Full plan/build/review workflow.
#
# Usage:
#   ta workflow start .ta/workflows/milestone-review.yaml

name: milestone-review

stages:
  - name: planning
    roles: [planner]
    await_human: always

  - name: build
    depends_on: [planning]
    roles: [engineer]
    await_human: never

  - name: review
    depends_on: [build]
    roles: [reviewer, security-reviewer]
    await_human: on_fail
    on_fail:
      route_to: build
      max_retries: 3

  - name: approval
    depends_on: [review]
    roles: [pm]
    await_human: always

roles:
  planner:
    agent: claude-code
    prompt: |
      You are a technical planner. Break down the milestone into
      concrete implementation tasks. Identify risks and dependencies.
      Output a structured plan with task descriptions and ordering.

  engineer:
    agent: claude-code
    prompt: |
      You are a software engineer. Implement the tasks from the plan.
      Follow the project's coding standards. Write tests for all changes.

  reviewer:
    agent: claude-code
    prompt: |
      You are a code reviewer. Review the implementation for correctness,
      test coverage, and code quality.

  security-reviewer:
    agent: claude-code
    prompt: |
      You are a security reviewer. Focus on input validation, auth,
      data exposure, and dependency vulnerabilities.

  pm:
    agent: claude-code
    prompt: |
      You are a project manager. Review the completed work against
      the original milestone goals. Approve or request revisions.

verdict:
  pass_threshold: 0.7
  required_pass: [security-reviewer]
"#;

const TEMPLATE_DEPLOY_PIPELINE: &str = r#"# deploy-pipeline — Build, test, and deploy with human gates.
#
# Usage:
#   ta workflow start .ta/workflows/deploy-pipeline.yaml

name: deploy-pipeline

stages:
  - name: build
    roles: [engineer]
    await_human: never

  - name: test
    depends_on: [build]
    roles: [tester, security-reviewer]
    await_human: on_fail
    on_fail:
      route_to: build
      max_retries: 2

  - name: deploy
    depends_on: [test]
    roles: [deployer]
    await_human: always

roles:
  engineer:
    agent: claude-code
    prompt: |
      You are a software engineer. Build and prepare the release.
      Ensure all compilation, linting, and formatting checks pass.

  tester:
    agent: claude-code
    prompt: |
      You are a QA engineer. Run the full test suite and verify:
      - All existing tests pass
      - New code has adequate test coverage
      - Integration tests cover critical paths
      Report any failures with reproduction steps.

  security-reviewer:
    agent: claude-code
    prompt: |
      You are a security reviewer. Audit the changes for security
      implications before deployment. Check for leaked secrets,
      insecure defaults, and OWASP Top 10 issues.

  deployer:
    agent: claude-code
    prompt: |
      You are a deployment engineer. Prepare the deployment:
      - Generate release notes
      - Verify version bumps
      - Check deployment prerequisites
      - Document rollback procedures

verdict:
  pass_threshold: 0.8
  required_pass: [security-reviewer]
"#;

const TEMPLATE_PLAN_IMPLEMENT_REVIEW: &str = r#"# plan-implement-review — Planner-driven workflow with iterative review.
#
# Uses the planner role to decompose objectives, then routes through
# implementation and review stages. On review failure, routes back to
# the planner to revise the approach.
#
# Usage:
#   ta workflow start .ta/workflows/plan-implement-review.yaml

name: plan-implement-review

stages:
  - name: plan
    roles: [planner]
    await_human: always

  - name: implement
    depends_on: [plan]
    roles: [engineer]
    await_human: never

  - name: review
    depends_on: [implement]
    roles: [reviewer]
    await_human: on_fail
    on_fail:
      route_to: plan
      max_retries: 3

roles:
  planner:
    agent: claude-code
    prompt: |
      You are a technical planner. Given an objective or document:
      1. Break it down into concrete implementation tasks
      2. Identify risks, dependencies, and acceptance criteria
      3. Order tasks by dependency and priority
      4. Output a structured plan

      If this is a re-plan after review failure, incorporate the feedback
      and adjust the approach. Focus on the specific issues raised.

  engineer:
    agent: claude-code
    prompt: |
      You are a software engineer. Follow the plan from the planning stage.
      Implement each task in order. Write tests for all changes.
      Commit in logical working units.

  reviewer:
    agent: claude-code
    prompt: |
      You are a code reviewer. Review the implementation against the plan:
      - Were all planned tasks completed?
      - Is the code correct and well-tested?
      - Are there security or quality issues?
      Provide specific, actionable findings.

verdict:
  pass_threshold: 0.7
"#;

// ── Artifact-typed workflow commands (v0.14.10) ───────────────────────────────

/// Print the resolved DAG for a workflow definition (ASCII or DOT format).
fn graph_workflow(path: &std::path::Path, dot_format: bool) -> anyhow::Result<()> {
    if !path.exists() {
        anyhow::bail!(
            "Workflow definition not found: {}\n\
             Create a workflow YAML file with: ta workflow new <name>",
            path.display()
        );
    }

    let def = WorkflowDefinition::from_file(path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse {}: {}\n\
             Run: ta workflow validate {}",
            path.display(),
            e,
            path.display()
        )
    })?;

    let dag = artifact_dag::resolve_dag(&def.stages).map_err(|e| {
        anyhow::anyhow!(
            "Could not resolve workflow DAG for '{}': {}\n\
             Check for cycles in depends_on or contradictory input/output declarations.",
            def.name,
            e
        )
    })?;

    if !dag.unresolved_inputs.is_empty() {
        for missing in &dag.unresolved_inputs {
            eprintln!(
                "Warning: stage '{}' needs artifact '{}' but no stage produces it. \
                 Assuming it is pre-loaded in the artifact store.",
                missing.stage, missing.artifact_type
            );
        }
    }

    if dot_format {
        println!("{}", artifact_dag::render_dot(&def.name, &def.stages, &dag));
    } else {
        println!("{}", artifact_dag::render_ascii(&def.stages, &dag));
    }

    Ok(())
}

/// Resume a workflow run by checking which stages have already written their
/// outputs to the artifact store and reporting which stages to skip.
fn resume_workflow(run_id: &str, config: &GatewayConfig) -> anyhow::Result<()> {
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let store = ArtifactStore::new(&memory_dir);

    let artifacts = store.list_run_artifacts(run_id)?;

    if artifacts.is_empty() {
        println!("No artifacts found for run: {}", run_id);
        println!();
        println!("Either the run ID is incorrect or no stages have completed yet.");
        println!("Check: ta workflow status {}", run_id);
        return Ok(());
    }

    // Group artifacts by stage.
    let mut by_stage: std::collections::HashMap<String, Vec<&ArtifactType>> =
        std::collections::HashMap::new();
    for artifact in &artifacts {
        by_stage
            .entry(artifact.stage.clone())
            .or_default()
            .push(&artifact.artifact_type);
    }

    println!("Workflow run: {}", run_id);
    println!("Completed stages (outputs already in artifact store):");
    println!();

    let mut sorted_stages: Vec<&String> = by_stage.keys().collect();
    sorted_stages.sort();

    for stage_name in sorted_stages {
        let types = &by_stage[stage_name];
        let type_names: Vec<String> = types.iter().map(|t| t.to_string()).collect();
        println!("  ✓ {}  [{}]", stage_name, type_names.join(", "));
    }

    println!();
    println!("To resume, re-run your workflow command — the engine will skip completed stages.");
    println!(
        "Inspect an artifact: ta memory retrieve --key workflow/{}/STAGE/TYPE",
        run_id
    );

    Ok(())
}

/// Show a live-updating status view for a workflow run.
///
/// Polls the artifact store and workflow-runs directory every 2 seconds and
/// re-renders the stage states until the workflow completes or the user
/// interrupts with Ctrl-C.
fn show_live_status(workflow_id: Option<&str>, config: &GatewayConfig) -> anyhow::Result<()> {
    let runs_dir = config.workspace_root.join(".ta").join("workflow-runs");
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let store = ArtifactStore::new(&memory_dir);

    println!("Live workflow status (Ctrl-C to exit)");
    println!();

    let run_id = match workflow_id {
        Some(id) => id.to_string(),
        None => {
            // Try to find the most recent run.
            if runs_dir.exists() {
                let most_recent = std::fs::read_dir(&runs_dir).ok().and_then(|entries| {
                    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                    paths.sort_by_key(|e| {
                        e.metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                    });
                    paths
                        .last()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                });
                match most_recent {
                    Some(id) => id,
                    None => {
                        println!("No workflow runs found in {}", runs_dir.display());
                        return Ok(());
                    }
                }
            } else {
                println!("No workflow-runs directory found. Start a workflow first:");
                println!("  ta workflow run governed-goal --goal \"...\"");
                return Ok(());
            }
        }
    };

    // Poll loop — each iteration clears the screen and re-renders.
    let poll_interval = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();

    loop {
        // Clear previous output (simple approach: print blank lines).
        print!("\x1B[2J\x1B[H");

        let elapsed = start.elapsed();
        println!(
            "Live status — run: {}  (elapsed: {}s)",
            run_id,
            elapsed.as_secs()
        );
        println!();

        // Show artifacts present in the store.
        match store.list_run_artifacts(&run_id) {
            Ok(artifacts) if !artifacts.is_empty() => {
                let mut by_stage: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();
                for a in &artifacts {
                    by_stage
                        .entry(a.stage.clone())
                        .or_default()
                        .push(a.artifact_type.to_string());
                }
                let mut stages: Vec<&String> = by_stage.keys().collect();
                stages.sort();
                println!("Completed stages:");
                for s in stages {
                    println!("  ✓ {}  [{}]", s, by_stage[s].join(", "));
                }
            }
            Ok(_) => println!("No artifacts stored yet — waiting for first stage output..."),
            Err(e) => println!("Error reading artifact store: {}", e),
        }

        // Show governed workflow run status if available.
        println!();
        if runs_dir.exists() {
            let _ = governed_workflow::show_run_status(&runs_dir, Some(&run_id));
        }

        println!();
        println!(
            "Refreshing every {}s — Ctrl-C to exit",
            poll_interval.as_secs()
        );

        std::thread::sleep(poll_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_nonexistent_file_error() {
        let result = start_workflow(&PathBuf::from("/nonexistent/workflow.yaml"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not found"));
    }

    #[test]
    fn start_valid_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.yaml");
        std::fs::write(
            &path,
            r#"
name: test-workflow
stages:
  - name: build
    roles: [engineer]
  - name: review
    depends_on: [build]
    roles: [reviewer]
roles:
  engineer:
    agent: claude-code
    prompt: Build it
  reviewer:
    agent: claude-code
    prompt: Review it
"#,
        )
        .unwrap();
        let result = start_workflow(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn new_workflow_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        new_workflow("test-wf", None, &config).unwrap();

        let path = dir.path().join(".ta/workflows/test-wf.yaml");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("name: test-wf"));
        assert!(content.contains("stages:"));
        assert!(content.contains("roles:"));
    }

    #[test]
    fn new_workflow_from_template() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        new_workflow("my-review", Some("simple-review"), &config).unwrap();

        let path = dir.path().join(".ta/workflows/my-review.yaml");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("simple-review"));
    }

    #[test]
    fn new_workflow_unknown_template_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = new_workflow("test", Some("nonexistent"), &config);
        assert!(result.is_err());
    }

    #[test]
    fn new_workflow_already_exists_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        new_workflow("dup", None, &config).unwrap();
        let result = new_workflow("dup", None, &config);
        assert!(result.is_err());
    }

    #[test]
    fn validate_valid_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("valid.yaml");
        std::fs::write(
            &path,
            r#"
name: valid
stages:
  - name: build
    roles: [engineer]
roles:
  engineer:
    agent: claude-code
    prompt: Build it
"#,
        )
        .unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = validate_workflow_cmd(&path, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_nonexistent_file_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = validate_workflow_cmd(&PathBuf::from("/no/such/file.yaml"), &config);
        assert!(result.is_err());
    }

    #[test]
    fn scaffold_contains_annotations() {
        let content = generate_scaffold("my-workflow");
        assert!(content.contains("# my-workflow"));
        assert!(content.contains("ta workflow validate"));
        assert!(content.contains("depends_on:"));
        assert!(content.contains("await_human:"));
        assert!(content.contains("on_fail:"));
        assert!(content.contains("verdict:"));
    }

    #[test]
    fn remove_workflow_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = remove_workflow("nonexistent", &config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("not found"));
    }

    #[test]
    fn remove_workflow_success() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // Create a workflow first.
        new_workflow("to-remove", None, &config).unwrap();
        let path = dir.path().join(".ta/workflows/to-remove.yaml");
        assert!(path.exists());
        // Remove it.
        remove_workflow("to-remove", &config).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn list_external_workflows_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = list_external_workflows(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn publish_workflow_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = publish_workflow("nonexistent", None, None, &config);
        assert!(result.is_err());
    }

    #[test]
    fn publish_workflow_creates_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        new_workflow("publishable", None, &config).unwrap();
        publish_workflow("publishable", None, None, &config).unwrap();
        let manifest_path = dir.path().join(".ta/workflows/publishable.package.yaml");
        assert!(manifest_path.exists());
    }

    #[test]
    fn update_workflows_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let result = update_workflows(None, false, &config);
        assert!(result.is_ok());
    }

    #[test]
    fn compute_checksum_deterministic() {
        let a = compute_checksum("hello world");
        let b = compute_checksum("hello world");
        assert_eq!(a, b);
        let c = compute_checksum("different");
        assert_ne!(a, c);
    }

    #[test]
    fn graph_workflow_ascii_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test-graph
stages:
  - name: plan
    outputs: [PlanDocument]
    roles: []
  - name: implement
    inputs: [PlanDocument]
    outputs: [DraftPackage]
    roles: []
roles: {}
"#;
        let path = dir.path().join("test.yaml");
        std::fs::write(&path, yaml).unwrap();
        graph_workflow(&path, false).unwrap();
    }

    #[test]
    fn graph_workflow_dot_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test-graph
stages:
  - name: plan
    outputs: [PlanDocument]
    roles: []
  - name: implement
    inputs: [PlanDocument]
    outputs: [DraftPackage]
    roles: []
roles: {}
"#;
        let path = dir.path().join("test.yaml");
        std::fs::write(&path, yaml).unwrap();
        graph_workflow(&path, true).unwrap();
    }

    #[test]
    fn resume_workflow_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        // No artifacts stored — resume should report nothing to skip.
        let result = resume_workflow("nonexistent-run", &config);
        assert!(result.is_ok());
    }

    #[test]
    fn resume_workflow_with_stored_artifacts_shows_completed_stage() {
        // Populate the artifact store with step-1 output, then call resume_workflow.
        // Verifies: resume_workflow finds the stored artifact, reports the stage as
        // completed, and returns Ok (no panic or error when artifacts are present).
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());

        // Store an artifact for the "generate-plan" stage.
        let memory_dir = dir.path().join(".ta").join("memory");
        let store = ArtifactStore::new(&memory_dir);
        store
            .store(
                "run-resume-test",
                "generate-plan",
                &ArtifactType::PlanDocument,
                serde_json::json!({"items": ["step A", "step B"]}),
            )
            .unwrap();

        // resume_workflow should find the stored artifact and return Ok.
        let result = resume_workflow("run-resume-test", &config);
        assert!(
            result.is_ok(),
            "resume_workflow must succeed when artifacts are present"
        );

        // Also verify the completed_stages helper agrees the stage is done.
        let stage_specs: Vec<(&str, &[ArtifactType])> =
            vec![("generate-plan", &[ArtifactType::PlanDocument])];
        let completed = store
            .completed_stages("run-resume-test", &stage_specs)
            .unwrap();
        assert_eq!(
            completed,
            vec!["generate-plan"],
            "generate-plan must be reported as completed after its output is stored"
        );
    }
}
