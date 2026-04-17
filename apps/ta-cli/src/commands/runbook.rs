// commands/runbook.rs — Operational runbooks (v0.13.1.6).
//
// Runbooks automate common recovery procedures as sequenced steps.
//
// Built-in runbooks:
//   disk-pressure    — identify and clean staging dirs when disk is tight
//   zombie-goals     — recover goals whose agent process has died
//   crashed-plugins  — detect and restart failed channel plugins
//   stale-drafts     — list and clean up drafts stuck in PendingReview
//   failed-ci        — diagnose and re-run a failed CI verification
//
// Project-local runbooks: YAML files in `.ta/runbooks/<name>.yaml`.

use std::path::Path;

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum RunbookCommands {
    /// List all available runbooks (built-in + project-local).
    List,
    /// Show the steps of a runbook without running it.
    Show {
        /// Runbook name (e.g., disk-pressure).
        name: String,
    },
    /// Run a runbook interactively, presenting each step for approval.
    Run {
        /// Runbook name (e.g., disk-pressure, zombie-goals, stale-drafts).
        name: String,
        /// Skip confirmation prompts for steps marked auto_approve.
        /// Non-auto-approve steps always require confirmation.
        #[arg(long)]
        auto: bool,
        /// Show what would be executed without actually running anything.
        #[arg(long)]
        dry_run: bool,
    },
}

/// A single step within a runbook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunbookStep {
    /// Unique ID for this step (used in logging).
    pub id: String,
    /// Human-readable name shown before execution.
    pub name: String,
    /// TA CLI command to run (everything after `ta`).
    /// Example: `gc --dry-run --compact` → runs `ta gc --dry-run --compact`.
    pub command: String,
    /// Optional description of what this step does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// If true, this step can run without explicit user confirmation.
    #[serde(default)]
    pub auto_approve: bool,
}

/// An optional trigger condition for a runbook (informational only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunbookTrigger {
    /// Human-readable description of when this runbook should be used.
    pub condition: String,
    /// Suggested severity level: "info", "warning", or "critical".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

/// A complete runbook definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunbookDefinition {
    /// Unique identifier (matches the filename without `.yaml`).
    pub name: String,
    /// Short description of what this runbook does.
    pub description: String,
    /// When this runbook should be triggered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<RunbookTrigger>,
    /// Ordered list of steps to execute.
    pub steps: Vec<RunbookStep>,
}

pub fn execute(command: &RunbookCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        RunbookCommands::List => list_runbooks(config),
        RunbookCommands::Show { name } => show_runbook(config, name),
        RunbookCommands::Run {
            name,
            auto,
            dry_run,
        } => run_runbook(config, name, *auto, *dry_run),
    }
}

// ── Built-in runbooks ──────────────────────────────────────────────────────

/// Return all built-in runbooks.
pub fn builtin_runbooks() -> Vec<RunbookDefinition> {
    vec![
        RunbookDefinition {
            name: "disk-pressure".to_string(),
            description: "Clean up staging directories to free disk space.".to_string(),
            trigger: Some(RunbookTrigger {
                condition: "Available disk < 2 GB or staging dirs > 20 GB total".to_string(),
                severity: Some("critical".to_string()),
            }),
            steps: vec![
                RunbookStep {
                    id: "survey".to_string(),
                    name: "Survey disk usage".to_string(),
                    command: "status --deep".to_string(),
                    description: Some(
                        "Show current disk usage for staging directories and draft packages."
                            .to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "dry-run-compact".to_string(),
                    name: "Preview compaction candidates".to_string(),
                    command: "gc --compact --dry-run".to_string(),
                    description: Some(
                        "Show which applied/closed staging directories would be removed."
                            .to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "compact".to_string(),
                    name: "Compact old staging directories".to_string(),
                    command: "gc --compact".to_string(),
                    description: Some(
                        "Remove staging directories for goals older than 30 days that are in a terminal state."
                            .to_string(),
                    ),
                    auto_approve: false,
                },
                RunbookStep {
                    id: "verify".to_string(),
                    name: "Verify disk pressure resolved".to_string(),
                    command: "status --deep".to_string(),
                    description: Some("Confirm disk usage has improved.".to_string()),
                    auto_approve: true,
                },
            ],
        },
        RunbookDefinition {
            name: "zombie-goals".to_string(),
            description: "Recover goals whose agent process has died without transitioning state."
                .to_string(),
            trigger: Some(RunbookTrigger {
                condition: "Goals in Running state with no heartbeat for > 30 minutes".to_string(),
                severity: Some("warning".to_string()),
            }),
            steps: vec![
                RunbookStep {
                    id: "list-running".to_string(),
                    name: "List running goals".to_string(),
                    command: "goal list --state running".to_string(),
                    description: Some(
                        "Show all goals currently in Running or Configured state.".to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "operations-log".to_string(),
                    name: "Review corrective actions".to_string(),
                    command: "operations log --severity warning".to_string(),
                    description: Some(
                        "Show watchdog-detected zombie goals and proposed actions.".to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "gc-stale".to_string(),
                    name: "Clean up stale/zombie goals".to_string(),
                    command: "gc --threshold-days 1 --dry-run".to_string(),
                    description: Some(
                        "Preview which goals qualify for cleanup (dry run first).".to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "gc-execute".to_string(),
                    name: "Execute goal cleanup".to_string(),
                    command: "gc --threshold-days 1".to_string(),
                    description: Some(
                        "Remove stale goal records. Staging dirs are preserved by default."
                            .to_string(),
                    ),
                    auto_approve: false,
                },
            ],
        },
        RunbookDefinition {
            name: "crashed-plugins".to_string(),
            description: "Detect and recover crashed or unresponsive channel plugins.".to_string(),
            trigger: Some(RunbookTrigger {
                condition: "Channel plugin process has exited or is not responding".to_string(),
                severity: Some("warning".to_string()),
            }),
            steps: vec![
                RunbookStep {
                    id: "list-plugins".to_string(),
                    name: "List installed plugins".to_string(),
                    command: "plugin list".to_string(),
                    description: Some("Show all installed plugins and their status.".to_string()),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "check-plugins".to_string(),
                    name: "Check plugin compatibility".to_string(),
                    command: "plugin check".to_string(),
                    description: Some(
                        "Validate that all installed plugins are compatible with the current daemon version."
                            .to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "daemon-restart".to_string(),
                    name: "Restart daemon (reloads plugins)".to_string(),
                    command: "daemon restart".to_string(),
                    description: Some(
                        "Restart the daemon, which re-loads and re-starts all channel plugins."
                            .to_string(),
                    ),
                    auto_approve: false,
                },
                RunbookStep {
                    id: "daemon-status".to_string(),
                    name: "Verify daemon and plugins are healthy".to_string(),
                    command: "daemon status".to_string(),
                    description: Some("Confirm daemon started cleanly and plugins are active.".to_string()),
                    auto_approve: true,
                },
            ],
        },
        RunbookDefinition {
            name: "stale-drafts".to_string(),
            description:
                "Identify and clean up draft packages stuck in PendingReview for a long time."
                    .to_string(),
            trigger: Some(RunbookTrigger {
                condition: "Drafts in PendingReview for > 7 days".to_string(),
                severity: Some("info".to_string()),
            }),
            steps: vec![
                RunbookStep {
                    id: "list-drafts".to_string(),
                    name: "List pending drafts".to_string(),
                    command: "draft list".to_string(),
                    description: Some(
                        "Show all drafts currently awaiting review, with their age.".to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "gc-drafts".to_string(),
                    name: "Preview draft GC candidates".to_string(),
                    command: "gc --threshold-days 7 --dry-run".to_string(),
                    description: Some(
                        "Show which stale drafts qualify for garbage collection.".to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "gc-execute".to_string(),
                    name: "Remove stale drafts".to_string(),
                    command: "gc --threshold-days 7".to_string(),
                    description: Some(
                        "Delete draft packages older than 7 days. Denied and applied drafts are always included."
                            .to_string(),
                    ),
                    auto_approve: false,
                },
            ],
        },
        RunbookDefinition {
            name: "failed-ci".to_string(),
            description:
                "Diagnose and re-run failed verification checks for a staged goal.".to_string(),
            trigger: Some(RunbookTrigger {
                condition: "ta verify fails for an active goal".to_string(),
                severity: Some("warning".to_string()),
            }),
            steps: vec![
                RunbookStep {
                    id: "list-goals".to_string(),
                    name: "List active goals".to_string(),
                    command: "goal list".to_string(),
                    description: Some(
                        "Identify the goal ID whose verification is failing.".to_string(),
                    ),
                    auto_approve: true,
                },
                RunbookStep {
                    id: "verify".to_string(),
                    name: "Re-run verification".to_string(),
                    command: "verify".to_string(),
                    description: Some(
                        "Run [verify] commands from workflow.toml in the most recent active goal's staging dir. See output for specific failures.".to_string(),
                    ),
                    auto_approve: false,
                },
                RunbookStep {
                    id: "follow-up".to_string(),
                    name: "Launch follow-up goal to fix failures".to_string(),
                    command: "run --follow-up".to_string(),
                    description: Some(
                        "Start a new agent session to fix the failures. The agent will see the failed verification output in context.".to_string(),
                    ),
                    auto_approve: false,
                },
            ],
        },
    ]
}

// ── Command implementations ────────────────────────────────────────────────

fn list_runbooks(config: &GatewayConfig) -> anyhow::Result<()> {
    let builtins = builtin_runbooks();
    let project_runbooks = load_project_runbooks(&config.workspace_root);

    println!("Available runbooks:");
    println!();

    println!("  Built-in:");
    for rb in &builtins {
        let trigger_note = rb
            .trigger
            .as_ref()
            .and_then(|t| t.severity.as_deref())
            .map(|s| format!(" [{}]", s))
            .unwrap_or_default();
        println!("    {:20} {}{}", rb.name, rb.description, trigger_note);
    }

    if !project_runbooks.is_empty() {
        println!();
        println!("  Project-local (.ta/runbooks/):");
        for rb in &project_runbooks {
            println!("    {:20} {}", rb.name, rb.description);
        }
    } else {
        println!();
        println!("  Project-local (.ta/runbooks/): none");
        println!("  (Create YAML files in .ta/runbooks/ to add project-specific runbooks)");
    }

    println!();
    println!("Run `ta runbook show <name>` to see steps.");
    println!("Run `ta runbook run <name>` to execute.");

    Ok(())
}

fn show_runbook(config: &GatewayConfig, name: &str) -> anyhow::Result<()> {
    let rb = find_runbook(config, name)?;

    println!("Runbook: {}", rb.name);
    println!("  {}", rb.description);

    if let Some(trigger) = &rb.trigger {
        let sev = trigger
            .severity
            .as_deref()
            .map(|s| format!(" [{}]", s))
            .unwrap_or_default();
        println!("  Trigger: {}{}", trigger.condition, sev);
    }

    println!();
    println!("Steps ({}):", rb.steps.len());
    for (i, step) in rb.steps.iter().enumerate() {
        let approval = if step.auto_approve {
            "(auto)"
        } else {
            "(requires approval)"
        };
        println!("  {}. {} {}", i + 1, step.name, approval);
        println!("     command: ta {}", step.command);
        if let Some(desc) = &step.description {
            println!("     {}", desc);
        }
    }

    println!();
    println!("Run with: ta runbook run {}", rb.name);

    Ok(())
}

fn run_runbook(
    config: &GatewayConfig,
    name: &str,
    auto: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let rb = find_runbook(config, name)?;

    println!("Runbook: {} — {}", rb.name, rb.description);
    if dry_run {
        println!("[DRY RUN — no commands will be executed]");
    }
    println!();

    for (i, step) in rb.steps.iter().enumerate() {
        let total = rb.steps.len();
        println!("Step {}/{}: {}", i + 1, total, step.name);
        println!("  Command: ta {}", step.command);
        if let Some(desc) = &step.description {
            println!("  {}", desc);
        }

        if dry_run {
            println!("  [dry run — skipped]");
            println!();
            continue;
        }

        // Determine whether to ask for approval.
        let should_ask = !step.auto_approve || !auto;
        if should_ask {
            let prompt = if step.auto_approve {
                "Run this step? [Y/n/skip/abort] (auto-approve eligible): ".to_string()
            } else {
                "Run this step? [Y/n/skip/abort]: ".to_string()
            };

            let response = prompt_user(&prompt);
            match response.trim().to_lowercase().as_str() {
                "n" | "no" | "abort" | "q" | "quit" => {
                    println!("Runbook aborted at step {}/{}.", i + 1, total);
                    return Ok(());
                }
                "s" | "skip" => {
                    println!("  Skipped.");
                    println!();
                    continue;
                }
                _ => {
                    // "y", "yes", "" → proceed
                }
            }
        } else {
            println!("  [auto-approve — running]");
        }

        // Execute the step.
        let exit_code = execute_step(config, &step.command);
        match exit_code {
            Ok(0) => {
                println!("  ✓ Step {} succeeded.", i + 1);
            }
            Ok(code) => {
                println!("  ✗ Step {} exited with code {}.", i + 1, code);
                let response = prompt_user("Continue despite failure? [y/N/abort]: ");
                match response.trim().to_lowercase().as_str() {
                    "y" | "yes" => {}
                    _ => {
                        println!("Runbook aborted after step {} failure.", i + 1);
                        return Ok(());
                    }
                }
            }
            Err(e) => {
                println!("  ✗ Step {} failed to execute: {}", i + 1, e);
                println!("  Command was: ta {}", step.command);
                let response = prompt_user("Continue despite error? [y/N/abort]: ");
                match response.trim().to_lowercase().as_str() {
                    "y" | "yes" => {}
                    _ => {
                        println!("Runbook aborted after step {} error.", i + 1);
                        return Ok(());
                    }
                }
            }
        }

        println!();
    }

    if !dry_run {
        println!(
            "Runbook '{}' completed ({} steps).",
            rb.name,
            rb.steps.len()
        );
    }

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Find a runbook by name: project-local first, then built-in.
fn find_runbook(config: &GatewayConfig, name: &str) -> anyhow::Result<RunbookDefinition> {
    // Check project-local first.
    let project_runbooks = load_project_runbooks(&config.workspace_root);
    if let Some(rb) = project_runbooks.into_iter().find(|r| r.name == name) {
        return Ok(rb);
    }

    // Check built-ins.
    if let Some(rb) = builtin_runbooks().into_iter().find(|r| r.name == name) {
        return Ok(rb);
    }

    let available: Vec<String> = builtin_runbooks().into_iter().map(|r| r.name).collect();
    Err(anyhow::anyhow!(
        "Runbook '{}' not found.\nAvailable built-in runbooks: {}\nProject-local runbooks go in .ta/runbooks/<name>.yaml",
        name,
        available.join(", ")
    ))
}

/// Load project-local runbooks from `.ta/runbooks/*.yaml`.
fn load_project_runbooks(project_root: &Path) -> Vec<RunbookDefinition> {
    let runbooks_dir = project_root.join(".ta/runbooks");
    if !runbooks_dir.exists() {
        return vec![];
    }

    let mut runbooks = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&runbooks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match serde_yaml::from_str::<RunbookDefinition>(&content) {
                        Ok(rb) => runbooks.push(rb),
                        Err(e) => {
                            eprintln!("Warning: failed to parse runbook {}: {}", path.display(), e);
                        }
                    },
                    Err(e) => {
                        eprintln!("Warning: failed to read {}: {}", path.display(), e);
                    }
                }
            }
        }
    }

    runbooks.sort_by_key(|r| r.name.clone());
    runbooks
}

/// Execute a single runbook step by spawning `ta <command>` as a subprocess.
///
/// Returns the exit code, or an error if the process couldn't be spawned.
fn execute_step(_config: &GatewayConfig, command: &str) -> anyhow::Result<i32> {
    // Find the current ta binary.
    let ta_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ta"));

    let args: Vec<&str> = command.split_whitespace().collect();

    let status = std::process::Command::new(&ta_bin)
        .args(&args)
        .status()
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to spawn '{}': {}. Install ta or ensure it is on PATH.",
                ta_bin.display(),
                e
            )
        })?;

    Ok(status.code().unwrap_or(1))
}

/// Prompt the user for input and return the trimmed response.
fn prompt_user(prompt: &str) -> String {
    use std::io::Write as _;
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_runbooks_all_present() {
        let runbooks = builtin_runbooks();
        let names: Vec<&str> = runbooks.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"disk-pressure"));
        assert!(names.contains(&"zombie-goals"));
        assert!(names.contains(&"crashed-plugins"));
        assert!(names.contains(&"stale-drafts"));
        assert!(names.contains(&"failed-ci"));
    }

    #[test]
    fn builtin_runbooks_have_steps() {
        for rb in builtin_runbooks() {
            assert!(!rb.steps.is_empty(), "runbook '{}' has no steps", rb.name);
        }
    }

    #[test]
    fn builtin_runbooks_steps_have_commands() {
        for rb in builtin_runbooks() {
            for step in &rb.steps {
                assert!(
                    !step.command.is_empty(),
                    "step '{}' in '{}' has empty command",
                    step.id,
                    rb.name
                );
            }
        }
    }

    #[test]
    fn load_project_runbooks_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let runbooks = load_project_runbooks(dir.path());
        assert!(runbooks.is_empty());
    }

    #[test]
    fn load_project_runbooks_valid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let runbooks_dir = dir.path().join(".ta/runbooks");
        std::fs::create_dir_all(&runbooks_dir).unwrap();

        let yaml = r#"
name: my-runbook
description: Test runbook
steps:
  - id: step1
    name: Show status
    command: status
    auto_approve: true
"#;
        std::fs::write(runbooks_dir.join("my-runbook.yaml"), yaml).unwrap();
        let runbooks = load_project_runbooks(dir.path());
        assert_eq!(runbooks.len(), 1);
        assert_eq!(runbooks[0].name, "my-runbook");
        assert_eq!(runbooks[0].steps.len(), 1);
    }

    #[test]
    fn find_runbook_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let config = ta_mcp_gateway::GatewayConfig::for_project(dir.path());
        let rb = find_runbook(&config, "disk-pressure").unwrap();
        assert_eq!(rb.name, "disk-pressure");
    }

    #[test]
    fn find_runbook_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let config = ta_mcp_gateway::GatewayConfig::for_project(dir.path());
        let result = find_runbook(&config, "nonexistent-runbook");
        assert!(result.is_err());
    }

    #[test]
    fn runbook_definition_roundtrip_yaml() {
        let rb = RunbookDefinition {
            name: "test".to_string(),
            description: "A test runbook".to_string(),
            trigger: Some(RunbookTrigger {
                condition: "when things go wrong".to_string(),
                severity: Some("warning".to_string()),
            }),
            steps: vec![RunbookStep {
                id: "s1".to_string(),
                name: "Step 1".to_string(),
                command: "status".to_string(),
                description: None,
                auto_approve: true,
            }],
        };
        let yaml = serde_yaml::to_string(&rb).unwrap();
        let restored: RunbookDefinition = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(restored.name, "test");
        assert_eq!(restored.steps.len(), 1);
        assert!(restored.steps[0].auto_approve);
    }
}
