// team_session.rs — `ta team-session` subcommand: start, pause, resume, stop, status.
//
// Mirrors `connector.rs`'s shape: no RPC to the running daemon — writes/reads
// the same `.ta/team-sessions/<id>/` files the daemon's `team_session.rs`
// supervisor loop uses (state.json, supervisor-status.json, and
// pause/resume/stop/restart signal files).

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

use ta_policy::business_budget::BudgetGuardrails;
use ta_workflow::WorkflowDefinition;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TeamSessionStatus {
    Active,
    Paused,
    Suspended,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamSessionStageConfig {
    name: String,
    roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamSessionConfig {
    name: String,
    workflow_path: String,
    team_toml_path: String,
    objective: String,
    /// Business-metric budget guardrail declared by the bound workflow's
    /// `budget:` section (v0.17.5.2), resolved once at `start` time — the
    /// daemon's supervisor loop never re-parses the workflow YAML.
    #[serde(default)]
    budget: Option<BudgetGuardrails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeamSessionState {
    id: String,
    config: TeamSessionConfig,
    stages: Vec<TeamSessionStageConfig>,
    status: TeamSessionStatus,
    current_stage_index: usize,
    findings: Vec<serde_json::Value>,
    restart_count: u32,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
struct SupervisorStatus {
    #[allow(dead_code)]
    id: String,
    status: String,
    current_stage: Option<String>,
    current_role: Option<String>,
    restart_count: u32,
    last_cycle_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn session_dir(project_root: &Path, id: &str) -> std::path::PathBuf {
    project_root.join(".ta").join("team-sessions").join(id)
}

fn state_path(project_root: &Path, id: &str) -> std::path::PathBuf {
    session_dir(project_root, id).join("state.json")
}

fn load_state(project_root: &Path, id: &str) -> Option<TeamSessionState> {
    let raw = std::fs::read_to_string(state_path(project_root, id)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn read_supervisor_status(project_root: &Path, id: &str) -> Option<SupervisorStatus> {
    let path = session_dir(project_root, id).join("supervisor-status.json");
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_signal(project_root: &Path, id: &str, signal: &str) -> std::io::Result<()> {
    let dir = session_dir(project_root, id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(signal), signal)
}

#[derive(Debug, Subcommand)]
pub enum TeamSessionCommands {
    /// Start a new persistent team session bound to a workflow YAML and `.ta/team.toml`.
    ///
    /// Resolves the workflow's stage order once at start time and persists it into
    /// the session's state.json — the daemon's supervisor loop reads that resolved
    /// list, it never re-parses the workflow YAML itself.
    ///
    /// Examples:
    ///   ta team-session start trading-desk --workflow templates/workflows/trading-desk.yaml --objective "Generate income > 2x within 6 months after fees"
    Start {
        /// Session name (also used as its ID).
        name: String,
        /// Path to the workflow YAML declaring roles/stages.
        #[arg(long)]
        workflow: String,
        /// Path to the team.toml binding roles to agents (defaults to `.ta/team.toml`).
        #[arg(long)]
        team_toml: Option<String>,
        /// The session's overall objective, carried into every role's context.
        #[arg(long, default_value = "")]
        objective: String,
    },
    /// Pause a running session — the supervisor stops firing new goal-runs
    /// until `ta team-session resume <name>`.
    Pause { name: String },
    /// Resume a paused session.
    Resume { name: String },
    /// Stop a session permanently (does not delete its state.json history).
    Stop { name: String },
    /// Show live supervisor status for one or all sessions.
    Status {
        /// Session name. Shows all if omitted.
        name: Option<String>,
    },
}

pub fn execute(command: &TeamSessionCommands, project_root: &Path) -> Result<()> {
    match command {
        TeamSessionCommands::Start {
            name,
            workflow,
            team_toml,
            objective,
        } => start(
            project_root,
            name,
            workflow,
            team_toml.as_deref(),
            objective,
        ),
        TeamSessionCommands::Pause { name } => {
            write_signal(project_root, name, "pause-signal").context("writing pause-signal")?;
            println!("[team-session] pause requested for '{name}' — takes effect on the supervisor's next poll.");
            Ok(())
        }
        TeamSessionCommands::Resume { name } => {
            write_signal(project_root, name, "resume-signal").context("writing resume-signal")?;
            println!("[team-session] resume requested for '{name}'.");
            Ok(())
        }
        TeamSessionCommands::Stop { name } => {
            write_signal(project_root, name, "stop-signal").context("writing stop-signal")?;
            println!("[team-session] stop requested for '{name}'.");
            Ok(())
        }
        TeamSessionCommands::Status { name } => status(project_root, name.as_deref()),
    }
}

fn start(
    project_root: &Path,
    name: &str,
    workflow_path: &str,
    team_toml: Option<&str>,
    objective: &str,
) -> Result<()> {
    if state_path(project_root, name).exists() {
        bail!(
            "Team session '{name}' already exists at {}. Use `ta team-session status {name}` to check it, or pick a different name.",
            state_path(project_root, name).display()
        );
    }

    let workflow_full_path = project_root.join(workflow_path);
    let definition = WorkflowDefinition::from_file(&workflow_full_path).with_context(|| {
        format!(
            "Failed to load workflow YAML at {} for team session '{name}'. Check the --workflow path is correct and the file is valid workflow YAML.",
            workflow_full_path.display()
        )
    })?;
    let stage_order = definition
        .stage_order()
        .with_context(|| format!("Workflow '{workflow_path}' has a cyclic stage dependency graph — cannot resolve a stage order for team session '{name}'."))?;

    let stages: Vec<TeamSessionStageConfig> = stage_order
        .iter()
        .filter_map(|stage_name| {
            definition
                .stages
                .iter()
                .find(|s| &s.name == stage_name)
                .map(|s| TeamSessionStageConfig {
                    name: s.name.clone(),
                    roles: s.roles.clone(),
                })
        })
        .collect();

    if stages.is_empty() {
        bail!(
            "Workflow '{workflow_path}' declares no stages — team session '{name}' would have nothing to run. Add at least one `stages:` entry to the workflow YAML."
        );
    }

    let budget = definition.budget.map(|b| {
        let b = *b;
        BudgetGuardrails {
            metric: b.metric,
            total: b.total,
            per_action_max_pct: b.per_action_max_pct,
            soft_threshold_pct: b.soft_threshold_pct,
            objective: b.objective,
        }
    });

    let now = chrono::Utc::now();
    let state = TeamSessionState {
        id: name.to_string(),
        config: TeamSessionConfig {
            name: name.to_string(),
            workflow_path: workflow_path.to_string(),
            team_toml_path: team_toml.unwrap_or(".ta/team.toml").to_string(),
            objective: objective.to_string(),
            budget,
        },
        stages,
        status: TeamSessionStatus::Active,
        current_stage_index: 0,
        findings: Vec::new(),
        restart_count: 0,
        created_at: now,
        updated_at: now,
    };

    let dir = session_dir(project_root, name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create session directory {}", dir.display()))?;
    let raw = serde_json::to_string_pretty(&state)?;
    std::fs::write(state_path(project_root, name), raw)
        .with_context(|| format!("Failed to write state.json for team session '{name}'"))?;

    println!(
        "[team-session] started '{name}' with {} stage(s) from '{workflow_path}'. The daemon supervisor picks it up on its next startup or poll cycle. Check progress with `ta team-session status {name}`.",
        state.stages.len()
    );
    Ok(())
}

fn status(project_root: &Path, name: Option<&str>) -> Result<()> {
    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => {
            let dir = project_root.join(".ta").join("team-sessions");
            let mut names = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        names.push(entry.file_name().to_string_lossy().to_string());
                    }
                }
            }
            names.sort();
            names
        }
    };

    if names.is_empty() {
        println!("No team sessions found under .ta/team-sessions/. Start one with `ta team-session start <name> --workflow <path>`.");
        return Ok(());
    }

    for n in &names {
        let Some(state) = load_state(project_root, n) else {
            println!("{n}: state.json missing or unreadable — not a valid team session.");
            continue;
        };
        match read_supervisor_status(project_root, n) {
            Some(sup) => {
                println!(
                    "{n}: {} (stage={:?} role={:?} restarts={} last_cycle={:?})",
                    sup.status,
                    sup.current_stage,
                    sup.current_role,
                    sup.restart_count,
                    sup.last_cycle_at
                );
            }
            None => {
                println!(
                    "{n}: {:?} (persisted state only — supervisor hasn't run a cycle yet; findings={})",
                    state.status,
                    state.findings.len()
                );
            }
        }
        print_budget_status(project_root, n, &state.config.budget);
    }
    Ok(())
}

/// Prints both budget concepts side by side (v0.17.5.2 item 5): the
/// business-metric budget (spent/total from this session's ledger) and the
/// LLM-token budget (`max_tokens_per_goal` from `.ta/policy.yaml`, if
/// configured). A session can be within one and over the other — both must
/// be independently visible, not folded into a single number.
fn print_budget_status(project_root: &Path, session_id: &str, budget: &Option<BudgetGuardrails>) {
    let ledger_path = budget_ledger_path(project_root, session_id);
    let spent = ta_policy::business_budget::ledger_running_total(&ledger_path);
    let policy_max_tokens = std::fs::read_to_string(project_root.join(".ta").join("policy.yaml"))
        .ok()
        .and_then(|raw| serde_yaml::from_str::<ta_policy::document::PolicyDocument>(&raw).ok())
        .and_then(|doc| doc.budget)
        .and_then(|b| b.max_tokens_per_goal);

    for line in format_budget_status(budget, spent, policy_max_tokens) {
        println!("{line}");
    }
}

/// Pure formatting logic for [`print_budget_status`], separated out so
/// tests can assert on content rather than just "doesn't panic".
fn format_budget_status(
    budget: &Option<BudgetGuardrails>,
    ledger_spent: f64,
    policy_max_tokens: Option<u64>,
) -> Vec<String> {
    let mut lines = Vec::new();

    match budget {
        Some(b) => {
            let pct = if b.total > 0.0 {
                ledger_spent / b.total * 100.0
            } else {
                0.0
            };
            lines.push(format!(
                "  business budget ({}): {:.2} / {:.2} spent ({:.1}%){}",
                b.metric,
                ledger_spent,
                b.total,
                pct,
                match b.soft_threshold_pct {
                    Some(soft) if pct >= soft => " — soft threshold crossed, escalating",
                    _ => "",
                }
            ));
        }
        None => {
            lines.push("  business budget: none declared by this session's workflow".to_string())
        }
    }

    lines.push(match policy_max_tokens {
        Some(max_tokens) => {
            format!("  token budget: max_tokens_per_goal={max_tokens} (from .ta/policy.yaml)")
        }
        None => {
            "  token budget: not configured (.ta/policy.yaml has no budget.max_tokens_per_goal)"
                .to_string()
        }
    });

    lines
}

fn budget_ledger_path(project_root: &Path, session_id: &str) -> std::path::PathBuf {
    session_dir(project_root, session_id).join("budget-ledger.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_role_workflow(dir: &Path) -> String {
        let path = dir.join("trading-desk.yaml");
        std::fs::write(
            &path,
            r#"
name: trading-desk
roles:
  analyst:
    agent: claude-code
    prompt: "Analyze the market."
  strategist:
    agent: claude-code
    prompt: "Decide a strategy."
stages:
  - name: analyze
    roles: ["analyst"]
  - name: decide
    depends_on: ["analyze"]
    roles: ["strategist"]
"#,
        )
        .unwrap();
        "trading-desk.yaml".to_string()
    }

    fn write_role_workflow_with_budget(dir: &Path) -> String {
        let path = dir.join("trading-desk-budgeted.yaml");
        std::fs::write(
            &path,
            r#"
name: trading-desk
roles:
  analyst:
    agent: claude-code
    prompt: "Analyze the market."
stages:
  - name: analyze
    roles: ["analyst"]
budget:
  metric: usd
  total: 1000.0
  per_action_max_pct: 10.0
  soft_threshold_pct: 80.0
  objective: "generate income > 2x within 6 months after fees"
"#,
        )
        .unwrap();
        "trading-desk-budgeted.yaml".to_string()
    }

    #[test]
    fn start_writes_state_json_with_resolved_stage_order() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());

        start(dir.path(), "sess-1", &workflow_path, None, "Make money").unwrap();

        let state = load_state(dir.path(), "sess-1").unwrap();
        assert_eq!(state.stages.len(), 2);
        assert_eq!(state.stages[0].name, "analyze");
        assert_eq!(state.stages[1].name, "decide");
        assert_eq!(state.status, TeamSessionStatus::Active);
        assert!(state.config.budget.is_none());
    }

    #[test]
    fn start_resolves_workflow_budget_into_session_config() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow_with_budget(dir.path());

        start(dir.path(), "sess-1", &workflow_path, None, "Make money").unwrap();

        let state = load_state(dir.path(), "sess-1").unwrap();
        let budget = state.config.budget.expect("budget should be resolved");
        assert_eq!(budget.metric, "usd");
        assert_eq!(budget.total, 1000.0);
        assert_eq!(budget.per_action_max_pct, Some(10.0));
        assert_eq!(budget.soft_threshold_pct, Some(80.0));
    }

    #[test]
    fn status_shows_business_and_token_budget_side_by_side() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow_with_budget(dir.path());
        start(dir.path(), "sess-1", &workflow_path, None, "Make money").unwrap();

        let ledger_path = budget_ledger_path(dir.path(), "sess-1");
        ta_policy::business_budget::record_ledger_spend(&ledger_path, "buy AAPL", 250.0).unwrap();

        std::fs::create_dir_all(dir.path().join(".ta")).unwrap();
        std::fs::write(
            dir.path().join(".ta").join("policy.yaml"),
            "budget:\n  max_tokens_per_goal: 50000\n",
        )
        .unwrap();

        // Wiring check: status() must not error with both budgets present.
        status(dir.path(), Some("sess-1")).unwrap();
    }

    #[test]
    fn format_budget_status_shows_both_budgets_independently() {
        let budget = Some(BudgetGuardrails {
            metric: "usd".to_string(),
            total: 1000.0,
            per_action_max_pct: Some(10.0),
            soft_threshold_pct: Some(80.0),
            objective: None,
        });

        let lines = format_budget_status(&budget, 250.0, Some(50_000));
        assert!(lines[0].contains("business budget (usd): 250.00 / 1000.00 spent (25.0%)"));
        assert!(!lines[0].contains("soft threshold crossed"));
        assert!(lines[1].contains("max_tokens_per_goal=50000"));
    }

    #[test]
    fn format_budget_status_flags_soft_threshold_crossing() {
        let budget = Some(BudgetGuardrails {
            metric: "usd".to_string(),
            total: 1000.0,
            per_action_max_pct: Some(10.0),
            soft_threshold_pct: Some(80.0),
            objective: None,
        });

        let lines = format_budget_status(&budget, 850.0, None);
        assert!(
            lines[0].contains("soft threshold crossed"),
            "got: {}",
            lines[0]
        );
        assert!(lines[1].contains("not configured"));
    }

    #[test]
    fn format_budget_status_reports_none_declared_when_absent() {
        let lines = format_budget_status(&None, 0.0, None);
        assert!(lines[0].contains("none declared"));
    }

    #[test]
    fn start_rejects_a_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());
        start(dir.path(), "sess-1", &workflow_path, None, "").unwrap();

        let result = start(dir.path(), "sess-1", &workflow_path, None, "");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn start_rejects_missing_workflow_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = start(dir.path(), "sess-1", "does-not-exist.yaml", None, "");
        assert!(result.is_err());
    }

    #[test]
    fn pause_resume_stop_write_expected_signal_files() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());
        start(dir.path(), "sess-1", &workflow_path, None, "").unwrap();

        execute(
            &TeamSessionCommands::Pause {
                name: "sess-1".to_string(),
            },
            dir.path(),
        )
        .unwrap();
        assert!(session_dir(dir.path(), "sess-1")
            .join("pause-signal")
            .exists());

        execute(
            &TeamSessionCommands::Resume {
                name: "sess-1".to_string(),
            },
            dir.path(),
        )
        .unwrap();
        assert!(session_dir(dir.path(), "sess-1")
            .join("resume-signal")
            .exists());

        execute(
            &TeamSessionCommands::Stop {
                name: "sess-1".to_string(),
            },
            dir.path(),
        )
        .unwrap();
        assert!(session_dir(dir.path(), "sess-1")
            .join("stop-signal")
            .exists());
    }

    #[test]
    fn status_reports_persisted_state_before_any_supervisor_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = write_role_workflow(dir.path());
        start(dir.path(), "sess-1", &workflow_path, None, "").unwrap();

        // No supervisor-status.json written yet (daemon hasn't run a cycle) —
        // must not error, must fall back to persisted state.json.
        status(dir.path(), Some("sess-1")).unwrap();
    }

    /// v0.17.5.3's reference implementation — proves
    /// `templates/workflows/trading-desk.yaml` (the file this module's own
    /// `Start` doc comment tells users to pass to `--workflow`) is real,
    /// loadable, and resolves to the analyst -> strategist -> trader stage
    /// chain with the declared business-metric budget, not just prose.
    #[test]
    fn real_trading_desk_template_parses_and_resolves_analyst_strategist_trader_chain() {
        let yaml = include_str!("../../../../templates/workflows/trading-desk.yaml");
        let definition = WorkflowDefinition::from_yaml(yaml)
            .expect("templates/workflows/trading-desk.yaml must be valid workflow YAML");

        assert_eq!(definition.name, "trading-desk");
        let stage_order = definition.stage_order().unwrap();
        assert_eq!(stage_order, vec!["analyze", "decide", "execute"]);

        for role in ["analyst", "strategist", "trader"] {
            assert!(
                definition.roles.contains_key(role),
                "expected a '{role}' role definition"
            );
        }

        let budget = definition
            .budget
            .as_ref()
            .expect("trading-desk.yaml must declare a budget guardrail");
        assert_eq!(budget.metric, "usd");
        assert_eq!(budget.total, 1000.0);
    }

    #[test]
    fn real_trading_desk_template_starts_a_team_session_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let workflow_path = dir.path().join("trading-desk.yaml");
        std::fs::write(
            &workflow_path,
            include_str!("../../../../templates/workflows/trading-desk.yaml"),
        )
        .unwrap();

        start(
            dir.path(),
            "trading-desk",
            "trading-desk.yaml",
            None,
            "Generate income > 2x within 6 months after fees",
        )
        .unwrap();

        status(dir.path(), Some("trading-desk")).unwrap();
    }
}
