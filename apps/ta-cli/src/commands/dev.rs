// dev.rs — Interactive developer loop (`ta dev`).
//
// Launches an orchestration agent that coordinates the development loop:
// reads the plan, suggests next goals, launches implementation agents,
// handles draft review, and manages releases — all from one persistent session.
//
// Unlike `ta run`, `ta dev` does NOT create a staging workspace. The agent
// operates in the project directory with read-only access, using TA's MCP
// tools (ta_plan, ta_goal, ta_draft, ta_context) for all actions.
//
// v0.9.3: Security hardening — orchestrator is restricted by default.
// `--unrestricted` flag bypasses restrictions for power users.

use std::path::Path;

use ta_mcp_gateway::GatewayConfig;

use super::plan;
use super::run::{build_memory_context_section_for_inject, restore_mcp_server_config};

/// Minimal agent config for the dev-loop orchestrator.
#[derive(serde::Deserialize, Clone, Debug)]
struct DevLoopConfig {
    command: String,
    args_template: Vec<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
}

/// Build the system prompt for the dev-loop agent.
///
/// Includes plan status, pending phases, memory context, and instructions
/// for using MCP tools.
fn build_dev_prompt(project_root: &Path, config: &GatewayConfig, unrestricted: bool) -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "You are the TA development orchestrator. Your job is to coordinate the development loop \
         for this project using TA's MCP tools. You do NOT write code directly — you launch \
         implementation agents via sub-goals.\n\n",
    );

    prompt.push_str("## Your Capabilities\n\n");
    prompt.push_str(
        "- **Read the plan**: Use `ta_plan` with action \"read\" to see project status\n",
    );
    prompt.push_str(
        "- **Start goals**: Use `ta_goal` with action \"start\" to launch implementation agents\n",
    );
    prompt.push_str(
        "- **Review drafts**: Use `ta_draft` to list, view, approve, or deny agent work\n",
    );
    prompt.push_str("- **Search memory**: Use `ta_context` to search project memory and context\n");
    prompt.push_str("- **Cut releases**: Use `ta_release` to run the release pipeline\n");
    prompt.push_str("- **Watch events**: Use `ta_event_subscribe` to check for goal completions, failures, and draft events without polling\n\n");

    prompt.push_str("## Workflow\n\n");
    prompt.push_str("1. Show the current plan status and next pending phase\n");
    prompt.push_str("2. When the user says \"run that\" or names a phase, launch a sub-goal\n");
    prompt.push_str("3. When an implementation agent finishes, review its draft\n");
    prompt.push_str("4. Approve good work, deny and re-launch if needed\n");
    prompt.push_str("5. Cut releases when milestones are reached\n\n");

    // Security boundaries — included unless --unrestricted.
    if unrestricted {
        prompt.push_str("## Security Mode: UNRESTRICTED\n\n");
        prompt.push_str(
            "This session is running in unrestricted mode. You have full access to all tools \
             including Write, Edit, Bash, and network operations. Use with caution.\n\n",
        );
    } else {
        prompt.push_str("## Security Boundaries\n\n");
        prompt.push_str(
            "You are a **read-only orchestrator**. You MUST NOT write files, execute shell commands, \
             or make outbound change operations. Specifically:\n\n",
        );
        prompt.push_str("- **No file writes**: Do not use Write, Edit, NotebookEdit, or any tool that modifies files\n");
        prompt.push_str("- **No shell access**: Do not use Bash or any shell execution tool\n");
        prompt.push_str("- **No outbound mutations**: Do not make HTTP POST/PUT/DELETE requests or modify external resources\n");
        prompt.push_str(
            "- **Read-only project access**: You may use Read, Grep, Glob to inspect the project\n",
        );
        prompt.push_str("- **TA MCP tools only**: All actions go through `ta_plan`, `ta_goal`, `ta_draft`, `ta_context`, `ta_release`, `ta_event_subscribe`\n\n");
        prompt.push_str("Implementation happens in **sub-goals** — you launch them, review their drafts, and approve or deny.\n\n");
    }

    prompt.push_str("## Natural Language Commands\n\n");
    prompt.push_str("Respond to conversational requests like:\n");
    prompt.push_str("- \"what's next\" → show next pending phase\n");
    prompt.push_str("- \"status\" or \"show plan\" → display plan progress\n");
    prompt.push_str("- \"run v0.7.6\" or \"run that\" → launch a goal for the phase\n");
    prompt.push_str("- \"show drafts\" → list pending drafts\n");
    prompt.push_str("- \"approve <id>\" → approve a draft\n");
    prompt.push_str("- \"release\" → run the release pipeline\n");
    prompt.push_str("- \"context search X\" → search project memory\n");
    prompt.push_str("- \"check events\" → query recent events for goal completions/failures\n\n");

    // Include current plan status.
    let plan_section = build_plan_summary(project_root);
    if !plan_section.is_empty() {
        prompt.push_str("## Current Plan Status\n\n");
        prompt.push_str(&plan_section);
        prompt.push('\n');
    }

    // Include pending drafts summary.
    let drafts_section = build_drafts_summary(config);
    if !drafts_section.is_empty() {
        prompt.push_str("## Pending Drafts\n\n");
        prompt.push_str(&drafts_section);
        prompt.push('\n');
    }

    // Include project memory context (v0.8.2: fix missing memory injection).
    let memory_section =
        build_memory_context_section_for_inject(config, "dev-loop orchestration", None);
    if !memory_section.is_empty() {
        prompt.push_str("## Prior Context (from TA memory)\n\n");
        prompt.push_str(&memory_section);
        prompt.push('\n');
    }

    prompt
}

/// Print plan progress summary to stdout before launching the agent (v0.8.2).
///
/// Shows done/total, next actionable phase, and any pending drafts — giving
/// the user immediate context.
fn print_plan_status_to_terminal(project_root: &Path) {
    let phases = match plan::load_plan(project_root) {
        Ok(p) => p,
        Err(_) => return,
    };

    if phases.is_empty() {
        return;
    }

    let total = phases.len();
    let done = phases
        .iter()
        .filter(|p| p.status == plan::PlanStatus::Done)
        .count();
    let deferred = phases
        .iter()
        .filter(|p| p.status == plan::PlanStatus::Deferred)
        .count();

    println!();
    println!("  Plan: {}/{} phases complete", done, total);
    if deferred > 0 {
        println!("  Deferred: {}", deferred);
    }

    if let Some(next) = plan::find_next_pending(&phases, None) {
        println!("  Next: {} — {}", next.id, next.title);
    }
}

/// Build a summary of plan progress for the dev-loop prompt.
fn build_plan_summary(project_root: &Path) -> String {
    let phases = match plan::load_plan(project_root) {
        Ok(p) => p,
        Err(_) => return "No PLAN.md found.\n".to_string(),
    };

    if phases.is_empty() {
        return "Plan is empty.\n".to_string();
    }

    let total = phases.len();
    let done = phases
        .iter()
        .filter(|p| p.status == plan::PlanStatus::Done)
        .count();

    let mut summary = format!("Progress: {}/{} phases complete.\n\n", done, total);

    // Show the checklist.
    let checklist = plan::format_plan_checklist(&phases, None);
    summary.push_str(&checklist);
    summary.push('\n');

    // Highlight next actionable phase (skips deferred).
    if let Some(next) = plan::find_next_pending(&phases, None) {
        summary.push_str(&format!(
            "\nNext pending: **{} — {}**\n",
            next.id, next.title
        ));
    }

    summary
}

/// Build a summary of pending drafts for the dev-loop prompt.
fn build_drafts_summary(config: &GatewayConfig) -> String {
    use ta_changeset::draft_package::{DraftPackage, DraftStatus};

    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return String::new();
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    let mut pending = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Ok(json) = std::fs::read_to_string(&path) {
                if let Ok(pkg) = serde_json::from_str::<DraftPackage>(&json) {
                    if matches!(pkg.status, DraftStatus::PendingReview) {
                        pending.push(pkg);
                    }
                }
            }
        }
    }

    if pending.is_empty() {
        return "No pending drafts.\n".to_string();
    }

    let mut summary = format!("{} draft(s) pending review:\n", pending.len());
    for draft in &pending {
        summary.push_str(&format!(
            "- {} — {} ({})\n",
            &draft.package_id.to_string()[..8],
            draft.summary.what_changed,
            draft.status
        ));
    }

    summary
}

/// Load agent config for the dev-loop agent from YAML or fall back to defaults.
fn load_dev_config(project_root: &Path, unrestricted: bool) -> DevLoopConfig {
    // In unrestricted mode, skip the restrictive YAML config and use a permissive fallback.
    if unrestricted {
        return DevLoopConfig {
            command: "claude".to_string(),
            args_template: vec!["--system-prompt".to_string(), "{prompt}".to_string()],
            env: Default::default(),
        };
    }

    let filename = "dev-loop.yaml";

    // 1. Project override: .ta/agents/dev-loop.yaml
    let project_path = project_root.join(".ta").join("agents").join(filename);
    if let Some(config) = try_load_config(&project_path) {
        return config;
    }

    // 2. User override: ~/.config/ta/agents/dev-loop.yaml
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let user_path = std::path::PathBuf::from(home)
            .join(".config")
            .join("ta")
            .join("agents")
            .join(filename);
        if let Some(config) = try_load_config(&user_path) {
            return config;
        }
    }

    // 3. Shipped defaults: <binary-dir>/agents/dev-loop.yaml
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let shipped_path = bin_dir.join("agents").join(filename);
            if let Some(config) = try_load_config(&shipped_path) {
                return config;
            }
        }
    }

    // 4. Hard-coded fallback.
    // Uses --system-prompt (not -p) so Claude stays in interactive mode
    // instead of processing a single prompt and exiting.
    DevLoopConfig {
        command: "claude".to_string(),
        args_template: vec![
            "--allowedTools".to_string(),
            "mcp__ta__ta_plan,mcp__ta__ta_goal,mcp__ta__ta_draft,mcp__ta__ta_context,mcp__ta__ta_release,mcp__ta__ta_event_subscribe,Read,Grep,Glob,WebFetch,WebSearch".to_string(),
            "--system-prompt".to_string(),
            "{prompt}".to_string(),
        ],
        env: Default::default(),
    }
}

fn try_load_config(path: &Path) -> Option<DevLoopConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&content).ok()
}

/// Write an audit log entry for the dev session start/end.
///
/// Uses the `.ta/dev-audit.log` file, appending JSON lines with session ID,
/// timestamp, event type, and optional context.
fn write_dev_audit(project_root: &Path, session_id: &str, event: &str, context: Option<&str>) {
    let ta_dir = project_root.join(".ta");
    if std::fs::create_dir_all(&ta_dir).is_err() {
        return;
    }

    let log_path = ta_dir.join("dev-audit.log");
    let entry = serde_json::json!({
        "session_id": session_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": event,
        "context": context,
    });

    let line = format!("{}\n", entry);
    // Append to log file.
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

pub fn execute(
    config: &GatewayConfig,
    project_root: &Path,
    agent: Option<&str>,
    unrestricted: bool,
) -> anyhow::Result<()> {
    let project_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    // Generate a session ID for audit trail.
    let session_id = uuid::Uuid::new_v4().to_string();

    println!("Starting interactive developer loop...");
    println!("  Project: {}", project_root.display());
    println!("  Session: {}", &session_id[..8]);

    if unrestricted {
        eprintln!();
        eprintln!("  ⚠ WARNING: Running in UNRESTRICTED mode.");
        eprintln!("  The orchestrator agent has full access to all tools.");
        eprintln!("  This bypasses read-only enforcement and sandbox restrictions.");
        eprintln!();
    }

    // Print plan status to terminal so the user sees context before agent starts (v0.8.2).
    print_plan_status_to_terminal(&project_root);

    // Build the orchestration prompt with plan status + memory context.
    let prompt = build_dev_prompt(&project_root, config, unrestricted);

    // Load agent config (dev-loop.yaml or fallback).
    let agent_config = if let Some(agent_id) = agent {
        DevLoopConfig {
            command: agent_id.to_string(),
            args_template: vec!["-p".to_string(), "{prompt}".to_string()],
            env: Default::default(),
        }
    } else {
        load_dev_config(&project_root, unrestricted)
    };

    let mode_label = if unrestricted {
        "unrestricted"
    } else {
        "restricted (read-only)"
    };

    println!("  Agent: {}", agent_config.command);
    println!("  Mode: orchestration — {}", mode_label);
    println!();

    // Inject TA MCP server into .mcp.json so the agent can call ta_plan,
    // ta_goal, ta_draft, ta_context, ta_release via MCP.
    // Without this, the agent has no MCP server to handle those tool calls.
    //
    // v0.9.3: Pass the session ID and caller mode as env vars to the MCP server
    // so it can log tool calls with the session ID and enforce policy for
    // orchestrator callers.
    inject_mcp_server_config_with_session(&project_root, &session_id, unrestricted)?;
    println!("  MCP: registered TA server (ta serve) in .mcp.json");
    println!();

    // Audit: log session start.
    write_dev_audit(
        &project_root,
        &session_id,
        "session_start",
        Some(if unrestricted {
            "unrestricted"
        } else {
            "restricted"
        }),
    );

    // Launch the agent in the project directory (not a staging workspace).
    let args: Vec<String> = agent_config
        .args_template
        .iter()
        .map(|t| t.replace("{prompt}", &prompt))
        .collect();

    let mut cmd = std::process::Command::new(&agent_config.command);
    cmd.current_dir(&project_root);
    cmd.args(&args);

    for (key, value) in &agent_config.env {
        cmd.env(key, value);
    }

    // Pass session ID to the agent process so it's available for audit correlation.
    cmd.env("TA_DEV_SESSION_ID", &session_id);

    let result = cmd.status();

    // Always restore .mcp.json, even if the agent failed.
    if let Err(e) = restore_mcp_server_config(&project_root) {
        eprintln!("Warning: failed to restore .mcp.json: {}", e);
    }

    // Audit: log session end.
    let exit_info = match &result {
        Ok(exit) => format!("exit_code={}", exit),
        Err(e) => format!("error={}", e),
    };
    write_dev_audit(&project_root, &session_id, "session_end", Some(&exit_info));

    match result {
        Ok(exit) => {
            if exit.success() {
                println!("\nDev session ended.");
            } else {
                println!("\nDev session ended with status {}.", exit);
            }
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                eprintln!(
                    "\n'{}' command not found. Install it or specify --agent.",
                    agent_config.command
                );
                eprintln!("  Example: ta dev --agent codex");
                return Ok(());
            }
            return Err(anyhow::anyhow!(
                "Failed to launch {}: {}",
                agent_config.command,
                e
            ));
        }
    }

    Ok(())
}

/// Inject the TA MCP server with session-specific env vars for audit and policy.
///
/// This is an enhanced version of `inject_mcp_server_config` that adds:
/// - `TA_DEV_SESSION_ID`: correlates tool calls with the dev session for audit
/// - `TA_CALLER_MODE`: "orchestrator" (default) or "unrestricted" — gateway uses
///   this to enforce forbidden actions for orchestrator callers
fn inject_mcp_server_config_with_session(
    project_root: &Path,
    session_id: &str,
    unrestricted: bool,
) -> anyhow::Result<()> {
    use super::run::{MCP_JSON_BACKUP, MCP_JSON_PATH, NO_ORIGINAL_SENTINEL};

    let mcp_json_path = project_root.join(MCP_JSON_PATH);
    let backup_path = project_root.join(MCP_JSON_BACKUP);

    // Save original content (or sentinel if file doesn't exist).
    let original_content = if mcp_json_path.exists() {
        std::fs::read_to_string(&mcp_json_path)?
    } else {
        NO_ORIGINAL_SENTINEL.to_string()
    };

    let backup_dir = project_root.join(".ta");
    std::fs::create_dir_all(&backup_dir)?;
    std::fs::write(&backup_path, &original_content)?;

    // Resolve the `ta` binary path for the server command.
    let ta_binary = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "ta".to_string());

    let caller_mode = if unrestricted {
        "unrestricted"
    } else {
        "orchestrator"
    };

    let ta_server_entry = serde_json::json!({
        "command": ta_binary,
        "args": ["serve"],
        "env": {
            "TA_PROJECT_ROOT": project_root.display().to_string(),
            "TA_DEV_SESSION_ID": session_id,
            "TA_CALLER_MODE": caller_mode
        }
    });

    // Merge with existing .mcp.json if present.
    let mut mcp_config: serde_json::Value = if original_content != NO_ORIGINAL_SENTINEL {
        serde_json::from_str(&original_content)
            .unwrap_or_else(|_| serde_json::json!({ "mcpServers": {} }))
    } else {
        serde_json::json!({ "mcpServers": {} })
    };

    if let Some(servers) = mcp_config
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
    {
        servers.insert("ta".to_string(), ta_server_entry);
    } else {
        mcp_config["mcpServers"] = serde_json::json!({
            "ta": ta_server_entry
        });
    }

    std::fs::write(&mcp_json_path, serde_json::to_string_pretty(&mcp_config)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_dev_prompt_includes_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let prompt = build_dev_prompt(dir.path(), &config, false);
        assert!(prompt.contains("Your Capabilities"));
        assert!(prompt.contains("ta_plan"));
        assert!(prompt.contains("ta_goal"));
        assert!(prompt.contains("ta_draft"));
    }

    #[test]
    fn test_build_dev_prompt_no_plan() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let prompt = build_dev_prompt(dir.path(), &config, false);
        assert!(prompt.contains("No PLAN.md found"));
    }

    #[test]
    fn test_build_dev_prompt_restricted_has_security_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let prompt = build_dev_prompt(dir.path(), &config, false);
        assert!(prompt.contains("Security Boundaries"));
        assert!(prompt.contains("No file writes"));
        assert!(prompt.contains("No shell access"));
        assert!(!prompt.contains("UNRESTRICTED"));
    }

    #[test]
    fn test_build_dev_prompt_unrestricted_has_warning() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let prompt = build_dev_prompt(dir.path(), &config, true);
        assert!(prompt.contains("UNRESTRICTED"));
        assert!(!prompt.contains("No file writes"));
    }

    #[test]
    fn test_build_plan_summary_with_plan() {
        let dir = tempfile::tempdir().unwrap();
        let plan_content = "\
# Plan\n\
\n\
### v0.1 — First Phase\n\
<!-- status: done -->\n\
\n\
### v0.2 — Second Phase\n\
<!-- status: pending -->\n\
";
        std::fs::write(dir.path().join("PLAN.md"), plan_content).unwrap();
        let summary = build_plan_summary(dir.path());
        assert!(summary.contains("1/2 phases complete"));
        assert!(summary.contains("Next pending"));
    }

    #[test]
    fn test_build_drafts_summary_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig::for_project(dir.path());
        let summary = build_drafts_summary(&config);
        // Either empty or "No pending drafts" — both acceptable.
        assert!(summary.is_empty() || summary.contains("No pending"));
    }

    #[test]
    fn test_load_dev_config_fallback_restricted() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_dev_config(dir.path(), false);
        assert_eq!(config.command, "claude");
        // Uses --system-prompt (not -p) so Claude stays interactive.
        assert!(config
            .args_template
            .contains(&"--system-prompt".to_string()));
        assert!(!config.args_template.contains(&"-p".to_string()));
        // Has --allowedTools in restricted mode.
        assert!(config.args_template.contains(&"--allowedTools".to_string()));
    }

    #[test]
    fn test_load_dev_config_unrestricted_no_allowed_tools() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_dev_config(dir.path(), true);
        assert_eq!(config.command, "claude");
        // Unrestricted mode has NO --allowedTools restriction.
        assert!(!config.args_template.contains(&"--allowedTools".to_string()));
    }

    #[test]
    fn test_write_dev_audit_creates_log() {
        let dir = tempfile::tempdir().unwrap();
        write_dev_audit(
            dir.path(),
            "test-session-id",
            "test_event",
            Some("test context"),
        );
        let log_path = dir.path().join(".ta/dev-audit.log");
        assert!(log_path.exists());
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("test-session-id"));
        assert!(content.contains("test_event"));
        assert!(content.contains("test context"));
    }

    #[test]
    fn test_inject_mcp_server_with_session() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        inject_mcp_server_config_with_session(dir.path(), "sess-123", false).unwrap();

        let mcp_json = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&mcp_json).unwrap();
        let env = &config["mcpServers"]["ta"]["env"];
        assert_eq!(env["TA_DEV_SESSION_ID"], "sess-123");
        assert_eq!(env["TA_CALLER_MODE"], "orchestrator");
    }

    #[test]
    fn test_inject_mcp_server_unrestricted_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        inject_mcp_server_config_with_session(dir.path(), "sess-456", true).unwrap();

        let mcp_json = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&mcp_json).unwrap();
        let env = &config["mcpServers"]["ta"]["env"];
        assert_eq!(env["TA_CALLER_MODE"], "unrestricted");
    }
}
