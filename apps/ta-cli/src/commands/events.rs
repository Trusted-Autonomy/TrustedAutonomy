// events.rs -- Event system CLI: listen, hooks, tokens.

use clap::Subcommand;
use ta_events::store::{EventQueryFilter, FsEventStore};
use ta_events::{EventStore, HookConfig, HookRunner};
use ta_mcp_gateway::GatewayConfig;

#[derive(Subcommand)]
pub enum EventsCommands {
    /// Stream events to stdout as NDJSON (one JSON object per line).
    Listen {
        /// Filter by event type (repeatable).
        #[arg(long)]
        filter: Vec<String>,
        /// Filter by goal ID.
        #[arg(long)]
        goal: Option<String>,
        /// Maximum number of events to show.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show event log statistics.
    Stats,
    /// Show configured event hooks.
    Hooks,
    /// Prune old event log files (v0.10.15).
    Prune {
        /// Remove events older than this many days (default: 30).
        #[arg(long, default_value = "30")]
        older_than_days: u64,
        /// Show what would be removed without deleting.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn execute(cmd: &EventsCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        EventsCommands::Listen {
            filter,
            goal,
            limit,
        } => listen_events(config, filter, goal.as_deref(), *limit),
        EventsCommands::Stats => show_stats(config),
        EventsCommands::Hooks => show_hooks(config),
        EventsCommands::Prune {
            older_than_days,
            dry_run,
        } => prune_events(config, *older_than_days, *dry_run),
    }
}

fn listen_events(
    config: &GatewayConfig,
    type_filters: &[String],
    goal_id: Option<&str>,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    let events_dir = config.workspace_root.join(".ta").join("events");
    let store = FsEventStore::new(&events_dir);

    let goal_uuid = match goal_id {
        Some(id) => Some(
            id.parse::<uuid::Uuid>()
                .map_err(|_| anyhow::anyhow!("invalid goal ID: {}", id))?,
        ),
        None => None,
    };

    let filter = EventQueryFilter {
        event_types: type_filters.to_vec(),
        goal_id: goal_uuid,
        limit,
        ..Default::default()
    };

    let events = store.query(&filter)?;

    if events.is_empty() {
        eprintln!("No events found. Events are logged to .ta/events/ during TA operations.");
        return Ok(());
    }

    for envelope in &events {
        let json = serde_json::to_string(envelope)?;
        println!("{}", json);
    }

    Ok(())
}

fn show_stats(config: &GatewayConfig) -> anyhow::Result<()> {
    let events_dir = config.workspace_root.join(".ta").join("events");
    let store = FsEventStore::new(&events_dir);

    let count = store.count()?;
    println!("Event Log Statistics");
    println!("{}", "=".repeat(40));
    println!("  Total events: {}", count);
    println!("  Events dir:   {}", events_dir.display());

    // Show breakdown by type.
    let all = store.query(&EventQueryFilter::default())?;
    if !all.is_empty() {
        let mut by_type: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for e in &all {
            *by_type.entry(e.event_type.clone()).or_insert(0) += 1;
        }
        println!();
        println!("  By type:");
        let mut types: Vec<_> = by_type.iter().collect();
        types.sort_by(|a, b| b.1.cmp(a.1));
        for (t, count) in types {
            println!("    {:<24} {}", t, count);
        }
    }

    Ok(())
}

fn prune_events(config: &GatewayConfig, older_than_days: u64, dry_run: bool) -> anyhow::Result<()> {
    let events_dir = config.workspace_root.join(".ta").join("events");
    let store = FsEventStore::new(&events_dir);

    let cutoff = chrono::Utc::now() - chrono::Duration::days(older_than_days as i64);
    let cutoff_date = cutoff.date_naive();

    if dry_run {
        // Count files that would be removed.
        let mut count = 0;
        if events_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&events_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(file_date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
                            if file_date < cutoff_date {
                                eprintln!("  would remove: {}", path.display());
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
        println!(
            "Dry run: {} event log file(s) older than {} days (before {}) would be removed.",
            count, older_than_days, cutoff_date
        );
    } else {
        let removed = store.prune(cutoff)?;
        println!(
            "Pruned {} event log file(s) older than {} days (before {}).",
            removed, older_than_days, cutoff_date
        );
        if removed == 0 {
            println!("  No event files eligible for pruning.");
        }
    }

    Ok(())
}

fn show_hooks(config: &GatewayConfig) -> anyhow::Result<()> {
    let hooks_path = config.workspace_root.join(".ta").join("hooks.toml");
    let hook_config = HookConfig::load(&hooks_path)?;
    let runner = HookRunner::new(hook_config);

    let count = runner.hook_count();
    println!("Event Hooks Configuration");
    println!("{}", "=".repeat(40));
    println!("  Config file: {}", hooks_path.display());
    println!("  Total hooks: {}", count);

    if count > 0 {
        let events = runner.configured_events();
        println!();
        println!("  Configured event types:");
        for event in &events {
            println!("    - {}", event);
        }
    } else {
        println!();
        println!("  No hooks configured. Add hooks to .ta/hooks.toml:");
        println!();
        println!("    [[hooks]]");
        println!("    event = \"draft_approved\"");
        println!("    command = \"echo 'Draft approved!'\"");
    }

    Ok(())
}
