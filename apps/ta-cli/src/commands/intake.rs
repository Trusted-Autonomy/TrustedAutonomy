// intake.rs — `ta intake` CLI commands for the trigger layer (`ta-intake`, v0.17.0.12.19).
//
// Thin glue over the `ta-intake` library crate: `ta-intake` itself only
// produces normalized `TriggerEvent`s (§13/§13.1, no CLI/daemon glue by
// design). This module is where "what to do with a fired event" lives for
// now — direct goal creation (shelling to `ta run`, the same pattern
// `ta workflow run email-manager` already uses) or appending to a queue —
// selected per-trigger by the config's `dispatch` field, not hardcoded here.
// `ta-brain` (v0.17.0.12.20) will own this dispatch decision going forward;
// this CLI command is the minimal end-to-end demonstration required by this
// phase's plan item 2 ("each producing a TriggerEvent that results in a
// real goal being created").
//
// Provides:
//   - `ta intake list`               — show configured trigger types
//   - `ta intake fire <type>`        — poll one trigger type once, dispatch fired events
//   - `ta intake queue`              — show events queued for later batch processing

use std::io::Write as _;
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
}

pub fn run_intake(project_root: &Path, command: &IntakeCommands) -> anyhow::Result<()> {
    match command {
        IntakeCommands::List => list(project_root),
        IntakeCommands::Fire {
            trigger_type,
            dry_run,
        } => fire(project_root, trigger_type, *dry_run),
        IntakeCommands::Queue => show_queue(project_root),
    }
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

fn queue_path(project_root: &Path) -> PathBuf {
    project_root.join(".ta").join("intake-queue.jsonl")
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
                dispatch_direct(event)?;
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
                enqueue(project_root, &events)?;
                println!(
                    "  appended {} event(s) to {}",
                    events.len(),
                    queue_path(project_root).display()
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
fn dispatch_direct(event: &TriggerEvent) -> anyhow::Result<()> {
    let objective = format!(
        "# Triggered goal\n\n\
         Fired by trigger type `{}` (source: {}) at {}.\n\n\
         ## Normalized event payload\n\n```json\n{}\n```\n",
        event.trigger_type,
        event.source,
        event.occurred_at.to_rfc3339(),
        serde_json::to_string_pretty(&event.payload).unwrap_or_default()
    );

    let tmp_path = std::env::temp_dir().join(format!("ta-intake-objective-{}.md", event.id));
    std::fs::write(&tmp_path, &objective)?;

    let mut cmd = std::process::Command::new("ta");
    cmd.arg("run")
        .arg(&event.suggested_goal_title)
        .arg("--headless")
        .arg("--objective-file")
        .arg(&tmp_path);

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

fn enqueue(project_root: &Path, events: &[TriggerEvent]) -> anyhow::Result<()> {
    let path = queue_path(project_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for event in events {
        writeln!(file, "{}", serde_json::to_string(event)?)?;
    }
    Ok(())
}

fn show_queue(project_root: &Path) -> anyhow::Result<()> {
    let path = queue_path(project_root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("No queued events ({} does not exist yet).", path.display());
        return Ok(());
    };
    let events: Vec<TriggerEvent> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
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
