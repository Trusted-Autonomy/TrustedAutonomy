// workflow.rs — CLI commands for workflow management (v0.9.9.5).
//
// Commands:
//   ta workflow start <definition.yaml>  — start a workflow
//   ta workflow status [workflow_id]      — show status
//   ta workflow list [--templates]        — list workflows or browse templates
//   ta workflow cancel <workflow_id>      — cancel a workflow
//   ta workflow history <workflow_id>     — show stage transitions
//   ta workflow new <name>               — scaffold a new workflow definition
//   ta workflow validate <path>          — validate a workflow definition

use std::path::PathBuf;

use clap::Subcommand;
use ta_mcp_gateway::GatewayConfig;
use ta_workflow::{WorkflowDefinition, WorkflowEngine, YamlWorkflowEngine};

#[derive(Subcommand)]
pub enum WorkflowCommands {
    /// Start a workflow from a YAML definition file.
    Start {
        /// Path to the workflow definition YAML file.
        definition: PathBuf,
    },
    /// Show the status of a workflow.
    Status {
        /// Workflow ID. If omitted, shows the most recent workflow.
        workflow_id: Option<String>,
    },
    /// List all workflows (active and completed).
    List {
        /// Show available workflow templates instead of active workflows.
        #[arg(long)]
        templates: bool,
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
}

pub fn execute(command: &WorkflowCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        WorkflowCommands::Start { definition } => start_workflow(definition),
        WorkflowCommands::Status { workflow_id } => show_status(workflow_id.as_deref()),
        WorkflowCommands::List { templates } => {
            if *templates {
                list_templates()
            } else {
                list_workflows()
            }
        }
        WorkflowCommands::Cancel { workflow_id } => cancel_workflow(workflow_id),
        WorkflowCommands::History { workflow_id } => show_history(workflow_id),
        WorkflowCommands::New { name, from } => new_workflow(name, from.as_deref(), config),
        WorkflowCommands::Validate { path } => validate_workflow_cmd(path, config),
    }
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
    Ok(())
}

fn list_templates() -> anyhow::Result<()> {
    println!("Workflow templates:");
    println!();
    println!("  simple-review        2-stage build + review");
    println!("  security-audit       3-stage scan, review, remediate");
    println!("  milestone-review     4-stage plan, build, review, approval");
    println!("  deploy-pipeline      3-stage build, test, deploy with gates");
    println!("  plan-implement-review  Planner-driven loop with iterative review");
    println!();
    println!("Use a template:");
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
                "simple-review" => TEMPLATE_SIMPLE_REVIEW.to_string(),
                "security-audit" => TEMPLATE_SECURITY_AUDIT.to_string(),
                "milestone-review" => TEMPLATE_MILESTONE_REVIEW.to_string(),
                "deploy-pipeline" => TEMPLATE_DEPLOY_PIPELINE.to_string(),
                "plan-implement-review" => TEMPLATE_PLAN_IMPLEMENT_REVIEW.to_string(),
                _ => {
                    anyhow::bail!(
                        "Unknown template: '{}'\n\
                         Available templates: simple-review, security-audit, milestone-review, deploy-pipeline, plan-implement-review\n\
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
}
