// operations.rs — ta operations subcommand (v0.13.1).
//
// Surfaces the operations log: corrective actions detected by the daemon watchdog.

use clap::Subcommand;
use ta_goal::{ActionSeverity, ActionStatus, OperationsLog};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum OperationsCommands {
    /// View the history of corrective actions detected by the daemon.
    ///
    /// The daemon watchdog continuously monitors goal health, disk space,
    /// and plugin status. When issues are detected, it records a corrective
    /// action proposal here. Use this to understand what the daemon has been
    /// doing and what issues it has detected.
    Log {
        /// Maximum number of entries to show (default: 20).
        #[arg(long, default_value = "20")]
        limit: usize,

        /// Show all entries (overrides --limit).
        #[arg(long)]
        all: bool,

        /// Filter to only show entries with this severity: info, warning, critical.
        #[arg(long)]
        severity: Option<String>,
    },
}

pub fn execute(command: &OperationsCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match command {
        OperationsCommands::Log {
            limit,
            all,
            severity,
        } => operations_log(config, *limit, *all, severity.as_deref()),
    }
}

fn operations_log(
    config: &GatewayConfig,
    limit: usize,
    all: bool,
    severity_filter: Option<&str>,
) -> anyhow::Result<()> {
    let log = OperationsLog::for_project(&config.workspace_root);
    let mut entries = log.read(if all { None } else { Some(limit) })?;

    // Filter by severity if requested.
    if let Some(sev) = severity_filter {
        let sev_lower = sev.to_lowercase();
        entries.retain(|a| match &a.severity {
            ActionSeverity::Info => sev_lower == "info",
            ActionSeverity::Warning => sev_lower == "warning" || sev_lower == "warn",
            ActionSeverity::Critical => sev_lower == "critical" || sev_lower == "crit",
        });
    }

    if entries.is_empty() {
        if let Some(sev) = severity_filter {
            println!("No corrective actions with severity '{}'.", sev);
        } else {
            println!("No corrective actions recorded.");
            println!("The daemon watchdog logs issues here when it detects problems.");
            println!("Start the daemon with `ta daemon start` to enable monitoring.");
        }
        return Ok(());
    }

    println!("{} corrective action(s):", entries.len());
    println!();

    for action in &entries {
        let severity_label = match action.severity {
            ActionSeverity::Info => "INFO",
            ActionSeverity::Warning => "WARN",
            ActionSeverity::Critical => "CRIT",
        };

        let status_label = match &action.status {
            ActionStatus::Proposed => "proposed".to_string(),
            ActionStatus::Approved { by } => format!("approved by {}", by),
            ActionStatus::Denied { reason } => format!("denied: {}", reason),
            ActionStatus::Executed { outcome } => format!("executed: {}", outcome),
            ActionStatus::Failed { error } => format!("failed: {}", error),
        };

        let ts = action.created_at.format("%Y-%m-%d %H:%M UTC").to_string();

        println!("[{}] {} — {}", severity_label, ts, action.issue);
        println!("  diagnosis: {}", action.diagnosis);
        println!("  action:    {}", action.proposed_action);
        println!("  status:    {}", status_label);
        if action.auto_healable {
            println!(
                "  heal:      eligible for auto-heal (action key: {})",
                action.action_key
            );
        }
        if let Some(goal_id) = action.goal_id {
            println!("  goal:      {}", &goal_id.to_string()[..8]);
        }
        println!();
    }

    Ok(())
}
