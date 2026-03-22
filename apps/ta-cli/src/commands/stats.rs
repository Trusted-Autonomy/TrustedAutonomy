// stats.rs — `ta stats` command: Feature Velocity Stats & Outcome Telemetry (v0.13.10).

use chrono::Utc;
use clap::Subcommand;
use ta_goal::{GoalOutcome, VelocityStore};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum StatsCommands {
    /// Show aggregate velocity stats (build time, outcomes, rework).
    Velocity {
        /// Filter to goals since date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
        /// Filter to a specific workflow type.
        #[arg(long)]
        workflow: Option<String>,
        /// Output raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Per-goal velocity breakdown table.
    #[command(name = "velocity-detail")]
    VelocityDetail {
        /// Filter to goals since date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<String>,
        /// Filter by outcome (applied, denied, cancelled, failed).
        #[arg(long)]
        outcome: Option<String>,
        /// Maximum entries to show (default: 20).
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Output raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export full velocity history as JSON or CSV.
    Export {
        /// Output format: json (default) or csv.
        #[arg(long, default_value = "json")]
        format: String,
    },
}

pub fn execute(cmd: &StatsCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let store = VelocityStore::for_project(&config.workspace_root);

    match cmd {
        StatsCommands::Velocity {
            since,
            workflow,
            json,
        } => {
            let mut entries = if let Some(since_str) = since {
                let dt = parse_date(since_str)?;
                store.load_since(dt)?
            } else {
                store.load_all()?
            };

            if let Some(wf) = workflow {
                entries.retain(|e| e.workflow == *wf);
            }

            if entries.is_empty() {
                println!("No velocity data recorded yet.");
                println!("Velocity data is written when goals complete (applied, denied, cancelled, failed).");
                return Ok(());
            }

            let agg = ta_goal::VelocityAggregate::from_entries(&entries);

            if *json {
                println!("{}", serde_json::to_string_pretty(&agg)?);
                return Ok(());
            }

            println!("Velocity Stats");
            println!("{}", "─".repeat(50));
            println!("  Total goals:        {}", agg.total_goals);
            println!(
                "  Applied:            {} ({:.0}%)",
                agg.applied,
                pct(agg.applied, agg.total_goals)
            );
            println!(
                "  Denied:             {} ({:.0}%)",
                agg.denied,
                pct(agg.denied, agg.total_goals)
            );
            println!(
                "  Cancelled:          {} ({:.0}%)",
                agg.cancelled,
                pct(agg.cancelled, agg.total_goals)
            );
            println!(
                "  Failed:             {} ({:.0}%)",
                agg.failed,
                pct(agg.failed, agg.total_goals)
            );
            println!("{}", "─".repeat(50));
            println!(
                "  Avg build time:     {}",
                fmt_duration(agg.avg_build_seconds)
            );
            println!(
                "  P90 build time:     {}",
                fmt_duration(agg.p90_build_seconds)
            );
            println!(
                "  Avg rework time:    {}",
                fmt_duration(agg.avg_rework_seconds)
            );
            println!(
                "  Total rework:       {}",
                fmt_duration(agg.total_rework_seconds)
            );
        }

        StatsCommands::VelocityDetail {
            since,
            outcome,
            limit,
            json,
        } => {
            let mut entries = if let Some(since_str) = since {
                let dt = parse_date(since_str)?;
                store.load_since(dt)?
            } else {
                store.load_all()?
            };

            if let Some(outcome_str) = outcome {
                let filter_outcome = parse_outcome(outcome_str)?;
                entries.retain(|e| e.outcome == filter_outcome);
            }

            // Newest first.
            entries.sort_by(|a, b| b.started_at.cmp(&a.started_at));
            entries.truncate(*limit);

            if entries.is_empty() {
                println!("No velocity data matching filters.");
                return Ok(());
            }

            if *json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
                return Ok(());
            }

            println!(
                "{:<36} {:<10} {:<12} {:<10} {:<8}",
                "TITLE", "OUTCOME", "BUILD", "REWORK", "AMENDED"
            );
            println!("{}", "─".repeat(80));
            for e in &entries {
                let title = truncate(&e.title, 34);
                let outcome = e.outcome.to_string();
                let build = fmt_duration(e.build_seconds);
                let rework = if e.rework_seconds > 0 {
                    fmt_duration(e.rework_seconds)
                } else {
                    "-".to_string()
                };
                let amended = if e.amended { "yes" } else { "no" };
                println!(
                    "{:<36} {:<10} {:<12} {:<10} {:<8}",
                    title, outcome, build, rework, amended
                );
            }
        }

        StatsCommands::Export { format } => {
            let entries = store.load_all()?;
            if entries.is_empty() {
                println!("No velocity data to export.");
                return Ok(());
            }
            match format.as_str() {
                "csv" => {
                    println!(
                        "goal_id,title,workflow,agent,plan_phase,outcome,started_at,build_seconds,review_seconds,total_seconds,amended,follow_up_count,rework_seconds"
                    );
                    for e in &entries {
                        println!(
                            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
                            e.goal_id,
                            csv_escape(&e.title),
                            e.workflow,
                            e.agent,
                            e.plan_phase.as_deref().unwrap_or(""),
                            e.outcome,
                            e.started_at.format("%Y-%m-%dT%H:%M:%SZ"),
                            e.build_seconds,
                            e.review_seconds,
                            e.total_seconds,
                            e.amended,
                            e.follow_up_count,
                            e.rework_seconds,
                        );
                    }
                }
                _ => {
                    println!("{}", serde_json::to_string_pretty(&entries)?);
                }
            }
        }
    }

    Ok(())
}

fn parse_date(s: &str) -> anyhow::Result<chrono::DateTime<Utc>> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("Invalid date '{}' — expected YYYY-MM-DD", s))?;
    Ok(Utc.from_utc_datetime(&naive.and_hms_opt(0, 0, 0).unwrap()))
}

fn parse_outcome(s: &str) -> anyhow::Result<GoalOutcome> {
    match s {
        "applied" => Ok(GoalOutcome::Applied),
        "denied" => Ok(GoalOutcome::Denied),
        "cancelled" => Ok(GoalOutcome::Cancelled),
        "failed" => Ok(GoalOutcome::Failed),
        "timeout" => Ok(GoalOutcome::Timeout),
        _ => anyhow::bail!(
            "Unknown outcome '{}' — valid values: applied, denied, cancelled, failed, timeout",
            s
        ),
    }
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 / total as f64 * 100.0
    }
}

fn fmt_duration(seconds: i64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        let h = seconds / 3600;
        let m = (seconds % 3600) / 60;
        format!("{}h {}m", h, m)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max.saturating_sub(3)])
    } else {
        s.to_string()
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
