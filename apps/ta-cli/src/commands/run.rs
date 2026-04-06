// run.rs — Orchestrate a full goal lifecycle: start → agent → PR build.
//
// `ta run "Fix the auth bug"` is a convenience wrapper that:
// 1. Creates a goal with an overlay workspace
// 2. Injects context (e.g., CLAUDE.md) for the agent
// 3. Launches the agent with the goal as its initial prompt
// 4. When the agent exits, restores injected files and builds a PR package
//
// The user then reviews/approves/applies via `ta draft` commands.

use std::io::IsTerminal;
use std::path::Path;

#[cfg(unix)]
use ta_changeset::{InteractionKind, InteractionRequest, InteractionResponse, Urgency};
use ta_changeset::{InteractiveSession, InteractiveSessionState, InteractiveSessionStore};
use ta_goal::GoalRunStore;
use ta_mcp_gateway::GatewayConfig;

use super::plan;
#[cfg(unix)]
use super::pty_capture;

// ── Per-agent launch configuration ──────────────────────────────

/// Agent launch descriptor.
/// Loaded from YAML config files (with hard-coded fallbacks for built-in agents).
///
/// Config search order:
///   1. `.ta/agents/<agent-id>.yaml`       (project override)
///   2. `~/.config/ta/agents/<agent-id>.yaml`  (user override)
///   3. `<binary-dir>/agents/<agent-id>.yaml`  (shipped defaults)
///   4. Hard-coded fallback
#[derive(serde::Deserialize, Clone, Debug)]
struct AgentLaunchConfig {
    /// The command to execute (e.g., "claude", "codex").
    command: String,
    /// Arguments to pass. `{prompt}` is replaced with the goal text.
    args_template: Vec<String>,
    /// Whether this agent reads CLAUDE.md for context injection.
    #[serde(default)]
    injects_context_file: bool,
    /// Whether to inject .claude/settings.local.json with TA permissions.
    #[serde(default)]
    injects_settings: bool,
    /// Optional command to run before the main agent launch (e.g., init).
    #[serde(default)]
    pre_launch: Option<PreLaunchConfig>,
    /// Environment variables to set for the agent process.
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    /// Shell to use for command execution: "bash", "powershell", "cmd".
    /// Auto-detected based on platform if not specified (v0.9.1).
    #[serde(default)]
    #[allow(dead_code)]
    shell: Option<String>,
    /// Human-readable name (informational only, used by `ta agent list` in future).
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    /// Description of the agent (informational only, used by `ta agent list` in future).
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    /// Interactive session configuration (v0.3.1.2).
    #[serde(default)]
    #[allow(dead_code)]
    interactive: Option<ta_changeset::InteractiveConfig>,
    /// Agent alignment profile (v0.4.0).
    /// Compiled into CapabilityManifest grants by the Policy Compiler.
    /// Read via YAML deserialization; will be used by gateway during goal start.
    #[serde(default)]
    #[allow(dead_code)]
    alignment: Option<ta_policy::AlignmentProfile>,
    /// Extra args appended in headless mode (v0.10.18.4).
    /// E.g., `["--output-format", "stream-json"]` for Claude Code.
    /// Agents without `headless_args` fall back to raw piped output.
    #[serde(default)]
    headless_args: Vec<String>,
    /// Environment variables set ONLY in headless (daemon-spawned) mode (v0.10.18.5).
    /// Used to suppress interactive prompts for agents that support non-interactive flags.
    /// Not applied in direct CLI mode where the user has a terminal.
    #[serde(default)]
    non_interactive_env: std::collections::HashMap<String, String>,
    /// Ordered list of regex→response mappings for known interactive prompts (v0.10.18.5).
    /// When the daemon detects a matching prompt in stdout, it auto-responds via stdin pipe.
    /// Template variables: `{goal_title}`, `{goal_id}`, `{project_name}`.
    #[serde(default)]
    #[allow(dead_code)]
    auto_answers: Vec<AutoAnswerConfig>,
    /// Path (relative to staging root) of the context file TA writes for non-Claude agents
    /// (v0.12.5). When set, TA writes the same context that would go into CLAUDE.md into
    /// this file instead. Example: `.ta/agent_context.md`. Ignored when absent.
    #[serde(default)]
    context_file: Option<String>,
    /// Which RuntimeAdapter backend to use for spawning this agent (v0.13.3).
    ///
    /// Accepted values: "process" (default), "oci", "vm", or the name of any
    /// installed `ta-runtime-<name>` plugin.
    ///
    /// Example in agent YAML:
    ///   runtime = "process"
    #[serde(default)]
    runtime: ta_runtime::RuntimeConfig,

    /// Whether this agent sends heartbeats to the daemon (v0.13.14).
    ///
    /// When `false` (default), the watchdog disables stale-based detection for goals
    /// run by this agent and only acts on zombie conditions (PID gone without clean exit).
    /// Claude Code and most built-in agents do not send heartbeats, so this defaults to `false`.
    ///
    /// When `true`, goals with no state update for `stale_threshold_secs` emit `GoalStale`.
    #[serde(default)]
    heartbeat_required: bool,
}

/// Auto-answer configuration for interactive prompts (v0.10.18.5).
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
struct AutoAnswerConfig {
    /// Regex pattern to match against agent stdout lines.
    prompt: String,
    /// Response to send to stdin. May contain template variables.
    response: String,
    /// If true, use this response as a timeout fallback for unmatched prompts.
    #[serde(default)]
    fallback: bool,
}

/// Pre-launch command configuration (deserialized from YAML).
#[derive(serde::Deserialize, Clone, Debug)]
struct PreLaunchConfig {
    command: String,
    args: Vec<String>,
}

/// Try to load agent config from YAML files, falling back to hard-coded defaults.
fn agent_launch_config(agent_id: &str, source_dir: Option<&Path>) -> AgentLaunchConfig {
    // Try YAML configs in priority order.
    if let Some(config) = load_agent_yaml(agent_id, source_dir) {
        return config;
    }
    // Fall back to built-in defaults.
    builtin_agent_config(agent_id)
}

/// Search for an agent YAML config in standard locations.
fn load_agent_yaml(agent_id: &str, source_dir: Option<&Path>) -> Option<AgentLaunchConfig> {
    let filename = format!("{}.yaml", agent_id);

    // 1. Project override: .ta/agents/<agent-id>.yaml
    if let Some(source) = source_dir {
        let project_path = source.join(".ta").join("agents").join(&filename);
        if let Some(config) = try_load_yaml(&project_path) {
            return Some(config);
        }
    }

    // 2. User override: ~/.config/ta/agents/<agent-id>.yaml
    if let Some(home) = dirs_path() {
        let user_path = home
            .join(".config")
            .join("ta")
            .join("agents")
            .join(&filename);
        if let Some(config) = try_load_yaml(&user_path) {
            return Some(config);
        }
    }

    // 3. Shipped defaults: <binary-dir>/agents/<agent-id>.yaml
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let shipped_path = bin_dir.join("agents").join(&filename);
            if let Some(config) = try_load_yaml(&shipped_path) {
                return Some(config);
            }
        }
    }

    None
}

/// Attempt to read and parse a single YAML config file.
fn try_load_yaml(path: &Path) -> Option<AgentLaunchConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str(&content).ok()
}

/// Get home directory (cross-platform).
fn dirs_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// Hard-coded built-in agent configs (fallback when no YAML file is found).
fn builtin_agent_config(agent_id: &str) -> AgentLaunchConfig {
    match agent_id {
        "claude-code" => AgentLaunchConfig {
            command: "claude".to_string(),
            args_template: vec!["{prompt}".to_string()],
            injects_context_file: true,
            injects_settings: true,
            pre_launch: None,
            env: Default::default(),
            shell: None,
            name: Some("claude-code".to_string()),
            description: Some("Anthropic's Claude Code CLI".to_string()),
            interactive: None,
            alignment: Some(ta_policy::AlignmentProfile::default_developer()),
            headless_args: vec![
                "--print".to_string(),
                "--verbose".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
            ],
            non_interactive_env: Default::default(),
            auto_answers: Vec::new(),
            context_file: None,
            runtime: Default::default(),
            // Claude Code does not send heartbeats — disable stale checking (v0.13.14).
            heartbeat_required: false,
        },
        "codex" => AgentLaunchConfig {
            command: "codex".to_string(),
            args_template: vec![
                "--approval-mode".to_string(),
                "full-auto".to_string(),
                "{prompt}".to_string(),
            ],
            injects_context_file: false,
            injects_settings: false,
            pre_launch: None,
            env: Default::default(),
            shell: None,
            name: Some("codex".to_string()),
            description: Some("OpenAI's Codex CLI".to_string()),
            interactive: None,
            alignment: Some(ta_policy::AlignmentProfile::default_developer()),
            headless_args: Vec::new(),
            non_interactive_env: Default::default(),
            auto_answers: Vec::new(),
            context_file: None,
            runtime: Default::default(),
            heartbeat_required: false,
        },
        "claude-flow" => AgentLaunchConfig {
            command: "npx".to_string(),
            args_template: vec![
                "claude-flow@alpha".to_string(),
                "hive-mind".to_string(),
                "spawn".to_string(),
                "{prompt}".to_string(),
                "--claude".to_string(),
            ],
            injects_context_file: true,
            injects_settings: true,
            pre_launch: Some(PreLaunchConfig {
                command: "npx".to_string(),
                args: vec![
                    "claude-flow@alpha".to_string(),
                    "hive-mind".to_string(),
                    "init".to_string(),
                ],
            }),
            env: Default::default(),
            shell: None,
            name: Some("claude-flow".to_string()),
            description: Some("Claude Flow multi-agent orchestration".to_string()),
            interactive: None,
            alignment: Some(ta_policy::AlignmentProfile::default_developer()),
            headless_args: Vec::new(),
            non_interactive_env: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "CLAUDE_FLOW_NON_INTERACTIVE".to_string(),
                    "true".to_string(),
                );
                m.insert("CLAUDE_FLOW_TOPOLOGY".to_string(), "mesh".to_string());
                m
            },
            auto_answers: vec![
                AutoAnswerConfig {
                    prompt: r"Select.*topology.*\[1\]".to_string(),
                    response: "1".to_string(),
                    fallback: false,
                },
                AutoAnswerConfig {
                    prompt: r"Continue\?.*\[y/N\]".to_string(),
                    response: "y".to_string(),
                    fallback: false,
                },
                AutoAnswerConfig {
                    prompt: r"Enter.*name:".to_string(),
                    response: "{goal_title}".to_string(),
                    fallback: false,
                },
            ],
            context_file: None,
            runtime: Default::default(),
            heartbeat_required: false,
        },
        _ => AgentLaunchConfig {
            command: agent_id.to_string(),
            args_template: vec![],
            injects_context_file: false,
            injects_settings: false,
            pre_launch: None,
            env: Default::default(),
            shell: None,
            name: None,
            description: None,
            interactive: None,
            alignment: None,
            headless_args: Vec::new(),
            non_interactive_env: Default::default(),
            auto_answers: Vec::new(),
            context_file: None,
            runtime: Default::default(),
            heartbeat_required: false,
        },
    }
}

/// Build an `AgentLaunchConfig` from a resolved `AgentFrameworkManifest` (v0.13.8 item 4).
///
/// Used when the framework is a non-built-in TOML manifest — provides the
/// minimal config needed to launch the process. Built-in frameworks keep their
/// hardcoded `builtin_agent_config()` values which have richer headless_args etc.
fn framework_to_launch_config(manifest: &ta_runtime::AgentFrameworkManifest) -> AgentLaunchConfig {
    use ta_runtime::ContextInjectMode;
    // Append {prompt} placeholder after the manifest's fixed args.
    let mut args_template = manifest.args.clone();
    args_template.push("{prompt}".to_string());
    let injects_context_file = matches!(manifest.context_inject, ContextInjectMode::Prepend);
    AgentLaunchConfig {
        command: manifest.command.clone(),
        args_template,
        injects_context_file,
        injects_settings: false, // custom frameworks don't get claude settings
        pre_launch: None,
        env: Default::default(),
        shell: None,
        name: Some(manifest.name.clone()),
        description: Some(manifest.description.clone()),
        interactive: None,
        alignment: None,
        headless_args: Vec::new(),
        non_interactive_env: Default::default(),
        auto_answers: Vec::new(),
        context_file: None,
        runtime: Default::default(),
        heartbeat_required: false,
    }
}

// ── Smart Follow-Up Resolution (v0.10.9) ────────────────────────

/// Resolve smart follow-up flags into concrete title, phase, follow-up ID, and context.
///
/// Priority:
/// 1. `--follow-up-draft <id>` → resolve from draft
/// 2. `--follow-up-goal <id>` → resolve from goal
/// 3. `--follow-up --phase <id>` → resolve from plan phase
/// 4. `--follow-up` (no arg) → interactive picker
/// 5. `--follow-up <id>` → existing behavior (goal/draft prefix match)
/// 6. No follow-up flags → pass through unchanged
///
/// Returns (title, phase, follow_up, follow_up_context) where follow_up_context
/// is an optional rich context string for CLAUDE.md injection.
#[allow(clippy::type_complexity)]
fn resolve_smart_follow_up(
    config: &GatewayConfig,
    title: Option<&str>,
    phase: Option<&str>,
    follow_up: Option<&Option<String>>,
    follow_up_draft: Option<&str>,
    follow_up_goal: Option<&str>,
) -> anyhow::Result<(
    Option<String>,
    Option<String>,
    Option<Option<String>>,
    Option<String>,
)> {
    use super::follow_up as fu;

    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    // 1. --follow-up-draft <id>
    if let Some(draft_prefix) = follow_up_draft {
        let candidate = fu::resolve_by_draft(config, &goal_store, draft_prefix)?;
        let context = fu::build_follow_up_context(&candidate, &goal_store, config);
        let resolved_title = title
            .map(|t| t.to_string())
            .unwrap_or_else(|| format!("Follow-up: {}", candidate.title));
        let resolved_phase = phase.map(|p| p.to_string()).or(candidate.phase_id.clone());
        let follow_up_id = candidate.goal_id.map(|id| id.to_string());
        return Ok((
            Some(resolved_title),
            resolved_phase,
            Some(follow_up_id),
            Some(context),
        ));
    }

    // 2. --follow-up-goal <id>
    if let Some(goal_prefix) = follow_up_goal {
        let candidate = fu::resolve_by_goal(&goal_store, goal_prefix)?;
        let context = fu::build_follow_up_context(&candidate, &goal_store, config);
        let resolved_title = title
            .map(|t| t.to_string())
            .unwrap_or_else(|| format!("Follow-up: {}", candidate.title));
        let resolved_phase = phase.map(|p| p.to_string()).or(candidate.phase_id.clone());
        let follow_up_id = candidate.goal_id.map(|id| id.to_string());
        return Ok((
            Some(resolved_title),
            resolved_phase,
            Some(follow_up_id),
            Some(context),
        ));
    }

    // 3. --follow-up with no arg AND --phase → resolve by phase
    if let Some(inner) = follow_up {
        if let (None, Some(phase_id)) = (inner, phase) {
            let candidate = fu::resolve_by_phase(config, &goal_store, phase_id)?;
            let context = fu::build_follow_up_context(&candidate, &goal_store, config);
            let resolved_title = title
                .map(|t| t.to_string())
                .unwrap_or_else(|| format!("Follow-up: {}", candidate.title));
            let follow_up_id = candidate.goal_id.map(|id| id.to_string());
            return Ok((
                Some(resolved_title),
                Some(phase_id.to_string()),
                Some(follow_up_id),
                Some(context),
            ));
        }

        // 4. --follow-up (no arg, no phase) → interactive picker
        if inner.is_none() {
            let candidates = fu::gather_follow_up_candidates(config, &goal_store)?;
            let selected = fu::pick_candidate(&candidates)?;
            let context = fu::build_follow_up_context(selected, &goal_store, config);
            let resolved_title = title
                .map(|t| t.to_string())
                .unwrap_or_else(|| format!("Follow-up: {}", selected.title));
            let resolved_phase = phase.map(|p| p.to_string()).or(selected.phase_id.clone());
            let follow_up_id = selected.goal_id.map(|id| id.to_string());
            return Ok((
                Some(resolved_title),
                resolved_phase,
                Some(follow_up_id),
                Some(context),
            ));
        }
    }

    // 5/6. --follow-up <id> or no follow-up → pass through unchanged (existing behavior).
    // The existing goal.rs find_parent_goal() handles ID prefix resolution.
    Ok((
        title.map(|t| t.to_string()),
        phase.map(|p| p.to_string()),
        follow_up.cloned(),
        None,
    ))
}

// ── Workflow routing (v0.13.7) ───────────────────────────────────

/// The kind of workflow to use for this goal run.
///
/// Resolved from (in priority order):
/// 1. Explicit `--workflow` flag
/// 2. `.ta/config.yaml` `channels.default_workflow` (project-level default, v0.13.7)
/// 3. Built-in `single-agent` (backwards-compatible default)
#[derive(Debug, Clone)]
enum WorkflowKind {
    /// Default: one agent, one staging directory. Backwards-compatible.
    SingleAgent,
    /// Chain phases serially: each phase as a follow-up in the same staging,
    /// one PR at the end. Full implementation in v0.13.7.
    SerialPhases,
    /// Parallel sub-goals in separate staging dirs, with optional integration agent.
    Swarm,
    /// Unknown/plugin workflow name.
    Unknown(String),
}

impl WorkflowKind {
    fn from_str(s: &str) -> Self {
        match s {
            "single-agent" | "" => WorkflowKind::SingleAgent,
            "serial-phases" => WorkflowKind::SerialPhases,
            "swarm" => WorkflowKind::Swarm,
            other => WorkflowKind::Unknown(other.to_string()),
        }
    }
}

/// Resolve the workflow kind from the explicit flag, then from `.ta/config.yaml`,
/// then fall back to `single-agent`.
///
/// Resolution order (v0.13.7, item 14):
/// 1. Explicit `--workflow` flag
/// 2. `channels.default_workflow` in `.ta/config.yaml`
/// 3. Built-in `single-agent`
fn resolve_workflow(explicit: Option<&str>, workspace_root: &std::path::Path) -> WorkflowKind {
    if let Some(w) = explicit {
        return WorkflowKind::from_str(w);
    }
    let ta_config = ta_changeset::channel_registry::load_config(workspace_root);
    if let Some(ref wf) = ta_config.channels.default_workflow {
        if !wf.is_empty() {
            return WorkflowKind::from_str(wf);
        }
    }
    WorkflowKind::SingleAgent
}

// ── Serial phases workflow (v0.13.7) ────────────────────────────

/// Execute a serial-phases workflow: run each phase in order, evaluate gates
/// between phases, and chain each phase as a follow-up to the previous one.
///
/// Each phase runs the full single-agent execution loop in the same staging
/// directory (follow-up chain). After each phase, gate commands are evaluated.
/// If a gate fails the workflow halts with actionable error + resume instructions.
///
/// Workflow state is persisted to `.ta/serial-workflow-<id>.json` for resume support.
#[allow(clippy::too_many_arguments)]
pub fn execute_serial_phases(
    config: &GatewayConfig,
    title: &str,
    agent: &str,
    objective: &str,
    phases: &[String],
    gates: &[String],
    quiet: bool,
) -> anyhow::Result<()> {
    if phases.is_empty() {
        anyhow::bail!(
            "serial-phases workflow requires at least one phase. \
             Use --phases v0.13.7.1,v0.13.7.2 to specify phases."
        );
    }

    let ta_bin = std::env::current_exe().map_err(|e| {
        anyhow::anyhow!(
            "Could not determine ta binary path for subprocess invocation: {}",
            e
        )
    })?;

    let workflow_id = uuid::Uuid::new_v4().to_string();
    let state_dir = config.workspace_root.join(".ta");
    let gate_specs: Vec<ta_workflow::WorkflowGate> = gates
        .iter()
        .map(|g| ta_workflow::WorkflowGate::parse(g))
        .collect();

    let mut state =
        ta_workflow::SerialPhasesState::new(&workflow_id, phases.to_vec(), gates.to_vec());
    state.save(&state_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to persist workflow state to {}: {}",
            state_dir.display(),
            e
        )
    })?;

    if !quiet {
        println!(
            "\nWorkflow: serial-phases ({} phases, {} gate{})",
            phases.len(),
            gates.len(),
            if gates.len() == 1 { "" } else { "s" }
        );
        println!("  Workflow ID: {}", workflow_id);
        println!("  Phases: {}", phases.join(", "));
        if !gates.is_empty() {
            println!("  Gates:  {}", gates.join(", "));
        }
        println!();
    }

    let mut prev_goal_id: Option<String> = None;

    for (i, phase) in phases.iter().enumerate() {
        if !quiet {
            println!(
                "── Phase [{}/{}]: {} ──────────────────────────────",
                i + 1,
                phases.len(),
                phase
            );
        }

        // Mark step as running.
        state.steps[i] = ta_workflow::StepState::Running {
            goal_id: String::new(),
        };
        state.current_step = i;
        let _ = state.save(&state_dir);

        // Build the subprocess command for this phase.
        let mut cmd = std::process::Command::new(&ta_bin);
        cmd.arg("run")
            .arg(title)
            .arg("--phase")
            .arg(phase)
            .arg("--agent")
            .arg(agent);

        if let Some(ref prev_id) = prev_goal_id {
            cmd.arg("--follow-up-goal").arg(prev_id);
        }

        if !objective.is_empty() {
            cmd.arg("--objective").arg(objective);
        }

        if quiet {
            cmd.arg("--quiet");
        }

        cmd.current_dir(&config.workspace_root);

        let status = cmd.status().map_err(|e| {
            anyhow::anyhow!(
                "Failed to launch agent for phase {}: {}. \
                 Resume with: ta run --workflow serial-phases --resume-workflow {}",
                phase,
                e,
                workflow_id
            )
        })?;

        if !status.success() {
            let code = status.code().unwrap_or(-1);
            state.steps[i] = ta_workflow::StepState::AgentFailed {
                error: format!("exit code {}", code),
            };
            let _ = state.save(&state_dir);
            anyhow::bail!(
                "Phase {} failed: agent exited with code {}.\n  \
                 Resume with: ta run --workflow serial-phases --resume-workflow {}",
                phase,
                code,
                workflow_id
            );
        }

        // Find the most recently created goal for this phase.
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let goals = goal_store.list()?;
        let goal = goals
            .into_iter()
            .find(|g| g.plan_phase.as_deref() == Some(phase.as_str()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Could not find completed goal for phase '{}' after agent run. \
                     Check goal store at {}.",
                    phase,
                    config.goals_dir.display()
                )
            })?;

        let goal_id = goal.goal_run_id.to_string();
        let staging_path = goal.workspace_path.clone();

        // Evaluate gates in the staging directory.
        if !gate_specs.is_empty() {
            if !quiet {
                println!("  Evaluating {} gate(s)...", gate_specs.len());
            }
            if let Err(failure) = ta_workflow::evaluate_gates(&gate_specs, &staging_path, quiet) {
                state.steps[i] = ta_workflow::StepState::GateFailed {
                    goal_id: goal_id.clone(),
                    failed_gate: failure.gate_name.clone(),
                    error: failure.to_string(),
                };
                let _ = state.save(&state_dir);
                anyhow::bail!(
                    "Gate '{}' failed after phase {}.\n  \
                     Staging: {}\n  \
                     Fix the issue, then resume with:\n  \
                     ta run --workflow serial-phases --resume-workflow {}",
                    failure.gate_name,
                    phase,
                    staging_path.display(),
                    workflow_id
                );
            }
        }

        // Mark step as passed.
        state.steps[i] = ta_workflow::StepState::Passed {
            goal_id: goal_id.clone(),
        };
        state.last_goal_id = Some(goal_id.clone());
        state.staging_path = Some(staging_path.clone());
        let _ = state.save(&state_dir);

        if !quiet {
            println!(
                "  Phase {} passed. Goal: {}  Staging: {}",
                phase,
                goal_id,
                staging_path.display()
            );
        }

        prev_goal_id = Some(goal_id);
    }

    // All phases complete — print summary.
    let last_goal = prev_goal_id.as_deref().unwrap_or("(unknown)");
    println!(
        "\nserial-phases workflow complete: {} phase{} passed.",
        phases.len(),
        if phases.len() == 1 { "" } else { "s" }
    );
    println!("  Build the combined draft with:");
    println!("    ta draft build --goal {}", last_goal);
    println!("    # Or: ta draft build --latest");

    Ok(())
}

// ── Swarm workflow (v0.13.7) ─────────────────────────────────────

/// Execute a swarm workflow: run each sub-goal as an independent agent in its
/// own staging directory, then optionally run an integration agent to merge results.
///
/// Sub-goals are run sequentially in this initial implementation; parallel
/// execution (via OS threads) is planned for v0.13.7.2.
///
/// Each sub-goal gets its own staging directory. Per-agent gates are evaluated
/// after each sub-goal. Failures are reported but do not stop remaining sub-goals.
/// After all complete, if `--integrate` is set, an integration agent is launched.
#[allow(clippy::too_many_arguments)]
pub fn execute_swarm(
    config: &GatewayConfig,
    title: &str,
    agent: &str,
    objective: &str,
    sub_goals: &[String],
    per_agent_gates: &[String],
    integrate: bool,
    quiet: bool,
) -> anyhow::Result<()> {
    if sub_goals.is_empty() {
        anyhow::bail!(
            "swarm workflow requires at least one sub-goal. \
             Use --sub-goals \"goal1\" \"goal2\" to specify sub-goals."
        );
    }

    let ta_bin = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("Could not determine ta binary path: {}", e))?;

    let workflow_id = uuid::Uuid::new_v4().to_string();
    let state_dir = config.workspace_root.join(".ta");

    let sub_goal_specs: Vec<ta_workflow::SubGoalSpec> = sub_goals
        .iter()
        .map(ta_workflow::SubGoalSpec::new)
        .collect();

    let mut state = ta_workflow::SwarmState::new(&workflow_id, title, sub_goal_specs, integrate);
    state.per_agent_gates = per_agent_gates.to_vec();
    state
        .save(&state_dir)
        .map_err(|e| anyhow::anyhow!("Failed to persist swarm state: {}", e))?;

    let gate_specs: Vec<ta_workflow::WorkflowGate> = per_agent_gates
        .iter()
        .map(|g| ta_workflow::WorkflowGate::parse(g))
        .collect();

    if !quiet {
        println!(
            "\nWorkflow: swarm ({} sub-goal{}, {} gate{} per agent)",
            sub_goals.len(),
            if sub_goals.len() == 1 { "" } else { "s" },
            per_agent_gates.len(),
            if per_agent_gates.len() == 1 { "" } else { "s" }
        );
        println!("  Swarm ID: {}", workflow_id);
        for (i, sg) in sub_goals.iter().enumerate() {
            println!("  Sub-goal [{}/{}]: {}", i + 1, sub_goals.len(), sg);
        }
        println!();
    }

    let mut passed_goals: Vec<(String, std::path::PathBuf)> = Vec::new();

    for (i, sub_goal_title) in sub_goals.iter().enumerate() {
        if !quiet {
            println!(
                "── Sub-goal [{}/{}]: {} ──────────────────────────",
                i + 1,
                sub_goals.len(),
                sub_goal_title
            );
        }

        state.sub_goal_states[i] = ta_workflow::SubGoalStatus::Running {
            goal_id: String::new(),
            staging_path: std::path::PathBuf::new(),
        };
        let _ = state.save(&state_dir);

        // Run the sub-goal as an independent agent.
        let mut cmd = std::process::Command::new(&ta_bin);
        cmd.arg("run").arg(sub_goal_title).arg("--agent").arg(agent);
        if !objective.is_empty() {
            cmd.arg("--objective").arg(objective);
        }
        if quiet {
            cmd.arg("--quiet");
        }
        cmd.current_dir(&config.workspace_root);

        let status = cmd.status().map_err(|e| {
            anyhow::anyhow!(
                "Failed to launch agent for sub-goal '{}': {}",
                sub_goal_title,
                e
            )
        })?;

        if !status.success() {
            let code = status.code().unwrap_or(-1);
            state.sub_goal_states[i] = ta_workflow::SubGoalStatus::AgentFailed {
                error: format!("exit code {}", code),
            };
            let _ = state.save(&state_dir);
            if !quiet {
                eprintln!(
                    "  Sub-goal '{}' FAILED (exit {}). Continuing.",
                    sub_goal_title, code
                );
            }
            continue;
        }

        // Find the most recent goal matching this title.
        let goal_store = GoalRunStore::new(&config.goals_dir)?;
        let goals = goal_store.list()?;
        let goal = match goals
            .into_iter()
            .find(|g| g.title.trim() == sub_goal_title.trim())
        {
            Some(g) => g,
            None => {
                state.sub_goal_states[i] = ta_workflow::SubGoalStatus::AgentFailed {
                    error: "goal not found in store after agent run".to_string(),
                };
                let _ = state.save(&state_dir);
                if !quiet {
                    eprintln!(
                        "  Warning: could not find goal record for '{}'. Continuing.",
                        sub_goal_title
                    );
                }
                continue;
            }
        };

        let goal_id = goal.goal_run_id.to_string();
        let staging_path = goal.workspace_path.clone();

        // Evaluate per-agent gates.
        if !gate_specs.is_empty() {
            if let Err(failure) = ta_workflow::evaluate_gates(&gate_specs, &staging_path, quiet) {
                state.sub_goal_states[i] = ta_workflow::SubGoalStatus::GateFailed {
                    goal_id,
                    staging_path,
                    failed_gate: failure.gate_name.clone(),
                    error: failure.to_string(),
                };
                let _ = state.save(&state_dir);
                if !quiet {
                    eprintln!(
                        "  Sub-goal '{}' gate FAILED ({}). Continuing.",
                        sub_goal_title, failure.gate_name
                    );
                }
                continue;
            }
        }

        state.sub_goal_states[i] = ta_workflow::SubGoalStatus::Passed {
            goal_id: goal_id.clone(),
            staging_path: staging_path.clone(),
        };
        let _ = state.save(&state_dir);
        passed_goals.push((goal_id, staging_path));

        if !quiet {
            println!("  Sub-goal '{}' passed.", sub_goal_title);
        }
    }

    // Summary.
    let passed = state.passed_count();
    let failed = state.failed_count();
    println!(
        "\nSwarm workflow complete: {}/{} sub-goals passed{}.",
        passed,
        sub_goals.len(),
        if failed > 0 {
            format!(", {} failed", failed)
        } else {
            String::new()
        }
    );
    println!("  Swarm ID: {}", workflow_id);

    if passed == 0 {
        anyhow::bail!(
            "All sub-goals failed. Nothing to integrate. \
             Check sub-goal errors above."
        );
    }

    if integrate {
        println!(
            "\nRunning integration agent to merge {} sub-goal(s)...",
            passed
        );
        let integration_title = format!("Integrate swarm results for: {}", title);
        let staging_list: Vec<String> = passed_goals
            .iter()
            .map(|(_, p)| p.display().to_string())
            .collect();
        let integration_objective = format!(
            "Merge and integrate the results of {} parallel sub-goals into a single coherent output.\n\
             Sub-goal staging directories:\n{}",
            passed,
            staging_list
                .iter()
                .map(|s| format!("  - {}", s))
                .collect::<Vec<_>>()
                .join("\n")
        );

        let mut cmd = std::process::Command::new(&ta_bin);
        cmd.arg("run")
            .arg(&integration_title)
            .arg("--agent")
            .arg(agent)
            .arg("--objective")
            .arg(&integration_objective);
        if quiet {
            cmd.arg("--quiet");
        }
        cmd.current_dir(&config.workspace_root);

        let int_status = cmd
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to launch integration agent: {}", e))?;

        if int_status.success() {
            println!("  Integration complete. Build final draft with:");
            println!("    ta draft build --latest");
        } else {
            eprintln!(
                "  Warning: integration agent exited with code {:?}.",
                int_status.code()
            );
            println!("  Individual drafts can still be built with:");
            for (goal_id, _) in &passed_goals {
                println!("    ta draft build --goal {}", goal_id);
            }
        }
    } else {
        println!("  Build individual drafts with:");
        for (goal_id, _) in &passed_goals {
            println!("    ta draft build --goal {}", goal_id);
        }
    }

    Ok(())
}

// ── Public API ──────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn execute(
    config: &GatewayConfig,
    title: Option<&str>,
    agent: &str,
    source: Option<&Path>,
    objective: &str,
    phase: Option<&str>,
    follow_up: Option<&Option<String>>,
    follow_up_draft: Option<&str>,
    follow_up_goal: Option<&str>,
    objective_file: Option<&Path>,
    no_launch: bool,
    interactive: bool,
    macro_goal: bool,
    resume: Option<&str>,
    headless: bool,
    skip_verify: bool,
    quiet: bool,
    existing_goal_id: Option<&str>,
    workflow: Option<&str>,
    persona_name: Option<&str>,
) -> anyhow::Result<()> {
    // ── Resume an existing session ──────────────────────────────
    if let Some(session_id_prefix) = resume {
        #[cfg(unix)]
        {
            return execute_resume(config, session_id_prefix, agent);
        }
        #[cfg(not(unix))]
        {
            let _ = (session_id_prefix, agent);
            anyhow::bail!("Session resume requires PTY support (not available on Windows)");
        }
    }

    // ── Smart Follow-Up Resolution (v0.10.9) ────────────────────
    //
    // Resolve --follow-up-draft, --follow-up-goal, or --follow-up (no arg)
    // to a concrete follow-up candidate, then derive title/phase/follow-up ID.
    let (title, phase, follow_up, follow_up_context) = resolve_smart_follow_up(
        config,
        title,
        phase,
        follow_up,
        follow_up_draft,
        follow_up_goal,
    )?;
    let title = title.as_deref();
    let phase = phase.as_deref();
    let follow_up = follow_up.as_ref();

    // ── Phase-order guard (v0.14.3) ──────────────────────────────
    //
    // When a target phase is specified, check that earlier pending phases
    // don't exist (ordering violation) and that declared depends_on phases
    // are all Done. The check respects `[workflow].enforce_phase_order` in
    // `.ta/workflow.toml`: "off" skips it, "warn" prints and continues,
    // "block" prompts in interactive mode.
    if let Some(target_phase) = phase {
        let source_root = source
            .map(|p| p.to_owned())
            .unwrap_or_else(|| config.workspace_root.clone());
        if let Ok(phases) = plan::load_plan(&source_root) {
            // 1. Check declared depends_on for target phase — always enforced
            //    regardless of enforce_phase_order setting.
            let target = phases
                .iter()
                .find(|p| plan::phase_ids_match(&p.id, target_phase));
            if let Some(t) = target {
                let unmet_deps: Vec<String> = t
                    .depends_on
                    .iter()
                    .filter(|dep_id| {
                        !phases.iter().any(|p| {
                            plan::phase_ids_match(&p.id, dep_id)
                                && p.status == plan::PlanStatus::Done
                        })
                    })
                    .cloned()
                    .collect();
                if !unmet_deps.is_empty() {
                    anyhow::bail!(
                        "Cannot start phase {}: required dependencies are not done: {}.\n\
                         Complete those phases first, or remove the depends_on declaration.",
                        target_phase,
                        unmet_deps.join(", ")
                    );
                }
            }

            // 2. Check phase ordering (configurable).
            let wf_config = ta_submit::WorkflowConfig::load_or_default(
                &config.workspace_root.join(".ta/workflow.toml"),
            );
            let enforce_mode = wf_config.workflow.enforce_phase_order.as_str();
            if enforce_mode != "off" {
                // Collect ordering warnings relevant to the target phase:
                // only warn about pending phases that come before the target phase
                // in document order.
                let target_idx = phases
                    .iter()
                    .position(|p| plan::phase_ids_match(&p.id, target_phase));
                if let Some(target_pos) = target_idx {
                    let ordering_warnings: Vec<String> = plan::check_phase_order(&phases)
                        .into_iter()
                        .filter(|w| {
                            // Only show warnings for pending phases that appear
                            // before the target phase.
                            phases[..target_pos]
                                .iter()
                                .any(|p| p.status == plan::PlanStatus::Pending && w.contains(&p.id))
                        })
                        .collect();

                    if !ordering_warnings.is_empty() {
                        eprintln!(
                            "WARNING: Phase ordering violation detected for phase {}:",
                            target_phase
                        );
                        for w in &ordering_warnings {
                            eprintln!("  {}", w);
                        }

                        if enforce_mode == "block" && !headless && !no_launch {
                            eprint!("Start anyway? [y/N] ");
                            use std::io::BufRead;
                            let stdin = std::io::stdin();
                            let mut line = String::new();
                            let _ = stdin.lock().read_line(&mut line);
                            let answer = line.trim().to_lowercase();
                            if answer != "y" {
                                anyhow::bail!(
                                    "Goal creation cancelled due to phase ordering violation. \
                                     Complete pending phases first, or set \
                                     [workflow].enforce_phase_order = \"warn\" in .ta/workflow.toml."
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // ── Daemon connectivity (v0.11.2.6) ─────────────────────────
    //
    // In non-headless mode, ensure the daemon is running before creating the
    // goal. This prevents the agent from running without output streaming
    // (the daemon relays stdout to ta shell via SSE).
    // In headless mode the daemon already spawned us, so skip the check.
    if !headless && !no_launch {
        super::daemon::ensure_running(&config.workspace_root)?;
    }

    // When --objective-file is provided without a title, derive the title from
    // the first Markdown heading in the file (strips leading `# ` / `## `).
    let derived_title: Option<String> = if title.is_none() {
        objective_file.and_then(|p| {
            std::fs::read_to_string(p).ok().and_then(|text| {
                text.lines()
                    .find(|l| l.starts_with('#') && !l.starts_with("##"))
                    .map(|l| l.trim_start_matches('#').trim().to_string())
                    .filter(|s| !s.is_empty())
            })
        })
    } else {
        None
    };
    let title_ref = title.or(derived_title.as_deref());
    let title = title_ref.ok_or_else(|| {
        anyhow::anyhow!(
            "Title is required. Provide it as an argument, or add a `# Heading` to your --objective-file."
        )
    })?;

    // ── Workflow routing (v0.13.7) ───────────────────────────────
    //
    // Resolve the workflow kind from flag, then config, then default.
    // serial-phases and swarm are dispatched via their dedicated execute_*
    // functions in main.rs when --phases / --sub-goals are provided.
    // When used without those flags (e.g. just --workflow serial-phases),
    // fall through as single-agent for backwards compat.
    let workflow_kind = resolve_workflow(workflow, &config.workspace_root);
    match &workflow_kind {
        WorkflowKind::SingleAgent => {}
        WorkflowKind::SerialPhases => {
            if !quiet {
                println!(
                    "Workflow: serial-phases (single-agent mode — use --phases to enable \
                     multi-phase chaining with gate evaluation)"
                );
            }
        }
        WorkflowKind::Swarm => {
            if !quiet {
                println!(
                    "Workflow: swarm (single-agent mode — use --sub-goals to enable \
                     parallel sub-goal execution)"
                );
            }
        }
        WorkflowKind::Unknown(name) => {
            if !ta_workflow::WorkflowCatalog::is_known(name) {
                eprintln!(
                    "Warning: unknown workflow '{}'. \
                     Run `ta workflow list --builtin` to see available built-in workflows. \
                     Falling back to single-agent.",
                    name
                );
            }
        }
    }

    // ── Agent framework resolution (v0.13.8 items 2, 4, 19) ────────────────
    //
    // Resolution order (highest-priority wins):
    //   1. --agent flag (goal-level, passed as `agent` parameter)
    //   2. workflow YAML `agent_framework` field (resolved upstream)
    //   3. [agent].default_framework in daemon.toml (not available here — resolved by caller)
    //   4. built-in default "claude-code"
    //
    // For built-in frameworks (claude-code, codex, claude-flow), the existing
    // `builtin_agent_config()` provides richer launch config (headless_args etc.).
    // For custom/discovered manifests, `framework_to_launch_config()` converts the
    // manifest to an AgentLaunchConfig.
    //
    // `resolved_framework` is stored for memory bridge injection later.
    let resolved_framework =
        ta_runtime::AgentFrameworkManifest::resolve(agent, &config.workspace_root);

    // Gate ollama agent behind experimental flag (v0.13.17).
    if agent == "ollama" {
        let daemon_toml = config.workspace_root.join(".ta").join("daemon.toml");
        let experimental_enabled = if daemon_toml.exists() {
            std::fs::read_to_string(&daemon_toml)
                .ok()
                .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
                .and_then(|v| v.get("experimental")?.get("ollama_agent")?.as_bool())
                .unwrap_or(false)
        } else {
            false
        };
        if !experimental_enabled {
            anyhow::bail!(
                "ta-agent-ollama is an experimental preview. Enable with:\n\n  \
                 [experimental]\n  ollama_agent = true\n\nin .ta/daemon.toml. \
                 See docs/USAGE.md for known limitations."
            );
        }
    }

    let agent_config = {
        let framework_source = if agent != "claude-code" {
            "goal --agent flag"
        } else {
            "default"
        };
        match &resolved_framework {
            Some(f) if !f.builtin => {
                // Custom manifest — log and build config from it.
                tracing::info!(
                    framework = %f.name,
                    command = %f.command,
                    source = framework_source,
                    "Selected custom agent framework"
                );
                if !quiet {
                    println!(
                        "Agent framework: {} — {} (source: {})",
                        f.name, f.description, framework_source
                    );
                }
                framework_to_launch_config(f)
            }
            Some(f) if f.name != "claude-code" => {
                // Known built-in (codex, claude-flow, ollama).
                tracing::info!(
                    framework = %f.name,
                    source = framework_source,
                    "Selected built-in agent framework"
                );
                if !quiet {
                    println!("Agent framework: {} ({})", f.name, f.description);
                }
                agent_launch_config(agent, source)
            }
            None => {
                eprintln!(
                    "Warning: unknown agent framework '{}' — falling back to claude-code.",
                    agent
                );
                eprintln!("  Run `ta agent frameworks` to see available frameworks.");
                tracing::warn!(
                    framework = %agent,
                    "Unknown agent framework — falling back to claude-code"
                );
                agent_launch_config("claude-code", source)
            }
            _ => {
                // claude-code (default) — no announcement needed.
                tracing::debug!(
                    framework = "claude-code",
                    source = framework_source,
                    "Selected agent framework"
                );
                agent_launch_config(agent, source)
            }
        }
    };
    // agent_config is mutable so we can extend its env with framework-specific vars.
    let mut agent_config = agent_config;

    // Disk space pre-flight (v0.11.3 item 28).
    {
        let wf = ta_submit::WorkflowConfig::load_or_default(
            &config.workspace_root.join(".ta/workflow.toml"),
        );
        let check_path = if config.staging_dir.exists() {
            &config.staging_dir
        } else {
            &config.workspace_root
        };
        match ta_submit::check_disk_space_mb(check_path) {
            Ok(mb) if mb < wf.staging.min_disk_mb => {
                eprintln!(
                    "WARNING: Low disk space ({} MB available, {} MB recommended).\n  \
                     Free up space or adjust [staging].min_disk_mb in .ta/workflow.toml.\n",
                    mb, wf.staging.min_disk_mb,
                );
            }
            Err(e) => tracing::warn!("Could not check disk space: {}", e),
            _ => {}
        }
    }

    // Staging size cap check (v0.15.6.2): if total staging exceeds [gc] max_staging_gb,
    // GC the oldest terminal dirs before allocating a new workspace.
    // This runs before the goal is created so we don't start work we can't store.
    super::gc::enforce_staging_cap(config);

    // 1. Start the goal (creates overlay workspace), or reuse an existing one
    //    when --goal-id is passed (v0.9.5.1: prevents duplicate goal creation
    //    when the MCP orchestrator's ta_goal_start already created the goal).
    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    let goal = if let Some(existing_id) = existing_goal_id {
        let goal_uuid = uuid::Uuid::parse_str(existing_id)
            .map_err(|e| anyhow::anyhow!("Invalid --goal-id '{}': {}", existing_id, e))?;
        let mut existing = goal_store
            .get(goal_uuid)?
            .ok_or_else(|| anyhow::anyhow!("Goal {} not found", existing_id))?;

        // The MCP tool creates the goal record but the overlay workspace
        // (full copy of source) hasn't been created yet. Create it now.
        let source_dir = source
            .map(|p| p.to_owned())
            .or_else(|| existing.source_dir.clone())
            .unwrap_or_else(|| config.workspace_root.clone());
        let source_dir = source_dir.canonicalize().unwrap_or(source_dir);
        let excludes = ta_workspace::ExcludePatterns::load(&source_dir);
        // v0.13.13: Use configured staging strategy.
        let workflow = ta_submit::config::WorkflowConfig::load_or_default(&source_dir);
        let staging_mode = match workflow.staging.strategy {
            ta_submit::config::StagingStrategy::Full => ta_workspace::OverlayStagingMode::Full,
            ta_submit::config::StagingStrategy::Smart => ta_workspace::OverlayStagingMode::Smart,
            ta_submit::config::StagingStrategy::RefsCow => {
                ta_workspace::OverlayStagingMode::RefsCow
            }
        };
        let overlay = ta_workspace::OverlayWorkspace::create_with_strategy(
            goal_uuid.to_string(),
            &source_dir,
            &config.staging_dir,
            excludes,
            staging_mode,
        )?;

        // Capture source snapshot for conflict detection.
        let snapshot_json = overlay
            .snapshot()
            .and_then(|snap| serde_json::to_value(snap).ok());

        // Update goal with overlay workspace paths.
        existing.workspace_path = overlay.staging_dir().to_path_buf();
        existing.source_dir = Some(source_dir);
        existing.source_snapshot = snapshot_json;
        if let Some(p) = phase {
            existing.plan_phase = Some(p.to_string());
        }
        goal_store.save(&existing)?;

        println!("Reusing existing goal: {}", goal_uuid);
        println!("  Title:   {}", existing.title);
        println!("  Staging: {}", existing.workspace_path.display());
        existing
    } else {
        super::goal::execute(
            &super::goal::GoalCommands::Start {
                title: title.to_string(),
                source: source.map(|p| p.to_path_buf()),
                objective: objective.to_string(),
                agent: agent.to_string(),
                phase: phase.map(|p| p.to_string()),
                follow_up: follow_up.cloned(),
                objective_file: objective_file.map(|p| p.to_path_buf()),
            },
            config,
        )?;

        // Get the goal we just created (most recent).
        let goals = goal_store.list()?;
        goals
            .first()
            .ok_or_else(|| anyhow::anyhow!("Failed to find created goal"))?
            .clone()
    };

    // Mark as macro goal if --macro was specified, and store heartbeat_required (v0.13.14).
    {
        let mut updated_goal = goal.clone();
        if macro_goal {
            updated_goal.is_macro = true;
        }
        updated_goal.heartbeat_required = agent_config.heartbeat_required;
        goal_store.save(&updated_goal)?;
    }

    let goal_id = goal.goal_run_id.to_string();
    let staging_path = goal.workspace_path.clone();

    // 2. Inject context and settings into the staging workspace.
    if agent_config.injects_context_file {
        // Load context budget config (v0.14.3.1).
        let ctx_wf = ta_submit::WorkflowConfig::load_or_default(
            &config.workspace_root.join(".ta/workflow.toml"),
        );
        let context_budget_chars = ctx_wf.workflow.context_budget_chars;
        let done_window = ctx_wf.workflow.plan_done_window;
        let pending_window = ctx_wf.workflow.plan_pending_window;
        let context_mode = ctx_wf.workflow.context_mode.clone();

        tracing::info!(
            goal_id = %goal.goal_run_id,
            staging = %staging_path.display(),
            "Injecting CLAUDE.md context into staging workspace"
        );
        inject_claude_md(
            &staging_path,
            title,
            &goal_id,
            goal.plan_phase.as_deref(),
            goal.source_dir.as_deref(),
            goal.parent_goal_id,
            &goal_store,
            config,
            macro_goal,
            interactive,
            follow_up_context.as_deref(),
            context_budget_chars,
            done_window,
            pending_window,
            &context_mode,
        )?;
        tracing::info!(goal_id = %goal.goal_run_id, "CLAUDE.md injected");

        // Inject persona section if --persona was specified (v0.14.20).
        if let Some(pname) = persona_name {
            match ta_goal::PersonaConfig::load(&config.workspace_root, pname) {
                Ok(persona) => {
                    let persona_section = persona.to_claude_md_section();
                    let claude_md_path = staging_path.join("CLAUDE.md");
                    if let Ok(existing) = std::fs::read_to_string(&claude_md_path) {
                        let updated = format!("{}{}", existing, persona_section);
                        std::fs::write(&claude_md_path, updated)?;
                    }
                    tracing::info!(persona = %pname, "Persona injected into CLAUDE.md");
                }
                Err(e) => {
                    anyhow::bail!(
                        "Could not load persona '{}': {}. Check .ta/personas/{}.toml exists.",
                        pname,
                        e,
                        pname
                    );
                }
            }
        }

        // v0.14.3.1: Warn at goal start when projected context > 80% of budget.
        if context_budget_chars > 0 {
            let sizes = compute_context_section_sizes(
                title,
                &goal_id,
                goal.plan_phase.as_deref(),
                goal.source_dir.as_deref(),
                config,
                done_window,
                pending_window,
            );
            let total = sizes.total();
            let warn_threshold = context_budget_chars * 80 / 100;
            if total > warn_threshold {
                let pct = (total * 100) / context_budget_chars;
                eprintln!(
                    "[warn] Injected context is {} chars ({}% of {}k budget). \
                     Run 'ta context size' for a breakdown. \
                     Set [workflow] context_budget_chars in .ta/workflow.toml to adjust.",
                    total,
                    pct,
                    context_budget_chars / 1_000
                );
            }
        }
    }
    // v0.12.5: For non-Claude agents that set context_file, write a generic
    // agent_context.md with the same sections (memory, plan, etc.).
    if let Some(ref ctx_file) = agent_config.context_file {
        inject_agent_context_file(
            &staging_path,
            title,
            &goal_id,
            goal.plan_phase.as_deref(),
            config,
            ctx_file,
            goal.source_dir.as_deref(),
        )?;
    }
    if agent_config.injects_settings {
        inject_claude_settings(&staging_path, source)?;
    }

    // v0.13.8 item 8: Env/Arg-mode context injection for non-prepend frameworks.
    // For custom frameworks with context_inject = "env" or "arg", write the goal
    // context to .ta/goal_context.md and set TA_GOAL_CONTEXT (env mode) or
    // inject --context <path> args (arg mode). This runs in addition to — or
    // instead of — the CLAUDE.md prepend path above.
    let framework_env_extras: std::collections::HashMap<String, String> = {
        use ta_runtime::{inject_context_arg, inject_context_env, ContextInjectMode};
        if let Some(ref fw) = resolved_framework {
            if !fw.builtin
                && !matches!(
                    fw.context_inject,
                    ContextInjectMode::Prepend | ContextInjectMode::None
                )
            {
                let context_text = build_goal_context_text(
                    title,
                    &goal_id,
                    goal.plan_phase.as_deref(),
                    config,
                    goal.source_dir.as_deref(),
                );
                match fw.context_inject {
                    ContextInjectMode::Env => {
                        match inject_context_env(&staging_path, &context_text) {
                            Ok(r) => {
                                tracing::info!(framework = %fw.name, "Injected context via env (TA_GOAL_CONTEXT)");
                                r.env_vars
                            }
                            Err(e) => {
                                tracing::warn!(framework = %fw.name, "Failed to inject env context: {}", e);
                                Default::default()
                            }
                        }
                    }
                    ContextInjectMode::Arg => {
                        match inject_context_arg(&staging_path, &context_text, "--context") {
                            Ok(r) => {
                                tracing::info!(framework = %fw.name, "Injected context via arg (--context)");
                                r.env_vars // no env vars for arg mode, but keeps structure
                            }
                            Err(e) => {
                                tracing::warn!(framework = %fw.name, "Failed to inject arg context: {}", e);
                                Default::default()
                            }
                        }
                    }
                    _ => Default::default(),
                }
            } else {
                Default::default()
            }
        } else {
            Default::default()
        }
    };

    // Inject TA MCP server config into .mcp.json for macro goals (#60).
    // Without this, the agent sees MCP tool documentation in CLAUDE.md but
    // can't actually call the tools because no MCP server is configured.
    if macro_goal {
        inject_mcp_server_config(&staging_path)?;
    }

    // v0.13.8 item 11: Memory bridge — MCP mode.
    // For frameworks with memory.inject = "mcp" (Claude Code, Codex, Claude-Flow),
    // inject ta-memory as a local MCP server so the agent can call memory tools natively.
    // This happens for all goals when the framework supports MCP memory, not just macro goals.
    {
        use ta_runtime::MemoryInjectMode;
        if let Some(ref fw) = resolved_framework {
            if matches!(fw.memory.inject, MemoryInjectMode::Mcp) {
                if let Err(e) = inject_memory_mcp_server(&staging_path) {
                    tracing::warn!(framework = %fw.name, "Failed to inject ta-memory MCP server: {}", e);
                }
            }
        }
    }

    // v0.13.8 item 12: Memory bridge — context mode.
    // For frameworks with memory.inject = "context", serialize relevant memory
    // entries into the context file alongside goal context.
    {
        use ta_runtime::MemoryInjectMode;
        if let Some(ref fw) = resolved_framework {
            if matches!(fw.memory.inject, MemoryInjectMode::Context) {
                let context_file_name = &fw.context_file;
                let max_entries = fw.memory.max_entries;
                let tags_filter = fw.memory.tags.clone();
                let recency_days = fw.memory.recency_days;
                inject_memory_context(
                    &staging_path,
                    context_file_name,
                    max_entries,
                    goal.plan_phase.as_deref(),
                    &tags_filter,
                    recency_days,
                    config,
                );
            }
        }
    }

    // Merge framework env extras into agent_config so they are passed to the process.
    agent_config.env.extend(framework_env_extras);

    // VCS environment isolation (v0.13.17.3).
    // Inject VCS env vars before the agent spawns to prevent index-lock
    // collisions and accidental commits to the developer's real repo.
    {
        use ta_submit::SourceAdapter;
        let workflow_toml = config.workspace_root.join(".ta/workflow.toml");
        let wf = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);
        let vcs_config = wf.vcs.agent.clone();

        // Only apply if the project is a git repo (detected by .git/ in source).
        let source_is_git = config.workspace_root.join(".git").exists()
            || goal
                .source_dir
                .as_ref()
                .map(|d| d.join(".git").exists())
                .unwrap_or(false);

        if source_is_git && vcs_config.git_mode != "inherit" {
            let git_adapter = ta_submit::GitAdapter::new(&staging_path);
            match git_adapter.stage_env(&staging_path, &vcs_config) {
                Ok(vcs_env) => {
                    if !vcs_env.is_empty() {
                        tracing::info!(
                            mode = %vcs_config.git_mode,
                            vars = vcs_env.len(),
                            "VCS isolation: injecting git env vars"
                        );
                        agent_config.env.extend(vcs_env);
                        // Record isolation mode on the goal record.
                        let vcs_isolation_label = format!("{} (git)", vcs_config.git_mode);
                        let goals_dir_for_vcs = config.goals_dir.clone();
                        if let Ok(store) = ta_goal::GoalRunStore::new(&goals_dir_for_vcs) {
                            if let Ok(Some(mut g)) = store.get(goal.goal_run_id) {
                                g.vcs_isolation = Some(vcs_isolation_label.clone());
                                let _ = store.save(&g);
                            }
                        }
                        if !quiet {
                            println!("VCS isolation: {} mode", vcs_config.git_mode);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "VCS isolation setup failed — agent will inherit VCS env"
                    );
                }
            }
        }
    }

    // Emit GoalStarted event to FsEventStore (v0.9.4.1).
    // Skip when reusing an existing goal — the MCP tool already emitted GoalStarted.
    if existing_goal_id.is_none() {
        use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
        let events_dir = config.workspace_root.join(".ta").join("events");
        let event_store = FsEventStore::new(&events_dir);
        let event = SessionEvent::GoalStarted {
            goal_id: goal.goal_run_id,
            title: title.to_string(),
            agent_id: agent.to_string(),
            phase: goal.plan_phase.clone(),
        };
        if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
            tracing::warn!("Failed to persist GoalStarted event: {}", e);
        }
        // In headless mode the daemon scans stderr for this line to register
        // a UUID→output_key alias so :tail and SSE auto-tail can connect.
        // Uses ta_events::GOAL_STARTED_SENTINEL — must stay in sync with cmd.rs scanner.
        if headless {
            eprintln!(
                "{} \"{}\" ({})",
                ta_events::GOAL_STARTED_SENTINEL,
                title,
                goal.goal_run_id
            );
        }
    }

    // Build the prompt string.
    let prompt = if objective.is_empty() {
        format!("Implement: {}", title)
    } else {
        format!("{}\n\nObjective: {}", title, objective)
    };

    if no_launch {
        // Restore injected files — user will run the agent manually,
        // so injected context stays only if they re-run `ta run`.
        if agent_config.injects_context_file {
            restore_claude_md(&staging_path)?;
        }
        if agent_config.injects_settings {
            restore_claude_settings(&staging_path)?;
        }
        // Restore MCP config unconditionally (v0.13.17.5 Bug 1 fix).
        // restore_mcp_server_config() is a no-op when no backup exists,
        // so calling it for non-macro goals is safe.
        restore_mcp_server_config(&staging_path)?;

        println!("\nWorkspace ready. To use manually:");
        println!("  cd {}", staging_path.display());
        if let Some(ref pre) = agent_config.pre_launch {
            println!(
                "  {} {}  # required init step",
                pre.command,
                pre.args.join(" ")
            );
        }
        println!("  {} {}", agent_config.command, shell_quote(&prompt));
        println!();
        println!("When done, build the draft:");
        println!("  ta draft build {}", goal_id);
        println!("  # Or: ta draft build --latest");
        return Ok(());
    }

    // 3. Run pre-launch command if needed (e.g., hive-mind init).
    if let Some(ref pre) = agent_config.pre_launch {
        println!(
            "\nRunning pre-launch: {} {}...",
            pre.command,
            pre.args.join(" ")
        );
        let pre_status = std::process::Command::new(&pre.command)
            .args(&pre.args)
            .current_dir(&staging_path)
            .status();
        match pre_status {
            Ok(exit) if exit.success() => {}
            Ok(exit) => {
                // §4.3 constitution fix: clean up injected files before returning on error.
                if agent_config.injects_context_file {
                    let _ = restore_claude_md(&staging_path);
                }
                if agent_config.injects_settings {
                    let _ = restore_claude_settings(&staging_path);
                }
                // Unconditional restore (v0.13.17.5 Bug 1 fix): no-op if no backup exists.
                let _ = restore_mcp_server_config(&staging_path);
                return Err(anyhow::anyhow!(
                    "Pre-launch command exited with status {}. \
                     Injected files have been cleaned up.",
                    exit
                ));
            }
            Err(e) => {
                // §4.3 constitution fix: clean up injected files before returning on error.
                if agent_config.injects_context_file {
                    let _ = restore_claude_md(&staging_path);
                }
                if agent_config.injects_settings {
                    let _ = restore_claude_settings(&staging_path);
                }
                // Unconditional restore (v0.13.17.5 Bug 1 fix): no-op if no backup exists.
                let _ = restore_mcp_server_config(&staging_path);
                return Err(anyhow::anyhow!(
                    "Failed to run pre-launch command '{}': {}. \
                     Injected files have been cleaned up.",
                    pre.command,
                    e
                ));
            }
        }
    }

    // 4. Create interactive session if --interactive.
    let mut session_store = if interactive {
        let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone())?;
        let channel_id = format!("cli:{}", std::process::id());
        let session = InteractiveSession::new(goal.goal_run_id, channel_id, agent.to_string());
        store.save(&session)?;
        println!("\nInteractive session: {}", session.session_id);
        println!("  Channel: {}", session.channel_id);
        Some((store, session))
    } else {
        None
    };

    // 5. Launch the agent in the staging directory.
    tracing::info!(
        goal_id = %goal.goal_run_id,
        agent = %agent_config.command,
        staging = %staging_path.display(),
        "Launching agent"
    );
    if !quiet {
        println!(
            "\nLaunching {} in staging workspace...",
            agent_config.command
        );
        println!("  Working dir: {}", staging_path.display());
        if headless {
            println!("  Mode: headless (non-interactive, piped output)");
        } else if interactive {
            println!("  Mode: interactive (PTY capture + session orchestration)");
        }
        if macro_goal {
            println!("  Mode: macro goal (inner-loop iteration enabled)");
        }
        println!();
    }

    // Track the start time BEFORE agent launch to compute accurate duration (v0.9.5.1).
    let agent_start = std::time::Instant::now();

    // PID persistence callback (v0.11.2.4): saves agent PID to goal record
    // immediately after spawn so the daemon watchdog can check liveness.
    let goal_run_id = goal.goal_run_id;
    let goals_dir_for_pid = config.goals_dir.clone();
    let save_pid = move |pid: u32| {
        if let Ok(store) = GoalRunStore::new(&goals_dir_for_pid) {
            if let Ok(Some(mut g)) = store.get(goal_run_id) {
                g.agent_pid = Some(pid);
                let _ = store.save(&g);
                tracing::info!(goal_id = %goal_run_id, pid = pid, "Stored agent PID for watchdog");
            }
        }
    };

    // Events directory for lifecycle event emission (v0.13.3).
    let events_dir_for_launch = config.workspace_root.join(".ta").join("events");

    // Choose launch mode: headless (piped), PTY-interactive, or simple.
    //
    // Non-interactive paths (headless, quiet, simple) go through
    // launch_agent_via_runtime which uses the RuntimeAdapter from the agent
    // config.  This emits AgentSpawned/AgentExited lifecycle events and
    // supports non-"process" runtimes (OCI, VM) when configured.
    //
    // PTY interactive mode falls back to the direct launch_agent_interactive
    // path because PTY allocation requires direct OS child process control.
    //
    // quiet=true uses headless launch to suppress streaming output (item 21).
    // Type alias for the guidance log — on Unix this contains captured human inputs
    // from PTY sessions; on Windows the Vec is always empty.
    type GuidanceLog = Vec<(String, String)>;
    let launch_result: std::io::Result<(std::process::ExitStatus, GuidanceLog)> = if headless
        || quiet
    {
        launch_agent_via_runtime(
            &agent_config,
            &staging_path,
            &prompt,
            true,
            Some(&save_pid),
            goal.goal_run_id,
            &events_dir_for_launch,
        )
        .map(|exit| (exit, Vec::new()))
    } else if interactive {
        #[cfg(unix)]
        {
            launch_agent_interactive(&agent_config, &staging_path, &prompt, &mut session_store).map(
                |(exit, log)| {
                    (
                        exit,
                        log.iter()
                            .map(|(req, resp)| (format!("{}", req), format!("{}", resp)))
                            .collect(),
                    )
                },
            )
        }
        #[cfg(not(unix))]
        {
            eprintln!("Warning: interactive PTY mode is not available on Windows. Falling back to simple mode.");
            launch_agent_via_runtime(
                &agent_config,
                &staging_path,
                &prompt,
                false,
                Some(&save_pid),
                goal.goal_run_id,
                &events_dir_for_launch,
            )
            .map(|exit| (exit, Vec::new()))
        }
    } else {
        launch_agent_via_runtime(
            &agent_config,
            &staging_path,
            &prompt,
            false,
            Some(&save_pid),
            goal.goal_run_id,
            &events_dir_for_launch,
        )
        .map(|exit| (exit, Vec::new()))
    };

    match launch_result {
        Ok((exit, guidance_log)) => {
            // Atomically clear agent PID and transition to Finalizing (v0.13.14).
            //
            // This single write closes the watchdog race: by transitioning from
            // Running → Finalizing before the slow draft-build starts, the watchdog
            // will see Finalizing (not Running) and skip liveness checks until the
            // draft is built and state transitions to PrReady. Without this, the
            // watchdog could detect PID gone + state=Running and mark the goal Failed
            // while draft creation is still in progress.
            if let Ok(store) = GoalRunStore::new(&config.goals_dir) {
                if let Ok(Some(mut g)) = store.get(goal.goal_run_id) {
                    g.agent_pid = None;
                    if matches!(g.state, ta_goal::GoalRunState::Running) {
                        // Store our own PID so the watchdog can confirm we're
                        // still alive and skip the finalize timeout (v0.13.17).
                        let _ = g.transition(ta_goal::GoalRunState::Finalizing {
                            exit_code: exit.code().unwrap_or(-1),
                            finalize_started_at: chrono::Utc::now(),
                            run_pid: Some(std::process::id()),
                        });
                    }
                    let _ = store.save(&g);
                }
            }

            // Write progress journal entry: agent_exit (v0.14.12).
            append_progress_journal(
                &config.goals_dir,
                goal.goal_run_id,
                "agent_exit",
                &format!("agent exited with code {}", exit.code().unwrap_or(-1)),
            );

            {
                let elapsed_secs = agent_start.elapsed().as_secs();
                if exit.success() {
                    tracing::info!(
                        goal_id = %goal.goal_run_id,
                        elapsed_secs = elapsed_secs,
                        "Agent exited successfully — building draft"
                    );
                    println!("\nAgent exited. Building draft...");
                } else {
                    tracing::info!(
                        goal_id = %goal.goal_run_id,
                        elapsed_secs = elapsed_secs,
                        exit_code = exit.code().unwrap_or(-1),
                        "Agent exited with error — building draft anyway"
                    );
                    println!(
                        "\nAgent exited with status {}. Building draft anyway...",
                        exit
                    );
                }
            }

            // Emit GoalCompleted or GoalFailed based on exit code (v0.9.4.1).
            {
                use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
                let events_dir = config.workspace_root.join(".ta").join("events");
                let event_store = FsEventStore::new(&events_dir);
                let duration = agent_start.elapsed().as_secs();
                let event = if exit.success() {
                    SessionEvent::GoalCompleted {
                        goal_id: goal.goal_run_id,
                        title: title.to_string(),
                        duration_secs: Some(duration),
                    }
                } else {
                    SessionEvent::GoalFailed {
                        goal_id: goal.goal_run_id,
                        error: format!("Agent exited with status {}", exit),
                        exit_code: exit.code(),
                    }
                };
                if let Err(e) = event_store.append(&EventEnvelope::new(event)) {
                    tracing::warn!("Failed to persist goal exit event: {}", e);
                }
            }

            // v0.13.8 item 13: Exit-file memory ingestion.
            // If the agent wrote new memories to TA_MEMORY_OUT (exit-file mode),
            // ingest them into ta-memory now.
            ingest_memory_out(&staging_path, config);

            // Log guidance interactions to session if any.
            if let Some((ref store, ref mut session)) = session_store {
                for (req_str, resp_str) in &guidance_log {
                    session.log_message(
                        "ta-system",
                        &format!("Guidance: {} → {}", req_str, resp_str),
                    );
                }
                store.save(session)?;
            }
        }
        Err(e) => {
            // Emit GoalFailed event on launch failure (v0.9.4.1).
            {
                use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
                let events_dir = config.workspace_root.join(".ta").join("events");
                let event_store = FsEventStore::new(&events_dir);
                let event = SessionEvent::GoalFailed {
                    goal_id: goal.goal_run_id,
                    error: format!("Failed to launch agent: {}", e),
                    exit_code: None,
                };
                if let Err(err) = event_store.append(&EventEnvelope::new(event)) {
                    tracing::warn!("Failed to persist GoalFailed event: {}", err);
                }
            }

            // Mark interactive session as aborted on launch failure.
            if let Some((ref store, ref mut session)) = session_store {
                session.log_message("ta-system", &format!("Agent launch failed: {}", e));
                let _ = session.transition(InteractiveSessionState::Aborted);
                let _ = store.save(session);
            }

            // Detect command-not-found regardless of error path:
            // - Direct spawn (legacy launch_agent): e.kind() == NotFound
            // - RuntimeAdapter path: kind() is Other but message says "Spawn failed:"
            let is_not_found =
                e.kind() == std::io::ErrorKind::NotFound || e.to_string().contains("Spawn failed:");
            if is_not_found {
                // Restore injected files before returning — agent won't run.
                if agent_config.injects_context_file {
                    restore_claude_md(&staging_path)?;
                }
                if agent_config.injects_settings {
                    restore_claude_settings(&staging_path)?;
                }
                // Unconditional restore (v0.13.17.5 Bug 1 fix): no-op if no backup exists.
                restore_mcp_server_config(&staging_path)?;

                println!(
                    "\n'{}' command not found. To use manually:",
                    agent_config.command
                );
                println!("  cd {}", staging_path.display());
                println!("  {} {}", agent_config.command, shell_quote(&prompt));
                println!();
                // Windows-specific diagnostic: Claude Code is installed as claude.cmd
                // via npm. If which resolves claude.cmd, the fix is to update TA.
                #[cfg(windows)]
                {
                    let found = which::which(&agent_config.command)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "not found on PATH".to_string());
                    println!("Windows diagnostic:");
                    println!("  Searching for '{}': {}", agent_config.command, found);
                    if found.to_lowercase().ends_with(".cmd")
                        || found.to_lowercase().ends_with(".bat")
                    {
                        println!("  Found as .cmd batch file — update TA to v0.13.4+ which handles this automatically.");
                    } else {
                        println!(
                            "  PATH: {}",
                            std::env::var("PATH").unwrap_or_else(|_| "(not set)".to_string())
                        );
                    }
                    println!();
                }
                println!("When done, build the draft:");
                println!("  ta draft build {}", goal_id);
                return Ok(());
            }
            // §4.4 constitution fix: all launch error paths must clean up injected files,
            // not just NotFound. Errors like PermissionDenied or ExecFormatError also
            // leave the agent not running — staging must be restored.
            if agent_config.injects_context_file {
                let _ = restore_claude_md(&staging_path);
            }
            if agent_config.injects_settings {
                let _ = restore_claude_settings(&staging_path);
            }
            // Unconditional restore (v0.13.17.5 Bug 1 fix): no-op if no backup exists.
            let _ = restore_mcp_server_config(&staging_path);
            return Err(anyhow::anyhow!(
                "Failed to launch {}: {}. Injected files have been cleaned up.",
                agent_config.command,
                e
            ));
        }
    }

    // 6. Restore injected files before diffing (removes TA injection).
    if agent_config.injects_context_file {
        restore_claude_md(&staging_path)?;
    }
    if agent_config.injects_settings {
        restore_claude_settings(&staging_path)?;
    }
    // Unconditional restore (v0.13.17.5 Bug 1 fix): the guard `if macro_goal` was wrong.
    // inject_mcp_server_config() also runs for memory-MCP goals (non-macro), so the
    // backup may exist for any goal type. restore_mcp_server_config() checks for the
    // backup file first and is a no-op when it doesn't exist.
    restore_mcp_server_config(&staging_path)?;

    // 6a-restore-check: Verify .mcp.json was restored correctly (v0.13.17.5 item 3).
    // If staging differs from source after restore, log a warning — catches any future
    // inject/restore asymmetries before they reach the diff.
    {
        let source_dir = goal.source_dir.as_deref().unwrap_or(&config.workspace_root);
        let staging_mcp = staging_path.join(".mcp.json");
        let source_mcp = source_dir.join(".mcp.json");
        match (staging_mcp.exists(), source_mcp.exists()) {
            (true, true) => {
                let staging_content = std::fs::read(&staging_mcp).unwrap_or_default();
                let source_content = std::fs::read(&source_mcp).unwrap_or_default();
                if staging_content != source_content {
                    tracing::warn!(
                        staging = %staging_mcp.display(),
                        "Warning: .mcp.json restore may be incomplete — staging differs from source. \
                         This file will be excluded from the diff."
                    );
                }
            }
            (true, false) => {
                tracing::warn!(
                    staging = %staging_mcp.display(),
                    "Warning: .mcp.json exists in staging but not in source after restore. \
                     This file will be excluded from the diff."
                );
            }
            _ => {} // Both absent or only source has it — nothing to check.
        }
    }

    // 6a. Log the file-change count in staging vs source (v0.12.6 item 7).
    {
        let source_dir = goal.source_dir.as_deref().unwrap_or(&config.workspace_root);
        let changed_count = count_changed_files(&staging_path, source_dir);
        tracing::info!(
            goal_id = %goal.goal_run_id,
            changed_files = changed_count,
            staging = %staging_path.display(),
            "Files changed in staging workspace after agent exit"
        );
    }

    // 6b. Pre-draft verification gate (v0.10.8).
    //     Runs [verify] commands from workflow.toml before creating the draft.
    let verification_warnings = if !skip_verify {
        let workflow_toml = staging_path.join(".ta/workflow.toml");
        let workflow_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);

        if !workflow_config.verify.commands.is_empty() {
            let mut result =
                super::verify::run_verification(&workflow_config.verify, &staging_path);

            if !result.passed {
                match workflow_config.verify.on_failure {
                    ta_submit::VerifyOnFailure::Block => {
                        let total_cmds = workflow_config.verify.commands.len();
                        let failed_count = result.warnings.len();

                        println!();
                        println!(
                            "Verification failed — {} of {} checks failed. Draft NOT created.",
                            failed_count, total_cmds
                        );

                        // Show full output for each failed command so the user
                        // can see exactly what went wrong (v0.10.18.1 item 4).
                        for w in &result.warnings {
                            println!();
                            println!(
                                "--- {} (exit code: {}) ---",
                                w.command,
                                w.exit_code.map_or("N/A".into(), |c| c.to_string())
                            );
                            if !w.output.is_empty() {
                                let lines: Vec<&str> = w.output.lines().collect();
                                let max_lines = 40;
                                if lines.len() <= max_lines {
                                    for line in &lines {
                                        println!("  {}", line);
                                    }
                                } else {
                                    // Show first 20 + last 20 lines with a gap indicator.
                                    for line in &lines[..20] {
                                        println!("  {}", line);
                                    }
                                    println!("  ... ({} lines omitted) ...", lines.len() - 40);
                                    for line in &lines[lines.len() - 20..] {
                                        println!("  {}", line);
                                    }
                                }
                            }
                            println!("---");
                        }

                        // Send desktop notification for verification failure.
                        super::notify::verification_failed(
                            &workflow_config.notify,
                            failed_count,
                            total_cmds,
                        );

                        // Interactive re-entry: if stdin is a TTY and not headless,
                        // offer to re-launch the agent immediately (v0.10.18.1 item 1).
                        if !headless && std::io::stdin().is_terminal() {
                            println!();
                            println!("Re-enter the agent to fix these issues? [Y/n] ");
                            let mut answer = String::new();
                            if std::io::stdin().read_line(&mut answer).is_ok() {
                                let answer = answer.trim().to_lowercase();
                                if answer.is_empty() || answer == "y" || answer == "yes" {
                                    println!("Re-launching agent to fix verification failures...");
                                    println!();

                                    // Build failure context for re-injection.
                                    let mut failure_context = String::new();
                                    failure_context
                                        .push_str("\n## Verification Failures (auto-injected)\n\n");
                                    failure_context.push_str(
                                        "The following verification commands failed. Fix these issues and ensure all checks pass.\n\n",
                                    );
                                    for w in &result.warnings {
                                        failure_context.push_str(&format!("### `{}`\n", w.command));
                                        failure_context.push_str(&format!(
                                            "Exit code: {}\n",
                                            w.exit_code
                                                .map_or("N/A".to_string(), |c| c.to_string())
                                        ));
                                        if !w.output.is_empty() {
                                            failure_context.push_str("```\n");
                                            // Show more output for agent context (up to 60 lines).
                                            for line in w.output.lines().take(60) {
                                                failure_context.push_str(line);
                                                failure_context.push('\n');
                                            }
                                            failure_context.push_str("```\n");
                                        }
                                        failure_context.push('\n');
                                    }

                                    // Re-inject CLAUDE.md with failure context.
                                    if agent_config.injects_context_file {
                                        let claude_md_path = staging_path.join("CLAUDE.md");
                                        if let Ok(existing) =
                                            std::fs::read_to_string(&claude_md_path)
                                        {
                                            let updated =
                                                format!("{}\n{}", existing, failure_context);
                                            let _ = std::fs::write(&claude_md_path, updated);
                                        }
                                    }

                                    let fix_prompt = "Verification checks failed after your previous changes. \
                                         Fix the issues described in the CLAUDE.md 'Verification Failures' \
                                         section. Run the failing commands to confirm they pass before exiting."
                                        .to_string();

                                    let relaunch_result = launch_agent(
                                        &agent_config,
                                        &staging_path,
                                        &fix_prompt,
                                        None,
                                    );
                                    match relaunch_result {
                                        Ok(exit) if exit.success() => {
                                            println!("\nAgent fix session exited successfully.");
                                        }
                                        Ok(exit) => {
                                            println!(
                                                "\nAgent fix session exited with status {}.",
                                                exit
                                            );
                                        }
                                        Err(e) => {
                                            println!("Failed to re-launch agent: {}", e);
                                            println!("Draft NOT created. To fix manually: ta run --follow-up");
                                            // §4.5: restore re-injected CLAUDE.md before returning.
                                            if agent_config.injects_context_file {
                                                let _ = restore_claude_md(&staging_path);
                                            }
                                            return Ok(());
                                        }
                                    }

                                    // Restore CLAUDE.md after re-launch.
                                    if agent_config.injects_context_file {
                                        restore_claude_md(&staging_path)?;
                                    }

                                    // Re-run verification after the fix.
                                    let recheck = super::verify::run_verification(
                                        &workflow_config.verify,
                                        &staging_path,
                                    );
                                    if !recheck.passed {
                                        println!();
                                        println!(
                                            "Verification STILL failing after agent fix session."
                                        );
                                        println!("Draft NOT created.");
                                        for w in &recheck.warnings {
                                            println!();
                                            println!(
                                                "--- {} (exit code: {}) ---",
                                                w.command,
                                                w.exit_code.map_or("N/A".into(), |c| c.to_string())
                                            );
                                            if !w.output.is_empty() {
                                                for line in w.output.lines().take(20) {
                                                    println!("  {}", line);
                                                }
                                            }
                                            println!("---");
                                        }
                                        println!();
                                        println!("To fix: ta run --follow-up");
                                        return Ok(());
                                    }
                                    println!("Verification passed after agent fix session.");
                                    result = recheck;
                                    // Fall through to draft build.
                                } else {
                                    println!();
                                    println!("To fix and retry:");
                                    println!("  ta run --follow-up     — re-enter the agent to fix issues");
                                    println!(
                                        "  ta verify {}  — re-run verification manually",
                                        &goal_id[..8]
                                    );
                                    return Ok(());
                                }
                            } else {
                                return Ok(());
                            }
                        } else {
                            // Non-interactive: just print instructions.
                            println!();
                            println!("To fix and retry:");
                            println!("  ta run --follow-up     — re-enter the agent to fix issues");
                            println!(
                                "  ta verify {}  — re-run verification manually",
                                &goal_id[..8]
                            );
                            println!();
                            println!("To bypass verification:");
                            println!("  ta run --skip-verify   — skip verification on next run");
                            return Ok(());
                        }
                    }
                    ta_submit::VerifyOnFailure::Warn => {
                        println!();
                        println!("Verification failed — creating draft with warnings.");
                        // Continue to draft build, passing warnings.
                    }
                    ta_submit::VerifyOnFailure::Agent => {
                        println!();
                        println!(
                            "Verification failed — re-launching agent with failure context..."
                        );
                        println!();

                        // Build failure context for re-injection (v0.10.14).
                        let mut failure_context = String::new();
                        failure_context.push_str("\n## Verification Failures (auto-injected)\n\n");
                        failure_context.push_str(
                            "The following verification commands failed. Fix these issues and ensure all checks pass.\n\n",
                        );
                        for w in &result.warnings {
                            failure_context.push_str(&format!("### `{}`\n", w.command));
                            failure_context.push_str(&format!(
                                "Exit code: {}\n",
                                w.exit_code.map_or("N/A".to_string(), |c| c.to_string())
                            ));
                            if !w.output.is_empty() {
                                failure_context.push_str("```\n");
                                for line in w.output.lines().take(30) {
                                    failure_context.push_str(line);
                                    failure_context.push('\n');
                                }
                                failure_context.push_str("```\n");
                            }
                            failure_context.push('\n');
                        }

                        // Re-inject CLAUDE.md with failure context.
                        if agent_config.injects_context_file {
                            let claude_md_path = staging_path.join("CLAUDE.md");
                            if let Ok(existing) = std::fs::read_to_string(&claude_md_path) {
                                let updated = format!("{}\n{}", existing, failure_context);
                                let _ = std::fs::write(&claude_md_path, updated);
                            }
                        }

                        // Re-launch the agent with a fix prompt.
                        let fix_prompt = "Verification checks failed after your previous changes. \
                             Fix the issues described in the CLAUDE.md 'Verification Failures' \
                             section. Run the failing commands to confirm they pass before exiting."
                            .to_string();
                        println!(
                            "Re-launching {} to fix verification failures...",
                            agent_config.command
                        );
                        let relaunch_result =
                            launch_agent(&agent_config, &staging_path, &fix_prompt, None);
                        match relaunch_result {
                            Ok(exit) if exit.success() => {
                                println!("\nAgent fix session exited successfully.");
                            }
                            Ok(exit) => {
                                println!("\nAgent fix session exited with status {}.", exit);
                            }
                            Err(e) => {
                                println!("Failed to re-launch agent: {}", e);
                                println!("Draft NOT created. To fix manually: ta run --follow-up");
                                // §4.6: restore re-injected CLAUDE.md before returning.
                                if agent_config.injects_context_file {
                                    let _ = restore_claude_md(&staging_path);
                                }
                                return Ok(());
                            }
                        }

                        // Restore CLAUDE.md after re-launch.
                        if agent_config.injects_context_file {
                            restore_claude_md(&staging_path)?;
                        }

                        // Re-run verification after the fix.
                        let recheck =
                            super::verify::run_verification(&workflow_config.verify, &staging_path);
                        if !recheck.passed {
                            println!();
                            println!("Verification STILL failing after agent fix session.");
                            println!("Draft NOT created.");
                            for w in &recheck.warnings {
                                println!();
                                println!(
                                    "--- {} (exit code: {}) ---",
                                    w.command,
                                    w.exit_code.map_or("N/A".into(), |c| c.to_string())
                                );
                                if !w.output.is_empty() {
                                    for line in w.output.lines().take(20) {
                                        println!("  {}", line);
                                    }
                                    let total = w.output.lines().count();
                                    if total > 20 {
                                        println!("  ... ({} more lines)", total - 20);
                                    }
                                }
                                println!("---");
                            }
                            println!();
                            println!("To fix: ta run --follow-up");
                            return Ok(());
                        }
                        println!("Verification passed after agent fix session.");
                        // Replace result with the passing recheck so
                        // result.warnings is empty below.
                        result = recheck;
                    }
                }
            }
            result.warnings
        } else {
            Vec::new()
        }
    } else {
        println!("  Skipping verification (--skip-verify).");
        Vec::new()
    };

    // 6c. Run required_checks and build validation_log (v0.13.17).
    //     These are hard-evidence checks embedded in the DraftPackage.
    //     Non-zero exit code blocks `ta draft approve` unless --override is passed.
    let validation_log = {
        let workflow_toml = staging_path.join(".ta/workflow.toml");
        let workflow_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);

        if workflow_config.required_checks.is_empty() {
            vec![]
        } else {
            // Helper: update progress note without blocking on errors (v0.13.17).
            let update_note_required = |note: &str| {
                if let Ok(store) = GoalRunStore::new(&config.goals_dir) {
                    let _ = store.update_progress_note(goal.goal_run_id, note);
                }
            };
            let checks = &workflow_config.required_checks;
            update_note_required(&format!(
                "running required_checks ({} checks)",
                checks.len()
            ));
            println!();
            println!("Running required_checks ({} checks)...", checks.len());
            let mut entries = Vec::new();
            for cmd_str in checks {
                let started = std::time::Instant::now();
                // Split command into program + args.
                let parts: Vec<&str> = cmd_str.split_whitespace().collect();
                if parts.is_empty() {
                    continue;
                }
                let output = std::process::Command::new(parts[0])
                    .args(&parts[1..])
                    .current_dir(&staging_path)
                    .output();
                let duration_secs = started.elapsed().as_secs();
                let (exit_code, stdout_tail) = match output {
                    Ok(out) => {
                        let combined = format!(
                            "{}{}",
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr)
                        );
                        let lines: Vec<&str> = combined.lines().collect();
                        let tail_start = lines.len().saturating_sub(20);
                        let tail = lines[tail_start..].join("\n");
                        (out.status.code().unwrap_or(-1), tail)
                    }
                    Err(e) => (-1, format!("Failed to run command: {}", e)),
                };
                let pass = if exit_code == 0 { "+" } else { "x" };
                println!("  [{}] {} ({}s)", pass, cmd_str, duration_secs);
                entries.push(ta_changeset::draft_package::ValidationEntry {
                    command: cmd_str.clone(),
                    exit_code,
                    duration_secs,
                    stdout_tail,
                });
            }
            let failed_count = entries.iter().filter(|e| e.exit_code != 0).count();
            if failed_count > 0 {
                println!(
                    "  {} of {} required_checks FAILED.",
                    failed_count,
                    entries.len()
                );
                println!("  Draft will be created but `ta draft approve` will be blocked.");
                println!("  Use `ta draft approve --override` to bypass.");
            } else {
                println!("  All required_checks passed.");
            }
            entries
        }
    };

    // 6d. Run supervisor agent for goal alignment and constitution review (v0.13.17.4).
    //     Runs after required_checks, before draft build. Falls back to warn on failure.
    let supervisor_review = {
        let workflow_toml = staging_path.join(".ta/workflow.toml");
        let wf = ta_submit::WorkflowConfig::load_or_default(&workflow_toml);
        let sup_cfg = wf.supervisor.clone();

        if !sup_cfg.enabled {
            println!("  Supervisor review: disabled.");
            None
        } else {
            // Update progress note.
            if let Ok(store) = GoalRunStore::new(&config.goals_dir) {
                let _ = store.update_progress_note(goal.goal_run_id, "running supervisor review");
            }

            println!();
            println!("Running supervisor review...");

            // Build run config — staging_path enables manifest-based custom agents.
            let run_config = ta_changeset::SupervisorRunConfig {
                enabled: true,
                agent: sup_cfg.agent.clone(),
                verdict_on_block: sup_cfg.verdict_on_block.clone(),
                constitution_path: sup_cfg.constitution_path.clone(),
                skip_if_no_constitution: sup_cfg.skip_if_no_constitution,
                timeout_secs: sup_cfg.timeout_secs,
                api_key_env: sup_cfg.api_key_env.clone(),
                staging_path: Some(staging_path.to_path_buf()),
            };

            // Load constitution text.
            let constitution_text = ta_changeset::load_constitution(&staging_path, &run_config);
            if constitution_text.is_some() {
                println!("  Constitution: loaded.");
            } else if !run_config.skip_if_no_constitution {
                println!("  Warning: no constitution file found.");
            }

            // Collect changed files from staging dir via change_summary.json or file walk.
            let changed_files: Vec<String> = collect_changed_files(&staging_path);

            // For follow-up goals the supervisor must understand the full parent chain
            // scope, not just the immediate goal title. Build an extended objective
            // that includes the parent context so the supervisor doesn't incorrectly
            // flag files that are in scope for the broader chain.
            let supervisor_objective: String = if let Some(ctx) = follow_up_context.as_deref() {
                // Extract just the parent goal titles from the follow-up context block
                // (first non-empty line is the parent description).
                let parent_summary: String = ctx
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(
                    "{}\n\nContext — this is a follow-up goal. Parent chain scope:\n{}",
                    title, parent_summary
                )
            } else {
                title.to_string()
            };

            // Dispatch to the appropriate supervisor agent (builtin/claude-code/codex/ollama/manifest).
            let review = ta_changeset::invoke_supervisor_agent(
                &supervisor_objective,
                &changed_files,
                constitution_text.as_deref(),
                &run_config,
            );

            // Print summary.
            let verdict_label = match review.verdict {
                ta_changeset::SupervisorVerdict::Pass => "[PASS]",
                ta_changeset::SupervisorVerdict::Warn => "[WARN]",
                ta_changeset::SupervisorVerdict::Block => "[BLOCK]",
            };
            println!("  Supervisor: {} {}", verdict_label, review.summary);
            if !review.findings.is_empty() {
                for finding in review.findings.iter().take(3) {
                    println!("    - {}", finding);
                }
            }

            // Update progress note with verdict.
            if let Ok(store) = GoalRunStore::new(&config.goals_dir) {
                let note = format!("Supervisor review: {}", review.verdict);
                let _ = store.update_progress_note(goal.goal_run_id, &note);
            }

            Some(review)
        }
    };

    // 7. Build draft package from the diff.
    //    In macro sessions, the agent may have already submitted/applied drafts
    //    via MCP tools, transitioning the goal out of Running state. Only build
    //    a draft if the goal is still running.
    let goal_current = goal_store
        .get(goal.goal_run_id)?
        .unwrap_or_else(|| goal.clone());
    // v0.13.14: Also build draft when state is Finalizing (set just above on agent exit).
    let draft_built = if matches!(
        goal_current.state,
        ta_goal::GoalRunState::Running | ta_goal::GoalRunState::Finalizing { .. }
    ) {
        // v0.13.17 / v0.13.17.2: Emit structured progress notes at each finalize step.
        // Daemon stores the latest note; `ta goal status` and `ta goal list` display it.
        let update_finalize_note = |note: &str| {
            if let Ok(store) = GoalRunStore::new(&config.goals_dir) {
                let _ = store.update_progress_note(goal.goal_run_id, note);
            }
        };

        // v0.15.6.2: In non-headless mode, spawn draft build as a background process so
        // `ta run` returns immediately after the agent exits. The background process does
        // the slow diff+package work; the watchdog tracks it via `run_pid`.
        // In headless mode, build synchronously so the caller gets the draft ID in JSON.
        let spawned_async = if !headless {
            try_spawn_background_draft_build(
                config,
                &goal_id,
                title,
                goal.goal_run_id,
                &verification_warnings,
                &validation_log,
                supervisor_review.as_ref(),
            )
        } else {
            None
        };

        if let Some(bg_pid) = spawned_async {
            // Background build was spawned — ta run is done.
            // The background process will transition the goal to PrReady when done.
            update_finalize_note("building draft package (background)");
            append_progress_journal(
                &config.goals_dir,
                goal.goal_run_id,
                "draft_build_spawned",
                &format!("draft build spawned as background process PID {}", bg_pid),
            );
            println!("\nAgent exited. Building draft in background — you'll be notified when it's ready.");
        } else {
            // Synchronous build (headless mode or spawn failed — fall back to inline).
            update_finalize_note("diffing workspace files");
            update_finalize_note("building draft package");
            super::draft::execute(
                &super::draft::DraftCommands::Build {
                    goal_id: goal_id.clone(),
                    summary: format!("Changes from goal: {}", title),
                    latest: false,
                    apply_context_file: None,
                },
                config,
            )?;

            // 7a. If there are verification warnings (warn mode), attach them to the draft.
            if !verification_warnings.is_empty() {
                if let Some(draft_id) = find_latest_draft_id(config, &goal_id) {
                    if let Ok(draft_uuid) = uuid::Uuid::parse_str(&draft_id) {
                        if let Ok(mut pkg) = super::draft::load_package(config, draft_uuid) {
                            pkg.verification_warnings = verification_warnings;
                            let _ = super::draft::save_package(config, &pkg);
                        }
                    }
                }
            }

            // 7b. Attach validation_log to the draft (v0.13.17).
            if !validation_log.is_empty() {
                if let Some(draft_id) = find_latest_draft_id(config, &goal_id) {
                    if let Ok(draft_uuid) = uuid::Uuid::parse_str(&draft_id) {
                        if let Ok(mut pkg) = super::draft::load_package(config, draft_uuid) {
                            pkg.validation_log = validation_log;
                            let _ = super::draft::save_package(config, &pkg);
                        }
                    }
                }
            }

            // 7c. Attach supervisor_review to the draft (v0.13.17.4).
            if let Some(ref sup_review) = supervisor_review {
                if let Some(draft_id) = find_latest_draft_id(config, &goal_id) {
                    if let Ok(draft_uuid) = uuid::Uuid::parse_str(&draft_id) {
                        if let Ok(mut pkg) = super::draft::load_package(config, draft_uuid) {
                            pkg.supervisor_review = Some(sup_review.clone());
                            let _ = super::draft::save_package(config, &pkg);
                        }
                    }
                }
            }

            // v0.13.17.2: Final progress note — draft is ready with its ID.
            // v0.14.12: Also write to the progress journal.
            if let Some(draft_id) = find_latest_draft_id(config, &goal_id) {
                let short_id = &draft_id[..8.min(draft_id.len())];
                update_finalize_note(&format!("draft ready — ID: {}", short_id));
                append_progress_journal(
                    &config.goals_dir,
                    goal.goal_run_id,
                    "draft_built",
                    &format!("draft {} created", short_id),
                );
            }
        }

        true
    } else {
        println!(
            "\nGoal is already in {} state — skipping automatic draft build.",
            goal_current.state
        );
        println!("(Drafts were submitted during the macro session.)");
        false
    };

    // 7b. Desktop notification when draft is ready (v0.10.18.1).
    if draft_built {
        let workflow_toml = staging_path.join(".ta/workflow.toml");
        let notify_config = ta_submit::WorkflowConfig::load_or_default(&workflow_toml).notify;
        let draft_display_id =
            find_latest_draft_id(config, &goal_id).unwrap_or_else(|| goal_id[..8].to_string());
        super::notify::draft_ready(&notify_config, title, &draft_display_id);
    }

    // 7c. Auto-capture goal completion into memory (v0.5.6).
    if draft_built {
        auto_capture_goal_completion(config, &goal, &staging_path);
    }

    // 8. Mark interactive session as completed.
    if let Some((store, mut session)) = session_store {
        if draft_built {
            session.log_message("ta-system", "Agent exited, draft built");
        } else {
            session.log_message(
                "ta-system",
                &format!("Agent exited, goal already {}", goal_current.state),
            );
        }
        let _ = session.transition(InteractiveSessionState::Completed);
        store.save(&session)?;
    }

    // In headless mode, output structured JSON for orchestrator consumption.
    // In quiet mode, print a minimal human-readable summary (no JSON).
    if headless && !quiet {
        let draft_id = if draft_built {
            // Find the most recent draft for this goal.
            find_latest_draft_id(config, &goal_id)
        } else {
            None
        };

        let output = serde_json::json!({
            "goal_id": goal_id,
            "draft_built": draft_built,
            "draft_id": draft_id,
            "state": goal_current.state.to_string(),
        });
        println!("\n__TA_HEADLESS_RESULT__:{}", output);
    } else if quiet {
        // Quiet mode: always print completion/failure summary regardless of verbosity.
        let draft_id = if draft_built {
            find_latest_draft_id(config, &goal_id)
        } else {
            None
        };
        if draft_built {
            println!(
                "Goal {} completed — draft {} ready for review.",
                &goal_id[..8.min(goal_id.len())],
                draft_id.as_deref().unwrap_or("(see ta draft list)")
            );
        } else {
            println!(
                "Goal {} completed — state: {}.",
                &goal_id[..8.min(goal_id.len())],
                goal_current.state
            );
        }
    } else {
        if draft_built {
            println!("\nNext steps:");
            println!("  ta draft list");
            println!("  ta draft view <draft-id>");
            println!("  ta draft approve <draft-id>");
            println!("  ta draft apply <draft-id> --git-commit");
        } else {
            println!("\nNext steps:");
            println!("  ta draft list      — view submitted drafts");
            println!("  ta goal status     — check goal state");
        }
        if interactive {
            println!("  ta session list");
            println!("  ta session show <session-id>");
        }
    }

    Ok(())
}

#[cfg(unix)]
/// Resume an existing interactive session by re-launching the agent in its workspace.
fn execute_resume(
    config: &GatewayConfig,
    session_id_prefix: &str,
    agent: &str,
) -> anyhow::Result<()> {
    let store = InteractiveSessionStore::new(config.interactive_sessions_dir.clone())?;
    let goal_store = GoalRunStore::new(&config.goals_dir)?;

    // Find the session by ID or prefix.
    let mut session = find_session_by_prefix(&store, session_id_prefix)?;

    // Validate session state — must be paused or active to resume.
    if !session.is_alive() {
        anyhow::bail!(
            "Session {} is {} — cannot resume",
            session.session_id,
            session.state
        );
    }

    // Transition from Paused → Active if needed.
    if session.state == InteractiveSessionState::Paused {
        session.transition(InteractiveSessionState::Active)?;
    }

    // Look up the goal to find the staging workspace.
    let goal = goal_store
        .list()?
        .into_iter()
        .find(|g| g.goal_run_id == session.goal_id)
        .ok_or_else(|| anyhow::anyhow!("Goal {} not found for session", session.goal_id))?;

    let staging_path = &goal.workspace_path;
    if !staging_path.exists() {
        // v0.7.5: PTY health check — workspace is gone.
        // Offer to close the session cleanly instead of erroring.
        eprintln!(
            "Staging workspace no longer exists: {}",
            staging_path.display()
        );
        eprintln!("The child process appears to have exited or the workspace was cleaned up.");
        eprintln!();
        eprintln!("Options:");
        eprintln!(
            "  ta session close {}  — close the session cleanly",
            &session.session_id.to_string()[..8]
        );
        eprintln!(
            "  ta session abort {}  — abort and discard",
            &session.session_id.to_string()[..8]
        );
        anyhow::bail!(
            "Staging workspace no longer exists: {}",
            staging_path.display()
        );
    }

    // v0.7.5: PTY health check — verify workspace health before reattaching.
    let health = super::session::check_session_health(&store, &goal_store, &session);
    match health {
        super::session::SessionHealthStatus::WorkspaceMissing => {
            eprintln!("Warning: staging workspace health check failed.");
            eprintln!("Options:");
            eprintln!(
                "  ta session close {}  — build a draft and close",
                &session.session_id.to_string()[..8]
            );
            eprintln!(
                "  ta session abort {}  — abort the session",
                &session.session_id.to_string()[..8]
            );
            anyhow::bail!("Session workspace is not healthy — cannot resume");
        }
        super::session::SessionHealthStatus::Healthy {
            has_staging_changes,
        } => {
            if has_staging_changes {
                println!("Note: staging workspace has uncommitted changes from a previous run.");
            }
        }
    }

    let agent_config = agent_launch_config(agent, goal.source_dir.as_deref());

    // Build resume command: use agent config's resume_cmd if available,
    // otherwise use the standard launch command.
    let resume_cmd = agent_config
        .interactive
        .as_ref()
        .and_then(|ic| ic.resume_cmd.as_deref())
        .map(|cmd| cmd.replace("{session_id}", &session.session_id.to_string()));

    // Update channel ID for this terminal.
    session.channel_id = format!("cli:{}", std::process::id());
    session.log_message("ta-system", "Session resumed");
    store.save(&session)?;

    println!("\nResuming session: {}", session.session_id);
    println!("  Goal: {}", session.goal_id);
    println!("  Agent: {}", session.agent_id);
    println!("  Workspace: {}", staging_path.display());
    println!("  Mode: interactive (PTY capture)");
    println!();

    let mut session_store = Some((store, session));

    let launch_result = if let Some(ref cmd_str) = resume_cmd {
        // Use the resume command from agent config.
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        let (cmd, args) = parts
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("Empty resume command"))?;

        let resume_config = AgentLaunchConfig {
            command: cmd.to_string(),
            args_template: args.iter().map(|a| a.to_string()).collect(),
            injects_context_file: false,
            injects_settings: false,
            pre_launch: None,
            env: agent_config.env.clone(),
            shell: None,
            name: None,
            description: None,
            interactive: None,
            alignment: None,
            headless_args: Vec::new(),
            non_interactive_env: Default::default(),
            auto_answers: Vec::new(),
            context_file: None,
            runtime: Default::default(),
            heartbeat_required: false,
        };

        launch_agent_interactive(&resume_config, staging_path, "", &mut session_store)
    } else {
        // Re-launch with the original prompt (empty for resume).
        launch_agent_interactive(&agent_config, staging_path, "", &mut session_store)
    };

    match launch_result {
        Ok((exit, _guidance_log)) => {
            if exit.success() {
                println!("\nAgent exited successfully.");
            } else {
                println!("\nAgent exited with status {}.", exit);
            }
        }
        Err(e) => {
            if let Some((ref store, ref mut session)) = session_store {
                session.log_message("ta-system", &format!("Resume launch failed: {}", e));
                let _ = session.transition(InteractiveSessionState::Aborted);
                let _ = store.save(session);
            }
            return Err(anyhow::anyhow!("Failed to resume agent: {}", e));
        }
    }

    // Mark session as paused (can be resumed again) or completed.
    if let Some((store, mut session)) = session_store {
        session.log_message("ta-system", "Agent exited from resumed session");
        // Transition to Paused so it can be resumed again, not Completed.
        let _ = session.transition(InteractiveSessionState::Paused);
        store.save(&session)?;
        println!("\nSession paused. To resume again:");
        println!("  ta run --resume {}", &session.session_id.to_string()[..8]);
    }

    Ok(())
}

#[cfg(unix)]
/// Find a session by full UUID or prefix match.
fn find_session_by_prefix(
    store: &InteractiveSessionStore,
    prefix: &str,
) -> anyhow::Result<InteractiveSession> {
    // Try exact UUID parse first.
    if let Ok(uuid) = uuid::Uuid::parse_str(prefix) {
        return Ok(store.load(uuid)?);
    }

    // Prefix match.
    let all = store.list()?;
    let matches: Vec<_> = all
        .into_iter()
        .filter(|s| s.session_id.to_string().starts_with(prefix))
        .collect();

    match matches.len() {
        0 => anyhow::bail!("No session found matching '{}'", prefix),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => anyhow::bail!(
            "Ambiguous prefix '{}' matches {} sessions. Use a longer prefix.",
            prefix,
            n
        ),
    }
}

// ── Agent launch ────────────────────────────────────────────────

/// Build a `Command` for `command` with `args`, handling Windows `.cmd`/`.bat` wrappers.
///
/// On Windows, npm-installed tools (Claude Code, npx, etc.) are `.cmd` batch files.
/// `Command::new("claude")` only finds `claude.exe` and fails with NotFound.
/// We use `which::which()` (which respects `PATHEXT`) and wrap `.cmd`/`.bat`
/// files in `cmd.exe /c` so they execute correctly.
fn resolve_agent_command(command: &str, args: &[String]) -> std::process::Command {
    #[cfg(windows)]
    {
        if let Ok(resolved) = which::which(command) {
            let ext = resolved
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "cmd" || ext == "bat" {
                tracing::debug!(
                    command = command,
                    resolved = %resolved.display(),
                    "Wrapping .cmd/.bat in cmd.exe /c"
                );
                let mut cmd = std::process::Command::new("cmd");
                cmd.arg("/c").arg(resolved);
                for arg in args {
                    cmd.arg(arg);
                }
                return cmd;
            }
        }
    }
    let mut cmd = std::process::Command::new(command);
    for arg in args {
        cmd.arg(arg);
    }
    cmd
}

/// Launch an agent process with template-substituted arguments.
///
/// If `pid_callback` is provided, it is called with the child PID immediately
/// after spawning (before waiting for exit). This allows the caller to persist
/// the PID for watchdog liveness checks (v0.11.2.4).
fn launch_agent(
    config: &AgentLaunchConfig,
    staging_path: &Path,
    prompt: &str,
    pid_callback: Option<&dyn Fn(u32)>,
) -> std::io::Result<std::process::ExitStatus> {
    let args: Vec<String> = config
        .args_template
        .iter()
        .map(|t| t.replace("{prompt}", prompt))
        .collect();
    let mut cmd = resolve_agent_command(&config.command, &args);
    cmd.current_dir(staging_path);

    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn()?;
    if let Some(cb) = pid_callback {
        cb(child.id());
    }
    child.wait()
}

/// Launch an agent in headless (non-interactive) mode.
///
/// Stdout/stderr are piped and streamed to the parent process, but no PTY is allocated.
/// Suitable for orchestrator-driven goals where no human interaction is expected.
///
/// If `pid_callback` is provided, it is called with the child PID immediately
/// after spawning (before waiting for exit) for watchdog liveness checks (v0.11.2.4).
// Retained for documentation and potential direct use in inner-loop paths
// that bypass the RuntimeAdapter (e.g., macro-goal re-launch).
#[allow(dead_code)]
fn launch_agent_headless(
    config: &AgentLaunchConfig,
    staging_path: &Path,
    prompt: &str,
    pid_callback: Option<&dyn Fn(u32)>,
) -> std::io::Result<std::process::ExitStatus> {
    use std::io::{BufRead, BufReader};

    let mut cmd = std::process::Command::new(&config.command);
    cmd.current_dir(staging_path);

    for arg_template in &config.args_template {
        let arg = arg_template.replace("{prompt}", prompt);
        cmd.arg(arg);
    }

    // Append agent-specific headless args (v0.10.18.4 item 2).
    // E.g., Claude Code gets --output-format stream-json for rich streaming output.
    for arg in &config.headless_args {
        cmd.arg(arg);
    }

    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    // Set non-interactive env vars (v0.10.18.5 item 1).
    // These are ONLY applied in headless mode to suppress interactive prompts.
    for (key, value) in &config.non_interactive_env {
        cmd.env(key, value);
    }

    // Pipe stdout so we can stream it without a PTY.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::inherit());
    // No stdin — headless mode.
    cmd.stdin(std::process::Stdio::null());

    let mut child = cmd.spawn()?;

    // Report PID for watchdog liveness tracking (v0.11.2.4).
    if let Some(cb) = pid_callback {
        cb(child.id());
    }

    // Stream stdout lines to the parent's stdout verbatim.
    // No prefix — the daemon's output schema parser expects raw stream-json lines
    // starting with '{' so it can extract human-readable text from JSON events.
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            println!("{}", line);
        }
    }

    child.wait()
}

#[cfg(unix)]
/// Launch an agent in interactive PTY mode with stdin interleaving and guidance logging.
///
/// Returns the exit status and a log of (InteractionRequest, InteractionResponse) pairs
/// for every human guidance message injected during the session.
fn launch_agent_interactive(
    config: &AgentLaunchConfig,
    staging_path: &Path,
    prompt: &str,
    session_store: &mut Option<(InteractiveSessionStore, InteractiveSession)>,
) -> std::io::Result<(
    std::process::ExitStatus,
    Vec<(InteractionRequest, InteractionResponse)>,
)> {
    // Build args with template substitution.
    let args: Vec<String> = config
        .args_template
        .iter()
        .map(|t| t.replace("{prompt}", prompt))
        .collect();

    // Launch via PTY.
    let pty_config = pty_capture::PtyLaunchConfig {
        command: &config.command,
        args,
        working_dir: staging_path,
        env_vars: &config.env,
        output_sink: None, // Default: TerminalSink (stdout). Replace for Slack/email.
    };

    let result = pty_capture::run_interactive_pty(pty_config)?;

    // Convert captured human inputs into InteractionRequest/Response pairs for audit.
    let mut guidance_log = Vec::new();
    for input in &result.human_inputs {
        let request = InteractionRequest::new(
            InteractionKind::Custom("guidance".to_string()),
            serde_json::json!({
                "text": input.text,
                "timestamp": input.timestamp.to_rfc3339(),
            }),
            Urgency::Advisory,
        );

        let response =
            InteractionResponse::new(request.interaction_id, ta_changeset::Decision::Approve)
                .with_reasoning(&input.text)
                .with_responder("cli:stdin");

        // Log to interactive session.
        if let Some((ref store, ref mut session)) = session_store {
            session.log_message("human", &input.text);
            let _ = store.save(session);
        }

        guidance_log.push((request, response));
    }

    // Log captured agent output to session (summary only, not full output).
    if let Some((ref store, ref mut session)) = session_store {
        let total_bytes: usize = result.captured_output.iter().map(|c| c.data.len()).sum();
        session.log_message(
            "ta-system",
            &format!(
                "PTY session captured {} bytes of agent output, {} human inputs",
                total_bytes,
                result.human_inputs.len()
            ),
        );
        let _ = store.save(session);
    }

    Ok((result.exit_status, guidance_log))
}

/// Launch an agent via the RuntimeAdapter and wait for it to exit (v0.13.3).
///
/// This is the non-interactive, non-PTY path.  It replaces `launch_agent` and
/// `launch_agent_headless` for runtimes other than "process".  For the
/// built-in "process" runtime it produces identical behaviour.
///
/// Events emitted:
/// - `AgentSpawned` immediately after spawn (carries PID, runtime name, command)
/// - `AgentExited` after the process exits (carries exit code, duration)
/// - `RuntimeError` (instead of returning Err) on spawn failure, so callers
///   always get a structured event even when the agent never starts.
fn launch_agent_via_runtime(
    config: &AgentLaunchConfig,
    staging_path: &std::path::Path,
    prompt: &str,
    headless: bool,
    pid_callback: Option<&dyn Fn(u32)>,
    goal_id: uuid::Uuid,
    events_dir: &std::path::Path,
) -> std::io::Result<std::process::ExitStatus> {
    use std::io::{BufRead, BufReader};
    use ta_events::{EventEnvelope, EventStore, FsEventStore, SessionEvent};
    use ta_runtime::{RuntimeRegistry, SpawnRequest, StdinMode, StdoutMode};

    // Build the environment map with template variables already applied.
    let mut env = config.env.clone();
    if headless {
        env.extend(config.non_interactive_env.clone());
    }

    // Expand args (replace {prompt} template variable).
    let mut args: Vec<String> = config
        .args_template
        .iter()
        .map(|t| t.replace("{prompt}", prompt))
        .collect();
    if headless {
        args.extend_from_slice(&config.headless_args);
    }

    let stdin_mode = if headless {
        StdinMode::Null
    } else {
        StdinMode::Inherited
    };
    let stdout_mode = if headless {
        StdoutMode::Piped
    } else {
        StdoutMode::Inherited
    };

    let raw_request = SpawnRequest {
        command: config.command.clone(),
        args,
        env,
        working_dir: staging_path.to_path_buf(),
        stdin_mode,
        stdout_mode,
    };

    // Apply sandbox policy from workflow.toml [sandbox] section (v0.14.0).
    // The policy wraps the spawn request in sandbox-exec (macOS) or bwrap (Linux)
    // when `sandbox.enabled = true`. Disabled by default — no behaviour change on upgrade.
    let request = {
        use ta_runtime::{SandboxPolicy, SandboxProvider};
        let wf_toml = staging_path.join(".ta/workflow.toml");
        let wf = ta_submit::WorkflowConfig::load_or_default(&wf_toml);
        if wf.sandbox.enabled {
            // Check experimental.sandbox flag (v0.13.17).
            // If not enabled in [experimental], print a warning and skip sandboxing.
            let daemon_toml = staging_path.join(".ta").join("daemon.toml");
            let sandbox_experimental = if daemon_toml.exists() {
                std::fs::read_to_string(&daemon_toml)
                    .ok()
                    .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
                    .and_then(|v| v.get("experimental")?.get("sandbox")?.as_bool())
                    .unwrap_or(false)
            } else {
                false
            };
            if !sandbox_experimental {
                println!(
                    "Warning: Sandbox is experimental — set [experimental]\nsandbox = true \
                     in .ta/daemon.toml to enable. Running without sandbox."
                );
                tracing::warn!(
                    "sandbox.enabled=true but experimental.sandbox=false — skipping sandbox"
                );
                raw_request
            } else {
                let provider = SandboxPolicy::detect_provider();
                if provider == SandboxProvider::None {
                    tracing::warn!(
                    "sandbox.enabled=true but no sandbox provider available on this platform — \
                     running agent without sandboxing"
                );
                    raw_request
                } else {
                    let policy = SandboxPolicy {
                        enabled: true,
                        provider,
                        allow_read: wf
                            .sandbox
                            .allow_read
                            .iter()
                            .map(std::path::PathBuf::from)
                            .collect(),
                        allow_write: wf
                            .sandbox
                            .allow_write
                            .iter()
                            .map(std::path::PathBuf::from)
                            .collect(),
                        allow_network: wf.sandbox.allow_network.clone(),
                    };
                    tracing::info!(
                        provider = ?policy.provider,
                        "Applying sandbox policy to agent process"
                    );
                    policy.apply(raw_request)
                }
            } // end sandbox_experimental else
        } else {
            raw_request
        }
    };

    // Resolve the runtime (default: "process").
    let registry = RuntimeRegistry::new();
    let runtime = match registry.resolve(&config.runtime.name) {
        Ok(rt) => rt,
        Err(e) => {
            // Emit RuntimeError event so the problem is observable.
            let event_store = FsEventStore::new(events_dir);
            let _ = event_store.append(&EventEnvelope::new(SessionEvent::RuntimeError {
                goal_id: Some(goal_id),
                runtime: config.runtime.name.clone(),
                error: format!(
                    "Failed to resolve runtime '{}': {}. \
                     Check that ta-runtime-{} is installed in .ta/plugins/runtimes/ or on $PATH.",
                    config.runtime.name, e, config.runtime.name
                ),
            }));
            return Err(std::io::Error::other(format!(
                "Runtime '{}' not available: {}",
                config.runtime.name, e
            )));
        }
    };

    let agent_start = std::time::Instant::now();

    // Spawn the agent.
    let mut handle = match runtime.spawn(request) {
        Ok(h) => h,
        Err(e) => {
            let event_store = FsEventStore::new(events_dir);
            let _ = event_store.append(&EventEnvelope::new(SessionEvent::RuntimeError {
                goal_id: Some(goal_id),
                runtime: runtime.name().to_string(),
                error: format!(
                    "Agent spawn failed (runtime: {}, command: {}): {}",
                    runtime.name(),
                    config.command,
                    e
                ),
            }));
            return Err(std::io::Error::other(format!("Spawn failed: {}", e)));
        }
    };

    // Report PID for watchdog liveness tracking.
    if let Some(pid) = handle.pid() {
        if let Some(cb) = pid_callback {
            cb(pid);
        }
    }

    // Emit AgentSpawned event.
    {
        let event_store = FsEventStore::new(events_dir);
        let _ = event_store.append(&EventEnvelope::new(SessionEvent::AgentSpawned {
            goal_id,
            pid: handle.pid(),
            runtime: runtime.name().to_string(),
            agent_command: config.command.clone(),
        }));
    }

    // If headless, stream stdout lines to parent stdout.
    if headless {
        if let Some(stdout) = handle.take_stdout() {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                println!("{}", line);
            }
        }
    }

    // Wait for the agent to finish.
    let exit_status = match handle.wait() {
        Ok(s) => s,
        Err(e) => {
            let event_store = FsEventStore::new(events_dir);
            let _ = event_store.append(&EventEnvelope::new(SessionEvent::RuntimeError {
                goal_id: Some(goal_id),
                runtime: runtime.name().to_string(),
                error: format!("Agent wait() failed: {}", e),
            }));
            return Err(std::io::Error::other(format!("Wait failed: {}", e)));
        }
    };

    // Emit AgentExited event.
    {
        let event_store = FsEventStore::new(events_dir);
        let _ = event_store.append(&EventEnvelope::new(SessionEvent::AgentExited {
            goal_id,
            pid: handle.pid(),
            runtime: runtime.name().to_string(),
            exit_code: exit_status.code(),
            duration_secs: agent_start.elapsed().as_secs(),
        }));
    }

    Ok(exit_status)
}

/// Spawn draft build as a detached background process (v0.15.6.2).
///
/// Writes a context JSON file so the background process can attach verification
/// warnings, validation log, and supervisor review to the draft after building.
/// Updates the goal's `run_pid` so the watchdog tracks the background process.
///
/// Returns `Some(pid)` on success, `None` if spawn fails (caller falls back to sync).
fn try_spawn_background_draft_build(
    config: &GatewayConfig,
    goal_id: &str,
    title: &str,
    goal_run_id: uuid::Uuid,
    verification_warnings: &[ta_changeset::draft_package::VerificationWarning],
    validation_log: &[ta_changeset::draft_package::ValidationEntry],
    supervisor_review: Option<&ta_changeset::supervisor_review::SupervisorReview>,
) -> Option<u32> {
    // Write the deferred context file.
    let ctx_dir = config.workspace_root.join(".ta/draft-build-ctx");
    if let Err(e) = std::fs::create_dir_all(&ctx_dir) {
        tracing::warn!("background draft build: could not create ctx dir: {}", e);
        return None;
    }
    let ctx_path = ctx_dir.join(format!("{}.json", goal_id));
    let ctx = super::draft::DraftBuildContext {
        goal_id: goal_id.to_string(),
        verification_warnings: verification_warnings.to_vec(),
        validation_log: validation_log.to_vec(),
        supervisor_review: supervisor_review.cloned(),
    };
    if let Err(e) = std::fs::write(
        &ctx_path,
        serde_json::to_string_pretty(&ctx).unwrap_or_default(),
    ) {
        tracing::warn!(
            "background draft build: could not write context file: {}",
            e
        );
        return None;
    }

    // Locate the ta binary.
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("background draft build: could not find ta binary: {}", e);
            let _ = std::fs::remove_file(&ctx_path);
            return None;
        }
    };

    // Spawn: ta --project-root <workspace_root> draft build <goal_id>
    //            --apply-context-file <ctx_path>
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--project-root")
        .arg(&config.workspace_root)
        .arg("draft")
        .arg("build")
        .arg(goal_id)
        .arg("--summary")
        .arg(format!("Changes from goal: {}", title))
        .arg("--apply-context-file")
        .arg(&ctx_path)
        // Detach from terminal — background process runs independently.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Create a new process group so the child doesn't die when the terminal closes.
        cmd.process_group(0);
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("background draft build: spawn failed: {}", e);
            let _ = std::fs::remove_file(&ctx_path);
            return None;
        }
    };
    let bg_pid = child.id();

    // Update the goal's run_pid so the watchdog tracks the background process.
    if let Ok(store) = GoalRunStore::new(&config.goals_dir) {
        if let Ok(Some(mut g)) = store.get(goal_run_id) {
            if let ta_goal::GoalRunState::Finalizing {
                ref mut run_pid, ..
            } = g.state
            {
                *run_pid = Some(bg_pid);
            }
            let _ = store.save(&g);
        }
    }

    // Detach (don't wait for the child; it runs independently).
    std::mem::forget(child);

    tracing::info!(
        goal_id = goal_id,
        bg_pid = bg_pid,
        "Spawned background draft build process"
    );
    Some(bg_pid)
}

/// Find the most recent draft ID for a goal (headless output).
///
/// Returns the canonical display ID (`<shortref>/<seq>` when available, else UUID prefix)
/// so that the ID shown in completion messages resolves via `ta draft view/approve/apply`.
pub(crate) fn find_latest_draft_id(config: &GatewayConfig, goal_id: &str) -> Option<String> {
    use ta_changeset::draft_package::DraftPackage;
    use ta_changeset::draft_resolver::draft_canonical_id;

    let dir = &config.pr_packages_dir;
    if !dir.exists() {
        return None;
    }

    let mut drafts: Vec<DraftPackage> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let json = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str::<DraftPackage>(&json).ok()
        })
        .filter(|pkg| pkg.goal.goal_id == goal_id)
        .collect();

    drafts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    // Return the canonical display ID so the emitted string resolves via `ta draft view`.
    drafts.first().map(draft_canonical_id)
}

/// Simple shell quoting for display purposes.
fn shell_quote(s: &str) -> String {
    if s.contains(' ') || s.contains('\n') {
        format!("\"{}\"", s.replace('\"', "\\\""))
    } else {
        s.to_string()
    }
}

// ── Supervisor helpers (v0.13.17.4) ────────────────────────────

/// Collect changed file paths by reading `.ta/change_summary.json` written by the agent,
/// or falling back to a recursive walk of source files in the staging directory.
fn collect_changed_files(staging_path: &std::path::Path) -> Vec<String> {
    // Prefer reading from change_summary.json — more accurate than a directory walk.
    let summary_path = staging_path.join(".ta/change_summary.json");
    if summary_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&summary_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(changes) = json.get("changes").and_then(|c| c.as_array()) {
                    let paths: Vec<String> = changes
                        .iter()
                        .filter_map(|c| c.get("path").and_then(|p| p.as_str()))
                        .map(|s| s.to_string())
                        .collect();
                    if !paths.is_empty() {
                        return paths;
                    }
                }
            }
        }
    }

    // Fallback: collect source files from staging directory.
    let mut files = Vec::new();
    collect_source_files(staging_path, staging_path, &mut files, 0);
    files.truncate(50);
    files
}

/// Recursively collect source file paths relative to `root`.
fn collect_source_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    files: &mut Vec<String>,
    depth: usize,
) {
    if depth > 8 || files.len() >= 50 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            collect_source_files(root, &path, files, depth + 1);
        } else {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(
                ext,
                "rs" | "toml" | "md" | "json" | "yaml" | "ts" | "js" | "py"
            ) {
                if let Ok(rel) = path.strip_prefix(root) {
                    files.push(rel.to_string_lossy().to_string());
                }
            }
        }
    }
}

// ── Claude Code settings injection ──────────────────────────────
//
// Instead of --dangerously-skip-permissions, TA injects a
// .claude/settings.local.json that allows all standard tools but
// denies patterns from the forbidden-tools list. This makes the
// agent work uninterrupted in the staging sandbox while keeping
// community-maintained safety rails.

const SETTINGS_BACKUP: &str = ".ta/claude_settings_original";
const SETTINGS_REL_PATH: &str = ".claude/settings.local.json";
const FORBIDDEN_TOOLS_FILE: &str = ".ta-forbidden-tools";

/// Tools to allow in the injected Claude Code settings.
const DEFAULT_ALLOWED_TOOLS: &[&str] = &[
    "Bash(*)",
    "Read(*)",
    "Write(*)",
    "Edit(*)",
    "MultiEdit(*)",
    "Glob(*)",
    "Grep(*)",
    "WebFetch(*)",
    "WebSearch(*)",
    "NotebookEdit(*)",
    "Task(*)",
    "Skill(*)",
    "TodoRead(*)",
    "TodoWrite(*)",
];

/// Built-in forbidden tool patterns — community-maintained deny list.
/// These are always denied even in TA staging workspaces.
/// Add patterns here as the community identifies dangerous tools/commands.
const DEFAULT_FORBIDDEN_TOOLS: &[&str] = &[];

/// Load forbidden tool patterns from the project's `.ta-forbidden-tools` file.
/// Returns an empty vec if the file doesn't exist.
fn load_forbidden_tools(source_dir: Option<&Path>) -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_FORBIDDEN_TOOLS
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    if let Some(source) = source_dir {
        let path = source.join(FORBIDDEN_TOOLS_FILE);
        if path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                for line in contents.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('#') {
                        patterns.push(trimmed.to_string());
                    }
                }
            }
        }
    }

    patterns
}

/// Inject .claude/settings.local.json with TA permissions.
/// Allows all standard tools, denies forbidden patterns.
fn inject_claude_settings(staging_path: &Path, source_dir: Option<&Path>) -> anyhow::Result<()> {
    let settings_path = staging_path.join(SETTINGS_REL_PATH);
    let backup_path = staging_path.join(SETTINGS_BACKUP);

    // §4.1 constitution fix: if a backup already exists (follow-up reusing parent staging),
    // restore the original settings first so we inject on top of the real file, not a
    // stale previously-injected copy that would overwrite the backup with injected content.
    if backup_path.exists() {
        let saved_original = std::fs::read_to_string(&backup_path)?;
        if saved_original == NO_ORIGINAL_SENTINEL {
            if settings_path.exists() {
                std::fs::remove_file(&settings_path)?;
            }
        } else {
            if let Some(parent) = settings_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&settings_path, &saved_original)?;
        }
    }

    // Read and save original content.
    let original_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)?
    } else {
        NO_ORIGINAL_SENTINEL.to_string()
    };

    // Save backup.
    let backup_dir = staging_path.join(".ta");
    std::fs::create_dir_all(&backup_dir)?;
    std::fs::write(&backup_path, &original_content)?;

    // Build allow and deny lists.
    let allow: Vec<String> = DEFAULT_ALLOWED_TOOLS
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect();
    let forbidden = load_forbidden_tools(source_dir);
    let deny: Vec<String> = forbidden.iter().map(|s| format!("\"{}\"", s)).collect();

    let settings_json = format!(
        r#"{{
  "_comment": "Injected by Trusted Autonomy. Agent works in a staging sandbox — all changes require human review before applying. See .ta-forbidden-tools to deny specific patterns.",
  "permissions": {{
    "allow": [
      {}
    ],
    "deny": [
      {}
    ]
  }}
}}"#,
        allow.join(",\n      "),
        deny.join(",\n      ")
    );

    // Ensure .claude/ directory exists.
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&settings_path, settings_json)?;
    Ok(())
}

/// Restore original .claude/settings.local.json before diffing.
fn restore_claude_settings(staging_path: &Path) -> anyhow::Result<()> {
    let backup_path = staging_path.join(SETTINGS_BACKUP);
    let settings_path = staging_path.join(SETTINGS_REL_PATH);

    if !backup_path.exists() {
        return Ok(());
    }

    let original = std::fs::read_to_string(&backup_path)?;

    if original == NO_ORIGINAL_SENTINEL {
        // Settings file didn't exist originally — remove it and the .claude/ dir if empty.
        if settings_path.exists() {
            std::fs::remove_file(&settings_path)?;
        }
        if let Some(parent) = settings_path.parent() {
            // Only remove .claude/ if it's empty (don't delete user's other configs).
            let _ = std::fs::remove_dir(parent);
        }
    } else {
        std::fs::write(&settings_path, original)?;
    }

    Ok(())
}

// ── .mcp.json injection for macro goals (#60) ──────────────────

pub(crate) const MCP_JSON_PATH: &str = ".mcp.json";
pub(crate) const MCP_JSON_BACKUP: &str = ".ta/mcp_json_original";

/// Inject TA MCP server config into a directory's `.mcp.json`.
///
/// This allows agents to call TA's MCP tools (ta_plan, ta_goal,
/// ta_draft, ta_context). Without this, the agent sees tool documentation
/// in CLAUDE.md but has no MCP server configured to handle the calls.
///
/// Used by both `ta run --macro` (staging workspace) and `ta dev` (project root).
pub(crate) fn inject_mcp_server_config(staging_path: &Path) -> anyhow::Result<()> {
    let mcp_json_path = staging_path.join(MCP_JSON_PATH);
    let backup_path = staging_path.join(MCP_JSON_BACKUP);

    // §4.2 constitution fix: if a backup already exists (follow-up reusing parent staging),
    // restore the original .mcp.json first so we inject on top of the real file, not a
    // stale previously-injected copy.
    if backup_path.exists() {
        let saved_original = std::fs::read_to_string(&backup_path)?;
        if saved_original == NO_ORIGINAL_SENTINEL {
            if mcp_json_path.exists() {
                std::fs::remove_file(&mcp_json_path)?;
            }
        } else {
            std::fs::write(&mcp_json_path, &saved_original)?;
        }
    }

    // Save original content (or sentinel if file doesn't exist).
    let original_content = if mcp_json_path.exists() {
        std::fs::read_to_string(&mcp_json_path)?
    } else {
        NO_ORIGINAL_SENTINEL.to_string()
    };

    let backup_dir = staging_path.join(".ta");
    std::fs::create_dir_all(&backup_dir)?;
    std::fs::write(&backup_path, &original_content)?;

    // Build the MCP config with TA server entry.
    // Resolve the `ta` binary path for the server command.
    let ta_binary = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "ta".to_string());

    let ta_server_entry = serde_json::json!({
        "command": ta_binary,
        "args": ["serve"],
        "env": {
            "TA_PROJECT_ROOT": staging_path.display().to_string(),
            "TA_IS_STAGING": "1"
        }
    });

    // Merge with existing .mcp.json if present.
    let mut mcp_config: serde_json::Value = if original_content != NO_ORIGINAL_SENTINEL {
        serde_json::from_str(&original_content)
            .unwrap_or_else(|_| serde_json::json!({ "mcpServers": {} }))
    } else {
        serde_json::json!({ "mcpServers": {} })
    };

    // Add or update the "trusted-autonomy" server entry.
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

/// Restore the original `.mcp.json` after agent exits.
///
/// Used by both `ta run --macro` (before diff) and `ta dev` (cleanup).
pub(crate) fn restore_mcp_server_config(staging_path: &Path) -> anyhow::Result<()> {
    let mcp_json_path = staging_path.join(MCP_JSON_PATH);
    let backup_path = staging_path.join(MCP_JSON_BACKUP);

    if backup_path.exists() {
        let original = std::fs::read_to_string(&backup_path)?;
        if original == NO_ORIGINAL_SENTINEL {
            if mcp_json_path.exists() {
                std::fs::remove_file(&mcp_json_path)?;
            }
        } else {
            std::fs::write(&mcp_json_path, original)?;
        }
        std::fs::remove_file(&backup_path)?;
    } else {
        // No TA-server backup, but ta-memory / ta-community-hub may still have been injected.
        // Remove injected keys if present so they don't pollute the source.
        if mcp_json_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&mcp_json_path) {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&content) {
                    let removed = val
                        .get_mut("mcpServers")
                        .and_then(|s| s.as_object_mut())
                        .map(|s| {
                            let r1 = s.remove("ta-memory").is_some();
                            let r2 = s.remove("ta-community-hub").is_some();
                            r1 || r2
                        })
                        .unwrap_or(false);
                    if removed {
                        if let Ok(cleaned) = serde_json::to_string_pretty(&val) {
                            let _ = std::fs::write(&mcp_json_path, cleaned);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// ── CLAUDE.md injection and restoration ─────────────────────────

const CLAUDE_MD_BACKUP: &str = ".ta/claude_md_original";
pub(crate) const NO_ORIGINAL_SENTINEL: &str = "__TA_NO_ORIGINAL__";

/// Build a plan context section for CLAUDE.md injection.
/// Returns empty string if no PLAN.md or no phase specified.
/// Uses windowed checklist (v0.14.3.1) to keep plan section compact.
fn build_plan_section(
    plan_phase: Option<&str>,
    source_dir: Option<&Path>,
    done_window: usize,
    pending_window: usize,
) -> String {
    let source = match source_dir {
        Some(s) => s,
        None => return String::new(),
    };

    let phases = match plan::load_plan(source) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };

    if phases.is_empty() {
        return String::new();
    }

    let checklist =
        plan::format_plan_checklist_windowed(&phases, plan_phase, done_window, pending_window);

    let current_line = if let Some(phase_id) = plan_phase {
        if let Some(phase) = phases
            .iter()
            .find(|p| plan::phase_ids_match(&p.id, phase_id))
        {
            format!(
                "\n**You are working on Phase {} — {}.**\n",
                phase.id, phase.title
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    format!(
        "\n## Plan Context\n{}\nPlan progress:\n{}\n",
        current_line, checklist
    )
}

/// Named section sizes for context budget accounting (v0.14.3.1).
#[derive(Debug)]
pub(crate) struct ContextSectionSizes {
    pub header: usize,
    pub plan: usize,
    pub memory: usize,
    pub solutions: usize,
    pub community: usize,
    pub parent: usize,
    pub original_claude_md: usize,
}

impl ContextSectionSizes {
    pub fn total(&self) -> usize {
        self.header
            + self.plan
            + self.memory
            + self.solutions
            + self.community
            + self.parent
            + self.original_claude_md
    }
}

/// Compute section sizes for context budget diagnostics without writing any files.
pub(crate) fn compute_context_section_sizes(
    title: &str,
    goal_id: &str,
    plan_phase: Option<&str>,
    source_dir: Option<&Path>,
    config: &ta_mcp_gateway::GatewayConfig,
    done_window: usize,
    pending_window: usize,
) -> ContextSectionSizes {
    use super::community;

    let plan_section = build_plan_section(plan_phase, source_dir, done_window, pending_window);
    let memory_section = build_memory_context_section_for_inject(config, title, plan_phase);
    let solutions_section = build_solutions_section_for_inject(config);
    let community_section = if let Some(src) = source_dir {
        community::build_community_context_section(src)
    } else {
        String::new()
    };

    // Approximate the fixed header size by building a minimal version of it.
    // The header is the TA header block + instructions (does not include original CLAUDE.md).
    let header_approx = format!(
        "# Trusted Autonomy — Mediated Goal\n\nYou are working on a TA-mediated goal in a staging workspace.\n\n**Goal:** {}\n**Goal ID:** {}\n",
        title, goal_id
    );
    // Add the fixed instructions portion (constant regardless of phase).
    let instructions_approx = "\n## How this works\n\n- This directory is a copy of the original project\n- Work normally — Read, Write, Edit, Bash all work as expected\n- When you're done, just exit. TA will diff your changes and create a draft for review\n- The human reviewer will see exactly what you changed and why\n\n## Important\n\n- Do NOT modify files outside this directory\n- All your changes will be captured as a draft for human review\n\n## Before You Exit — Change Summary (REQUIRED)\n";

    let header = header_approx.len() + instructions_approx.len();

    // Read original CLAUDE.md from source_dir to measure it.
    let original_claude_md = source_dir
        .and_then(|src| {
            let path = src.join("CLAUDE.md");
            std::fs::read_to_string(path).ok()
        })
        .map(|s| s.len())
        .unwrap_or(0);

    ContextSectionSizes {
        header,
        plan: plan_section.len(),
        memory: memory_section.len(),
        solutions: solutions_section.len(),
        community: community_section.len(),
        parent: 0, // parent context is goal-specific; omit from static estimate
        original_claude_md,
    }
}

/// Build a parent goal context section for CLAUDE.md injection.
/// Returns empty string if no parent goal or if parent's PR is not available.
fn build_parent_context_section(
    parent_goal_id: Option<uuid::Uuid>,
    goal_store: &ta_goal::GoalRunStore,
    config: &GatewayConfig,
) -> String {
    let parent_id = match parent_goal_id {
        Some(id) => id,
        None => return String::new(),
    };

    let parent_goal = match goal_store.get(parent_id) {
        Ok(Some(g)) => g,
        _ => return String::new(),
    };

    let mut context = format!(
        "\n## Follow-Up Context\n\nThis is a follow-up goal building on:\n\
         **Parent Goal:** {} ({})\n\
         **Parent Objective:** {}\n",
        parent_goal.title, parent_id, parent_goal.objective
    );

    // If parent has a PR, include artifact dispositions and discuss items.
    if let Some(pr_id) = parent_goal.pr_package_id {
        use crate::commands::draft::load_package;
        if let Ok(parent_pr) = load_package(config, pr_id) {
            let approved = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Approved
                    )
                })
                .count();
            let rejected = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Rejected
                    )
                })
                .count();
            let discuss = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Discuss
                    )
                })
                .count();

            context.push_str(&format!(
                "\n**Parent PR Status:** {} ({} approved, {} rejected, {} discuss)\n",
                parent_pr.status, approved, rejected, discuss
            ));

            // List discuss items with their rationale.
            let discuss_items: Vec<_> = parent_pr
                .changes
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.disposition,
                        ta_changeset::draft_package::ArtifactDisposition::Discuss
                    )
                })
                .collect();

            if !discuss_items.is_empty() {
                context.push_str("\n### Items for Discussion:\n\n");
                context
                    .push_str("The following artifacts were marked for discussion during review. ");
                context.push_str(
                    "Please address the reviewer's concerns in this follow-up iteration.\n\n",
                );

                for artifact in discuss_items {
                    context.push_str(&format!("#### {}\n\n", artifact.resource_uri));

                    // Include rationale if available.
                    if let Some(ref why) = artifact.rationale {
                        context.push_str(&format!("**Agent's original rationale:** {}\n\n", why));
                    }

                    // Include explanation tiers if available (v0.2.3+).
                    if let Some(ref tiers) = artifact.explanation_tiers {
                        if !tiers.summary.is_empty() {
                            context
                                .push_str(&format!("**What was changed:** {}\n\n", tiers.summary));
                        }
                        if !tiers.explanation.is_empty() {
                            context.push_str(&format!("**Why:** {}\n\n", tiers.explanation));
                        }
                    }

                    // Include comment thread if available (v0.3.0 — the key missing piece!).
                    if let Some(ref comments) = artifact.comments {
                        if !comments.is_empty() {
                            context.push_str("**Review discussion:**\n\n");
                            for (idx, comment) in comments.comments.iter().enumerate() {
                                context.push_str(&format!(
                                    "{}. **{}** ({}): {}\n",
                                    idx + 1,
                                    comment.commenter,
                                    comment.created_at.format("%Y-%m-%d %H:%M UTC"),
                                    comment.text
                                ));
                            }
                            context.push('\n');
                        }
                    }

                    context.push_str("---\n\n");
                }

                context.push_str("**Your task:** Address each discussion item above. ");
                context.push_str("For each artifact, either revise it to address the concerns, ");
                context.push_str("provide clarification in your change_summary.json, or explain ");
                context.push_str("why the change is correct as-is.\n\n");
            }
        }
    }

    context
}

/// Build the macro goal context section for CLAUDE.md injection.
/// Explains the MCP tools available for inner-loop iteration.
fn build_macro_goal_section(goal_id: &str) -> String {
    format!(
        r#"
## Macro Goal Mode (Inner-Loop Iteration)

This is a **macro goal** session. You can decompose your work into sub-goals,
submit drafts for human review mid-session, and iterate based on feedback —
all without exiting.

### Available MCP Tools

Use these tools to interact with TA during your session:

- **`ta_draft`** — Manage draft packages
  - `action: "build"` — Bundle your current changes into a draft for review
  - `action: "submit"` — Submit a draft for human review (blocks until response)
  - `action: "status"` — Check the review status of a draft
  - `action: "list"` — List all drafts for this goal

- **`ta_goal`** — Manage sub-goals
  - `action: "start"` — Create a sub-goal within this macro session
  - `action: "status"` — Check the status of a sub-goal

- **`ta_plan`** — Interact with the project plan
  - `action: "read"` — Read current plan progress
  - `action: "update"` — Propose plan updates (held for human approval)

### Workflow

1. Work on a logical unit of change
2. Call `ta_draft` with `action: "build"` to package your changes
3. Call `ta_draft` with `action: "submit"` to send for human review
4. Wait for approval or feedback
5. If approved, continue to the next sub-goal
6. If denied, revise and resubmit

### Security Boundaries

- You **CAN**: propose sub-goals, build drafts, submit for review, read plan status
- You **CANNOT**: approve your own drafts, apply changes, bypass checkpoints

**Macro Goal ID:** {}
"#,
        goal_id
    )
}

/// Inject a CLAUDE.md file into the staging workspace to orient the agent.
/// Saves the original content to `.ta/claude_md_original` for later restoration.
#[allow(clippy::too_many_arguments)]
/// Build the interactive mode section for CLAUDE.md injection (v0.9.9.2).
fn build_interactive_section() -> String {
    r#"
## Interactive Mode

This goal is running in **interactive mode**. You have access to the `ta_ask_human`
MCP tool, which lets you ask the human operator questions and wait for their response.

### When to use `ta_ask_human`

- **Clarification**: When the goal is ambiguous and you need human guidance
- **Decision points**: When multiple valid approaches exist and the human should choose
- **Confirmation**: Before making large or risky changes
- **Missing information**: When you need project-specific knowledge you don't have

### How it works

Call the `ta_ask_human` tool with:
- `question` (required): Your question text
- `context` (optional): What you've done so far, to help the human understand
- `response_hint`: `"freeform"` (default), `"yes_no"`, or `"choice"`
- `choices` (optional): List of options when using `"choice"` hint
- `timeout_secs` (optional): How long to wait (default: 600s)

The human sees your question in their terminal (or other channel) and types a response.
Your execution pauses until they respond or the timeout expires.

### Guidelines

- Ask focused, specific questions — avoid vague "what should I do?"
- Provide context so the human can answer without re-reading your work
- Don't ask about things you can figure out from the codebase
- If a question times out, proceed with your best judgment
"#
    .to_string()
}

#[allow(clippy::too_many_arguments)]
fn inject_claude_md(
    staging_path: &Path,
    title: &str,
    goal_id: &str,
    plan_phase: Option<&str>,
    source_dir: Option<&Path>,
    parent_goal_id: Option<uuid::Uuid>,
    goal_store: &ta_goal::GoalRunStore,
    config: &GatewayConfig,
    macro_goal: bool,
    interactive: bool,
    smart_follow_up_context: Option<&str>,
    context_budget_chars: usize,
    done_window: usize,
    pending_window: usize,
    context_mode: &ta_submit::config::ContextMode,
) -> anyhow::Result<()> {
    let claude_md_path = staging_path.join("CLAUDE.md");
    let backup_path = staging_path.join(CLAUDE_MD_BACKUP);

    // If a backup already exists from a previous injection (e.g., follow-up reusing
    // parent staging), restore the original CLAUDE.md first so we inject fresh
    // content on top of the real project file, not a stale injected copy.
    if backup_path.exists() {
        let saved_original = std::fs::read_to_string(&backup_path)?;
        if saved_original == NO_ORIGINAL_SENTINEL {
            if claude_md_path.exists() {
                std::fs::remove_file(&claude_md_path)?;
            }
        } else {
            std::fs::write(&claude_md_path, &saved_original)?;
        }
    }

    // Read and save original content.
    let original_content = if claude_md_path.exists() {
        std::fs::read_to_string(&claude_md_path)?
    } else {
        NO_ORIGINAL_SENTINEL.to_string()
    };

    // Save backup to .ta/ in staging (excluded from copy and diff).
    let backup_dir = staging_path.join(".ta");
    std::fs::create_dir_all(&backup_dir)?;
    std::fs::write(&backup_path, &original_content)?;

    // Build injected content.
    let existing_section = if original_content == NO_ORIGINAL_SENTINEL {
        String::new()
    } else {
        original_content
    };

    // Build plan context section if PLAN.md exists in source (windowed, v0.14.3.1).
    // v0.14.3.2: Skip plan injection when context_mode is "mcp" or "hybrid".
    let use_inject_mode = *context_mode == ta_submit::config::ContextMode::Inject;
    let plan_section = if use_inject_mode {
        build_plan_section(plan_phase, source_dir, done_window, pending_window)
    } else {
        String::new()
    };

    // Build parent context section if this is a follow-up goal.
    // v0.10.9: Prefer smart follow-up context when available (richer context
    // including verification failures, denial reasons, and reviewer feedback).
    let mut parent_section = if let Some(ctx) = smart_follow_up_context {
        ctx.to_string()
    } else {
        build_parent_context_section(parent_goal_id, goal_store, config)
    };

    // Build macro goal section if --macro was specified.
    let macro_section = if macro_goal {
        build_macro_goal_section(goal_id)
    } else {
        String::new()
    };

    // Build interactive mode section if --interactive was specified (v0.9.9.2).
    let interactive_section = if interactive {
        build_interactive_section()
    } else {
        String::new()
    };

    // Build memory context section from prior sessions (v0.6.3: phase-aware).
    let mut memory_section = build_memory_context_section_for_inject(config, title, plan_phase);

    // Build solutions section from curated knowledge base (v0.8.1).
    let mut solutions_section = build_solutions_section_for_inject(config);

    // Build community knowledge section (v0.13.6: auto_query resources).
    // v0.14.3.2: Skip community injection when context_mode is "mcp" or "hybrid".
    let community_section = if use_inject_mode {
        if let Some(src) = source_dir {
            super::community::build_community_context_section(src)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // v0.14.3.2: For hybrid/mcp modes, add a one-line hint about available context tools.
    let context_tools_hint = if !use_inject_mode {
        "\n# Context tools: ta_plan_status, community_search, community_get — call these when you need plan or community context.\n".to_string()
    } else {
        String::new()
    };

    // v0.14.3.1: Enforce context budget. Trim in priority order when over limit.
    if context_budget_chars > 0 {
        let fixed_size = existing_section.len()
            + plan_section.len()
            + community_section.len()
            + macro_section.len()
            + interactive_section.len();
        let mut variable_total =
            solutions_section.len() + parent_section.len() + memory_section.len();
        let total = fixed_size + variable_total;

        if total > context_budget_chars {
            let mut trims: Vec<String> = Vec::new();

            // 1. Trim solutions first (lowest priority).
            if fixed_size + variable_total > context_budget_chars && !solutions_section.is_empty() {
                let old_len = solutions_section.len();
                solutions_section = trim_solutions_section(&solutions_section, 5);
                let saved = old_len - solutions_section.len();
                if saved > 0 {
                    variable_total -= saved;
                    trims.push(format!("solutions trimmed ({} chars)", saved));
                }
            }

            // 2. Truncate parent/follow-up context to 2k.
            const PARENT_TRUNCATE: usize = 2_000;
            if fixed_size + variable_total > context_budget_chars
                && parent_section.len() > PARENT_TRUNCATE
            {
                let old_len = parent_section.len();
                parent_section.truncate(PARENT_TRUNCATE);
                parent_section.push_str("\n... [truncated for context budget]\n");
                let saved = old_len - parent_section.len();
                variable_total -= saved;
                trims.push(format!(
                    "parent context truncated to {}k",
                    PARENT_TRUNCATE / 1000
                ));
            }

            // 3. Trim memory entries.
            if fixed_size + variable_total > context_budget_chars && !memory_section.is_empty() {
                let old_len = memory_section.len();
                memory_section = build_memory_context_section_for_inject_with_limit(
                    config, title, plan_phase, 5,
                );
                let saved = old_len.saturating_sub(memory_section.len());
                if saved > 0 {
                    variable_total -= saved;
                    trims.push(format!("memory entries reduced ({} chars)", saved));
                }
            }

            if !trims.is_empty() {
                let new_total = fixed_size + variable_total;
                tracing::warn!(
                    budget = context_budget_chars,
                    actual = new_total,
                    trimmed = trims.join(", "),
                    "CLAUDE.md context budget exceeded; sections trimmed"
                );
            }
        }
    }

    let injected = format!(
        r#"# Trusted Autonomy — Mediated Goal

You are working on a TA-mediated goal in a staging workspace.

**Goal:** {}
**Goal ID:** {}
{}{}{}{}{}{}{}{}
## How this works

- This directory is a copy of the original project
- Work normally — Read, Write, Edit, Bash all work as expected
- When you're done, just exit. TA will diff your changes and create a draft for review
- The human reviewer will see exactly what you changed and why

## Important

- Do NOT modify files outside this directory
- All your changes will be captured as a draft for human review

## Agent Progress Journal (strongly encouraged)

Write checkpoints to `.ta/ta-progress.json` as you complete significant steps. This survives process crashes and lets TA's recovery tools know how far you got. Write each checkpoint **immediately after** completing a verification step.

```json
{{ "goal_id": "GOAL_ID_PLACEHOLDER", "checkpoints": [
    {{ "label": "compiled", "at": "<ISO timestamp>", "detail": "cargo build passed" }},
    {{ "label": "tests_pass", "at": "<ISO timestamp>", "detail": "847 tests passed" }},
    {{ "label": "work_complete", "at": "<ISO timestamp>", "detail": "all items implemented" }}
]}}
```

Use labels like: `compiled`, `tests_pass`, `linted`, `work_complete`. The file is TA-internal and excluded from the diff shown to reviewers.

## Agent Decision Log (optional but encouraged)

If you make significant design decisions during this session, record them in `.ta-decisions.json` so the reviewer can see your reasoning. This is separate from the change summary — it captures *why you chose one approach over another*.

```json
[
  {{
    "decision": "One-line description of the choice made",
    "rationale": "Why this approach was chosen",
    "alternatives": ["other option A", "other option B"],
    "confidence": 0.9
  }}
]
```

This file is optional. If present, TA will surface these decisions in `ta draft view` under "Agent Decision Log".

## Before You Exit — Change Summary (REQUIRED)

You MUST create `.ta/change_summary.json` before exiting. The human reviewer relies on this to understand your work. Every changed file needs a clear "what I did" and "why" — reviewers who don't understand a change will reject it.

```json
{{
  "summary": "Brief description of all changes made in this session",
  "changes": [
    {{
      "path": "relative/path/to/file",
      "action": "modified|created|deleted",
      "what": "Specific description of what was changed in this target",
      "why": "Why this change was needed (motivation, not just restating what)",
      "independent": true,
      "depends_on": [],
      "depended_by": []
    }}
  ],
  "dependency_notes": "Human-readable explanation of which changes are coupled and why"
}}
```

Rules for per-target descriptions:
- **`what`** (REQUIRED): Describe specifically what you changed. NOT "updated file" — instead "Added JWT validation middleware with RS256 signature verification" or "Removed deprecated session-cookie auth fallback". The reviewer sees this as the primary description for each changed file.
- **`why`**: The motivation, not a restatement of what. "Security audit flagged session cookies as vulnerable" not "To add JWT validation".
- For lockfiles, config files, and generated files: still provide `what` (e.g., "Added jsonwebtoken v9.3 dependency") — don't leave them blank.
- `independent`: true if this change can be applied or reverted without affecting other changes
- `depends_on`: list of other file paths this change requires (e.g., if you add a function call, it depends on the file where the function is defined)
- `depended_by`: list of other file paths that would break if this change is reverted
- Be honest about dependencies — the reviewer uses this to decide which changes to accept individually

## Plan Updates (REQUIRED if PLAN.md exists)

As you complete planned work items, update PLAN.md to reflect progress:
- Move completed items from "Remaining" to "Completed" with a ✅ checkmark
- Update test counts when you add or remove tests
- Do NOT change the `<!-- status: ... -->` marker — only `ta draft apply` transitions phase status
- If you complete all remaining items in a phase, note that in your change_summary.json

## Documentation Updates

If your changes affect user-facing behavior (new commands, changed flags, new config options, workflow changes):
- Update `docs/USAGE.md` with the new/changed functionality
- Keep the tone consumer-friendly (no internal implementation details)
- Update version references if they exist in the docs
- Update the `CLAUDE.md` "Current State" section if the test count changes

---

{}
"#,
        title,
        goal_id,
        plan_section,
        parent_section,
        macro_section,
        interactive_section,
        memory_section,
        solutions_section,
        community_section,
        context_tools_hint,
        existing_section
    );

    // Replace placeholder in progress journal section with the actual goal ID.
    let injected = injected.replace("GOAL_ID_PLACEHOLDER", goal_id);

    std::fs::write(&claude_md_path, injected)?;
    Ok(())
}

/// Write a generic context file for non-Claude agents (v0.12.5).
///
/// Agents that set `context_file` in their YAML get the same memory/plan
/// context that Claude Code sees in CLAUDE.md, written to a generic markdown
/// file at the given path (relative to staging_path).
fn inject_agent_context_file(
    staging_path: &Path,
    title: &str,
    goal_id: &str,
    plan_phase: Option<&str>,
    config: &GatewayConfig,
    context_file: &str,
    source_dir: Option<&Path>,
) -> anyhow::Result<()> {
    let memory_section = build_memory_context_section_for_inject(config, title, plan_phase);

    let plan_content = {
        let plan_path = staging_path.join("PLAN.md");
        if plan_path.exists() {
            std::fs::read_to_string(&plan_path).ok()
        } else {
            None
        }
    };
    let plan_section = plan_content
        .as_deref()
        .map(|p| format!("\n## Plan\n\n{}\n", truncate_str(p, 8_000)))
        .unwrap_or_default();

    // Build community knowledge section (v0.13.17).
    let community_section = if let Some(src) = source_dir {
        super::community::build_community_context_section(src)
    } else {
        String::new()
    };

    let content = format!(
        "# TA Agent Context\n\n**Goal:** {}\n**Goal ID:** {}{}{}{}\n",
        title, goal_id, plan_section, memory_section, community_section,
    );

    let target = if std::path::Path::new(context_file).is_absolute() {
        std::path::PathBuf::from(context_file)
    } else {
        staging_path.join(context_file)
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, content)?;
    tracing::debug!(path = %target.display(), "wrote agent context file");
    Ok(())
}

/// Truncate a string to max_bytes without splitting UTF-8 characters.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        s
    } else {
        // Find the last valid char boundary at or before max_bytes.
        let mut end = max_bytes;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Auto-capture goal completion into memory (v0.5.6 / v0.12.5: RuVectorStore primary).
///
/// Reads `.ta/change_summary.json` from the staging workspace and stores
/// the goal completion event in the memory store for future context injection.
/// Uses RuVectorStore as primary backend (with FsMemoryStore as fallback).
fn auto_capture_goal_completion(
    config: &GatewayConfig,
    goal: &ta_goal::GoalRun,
    staging_path: &Path,
) {
    let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
    let capture_config = ta_memory::auto_capture::load_config(&workflow_toml);
    let capture = ta_memory::AutoCapture::new(capture_config);

    // Try to read change_summary.json from staging.
    let summary_path = staging_path.join(".ta").join("change_summary.json");
    let change_summary = if summary_path.exists() {
        std::fs::read_to_string(&summary_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    } else {
        None
    };

    // Extract changed file list from change_summary if available.
    let changed_files = change_summary
        .as_ref()
        .and_then(|v: &serde_json::Value| v.get("changes"))
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("path").and_then(|p| p.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let event = ta_memory::GoalCompleteEvent {
        goal_id: goal.goal_run_id,
        title: goal.title.clone(),
        agent_framework: goal.agent_id.clone(),
        change_summary,
        changed_files,
        phase_id: goal.plan_phase.clone(),
    };

    // v0.12.5: Write to RuVectorStore (primary). Migrate FsMemoryStore on first open.
    #[cfg(feature = "ruvector")]
    {
        let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
        if memory_config.backend.as_deref() != Some("fs") {
            let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
            match ta_memory::RuVectorStore::open(&rvf_path) {
                Ok(mut rv_store) => {
                    // Auto-migrate from FsMemoryStore on first open.
                    let fs_dir = config.workspace_root.join(".ta").join("memory");
                    if fs_dir.exists() {
                        if let Err(e) = rv_store.migrate_from_fs(&fs_dir) {
                            tracing::warn!("memory migration error: {}", e);
                        }
                    }
                    if let Err(e) = capture.on_goal_complete(&mut rv_store, &event) {
                        tracing::warn!("failed to auto-capture goal completion (ruvector): {}", e);
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!("could not open ruvector store, falling back to fs: {}", e);
                }
            }
        }
    }

    // Fallback: FsMemoryStore.
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut store = ta_memory::FsMemoryStore::new(&memory_dir);
    if let Err(e) = capture.on_goal_complete(&mut store, &event) {
        tracing::warn!("failed to auto-capture goal completion: {}", e);
    }

    // Ingest community feedback written by the agent (v0.13.17).
    // If the agent wrote .ta/community_feedback.json, parse and store each entry
    // in the local community cache with source: "agent-observed".
    let feedback_path = staging_path.join(".ta").join("community_feedback.json");
    if feedback_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&feedback_path) {
            if let Ok(serde_json::Value::Array(arr)) =
                serde_json::from_str::<serde_json::Value>(&content)
            {
                let cache_dir = config.workspace_root.join(".ta").join("community-cache");
                if std::fs::create_dir_all(&cache_dir).is_ok() {
                    let ts = chrono::Utc::now().to_rfc3339();
                    let mut ingested = 0usize;
                    for entry in &arr {
                        let mut e = entry.clone();
                        if let Some(obj) = e.as_object_mut() {
                            obj.insert("source".to_string(), serde_json::json!("agent-observed"));
                            obj.insert("observed_at".to_string(), serde_json::json!(ts));
                        }
                        let id = uuid::Uuid::new_v4();
                        let out_path = cache_dir.join(format!("feedback-{}.json", id));
                        let _ = std::fs::write(
                            &out_path,
                            serde_json::to_string_pretty(&e).unwrap_or_default(),
                        );
                        ingested += 1;
                    }
                    if ingested > 0 {
                        println!("Community feedback: {} observation(s) ingested.", ingested);
                        tracing::info!(count = ingested, "Ingested agent community feedback");
                    }
                }
            }
        }
    }
}

/// Build a memory context section from prior sessions for CLAUDE.md injection (v0.12.5).
///
/// Phase-aware: filters entries by the current plan phase. Respects the
/// `backend` field in `.ta/memory.toml`: "ruvector" (default) uses semantic
/// search; "fs" forces filesystem-only mode.
///
/// On every call, indexes the project constitution (`.ta/constitution.md`) into
/// the active store so constitution rules appear in context injection.
pub fn build_memory_context_section_for_inject(
    config: &GatewayConfig,
    goal_title: &str,
    phase_id: Option<&str>,
) -> String {
    let workflow_toml = config.workspace_root.join(".ta").join("workflow.toml");
    let capture_config = ta_memory::auto_capture::load_config(&workflow_toml);
    let max_entries = capture_config.max_context_entries;

    // Respect backend toggle from .ta/memory.toml.
    let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
    let backend = memory_config.backend.as_deref().unwrap_or("ruvector");

    // Load project constitution content for indexing (v0.12.5).
    let constitution_content = {
        let constitution_path = config.workspace_root.join(".ta").join("constitution.md");
        if constitution_path.exists() {
            std::fs::read_to_string(&constitution_path).ok()
        } else {
            None
        }
    };

    // v0.12.5: RuVectorStore is the primary backend. Always create/open it (creates
    // the directory if needed), then auto-migrate FsMemoryStore entries on first open.
    #[cfg(feature = "ruvector")]
    if backend != "fs" {
        let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
        match ta_memory::RuVectorStore::open(&rvf_path) {
            Ok(mut store) => {
                // Auto-migrate legacy FsMemoryStore entries on first open.
                let fs_dir = config.workspace_root.join(".ta").join("memory");
                if fs_dir.exists() {
                    if let Err(e) = store.migrate_from_fs(&fs_dir) {
                        tracing::warn!("memory migration error during context build: {}", e);
                    }
                }
                // Index constitution rules (v0.12.5).
                if let Some(ref content) = constitution_content {
                    if let Err(e) = ta_memory::index_constitution_rules(&mut store, content) {
                        tracing::warn!("failed to index constitution rules: {}", e);
                    }
                }
                return ta_memory::auto_capture::build_memory_context_section_with_phase(
                    &store,
                    goal_title,
                    max_entries,
                    phase_id,
                )
                .unwrap_or_default();
            }
            Err(e) => {
                tracing::warn!("could not open ruvector store for context injection: {}", e);
            }
        }
    }

    // Filesystem backend (explicit "fs" or ruvector unavailable).
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut fs_store = ta_memory::FsMemoryStore::new(&memory_dir);
    // Index constitution rules into fs store too (v0.12.5).
    if let Some(ref content) = constitution_content {
        if let Err(e) = ta_memory::index_constitution_rules(&mut fs_store, content) {
            tracing::warn!("failed to index constitution rules (fs): {}", e);
        }
    }
    ta_memory::auto_capture::build_memory_context_section_with_phase(
        &fs_store,
        goal_title,
        max_entries,
        phase_id,
    )
    .unwrap_or_default()
}

/// Like `build_memory_context_section_for_inject` but uses the manifest's `max_entries`
/// override instead of the workflow.toml global config (v0.13.16 memory relevance tuning).
fn build_memory_context_section_for_inject_with_limit(
    config: &GatewayConfig,
    goal_title: &str,
    phase_id: Option<&str>,
    max_entries: usize,
) -> String {
    let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
    let backend = memory_config.backend.as_deref().unwrap_or("ruvector");

    let constitution_content = {
        let p = config.workspace_root.join(".ta").join("constitution.md");
        if p.exists() {
            std::fs::read_to_string(&p).ok()
        } else {
            None
        }
    };

    #[cfg(feature = "ruvector")]
    if backend != "fs" {
        let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
        if let Ok(mut store) = ta_memory::RuVectorStore::open(&rvf_path) {
            let fs_dir = config.workspace_root.join(".ta").join("memory");
            if fs_dir.exists() {
                let _ = store.migrate_from_fs(&fs_dir);
            }
            if let Some(ref content) = constitution_content {
                let _ = ta_memory::index_constitution_rules(&mut store, content);
            }
            return ta_memory::auto_capture::build_memory_context_section_with_phase(
                &store,
                goal_title,
                max_entries,
                phase_id,
            )
            .unwrap_or_default();
        }
    }

    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut fs_store = ta_memory::FsMemoryStore::new(&memory_dir);
    if let Some(ref content) = constitution_content {
        let _ = ta_memory::index_constitution_rules(&mut fs_store, content);
    }
    ta_memory::auto_capture::build_memory_context_section_with_phase(
        &fs_store,
        goal_title,
        max_entries,
        phase_id,
    )
    .unwrap_or_default()
}

/// Build a filtered memory context section using manifest-level relevance tuning (v0.13.16).
///
/// Applies tag and recency filters on top of the standard memory lookup, respecting
/// the framework manifest's `[memory]` section config.
fn build_memory_context_section_filtered(
    config: &GatewayConfig,
    phase_id: Option<&str>,
    max_entries: usize,
    tags_filter: &[String],
    recency_days: u32,
) -> String {
    let memory_config = ta_memory::key_schema::load_memory_config(&config.workspace_root);
    let backend = memory_config.backend.as_deref().unwrap_or("ruvector");

    let constitution_content = {
        let p = config.workspace_root.join(".ta").join("constitution.md");
        if p.exists() {
            std::fs::read_to_string(&p).ok()
        } else {
            None
        }
    };

    #[cfg(feature = "ruvector")]
    if backend != "fs" {
        let rvf_path = config.workspace_root.join(".ta").join("memory.rvf");
        if let Ok(mut store) = ta_memory::RuVectorStore::open(&rvf_path) {
            let fs_dir = config.workspace_root.join(".ta").join("memory");
            if fs_dir.exists() {
                let _ = store.migrate_from_fs(&fs_dir);
            }
            if let Some(ref content) = constitution_content {
                let _ = ta_memory::index_constitution_rules(&mut store, content);
            }
            return ta_memory::build_memory_context_section_with_manifest_filter(
                &store,
                "",
                max_entries,
                phase_id,
                tags_filter,
                recency_days,
            )
            .unwrap_or_default();
        }
    }

    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut fs_store = ta_memory::FsMemoryStore::new(&memory_dir);
    if let Some(ref content) = constitution_content {
        let _ = ta_memory::index_constitution_rules(&mut fs_store, content);
    }
    ta_memory::build_memory_context_section_with_manifest_filter(
        &fs_store,
        "",
        max_entries,
        phase_id,
        tags_filter,
        recency_days,
    )
    .unwrap_or_default()
}

/// Build the solutions section for CLAUDE.md injection (v0.8.1).
///
/// Reads from `.ta/solutions/solutions.toml` and includes relevant entries
/// matched by project type.
/// Trim a solutions section string to keep only the first `max_solutions` entries.
///
/// Used by the context budget enforcer (v0.14.3.1) to reduce solutions bulk.
/// Returns the original string unchanged if it has ≤ `max_solutions` entries.
fn trim_solutions_section(section: &str, max_solutions: usize) -> String {
    // Each entry starts with "- **".
    let mut kept = 0usize;
    let mut result = String::new();
    let mut in_header = true;

    for line in section.lines() {
        if in_header {
            result.push_str(line);
            result.push('\n');
            if line.starts_with("- **") {
                in_header = false;
                kept += 1;
            }
        } else if line.starts_with("- **") {
            if kept >= max_solutions {
                break;
            }
            result.push_str(line);
            result.push('\n');
            kept += 1;
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

fn build_solutions_section_for_inject(config: &GatewayConfig) -> String {
    let solutions_path = config
        .workspace_root
        .join(".ta")
        .join("solutions")
        .join("solutions.toml");
    let store = ta_memory::SolutionStore::new(&solutions_path);

    let solutions = match store.load() {
        Ok(s) => s,
        Err(_) => return String::new(),
    };

    if solutions.is_empty() {
        return String::new();
    }

    // Filter by project type.
    let schema = ta_memory::KeySchema::resolve(&config.workspace_root);
    let language = match schema.project_type {
        ta_memory::ProjectType::RustWorkspace => Some("rust"),
        ta_memory::ProjectType::TypeScript => Some("typescript"),
        ta_memory::ProjectType::Python => Some("python"),
        ta_memory::ProjectType::Go => Some("go"),
        ta_memory::ProjectType::UnrealCpp => Some("cpp"),
        ta_memory::ProjectType::UnityCsharp => Some("csharp"),
        ta_memory::ProjectType::Generic => None,
    };

    let relevant: Vec<_> = solutions
        .iter()
        .filter(|s| {
            // Include if context language matches or is unset.
            match (&s.context.language, language) {
                (Some(sl), Some(pl)) => sl == pl,
                (None, _) => true,
                (_, None) => true,
            }
        })
        .take(15) // Limit to avoid overwhelming the context.
        .collect();

    if relevant.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n## Known Solutions\n\n");
    section
        .push_str("The following problem/solution pairs were captured from previous sessions:\n\n");
    for sol in &relevant {
        section.push_str(&format!("- **{}**: {}\n", sol.problem, sol.solution));
    }
    section.push('\n');
    section
}

/// Restore the original CLAUDE.md content before computing diffs.
/// This removes TA's injection so it doesn't appear in PR packages.
fn restore_claude_md(staging_path: &Path) -> anyhow::Result<()> {
    let backup_path = staging_path.join(CLAUDE_MD_BACKUP);
    let claude_md_path = staging_path.join("CLAUDE.md");

    if !backup_path.exists() {
        return Ok(()); // No backup — nothing to restore.
    }

    let original = std::fs::read_to_string(&backup_path)?;

    if original == NO_ORIGINAL_SENTINEL {
        // CLAUDE.md didn't exist originally — remove it.
        if claude_md_path.exists() {
            std::fs::remove_file(&claude_md_path)?;
        }
    } else {
        // Restore original content.
        std::fs::write(&claude_md_path, original)?;
    }

    Ok(())
}

/// Count files that differ between staging and source (v0.12.6 item 7).
///
/// Returns the number of files that exist in staging (outside `.ta/`) and either:
/// - Don't exist in the corresponding source path, or
/// - Have a different size from the source file.
///
/// This is an O(N) approximation (no content hashing) to keep logging fast.
fn count_changed_files(staging: &Path, source: &Path) -> usize {
    count_changed_recursive(staging, staging, source)
}

fn count_changed_recursive(staging_root: &Path, dir: &Path, source_root: &Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut count = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let rel = match path.strip_prefix(staging_root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // Skip .ta/ directory — it's TA metadata, not agent work.
        if rel
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == ".ta")
        {
            continue;
        }
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            count += count_changed_recursive(staging_root, &path, source_root);
        } else if ft.is_file() {
            let source_path = source_root.join(rel);
            let staging_size = entry.metadata().ok().map(|m| m.len());
            let source_size = source_path.metadata().ok().map(|m| m.len());
            match (staging_size, source_size) {
                (Some(s), Some(d)) if s != d => count += 1, // size changed
                (Some(_), None) => count += 1,              // new file
                _ => {}
            }
        }
    }
    count
}

// ── v0.13.8: Memory bridge helper functions ─────────────────────────────────

/// Build a plain goal context text string for env/arg-mode injection.
/// (Simpler than the full CLAUDE.md prepend — just goal ID, title, plan phase.)
fn build_goal_context_text(
    title: &str,
    goal_id: &str,
    plan_phase: Option<&str>,
    config: &GatewayConfig,
    source_dir: Option<&Path>,
) -> String {
    let phase_line = plan_phase
        .map(|p| format!("\n**Plan Phase:** {}", p))
        .unwrap_or_default();
    let memory_section = build_memory_context_section_for_inject(config, title, plan_phase);
    let community_section = if let Some(src) = source_dir {
        super::community::build_community_context_section(src)
    } else {
        String::new()
    };
    format!(
        "# TA Goal Context\n\n**Goal:** {}\n**Goal ID:** {}{}\n{}{}",
        title, goal_id, phase_line, memory_section, community_section
    )
}

/// Inject ta-memory as a local MCP server into `.mcp.json` (v0.13.8 item 11).
///
/// For MCP-mode memory frameworks (Claude Code, Codex, Claude-Flow), TA exposes
/// `ta-memory` as an MCP server so the agent can call memory_read/write/search natively.
/// This extends the existing inject_mcp_server_config logic.
fn inject_memory_mcp_server(staging_path: &Path) -> anyhow::Result<()> {
    let mcp_json_path = staging_path.join(MCP_JSON_PATH);

    let ta_binary = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "ta".to_string());

    let memory_server_entry = serde_json::json!({
        "command": ta_binary,
        "args": ["memory", "serve"],
        "env": {
            "TA_PROJECT_ROOT": staging_path.display().to_string()
        }
    });

    // Read or create .mcp.json
    let mut mcp_config: serde_json::Value = if mcp_json_path.exists() {
        let content = std::fs::read_to_string(&mcp_json_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({ "mcpServers": {} }))
    } else {
        serde_json::json!({ "mcpServers": {} })
    };

    let community_hub_entry = serde_json::json!({
        "command": ta_binary,
        "args": ["community", "serve"],
        "env": {
            "TA_PROJECT_ROOT": staging_path.display().to_string()
        }
    });

    if let Some(servers) = mcp_config
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
    {
        servers.insert("ta-memory".to_string(), memory_server_entry);
        servers.insert("ta-community-hub".to_string(), community_hub_entry);
    } else {
        mcp_config["mcpServers"] = serde_json::json!({
            "ta-memory": memory_server_entry,
            "ta-community-hub": community_hub_entry,
        });
    }

    if let Some(parent) = mcp_json_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&mcp_json_path, serde_json::to_string_pretty(&mcp_config)?)?;
    tracing::debug!("Injected ta-memory and ta-community-hub MCP servers into .mcp.json");
    Ok(())
}

/// Serialize memory entries into the agent's context file (v0.13.8 item 12).
///
/// For context-mode frameworks, appends relevant memory entries to the context
/// file as a markdown block so the agent sees prior context passively.
fn inject_memory_context(
    staging_path: &Path,
    context_file: &str,
    max_entries: usize,
    plan_phase: Option<&str>,
    tags_filter: &[String],
    recency_days: u32,
    config: &GatewayConfig,
) {
    // If no manifest-level filters are active, delegate to the standard builder
    // which reads max_entries from workflow.toml.  When filters ARE set, use the
    // filtered builder that respects the manifest's max_entries, tags, and recency.
    let memory_section = if !tags_filter.is_empty() || recency_days > 0 {
        build_memory_context_section_filtered(
            config,
            plan_phase,
            max_entries,
            tags_filter,
            recency_days,
        )
    } else {
        build_memory_context_section_for_inject_with_limit(config, "", plan_phase, max_entries)
    };
    if memory_section.is_empty() {
        return;
    }
    let target = if std::path::Path::new(context_file).is_absolute() {
        std::path::PathBuf::from(context_file)
    } else {
        staging_path.join(context_file)
    };
    if let Ok(existing) = std::fs::read_to_string(&target) {
        let updated = format!("{}\n{}", existing, memory_section);
        if let Err(e) = std::fs::write(&target, updated) {
            tracing::warn!(file = %target.display(), "Failed to append memory context: {}", e);
        }
    }
}

/// Ingest memory entries written by the agent to TA_MEMORY_OUT (v0.13.8 item 13).
///
/// After agent exit, if `.ta/memory_out.json` exists, parse it as a JSON array
/// of memory entries and store each in the project's ta-memory store.
fn ingest_memory_out(staging_path: &Path, config: &GatewayConfig) {
    let out_path = staging_path.join(".ta").join("memory_out.json");
    if !out_path.exists() {
        return;
    }
    let content = match std::fs::read_to_string(&out_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %out_path.display(), "Failed to read memory_out.json: {}", e);
            return;
        }
    };
    // Parse as array of {key, value, tags, category} objects.
    let entries: Vec<serde_json::Value> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(path = %out_path.display(), "Failed to parse memory_out.json: {}", e);
            return;
        }
    };
    if entries.is_empty() {
        return;
    }
    // Write entries to ta-memory using the FsMemoryStore.
    let memory_dir = config.workspace_root.join(".ta").join("memory");
    let mut store = ta_memory::FsMemoryStore::new(&memory_dir);
    let mut ingested = 0usize;
    for entry in &entries {
        let key = entry
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("agent-memory");
        let value = entry.get("value").cloned().unwrap_or_else(|| entry.clone());
        let tags: Vec<String> = entry
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        use ta_memory::MemoryStore as _;
        if let Err(e) = store.store(key, value, tags, "agent-exit-file") {
            tracing::warn!(key = key, "Failed to ingest memory entry: {}", e);
        } else {
            ingested += 1;
        }
    }
    tracing::info!(
        count = ingested,
        total = entries.len(),
        "Ingested agent exit-file memory entries"
    );
    // Clean up the output file.
    let _ = std::fs::remove_file(&out_path);
}

/// Append a progress journal entry to `.ta/goals/<id>/progress.jsonl` (v0.14.12).
///
/// The journal is append-only and survives process crashes. It provides
/// TA's recovery tools with a chronological record of key lifecycle events.
/// Each entry is a single JSON line with `label`, `at`, and `detail` fields.
fn append_progress_journal(
    goals_dir: &std::path::Path,
    goal_id: uuid::Uuid,
    label: &str,
    detail: &str,
) {
    let goal_dir = goals_dir.join(goal_id.to_string());
    // Ensure the goal directory exists (it should, but be defensive).
    if let Err(e) = std::fs::create_dir_all(&goal_dir) {
        tracing::warn!(
            goal_id = %goal_id,
            label = label,
            "append_progress_journal: cannot create goal dir: {}",
            e
        );
        return;
    }
    let journal_path = goal_dir.join("progress.jsonl");
    let at = chrono::Utc::now().to_rfc3339();
    let entry = format!(
        "{{\"label\":{},\"at\":{},\"detail\":{}}}\n",
        serde_json::json!(label),
        serde_json::json!(at),
        serde_json::json!(detail),
    );
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&journal_path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(entry.as_bytes()) {
                tracing::warn!(
                    goal_id = %goal_id,
                    label = label,
                    "append_progress_journal: write failed: {}",
                    e
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                goal_id = %goal_id,
                label = label,
                "append_progress_journal: open failed: {}",
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── v0.12.6 count_changed_files tests ───────────────────────

    #[test]
    fn count_changed_files_empty_staging() {
        let staging = TempDir::new().unwrap();
        let source = TempDir::new().unwrap();
        assert_eq!(count_changed_files(staging.path(), source.path()), 0);
    }

    #[test]
    fn count_changed_files_new_file_in_staging() {
        let staging = TempDir::new().unwrap();
        let source = TempDir::new().unwrap();
        // Write a file in staging that doesn't exist in source.
        std::fs::write(staging.path().join("new.rs"), "fn main() {}").unwrap();
        assert_eq!(count_changed_files(staging.path(), source.path()), 1);
    }

    #[test]
    fn count_changed_files_identical_file_not_counted() {
        let staging = TempDir::new().unwrap();
        let source = TempDir::new().unwrap();
        // Same content → same size → not counted.
        std::fs::write(staging.path().join("same.rs"), "fn foo() {}").unwrap();
        std::fs::write(source.path().join("same.rs"), "fn foo() {}").unwrap();
        assert_eq!(count_changed_files(staging.path(), source.path()), 0);
    }

    #[test]
    fn count_changed_files_modified_file_counted() {
        let staging = TempDir::new().unwrap();
        let source = TempDir::new().unwrap();
        // Different sizes → counted as changed.
        std::fs::write(staging.path().join("lib.rs"), "fn foo() { /* modified */ }").unwrap();
        std::fs::write(source.path().join("lib.rs"), "fn foo() {}").unwrap();
        assert_eq!(count_changed_files(staging.path(), source.path()), 1);
    }

    #[test]
    fn count_changed_files_ta_dir_excluded() {
        let staging = TempDir::new().unwrap();
        let source = TempDir::new().unwrap();
        // File inside .ta/ should NOT be counted.
        let ta_dir = staging.path().join(".ta");
        std::fs::create_dir_all(&ta_dir).unwrap();
        std::fs::write(ta_dir.join("state.json"), "{}").unwrap();
        assert_eq!(count_changed_files(staging.path(), source.path()), 0);
    }

    #[test]
    fn run_creates_goal_and_restores_on_no_launch() {
        let project = TempDir::new().unwrap();
        std::fs::write(project.path().join("README.md"), "# Test\n").unwrap();
        std::fs::write(
            project.path().join("CLAUDE.md"),
            "# Existing project instructions\n",
        )
        .unwrap();

        let config = GatewayConfig::for_project(project.path());

        // Run with --no-launch to avoid actually starting the agent.
        execute(
            &config,
            Some("Test goal"),
            "claude-code",
            Some(project.path()),
            "Test objective",
            None,
            None,
            None, // follow_up_draft
            None, // follow_up_goal
            None,
            true,
            false,
            false,
            None,
            false, // not headless
            false, // skip_verify = false
            false, // quiet = false
            None,  // no existing goal id
            None,  // workflow = default (single-agent)
            None,  // persona_name = None
        )
        .unwrap();

        // Verify goal was created.
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let goals = goal_store.list().unwrap();
        assert_eq!(goals.len(), 1);

        // With --no-launch, injected files should be restored so they
        // don't contaminate a subsequent `ta pr build` diff.
        let claude_md = std::fs::read_to_string(goals[0].workspace_path.join("CLAUDE.md")).unwrap();
        assert_eq!(claude_md, "# Existing project instructions\n");

        // Settings should also be restored (removed since it didn't exist).
        assert!(!goals[0].workspace_path.join(SETTINGS_REL_PATH).exists());
    }

    #[test]
    fn run_injects_context_for_agent() {
        // Verify that inject + restore roundtrip works for the agent path.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        std::fs::write(
            staging.path().join("CLAUDE.md"),
            "# Existing project instructions\n",
        )
        .unwrap();

        inject_claude_md(
            staging.path(),
            "Test goal",
            "goal-123",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        // Verify CLAUDE.md was injected.
        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Trusted Autonomy"));
        assert!(claude_md.contains("Test goal"));
        assert!(claude_md.contains("Existing project instructions"));

        // Verify backup was saved.
        let backup = std::fs::read_to_string(staging.path().join(CLAUDE_MD_BACKUP)).unwrap();
        assert_eq!(backup, "# Existing project instructions\n");

        inject_claude_settings(staging.path(), None).unwrap();

        // Verify settings.local.json was injected.
        let settings = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(settings.contains("Trusted Autonomy"));
        assert!(settings.contains("Bash(*)"));
    }

    #[test]
    fn inject_and_restore_claude_md_roundtrip() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        let original = "# My Project\nExisting instructions.\n";
        std::fs::write(staging.path().join("CLAUDE.md"), original).unwrap();

        inject_claude_md(
            staging.path(),
            "Fix bug",
            "goal-123",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        // Verify injection happened.
        let injected = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(injected.contains("Trusted Autonomy"));
        assert!(injected.contains("Fix bug"));
        assert!(injected.contains("Existing instructions"));

        // Restore.
        restore_claude_md(staging.path()).unwrap();

        // Verify original content is back.
        let restored = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn restore_removes_claude_md_if_not_originally_present() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();
        // No CLAUDE.md exists initially.

        inject_claude_md(
            staging.path(),
            "New goal",
            "goal-456",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        // CLAUDE.md was created by injection.
        assert!(staging.path().join("CLAUDE.md").exists());

        // Restore should remove it.
        restore_claude_md(staging.path()).unwrap();
        assert!(!staging.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn inject_claude_md_with_macro_goal_section() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        inject_claude_md(
            staging.path(),
            "Macro goal test",
            "goal-macro-789",
            None,
            None,
            None,
            &goal_store,
            &config,
            true, // macro_goal = true
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Macro Goal Mode"));
        assert!(claude_md.contains("ta_draft"));
        assert!(claude_md.contains("ta_goal"));
        assert!(claude_md.contains("ta_plan"));
        assert!(claude_md.contains("Inner-Loop"));
        assert!(claude_md.contains("goal-macro-789"));
    }

    #[test]
    fn inject_claude_md_with_interactive_section() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        inject_claude_md(
            staging.path(),
            "Interactive goal",
            "goal-interactive-101",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            true, // interactive = true
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(
            claude_md.contains("Interactive Mode"),
            "should contain interactive section"
        );
        assert!(
            claude_md.contains("ta_ask_human"),
            "should mention ta_ask_human tool"
        );
        assert!(
            claude_md.contains("response_hint"),
            "should document response hints"
        );
    }

    #[test]
    fn inject_claude_md_without_interactive_has_no_interactive_section() {
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        inject_claude_md(
            staging.path(),
            "Non-interactive goal",
            "goal-nointeractive-102",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(
            !claude_md.contains("Interactive Mode"),
            "should NOT contain interactive section"
        );
    }

    #[test]
    fn agent_config_returns_correct_launch_config() {
        // Pass None for source_dir — tests use built-in fallbacks (no YAML files).
        let claude = agent_launch_config("claude-code", None);
        assert_eq!(claude.command, "claude");
        assert!(claude.injects_context_file);
        assert!(claude.injects_settings);
        assert!(!claude
            .args_template
            .contains(&"--dangerously-skip-permissions".to_string()));

        let codex = agent_launch_config("codex", None);
        assert_eq!(codex.command, "codex");
        assert!(!codex.injects_context_file);
        assert!(!codex.injects_settings);

        let flow = agent_launch_config("claude-flow", None);
        assert_eq!(flow.command, "npx");
        assert!(flow.injects_context_file);
        assert!(flow.injects_settings);
        assert!(flow
            .args_template
            .contains(&"claude-flow@alpha".to_string()));
        assert!(flow.args_template.contains(&"hive-mind".to_string()));
        assert!(flow.args_template.contains(&"--claude".to_string()));
        let pre = flow.pre_launch.expect("claude-flow should have pre_launch");
        assert_eq!(pre.command, "npx");
        assert!(pre.args.contains(&"hive-mind".to_string()));
        assert!(pre.args.contains(&"init".to_string()));

        let unknown = agent_launch_config("my-custom-agent", None);
        assert_eq!(unknown.command, "my-custom-agent");
        assert!(unknown.args_template.is_empty());
        assert!(unknown.pre_launch.is_none());
        assert!(!unknown.injects_settings);
    }

    #[test]
    fn agent_config_loads_from_yaml() {
        let project = TempDir::new().unwrap();
        let agents_dir = project.path().join(".ta").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let yaml = r#"
name: test-agent
description: "A test agent"
command: my-test-cmd
args_template:
  - "--flag"
  - "{prompt}"
injects_context_file: true
injects_settings: false
env:
  MY_VAR: "hello"
"#;
        std::fs::write(agents_dir.join("test-agent.yaml"), yaml).unwrap();

        let config = agent_launch_config("test-agent", Some(project.path()));
        assert_eq!(config.command, "my-test-cmd");
        assert_eq!(config.args_template, vec!["--flag", "{prompt}"]);
        assert!(config.injects_context_file);
        assert!(!config.injects_settings);
        assert!(config.pre_launch.is_none());
        assert_eq!(config.env.get("MY_VAR").unwrap(), "hello");
    }

    #[test]
    fn agent_config_yaml_with_pre_launch() {
        let project = TempDir::new().unwrap();
        let agents_dir = project.path().join(".ta").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        let yaml = r#"
name: flow-test
command: npx
args_template: ["{prompt}"]
pre_launch:
  command: npx
  args: ["flow", "init"]
"#;
        std::fs::write(agents_dir.join("flow-test.yaml"), yaml).unwrap();

        let config = agent_launch_config("flow-test", Some(project.path()));
        assert_eq!(config.command, "npx");
        let pre = config.pre_launch.expect("should have pre_launch");
        assert_eq!(pre.command, "npx");
        assert_eq!(pre.args, vec!["flow", "init"]);
    }

    #[test]
    fn inject_and_restore_settings_roundtrip() {
        let staging = TempDir::new().unwrap();

        inject_claude_settings(staging.path(), None).unwrap();

        let settings_path = staging.path().join(SETTINGS_REL_PATH);
        assert!(settings_path.exists());
        let content = std::fs::read_to_string(&settings_path).unwrap();
        assert!(content.contains("Trusted Autonomy"));
        assert!(content.contains("Bash(*)"));
        assert!(content.contains("\"deny\": ["));

        restore_claude_settings(staging.path()).unwrap();
        assert!(!settings_path.exists());
    }

    #[test]
    fn inject_settings_preserves_existing() {
        let staging = TempDir::new().unwrap();
        let claude_dir = staging.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let original = r#"{"customSetting": true}"#;
        std::fs::write(claude_dir.join("settings.local.json"), original).unwrap();

        inject_claude_settings(staging.path(), None).unwrap();

        let injected = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(injected.contains("Trusted Autonomy"));

        restore_claude_settings(staging.path()).unwrap();
        let restored = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn forbidden_tools_loaded_from_file() {
        let project = TempDir::new().unwrap();
        std::fs::write(
            project.path().join(FORBIDDEN_TOOLS_FILE),
            "# Comment line\nBash(rm -rf /*)\n\nBash(curl * | sh)\n",
        )
        .unwrap();

        let forbidden = load_forbidden_tools(Some(project.path()));
        assert_eq!(forbidden.len(), 2);
        assert_eq!(forbidden[0], "Bash(rm -rf /*)");
        assert_eq!(forbidden[1], "Bash(curl * | sh)");
    }

    #[test]
    fn forbidden_tools_empty_when_no_file() {
        let project = TempDir::new().unwrap();
        let forbidden = load_forbidden_tools(Some(project.path()));
        assert!(forbidden.is_empty());
    }

    #[test]
    fn inject_settings_includes_forbidden_tools() {
        let staging = TempDir::new().unwrap();
        let source = TempDir::new().unwrap();
        std::fs::write(
            source.path().join(FORBIDDEN_TOOLS_FILE),
            "Bash(rm -rf /*)\n",
        )
        .unwrap();

        inject_claude_settings(staging.path(), Some(source.path())).unwrap();

        let content = std::fs::read_to_string(staging.path().join(SETTINGS_REL_PATH)).unwrap();
        assert!(content.contains("Bash(rm -rf /*)"));
    }

    #[test]
    fn parent_context_injects_comment_threads_for_discuss_items() {
        use chrono::Utc;
        use ta_changeset::draft_package::{
            AgentIdentity, Artifact, ArtifactDisposition, ChangeType, Changes, DraftPackage,
            DraftStatus, ExplanationTiers, Goal, Iteration, Plan, Provenance, ReviewRequests, Risk,
            Signatures, Summary, WorkspaceRef,
        };
        use ta_changeset::review_session::CommentThread;
        use ta_goal::GoalRun;
        use uuid::Uuid;

        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Create a parent goal with a draft package containing discuss items with comments.
        let parent_goal_id = Uuid::new_v4();
        let parent_pr_id = Uuid::new_v4();

        let mut parent_goal = GoalRun::new(
            "Fix auth bug",
            "Fix the authentication issue",
            "test-agent",
            project.path().join(".ta/staging/parent"),
            project.path().join(".ta/store/parent"),
        );
        parent_goal.goal_run_id = parent_goal_id; // Override the UUID for testing
        parent_goal.pr_package_id = Some(parent_pr_id);
        parent_goal.source_dir = Some(project.path().to_path_buf());
        goal_store.save(&parent_goal).unwrap();

        // Create a draft package with discuss items that have comment threads.
        let mut comment_thread = CommentThread::new();
        comment_thread.add("reviewer-1", "This needs error handling for null tokens");
        comment_thread.add("agent-1", "Understood, I'll add validation");
        comment_thread.add("reviewer-1", "Also consider adding tests for edge cases");

        let artifact_with_comments = Artifact {
            resource_uri: "fs://workspace/src/auth/middleware.rs".to_string(),
            change_type: ChangeType::Modify,
            diff_ref: "changeset:0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Discuss,
            rationale: Some("Refactored to use JWT".to_string()),
            dependencies: vec![],
            explanation_tiers: Some(ExplanationTiers {
                summary: "Switched auth from sessions to JWT tokens".to_string(),
                explanation: "Implemented RS256 signature verification".to_string(),
                tags: vec!["security".to_string()],
                related_artifacts: vec![],
            }),
            comments: Some(comment_thread),
            amendment: None,
            kind: None,
        };

        let parent_draft = DraftPackage {
            package_version: "1.0.0".to_string(),
            package_id: parent_pr_id,
            created_at: Utc::now(),
            goal: Goal {
                goal_id: parent_goal_id.to_string(),
                title: "Fix auth bug".to_string(),
                objective: "Fix authentication".to_string(),
                success_criteria: vec![],
                constraints: vec![],
                parent_goal_title: None,
            },
            iteration: Iteration {
                iteration_id: "iter-1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging".to_string(),
                    ref_name: "staging/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "agent-1".to_string(),
                agent_type: "coder".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "hash-123".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Auth refactor".to_string(),
                why: "Modernize auth".to_string(),
                impact: "1 file changed".to_string(),
                rollback_plan: "Revert commit".to_string(),
                open_questions: vec![],
                alternatives_considered: vec![],
            },
            plan: Plan {
                completed_steps: vec![],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![artifact_with_comments],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 0,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "trace-123".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![],
                reviewers: vec![],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "hash-456".to_string(),
                agent_signature: "sig-789".to_string(),
                gateway_attestation: None,
            },
            status: DraftStatus::PendingReview,
            verification_warnings: vec![],
            validation_log: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
            parent_draft_id: None,
            pending_approvals: vec![],
            supervisor_review: None,
            ignored_artifacts: vec![],
            baseline_artifacts: vec![],
            agent_decision_log: vec![],
            goal_shortref: None,
            draft_seq: 0,
        };

        // Save the draft package.
        super::super::draft::save_package(&config, &parent_draft).unwrap();

        // Build parent context section.
        let context = build_parent_context_section(Some(parent_goal_id), &goal_store, &config);

        // Verify the context includes follow-up information.
        assert!(context.contains("Follow-Up Context"));
        assert!(context.contains("Fix auth bug"));

        // Verify discuss items are listed.
        assert!(context.contains("Items for Discussion"));
        assert!(context.contains("fs://workspace/src/auth/middleware.rs"));

        // Verify original rationale is included.
        assert!(context.contains("Agent's original rationale:"));
        assert!(context.contains("Refactored to use JWT"));

        // Verify explanation tiers are included.
        assert!(context.contains("What was changed:"));
        assert!(context.contains("Switched auth from sessions to JWT tokens"));
        assert!(context.contains("Why:"));
        assert!(context.contains("Implemented RS256 signature verification"));

        // *** THE KEY TEST: Verify comment threads are injected! ***
        assert!(context.contains("Review discussion:"));
        assert!(context.contains("reviewer-1"));
        assert!(context.contains("This needs error handling for null tokens"));
        assert!(context.contains("agent-1"));
        assert!(context.contains("Understood, I'll add validation"));
        assert!(context.contains("Also consider adding tests for edge cases"));

        // Verify guidance is included.
        assert!(context.contains("Your task:"));
        assert!(context.contains("Address each discussion item"));
    }

    #[test]
    fn parent_context_handles_discuss_items_without_comments() {
        // Ensure graceful handling when discuss items don't have comment threads yet.
        use chrono::Utc;
        use ta_changeset::draft_package::{
            AgentIdentity, Artifact, ArtifactDisposition, ChangeType, Changes, DraftPackage,
            DraftStatus, Goal, Iteration, Plan, Provenance, ReviewRequests, Risk, Signatures,
            Summary, WorkspaceRef,
        };
        use ta_goal::GoalRun;
        use uuid::Uuid;

        let project = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(project.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        let parent_goal_id = Uuid::new_v4();
        let parent_pr_id = Uuid::new_v4();

        let mut parent_goal = GoalRun::new(
            "Test goal",
            "Test objective",
            "test-agent",
            project.path().join(".ta/staging/parent"),
            project.path().join(".ta/store/parent"),
        );
        parent_goal.goal_run_id = parent_goal_id; // Override the UUID for testing
        parent_goal.pr_package_id = Some(parent_pr_id);
        parent_goal.source_dir = Some(project.path().to_path_buf());
        goal_store.save(&parent_goal).unwrap();

        // Artifact with Discuss disposition but NO comment thread.
        let artifact_no_comments = Artifact {
            resource_uri: "fs://workspace/test.rs".to_string(),
            change_type: ChangeType::Add,
            diff_ref: "changeset:0".to_string(),
            tests_run: vec![],
            disposition: ArtifactDisposition::Discuss,
            rationale: Some("Needs review".to_string()),
            dependencies: vec![],
            explanation_tiers: None,
            comments: None, // No comments yet
            amendment: None,
            kind: None,
        };

        let parent_draft = DraftPackage {
            package_version: "1.0.0".to_string(),
            package_id: parent_pr_id,
            created_at: Utc::now(),
            goal: Goal {
                goal_id: parent_goal_id.to_string(),
                title: "Test".to_string(),
                objective: "Test".to_string(),
                success_criteria: vec![],
                constraints: vec![],
                parent_goal_title: None,
            },
            iteration: Iteration {
                iteration_id: "iter-1".to_string(),
                sequence: 1,
                workspace_ref: WorkspaceRef {
                    ref_type: "staging".to_string(),
                    ref_name: "staging/1".to_string(),
                    base_ref: None,
                },
            },
            agent_identity: AgentIdentity {
                agent_id: "agent-1".to_string(),
                agent_type: "coder".to_string(),
                constitution_id: "default".to_string(),
                capability_manifest_hash: "hash-123".to_string(),
                orchestrator_run_id: None,
            },
            summary: Summary {
                what_changed: "Test".to_string(),
                why: "Test".to_string(),
                impact: "Test".to_string(),
                rollback_plan: "Test".to_string(),
                open_questions: vec![],
                alternatives_considered: vec![],
            },
            plan: Plan {
                completed_steps: vec![],
                next_steps: vec![],
                decision_log: vec![],
            },
            changes: Changes {
                artifacts: vec![artifact_no_comments],
                patch_sets: vec![],
                pending_actions: vec![],
            },
            risk: Risk {
                risk_score: 0,
                findings: vec![],
                policy_decisions: vec![],
            },
            provenance: Provenance {
                inputs: vec![],
                tool_trace_hash: "trace-123".to_string(),
            },
            review_requests: ReviewRequests {
                requested_actions: vec![],
                reviewers: vec![],
                required_approvals: 1,
                notes_to_reviewer: None,
            },
            signatures: Signatures {
                package_hash: "hash-456".to_string(),
                agent_signature: "sig-789".to_string(),
                gateway_attestation: None,
            },
            status: DraftStatus::PendingReview,
            verification_warnings: vec![],
            validation_log: vec![],
            display_id: None,
            tag: None,
            vcs_status: None,
            parent_draft_id: None,
            pending_approvals: vec![],
            supervisor_review: None,
            ignored_artifacts: vec![],
            baseline_artifacts: vec![],
            agent_decision_log: vec![],
            goal_shortref: None,
            draft_seq: 0,
        };

        super::super::draft::save_package(&config, &parent_draft).unwrap();

        let context = build_parent_context_section(Some(parent_goal_id), &goal_store, &config);

        // Should still show discuss items even without comments.
        assert!(context.contains("Items for Discussion"));
        assert!(context.contains("fs://workspace/test.rs"));
        assert!(context.contains("Needs review"));

        // Should NOT crash or show "Review discussion:" when there are no comments.
        assert!(!context.contains("Review discussion:"));
    }

    #[test]
    fn inject_claude_md_restores_original_on_reinjection() {
        // Simulates the follow-up-reusing-parent-staging scenario:
        // 1. First injection writes TA header on top of original content
        // 2. Second injection (follow-up) should restore original, then inject fresh
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        let original = "# Original CLAUDE.md\n\nCurrent version: 0.4.4-alpha\n";
        std::fs::write(staging.path().join("CLAUDE.md"), original).unwrap();

        // First injection (parent goal).
        inject_claude_md(
            staging.path(),
            "Parent goal",
            "goal-parent-111",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        let first_injected = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(first_injected.contains("Trusted Autonomy"));
        assert!(first_injected.contains("Parent goal"));
        assert!(first_injected.contains("Original CLAUDE.md"));

        // Second injection (follow-up reusing same staging — no restore between).
        inject_claude_md(
            staging.path(),
            "Follow-up goal",
            "goal-followup-222",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        let second_injected = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();

        // Should contain the NEW goal info.
        assert!(
            second_injected.contains("Follow-up goal"),
            "should have follow-up title"
        );
        assert!(
            second_injected.contains("goal-followup-222"),
            "should have follow-up goal ID"
        );

        // Should contain the ORIGINAL content (not the parent's injection).
        assert!(
            second_injected.contains("0.4.4-alpha"),
            "should have original version, not stale parent version"
        );

        // Should NOT contain the parent's goal info (that was the old injection).
        assert!(
            !second_injected.contains("goal-parent-111"),
            "should not contain parent's injected goal ID"
        );

        // The original content should appear exactly once (no nesting).
        assert_eq!(
            second_injected.matches("Original CLAUDE.md").count(),
            1,
            "original content should appear exactly once, not nested"
        );

        // Restore should get back the true original.
        restore_claude_md(staging.path()).unwrap();
        let restored = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert_eq!(restored, original);
    }

    // ── v0.10.18.5 tests ──────────────────────────────────────

    #[test]
    fn non_interactive_env_in_config() {
        let config = builtin_agent_config("claude-flow");
        assert!(!config.non_interactive_env.is_empty());
        assert_eq!(
            config
                .non_interactive_env
                .get("CLAUDE_FLOW_NON_INTERACTIVE"),
            Some(&"true".to_string())
        );
        assert_eq!(
            config.non_interactive_env.get("CLAUDE_FLOW_TOPOLOGY"),
            Some(&"mesh".to_string())
        );
    }

    #[test]
    fn non_interactive_env_not_set_for_non_headless_agents() {
        // Claude Code should have empty non_interactive_env.
        let config = builtin_agent_config("claude-code");
        assert!(config.non_interactive_env.is_empty());
    }

    #[test]
    fn auto_answers_in_config() {
        let config = builtin_agent_config("claude-flow");
        assert!(!config.auto_answers.is_empty());
        assert!(config.auto_answers[0].prompt.contains("topology"));
        assert_eq!(config.auto_answers[0].response, "1");
    }

    #[test]
    fn auto_answer_config_deserialize() {
        let yaml = r#"
command: claude
args_template: ["{prompt}"]
auto_answers:
  - prompt: "Continue\\?"
    response: "y"
    fallback: true
  - prompt: "Enter name:"
    response: "{goal_title}"
"#;
        let config: AgentLaunchConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.auto_answers.len(), 2);
        assert!(config.auto_answers[0].fallback);
        assert!(!config.auto_answers[1].fallback);
        assert_eq!(config.auto_answers[1].response, "{goal_title}");
    }

    #[test]
    fn non_interactive_env_deserialize() {
        let yaml = r#"
command: agent
args_template: []
non_interactive_env:
  MY_FLAG: "true"
  MY_TOPOLOGY: "mesh"
"#;
        let config: AgentLaunchConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.non_interactive_env.len(), 2);
        assert_eq!(
            config.non_interactive_env.get("MY_FLAG"),
            Some(&"true".to_string())
        );
    }

    // ── Headless args tests ───────────────────────────────────────

    #[test]
    fn claude_code_headless_args_include_stream_json() {
        let config = builtin_agent_config("claude-code");
        assert!(
            config.headless_args.contains(&"--print".to_string()),
            "claude-code headless_args must include --print for non-interactive execution"
        );
        assert!(
            config.headless_args.contains(&"--verbose".to_string()),
            "claude-code headless_args must include --verbose for stream-json to work"
        );
        assert!(
            config
                .headless_args
                .contains(&"--output-format".to_string()),
            "claude-code headless_args must include --output-format"
        );
        assert!(
            config.headless_args.contains(&"stream-json".to_string()),
            "claude-code headless_args must include stream-json"
        );
    }

    #[test]
    fn claude_code_headless_args_order() {
        // The args must appear in the right order for the CLI to parse them.
        let config = builtin_agent_config("claude-code");
        let args = &config.headless_args;
        let print_pos = args.iter().position(|a| a == "--print");
        let verbose_pos = args.iter().position(|a| a == "--verbose");
        let format_pos = args.iter().position(|a| a == "--output-format");
        let json_pos = args.iter().position(|a| a == "stream-json");

        assert!(print_pos.is_some(), "--print must be present");
        assert!(verbose_pos.is_some(), "--verbose must be present");
        assert!(format_pos.is_some(), "--output-format must be present");
        assert!(json_pos.is_some(), "stream-json must be present");

        // --print must come first, --output-format must precede stream-json.
        assert!(
            print_pos.unwrap() < verbose_pos.unwrap(),
            "--print must precede --verbose"
        );
        assert!(
            format_pos.unwrap() < json_pos.unwrap(),
            "--output-format must precede stream-json"
        );
    }

    #[test]
    fn other_agents_have_no_headless_args() {
        // Codex and generic agents don't need special headless flags.
        assert!(builtin_agent_config("codex").headless_args.is_empty());
        assert!(builtin_agent_config("unknown-agent")
            .headless_args
            .is_empty());
    }

    #[test]
    fn claude_flow_has_non_interactive_env() {
        let config = builtin_agent_config("claude-flow");
        assert!(config.headless_args.is_empty());
        assert!(config
            .non_interactive_env
            .contains_key("CLAUDE_FLOW_NON_INTERACTIVE"));
    }

    // ── v0.13.15: MCP injection cleanup tests (item 5) ───────────

    #[test]
    fn inject_memory_then_restore_removes_ta_memory_key() {
        let staging = tempfile::TempDir::new().unwrap();

        // Inject ta-memory into an empty staging dir (no pre-existing .mcp.json).
        inject_memory_mcp_server(staging.path()).unwrap();

        // .mcp.json should now exist and contain the ta-memory key.
        let mcp_path = staging.path().join(MCP_JSON_PATH);
        assert!(mcp_path.exists(), ".mcp.json should be created by inject");
        let content = std::fs::read_to_string(&mcp_path).unwrap();
        assert!(
            content.contains("ta-memory"),
            "ta-memory key must be present after inject"
        );

        // Restore — no backup exists (inject doesn't create one for a missing file).
        restore_mcp_server_config(staging.path()).unwrap();

        // After restore, ta-memory key must be absent.
        if mcp_path.exists() {
            let after = std::fs::read_to_string(&mcp_path).unwrap();
            assert!(
                !after.contains("ta-memory"),
                "ta-memory key must be removed after restore, got: {}",
                after
            );
        }
        // If the file was removed entirely, that also satisfies the postcondition.
    }

    #[test]
    fn restore_mcp_no_injection_is_noop() {
        let staging = tempfile::TempDir::new().unwrap();
        // No .mcp.json, no backup — restore should be a no-op.
        let result = restore_mcp_server_config(staging.path());
        assert!(result.is_ok(), "restore with no state should succeed");
    }

    #[test]
    fn inject_memory_preserves_other_mcp_keys() {
        let staging = tempfile::TempDir::new().unwrap();
        let mcp_path = staging.path().join(MCP_JSON_PATH);

        // Write a .mcp.json with an existing non-TA server.
        std::fs::write(
            &mcp_path,
            r#"{"mcpServers": {"my-server": {"command": "my-cmd"}}}"#,
        )
        .unwrap();

        inject_memory_mcp_server(staging.path()).unwrap();

        let content = std::fs::read_to_string(&mcp_path).unwrap();
        assert!(content.contains("ta-memory"), "ta-memory must be added");
        assert!(
            content.contains("my-server"),
            "existing server must be preserved"
        );

        restore_mcp_server_config(staging.path()).unwrap();

        // After restore: my-server present, ta-memory absent.
        if mcp_path.exists() {
            let after = std::fs::read_to_string(&mcp_path).unwrap();
            assert!(!after.contains("ta-memory"), "ta-memory must be removed");
            // my-server was present before inject_memory and there was no backup,
            // so restore only strips ta-memory; other keys survive.
        }
    }

    // ── v0.13.17.5: Bug 1 fix — restore_mcp unconditional ─────────

    /// test_restore_runs_for_non_macro_goal (plan item 9.1):
    /// inject_mcp_server_config() + restore_mcp_server_config() must round-trip
    /// correctly even when the caller is not a macro goal. After restore, staging
    /// .mcp.json must match the original.
    #[test]
    fn restore_runs_for_non_macro_goal() {
        let staging = tempfile::TempDir::new().unwrap();
        let mcp_path = staging.path().join(MCP_JSON_PATH);

        // Simulate a pre-existing .mcp.json (user's original config).
        let original_content = r#"{"mcpServers": {"user-server": {"command": "user-cmd"}}}"#;
        std::fs::write(&mcp_path, original_content).unwrap();

        // Inject TA server config (runs for all goals, not just macro goals).
        inject_mcp_server_config(staging.path()).unwrap();

        // After inject, backup should exist and .mcp.json should contain TA entries.
        let backup_path = staging.path().join(MCP_JSON_BACKUP);
        assert!(backup_path.exists(), "backup must exist after inject");
        let injected = std::fs::read_to_string(&mcp_path).unwrap();
        assert!(
            injected.contains("ta"),
            ".mcp.json must have TA entry after inject"
        );

        // Restore (called unconditionally — simulates non-macro goal path).
        restore_mcp_server_config(staging.path()).unwrap();

        // After restore, .mcp.json must match original exactly.
        assert!(mcp_path.exists(), ".mcp.json must exist after restore");
        let restored = std::fs::read_to_string(&mcp_path).unwrap();
        assert_eq!(
            restored, original_content,
            "staging .mcp.json must match source after restore"
        );
        assert!(
            !backup_path.exists(),
            "backup must be removed after restore"
        );
    }

    /// test_mcp_json_absent_from_draft_artifacts (plan item 9.2):
    /// The overlay diff excludes .mcp.json (TA_MANAGED_FILES) even if staging
    /// has a modified .mcp.json. Simulates Bug 1 scenario.
    #[test]
    fn mcp_json_excluded_from_overlay_diff() {
        use ta_workspace::{ExcludePatterns, OverlayWorkspace};

        let source = tempfile::TempDir::new().unwrap();
        let staging = tempfile::TempDir::new().unwrap();

        // Create source files.
        std::fs::write(source.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(source.path().join(".mcp.json"), r#"{"mcpServers": {}}"#).unwrap();

        // Copy source to staging (simulates overlay creation).
        std::fs::write(
            staging.path().join("main.rs"),
            "fn main() { println!(\"hi\"); }",
        )
        .unwrap();
        // .mcp.json in staging differs (simulates TA injection residue).
        std::fs::write(
            staging.path().join(".mcp.json"),
            r#"{"mcpServers": {"ta": {"command": "/usr/bin/ta", "args": ["serve"]}}}"#,
        )
        .unwrap();

        let overlay = OverlayWorkspace::open(
            "test-goal".to_string(),
            source.path(),
            staging.path(),
            ExcludePatterns::defaults(),
        );
        let changes = overlay.diff_all().unwrap();

        // .mcp.json must NOT appear in the diff.
        let mcp_in_diff = changes.iter().any(|c| {
            let path = match c {
                ta_workspace::overlay::OverlayChange::Modified { path, .. } => path,
                ta_workspace::overlay::OverlayChange::Created { path, .. } => path,
                ta_workspace::overlay::OverlayChange::Deleted { path } => path,
            };
            path == ".mcp.json"
        });
        assert!(
            !mcp_in_diff,
            ".mcp.json must be excluded from overlay diff (TA-managed file)"
        );

        // main.rs change SHOULD appear.
        let main_in_diff = changes.iter().any(|c| {
            let path = match c {
                ta_workspace::overlay::OverlayChange::Modified { path, .. } => path,
                ta_workspace::overlay::OverlayChange::Created { path, .. } => path,
                ta_workspace::overlay::OverlayChange::Deleted { path } => path,
            };
            path == "main.rs"
        });
        assert!(main_in_diff, "main.rs change must appear in diff");
    }

    // ── Context budget tests (v0.14.3.1) ──

    #[test]
    fn test_budget_trims_solutions_section() {
        // Verify that trim_solutions_section reduces entry count.
        let section = "\n## Known Solutions\n\nThe following problem/solution pairs were captured from previous sessions:\n\n\
            - **Problem A**: Solution A\n\
            - **Problem B**: Solution B\n\
            - **Problem C**: Solution C\n\
            - **Problem D**: Solution D\n\
            - **Problem E**: Solution E\n\
            - **Problem F**: Solution F\n\
            - **Problem G**: Solution G\n\
            - **Problem H**: Solution H\n\
            - **Problem I**: Solution I\n\
            - **Problem J**: Solution J\n\
            - **Problem K**: Solution K\n\
            - **Problem L**: Solution L\n\
            - **Problem M**: Solution M\n\
            - **Problem N**: Solution N\n\
            - **Problem O**: Solution O\n";

        let trimmed = trim_solutions_section(section, 5);
        // Should contain exactly 5 entries.
        let entry_count = trimmed.matches("- **Problem").count();
        assert_eq!(
            entry_count, 5,
            "should keep exactly 5 entries, got {}",
            entry_count
        );
        assert!(trimmed.contains("Problem A"), "first entry preserved");
        assert!(trimmed.contains("Problem E"), "fifth entry preserved");
        assert!(!trimmed.contains("Problem F"), "sixth entry removed");
    }

    #[test]
    fn test_budget_inject_with_tight_budget_does_not_panic() {
        // Inject with a very small budget — should not panic, just trim.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        inject_claude_md(
            staging.path(),
            "Budget test goal",
            "goal-budget-001",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            1_000, // very tight budget
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        // CLAUDE.md must still exist and contain the TA header.
        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(
            claude_md.contains("Trusted Autonomy"),
            "header preserved even under tight budget"
        );
    }

    #[test]
    fn test_budget_disabled_when_zero() {
        // Budget = 0 means no trimming — inject proceeds normally.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        inject_claude_md(
            staging.path(),
            "No-budget goal",
            "goal-nobudget-002",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0, // budget disabled
            5,
            5,
            &ta_submit::config::ContextMode::default(),
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Trusted Autonomy"));
    }

    #[test]
    fn test_context_budget_config_defaults() {
        let section = ta_submit::config::WorkflowSection::default();
        assert_eq!(section.context_budget_chars, 40_000);
        assert_eq!(section.plan_done_window, 5);
        assert_eq!(section.plan_pending_window, 5);
    }

    #[test]
    fn test_context_budget_config_from_toml() {
        let toml = r#"
[workflow]
context_budget_chars = 20000
plan_done_window = 3
plan_pending_window = 7
"#;
        let wf: ta_submit::WorkflowConfig = toml::from_str(toml).unwrap();
        assert_eq!(wf.workflow.context_budget_chars, 20_000);
        assert_eq!(wf.workflow.plan_done_window, 3);
        assert_eq!(wf.workflow.plan_pending_window, 7);
    }

    #[test]
    fn test_mcp_mode_skips_plan_injection() {
        // In "mcp" context mode, inject_claude_md should omit the Plan Context section.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        // Write a PLAN.md so build_plan_section would normally produce content.
        let plan_dir = staging.path();
        std::fs::write(
            plan_dir.join("PLAN.md"),
            "### v0.1 — Test Phase\n<!-- status: pending -->\n",
        )
        .unwrap();

        inject_claude_md(
            staging.path(),
            "MCP mode goal",
            "goal-mcp-001",
            Some("v0.1"),
            Some(staging.path()),
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::Mcp,
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Trusted Autonomy"), "header present");
        assert!(
            !claude_md.contains("## Plan Context"),
            "plan section omitted in mcp mode"
        );
        assert!(
            claude_md.contains("ta_plan_status"),
            "tool hint present in mcp mode"
        );
    }

    #[test]
    fn test_mcp_mode_registers_ta_plan_tool_hint() {
        // The hint line must mention ta_plan_status and community tools.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        inject_claude_md(
            staging.path(),
            "MCP hint goal",
            "goal-mcp-hint",
            None,
            None,
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::Mcp,
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(
            claude_md.contains("ta_plan_status"),
            "mcp mode hint mentions ta_plan_status"
        );
        assert!(
            claude_md.contains("community_search"),
            "mcp mode hint mentions community_search"
        );
    }

    #[test]
    fn test_hybrid_mode_includes_memory_not_plan() {
        // In "hybrid" context mode, plan section is omitted but the tool hint is present.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        std::fs::write(
            staging.path().join("PLAN.md"),
            "### v0.1 — Test Phase\n<!-- status: pending -->\n",
        )
        .unwrap();

        inject_claude_md(
            staging.path(),
            "Hybrid mode goal",
            "goal-hybrid-001",
            Some("v0.1"),
            Some(staging.path()),
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::Hybrid,
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Trusted Autonomy"), "header present");
        assert!(
            !claude_md.contains("## Plan Context"),
            "plan section omitted in hybrid mode"
        );
        assert!(
            claude_md.contains("ta_plan_status"),
            "tool hint present in hybrid mode"
        );
    }

    #[test]
    fn test_inject_mode_includes_plan_section() {
        // Default "inject" mode should include Plan Context when PLAN.md exists.
        let staging = TempDir::new().unwrap();
        let config = GatewayConfig::for_project(staging.path());
        let goal_store = GoalRunStore::new(&config.goals_dir).unwrap();

        std::fs::write(
            staging.path().join("PLAN.md"),
            "### v0.1 — Test Phase\n<!-- status: pending -->\n",
        )
        .unwrap();

        inject_claude_md(
            staging.path(),
            "Inject mode goal",
            "goal-inject-001",
            Some("v0.1"),
            Some(staging.path()),
            None,
            &goal_store,
            &config,
            false,
            false,
            None,
            0,
            5,
            5,
            &ta_submit::config::ContextMode::Inject,
        )
        .unwrap();

        let claude_md = std::fs::read_to_string(staging.path().join("CLAUDE.md")).unwrap();
        assert!(
            claude_md.contains("## Plan Context"),
            "plan section present in inject mode"
        );
        assert!(
            !claude_md.contains("ta_plan_status"),
            "no tool hint in inject mode"
        );
    }

    #[test]
    fn test_context_mode_config_defaults_to_inject() {
        let section = ta_submit::config::WorkflowSection::default();
        assert_eq!(section.context_mode, ta_submit::config::ContextMode::Inject);
    }

    #[test]
    fn test_context_mode_config_from_toml() {
        let toml_mcp = "[workflow]\ncontext_mode = \"mcp\"\n";
        let wf: ta_submit::WorkflowConfig = toml::from_str(toml_mcp).unwrap();
        assert_eq!(
            wf.workflow.context_mode,
            ta_submit::config::ContextMode::Mcp
        );

        let toml_hybrid = "[workflow]\ncontext_mode = \"hybrid\"\n";
        let wf2: ta_submit::WorkflowConfig = toml::from_str(toml_hybrid).unwrap();
        assert_eq!(
            wf2.workflow.context_mode,
            ta_submit::config::ContextMode::Hybrid
        );

        let toml_inject = "[workflow]\ncontext_mode = \"inject\"\n";
        let wf3: ta_submit::WorkflowConfig = toml::from_str(toml_inject).unwrap();
        assert_eq!(
            wf3.workflow.context_mode,
            ta_submit::config::ContextMode::Inject
        );
    }
}
