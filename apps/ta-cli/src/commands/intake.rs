// intake.rs — `ta intake` CLI commands for the trigger layer (`ta-intake`, v0.17.0.12.19).
//
// Thin glue over the `ta-intake` library crate: `ta-intake` itself only
// produces normalized `TriggerEvent`s (§13/§13.1, no CLI/daemon glue by
// design). This module is where "what to do with a fired event" lives —
// direct goal creation (shelling to `ta run`, the same pattern
// `ta workflow run email-manager` already uses) or appending to the batch
// queue (`ta_intake::queue`) — selected per-trigger by the config's
// `dispatch` field, not hardcoded here. Routing (team/persona/agent/
// security/priority) is delegated to `ta_brain::route()` (v0.17.0.12.20) for
// both the direct-dispatch and queued/coordinated paths, so this module
// never reimplements routing logic itself.
//
// Provides:
//   - `ta intake list`               — show configured trigger types
//   - `ta intake fire <type>`        — poll one trigger type once, dispatch fired events
//   - `ta intake queue`              — show events queued for later batch processing
//   - `ta intake coordinate`         — team-coordinator: priority-ordered recommendations
//                                       for the queue, with `--dispatch` to act on them
//   - `ta intake routing`            — show logged `ta-brain` routing decisions

use std::path::{Path, PathBuf};

use chrono::Utc;
use clap::Subcommand;
use ta_intake::{
    discover_triggers, find_trigger, Dispatch, EmailTriggerSource, ScheduleTriggerSource,
    TriggerEvent, TriggerSource,
};

#[derive(Subcommand)]
pub enum IntakeCommands {
    /// List configured trigger types from `.ta/triggers/*.toml`.
    List,
    /// Poll one trigger type once and dispatch any fired events per its
    /// configured `dispatch` mode: `direct` creates a goal immediately for
    /// each event, `queue` appends events to `.ta/intake-queue.jsonl` for
    /// later batch processing.
    Fire {
        /// Trigger type to fire (matches `.ta/triggers/<type>.toml`'s `type` field).
        trigger_type: String,
        /// Show what would fire without creating a goal, writing the queue, or advancing the watermark.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show events queued for later batch processing (`.ta/intake-queue.jsonl`).
    Queue,
    /// Show `ta-brain` routing decisions logged for triggered goals
    /// (`.ta/routing-decisions.jsonl`, v0.17.0.12.20).
    Routing {
        /// Show only the most recent N decisions.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Team-coordinator: priority-ordered recommendations for the queue
    /// (`ta_advisor::coordinator`, v0.17.0.12.20). Read-only by default.
    Coordinate {
        /// Dispatch (create goals for + dequeue) every recommendation whose
        /// resolved security tier is "auto"; leave the rest queued.
        #[arg(long)]
        dispatch: bool,
    },
}

pub fn run_intake(project_root: &Path, command: &IntakeCommands) -> anyhow::Result<()> {
    match command {
        IntakeCommands::List => list(project_root),
        IntakeCommands::Fire {
            trigger_type,
            dry_run,
        } => fire(project_root, trigger_type, *dry_run),
        IntakeCommands::Queue => show_queue(project_root),
        IntakeCommands::Routing { limit } => show_routing(project_root, *limit),
        IntakeCommands::Coordinate { dispatch } => coordinate(project_root, *dispatch),
    }
}

fn show_routing(project_root: &Path, limit: Option<usize>) -> anyhow::Result<()> {
    let mut decisions = super::run::read_routing_decisions(project_root);
    if decisions.is_empty() {
        println!(
            "No routing decisions recorded yet. Decisions are logged to \
             .ta/routing-decisions.jsonl by `ta run` and `ta intake fire`."
        );
        return Ok(());
    }
    if let Some(n) = limit {
        let skip = decisions.len().saturating_sub(n);
        decisions.drain(..skip);
    }
    for d in &decisions {
        println!(
            "team={} persona={} agent={} security={} priority={} workload={} ({:.0}% confidence)",
            d.team,
            d.persona.as_deref().unwrap_or("-"),
            d.agent,
            d.security_tier,
            d.priority,
            d.workload_type,
            d.workload_confidence * 100.0
        );
        for line in &d.rationale {
            println!("  {line}");
        }
        println!();
    }
    Ok(())
}

fn list(project_root: &Path) -> anyhow::Result<()> {
    let triggers = discover_triggers(project_root);
    if triggers.is_empty() {
        println!(
            "No trigger configs found under {}/.ta/triggers/.\n\
             Add one, e.g. .ta/triggers/schedule.toml with `type = \"schedule\"`.\n\
             See docs/USAGE.md \"Trigger Layer\" for the full config reference.",
            project_root.display()
        );
        return Ok(());
    }
    println!(
        "{:<16} {:<9} {:<8} description",
        "type", "enabled", "dispatch"
    );
    for t in &triggers {
        let dispatch = match t.manifest.dispatch {
            Dispatch::Direct => "direct",
            Dispatch::Queue => "queue",
        };
        println!(
            "{:<16} {:<9} {:<8} {}",
            t.manifest.trigger_type,
            t.manifest.enabled,
            dispatch,
            t.manifest.description.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn watermark_path(project_root: &Path, trigger_type: &str) -> PathBuf {
    project_root
        .join(".ta")
        .join("triggers")
        .join(".state")
        .join(format!("{trigger_type}.watermark"))
}

fn read_watermark(project_root: &Path, trigger_type: &str) -> Option<String> {
    std::fs::read_to_string(watermark_path(project_root, trigger_type))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_watermark(project_root: &Path, trigger_type: &str) -> anyhow::Result<()> {
    let path = watermark_path(project_root, trigger_type);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, Utc::now().to_rfc3339())?;
    Ok(())
}

fn build_source(
    trigger_type: &str,
    manifest: ta_intake::TriggerManifest,
    project_root: &Path,
) -> anyhow::Result<Box<dyn TriggerSource>> {
    match trigger_type {
        "schedule" => Ok(Box::new(ScheduleTriggerSource::new(manifest))),
        "inbound-email" => {
            let source = EmailTriggerSource::from_plugin(manifest, project_root)?;
            Ok(Box::new(source))
        }
        other => anyhow::bail!(
            "No built-in TriggerSource implementation for type '{other}'.\n\
             Built-in types: schedule, inbound-email.\n\
             Community trigger types need their own `TriggerSource` registered \
             (planned for the `ta-brain` routing layer, v0.17.0.12.20)."
        ),
    }
}

fn fire(project_root: &Path, trigger_type: &str, dry_run: bool) -> anyhow::Result<()> {
    let discovered = find_trigger(trigger_type, project_root).ok_or_else(|| {
        anyhow::anyhow!(
            "No trigger config found for type '{trigger_type}' at {}/.ta/triggers/{trigger_type}.toml.\n\
             Run `ta intake list` to see configured trigger types, or create one — \
             see docs/USAGE.md \"Trigger Layer\".",
            project_root.display()
        )
    })?;

    if !discovered.manifest.enabled {
        anyhow::bail!(
            "Trigger '{trigger_type}' is disabled (enabled = false in {}).\n\
             Set `enabled = true` to fire it.",
            discovered.config_path.display()
        );
    }

    let dispatch = discovered.manifest.dispatch;
    let manifest = discovered.manifest.clone();
    let source = build_source(trigger_type, discovered.manifest, project_root)?;
    let watermark = read_watermark(project_root, trigger_type);

    let events = source
        .poll(watermark.as_deref())
        .map_err(|e| anyhow::anyhow!("Polling trigger '{trigger_type}' failed: {e}"))?;

    if events.is_empty() {
        println!(
            "[intake] trigger '{trigger_type}': no new events since {}.",
            watermark.as_deref().unwrap_or("the beginning")
        );
        return Ok(());
    }

    println!(
        "[intake] trigger '{trigger_type}': {} event(s) fired, dispatch = {}{}.",
        events.len(),
        match dispatch {
            Dispatch::Direct => "direct",
            Dispatch::Queue => "queue",
        },
        if dry_run { " (dry run)" } else { "" }
    );

    match dispatch {
        Dispatch::Direct => {
            for event in &events {
                if dry_run {
                    println!(
                        "  would create goal: \"{}\" (dedupe_key={:?})",
                        event.suggested_goal_title, event.dedupe_key
                    );
                    continue;
                }
                dispatch_direct(event, &manifest, project_root)?;
            }
        }
        Dispatch::Queue => {
            if dry_run {
                for event in &events {
                    println!(
                        "  would queue: \"{}\" (dedupe_key={:?})",
                        event.suggested_goal_title, event.dedupe_key
                    );
                }
            } else {
                ta_intake::append_to_queue(project_root, &events)?;
                println!(
                    "  appended {} event(s) to {}",
                    events.len(),
                    ta_intake::queue_path(project_root).display()
                );
            }
        }
    }

    if !dry_run {
        write_watermark(project_root, trigger_type)?;
    }

    Ok(())
}

/// Create a goal directly for a fired event by shelling to `ta run`, the
/// same mechanism `ta workflow run email-manager`'s `TaReplyGoalRunner`
/// uses for its reply-drafting goals.
///
/// Routes the event through `ta_brain::route()` first (v0.17.0.12.20) — the
/// same function the explicit `ta run` path calls
/// (`commands::run::route_and_log`) — so the triggered path never
/// reimplements team/persona/agent/security/priority resolution. The
/// resolved decision is logged/persisted by `route()`'s caller contract
/// exactly like the explicit path, then passed through to the shelled `ta
/// run` invocation as explicit flags.
fn dispatch_direct(
    event: &TriggerEvent,
    manifest: &ta_intake::TriggerManifest,
    project_root: &Path,
) -> anyhow::Result<()> {
    let input = ta_brain::RoutingInput::Trigger(
        ta_brain::TriggerRoutingInput::from_event_and_manifest(event.clone(), manifest),
    );
    let decision = ta_brain::route(&input, project_root);
    print_routing_decision(&decision);
    execute_routed_goal(event, &decision)
}

fn print_routing_decision(decision: &ta_brain::RoutingDecision) {
    println!(
        "[routing] team={} persona={} agent={} security={} priority={} workload={} ({:.0}% confidence)",
        decision.team,
        decision.persona.as_deref().unwrap_or("-"),
        decision.agent,
        decision.security_tier,
        decision.priority,
        decision.workload_type,
        decision.workload_confidence * 100.0
    );
    for line in &decision.rationale {
        println!("  rationale: {line}");
    }
}

/// Shell out to `ta run` for an already-routed event, passing the resolved
/// `RoutingDecision` through as explicit flags. Shared by the direct-dispatch
/// path (`dispatch_direct`) and the team-coordinator's `--dispatch` path
/// (`coordinate`) — one execution mechanism, not reimplemented per caller.
fn execute_routed_goal(
    event: &TriggerEvent,
    decision: &ta_brain::RoutingDecision,
) -> anyhow::Result<()> {
    let objective = format!(
        "# Triggered goal\n\n\
         Fired by trigger type `{}` (source: {}) at {}.\n\n\
         Routed via ta-brain: team={}, persona={}, security={}, priority={}, workload={}.\n\n\
         ## Normalized event payload\n\n```json\n{}\n```\n",
        event.trigger_type,
        event.source,
        event.occurred_at.to_rfc3339(),
        decision.team,
        decision.persona.as_deref().unwrap_or("none"),
        decision.security_tier,
        decision.priority,
        decision.workload_type,
        serde_json::to_string_pretty(&event.payload).unwrap_or_default()
    );

    let tmp_path = std::env::temp_dir().join(format!("ta-intake-objective-{}.md", event.id));
    std::fs::write(&tmp_path, &objective)?;

    let mut cmd = std::process::Command::new("ta");
    cmd.arg("run")
        .arg(&event.suggested_goal_title)
        .arg("--headless")
        .arg("--agent")
        .arg(&decision.agent)
        .arg("--team")
        .arg(decision.team.as_str())
        .arg("--security")
        .arg(decision.security_tier.to_string())
        .arg("--priority")
        .arg(decision.priority.to_string())
        .arg("--workload")
        .arg(&decision.workload_type)
        .arg("--objective-file")
        .arg(&tmp_path);
    if let Some(persona) = &decision.persona {
        cmd.arg("--persona").arg(persona);
    }

    let output = cmd.output().map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::anyhow!(
            "Failed to invoke 'ta run' for triggered goal \"{}\": {e}\n\
             Is ta installed and on PATH?",
            event.suggested_goal_title
        )
    })?;
    let _ = std::fs::remove_file(&tmp_path);

    if output.status.success() {
        println!(
            "  created goal: \"{}\" (exit 0)",
            event.suggested_goal_title
        );
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "ta run failed (exit {}) for triggered goal \"{}\"\nstdout: {}\nstderr: {}",
            output.status,
            event.suggested_goal_title,
            stdout.trim(),
            stderr.trim()
        )
    }
}

/// `ta intake coordinate` — the team-coordinator surface (v0.17.0.12.20):
/// a priority-ordered set of recommendations for `.ta/intake-queue.jsonl`'s
/// current contents, extending the existing Advisor (`ta_advisor::coordinator`
/// — see that module for the "extend Advisor, not a new role" rationale).
///
/// Without `--dispatch`: read-only, prints recommendations + rationale only.
/// With `--dispatch`: dispatches (creates a goal for, then dequeues) every
/// recommendation whose resolved `security_tier` is `Auto`; everything else
/// is left in the queue for a human to review/promote via `ta intake fire`.
fn coordinate(project_root: &Path, dispatch: bool) -> anyhow::Result<()> {
    let report = ta_advisor::build_report(project_root);
    if report.recommendations.is_empty() {
        println!(
            "No queued events to coordinate ({} is empty or missing).",
            ta_intake::queue_path(project_root).display()
        );
        return Ok(());
    }

    println!(
        "{} queued event(s), priority-ordered:",
        report.recommendations.len()
    );
    for rec in &report.recommendations {
        print_routing_decision(&rec.decision);
        println!(
            "  event: \"{}\" (auto_dispatch_eligible={})",
            rec.event.suggested_goal_title, rec.auto_dispatch_eligible
        );
    }

    if !dispatch {
        println!(
            "\n(read-only — pass --dispatch to create goals for auto_dispatch_eligible=true \
             recommendations; everything else needs `ta intake fire` or manual promotion)"
        );
        return Ok(());
    }

    let mut remaining = Vec::new();
    for rec in report.recommendations {
        if rec.auto_dispatch_eligible {
            execute_routed_goal(&rec.event, &rec.decision)?;
        } else {
            println!(
                "  left in queue: \"{}\" (security_tier={} — needs human review)",
                rec.event.suggested_goal_title, rec.decision.security_tier
            );
            remaining.push(rec.event);
        }
    }
    ta_intake::write_queue(project_root, &remaining)?;
    Ok(())
}

fn show_queue(project_root: &Path) -> anyhow::Result<()> {
    let path = ta_intake::queue_path(project_root);
    if !path.exists() {
        println!("No queued events ({} does not exist yet).", path.display());
        return Ok(());
    }
    let events = ta_intake::read_queue(project_root);
    if events.is_empty() {
        println!("No queued events.");
        return Ok(());
    }
    println!("{} queued event(s):", events.len());
    for event in &events {
        println!(
            "  [{}] {} — \"{}\" (occurred_at={})",
            event.trigger_type,
            event.source,
            event.suggested_goal_title,
            event.occurred_at.to_rfc3339()
        );
    }
    Ok(())
}
