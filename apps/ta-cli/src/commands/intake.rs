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
    TriggerEvent, TriggerSource, WebhookTriggerSource,
};

#[derive(Subcommand, Debug)]
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
        "webhook" => Ok(Box::new(WebhookTriggerSource::new(manifest, project_root))),
        other => anyhow::bail!(
            "No built-in TriggerSource implementation for type '{other}'.\n\
             Built-in types: schedule, inbound-email, webhook.\n\
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
    // Build-milestone automation (v0.17.0.12.31): a matched "PR merged"
    // `webhook` trigger event doesn't go through the generic ta-brain
    // routing + headless-goal path — it drives the specific PR-merged →
    // pull/build/next-phase sequence PLAN.md item 2 describes, matching
    // exactly what this session performed by hand for phases 12.28-12.30.
    if event.trigger_type == "webhook" {
        return execute_pr_merged_continuation(event, manifest, project_root);
    }

    let input = ta_brain::RoutingInput::Trigger(
        ta_brain::TriggerRoutingInput::from_event_and_manifest(event.clone(), manifest),
    );
    let decision = ta_brain::route(&input, project_root);
    print_routing_decision(&decision);
    execute_routed_goal(event, &decision)
}

/// Default install/build step run after pulling a merged build-milestone PR,
/// before resolving and launching the next plan phase — this repo's own
/// dev-loop build+install+restart sequence (PLAN.md v0.17.0.12.31 item 2b).
/// Override per-project via `.ta/triggers/webhook.toml`'s `build_command`
/// setting.
const DEFAULT_BUILD_COMMAND: &str =
    "./dev cargo build --release -p ta-cli -p ta-daemon && bash install_local.sh && ta daemon restart";

/// React to a matched PR-merged `webhook` trigger event: pull the merged
/// branch, rebuild/install, resolve the next pending PLAN.md phase (via the
/// dependency-graph-aware [`super::plan::next_actionable_phase_id`], not a
/// document-position heuristic), and launch it — the automated equivalent of
/// this session's manual "apply, watch CI, rebuild, launch next phase" loop.
///
/// A no-op (not an error) when there is no next pending phase: the milestone
/// chain is complete.
fn execute_pr_merged_continuation(
    event: &TriggerEvent,
    manifest: &ta_intake::TriggerManifest,
    project_root: &Path,
) -> anyhow::Result<()> {
    let source_phase = event.payload["source_phase"].as_str().unwrap_or("unknown");
    let base_branch = event.payload["base_branch"].as_str().unwrap_or("main");

    println!(
        "[build-milestone] PR #{} merged (closes {}) — continuing the phase chain.",
        event.payload["pr_number"], source_phase
    );

    println!("[build-milestone] git pull origin {base_branch}");
    run_shell_checked(
        project_root,
        &format!("git pull origin {base_branch}"),
        "git pull",
    )?;

    let build_command = manifest
        .get_str("build_command")
        .unwrap_or(DEFAULT_BUILD_COMMAND);
    println!("[build-milestone] build command: {build_command}");
    run_shell_checked(project_root, build_command, "build/install command")?;

    let phases = super::plan::load_plan(project_root)?;
    match super::plan::next_actionable_phase_id(&phases) {
        None => {
            println!(
                "[build-milestone] no next pending phase after {source_phase} — \
                 milestone complete, nothing to launch."
            );
            Ok(())
        }
        Some(next_phase) => {
            println!("[build-milestone] launching next phase: {next_phase}");
            let ta_bin = std::env::current_exe().map_err(|e| {
                anyhow::anyhow!("Could not determine ta binary path for subprocess invocation: {e}")
            })?;
            let status = std::process::Command::new(&ta_bin)
                .arg("run")
                .arg(&next_phase)
                .arg("--accept-terms")
                .current_dir(project_root)
                .status()
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to launch 'ta run \"{next_phase}\" --accept-terms': {e}"
                    )
                })?;
            if !status.success() {
                anyhow::bail!(
                    "'ta run \"{next_phase}\" --accept-terms' exited with {}. \
                     Check the goal's logs, fix the issue, and re-run \
                     `ta intake fire webhook` to retry once resolved.",
                    status
                );
            }
            println!("[build-milestone] phase {next_phase} launched successfully.");
            Ok(())
        }
    }
}

/// Run a shell command in `project_root`, streaming output, and bail with an
/// actionable error (naming the failed step and the exact command, per the
/// Observability Mandate) on non-zero exit.
fn run_shell_checked(project_root: &Path, command: &str, step_name: &str) -> anyhow::Result<()> {
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let shell_flag = if cfg!(windows) { "/C" } else { "-c" };
    let status = std::process::Command::new(shell)
        .arg(shell_flag)
        .arg(command)
        .current_dir(project_root)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to spawn {step_name} ('{command}'): {e}"))?;
    if !status.success() {
        anyhow::bail!(
            "{step_name} failed (exit {}). Command: `{}`\n\
             Re-run manually in {} to see full output, fix the issue, then \
             re-fire the trigger: `ta intake fire webhook`.",
            status,
            command,
            project_root.display()
        );
    }
    Ok(())
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
/// `AutoEligible` recommendation. `NeedsClarification` recommendations
/// (v0.17.0.12.23) get exactly one clarifying question first — via the same
/// `ta_ask_human`-backed headless-agent mechanism `ta advisor create` uses —
/// then are dispatched if the re-routed decision clears `Auto`, or left in
/// the queue otherwise. `NeedsReview` recommendations are always left in the
/// queue for a human to review/promote via `ta intake fire`.
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
            "  event: \"{}\" (outcome={:?})",
            rec.event.suggested_goal_title, rec.outcome
        );
    }

    if !dispatch {
        println!(
            "\n(read-only — pass --dispatch to create goals for auto-eligible recommendations; \
             needs-clarification recommendations get one clarifying question first; \
             everything else needs `ta intake fire` or manual promotion)"
        );
        return Ok(());
    }

    let mut remaining = Vec::new();
    for rec in report.recommendations {
        match rec.outcome {
            ta_advisor::RecommendationOutcome::AutoEligible => {
                execute_routed_goal(&rec.event, &rec.decision)?;
            }
            ta_advisor::RecommendationOutcome::NeedsReview => {
                println!(
                    "  left in queue: \"{}\" (security_tier={} — needs human review)",
                    rec.event.suggested_goal_title, rec.decision.security_tier
                );
                remaining.push(rec.event);
            }
            ta_advisor::RecommendationOutcome::NeedsClarification => {
                match clarify_and_reroute(project_root, &rec.event, &rec.decision) {
                    Some((clarified_event, clarified_decision)) => {
                        if clarified_decision.security_tier
                            == ta_session::workflow_session::AdvisorSecurity::Auto
                        {
                            execute_routed_goal(&clarified_event, &clarified_decision)?;
                        } else {
                            println!(
                                "  left in queue after clarification: \"{}\" (security_tier={} \
                                 — needs human review)",
                                clarified_event.suggested_goal_title,
                                clarified_decision.security_tier
                            );
                            remaining.push(clarified_event);
                        }
                    }
                    None => {
                        println!(
                            "  left in queue: \"{}\" (no clarification answer — needs human review)",
                            rec.event.suggested_goal_title
                        );
                        remaining.push(rec.event);
                    }
                }
            }
        }
    }
    ta_intake::write_queue(project_root, &remaining)?;
    Ok(())
}

/// Ask exactly one clarifying question for a low-confidence queued event,
/// via the same headless-agent mechanism `ta advisor create` uses, and
/// re-route with the answer folded into the event's suggested title.
///
/// Returns `None` (leave the original event queued, unmodified) when the
/// clarification agent fails to spawn or times out — Observable & Actionable
/// callers print why before falling back.
fn clarify_and_reroute(
    project_root: &Path,
    event: &TriggerEvent,
    decision: &ta_brain::RoutingDecision,
) -> Option<(TriggerEvent, ta_brain::RoutingDecision)> {
    let ta_bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ta"));
    let question = ta_advisor::clarifying_question(decision);
    println!(
        "  clarifying: \"{}\" — {}",
        event.suggested_goal_title, question
    );

    let intent_config = ta_session::IntentAgentConfig::new(
        project_root.to_path_buf(),
        event.suggested_goal_title.clone(),
        question,
    );
    if let Err(e) = ta_session::spawn_intent_agent(&intent_config, &ta_bin) {
        eprintln!("  Failed to spawn clarification agent: {}", e);
        return None;
    }

    let answer = match ta_session::poll_intent_answer(
        project_root,
        intent_config.item_id,
        intent_config.timeout,
        std::time::Duration::from_secs(1),
    ) {
        ta_session::IntentAgentOutcome::Answered(answer) => answer,
        ta_session::IntentAgentOutcome::TimedOut => {
            eprintln!(
                "  Clarifying question timed out after {}s.",
                intent_config.timeout.as_secs()
            );
            return None;
        }
    };

    let mut clarified_event = event.clone();
    clarified_event.suggested_goal_title =
        format!("{} {}", event.suggested_goal_title, answer.trim());
    let re_decision = ta_brain::route(
        &ta_brain::RoutingInput::Trigger(ta_brain::TriggerRoutingInput::from_event(
            clarified_event.clone(),
        )),
        project_root,
    );
    Some((clarified_event, re_decision))
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
