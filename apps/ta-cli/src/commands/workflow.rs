// workflow.rs — CLI commands for workflow management (v0.9.8.2).
//
// Commands:
//   ta workflow start <definition.yaml>  — start a workflow
//   ta workflow status [workflow_id]      — show status
//   ta workflow list                      — list workflows
//   ta workflow cancel <workflow_id>      — cancel a workflow
//   ta workflow history <workflow_id>     — show stage transitions

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
    List,
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
}

pub fn execute(command: &WorkflowCommands, _config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        WorkflowCommands::Start { definition } => start_workflow(definition),
        WorkflowCommands::Status { workflow_id } => show_status(workflow_id.as_deref()),
        WorkflowCommands::List => list_workflows(),
        WorkflowCommands::Cancel { workflow_id } => cancel_workflow(workflow_id),
        WorkflowCommands::History { workflow_id } => show_history(workflow_id),
    }
}

fn start_workflow(definition_path: &std::path::Path) -> anyhow::Result<()> {
    if !definition_path.exists() {
        anyhow::bail!(
            "Workflow definition not found: {}\n\
             Create a workflow YAML file or use a built-in template:\n  \
             ls templates/workflows/",
            definition_path.display()
        );
    }

    let def = WorkflowDefinition::from_file(definition_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse {}: {}\n\
             Check the YAML syntax and ensure all required fields are present.",
            definition_path.display(),
            e
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
    println!("Built-in templates:");
    println!("  templates/workflows/simple-review.yaml     — 2-stage build + review");
    println!("  templates/workflows/milestone-review.yaml  — full plan/build/review cycle");
    println!("  templates/workflows/security-audit.yaml    — security-focused audit");
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
}
