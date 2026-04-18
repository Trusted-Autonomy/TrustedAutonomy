// events.rs -- Event system CLI: listen, hooks, tokens, routing, subscriptions.

use std::cmp::Reverse;

use clap::Subcommand;
use ta_events::router::{EventRouter, ResponseStrategy, RoutingConfig};
use ta_events::store::{EventQueryFilter, FsEventStore};
use ta_events::subscription::{Subscription, SubscriptionAction, SubscriptionFilter};
use ta_events::{EventStore, HookConfig, HookRunner, SubscriptionStore};
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
    /// Manage event routing configuration.
    Routing {
        #[command(subcommand)]
        command: RoutingCommands,
    },
    /// Manage persistent event subscriptions (v0.15.19.1).
    Subscriptions {
        #[command(subcommand)]
        command: SubscriptionsCommands,
    },
}

/// Subcommands for `ta events subscriptions`.
#[derive(Subcommand)]
pub enum SubscriptionsCommands {
    /// Create a new subscription.
    Add {
        /// Unique name for this subscription.
        #[arg(long)]
        name: String,
        /// Event types to match (repeatable; omit for all events).
        #[arg(long = "type")]
        event_types: Vec<String>,
        /// Filter by plan phase.
        #[arg(long)]
        phase: Option<String>,
        /// Action: log (default), workflow:<name>, notify:<channel,...>, webhook:<url>
        #[arg(long, default_value = "log")]
        action: String,
        /// Optional description.
        #[arg(long)]
        description: Option<String>,
    },
    /// List all subscriptions.
    List,
    /// Remove a subscription by name or ID.
    Remove {
        /// Subscription name or UUID.
        target: String,
    },
    /// Enable or disable a subscription.
    Enable {
        /// Subscription name or UUID.
        target: String,
        /// Disable instead of enabling.
        #[arg(long)]
        disable: bool,
    },
    /// Replay unprocessed events for a subscription from its cursor.
    Replay {
        /// Subscription name or UUID.
        target: String,
        /// Show what would be dispatched without updating the cursor.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum RoutingCommands {
    /// List active event responders and their strategies.
    List,
    /// Dry-run: show what would happen for a given event type.
    Test {
        /// Event type to test (e.g., "goal_failed", "draft_denied").
        event_type: String,
    },
    /// Quick override: set the strategy for an event type.
    Set {
        /// Event type to configure (e.g., "goal_failed").
        event_type: String,
        /// Strategy to use: notify, block, agent, workflow, ignore.
        strategy: String,
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
        EventsCommands::Routing { command } => handle_routing(command, config),
        EventsCommands::Subscriptions { command } => handle_subscriptions(command, config),
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
        types.sort_by_key(|e| Reverse(*e.1));
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

fn handle_routing(cmd: &RoutingCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        RoutingCommands::List => routing_list(config),
        RoutingCommands::Test { event_type } => routing_test(config, event_type),
        RoutingCommands::Set {
            event_type,
            strategy,
        } => routing_set(config, event_type, strategy),
    }
}

fn routing_list(config: &GatewayConfig) -> anyhow::Result<()> {
    let config_path = config.workspace_root.join(".ta").join("event-routing.yaml");
    let router = EventRouter::from_project(&config.workspace_root)?;

    let responders = router.responders();
    let defaults = router.defaults();

    println!("Event Routing Configuration");
    println!("{}", "=".repeat(56));
    println!("  Config: {}", config_path.display());
    if !config_path.exists() {
        println!("  (using built-in defaults — no config file found)");
    }
    println!("  Responders: {}", responders.len());
    println!();

    if responders.is_empty() {
        println!("  No responders configured. All events use the default strategy.");
        println!();
        println!("  To create a config file:");
        println!("    cp templates/event-routing.yaml .ta/event-routing.yaml");
    } else {
        let header_event = "EVENT TYPE";
        let header_strategy = "STRATEGY";
        println!("  {:<24} {:<12} FILTER", header_event, header_strategy);
        println!("  {}", "-".repeat(52));
        for r in responders {
            let filter_desc = match &r.filter {
                Some(f) => {
                    let mut parts = Vec::new();
                    if let Some(p) = &f.phase {
                        parts.push(format!("phase={}", p));
                    }
                    if let Some(a) = &f.agent_id {
                        parts.push(format!("agent={}", a));
                    }
                    if let Some(s) = &f.severity {
                        parts.push(format!("severity={}", s));
                    }
                    if parts.is_empty() {
                        "-".to_string()
                    } else {
                        parts.join(", ")
                    }
                }
                None => "-".to_string(),
            };
            println!(
                "  {:<24} {:<12} {}",
                r.event,
                r.strategy.to_string(),
                filter_desc
            );
        }
    }

    println!();
    println!(
        "  Defaults: strategy={}, max_attempts={}, escalate_after={}",
        defaults.default_strategy, defaults.max_attempts, defaults.escalate_after
    );

    Ok(())
}

fn routing_test(config: &GatewayConfig, event_type: &str) -> anyhow::Result<()> {
    let router = EventRouter::from_project(&config.workspace_root)?;
    let decision = router.test_route(event_type);

    println!("Routing Test: {}", event_type);
    println!("{}", "=".repeat(40));

    // Check if there was a matching responder.
    let matched = router.responders().iter().any(|r| r.event == event_type);

    if matched {
        println!(
            "  Matched responder: {} -> {}",
            event_type, decision.strategy
        );
    } else {
        println!("  No responder matched — using default strategy");
    }

    println!("  Strategy:    {}", decision.strategy);

    match decision.strategy {
        ResponseStrategy::Agent => {
            if let Some(agent) = &decision.agent {
                println!("  Agent:       {}", agent);
            }
            if let Some(prompt) = &decision.prompt {
                let preview: String = prompt.chars().take(60).collect();
                println!("  Prompt:      {}...", preview.trim());
            }
            println!(
                "  Approval:    {}",
                if decision.require_approval {
                    "required"
                } else {
                    "auto"
                }
            );
        }
        ResponseStrategy::Workflow => {
            if let Some(wf) = &decision.workflow {
                println!("  Workflow:    {}", wf);
            }
        }
        ResponseStrategy::Notify => {
            if !decision.channels.is_empty() {
                println!("  Channels:    {}", decision.channels.join(", "));
            } else {
                println!("  Channels:    (daemon defaults)");
            }
        }
        ResponseStrategy::Block => {
            println!("  Action:      halt pipeline, require human intervention");
        }
        ResponseStrategy::Ignore => {
            println!("  Action:      event suppressed");
        }
    }

    println!("  Max attempts: {}", decision.max_attempts);
    println!();
    println!(
        "  To change: ta events routing set {} <strategy>",
        event_type
    );

    Ok(())
}

fn routing_set(config: &GatewayConfig, event_type: &str, strategy_str: &str) -> anyhow::Result<()> {
    let strategy: ResponseStrategy = strategy_str
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{}", e))?;

    let config_path = config.workspace_root.join(".ta").join("event-routing.yaml");

    // Load existing config or create default.
    let mut routing_config = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        serde_yaml::from_str::<RoutingConfig>(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", config_path.display(), e))?
    } else {
        RoutingConfig::default()
    };

    // Find existing responder or create a new one.
    let mut found = false;
    for r in &mut routing_config.responders {
        if r.event == event_type && r.filter.is_none() {
            r.strategy = strategy.clone();
            found = true;
            break;
        }
    }

    if !found {
        routing_config.responders.push(ta_events::Responder {
            event: event_type.to_string(),
            strategy: strategy.clone(),
            filter: None,
            agent: None,
            prompt: None,
            require_approval: None,
            escalate_after: None,
            max_attempts: None,
            workflow: None,
            channels: vec![],
        });
    }

    // Validate before writing.
    EventRouter::new(routing_config.clone())?;

    // Write back.
    std::fs::create_dir_all(config_path.parent().unwrap())?;
    let yaml = serde_yaml::to_string(&routing_config)?;
    std::fs::write(&config_path, yaml)?;

    println!("Updated event routing: {} -> {}", event_type, strategy);
    println!("  Config written to {}", config_path.display());
    println!("  Run 'ta events routing test {}' to verify.", event_type);

    Ok(())
}

// ---------------------------------------------------------------------------
// Subscription handlers
// ---------------------------------------------------------------------------

fn handle_subscriptions(cmd: &SubscriptionsCommands, config: &GatewayConfig) -> anyhow::Result<()> {
    match cmd {
        SubscriptionsCommands::Add {
            name,
            event_types,
            phase,
            action,
            description,
        } => subscriptions_add(
            config,
            name,
            event_types,
            phase.as_deref(),
            action,
            description.as_deref(),
        ),
        SubscriptionsCommands::List => subscriptions_list(config),
        SubscriptionsCommands::Remove { target } => subscriptions_remove(config, target),
        SubscriptionsCommands::Enable { target, disable } => {
            subscriptions_enable(config, target, !disable)
        }
        SubscriptionsCommands::Replay { target, dry_run } => {
            subscriptions_replay(config, target, *dry_run)
        }
    }
}

/// Parse the `--action` string into a `SubscriptionAction`.
///
/// Supported formats:
/// - `log`
/// - `workflow:<name>`
/// - `notify:<channel>[,<channel>...]`
/// - `webhook:<url>`
fn parse_action(action: &str) -> anyhow::Result<SubscriptionAction> {
    if action == "log" {
        return Ok(SubscriptionAction::Log);
    }
    if let Some(name) = action.strip_prefix("workflow:") {
        return Ok(SubscriptionAction::RunWorkflow {
            workflow: name.to_string(),
            params: Default::default(),
        });
    }
    if let Some(channels_str) = action.strip_prefix("notify:") {
        let channels: Vec<String> = channels_str
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        return Ok(SubscriptionAction::Notify {
            channels,
            template: None,
        });
    }
    if let Some(url) = action.strip_prefix("webhook:") {
        return Ok(SubscriptionAction::Webhook {
            url: url.to_string(),
            headers: Default::default(),
            secret: None,
        });
    }
    anyhow::bail!(
        "unrecognised action format '{}'. \
         Expected: log | workflow:<name> | notify:<channel,...> | webhook:<url>",
        action
    )
}

/// Resolve a subscription by name or UUID string.
fn resolve_subscription(store: &SubscriptionStore, target: &str) -> anyhow::Result<Subscription> {
    // Try UUID first.
    if let Ok(id) = target.parse::<uuid::Uuid>() {
        return store
            .get(id)?
            .ok_or_else(|| anyhow::anyhow!("no subscription with ID {}", id));
    }
    // Fall back to name lookup.
    store
        .get_by_name(target)?
        .ok_or_else(|| anyhow::anyhow!("no subscription named '{}'", target))
}

fn subscriptions_add(
    config: &GatewayConfig,
    name: &str,
    event_types: &[String],
    phase: Option<&str>,
    action_str: &str,
    description: Option<&str>,
) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    let store = SubscriptionStore::new(&ta_dir);

    let filter = build_filter(event_types, phase)?;
    let action = parse_action(action_str)?;

    let mut sub = Subscription::new(name, filter, action);
    if let Some(desc) = description {
        sub.description = Some(desc.to_string());
    }
    let sub_id = sub.id;

    store.add(sub)?;

    println!("Subscription created: {} ({})", name, sub_id);
    println!("  Action:  {}", action_str);
    if !event_types.is_empty() {
        println!("  Filter:  types = [{}]", event_types.join(", "));
    }
    if let Some(p) = phase {
        println!("  Filter:  phase = {}", p);
    }
    println!();
    println!("  Run 'ta events subscriptions list' to see all subscriptions.");
    println!(
        "  Run 'ta events subscriptions replay {}' to catch up on missed events.",
        name
    );
    Ok(())
}

/// Build a `SubscriptionFilter` from CLI args.
fn build_filter(event_types: &[String], phase: Option<&str>) -> anyhow::Result<SubscriptionFilter> {
    let mut filters: Vec<SubscriptionFilter> = Vec::new();

    if !event_types.is_empty() {
        filters.push(SubscriptionFilter::ByTypes {
            types: event_types.to_vec(),
        });
    }
    if let Some(p) = phase {
        filters.push(SubscriptionFilter::ByPhase {
            phase: p.to_string(),
        });
    }

    match filters.len() {
        0 => Ok(SubscriptionFilter::All),
        1 => Ok(filters.remove(0)),
        _ => Ok(SubscriptionFilter::And { filters }),
    }
}

fn subscriptions_list(config: &GatewayConfig) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    let store = SubscriptionStore::new(&ta_dir);
    let subs = store.list()?;

    println!("Event Subscriptions");
    println!("{}", "=".repeat(60));

    if subs.is_empty() {
        println!("  No subscriptions. Create one with:");
        println!();
        println!("    ta events subscriptions add --name <name> --type goal_failed --action log");
        println!();
        println!("  See 'ta events subscriptions add --help' for all options.");
        return Ok(());
    }

    println!(
        "  {:<4} {:<20} {:<8} {:<24} CURSOR",
        "ID", "NAME", "ENABLED", "ACTION"
    );
    println!("  {}", "-".repeat(70));

    for sub in &subs {
        let id_short = sub.id.to_string();
        let id_display: String = id_short.chars().take(8).collect();
        let cursor_display = sub
            .cursor
            .map(|ts| ts.format("%Y-%m-%dT%H:%M").to_string())
            .unwrap_or_else(|| "(never)".to_string());

        println!(
            "  {:<4} {:<20} {:<8} {:<24} {}",
            id_display,
            sub.name.chars().take(20).collect::<String>(),
            if sub.enabled { "yes" } else { "no" },
            sub.action.describe().chars().take(24).collect::<String>(),
            cursor_display,
        );
    }

    println!();
    println!("  {} subscription(s).", subs.len());
    Ok(())
}

fn subscriptions_remove(config: &GatewayConfig, target: &str) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    let store = SubscriptionStore::new(&ta_dir);
    let sub = resolve_subscription(&store, target)?;
    let id = sub.id;
    let name = sub.name.clone();

    store.remove(id)?;
    println!("Removed subscription '{}' ({}).", name, id);
    Ok(())
}

fn subscriptions_enable(config: &GatewayConfig, target: &str, enable: bool) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    let store = SubscriptionStore::new(&ta_dir);
    let sub = resolve_subscription(&store, target)?;

    store.set_enabled(sub.id, enable)?;
    println!(
        "Subscription '{}' {}.",
        sub.name,
        if enable { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn subscriptions_replay(config: &GatewayConfig, target: &str, dry_run: bool) -> anyhow::Result<()> {
    let ta_dir = config.workspace_root.join(".ta");
    let sub_store = SubscriptionStore::new(&ta_dir);
    let sub = resolve_subscription(&sub_store, target)?;

    let ev_store = FsEventStore::new(ta_dir.join("events"));
    let filter = ta_events::store::EventQueryFilter {
        since: sub.cursor,
        ..Default::default()
    };
    let events = ev_store.query(&filter)?;

    let matching: Vec<_> = events.iter().filter(|e| sub.matches(e)).collect();

    println!(
        "Replay: subscription '{}' (cursor: {})",
        sub.name,
        sub.cursor
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_else(|| "(none)".to_string())
    );
    println!("{}", "=".repeat(60));

    if matching.is_empty() {
        println!("  No unprocessed events found.");
        return Ok(());
    }

    for envelope in &matching {
        println!(
            "  {} {} {}",
            envelope.timestamp.format("%Y-%m-%dT%H:%M:%S"),
            envelope.event_type,
            envelope.id
        );
    }

    println!();
    println!("  {} event(s) pending dispatch.", matching.len());

    if dry_run {
        println!("  (dry-run — cursor not updated)");
    } else {
        // Advance cursor to the latest event timestamp.
        if let Some(last) = matching.last() {
            sub_store.update_cursor(sub.id, last.timestamp)?;
            println!("  Cursor advanced to {}.", last.timestamp.to_rfc3339());
        }
    }

    Ok(())
}
