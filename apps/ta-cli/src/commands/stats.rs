// stats.rs — `ta stats` command: Feature Velocity Stats & Outcome Telemetry (v0.13.10).
//
// v0.15.7: Two-file design. ta stats velocity always shows merged aggregate,
// per-contributor breakdown, and phase conflict warnings.

use clap::Subcommand;
use ta_goal::{GoalOutcome, VelocityHistoryStore, VelocityStore};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum StatsCommands {
    /// Show aggregate velocity stats (build time, outcomes, rework).
    ///
    /// Merges local `velocity-stats.jsonl` and committed `velocity-history.jsonl`,
    /// deduplicating by goal_id. Always shows per-contributor breakdown and
    /// flags any plan phases where more than one person submitted work.
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
        /// Include only committed (shared) history entries.
        #[arg(long)]
        committed_only: bool,
    },
    /// Promote local velocity-stats.jsonl entries into the committed velocity-history.jsonl.
    ///
    /// Non-destructive — local file is unchanged. Stamps each migrated entry with
    /// the current machine_id. Skips entries already present in the history file.
    Migrate {
        /// Dry-run: show what would be migrated without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn execute(cmd: &StatsCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    let local_store = VelocityStore::for_project(&config.workspace_root);
    let history_store = VelocityHistoryStore::for_project(&config.workspace_root);

    match cmd {
        StatsCommands::Velocity {
            since,
            workflow,
            json,
        } => {
            // Merge local + committed, dedup by goal_id.
            let local = local_store.load_all()?;
            let committed = history_store.load_all()?;
            let (merged, committed_ids) = ta_goal::merge_velocity_entries(local, committed);

            let mut entries = if let Some(since_str) = since {
                let dt = parse_date(since_str)?;
                merged
                    .into_iter()
                    .filter(|e| e.started_at >= dt)
                    .collect::<Vec<_>>()
            } else {
                merged
            };

            if let Some(wf) = workflow {
                entries.retain(|e| e.workflow == *wf);
            }

            if entries.is_empty() {
                println!("No velocity data recorded yet.");
                println!(
                    "Velocity data is written when goals complete (applied, denied, cancelled, failed)."
                );
                return Ok(());
            }

            let local_only_count = entries
                .iter()
                .filter(|e| !committed_ids.contains(&e.goal_id))
                .count();

            let agg = ta_goal::VelocityAggregate::from_entries(&entries);

            // Only committed entries carry reliable committer/machine_id for team views.
            let committed_entries: Vec<_> = entries
                .iter()
                .filter(|e| committed_ids.contains(&e.goal_id))
                .cloned()
                .collect();
            let by_contributor = ta_goal::aggregate_by_contributor(&committed_entries);
            let conflicts = ta_goal::detect_phase_conflicts(&committed_entries);

            if *json {
                let out = serde_json::json!({
                    "aggregate": agg,
                    "by_contributor": by_contributor,
                    "phase_conflicts": conflicts,
                    "local_only_count": local_only_count,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
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

            // Per-contributor breakdown — only shown when committed history has entries.
            if !by_contributor.is_empty() {
                println!();
                println!(
                    "{:<24} {:>8} {:>8} {:>12}",
                    "CONTRIBUTOR", "GOALS", "APPLIED", "AVG BUILD"
                );
                println!("{}", "─".repeat(56));
                for c in &by_contributor {
                    println!(
                        "{:<24} {:>8} {:>8} {:>12}",
                        truncate(&c.contributor, 22),
                        c.total_goals,
                        c.applied,
                        fmt_duration(c.avg_build_seconds)
                    );
                }
            }

            // Phase conflict warnings.
            if !conflicts.is_empty() {
                println!();
                println!(
                    "⚠  Phase conflicts detected ({} phase{}):",
                    conflicts.len(),
                    if conflicts.len() == 1 { "" } else { "s" }
                );
                for c in &conflicts {
                    println!(
                        "   {} — {} entries from: {}",
                        c.phase_id,
                        c.entry_count,
                        c.contributors.join(", ")
                    );
                }
                println!("   Review these phases to ensure work is not being duplicated.");
            }

            if local_only_count > 0 {
                println!();
                println!(
                    "  Note: {} local-only entr{} not yet committed.",
                    local_only_count,
                    if local_only_count == 1 { "y" } else { "ies" }
                );
                println!("  Run `ta stats migrate` to promote to shared history.");
            }
        }

        StatsCommands::VelocityDetail {
            since,
            outcome,
            limit,
            json,
        } => {
            let local = local_store.load_all()?;
            let committed = history_store.load_all()?;
            let committed_ids: std::collections::HashSet<_> =
                committed.iter().map(|e| e.goal_id).collect();
            let (merged, _) = ta_goal::merge_velocity_entries(local, committed);

            let mut entries = if let Some(since_str) = since {
                let dt = parse_date(since_str)?;
                merged
                    .into_iter()
                    .filter(|e| e.started_at >= dt)
                    .collect::<Vec<_>>()
            } else {
                merged
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
                "{:<36} {:<10} {:<12} {:<10} {:<8} {:<8}",
                "TITLE", "OUTCOME", "BUILD", "REWORK", "AMENDED", "SOURCE"
            );
            println!("{}", "─".repeat(88));
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
                let source = if committed_ids.contains(&e.goal_id) {
                    "shared"
                } else {
                    "[local]"
                };
                println!(
                    "{:<36} {:<10} {:<12} {:<10} {:<8} {:<8}",
                    title, outcome, build, rework, amended, source
                );
            }
        }

        StatsCommands::Export {
            format,
            committed_only,
        } => {
            let entries = if *committed_only {
                history_store.load_all()?
            } else {
                let local = local_store.load_all()?;
                let committed = history_store.load_all()?;
                let (merged, _) = ta_goal::merge_velocity_entries(local, committed);
                merged
            };

            if entries.is_empty() {
                println!("No velocity data to export.");
                return Ok(());
            }
            match format.as_str() {
                "csv" => {
                    println!(
                        "goal_id,title,workflow,agent,plan_phase,outcome,started_at,\
                         build_seconds,review_seconds,total_seconds,amended,\
                         follow_up_count,rework_seconds,machine_id,committer"
                    );
                    for e in &entries {
                        println!(
                            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
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
                            e.machine_id,
                            csv_escape(e.committer.as_deref().unwrap_or("")),
                        );
                    }
                }
                _ => {
                    println!("{}", serde_json::to_string_pretty(&entries)?);
                }
            }
        }

        StatsCommands::Migrate { dry_run } => {
            let local_entries = local_store.load_all()?;
            let committed_entries = history_store.load_all()?;
            let committed_ids: std::collections::HashSet<_> =
                committed_entries.iter().map(|e| e.goal_id).collect();

            let pending: Vec<_> = local_entries
                .iter()
                .filter(|e| !committed_ids.contains(&e.goal_id))
                .collect();

            if pending.is_empty() {
                println!(
                    "Nothing to migrate — all local entries are already in committed history."
                );
                return Ok(());
            }

            println!(
                "{} local entr{} to promote into velocity-history.jsonl:",
                pending.len(),
                if pending.len() == 1 { "y" } else { "ies" }
            );
            for e in &pending {
                println!(
                    "  {} — {} ({})",
                    &e.goal_id.to_string()[..8],
                    truncate(&e.title, 50),
                    e.outcome
                );
            }

            if *dry_run {
                println!("\n[dry-run] No changes written. Remove --dry-run to migrate.");
                return Ok(());
            }

            let written = ta_goal::migrate_local_to_history(
                &local_store,
                &history_store,
                &config.workspace_root,
            )?;
            println!(
                "\nMigrated {} entr{} to .ta/velocity-history.jsonl.",
                written,
                if written == 1 { "y" } else { "ies" }
            );
            println!("Local velocity-stats.jsonl is unchanged.");
            println!("Commit .ta/velocity-history.jsonl to share with your team.");
        }
    }

    Ok(())
}

fn parse_date(s: &str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("Invalid date '{}' — expected YYYY-MM-DD", s))?;
    Ok(chrono::Utc.from_utc_datetime(&naive.and_hms_opt(0, 0, 0).unwrap()))
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
